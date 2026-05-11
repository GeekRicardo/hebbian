//! 工具调用的 effects 分析（架构 §4.4.2）。
//!
//! 把 Tool trait 的 `classify` / `affected_paths` / `permission_fingerprint`
//! 默认实现挪到这里集中分发，让 Tool trait 只剩 `name / description /
//! parameters_schema / execute` 四个真正属于「工具自我描述」的方法。
//!
//! `analyze_effects` 按工具名 dispatch 到对应的 helper：
//! - Ask → NeedsHumanInput（不进 HITL 审批，走 ask 通路）
//! - Bash / PowerShell → Destructive，fingerprint = 第一个命令 token，paths = cwd
//! - Read / Write / Edit / Glob → 解析 input.file_path / pattern
//! - Grep → 解析 input.path（缺省 workdir）
//! - WebSearch / Fetch → Network，按 URL 解析 domain
//! - Skill / TodoWrite / ExitPlanMode / BashOutput / KillShell → ReadOnly
//!
//! 路径解析失败、未知工具一律降级为 `Mutating(Medium)` 兜底——HITL 会按
//! "需要审批" 处理，比误判 ReadOnly 安全。

use std::path::PathBuf;

use protocol::RiskLevel;
use serde_json::Value;

use crate::tools::{safe_commands, shell_parse};

/// 工具调用的语义分类，对应原 `ToolClass`。Dispatcher 据此决定并发与 HITL 路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectClass {
    /// 只读：可并发执行，免审批。
    ReadOnly,
    /// 修改 workspace 内文件：串行 + 按 policy 询问。
    Mutating,
    /// 破坏性操作（执行命令）：串行 + 默认询问。
    Destructive,
    /// 网络访问：远端 channel 强制询问。
    Network,
    /// 走 HitlGate.ask 路径，向用户求助（Ask 工具专用）。
    NeedsHumanInput,
}

/// 单次工具调用解析出来的 effects。
///
/// `paths` 用于路径越界检查（filter workspace.allowed_dirs）。
/// `command_fingerprint` 用于命令级记忆（Bash 的 `git status` 前缀）。
/// `domain` 用于按域名匹配 PermissionRule。
#[derive(Debug, Clone)]
pub struct Effects {
    pub paths: Vec<PathBuf>,
    pub command_fingerprint: Option<String>,
    pub network: bool,
    pub domain: Option<String>,
    pub risk: RiskLevel,
    pub class: EffectClass,
    pub is_concurrent_safe: bool,
}

impl Effects {
    fn read_only() -> Self {
        Self {
            paths: Vec::new(),
            command_fingerprint: None,
            network: false,
            domain: None,
            risk: RiskLevel::Low,
            class: EffectClass::ReadOnly,
            is_concurrent_safe: true,
        }
    }

    fn needs_human_input() -> Self {
        Self {
            paths: Vec::new(),
            command_fingerprint: None,
            network: false,
            domain: None,
            risk: RiskLevel::Low,
            class: EffectClass::NeedsHumanInput,
            is_concurrent_safe: false,
        }
    }

    fn network(domain: Option<String>) -> Self {
        Self {
            paths: Vec::new(),
            command_fingerprint: None,
            network: true,
            domain,
            risk: RiskLevel::Medium,
            class: EffectClass::Network,
            is_concurrent_safe: false,
        }
    }

    fn mutating(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            command_fingerprint: None,
            network: false,
            domain: None,
            risk: RiskLevel::Medium,
            class: EffectClass::Mutating,
            is_concurrent_safe: false,
        }
    }
}

/// 按工具名 + 输入分析这次调用的 effects（架构 §4.4.2）。
///
/// 未知工具走 `Mutating(Medium)` 兜底：让 HITL 把关。
pub fn analyze_effects(tool_name: &str, input: &Value) -> Effects {
    match tool_name {
        "Ask" | "ask" => Effects::needs_human_input(),

        "Bash" | "PowerShell" => analyze_shell(input),

        "Read" => {
            let paths = file_path_paths(input, "file_path");
            Effects {
                paths,
                ..Effects::read_only()
            }
        }
        "Write" | "Edit" => {
            let paths = file_path_paths(input, "file_path");
            Effects::mutating(paths)
        }
        "Glob" => Effects {
            paths: file_path_paths(input, "path"),
            ..Effects::read_only()
        },
        "Grep" => Effects {
            paths: file_path_paths(input, "path"),
            ..Effects::read_only()
        },

        "WebSearch" => Effects::network(None),
        "Fetch" => {
            let domain = input
                .get("url")
                .and_then(|v| v.as_str())
                .and_then(|s| reqwest::Url::parse(s).ok())
                .and_then(|u| u.host_str().map(str::to_string));
            Effects::network(domain)
        }

        "Skill" | "TodoWrite" | "ExitPlanMode" | "BashOutput" | "KillShell" => {
            Effects::read_only()
        }

        _ => Effects::mutating(Vec::new()),
    }
}

/// 把 input[`field`] 当作绝对路径解析。缺省 / 空串返回空 Vec。
fn file_path_paths(input: &Value, field: &str) -> Vec<PathBuf> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect()
}

/// Bash / PowerShell 分析：
/// - paths = `cwd`（缺省时 dispatcher 在调用方按 workspace.workdir 补齐）
/// - command_fingerprint = 首个子命令的 `"root sub ..."`，便于命令级记忆
/// - class = 解析失败 / 不安全 → Destructive，全部子命令在白名单且无危险结构 → ReadOnly
fn analyze_shell(input: &Value) -> Effects {
    let paths: Vec<PathBuf> = input
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();

    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let Some(raw) = command else {
        return Effects {
            paths,
            command_fingerprint: None,
            network: false,
            domain: None,
            risk: RiskLevel::High,
            class: EffectClass::Destructive,
            is_concurrent_safe: false,
        };
    };

    let parsed = shell_parse::parse(raw);
    let (fingerprint, classify_readonly) = match &parsed {
        Ok(p) if !p.commands.is_empty() => {
            let fp = Some(p.commands[0].argv.join(" "));
            let ro = !p.dangerous && p.commands.iter().all(safe_commands::is_safe);
            (fp, ro)
        }
        _ => (Some(raw.to_string()), false),
    };

    if classify_readonly {
        Effects {
            paths,
            command_fingerprint: fingerprint,
            network: false,
            domain: None,
            risk: RiskLevel::Low,
            class: EffectClass::ReadOnly,
            is_concurrent_safe: true,
        }
    } else {
        Effects {
            paths,
            command_fingerprint: fingerprint,
            network: false,
            domain: None,
            risk: RiskLevel::High,
            class: EffectClass::Destructive,
            is_concurrent_safe: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ask_is_needs_human_input() {
        let e = analyze_effects("Ask", &json!({}));
        assert!(matches!(e.class, EffectClass::NeedsHumanInput));
        assert!(!e.is_concurrent_safe);
    }

    #[test]
    fn bash_ls_is_readonly() {
        let e = analyze_effects("Bash", &json!({"command": "ls -la"}));
        assert!(matches!(e.class, EffectClass::ReadOnly));
        assert_eq!(e.command_fingerprint.as_deref(), Some("ls -la"));
    }

    #[test]
    fn bash_rm_is_destructive() {
        let e = analyze_effects("Bash", &json!({"command": "rm -rf /tmp/x"}));
        assert!(matches!(e.class, EffectClass::Destructive));
        assert!(matches!(e.risk, RiskLevel::High));
        assert_eq!(e.command_fingerprint.as_deref(), Some("rm -rf /tmp/x"));
    }

    #[test]
    fn bash_cwd_becomes_path() {
        let e = analyze_effects("Bash", &json!({"command": "ls", "cwd": "/etc"}));
        assert_eq!(e.paths, vec![PathBuf::from("/etc")]);
    }

    #[test]
    fn write_is_mutating_with_file_path() {
        let e = analyze_effects("Write", &json!({"file_path": "/x/y.txt"}));
        assert!(matches!(e.class, EffectClass::Mutating));
        assert_eq!(e.paths, vec![PathBuf::from("/x/y.txt")]);
    }

    #[test]
    fn read_extracts_file_path_but_is_readonly() {
        let e = analyze_effects("Read", &json!({"file_path": "/etc/hosts"}));
        assert!(matches!(e.class, EffectClass::ReadOnly));
        assert_eq!(e.paths, vec![PathBuf::from("/etc/hosts")]);
    }

    #[test]
    fn fetch_extracts_domain() {
        let e = analyze_effects("Fetch", &json!({"url": "https://example.com/a"}));
        assert!(matches!(e.class, EffectClass::Network));
        assert!(e.network);
        assert_eq!(e.domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn web_search_is_network_without_domain() {
        let e = analyze_effects("WebSearch", &json!({"query": "hi"}));
        assert!(matches!(e.class, EffectClass::Network));
        assert!(e.domain.is_none());
    }

    #[test]
    fn skill_todo_exit_plan_are_readonly() {
        for t in ["Skill", "TodoWrite", "ExitPlanMode", "BashOutput", "KillShell"] {
            assert!(matches!(
                analyze_effects(t, &json!({})).class,
                EffectClass::ReadOnly
            ));
        }
    }

    #[test]
    fn unknown_tool_falls_back_to_mutating() {
        let e = analyze_effects("UnknownTool", &json!({}));
        assert!(matches!(e.class, EffectClass::Mutating));
    }

    #[test]
    fn bash_redirection_is_destructive() {
        let e = analyze_effects("Bash", &json!({"command": "echo hi > /tmp/x"}));
        assert!(matches!(e.class, EffectClass::Destructive));
    }
}
