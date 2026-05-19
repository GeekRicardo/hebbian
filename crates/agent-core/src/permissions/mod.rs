//! PermissionStore + PermissionRule（架构 §4.5.4 / §4.6）。
//!
//! 数据模型遵从架构 §4.5.4：
//!
//! - [`PermissionRule`]：一条规则，含 scope / tool_name / matcher / decision / 时间戳。
//! - [`PermissionMatcher`]：按工具语义分类的匹配器（Bash / BashWithPath / FilePath /
//!   Network / Any）。
//! - [`PermissionStore`]：内存索引 + 落盘。Global 规则从
//!   `~/.hebbian/permissions.json` 一次加载；Session 规则由调用方在加载 session
//!   时遍历 jsonl entry 灌进来。
//!
//! `match(scope_chain, tool_name, effects)`：按 `[Session, Global]` 顺序查表，
//! 命中即返回（架构 §4.6.1）。

use std::sync::Mutex;

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
/// `global_rules` 启动时一次性从 `~/.hebbian/permissions.json` 加载。
/// `session_rules_for` 提供按 session 索引的 in-memory 视图——调用方在
/// load_session 时遍历 jsonl 中的 `PermissionRule` entry 灌进来。
pub struct PermissionStore {
    data_dir: std::path::PathBuf,
    global_rules: Mutex<Vec<PermissionRule>>,
    session_rules: Mutex<std::collections::HashMap<String, Vec<PermissionRule>>>,
}

impl PermissionStore {
    /// 创建并加载 global 规则。
    pub fn open(data_dir: impl Into<std::path::PathBuf>) -> AppResult<Self> {
        let data_dir = data_dir.into();
        let file = permissions_store::load(&data_dir)?;
        Ok(Self {
            data_dir,
            global_rules: Mutex::new(file.rules),
            session_rules: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// 在 session 内存视图里追加一条规则（不落盘——session 规则随 session.jsonl
    /// 持久化，由 Recorder 写入）。
    pub fn load_session_rules(&self, session_id: &str, rules: Vec<PermissionRule>) {
        self.session_rules
            .lock()
            .unwrap()
            .insert(session_id.to_string(), rules);
    }

    /// 查找是否命中规则（[Session, Global] 顺序）。
    /// 命中返回 [`PermissionDecisionKind`]；未命中返回 `None`，调用方按 RunMode 默认。
    ///
    /// `tool_name = "*"` 的规则作为兜底通配：匹配任意工具名。用于路径审批
    /// 产生的跨工具 FilePath 规则。
    pub fn find(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        fingerprint: Option<&str>,
        path: Option<&str>,
    ) -> Option<PermissionDecisionKind> {
        if let Some(sid) = session_id {
            let session_rules = self.session_rules.lock().unwrap();
            if let Some(rules) = session_rules.get(sid) {
                if let Some(rule) = rules
                    .iter()
                    .find(|r| tool_matches(&r.tool_name, tool_name) && r.matcher.matches(fingerprint, path))
                {
                    return Some(rule.decision);
                }
            }
        }
        let global = self.global_rules.lock().unwrap();
        global
            .iter()
            .find(|r| tool_matches(&r.tool_name, tool_name) && r.matcher.matches(fingerprint, path))
            .map(|r| r.decision)
    }

    /// Bash / PowerShell 段级查询（架构 §4.4.2）：
    ///
    /// 输入是每段 `(fingerprint, write_targets)`，返回：
    /// - `Deny`  当任一段命中任一 deny 规则（fingerprint 命中 `tool_name` 维度的
    ///   Bash/BashWithPath 规则，或 write_target 命中任一 FilePath/BashWithPath
    ///   规则——后者用来兜底 Bash 写文件绕过 Edit deny 的工具切换攻击）
    /// - `Allow` 当**全部段**在 `tool_name` 维度命中至少一条 Bash/Any allow 规则
    /// - `None`  其余（调用方按 RunMode 默认决策）
    ///
    /// `tool_name` 通常是 `"Bash"` 或 `"PowerShell"`；写目标维度会跨工具查
    /// `"Edit"` / `"Write"` / `"Bash"` 等 tool 下的 FilePath/BashWithPath 规则。
    /// `tool_name = "*"` 的规则作为兜底通配：匹配任意工具名。
    pub fn find_for_segments(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        segments: &[(String, Vec<String>)],
    ) -> Option<PermissionDecisionKind> {
        if segments.is_empty() {
            return None;
        }
        // 收集生效规则：Session → Global 拼接
        let mut all_rules: Vec<PermissionRule> = Vec::new();
        if let Some(sid) = session_id {
            if let Some(rules) = self.session_rules.lock().unwrap().get(sid) {
                all_rules.extend(rules.iter().cloned());
            }
        }
        all_rules.extend(self.global_rules.lock().unwrap().iter().cloned());

        // 阶段 1：deny 优先
        for (fp, write_targets) in segments {
            // (a) tool_name 维度按 fingerprint 命中（含 wildcard "*"）
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
            // (b) 写目标跨工具维度：让 Edit/Write deny 规则兜底 Bash 写文件
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
    /// 用于 dispatcher 路径越界检查，让跨 session 的全局路径规则生效。
    pub fn allows_path(&self, session_id: Option<&str>, path: &str) -> bool {
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
        let global = self.global_rules.lock().unwrap();
        global
            .iter()
            .any(|r| r.decision == PermissionDecisionKind::Allow && r.matcher.matches(None, Some(path)))
    }

    /// 给一条路径创建 wildcard (`"*"`) tool_name 的 FilePath Allow 规则。
    /// 路径审批选了「加入全局」后调用，让其他 session 也能共享。
    pub fn add_path_rule(
        &self,
        session_id: Option<&str>,
        path: String,
        scope: PermissionScope,
    ) -> AppResult<()> {
        let rule = PermissionRule {
            id: new_rule_id(),
            scope,
            tool_name: "*".to_string(),
            matcher: PermissionMatcher::FilePath {
                path_prefix: path,
            },
            decision: PermissionDecisionKind::Allow,
            created_at: chrono::Utc::now().timestamp_millis(),
            created_by: "user".to_string(),
        };
        self.add(session_id, rule)
    }

    /// 增加规则：Session 仅写内存（落 jsonl 由 Recorder 负责）；Global 重写
    /// `~/.hebbian/permissions.json`。
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
            PermissionScope::Global => {
                let mut g = self.global_rules.lock().unwrap();
                g.push(rule);
                let file = permissions_store::PermissionsFile { rules: g.clone() };
                permissions_store::save(&self.data_dir, &file)
            }
        }
    }

    /// 移除规则（按 id）。Session / Global 都查；Global 命中则重写文件。
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
        let mut g = self.global_rules.lock().unwrap();
        let before = g.len();
        g.retain(|r| r.id != rule_id);
        if g.len() != before {
            let file = permissions_store::PermissionsFile { rules: g.clone() };
            permissions_store::save(&self.data_dir, &file)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 列规则：scope=Some(Session) 时需要 session_id；Global 不需要。
    pub fn list(&self, scope: PermissionScope, session_id: Option<&str>) -> Vec<PermissionRule> {
        match scope {
            PermissionScope::Once => Vec::new(),
            PermissionScope::Session => session_id
                .and_then(|sid| self.session_rules.lock().unwrap().get(sid).cloned())
                .unwrap_or_default(),
            PermissionScope::Global => self.global_rules.lock().unwrap().clone(),
        }
    }

    /// 清空某 scope。Session 仅内存；Global 重写文件。
    pub fn clear(&self, scope: PermissionScope, session_id: Option<&str>) -> AppResult<()> {
        match scope {
            PermissionScope::Once => Ok(()),
            PermissionScope::Session => {
                if let Some(sid) = session_id {
                    self.session_rules.lock().unwrap().remove(sid);
                }
                Ok(())
            }
            PermissionScope::Global => {
                self.global_rules.lock().unwrap().clear();
                permissions_store::save(
                    &self.data_dir,
                    &permissions_store::PermissionsFile::default(),
                )
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
        }
    }

    #[test]
    fn global_rule_persists_and_reloads() {
        let dir = tmp("global");
        let store = PermissionStore::open(&dir).unwrap();
        store
            .add(None, bash_rule("git status", PermissionScope::Global))
            .unwrap();
        // 重开同目录：global 规则应已加载
        drop(store);
        let store2 = PermissionStore::open(&dir).unwrap();
        let dec = store2.find(None, "Bash", Some("git status -uno"), None);
        assert_eq!(dec, Some(PermissionDecisionKind::Allow));
    }

    #[test]
    fn session_takes_precedence_over_global() {
        let dir = tmp("precedence");
        let store = PermissionStore::open(&dir).unwrap();
        // Global allow git
        store
            .add(None, bash_rule("git", PermissionScope::Global))
            .unwrap();
        // Session deny git push
        let sid = "s1";
        let mut deny = bash_rule("git push", PermissionScope::Session);
        deny.decision = PermissionDecisionKind::Deny;
        store.load_session_rules(sid, vec![deny]);
        // git push 命中 session deny 而非 global allow
        assert_eq!(
            store.find(Some(sid), "Bash", Some("git push origin"), None),
            Some(PermissionDecisionKind::Deny)
        );
        // git status 仅命中 global allow
        assert_eq!(
            store.find(Some(sid), "Bash", Some("git status"), None),
            Some(PermissionDecisionKind::Allow)
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
}
