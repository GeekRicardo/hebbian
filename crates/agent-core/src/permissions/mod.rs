//! PermissionStore + PermissionRule（架构 §4.5.4 / §4.6）。
//!
//! 数据模型遵从架构 §4.5.4：
//!
//! - [`PermissionRule`]：一条规则，含 scope / tool_name / matcher / decision / 时间戳 /
//!   可选的 `workdir`（Project scope 必填）。
//! - [`PermissionMatcher`]：按工具语义分类的匹配器（Bash / BashWithPath / FilePath /
//!   Network / Any）。
//! - [`PermissionStore`]：内存索引 + 落盘。Project + Global 规则共用
//!   `~/.hebbian/permissions.json`，匹配前按 mtime 做热加载（架构 §4.6.2）；
//!   Session 规则由调用方在加载 session 时遍历 jsonl entry 灌进来。
//!
//! `match(session_id, workdir, tool_name, effects)`：按 `[Session, Project, Global]`
//! 顺序查表，命中即返回（架构 §4.6.1）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use common::AppResult;
use protocol::PermissionScope;

use crate::storage::permissions as permissions_store;

/// 单条权限规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub id: String,
    pub scope: PermissionScope,
    #[serde(rename = "toolName")]
    pub tool_name: String,
    pub matcher: PermissionMatcher,
    pub decision: PermissionDecisionKind,
    #[serde(default, rename = "createdAt")]
    pub created_at: i64,
    #[serde(default, rename = "createdBy")]
    pub created_by: String,
    /// Project scope 规则必填：用户写规则时的 workdir。
    /// 匹配阶段按 `current_workdir.starts_with(rule.workdir)` 命中（含子目录）。
    /// 旧 Global 规则未带此字段时 deserialize 为 `None`，向前兼容。
    #[serde(default)]
    pub workdir: Option<PathBuf>,
}

/// 写盘时的决定类型（仅 Allow / Deny，无 "Once"——Once 本就不持久化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecisionKind {
    Allow,
    Deny,
}

/// 按工具语义分类的匹配器（架构 §4.5.4）。
///
/// JSON 形态：`{ "type": "Bash", "commandPrefix": "git" }` —— 与文档示例一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PermissionMatcher {
    /// 工具级全放行（适用于 Read / Glob 等 ReadOnly 工具）。
    Any,
    /// Bash 命令前缀（按空白 token 边界匹配）。
    Bash {
        #[serde(rename = "commandPrefix")]
        command_prefix: String,
    },
    /// Bash + 命令前缀 + 路径前缀（如 `rm` 限制只能在 `~/tmp`）。
    BashWithPath {
        #[serde(rename = "commandPrefix")]
        command_prefix: String,
        #[serde(rename = "pathPrefix")]
        path_prefix: String,
    },
    /// 文件路径前缀（Read / Write / Edit 等）。
    FilePath {
        #[serde(rename = "pathPrefix")]
        path_prefix: String,
    },
    /// 网络访问域名后缀（Fetch / WebSearch）。
    Network {
        #[serde(rename = "domainSuffix")]
        domain_suffix: String,
    },
}

impl PermissionMatcher {
    /// 是否匹配一次工具调用。`fingerprint` 是工具自报的命令级指纹
    /// （Bash 给 `"git status -uno"`），`path` 是工具操作的路径（如 Write 的目标文件）。
    pub fn matches(&self, fingerprint: Option<&str>, path: Option<&str>) -> bool {
        match self {
            PermissionMatcher::Any => true,
            PermissionMatcher::Bash { command_prefix } => fingerprint
                .map(|fp| prefix_with_token_boundary(fp, command_prefix))
                .unwrap_or(false),
            PermissionMatcher::BashWithPath {
                command_prefix,
                path_prefix,
            } => {
                let cmd_ok = fingerprint
                    .map(|fp| prefix_with_token_boundary(fp, command_prefix))
                    .unwrap_or(false);
                let path_ok = path
                    .map(|p| p.starts_with(path_prefix.as_str()))
                    .unwrap_or(false);
                cmd_ok && path_ok
            }
            PermissionMatcher::FilePath { path_prefix } => path
                .map(|p| p.starts_with(path_prefix.as_str()))
                .unwrap_or(false),
            PermissionMatcher::Network { domain_suffix } => path
                .map(|d| d.ends_with(domain_suffix.as_str()))
                .unwrap_or(false),
        }
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

/// rule 的 `tool_name` 是否命中当前工具名：精确匹配，或 wildcard `"*"` 匹配任意工具。
fn tool_matches(rule_tool: &str, current_tool: &str) -> bool {
    rule_tool == "*" || rule_tool == current_tool
}

/// 当前 workdir 是否被 rule 的 scope/workdir 覆盖。
///
/// - `Session` 不在持久化文件里，本函数不应被调用；防御性返回 false。
/// - `Project` 仅当 rule.workdir 是当前 workdir 的前缀（含相等）时命中。
/// - `Global` workdir = None，对任意 workdir 都命中。
/// - `Once` 不持久化，同 Session 防御性 false。
fn workdir_matches(rule: &PermissionRule, current_workdir: Option<&Path>) -> bool {
    match rule.scope {
        PermissionScope::Once | PermissionScope::Session => false,
        PermissionScope::Project => match (&rule.workdir, current_workdir) {
            (Some(rule_dir), Some(cwd)) => cwd.starts_with(rule_dir),
            _ => false,
        },
        PermissionScope::Global => true,
    }
}

/// 判断 matcher 是否命中某段（fingerprint + write_targets 任一即算）。
/// 由 [`PermissionStore::find_for_segments`] 内部使用。
fn matcher_hits_segment(
    matcher: &PermissionMatcher,
    fingerprint: &str,
    write_targets: &[String],
) -> bool {
    if matcher.matches(Some(fingerprint), None) {
        return true;
    }
    write_targets
        .iter()
        .any(|t| matcher.matches(Some(fingerprint), Some(t.as_str())))
}

/// 持久化的权限规则集合（架构 §4.6.1 / §4.6.2）。
///
/// `persisted_rules` 启动时一次性从 `~/.hebbian/permissions.json` 加载（Project + Global
/// 共用此文件，按 scope + rule.workdir 字段区分）。匹配前 [`Self::reload_if_stale`] 检查
/// 文件 mtime 做热加载——用户手动改文件下一次审批立即生效。
///
/// `session_rules_for` 提供按 session 索引的 in-memory 视图——调用方在
/// load_session 时遍历 jsonl 中的 `PermissionRule` entry 灌进来。
pub struct PermissionStore {
    data_dir: PathBuf,
    persisted_rules: Mutex<Vec<PermissionRule>>,
    /// 上次加载时文件的 mtime；用于检测外部修改触发 reload。
    last_loaded_mtime: Mutex<Option<SystemTime>>,
    session_rules: Mutex<std::collections::HashMap<String, Vec<PermissionRule>>>,
}

impl PermissionStore {
    /// 创建并加载持久化规则。
    pub fn open(data_dir: impl Into<PathBuf>) -> AppResult<Self> {
        let data_dir = data_dir.into();
        let file = permissions_store::load(&data_dir)?;
        let mtime = permissions_store::mtime(&data_dir);
        Ok(Self {
            data_dir,
            persisted_rules: Mutex::new(file.rules),
            last_loaded_mtime: Mutex::new(mtime),
            session_rules: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// 检查 `permissions.json` 的 mtime；若文件比上次加载新，重新读盘 + 替换 cache。
    /// 失败时打 warn 不阻塞决策路径（仍用旧 cache）。
    fn reload_if_stale(&self) {
        let current = permissions_store::mtime(&self.data_dir);
        let mut last = self.last_loaded_mtime.lock().unwrap();
        let stale = match (current, *last) {
            (Some(cur), Some(prev)) => cur > prev,
            (Some(_), None) => true,
            // 文件被删了：清空 cache 让规则失效
            (None, Some(_)) => {
                *self.persisted_rules.lock().unwrap() = Vec::new();
                *last = None;
                return;
            }
            (None, None) => false,
        };
        if !stale {
            return;
        }
        match permissions_store::load(&self.data_dir) {
            Ok(file) => {
                *self.persisted_rules.lock().unwrap() = file.rules;
                *last = current;
            }
            Err(e) => {
                tracing::warn!(error = %e, "permissions.json reload 失败，沿用旧 cache");
            }
        }
    }

    /// 在 session 内存视图里追加一条规则（不落盘——session 规则随 session.jsonl
    /// 持久化，由 Recorder 写入）。
    ///
    /// **严禁**在用户发新消息 / turn 切换时无脑调用 `load_session_rules(sid, vec![])`
    /// 重置规则——会把累积的 AllowAndRemember(Session) 清空，导致审批反复弹（架构 §4.6.2）。
    /// 本函数只用于 surface 启动加载 session 时一次性灌入历史 PermissionRule。
    pub fn load_session_rules(&self, session_id: &str, rules: Vec<PermissionRule>) {
        self.session_rules
            .lock()
            .unwrap()
            .insert(session_id.to_string(), rules);
    }

    /// 仅当该 session_id 还没有任何视图时，初始化一个空 vec；已有则保留现有规则。
    /// 用于 surface 在 session 启动 / 新 turn 入口的"幂等初始化"，避免 [`Self::load_session_rules`]
    /// 误清空累积规则的 bug。
    pub fn ensure_session_view(&self, session_id: &str) {
        self.session_rules
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default();
    }

    /// 查找是否命中规则（[Session, Project, Global] 顺序）。
    /// 命中返回 [`PermissionDecisionKind`]；未命中返回 `None`，调用方按 RunMode 默认。
    ///
    /// `tool_name = "*"` 的规则作为兜底通配：匹配任意工具名。用于路径审批
    /// 产生的跨工具 FilePath 规则。
    pub fn find(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        tool_name: &str,
        fingerprint: Option<&str>,
        path: Option<&str>,
    ) -> Option<PermissionDecisionKind> {
        self.reload_if_stale();
        if let Some(sid) = session_id {
            let session_rules = self.session_rules.lock().unwrap();
            if let Some(rules) = session_rules.get(sid) {
                if let Some(rule) = rules.iter().find(|r| {
                    tool_matches(&r.tool_name, tool_name) && r.matcher.matches(fingerprint, path)
                }) {
                    return Some(rule.decision);
                }
            }
        }
        let persisted = self.persisted_rules.lock().unwrap();
        // Project 优先于 Global：用户对项目级的精细化覆盖更具体
        for scope in [PermissionScope::Project, PermissionScope::Global] {
            if let Some(rule) = persisted.iter().find(|r| {
                r.scope == scope
                    && workdir_matches(r, workdir)
                    && tool_matches(&r.tool_name, tool_name)
                    && r.matcher.matches(fingerprint, path)
            }) {
                return Some(rule.decision);
            }
        }
        None
    }

    /// Bash / PowerShell 段级查询（架构 §4.4.2）：
    ///
    /// 输入是每段 `(fingerprint, write_targets)`，返回：
    /// - `Deny`  当任一段命中任一 deny 规则
    /// - `Allow` 当**全部段**命中至少一条 Bash/Any allow 规则
    /// - `None`  其余（调用方按 RunMode 默认决策）
    ///
    /// `tool_name = "*"` 的规则作为兜底通配：匹配任意工具名。
    pub fn find_for_segments(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        tool_name: &str,
        segments: &[(String, Vec<String>)],
    ) -> Option<PermissionDecisionKind> {
        if segments.is_empty() {
            return None;
        }
        self.reload_if_stale();
        // 收集生效规则：Session → 持久化文件（已 workdir 过滤）
        let mut all_rules: Vec<PermissionRule> = Vec::new();
        if let Some(sid) = session_id {
            if let Some(rules) = self.session_rules.lock().unwrap().get(sid) {
                all_rules.extend(rules.iter().cloned());
            }
        }
        {
            let persisted = self.persisted_rules.lock().unwrap();
            for r in persisted.iter() {
                if matches!(r.scope, PermissionScope::Project | PermissionScope::Global)
                    && workdir_matches(r, workdir)
                {
                    all_rules.push(r.clone());
                }
            }
        }

        // 阶段 1：deny 优先
        for (fp, write_targets) in segments {
            for r in &all_rules {
                if !tool_matches(&r.tool_name, tool_name) {
                    continue;
                }
                if r.decision != PermissionDecisionKind::Deny {
                    continue;
                }
                if matcher_hits_segment(&r.matcher, fp, write_targets) {
                    return Some(PermissionDecisionKind::Deny);
                }
            }
            // 写目标跨工具维度：让 Edit/Write deny 规则兜底 Bash 写文件
            for t in write_targets {
                for r in &all_rules {
                    if r.decision != PermissionDecisionKind::Deny {
                        continue;
                    }
                    if matches!(
                        r.matcher,
                        PermissionMatcher::FilePath { .. } | PermissionMatcher::BashWithPath { .. }
                    ) && r.matcher.matches(None, Some(t))
                    {
                        return Some(PermissionDecisionKind::Deny);
                    }
                }
            }
        }

        // 阶段 2：全部段都至少命中一条 allow
        let all_allow = segments.iter().all(|(fp, write_targets)| {
            all_rules.iter().any(|r| {
                tool_matches(&r.tool_name, tool_name)
                    && r.decision == PermissionDecisionKind::Allow
                    && matcher_hits_segment(&r.matcher, fp, write_targets)
            })
        });
        if all_allow {
            return Some(PermissionDecisionKind::Allow);
        }
        None
    }

    /// 检查是否有 Allow 规则匹配给定路径（不限 tool_name）。
    /// 用于 dispatcher 路径越界检查，让跨 session 的 Project / Global 路径规则生效。
    pub fn allows_path(
        &self,
        session_id: Option<&str>,
        workdir: Option<&Path>,
        path: &str,
    ) -> bool {
        self.reload_if_stale();
        if let Some(sid) = session_id {
            let session_rules = self.session_rules.lock().unwrap();
            if let Some(rules) = session_rules.get(sid) {
                if rules
                    .iter()
                    .any(|r| r.decision == PermissionDecisionKind::Allow && r.matcher.matches(None, Some(path)))
                {
                    return true;
                }
            }
        }
        let persisted = self.persisted_rules.lock().unwrap();
        persisted.iter().any(|r| {
            matches!(r.scope, PermissionScope::Project | PermissionScope::Global)
                && workdir_matches(r, workdir)
                && r.decision == PermissionDecisionKind::Allow
                && r.matcher.matches(None, Some(path))
        })
    }

    /// 给一条路径创建 wildcard (`"*"`) tool_name 的 FilePath Allow 规则。
    /// 路径审批选了「加入本项目 / 加入全局」后调用。
    pub fn add_path_rule(
        &self,
        session_id: Option<&str>,
        workdir: Option<PathBuf>,
        path: String,
        scope: PermissionScope,
    ) -> AppResult<()> {
        let rule = PermissionRule {
            id: new_rule_id(),
            scope,
            tool_name: "*".to_string(),
            matcher: PermissionMatcher::FilePath { path_prefix: path },
            decision: PermissionDecisionKind::Allow,
            created_at: chrono::Utc::now().timestamp_millis(),
            created_by: "user".to_string(),
            workdir: if scope == PermissionScope::Project {
                workdir
            } else {
                None
            },
        };
        self.add(session_id, rule)
    }

    /// 增加规则：Session 仅写内存（落 jsonl 由 Recorder 负责）；Project / Global 重写
    /// `~/.hebbian/permissions.json`，同步更新 in-memory cache 与 mtime。
    pub fn add(&self, session_id: Option<&str>, rule: PermissionRule) -> AppResult<()> {
        match rule.scope {
            PermissionScope::Once => Ok(()),
            PermissionScope::Session => {
                let sid = session_id
                    .ok_or_else(|| common::AppError::msg("Session scope 需要 session_id"))?;
                self.session_rules
                    .lock()
                    .unwrap()
                    .entry(sid.to_string())
                    .or_default()
                    .push(rule);
                Ok(())
            }
            PermissionScope::Project | PermissionScope::Global => {
                if rule.scope == PermissionScope::Project && rule.workdir.is_none() {
                    return Err(common::AppError::msg("Project scope 规则必须带 workdir"));
                }
                self.reload_if_stale();
                let mut g = self.persisted_rules.lock().unwrap();
                g.push(rule);
                let file = permissions_store::PermissionsFile { rules: g.clone() };
                permissions_store::save(&self.data_dir, &file)?;
                // 写完更新 mtime 缓存，避免触发自身的"已变更" reload。
                *self.last_loaded_mtime.lock().unwrap() =
                    permissions_store::mtime(&self.data_dir);
                Ok(())
            }
        }
    }

    /// 移除规则（按 id）。Session / Project / Global 都查；持久化规则命中则重写文件。
    pub fn remove(&self, session_id: Option<&str>, rule_id: &str) -> AppResult<bool> {
        if let Some(sid) = session_id {
            let mut session_rules = self.session_rules.lock().unwrap();
            if let Some(rules) = session_rules.get_mut(sid) {
                let before = rules.len();
                rules.retain(|r| r.id != rule_id);
                if rules.len() != before {
                    return Ok(true);
                }
            }
        }
        let mut g = self.persisted_rules.lock().unwrap();
        let before = g.len();
        g.retain(|r| r.id != rule_id);
        if g.len() != before {
            let file = permissions_store::PermissionsFile { rules: g.clone() };
            permissions_store::save(&self.data_dir, &file)?;
            *self.last_loaded_mtime.lock().unwrap() = permissions_store::mtime(&self.data_dir);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 列规则：scope=Some(Session) 时需要 session_id；Project / Global 不需要。
    /// Project 列出所有 Project 规则（不按 workdir 过滤——CLI/调试用途；
    /// 匹配阶段才按 workdir 过滤）。
    pub fn list(&self, scope: PermissionScope, session_id: Option<&str>) -> Vec<PermissionRule> {
        self.reload_if_stale();
        match scope {
            PermissionScope::Once => Vec::new(),
            PermissionScope::Session => session_id
                .and_then(|sid| self.session_rules.lock().unwrap().get(sid).cloned())
                .unwrap_or_default(),
            PermissionScope::Project | PermissionScope::Global => self
                .persisted_rules
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.scope == scope)
                .cloned()
                .collect(),
        }
    }

    /// 清空某 scope。Session 仅内存；Project / Global 重写文件。
    pub fn clear(&self, scope: PermissionScope, session_id: Option<&str>) -> AppResult<()> {
        match scope {
            PermissionScope::Once => Ok(()),
            PermissionScope::Session => {
                if let Some(sid) = session_id {
                    self.session_rules.lock().unwrap().remove(sid);
                }
                Ok(())
            }
            PermissionScope::Project | PermissionScope::Global => {
                let mut g = self.persisted_rules.lock().unwrap();
                g.retain(|r| r.scope != scope);
                let file = permissions_store::PermissionsFile { rules: g.clone() };
                permissions_store::save(&self.data_dir, &file)?;
                *self.last_loaded_mtime.lock().unwrap() = permissions_store::mtime(&self.data_dir);
                Ok(())
            }
        }
    }
}

/// 生成一条规则的 uuid id。
pub fn new_rule_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hebbian-perm-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn bash_rule(prefix: &str, scope: PermissionScope) -> PermissionRule {
        PermissionRule {
            id: new_rule_id(),
            scope,
            tool_name: "Bash".into(),
            matcher: PermissionMatcher::Bash {
                command_prefix: prefix.into(),
            },
            decision: PermissionDecisionKind::Allow,
            created_at: 0,
            created_by: "user".into(),
            workdir: None,
        }
    }

    #[test]
    fn global_rule_persists_and_reloads() {
        let dir = tmp("global");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(None, bash_rule("git status", PermissionScope::Global))
            .unwrap();
        drop(store);
        let store2 = PermissionStore::open(&dir).unwrap();
        let dec = store2.find(None, None, "Bash", Some("git status -uno"), None);
        assert_eq!(dec, Some(PermissionDecisionKind::Allow));
    }

    #[test]
    fn session_takes_precedence_over_global() {
        let dir = tmp("precedence");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(None, bash_rule("git", PermissionScope::Global))
            .unwrap();
        let sid = "s1";
        let mut deny = bash_rule("git push", PermissionScope::Session);
        deny.decision = PermissionDecisionKind::Deny;
        store.load_session_rules(sid, vec![deny]);
        assert_eq!(
            store.find(Some(sid), None, "Bash", Some("git push origin"), None),
            Some(PermissionDecisionKind::Deny)
        );
        assert_eq!(
            store.find(Some(sid), None, "Bash", Some("git status"), None),
            Some(PermissionDecisionKind::Allow)
        );
    }

    #[test]
    fn project_rule_scoped_by_workdir() {
        let dir = tmp("project");
        let store = PermissionStore::open(&dir).unwrap();
        let proj_dir = PathBuf::from("/Users/x/proj/foo");
        let mut rule = bash_rule("cd", PermissionScope::Project);
        rule.workdir = Some(proj_dir.clone());
        store.add(None, rule).unwrap();

        // 当前 workdir = proj_dir 子目录 → 命中
        assert_eq!(
            store.find(
                None,
                Some(&proj_dir.join("src")),
                "Bash",
                Some("cd /tmp/foo"),
                None
            ),
            Some(PermissionDecisionKind::Allow)
        );
        // 别的项目 workdir → 不命中
        assert_eq!(
            store.find(
                None,
                Some(Path::new("/Users/x/proj/bar")),
                "Bash",
                Some("cd /tmp/foo"),
                None
            ),
            None
        );
    }

    #[test]
    fn matcher_file_path_prefix() {
        let m = PermissionMatcher::FilePath {
            path_prefix: "/etc/".into(),
        };
        assert!(m.matches(None, Some("/etc/passwd")));
        assert!(!m.matches(None, Some("/home/etc/x")));
    }

    #[test]
    fn ensure_session_view_does_not_clear_existing() {
        let dir = tmp("ensure");
        let store = PermissionStore::open(&dir).unwrap();
        let sid = "s1";
        store.load_session_rules(
            sid,
            vec![bash_rule("cd", PermissionScope::Session)],
        );
        // ensure 在已存在视图时必须保留规则——这是 chat.rs bug 修复的核心保证
        store.ensure_session_view(sid);
        assert_eq!(
            store.find(Some(sid), None, "Bash", Some("cd /tmp"), None),
            Some(PermissionDecisionKind::Allow),
            "ensure_session_view 不应清空已有规则"
        );
    }

    #[test]
    fn external_file_edit_is_hot_loaded() {
        // 用户手动改 permissions.json，下一次 find 必须看到新规则。
        let dir = tmp("hotload");
        let store = PermissionStore::open(&dir).unwrap();
        // 初始无规则
        assert_eq!(store.find(None, None, "Bash", Some("ls"), None), None);

        // 模拟外部进程改文件
        let file = permissions_store::PermissionsFile {
            rules: vec![bash_rule("ls", PermissionScope::Global)],
        };
        // 设法让 mtime 一定变化（同一秒内写入 mtime 可能相同）
        std::thread::sleep(std::time::Duration::from_millis(20));
        permissions_store::save(&dir, &file).unwrap();
        // 再 touch 一次保证 mtime 跳变（防同毫秒精度）
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(permissions_store::path(&dir))
            .and_then(|f| f.set_len(f.metadata()?.len()));

        // 等待文件系统 mtime 推进
        std::thread::sleep(std::time::Duration::from_millis(20));
        let new_file = permissions_store::PermissionsFile {
            rules: vec![bash_rule("ls", PermissionScope::Global)],
        };
        permissions_store::save(&dir, &new_file).unwrap();

        assert_eq!(
            store.find(None, None, "Bash", Some("ls -la"), None),
            Some(PermissionDecisionKind::Allow),
            "外部修改 permissions.json 应在下一次 find 时生效"
        );
    }
}
