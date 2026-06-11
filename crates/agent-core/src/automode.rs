//! AutoMode：在 destructive 工具调用前调一次轻量 LLM 决定是否放行。
//!
//! 架构 §4.4.4。流程：
//! 1. 仅当 `current_model_id` 命中用户配置的 AutoMode 白名单时启用——dispatcher 先判，
//!    不支持的模型不调判官，改成 toast 提示 + 降级普通审批（架构 §4.4.4）
//! 2. 构造 judge prompt：[`AUTOMODE_JUDGE_SYSTEM`] + 调用上下文 + hebbian 已识别的
//!    `effects.segments` / `write_targets` / `paths` / `dangerous_kinds`。判官**不重复**
//!    解析 shell，只在静态分析结果上做语境判定。
//! 3. 一次 [`ModelClient::complete`] 拿首行决策
//! 4. 解析 `ALLOW` / `DENY: <reason>` / `ASK: <reason>`；判官 reason 按段拆解风险，由
//!    HITL 弹窗原样展示给用户
//!
//! emit `PermissionAutoJudged { decision, reason }` 由调用方负责（dispatcher）。
//!
//! `force_automode` 子开关由调用方在拿到 `Ask` 后自行折叠成 `Deny`，本模块不处理——
//! 让 dispatcher 控制策略，automode 只负责 LLM 判定。

use std::sync::Arc;

use serde_json::Value;
use tracing::warn;

use common::reasoning::normalize_model_id;
use model_gateway::client::ModelClient;
use model_gateway::types::{ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry};

use crate::effects::Effects;
use crate::storage::settings::AppLanguage;
use crate::tools::{bash_prefix, shell_parse};

/// AutoMode 的判官 system prompt（编译进二进制，跨会话稳定）。
pub const AUTOMODE_JUDGE_SYSTEM: &str = include_str!("../prompts/automode_judge.md");

/// 判定 model id 是否在 AutoMode 白名单内。`allowed` 来自
/// [`crate::storage::settings`]`::Settings.general.automode_models`（用户在设置里多选，
/// 内置默认见 [`crate::storage::settings::default_automode_models`]）。
///
/// 双方都归一化（大小写 + 版本分隔符）后匹配：归一化相等，或 model 归一化以某条目
/// 归一化为前缀（兼容带日期后缀的真实 id，如 `claude-opus-4-7-20260416` 命中
/// `claude-opus-4-7`）。空白名单 → 全部不允许。模型质量由配置者负责。
pub fn is_allowed_model(model_id: &str, allowed: &[String]) -> bool {
    let normalized = normalize_model_id(model_id);
    allowed.iter().any(|a| {
        let na = normalize_model_id(a);
        !na.is_empty() && (normalized == na || normalized.starts_with(na.as_str()))
    })
}

/// AutoMode 的判官决策。
#[derive(Debug, Clone)]
pub enum AutoModeDecision {
    Allow,
    Deny(String),
    Ask(String),
}

impl AutoModeDecision {
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny(_) => "deny",
            Self::Ask(_) => "ask",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Allow => None,
            Self::Deny(r) | Self::Ask(r) => Some(r.as_str()),
        }
    }

    /// 把 `Ask(reason)` 折叠成 `Deny`，并在 reason 头部加 `force-automode:` 前缀；
    /// `Allow` / `Deny` 不变。
    ///
    /// 调用方在 `force_automode = true` 且 RunMode = AutoMode 时使用——含义：用户开了
    /// "放手跑、不打断我"开关，判官拿不准的动作直接拒，agent 自己换路子。
    pub fn collapse_ask_to_deny(self) -> Self {
        match self {
            Self::Ask(reason) => Self::Deny(format!("force-automode: {reason}")),
            other => other,
        }
    }
}

/// 调一次模型作为 AutoMode 判官。
///
/// `judge_client` 通常等于会话的主 client。白名单判断已由 dispatcher 在调用前完成
/// （见 [`is_allowed_model`]），本函数假定模型已获准启用判官，不再重复校验。
///
/// `effects` 必须传 hebbian 已分析好的结果——判官 prompt 依赖 `segments` / `paths` /
/// `dangerous_kinds` 做段级拆解，**不能**让判官自己重新解析 shell。
pub async fn judge_auto_mode(
    judge_client: &Arc<dyn ModelClient>,
    current_model_id: &str,
    tool_name: &str,
    tool_input: &Value,
    effects: &Effects,
    recent_transcript: &[TranscriptEntry],
    whitelisted_fingerprints: &[String],
    language: AppLanguage,
    cancel: common::CancelFlag,
) -> AutoModeDecision {
    let prompt = format_judge_prompt(
        tool_name,
        tool_input,
        effects,
        recent_transcript,
        whitelisted_fingerprints,
        language,
    );

    let request = ModelRequest {
        model: current_model_id.to_string(),
        system: Some(AUTOMODE_JUDGE_SYSTEM.to_string()),
        entries: vec![TranscriptEntry::User(UserEntry::text(prompt))],
        tools: Vec::new(),
        // ASK reason 要按段拆解，原 200 不够；保守留 300 token 上限。
        max_tokens: 300,
        reasoning: None,
    };

    // 必须传 dispatcher 的真实 cancel——用户点中断时这个 judge LLM 请求要能立即停，
    // 否则 AutoMode 自动审批阶段中断按钮失效（judge 照样跑完才返回）。
    match judge_client.complete(request, cancel).await {
        Ok(resp) => parse_decision(&extract_text(&resp)),
        Err(ModelError::Cancelled) => {
            AutoModeDecision::Ask("AutoMode judge 调用被取消".to_string())
        }
        Err(err) => {
            warn!(tool = %tool_name, %err, "automode judge 调用失败，降级到 Ask");
            AutoModeDecision::Ask(format!("AutoMode judge 失败：{err}"))
        }
    }
}

#[derive(Debug, Clone)]
pub struct BashPrefixClassifierOutcome {
    pub effects: Effects,
    pub command_injection_detected: bool,
}

/// AutoMode-only Classifier A pass. It enriches the effects sent to the judge with
/// LLM-extracted Bash prefixes. Normal permission matching keeps the static
/// tree-sitter fingerprint so ordinary Bash calls do not pay an extra model round-trip.
pub async fn classify_bash_prefixes_for_automode(
    judge_client: &Arc<dyn ModelClient>,
    current_model_id: &str,
    tool_name: &str,
    tool_input: &Value,
    effects: &Effects,
    cancel: common::CancelFlag,
) -> BashPrefixClassifierOutcome {
    let mut enriched = effects.clone();
    let mut command_injection_detected = false;

    // 白名单已由 dispatcher 在调用前确认，这里只判工具类型（非 Bash/PowerShell 不 classify）。
    if !matches!(tool_name, "Bash" | "PowerShell") {
        return BashPrefixClassifierOutcome {
            effects: enriched,
            command_injection_detected,
        };
    }

    let Some(raw) = tool_input
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return BashPrefixClassifierOutcome {
            effects: enriched,
            command_injection_detected,
        };
    };

    let Ok(parsed) = shell_parse::parse(raw) else {
        return BashPrefixClassifierOutcome {
            effects: enriched,
            command_injection_detected,
        };
    };

    for (index, cmd) in parsed.commands.iter().enumerate() {
        let segment_text = cmd.argv.join(" ");
        let classified = match bash_prefix::classify_prefix(
            judge_client,
            current_model_id,
            &segment_text,
            cancel.clone(),
        )
        .await
        {
            Ok(Some(result)) => result,
            Ok(None) => continue,
            Err(err) => {
                warn!(%err, segment = %segment_text, "bash prefix classifier failed");
                continue;
            }
        };

        match classified {
            bash_prefix::BashPrefix::Prefix(prefix) => {
                if let Some(seg) = enriched.segments.get_mut(index) {
                    seg.fingerprint = prefix.clone();
                    if index == 0 {
                        enriched.command_fingerprint = Some(prefix);
                    }
                }
            }
            bash_prefix::BashPrefix::None => {}
            bash_prefix::BashPrefix::CommandInjectionDetected => {
                command_injection_detected = true;
                if !enriched
                    .dangerous_kinds
                    .iter()
                    .any(|kind| kind == "ast-too-complex")
                {
                    enriched.dangerous_kinds.push("ast-too-complex".to_string());
                }
            }
        }
    }

    BashPrefixClassifierOutcome {
        effects: enriched,
        command_injection_detected,
    }
}

fn extract_text(resp: &ModelResponse) -> String {
    match resp {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text.clone(),
    }
}

fn format_judge_prompt(
    tool_name: &str,
    tool_input: &Value,
    effects: &Effects,
    recent_transcript: &[TranscriptEntry],
    whitelisted_fingerprints: &[String],
    language: AppLanguage,
) -> String {
    let recent: Vec<String> = recent_transcript
        .iter()
        .rev()
        .take(5)
        .rev()
        .map(summarize_entry)
        .collect();

    let input_pretty = serde_json::to_string(tool_input).unwrap_or_else(|_| tool_input.to_string());

    let segments_block = if effects.segments.is_empty() {
        "  (single-segment tool, no Bash split applies)".to_string()
    } else {
        effects
            .segments
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                let env = if seg.env_prefix.is_empty() {
                    String::new()
                } else {
                    format!(" env={:?}", seg.env_prefix)
                };
                let targets = if seg.write_targets.is_empty() {
                    String::new()
                } else {
                    format!(" write_targets={:?}", seg.write_targets)
                };
                // 该段命中用户已存的 allow 规则 / session 记忆 → 标注，让判官放心 ALLOW
                // （用户先前已显式批准过这条命令，架构 §4.4.4）。
                let allowed = if whitelisted_fingerprints.contains(&seg.fingerprint) {
                    " [user-allowed]"
                } else {
                    ""
                };
                format!(
                    "  [{}] fingerprint={:?}{}{}{}",
                    i + 1,
                    seg.fingerprint,
                    env,
                    targets,
                    allowed
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let paths_block = if effects.paths.is_empty() {
        "(none)".to_string()
    } else {
        effects
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let dangerous_block = if effects.dangerous_kinds.is_empty() {
        "(none)".to_string()
    } else {
        effects.dangerous_kinds.join(", ")
    };

    let network_block = if effects.network {
        format!("yes (domain={})", effects.domain.as_deref().unwrap_or("?"))
    } else {
        "no".to_string()
    };

    format!(
        "tool: {tool_name}\n\
         input: {input_pretty}\n\
         \n\
         effects:\n\
           class: {class:?}\n\
           command_fingerprint: {fingerprint}\n\
           paths: {paths_block}\n\
           network: {network_block}\n\
           dangerous_kinds: {dangerous_block}\n\
         segments:\n\
         {segments_block}\n\
         \n\
         reason_language: {reason_language}\n\
         \n\
         recent_transcript (oldest first):\n\
         {recent}\n\
         \n\
         Output one line per the system prompt's format.",
        class = effects.class,
        fingerprint = effects.command_fingerprint.as_deref().unwrap_or("(n/a)"),
        reason_language = language.judge_reason_instruction(),
        recent = recent.join("\n"),
    )
}

fn summarize_entry(entry: &TranscriptEntry) -> String {
    match entry {
        TranscriptEntry::User(u) => format!("- user: {}", trim(&u.text, 200)),
        TranscriptEntry::Assistant(a) => format!("- assistant: {}", trim(&a.text, 200)),
        TranscriptEntry::ToolResults(results) => {
            let summary: Vec<String> = results
                .iter()
                .map(|t| format!("{}={}", t.name, trim(&t.content, 80)))
                .collect();
            format!("- tool_results: {}", summary.join(" / "))
        }
    }
}

fn trim(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    }
}

fn parse_decision(raw: &str) -> AutoModeDecision {
    let first = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");

    if let Some(rest) = first.strip_prefix("DENY:") {
        AutoModeDecision::Deny(rest.trim().to_string())
    } else if let Some(rest) = first.strip_prefix("ASK:") {
        AutoModeDecision::Ask(rest.trim().to_string())
    } else if first.eq_ignore_ascii_case("ALLOW") {
        AutoModeDecision::Allow
    } else if first.is_empty() {
        AutoModeDecision::Ask("AutoMode judge 返回空响应".to_string())
    } else {
        // 任何非 ALLOW/DENY/ASK 开头的响应 → fail-closed 兜底为 Ask，
        // 由用户最终拍板。绝不静默 Allow。
        AutoModeDecision::Ask(format!(
            "AutoMode judge 返回未识别格式：{}",
            trim(first, 120)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use common::CancelFlag;
    use model_gateway::types::{ModelStreamEvent, Usage};
    use serde_json::json;

    struct StaticClassifierClient {
        output: &'static str,
    }

    #[async_trait]
    impl ModelClient for StaticClassifierClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: self.output.to_string(),
                reasoning: String::new(),
                attachments: Vec::new(),
                usage: Usage::default(),
                reasoning_signature: String::new(),
            })
        }

        async fn stream(
            &self,
            req: ModelRequest,
            cancel: CancelFlag,
            _on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            self.complete(req, cancel).await
        }
    }

    #[test]
    fn parse_allow() {
        assert!(matches!(parse_decision("ALLOW"), AutoModeDecision::Allow));
        assert!(matches!(
            parse_decision("allow\nmore text"),
            AutoModeDecision::Allow
        ));
    }

    #[test]
    fn parse_deny() {
        let d = parse_decision("DENY: rm -rf 根目录");
        match d {
            AutoModeDecision::Deny(r) => assert_eq!(r, "rm -rf 根目录"),
            _ => panic!("expected Deny"),
        }
    }

    #[test]
    fn parse_ask() {
        let d = parse_decision("ASK: 不确定意图");
        assert!(matches!(d, AutoModeDecision::Ask(_)));
    }

    #[test]
    fn parse_unknown_falls_back_to_ask() {
        assert!(matches!(parse_decision("MAYBE"), AutoModeDecision::Ask(_)));
        assert!(matches!(parse_decision(""), AutoModeDecision::Ask(_)));
    }

    #[test]
    fn judge_prompt_carries_reason_language() {
        let effects = crate::effects::analyze_effects("Edit", &json!({"file_path": "/tmp/a"}));
        let zh = format_judge_prompt(
            "Edit",
            &json!({"file_path": "/tmp/a"}),
            &effects,
            &[],
            &[],
            AppLanguage::ZhCn,
        );
        let en = format_judge_prompt(
            "Edit",
            &json!({"file_path": "/tmp/a"}),
            &effects,
            &[],
            &[],
            AppLanguage::En,
        );
        assert!(zh.contains("Simplified Chinese"));
        assert!(en.contains("English"));
    }

    #[test]
    fn allowed_model_accepts_real_supported_ids() {
        let allowed = crate::storage::settings::default_automode_models();
        // 内置默认（claude-opus-4-7 / 4-8 / gpt-5.5）+ 真实上游 / 网关 id 归一化命中
        assert!(is_allowed_model("claude-opus-4-7", &allowed));
        assert!(is_allowed_model("claude-opus-4.7", &allowed)); // 分隔符归一
        assert!(is_allowed_model("claude-opus-4-7-20260416", &allowed)); // 日期后缀前缀匹配
        assert!(is_allowed_model("claude-opus-4-8", &allowed));
        assert!(is_allowed_model("gpt-5.5", &allowed));
        assert!(is_allowed_model("gpt-5-5", &allowed));
        assert!(is_allowed_model("GPT-5.5", &allowed)); // 大小写归一
    }

    #[test]
    fn allowed_model_rejects_unsupported_and_empty_list() {
        let allowed = crate::storage::settings::default_automode_models();
        assert!(!is_allowed_model("claude-sonnet-4-6", &allowed));
        assert!(!is_allowed_model("gpt-5.4", &allowed));
        assert!(!is_allowed_model("gpt-4o", &allowed));
        // 空白名单 → 全部不允许
        assert!(!is_allowed_model("claude-opus-4-7", &[]));
        // 用户没把某模型加进白名单 → 不允许（即使是强模型）
        assert!(!is_allowed_model(
            "claude-opus-4-8",
            &["claude-opus-4-7".to_string()]
        ));
    }

    #[test]
    fn collapse_ask_to_deny_prefixes_reason() {
        let collapsed = AutoModeDecision::Ask("reads SSH key".into()).collapse_ask_to_deny();
        match collapsed {
            AutoModeDecision::Deny(r) => assert!(r.starts_with("force-automode: "), "{r}"),
            _ => panic!("expected Deny"),
        }
        // Allow / Deny 不变
        assert!(matches!(
            AutoModeDecision::Allow.collapse_ask_to_deny(),
            AutoModeDecision::Allow
        ));
        assert!(matches!(
            AutoModeDecision::Deny("x".into()).collapse_ask_to_deny(),
            AutoModeDecision::Deny(_)
        ));
    }

    #[tokio::test]
    async fn bash_prefix_classifier_enriches_judge_effects() {
        let effects =
            crate::effects::analyze_effects("Bash", &json!({"command": "python3 script.py arg"}));
        assert_eq!(
            effects.command_fingerprint.as_deref(),
            Some("python3 script.py")
        );

        let client: Arc<dyn ModelClient> = Arc::new(StaticClassifierClient {
            output: "prefix: python3",
        });
        let outcome = classify_bash_prefixes_for_automode(
            &client,
            "gpt-5.5",
            "Bash",
            &json!({"command": "python3 script.py arg"}),
            &effects,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;

        assert!(!outcome.command_injection_detected);
        assert_eq!(
            outcome.effects.command_fingerprint.as_deref(),
            Some("python3")
        );
        assert_eq!(outcome.effects.segments[0].fingerprint, "python3");
    }

    #[tokio::test]
    async fn bash_prefix_classifier_marks_injection_for_judge() {
        let effects = crate::effects::analyze_effects("Bash", &json!({"command": "echo ok"}));
        let client: Arc<dyn ModelClient> = Arc::new(StaticClassifierClient {
            output: "command_injection_detected",
        });
        let outcome = classify_bash_prefixes_for_automode(
            &client,
            "gpt-5.5",
            "Bash",
            &json!({"command": "echo ok"}),
            &effects,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;

        assert!(outcome.command_injection_detected);
        assert!(outcome
            .effects
            .dangerous_kinds
            .iter()
            .any(|kind| kind == "ast-too-complex"));
    }
}
