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
use tracing::info;

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

/// 一个 Bash 段的 effects（架构 §4.4.2 段级判定）。
///
/// 一整条 Bash 命令按 `&&` `||` `;` `|` 拆段后，每段产出一个 SegmentEffect。
/// PermissionStore 按段独立匹配，**全段 allow 才整体 allow，任一 deny 即整体 deny**。
#[derive(Debug, Clone)]
pub struct SegmentEffect {
    /// 段级 fingerprint：剥掉 timeout/nice/nohup/env 修饰符后的 `"base [sub]"`。
    /// **env-var 不在 fingerprint 内**——分离到 [`env_prefix`](Self::env_prefix)；命中敏感
    /// env-var 时 [`Effects::dangerous_kinds`] 会包含 `"sensitive-env-prefix"`（架构 §4.4.2.1）。
    pub fingerprint: String,
    /// 段内识别到的行内 env-var 赋值（`FOO=bar` / `RUST_LOG=info` 等）。
    pub env_prefix: Vec<String>,
    /// 段内识别到的写文件目标（重定向 / tee / sed -i / python open(...,'w')）。
    pub write_targets: Vec<String>,
    /// 该段是否只读（[`safe_commands::is_safe`]）。只读段**免审批、免记忆**：
    /// 段级权限匹配只要求"会写段"全部命中 allow，只读段直接跳过（架构 §4.4.2）。
    pub is_readonly: bool,
    /// 该段是否"不可记忆"（[`safe_commands::is_never_remember`]，如 `rm` / `dd`）。
    /// 命中时整条审批强制 refuse_remember，且该段永远不被记忆放行——每次确认（架构 §4.4.2.3）。
    pub unmemorable: bool,
}

/// 单次工具调用解析出来的 effects。
///
/// `paths` 用于路径越界检查（filter workspace.allowed_paths）；Bash/PowerShell 时
/// 包含 cwd + 所有段的写目标。
/// `command_fingerprint` = `segments[0].fingerprint`，保留旧字段做规则向后兼容；
/// PermissionStore 实际按 `segments` 段级匹配。
/// `dangerous_kinds` 命中任一种 → 强制 NeedsApproval 且 HitlGate 拒绝 AllowAndRemember。
#[derive(Debug, Clone)]
pub struct Effects {
    pub paths: Vec<PathBuf>,
    pub command_fingerprint: Option<String>,
    pub network: bool,
    pub domain: Option<String>,
    pub risk: RiskLevel,
    pub class: EffectClass,
    pub is_concurrent_safe: bool,
    /// Bash/PowerShell 的段级 effects。其它工具为空 vec。
    pub segments: Vec<SegmentEffect>,
    /// 命中的危险复合模式 label（cd-git-compound / write-git-meta / ...）。
    pub dangerous_kinds: Vec<String>,
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
            segments: Vec::new(),
            dangerous_kinds: Vec::new(),
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
            segments: Vec::new(),
            dangerous_kinds: Vec::new(),
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
            segments: Vec::new(),
            dangerous_kinds: Vec::new(),
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
            segments: Vec::new(),
            dangerous_kinds: Vec::new(),
        }
    }

    /// 是否命中任一危险复合模式（HitlGate 据此强制审批 + 拒绝 AllowAndRemember）。
    pub fn has_dangerous_pattern(&self) -> bool {
        !self.dangerous_kinds.is_empty()
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
        "Edit" => {
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

        "Skill" | "TodoWrite" | "ExitPlanMode" | "BashOutput" | "KillShell" => Effects::read_only(),

        // 记忆工具只动 hebbian 内部记忆库（~/.hebbian/.../memory/），不碰用户工作区文件——
        // 免审批。否则 agent 每记一条 / 每读一条都弹审批，体验灾难（架构 §4.14）。
        "ReadMemory" | "WriteMemory" => Effects::read_only(),

        name if name.starts_with("Mcp__") => {
            let server = input
                .get("_server_transport")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match server {
                "streamable_http" | "sse" => Effects::network(None),
                _ => Effects::mutating(Vec::new()),
            }
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

/// Bash / PowerShell 分析（架构 §4.4.2 段级判定）：
/// - segments：按 `&&` `||` `;` `|` 拆段，每段独立产 `fingerprint` + `write_targets`
/// - paths = cwd ∪ ⋃ segments[i].write_targets（write_targets 也走越界检查 +
///   Edit FilePath deny 规则，防 Bash 重定向绕过 Edit 规则）
/// - command_fingerprint = segments[0].fingerprint（向后兼容旧规则匹配点）
/// - dangerous_kinds 命中 → Destructive 且 HITL 不可记忆
/// - 全部子命令在白名单 + 无 dangerous → ReadOnly
fn analyze_shell(input: &Value) -> Effects {
    let mut paths: Vec<PathBuf> = input
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
            segments: Vec::new(),
            dangerous_kinds: Vec::new(),
        };
    };

    let parsed = shell_parse::parse(raw);
    let mut segments: Vec<SegmentEffect> = Vec::new();
    let mut dangerous_kinds: Vec<String> = Vec::new();
    let mut classify_readonly = false;
    let mut first_fingerprint: Option<String> = None;

    match &parsed {
        Ok(p) => {
            for cmd in &p.commands {
                let fingerprint = cmd.fingerprint();
                if first_fingerprint.is_none() {
                    first_fingerprint = Some(fingerprint.clone());
                }
                // 写目标合并到 effects.paths，让 workspace 越界检查 +
                // Edit FilePath deny 规则一起兜底
                for t in &cmd.write_targets {
                    paths.push(PathBuf::from(t));
                }
                segments.push(SegmentEffect {
                    fingerprint,
                    env_prefix: cmd.env_prefix.clone(),
                    write_targets: cmd.write_targets.clone(),
                    is_readonly: safe_commands::is_safe(cmd),
                    unmemorable: safe_commands::is_never_remember(cmd),
                });
            }
            for k in &p.dangerous_kinds {
                dangerous_kinds.push(k.label().to_string());
            }
            classify_readonly = !p.dangerous
                && p.dangerous_kinds.is_empty()
                && !p.commands.is_empty()
                && p.commands.iter().all(safe_commands::is_safe);
        }
        Err(_) => {
            // tokenize / unbalanced 失败：保守地把原文当 fingerprint，且强制 destructive
            first_fingerprint = Some(raw.to_string());
            dangerous_kinds.push("ast-too-complex".to_string());
        }
    }

    if first_fingerprint.is_none() {
        first_fingerprint = Some(raw.to_string());
    }

    // —— [Permission:Bash:Extract] 解析日志：逐段展示 fingerprint + 分类标记，
    //    并标出「哪些段会写、需走审批」。每段格式 `fp{ro|WRITE}[w:目标][!danger]`。——
    {
        let seg_list: Vec<String> = segments
            .iter()
            .map(|s| {
                let mut tag = String::new();
                tag.push_str(if s.is_readonly { "{ro}" } else { "{WRITE}" });
                if !s.write_targets.is_empty() {
                    tag.push_str(&format!("[w:{}]", s.write_targets.join(",")));
                }
                if s.unmemorable {
                    tag.push_str("[no-mem]");
                }
                format!("{}{}", s.fingerprint, tag)
            })
            .collect();
        // 会写段 = 真正会触发审批的段（只读段免审）
        let writable: Vec<&str> = segments
            .iter()
            .filter(|s| !s.is_readonly)
            .map(|s| s.fingerprint.as_str())
            .collect();
        let dangerous_list: Vec<&str> = dangerous_kinds.iter().map(|s| s.as_str()).collect();
        if classify_readonly {
            info!(
                target: "permission",
                segments = seg_list.join(" | "),
                "[Permission:Bash:Extract] 全段只读 → 自动放行，无需审批"
            );
        } else if dangerous_kinds.is_empty() {
            info!(
                target: "permission",
                segments = seg_list.join(" | "),
                writable_segments = writable.join(", "),
                "[Permission:Bash:Extract] 含会写段 → 这些段需审批（只读段免审）"
            );
        } else {
            info!(
                target: "permission",
                segments = seg_list.join(" | "),
                writable_segments = writable.join(", "),
                dangerous_kinds = ?dangerous_list,
                "[Permission:Bash:Extract] 含危险复合模式 → 强制审批且不可记忆"
            );
        }
    }

    if classify_readonly {
        Effects {
            paths,
            command_fingerprint: first_fingerprint,
            network: false,
            domain: None,
            risk: RiskLevel::Low,
            class: EffectClass::ReadOnly,
            is_concurrent_safe: true,
            segments,
            dangerous_kinds,
        }
    } else {
        Effects {
            paths,
            command_fingerprint: first_fingerprint,
            network: false,
            domain: None,
            risk: RiskLevel::High,
            class: EffectClass::Destructive,
            is_concurrent_safe: false,
            segments,
            dangerous_kinds,
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
        // 新 fingerprint 语义：剥掉 flag，留 base + 第一个非 flag 位置参数
        assert_eq!(e.command_fingerprint.as_deref(), Some("ls"));
        assert_eq!(e.segments.len(), 1);
        assert_eq!(e.segments[0].fingerprint, "ls");
    }

    #[test]
    fn bash_rm_is_destructive() {
        let e = analyze_effects("Bash", &json!({"command": "rm -rf /tmp/x"}));
        assert!(matches!(e.class, EffectClass::Destructive));
        assert!(matches!(e.risk, RiskLevel::High));
        // 新 fingerprint 语义：base + 首个非 flag 位置参数
        assert_eq!(e.command_fingerprint.as_deref(), Some("rm /tmp/x"));
    }

    #[test]
    fn bash_cwd_becomes_path() {
        let e = analyze_effects("Bash", &json!({"command": "ls", "cwd": "/etc"}));
        assert_eq!(e.paths, vec![PathBuf::from("/etc")]);
    }

    #[test]
    fn edit_is_mutating_with_file_path() {
        let e = analyze_effects("Edit", &json!({"file_path": "/x/y.txt"}));
        assert!(matches!(e.class, EffectClass::Mutating));
        assert_eq!(e.paths, vec![PathBuf::from("/x/y.txt")]);
    }

    #[test]
    fn read_extracts_file_path_but_is_readonly() {
        let e = analyze_effects("Read", &json!({"file_path": "/etc/hosts"}));
        assert!(matches!(e.class, EffectClass::ReadOnly));

        // 记忆工具免审批：只动内部记忆库（架构 §4.14）
        for t in ["ReadMemory", "WriteMemory"] {
            let e = analyze_effects(t, &json!({}));
            assert!(matches!(e.class, EffectClass::ReadOnly), "{t} 应免审批");
            assert!(e.paths.is_empty(), "{t} 不应带路径触发 PathAccess");
        }
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
        for t in [
            "Skill",
            "TodoWrite",
            "ExitPlanMode",
            "BashOutput",
            "KillShell",
        ] {
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
        // 重定向目标进 paths（让 Edit FilePath deny 规则统一兜底）
        assert!(e.paths.iter().any(|p| p == &PathBuf::from("/tmp/x")));
        // segments 第一段含写目标
        assert_eq!(e.segments.len(), 1);
        assert_eq!(e.segments[0].write_targets, vec!["/tmp/x".to_string()]);
    }

    #[test]
    fn bash_compound_cd_rm_carries_two_segments() {
        let e = analyze_effects("Bash", &json!({"command": "cd /tmp/safe && rm -rf foo"}));
        assert!(matches!(e.class, EffectClass::Destructive));
        assert_eq!(e.segments.len(), 2);
        assert_eq!(e.segments[0].fingerprint, "cd /tmp/safe");
        assert_eq!(e.segments[1].fingerprint, "rm foo");
    }

    #[test]
    fn bash_cd_git_compound_flagged() {
        let e = analyze_effects("Bash", &json!({"command": "cd /tmp/evil && git status"}));
        assert!(e.has_dangerous_pattern());
        assert!(e.dangerous_kinds.iter().any(|k| k == "cd-git-compound"));
    }

    #[test]
    fn bash_write_to_git_meta_flagged() {
        let e = analyze_effects(
            "Bash",
            &json!({"command": "echo evil > /repo/.git/hooks/post-merge"}),
        );
        assert!(e.has_dangerous_pattern());
        assert!(e.dangerous_kinds.iter().any(|k| k == "write-git-meta"));
        assert!(e
            .paths
            .iter()
            .any(|p| p == &PathBuf::from("/repo/.git/hooks/post-merge")));
    }

    #[test]
    fn bash_timeout_prefix_stripped_in_fingerprint() {
        let e = analyze_effects(
            "Bash",
            &json!({"command": "timeout 30 git push origin main"}),
        );
        assert_eq!(e.command_fingerprint.as_deref(), Some("git push"));
        assert_eq!(e.segments[0].fingerprint, "git push");
    }

    #[test]
    fn bash_sensitive_env_prefix_flagged() {
        // PYTHONPATH / LD_PRELOAD 等敏感 env-var 不再保留到 fingerprint，
        // 而是触发 dangerous_kinds=["sensitive-env-prefix"] 强制审批
        let e = analyze_effects(
            "Bash",
            &json!({"command": "PYTHONPATH=/tmp python3 script.py"}),
        );
        assert_eq!(e.command_fingerprint.as_deref(), Some("python3 script.py"));
        assert!(e
            .dangerous_kinds
            .iter()
            .any(|k| k == "sensitive-env-prefix"));
    }

    #[test]
    fn bash_inline_env_does_not_pollute_fingerprint() {
        // 普通 env-var：不触发 dangerous，fingerprint 也不被污染
        let e = analyze_effects("Bash", &json!({"command": "FOO=bar make all"}));
        assert_eq!(e.command_fingerprint.as_deref(), Some("make all"));
        assert!(!e
            .dangerous_kinds
            .iter()
            .any(|k| k == "sensitive-env-prefix"));
    }

    #[test]
    fn bash_heredoc_is_approvable_but_not_dangerous_no_remember() {
        let e = analyze_effects(
            "Bash",
            &json!({"command": "python3 - <<'PY'\nfrom pathlib import Path\nprint(Path('docs/架构.md').read_text())\nPY"}),
        );

        assert!(matches!(e.class, EffectClass::Destructive));
        assert_eq!(e.command_fingerprint.as_deref(), Some("python3"));
        assert_eq!(e.segments.len(), 1);
        assert!(e.dangerous_kinds.is_empty());
    }

    #[test]
    fn segments_carry_readonly_flags() {
        // cd 会写（不在只读白名单），grep / tail / wc 只读
        let e = analyze_effects(
            "Bash",
            &json!({"command": "cd src && grep -rn foo bar | tail -5 | wc -l"}),
        );
        assert_eq!(e.segments.len(), 4);
        assert_eq!(e.segments[0].fingerprint, "cd src");
        assert!(!e.segments[0].is_readonly, "cd 应判会写");
        assert!(e.segments[1].is_readonly, "grep 应判只读");
        assert!(e.segments[2].is_readonly, "tail 应判只读");
        assert!(e.segments[3].is_readonly, "wc 应判只读");
        assert!(e.segments.iter().all(|s| !s.unmemorable));
    }

    #[test]
    fn rm_segment_is_writable_and_unmemorable() {
        let e = analyze_effects("Bash", &json!({"command": "rm -rf ./build"}));
        assert_eq!(e.segments.len(), 1);
        assert!(!e.segments[0].is_readonly);
        assert!(e.segments[0].unmemorable, "rm 应标记不可记忆");
    }
}
