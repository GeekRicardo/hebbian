//! Session 标题自动生成（utility 短调用）。
//!
//! 用一次「无工具、关思考、短输出」的 LLM 调用，把用户首条消息提炼成一个对话标题。
//! 不进 agent loop，也不进 transcript——纯辅助。
//!
//! 关键点：
//! - `ReasoningConfig.enabled = Some(false)`。对 DeepSeek thinking 模型，这会让
//!   `model-gateway::protocols::openai::apply_deepseek_compat` 走「thinking: disabled」
//!   分支，避免短输出耗在推理上 / 触发 thinking 最小 32K 预算的硬下限。
//! - `max_tokens` 给到 128 就够。
//!
//! 触发时机由 surface / 上层 Session 决定（当前未自动挂钩，调用方按需触发）。
//! 后续若有压缩摘要、关键词抽取等更多 utility 场景，再考虑是否升级成正式 mode 概念。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use common::{CancelFlag, ReasoningConfig};
use model_gateway::client::ModelClient;
use model_gateway::types::{
    ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry,
};

const TITLE_INSTRUCTION: &str =
    "为以下用户消息生成一个 4-12 字的中文对话主题作为标题，只输出标题本身，\
     不要引号、标点、解释或前后缀。\n\n用户消息：\n";

const TITLE_MAX_TOKENS: u32 = 128;
const TITLE_MAX_CHARS: usize = 32;
const TITLE_TRIM_CHARS: &[char] = &[
    ' ', '\t', '\n', '"', '\'', '`', '「', '」', '“', '”', '【', '】', '(', ')', '（', '）',
];

/// 用「关思考」的 utility 短调用让模型从用户首条消息提炼对话标题。
/// 调用方负责把返回值写入 `Session.title`（建议先 trim 检查非空再写）。
pub async fn generate_title(
    client: &dyn ModelClient,
    model: &str,
    user_message: &str,
) -> Result<String, ModelError> {
    let trimmed_user = user_message.trim();
    if trimmed_user.is_empty() {
        return Ok(String::new());
    }
    let prompt = format!("{TITLE_INSTRUCTION}{trimmed_user}");
    let req = ModelRequest {
        model: model.into(),
        system: None,
        entries: vec![TranscriptEntry::User(UserEntry::text(prompt))],
        tools: vec![],
        max_tokens: TITLE_MAX_TOKENS,
        reasoning: Some(ReasoningConfig {
            enabled: Some(false),
            effort: None,
            long_context: None,
        }),
    };
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let raw_text = match client.complete(req, cancel).await? {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text,
    };
    Ok(sanitize_title(&raw_text))
}

fn sanitize_title(raw: &str) -> String {
    let first_line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let cleaned = first_line.trim_matches(|c: char| TITLE_TRIM_CHARS.contains(&c));
    if cleaned.is_empty() {
        return String::new();
    }
    if cleaned.chars().count() > TITLE_MAX_CHARS {
        cleaned.chars().take(TITLE_MAX_CHARS).collect()
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use model_gateway::types::{ModelStreamEvent, Usage};

    struct StaticClient {
        last_req: std::sync::Mutex<Option<ModelRequest>>,
        reply: String,
    }

    #[async_trait]
    impl ModelClient for StaticClient {
        fn provider_id(&self) -> &str {
            "static"
        }
        async fn complete(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            *self.last_req.lock().unwrap() = Some(req);
            Ok(ModelResponse::Done {
                text: self.reply.clone(),
                reasoning: String::new(),
                attachments: vec![],
                usage: Usage::default(),
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

    #[tokio::test]
    async fn passes_reasoning_disabled_to_client() {
        let client = StaticClient {
            last_req: std::sync::Mutex::new(None),
            reply: "天气查询".into(),
        };
        let title = generate_title(&client, "deepseek-v4-pro", "北京今天热不热")
            .await
            .unwrap();
        assert_eq!(title, "天气查询");
        let req = client.last_req.lock().unwrap().clone().unwrap();
        let cfg = req.reasoning.expect("reasoning should be Some");
        assert_eq!(cfg.enabled, Some(false));
        assert!(req.tools.is_empty());
        assert_eq!(req.max_tokens, TITLE_MAX_TOKENS);
    }

    #[tokio::test]
    async fn empty_user_message_returns_empty_without_call() {
        let client = StaticClient {
            last_req: std::sync::Mutex::new(None),
            reply: "不应被使用".into(),
        };
        let title = generate_title(&client, "deepseek-v4-pro", "   ")
            .await
            .unwrap();
        assert_eq!(title, "");
        assert!(client.last_req.lock().unwrap().is_none());
    }

    #[test]
    fn sanitize_strips_quotes_and_caps_length() {
        assert_eq!(sanitize_title("「天气查询」"), "天气查询");
        assert_eq!(sanitize_title("\"hello world\""), "hello world");
        assert_eq!(sanitize_title("  多余空格  "), "多余空格");
        // 多行：取第一非空行
        assert_eq!(
            sanitize_title("\n\n第一行标题\n后续解释不应保留"),
            "第一行标题"
        );
        // 超长截断
        let long: String = std::iter::repeat('字').take(40).collect();
        let cut = sanitize_title(&long);
        assert_eq!(cut.chars().count(), 32);
    }
}
