//! PermissionStore + Permission pattern（架构 §4.6 / §6.1.2）。
//!
//! 设计走 Claude Code 风格：
//! - 落盘只有 `allow` / `deny` / `paths` 三个数组（详见 [`crate::storage::permissions`]）
//! - 每条 allow/deny 是字符串 pattern：`<Tool>(<arg>)` 或 `<Tool>`（任意调用）
//! - Scope 由文件位置区分：global / project / session 三个文件天然分层
//! - PermissionRule struct 已删除——pattern 字符串本身是唯一的标识/数据
//!
//! Pattern 语法：
//! - `Bash(xargs)` / `Bash(git status)` — 命令前缀（按 token 边界匹配）
//! - `Bash(rm:/tmp)` — 命令前缀 + 路径前缀（冒号分隔）
//! - `Edit(/Users/x/file)` / `Read(/etc)` / `Write(/tmp)` — 路径前缀
//! - `WebFetch(github.com)` / `WebSearch(example.com)` — 域名后缀
//! - `Bash` — 任意 Bash 调用
//! - `*(...)` — 任意工具（仅在程序层使用，UI 一般不暴露）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use common::{AppError, AppResult};
use protocol::PermissionScope;

use crate::storage::permissions as permissions_store;
use crate::storage::projects;

/// Allow / Deny —— 对应文件中的 `allow` / `deny` 数组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    Allow,
    Deny,
}

/// 权限规则诊断命中结果。`find*` 保持只返回 effect；日志链路用这个结构说明
/// 具体命中了哪一层和哪条 pattern。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionMatch {
    pub effect: RuleEffect,
    pub scope: PermissionScope,
    pub pattern: String,
}

/// 解析后的 pattern。`raw` 保留原字符串，用于 list / remove / 落盘。
#[derive(Debug, Clone)]
pub struct Permission {
    pub raw: String,
    tool: String,
    arg: Arg,
}

#[derive(Debug, Clone)]
enum Arg {
    /// `<Tool>` 或 `<Tool>()` — 任意调用该工具
    Any,
    /// `Bash(cmd)` 或 `Bash(cmd:path)` — 命令前缀 + 可选路径前缀
    Bash { cmd: String, path: Option<String> },
    /// `<FileTool>(/path)` — 路径前缀
    Path { prefix: String },
    /// `<WebTool>(domain.com)` — 域名后缀
    Domain { suffix: String },
}

impl Permission {
    /// 解析一条字符串 pattern。
    pub fn parse(raw: &str) -> AppResult<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return Err(AppError::msg("权限 pattern 不能为空"));
        }
        if let Some(open) = s.find('(') {
            if !s.ends_with(')') {
                return Err(AppError::msg(format!("权限 pattern 缺少右括号：{raw}")));
            }
            let tool = s[..open].trim().to_string();
            if tool.is_empty() {
                return Err(AppError::msg(format!("权限 pattern 缺少工具名：{raw}")));
            }
            let inner = &s[open + 1..s.len() - 1];
            let arg = parse_arg(&tool, inner);
            Ok(Permission {
                raw: raw.to_string(),
                tool,
                arg,
            })
        } else {
            Ok(Permission {
                raw: raw.to_string(),
                tool: s.to_string(),
                arg: Arg::Any,
            })
        }
    }

    /// 是否命中一次工具调用。
    /// - `tool_name`：当前工具（如 `"Bash"`）
    /// - `fingerprint`：Bash 类工具的命令级指纹（如 `"git status -uno"`）
    /// - `path`：工具操作的路径或域名
    pub fn matches(&self, tool_name: &str, fingerprint: Option<&str>, path: Option<&str>) -> bool {
        if self.tool != "*" && self.tool != tool_name {
            return false;
        }
        match &self.arg {
            Arg::Any => true,
            Arg::Bash { cmd, path: pp } => {
                let cmd_ok = fingerprint
                    .map(|fp| prefix_with_token_boundary(fp, cmd))
                    .unwrap_or(false);
                let path_ok = match pp {
                    None => true,
                    Some(prefix) => path
                        .map(|p| p.starts_with(prefix.as_str()))
                        .unwrap_or(false),
                };
                cmd_ok && path_ok
            }
            Arg::Path { prefix } => path
                .map(|p| p.starts_with(prefix.as_str()))
                .unwrap_or(false),
            Arg::Domain { suffix } => path.map(|d| d.ends_with(suffix.as_str())).unwrap_or(false),
        }
    }

    /// 仅检查"该 pattern 是否能命中给定路径"，与 tool_name 无关。
    /// 用于跨工具的路径检查（dispatch 路径越界兜底）。
    pub fn matches_path(&self, path: &str) -> bool {
        match &self.arg {
            Arg::Path { prefix } => path.starts_with(prefix.as_str()),
            _ => false,
        }
    }
}

fn parse_arg(tool: &str, inner: &str) -> Arg {
    let arg = inner.trim();
    if arg.is_empty() {
        return Arg::Any;
    }
    match tool {
        "Bash" | "PowerShell" => {
            if let Some(colon) = arg.find(':') {
                let cmd = arg[..colon].trim().to_string();
                let path = arg[colon + 1..].trim().to_string();
                Arg::Bash {
                    cmd,
                    path: if path.is_empty() { None } else { Some(path) },
                }
            } else {
                Arg::Bash {
                    cmd: arg.to_string(),
                    path: None,
                }
            }
        }
        "WebFetch" | "WebSearch" | "Fetch" => Arg::Domain {
            suffix: arg.to_string(),
        },
        _ => Arg::Path {
            prefix: arg.to_string(),
        },
    }
}

fn prefix_with_token_boundary(haystack: &str, prefix: &str) -> bool {
    if haystack == prefix {
        return true;
    }
    if let Some(rest) = haystack.strip_prefix(prefix) {
        rest.starts_with(' ') || rest.starts_with('\t')
    } else {
        false
    }
}

/// 单个文件（global 或某 project）的内存视图。
#[derive(Default)]
struct PermissionsView {
    allow: Vec<Permission>,
    deny: Vec<Permission>,
    paths: Vec<PathBuf>,
    mtime: Option<SystemTime>,
}

impl PermissionsView {
    fn from_file(file: permissions_store::PermissionsFile, mtime: Option<SystemTime>) -> Self {
        Self {
            allow: parse_list(&file.allow),
            deny: parse_list(&file.deny),
            paths: file.paths,
            mtime,
        }
    }

    fn to_file(&self) -> permissions_store::PermissionsFile {
        permissions_store::PermissionsFile {
            allow: self.allow.iter().map(|p| p.raw.clone()).collect(),
            deny: self.deny.iter().map(|p| p.raw.clone()).collect(),
            paths: self.paths.clone(),
        }
    }
}

fn parse_list(raws: &[String]) -> Vec<Permission> {
    let mut out = Vec::new();
    for raw in raws {
        match Permission::parse(raw) {
            Ok(p) => out.push(p),
            Err(e) => tracing::warn!(error = %e, pattern = raw, "跳过非法 permission pattern"),
        }
    }
    out
}

/// 持久化的权限规则集合（架构 §4.6.1 / §4.6.2）。
pub struct PermissionStore {
    data_dir: PathBuf,
    global: Mutex<PermissionsView>,
    /// key = `projects::encode_workdir(workdir)`
    projects: Mutex<HashMap<String, PermissionsView>>,
    /// session 内规则：(allow, deny)。不持久化，进程内有效。
    session_views: Mutex<HashMap<String, (Vec<Permission>, Vec<Permission>)>>,
}

impl PermissionStore {
    pub fn open(data_dir: impl Into<PathBuf>) -> AppResult<Self> {
        let data_dir = data_dir.into();
        let file = permissions_store::load_global(&data_dir)?;
        let mtime = permissions_store::global_mtime(&data_dir);
        Ok(Self {
            data_dir,
            global: Mutex::new(PermissionsView::from_file(file, mtime)),
            projects: Mutex::new(HashMap::new()),
            session_views: Mutex::new(HashMap::new()),
        })
    }

    fn refresh_global(&self) {
        let current = permissions_store::global_mtime(&self.data_dir);
        let mut g = self.global.lock().unwrap();
        let stale = match (current, g.mtime) {
            (Some(cur), Some(prev)) => cur > prev,
            (Some(_), None) => true,
            (None, Some(_)) => {
                *g = PermissionsView::default();
                return;
            }
            (None, None) => false,
        };
        if !stale {
            return;
        }
        match permissions_store::load_global(&self.data_dir) {
            Ok(file) => *g = PermissionsView::from_file(file, current),
            Err(e) => {
                tracing::warn!(error = %e, "global permissions.json reload 失败，沿用旧 cache")
            }
        }
    }

    fn refresh_project(&self, workdir: &Path) {
        let enc = projects::encode_workdir(workdir);
        let current = permissions_store::project_mtime(&self.data_dir, Some(workdir));
        let mut p = self.projects.lock().unwrap();
        let view = p.entry(enc.clone()).or_default();
        let stale = match (current, view.mtime) {
            (Some(cur), Some(prev)) => cur > prev,
            (Some(_), None) => true,
            (None, Some(_)) => {
                *view = PermissionsView::default();
                return;
            }
            (None, None) => {
                view.mtime.is_none()
                    && view.allow.is_empty()
                    && view.deny.is_empty()
                    && view.paths.is_empty()
            }
        };
        if !stale {
            return;
        }
        match permissions_store::load_project(&self.data_dir, workdir) {
            Ok(file) => *view = PermissionsView::from_file(file, current),
            Err(e) => {
                tracing::warn!(error = %e, enc, "project permissions.json reload 失败，沿用旧 cache")
            }
        }
    }

    /// 给 session 视图灌入历史 pattern（surface 启动加载 session 时用）。
    pub fn load_session_view(&self, session_id: &str, allow: Vec<String>, deny: Vec<String>) {
        self.session_views.lock().unwrap().insert(
            session_id.to_string(),
            (parse_list(&allow), parse_list(&deny)),
        );
    }

    /// 兼容旧接口名：仅当 session_id 还没视图时插入空视图，保留已有规则。
    pub fn ensure_session_view(&self, session_id: &str) {
        self.session_views
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_insert_with(|| (Vec::new(), Vec::new()));
    }

    /// 取当前 workdir 对应的 effective paths（global ∪ project）。
    pub fn effective_paths(&self, workdir: Option<&Path>) -> Vec<PathBuf> {
        self.refresh_global();
        let mut out: Vec<PathBuf> = self.global.lock().unwrap().paths.clone();
        if let Some(wd) = workdir {
            self.refresh_project(wd);
            let enc = projects::encode_workdir(wd);
            if let Some(view) = self.projects.lock().unwrap().get(&enc) {
                for p in &view.paths {
                    if !out.contains(p) {
                        out.push(p.clone());
                    }
                }
            }
        }
        out
    }

    /// 查找匹配的决定。优先级：deny 命中 > allow 命中；分层 Session → Project → Global。
    pub fn find(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        tool_name: &str,
        fingerprint: Option<&str>,
        path: Option<&str>,
    ) -> Option<RuleEffect> {
        self.find_diagnostic(session_id, workdir, tool_name, fingerprint, path)
            .map(|m| m.effect)
    }

    /// 同 [`Self::find`]，但返回命中的 scope 和原始 pattern，供审批日志定位。
    pub fn find_diagnostic(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        tool_name: &str,
        fingerprint: Option<&str>,
        path: Option<&str>,
    ) -> Option<PermissionMatch> {
        if let Some(d) = self.find_in_session(session_id, tool_name, fingerprint, path) {
            return Some(d);
        }
        if let Some(wd) = workdir {
            self.refresh_project(wd);
            let enc = projects::encode_workdir(wd);
            let projects = self.projects.lock().unwrap();
            if let Some(view) = projects.get(&enc) {
                if let Some(d) =
                    match_view(view, PermissionScope::Project, tool_name, fingerprint, path)
                {
                    return Some(d);
                }
            }
        }
        self.refresh_global();
        let g = self.global.lock().unwrap();
        match_view(&g, PermissionScope::Global, tool_name, fingerprint, path)
    }

    fn find_in_session(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        fingerprint: Option<&str>,
        path: Option<&str>,
    ) -> Option<PermissionMatch> {
        let sid = session_id?;
        let views = self.session_views.lock().unwrap();
        let (allow, deny) = views.get(sid)?;
        if let Some(p) = deny
            .iter()
            .find(|p| p.matches(tool_name, fingerprint, path))
        {
            return Some(PermissionMatch {
                effect: RuleEffect::Deny,
                scope: PermissionScope::Session,
                pattern: p.raw.clone(),
            });
        }
        if let Some(p) = allow
            .iter()
            .find(|p| p.matches(tool_name, fingerprint, path))
        {
            return Some(PermissionMatch {
                effect: RuleEffect::Allow,
                scope: PermissionScope::Session,
                pattern: p.raw.clone(),
            });
        }
        None
    }

    /// 多路径查询（架构 §4.6）：非 Bash 工具的 `effects.paths` 可能含一条或多条
    /// 路径（如 `Edit { file_path }`、Glob/Grep 的 search root）。语义对称
    /// [`Self::find_for_segments`]：
    /// - 任一 path 命中 deny → `Deny`
    /// - 所有 path 都命中 allow → `Allow`
    /// - 否则 `None`（落到默认策略）
    ///
    /// `paths` 为空时退化为 `find(.., None)`——让 `Arg::Any`（`Edit` 这种工具名级
    /// 规则）仍能生效。修复了 HitlGate::check 早期版本只传 `None` 导致
    /// `Edit(/dir/)` 类目录前缀规则下次永远命中不到 → 子文件反复审批的 bug。
    pub fn find_for_paths(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        tool_name: &str,
        fingerprint: Option<&str>,
        paths: &[String],
    ) -> Option<RuleEffect> {
        self.find_for_paths_diagnostic(session_id, workdir, tool_name, fingerprint, paths)
            .map(|m| m.effect)
    }

    /// 同 [`Self::find_for_paths`]，但保留 scope/pattern 诊断信息。
    pub fn find_for_paths_diagnostic(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        tool_name: &str,
        fingerprint: Option<&str>,
        paths: &[String],
    ) -> Option<PermissionMatch> {
        if paths.is_empty() {
            return self.find_diagnostic(session_id, workdir, tool_name, fingerprint, None);
        }
        for p in paths {
            if let Some(hit) =
                self.find_diagnostic(session_id, workdir, tool_name, fingerprint, Some(p))
            {
                if hit.effect == RuleEffect::Deny {
                    return Some(hit);
                }
            }
        }
        let mut first_allow: Option<PermissionMatch> = None;
        for p in paths {
            match self.find_diagnostic(session_id, workdir, tool_name, fingerprint, Some(p)) {
                Some(hit) if hit.effect == RuleEffect::Allow => {
                    first_allow.get_or_insert(hit);
                }
                _ => return None,
            }
        }
        first_allow
    }

    /// Bash / PowerShell 段级查询（架构 §4.4.2）：
    /// 每段 `(fingerprint, write_targets)`：任一段命中 deny → Deny；全部段命中 allow → Allow。
    pub fn find_for_segments(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        tool_name: &str,
        segments: &[crate::effects::SegmentEffect],
    ) -> Option<RuleEffect> {
        self.find_for_segments_diagnostic(session_id, workdir, tool_name, segments)
            .map(|m| m.effect)
    }

    /// 同 [`Self::find_for_segments`]，但保留第一条命中规则的诊断信息。
    ///
    /// 段级语义（架构 §4.4.2）：
    /// - 任一段（含只读段）命中 deny → 整体 Deny（尊重用户显式 `Bash(cat)` 这类 deny）
    /// - **只读段免匹配**：只要求所有「会写段」命中 allow，整体才 Allow
    /// - 没有会写段（全只读）→ 返回 None，交由 ReadOnly 短路放行
    pub fn find_for_segments_diagnostic(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        tool_name: &str,
        segments: &[crate::effects::SegmentEffect],
    ) -> Option<PermissionMatch> {
        if segments.is_empty() {
            return None;
        }
        // 收集所有层 allow + deny 引用
        let mut allow: Vec<(PermissionScope, Permission)> = Vec::new();
        let mut deny: Vec<(PermissionScope, Permission)> = Vec::new();
        if let Some(sid) = session_id {
            if let Some((a, d)) = self.session_views.lock().unwrap().get(sid) {
                allow.extend(a.iter().cloned().map(|p| (PermissionScope::Session, p)));
                deny.extend(d.iter().cloned().map(|p| (PermissionScope::Session, p)));
            }
        }
        if let Some(wd) = workdir {
            self.refresh_project(wd);
            let enc = projects::encode_workdir(wd);
            if let Some(view) = self.projects.lock().unwrap().get(&enc) {
                allow.extend(
                    view.allow
                        .iter()
                        .cloned()
                        .map(|p| (PermissionScope::Project, p)),
                );
                deny.extend(
                    view.deny
                        .iter()
                        .cloned()
                        .map(|p| (PermissionScope::Project, p)),
                );
            }
        }
        self.refresh_global();
        {
            let g = self.global.lock().unwrap();
            allow.extend(
                g.allow
                    .iter()
                    .cloned()
                    .map(|p| (PermissionScope::Global, p)),
            );
            deny.extend(g.deny.iter().cloned().map(|p| (PermissionScope::Global, p)));
        }

        // 阶段 1：任一段（含只读段）命中 deny → Deny
        for seg in segments {
            for (scope, r) in &deny {
                if segment_hits(r, tool_name, &seg.fingerprint, &seg.write_targets) {
                    return Some(PermissionMatch {
                        effect: RuleEffect::Deny,
                        scope: *scope,
                        pattern: r.raw.clone(),
                    });
                }
                // 跨工具：FilePath deny 兜底 Bash 写文件目标
                for t in &seg.write_targets {
                    if r.matches_path(t) {
                        return Some(PermissionMatch {
                            effect: RuleEffect::Deny,
                            scope: *scope,
                            pattern: r.raw.clone(),
                        });
                    }
                }
            }
        }

        // 阶段 2：全部「会写段」命中 allow → Allow（只读段免匹配）。
        // 没有会写段时 first_allow 仍为 None → 返回 None，交给 ReadOnly 短路。
        let mut first_allow: Option<PermissionMatch> = None;
        let all_writable_allowed = segments
            .iter()
            .filter(|seg| !seg.is_readonly)
            .all(|seg| {
                let matched = allow
                    .iter()
                    .find(|(_, r)| segment_hits(r, tool_name, &seg.fingerprint, &seg.write_targets));
                if let Some((scope, r)) = matched {
                    first_allow.get_or_insert_with(|| PermissionMatch {
                        effect: RuleEffect::Allow,
                        scope: *scope,
                        pattern: r.raw.clone(),
                    });
                    true
                } else {
                    false
                }
            });
        if all_writable_allowed {
            return first_allow;
        }
        None
    }

    /// 诊断路径白名单命中层级。只返回 allow 命中；未命中返回 `None`。
    pub fn allows_path_diagnostic(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        path: &str,
    ) -> Option<PermissionMatch> {
        if let Some(sid) = session_id {
            let views = self.session_views.lock().unwrap();
            if let Some((allow, _)) = views.get(sid) {
                if let Some(p) = allow.iter().find(|p| p.matches_path(path)) {
                    return Some(PermissionMatch {
                        effect: RuleEffect::Allow,
                        scope: PermissionScope::Session,
                        pattern: p.raw.clone(),
                    });
                }
            }
        }
        if let Some(wd) = workdir {
            self.refresh_project(wd);
            let enc = projects::encode_workdir(wd);
            let projects = self.projects.lock().unwrap();
            if let Some(view) = projects.get(&enc) {
                if let Some(p) = view.allow.iter().find(|p| p.matches_path(path)) {
                    return Some(PermissionMatch {
                        effect: RuleEffect::Allow,
                        scope: PermissionScope::Project,
                        pattern: p.raw.clone(),
                    });
                }
                if let Some(p) = view.paths.iter().find(|p| path_starts_with(path, p)) {
                    return Some(PermissionMatch {
                        effect: RuleEffect::Allow,
                        scope: PermissionScope::Project,
                        pattern: p.display().to_string(),
                    });
                }
            }
        }
        self.refresh_global();
        let g = self.global.lock().unwrap();
        if let Some(p) = g.allow.iter().find(|p| p.matches_path(path)) {
            return Some(PermissionMatch {
                effect: RuleEffect::Allow,
                scope: PermissionScope::Global,
                pattern: p.raw.clone(),
            });
        }
        if let Some(p) = g.paths.iter().find(|p| path_starts_with(path, p)) {
            return Some(PermissionMatch {
                effect: RuleEffect::Allow,
                scope: PermissionScope::Global,
                pattern: p.display().to_string(),
            });
        }
        None
    }

    /// 检查给定路径是否被允许（rule 中的 path/Any 类 allow，或 paths 白名单命中）。
    pub fn allows_path(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        path: &str,
    ) -> bool {
        self.allows_path_diagnostic(session_id, workdir, path)
            .is_some()
    }

    /// 增加一条 allow / deny pattern。
    pub fn add(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        decision: RuleEffect,
        pattern: String,
    ) -> AppResult<()> {
        // 校验 pattern 合法
        let parsed = Permission::parse(&pattern)?;
        match scope {
            PermissionScope::Once => Ok(()),
            PermissionScope::Session => {
                let sid =
                    session_id.ok_or_else(|| AppError::msg("Session scope 需要 session_id"))?;
                let mut views = self.session_views.lock().unwrap();
                let entry = views
                    .entry(sid.to_string())
                    .or_insert_with(|| (Vec::new(), Vec::new()));
                let target = match decision {
                    RuleEffect::Allow => &mut entry.0,
                    RuleEffect::Deny => &mut entry.1,
                };
                if !target.iter().any(|p| p.raw == parsed.raw) {
                    target.push(parsed);
                }
                Ok(())
            }
            PermissionScope::Project => {
                let wd = workdir.ok_or_else(|| AppError::msg("Project scope 需要 workdir"))?;
                self.refresh_project(wd);
                let enc = projects::encode_workdir(wd);
                let mut p = self.projects.lock().unwrap();
                let view = p.entry(enc).or_default();
                push_unique(view, decision, parsed);
                let file = view.to_file();
                permissions_store::save_project(&self.data_dir, wd, &file)?;
                view.mtime = permissions_store::project_mtime(&self.data_dir, Some(wd));
                Ok(())
            }
            PermissionScope::Global => {
                self.refresh_global();
                let mut g = self.global.lock().unwrap();
                push_unique(&mut g, decision, parsed);
                let file = g.to_file();
                permissions_store::save_global(&self.data_dir, &file)?;
                g.mtime = permissions_store::global_mtime(&self.data_dir);
                Ok(())
            }
        }
    }

    /// 删除一条 pattern。返回是否真删了。
    pub fn remove(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        decision: RuleEffect,
        pattern: &str,
    ) -> AppResult<bool> {
        match scope {
            PermissionScope::Once => Ok(false),
            PermissionScope::Session => {
                let Some(sid) = session_id else {
                    return Ok(false);
                };
                let mut views = self.session_views.lock().unwrap();
                let Some(entry) = views.get_mut(sid) else {
                    return Ok(false);
                };
                let target = match decision {
                    RuleEffect::Allow => &mut entry.0,
                    RuleEffect::Deny => &mut entry.1,
                };
                let before = target.len();
                target.retain(|p| p.raw != pattern);
                Ok(target.len() != before)
            }
            PermissionScope::Project => {
                let wd = workdir.ok_or_else(|| AppError::msg("Project scope 需要 workdir"))?;
                self.refresh_project(wd);
                let enc = projects::encode_workdir(wd);
                let mut p = self.projects.lock().unwrap();
                let Some(view) = p.get_mut(&enc) else {
                    return Ok(false);
                };
                let removed = remove_unique(view, decision, pattern);
                if removed {
                    let file = view.to_file();
                    permissions_store::save_project(&self.data_dir, wd, &file)?;
                    view.mtime = permissions_store::project_mtime(&self.data_dir, Some(wd));
                }
                Ok(removed)
            }
            PermissionScope::Global => {
                self.refresh_global();
                let mut g = self.global.lock().unwrap();
                let removed = remove_unique(&mut g, decision, pattern);
                if removed {
                    let file = g.to_file();
                    permissions_store::save_global(&self.data_dir, &file)?;
                    g.mtime = permissions_store::global_mtime(&self.data_dir);
                }
                Ok(removed)
            }
        }
    }

    /// 列 patterns：返回原字符串。
    pub fn list(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        decision: RuleEffect,
    ) -> Vec<String> {
        match scope {
            PermissionScope::Once => Vec::new(),
            PermissionScope::Session => {
                let Some(sid) = session_id else {
                    return Vec::new();
                };
                let views = self.session_views.lock().unwrap();
                let Some((a, d)) = views.get(sid) else {
                    return Vec::new();
                };
                let list = match decision {
                    RuleEffect::Allow => a,
                    RuleEffect::Deny => d,
                };
                list.iter().map(|p| p.raw.clone()).collect()
            }
            PermissionScope::Project => {
                if let Some(wd) = workdir {
                    self.refresh_project(wd);
                    let enc = projects::encode_workdir(wd);
                    let p = self.projects.lock().unwrap();
                    p.get(&enc)
                        .map(|v| {
                            select_list(v, decision)
                                .iter()
                                .map(|p| p.raw.clone())
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    self.projects
                        .lock()
                        .unwrap()
                        .values()
                        .flat_map(|v| {
                            select_list(v, decision)
                                .iter()
                                .map(|p| p.raw.clone())
                                .collect::<Vec<_>>()
                        })
                        .collect()
                }
            }
            PermissionScope::Global => {
                self.refresh_global();
                let g = self.global.lock().unwrap();
                select_list(&g, decision)
                    .iter()
                    .map(|p| p.raw.clone())
                    .collect()
            }
        }
    }

    /// 列 paths 白名单。
    pub fn list_paths(&self, scope: PermissionScope, workdir: Option<&Path>) -> Vec<PathBuf> {
        match scope {
            PermissionScope::Project => {
                let Some(wd) = workdir else { return Vec::new() };
                self.refresh_project(wd);
                let enc = projects::encode_workdir(wd);
                self.projects
                    .lock()
                    .unwrap()
                    .get(&enc)
                    .map(|v| v.paths.clone())
                    .unwrap_or_default()
            }
            PermissionScope::Global => {
                self.refresh_global();
                self.global.lock().unwrap().paths.clone()
            }
            _ => Vec::new(),
        }
    }

    /// 增加一条 paths 白名单条目。
    pub fn add_path(
        &self,
        scope: PermissionScope,
        workdir: Option<&Path>,
        path: PathBuf,
    ) -> AppResult<()> {
        match scope {
            PermissionScope::Project => {
                let wd = workdir.ok_or_else(|| AppError::msg("Project scope 需要 workdir"))?;
                self.refresh_project(wd);
                let enc = projects::encode_workdir(wd);
                let mut p = self.projects.lock().unwrap();
                let view = p.entry(enc).or_default();
                if !view.paths.contains(&path) {
                    view.paths.push(path);
                }
                let file = view.to_file();
                permissions_store::save_project(&self.data_dir, wd, &file)?;
                view.mtime = permissions_store::project_mtime(&self.data_dir, Some(wd));
                Ok(())
            }
            PermissionScope::Global => {
                self.refresh_global();
                let mut g = self.global.lock().unwrap();
                if !g.paths.contains(&path) {
                    g.paths.push(path);
                }
                let file = g.to_file();
                permissions_store::save_global(&self.data_dir, &file)?;
                g.mtime = permissions_store::global_mtime(&self.data_dir);
                Ok(())
            }
            _ => Err(AppError::msg("add_path 仅支持 Global / Project").into()),
        }
    }

    /// 删除一条 paths 白名单条目。
    pub fn remove_path(
        &self,
        scope: PermissionScope,
        workdir: Option<&Path>,
        path: &Path,
    ) -> AppResult<bool> {
        match scope {
            PermissionScope::Project => {
                let wd = workdir.ok_or_else(|| AppError::msg("Project scope 需要 workdir"))?;
                self.refresh_project(wd);
                let enc = projects::encode_workdir(wd);
                let mut p = self.projects.lock().unwrap();
                let Some(view) = p.get_mut(&enc) else {
                    return Ok(false);
                };
                let before = view.paths.len();
                view.paths.retain(|p| p.as_path() != path);
                let removed = view.paths.len() != before;
                if removed {
                    let file = view.to_file();
                    permissions_store::save_project(&self.data_dir, wd, &file)?;
                    view.mtime = permissions_store::project_mtime(&self.data_dir, Some(wd));
                }
                Ok(removed)
            }
            PermissionScope::Global => {
                self.refresh_global();
                let mut g = self.global.lock().unwrap();
                let before = g.paths.len();
                g.paths.retain(|p| p.as_path() != path);
                let removed = g.paths.len() != before;
                if removed {
                    let file = g.to_file();
                    permissions_store::save_global(&self.data_dir, &file)?;
                    g.mtime = permissions_store::global_mtime(&self.data_dir);
                }
                Ok(removed)
            }
            _ => Ok(false),
        }
    }

    /// 清空某 scope 的所有规则 + paths。
    pub fn clear(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&Path>,
    ) -> AppResult<()> {
        match scope {
            PermissionScope::Once => Ok(()),
            PermissionScope::Session => {
                if let Some(sid) = session_id {
                    self.session_views.lock().unwrap().remove(sid);
                }
                Ok(())
            }
            PermissionScope::Project => {
                let wd = workdir.ok_or_else(|| AppError::msg("Project scope 需要 workdir"))?;
                self.refresh_project(wd);
                let enc = projects::encode_workdir(wd);
                let mut p = self.projects.lock().unwrap();
                if let Some(view) = p.get_mut(&enc) {
                    *view = PermissionsView::default();
                    let file = view.to_file();
                    permissions_store::save_project(&self.data_dir, wd, &file)?;
                    view.mtime = permissions_store::project_mtime(&self.data_dir, Some(wd));
                }
                Ok(())
            }
            PermissionScope::Global => {
                self.refresh_global();
                let mut g = self.global.lock().unwrap();
                *g = PermissionsView::default();
                permissions_store::save_global(&self.data_dir, &g.to_file())?;
                g.mtime = permissions_store::global_mtime(&self.data_dir);
                Ok(())
            }
        }
    }
}

fn select_list(view: &PermissionsView, decision: RuleEffect) -> &[Permission] {
    match decision {
        RuleEffect::Allow => &view.allow,
        RuleEffect::Deny => &view.deny,
    }
}

fn push_unique(view: &mut PermissionsView, decision: RuleEffect, p: Permission) {
    let list = match decision {
        RuleEffect::Allow => &mut view.allow,
        RuleEffect::Deny => &mut view.deny,
    };
    if !list.iter().any(|x| x.raw == p.raw) {
        list.push(p);
    }
}

fn remove_unique(view: &mut PermissionsView, decision: RuleEffect, pattern: &str) -> bool {
    let list = match decision {
        RuleEffect::Allow => &mut view.allow,
        RuleEffect::Deny => &mut view.deny,
    };
    let before = list.len();
    list.retain(|p| p.raw != pattern);
    list.len() != before
}

fn match_view(
    view: &PermissionsView,
    scope: PermissionScope,
    tool_name: &str,
    fingerprint: Option<&str>,
    path: Option<&str>,
) -> Option<PermissionMatch> {
    if let Some(p) = view
        .deny
        .iter()
        .find(|p| p.matches(tool_name, fingerprint, path))
    {
        return Some(PermissionMatch {
            effect: RuleEffect::Deny,
            scope,
            pattern: p.raw.clone(),
        });
    }
    if let Some(p) = view
        .allow
        .iter()
        .find(|p| p.matches(tool_name, fingerprint, path))
    {
        return Some(PermissionMatch {
            effect: RuleEffect::Allow,
            scope,
            pattern: p.raw.clone(),
        });
    }
    None
}

fn segment_hits(
    p: &Permission,
    tool_name: &str,
    fingerprint: &str,
    write_targets: &[String],
) -> bool {
    if p.matches(tool_name, Some(fingerprint), None) {
        return true;
    }
    write_targets
        .iter()
        .any(|t| p.matches(tool_name, Some(fingerprint), Some(t)))
}

fn path_starts_with(target: &str, prefix: &Path) -> bool {
    let prefix_str = prefix.to_string_lossy();
    if target == prefix_str {
        return true;
    }
    let trimmed = prefix_str.trim_end_matches('/').trim_end_matches('\\');
    if let Some(rest) = target.strip_prefix(trimmed) {
        rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hebbian-perm-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parse_bash_simple() {
        let p = Permission::parse("Bash(xargs)").unwrap();
        assert!(p.matches("Bash", Some("xargs -L 1 echo"), None));
        assert!(!p.matches("Bash", Some("other"), None));
        assert!(!p.matches("Edit", Some("xargs"), None));
    }

    #[test]
    fn parse_bash_with_path() {
        let p = Permission::parse("Bash(rm:/tmp/)").unwrap();
        assert!(p.matches("Bash", Some("rm -rf foo"), Some("/tmp/foo")));
        assert!(!p.matches("Bash", Some("rm -rf foo"), Some("/var/foo")));
    }

    #[test]
    fn parse_file_path() {
        let p = Permission::parse("Edit(/Users/x/proj)").unwrap();
        assert!(p.matches("Edit", None, Some("/Users/x/proj/src/a.rs")));
        assert!(!p.matches("Edit", None, Some("/Users/y/other")));
    }

    #[test]
    fn parse_network() {
        let p = Permission::parse("WebFetch(github.com)").unwrap();
        assert!(p.matches("WebFetch", None, Some("https://api.github.com")));
    }

    #[test]
    fn parse_any() {
        let p = Permission::parse("Bash").unwrap();
        assert!(p.matches("Bash", Some("anything"), None));
        assert!(!p.matches("Edit", None, Some("/x")));
    }

    #[test]
    fn parse_empty_errors() {
        assert!(Permission::parse("").is_err());
        assert!(Permission::parse("Bash(").is_err());
    }

    #[test]
    fn global_allow_persists_and_reloads() {
        let dir = tmp("global");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Allow,
                "Bash(git status)".to_string(),
            )
            .unwrap();
        drop(store);
        let store2 = PermissionStore::open(&dir).unwrap();
        let dec = store2.find(None, None, "Bash", Some("git status -uno"), None);
        assert_eq!(dec, Some(RuleEffect::Allow));
    }

    #[test]
    fn deny_overrides_allow() {
        let dir = tmp("deny");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Allow,
                "Bash(git)".to_string(),
            )
            .unwrap();
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Deny,
                "Bash(git push)".to_string(),
            )
            .unwrap();
        assert_eq!(
            store.find(None, None, "Bash", Some("git status"), None),
            Some(RuleEffect::Allow)
        );
        assert_eq!(
            store.find(None, None, "Bash", Some("git push origin"), None),
            Some(RuleEffect::Deny)
        );
    }

    #[test]
    fn session_takes_precedence_over_global() {
        let dir = tmp("precedence");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Allow,
                "Bash(git)".to_string(),
            )
            .unwrap();
        let sid = "s1";
        store
            .add(
                PermissionScope::Session,
                Some(sid),
                None,
                RuleEffect::Deny,
                "Bash(git push)".to_string(),
            )
            .unwrap();
        assert_eq!(
            store.find(Some(sid), None, "Bash", Some("git push origin"), None),
            Some(RuleEffect::Deny)
        );
        assert_eq!(
            store.find(Some(sid), None, "Bash", Some("git status"), None),
            Some(RuleEffect::Allow)
        );
    }

    #[test]
    fn project_scope_isolated_by_workdir() {
        let dir = tmp("project");
        let store = PermissionStore::open(&dir).unwrap();
        let proj_a = PathBuf::from("/Users/x/proj/foo");
        let proj_b = PathBuf::from("/Users/x/proj/bar");
        store
            .add(
                PermissionScope::Project,
                None,
                Some(&proj_a),
                RuleEffect::Allow,
                "Bash(cd)".to_string(),
            )
            .unwrap();
        assert_eq!(
            store.find(None, Some(&proj_a), "Bash", Some("cd /tmp/foo"), None),
            Some(RuleEffect::Allow)
        );
        assert_eq!(
            store.find(None, Some(&proj_b), "Bash", Some("cd /tmp/foo"), None),
            None
        );
    }

    #[test]
    fn global_paths_whitelist_allows_path() {
        let dir = tmp("paths");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add_path(PermissionScope::Global, None, PathBuf::from("/etc"))
            .unwrap();
        assert!(store.allows_path(None, None, "/etc/hosts"));
        assert!(!store.allows_path(None, None, "/usr/local"));
    }

    /// 回归：`Edit(/foo/bar/)` 规则必须能匹配 `/foo/bar/` 下的任意子文件
    /// （父目录前缀递归覆盖子目录，与 Claude Code 的 dirname/starts_with 语义对齐）。
    /// 修前 HitlGate::check 把 path 传成 None 导致这条规则永远不命中 → 子文件反复审批。
    #[test]
    fn find_for_paths_matches_subdirectory_under_project_rule() {
        let dir = tmp("subdir");
        let store = PermissionStore::open(&dir).unwrap();
        let proj = PathBuf::from("/Users/x/proj");
        store
            .add(
                PermissionScope::Project,
                None,
                Some(&proj),
                RuleEffect::Allow,
                "Edit(/foo/bar/)".to_string(),
            )
            .unwrap();
        // 直接父目录下的文件 → Allow
        let dec = store.find_for_paths(
            None,
            Some(&proj),
            "Edit",
            None,
            &["/foo/bar/x.rs".to_string()],
        );
        assert_eq!(dec, Some(RuleEffect::Allow));
        // 更深子目录的文件 → Allow（递归覆盖）
        let dec = store.find_for_paths(
            None,
            Some(&proj),
            "Edit",
            None,
            &["/foo/bar/sub/deep/y.rs".to_string()],
        );
        assert_eq!(dec, Some(RuleEffect::Allow));
        // 同级别但不在前缀下的文件 → 未决（落回默认策略，仍审批）
        let dec = store.find_for_paths(
            None,
            Some(&proj),
            "Edit",
            None,
            &["/foo/other/z.rs".to_string()],
        );
        assert_eq!(dec, None);
    }

    /// 多 path 全允许才整体 Allow；任一 path 命中 deny 即整体 Deny。
    #[test]
    fn find_for_paths_multi_path_semantics() {
        let dir = tmp("multipath");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Allow,
                "Edit(/tmp/)".to_string(),
            )
            .unwrap();
        // 两条都在 /tmp/ 下 → Allow
        let dec = store.find_for_paths(
            None,
            None,
            "Edit",
            None,
            &["/tmp/a.txt".to_string(), "/tmp/b/c.txt".to_string()],
        );
        assert_eq!(dec, Some(RuleEffect::Allow));
        // 一条不在 allow 前缀下 → 未决（不整体 Allow）
        let dec = store.find_for_paths(
            None,
            None,
            "Edit",
            None,
            &["/tmp/a.txt".to_string(), "/etc/passwd".to_string()],
        );
        assert_eq!(dec, None);
        // 加一条 deny，任一命中即整体 Deny
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Deny,
                "Edit(/etc/)".to_string(),
            )
            .unwrap();
        let dec = store.find_for_paths(
            None,
            None,
            "Edit",
            None,
            &["/tmp/a.txt".to_string(), "/etc/passwd".to_string()],
        );
        assert_eq!(dec, Some(RuleEffect::Deny));
    }

    /// 空 paths 退化到 find(None) —— `Arg::Any`（工具名级 `Edit`）仍生效。
    #[test]
    fn find_for_paths_empty_falls_back_to_tool_name_rule() {
        let dir = tmp("anyfb");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Allow,
                "Edit".to_string(),
            )
            .unwrap();
        let dec = store.find_for_paths(None, None, "Edit", None, &[]);
        assert_eq!(dec, Some(RuleEffect::Allow));
    }

    #[test]
    fn find_diagnostic_reports_scope_and_pattern() {
        let dir = tmp("diag");
        let store = PermissionStore::open(&dir).unwrap();
        let sid = "s1";
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Allow,
                "Bash(git)".to_string(),
            )
            .unwrap();
        store
            .add(
                PermissionScope::Session,
                Some(sid),
                None,
                RuleEffect::Allow,
                "Bash(grep)".to_string(),
            )
            .unwrap();

        let hit = store
            .find_diagnostic(Some(sid), None, "Bash", Some("grep foo"), None)
            .expect("session rule should match");
        assert_eq!(hit.effect, RuleEffect::Allow);
        assert_eq!(hit.scope, PermissionScope::Session);
        assert_eq!(hit.pattern, "Bash(grep)");
    }

    #[test]
    fn list_and_remove() {
        let dir = tmp("list");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Allow,
                "Bash(ls)".to_string(),
            )
            .unwrap();
        store
            .add(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Deny,
                "Bash(rm)".to_string(),
            )
            .unwrap();
        let allows = store.list(PermissionScope::Global, None, None, RuleEffect::Allow);
        let denies = store.list(PermissionScope::Global, None, None, RuleEffect::Deny);
        assert_eq!(allows, vec!["Bash(ls)".to_string()]);
        assert_eq!(denies, vec!["Bash(rm)".to_string()]);

        let removed = store
            .remove(
                PermissionScope::Global,
                None,
                None,
                RuleEffect::Allow,
                "Bash(ls)",
            )
            .unwrap();
        assert!(removed);
        let allows = store.list(PermissionScope::Global, None, None, RuleEffect::Allow);
        assert!(allows.is_empty());
    }
}
