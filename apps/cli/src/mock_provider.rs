//! 用于无网络环境快速验证的 mock provider。
//!
//! 输出固定的流式文本，不调用任何工具。`--mock` 启用。

use async_trait::async_trait;
use model_gateway::{
    client::ModelClient,
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent, Usage},
};
use common::CancelFlag;

pub struct MockClient;

impl MockClient {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ModelClient for MockClient {
    fn provider_id(&self) -> &str {
        "mock"
    }

    fn supports_streaming_tools(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        _req: ModelRequest,
        _cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Other("mock provider 仅支持 stream".into()))
    }

    async fn stream(
        &self,
        req: ModelRequest,
        _cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        // 把最后一条 user 消息回显出来，便于人眼验证多 turn 上下文传对了没
        let last_user = req
            .entries
            .iter()
            .rev()
            .find_map(|e| match e {
                model_gateway::types::TranscriptEntry::User(u) => Some(u.text.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let chunks = [
            "[mock] 收到：".to_string(),
            format!("「{last_user}」"),
            " — 这是一条假回复。".to_string(),
        ];
        let mut full = String::new();
        for chunk in &chunks {
            on_event(ModelStreamEvent::TextDelta {
                text: chunk.clone(),
            });
            full.push_str(chunk);
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            // 调试用：先 emit 一段 partial，再模拟「流式中途返回错误」（架构 §4.3 复现）。
            if std::env::var("HEBBIAN_MOCK_STREAM_ERROR").is_ok() {
                return Err(ModelError::Other(
                    "模型流式返回错误：{\"error\":{\"message\":\"upstream stream disconnected: unexpected EOF\",\"type\":\"stream_read_error\"},\"type\":\"error\"}".into(),
                ));
            }
        }
        Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
            text: full,
            reasoning: String::new(),
            reasoning_signature: String::new(),
            attachments: Vec::new(),
            usage: Usage {
                input_tokens: 8,
                output_tokens: 4,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        })
    }
}
