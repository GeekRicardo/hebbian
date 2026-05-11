//! AutoMode：在 destructive 工具调用前调一次轻量 LLM 决定是否放行。
//!
//! 架构 §4.4.4。流程：
//! 1. 仅当 `current_model_id == "claude-opus-4-7"` 时启用（其他模型直接降级 Ask）
//! 2. 构造 judge prompt（[`AUTOMODE_JUDGE_SYSTEM`] + 调用上下文）
//! 3. 一次 [`ModelClient::complete`] 拿首行决策
//! 4. 解析 `ALLOW` / `DENY: <reason>` / `ASK: <reason>`
//!
//! emit `PermissionAutoJudged { decision, reason }` 由调用方负责（dispatcher）。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;
use tracing::warn;

use model_gateway::client::ModelClient;
use model_gateway::types::{ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry};

/// AutoMode 的判官 system prompt（编译进二进制，跨会话稳定）。
pub const AUTOMODE_JUDGE_SYSTEM: &str = include_str!("../prompts/automode_judge.md");

/// 当前 AutoMode 唯一支持的模型 id。
pub const AUTOMODE_REQUIRED_MODEL: &str = "claude-opus-4-7";

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
}

/// 调一次模型作为 AutoMode 判官。
///
/// `judge_client` 通常等于会话的主 client（同 model id 才符合限定）。本函数会先校验
/// `current_model_id`，不匹配直接返回 `Ask`，不发请求。
pub async fn judge_auto_mode(
    judge_client: &Arc<dyn ModelClient>,
    current_model_id: &str,
    tool_name: &str,
    tool_input: &Value,
    recent_transcript: &[TranscriptEntry],
) -> AutoModeDecision {
    if current_model_id != AUTOMODE_REQUIRED_MODEL {
        return AutoModeDecision::Ask(format!(
            "AutoMode 仅在 {AUTOMODE_REQUIRED_MODEL} 启用；当前模型 {current_model_id} 不支持自动判断"
        ));
    }

    // 拼装 user prompt：把上下文渲染成一段 JSON 描述
    let prompt = format_judge_prompt(tool_name, tool_input, recent_transcript);

    let request = ModelRequest {
        model: current_model_id.to_string(),
        system: Some(AUTOMODE_JUDGE_SYSTEM.to_string()),
        entries: vec![TranscriptEntry::User(UserEntry::text(prompt))],
        tools: Vec::new(),
        max_tokens: 200,
        reasoning: None,
    };

    let cancel = Arc::new(AtomicBool::new(false));
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

fn extract_text(resp: &ModelResponse) -> String {
    match resp {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text.clone(),
    }
}

fn format_judge_prompt(
    tool_name: &str,
    tool_input: &Value,
    recent_transcript: &[TranscriptEntry],
) -> String {
    let recent: Vec<String> = recent_transcript
        .iter()
        .rev()
        .take(5)
        .rev()
        .map(summarize_entry)
        .collect();

    let input_pretty = serde_json::to_string(tool_input)
        .unwrap_or_else(|_| tool_input.to_string());

    format!(
        "tool: {tool_name}\ninput: {input_pretty}\nrecent_transcript:\n{}\n\n按 system prompt 的格式输出一行决策。",
        recent.join("\n")
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
        // 无法识别的输出：保守降级
        AutoModeDecision::Ask(format!("AutoMode judge 返回未识别格式：{}", trim(first, 120)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_allow() {
        assert!(matches!(parse_decision("ALLOW"), AutoModeDecision::Allow));
        assert!(matches!(parse_decision("allow\nmore text"), AutoModeDecision::Allow));
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
}
