//! 用于协议测试的确定性 mock provider。
//!
//! 行为：
//! - 第一次调用：发出几段 TextDelta，可选发出一个 ToolCall
//! - 后续调用（如果上一轮有 tool call）：发出最终回答 Done
//!
//! 不依赖网络、确定可重放。

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use model_gateway::{
    client::ModelClient,
    types::{
        ModelError, ModelRequest, ModelResponse, ModelStreamEvent, ToolCall, ToolCallStreamDelta,
        Usage,
    },
};
use platform::CancelFlag;

pub struct MockClient {
    pub emit_tool_call: bool,
    calls: AtomicUsize,
}

impl MockClient {
    pub fn new(emit_tool_call: bool) -> Self {
        Self {
            emit_tool_call,
            calls: AtomicUsize::new(0),
        }
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
        // 非 stream 路径不支持
        Err(ModelError::Other("mock provider 仅支持 stream".into()))
    }

    async fn stream(
        &self,
        _req: ModelRequest,
        _cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        // 模拟流式延迟
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        if n == 0 && self.emit_tool_call {
            on_event(ModelStreamEvent::TextDelta {
                text: "我先调用一下工具：".into(),
            });
            on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                index: 0,
                id: Some("call_mock_1".into()),
                name: Some("mock_tool".into()),
                arguments_delta: Some("{\"q\":\"hello\"}".into()),
            }));
            return Ok(ModelResponse::ToolCalls {
                text: String::new(),
                calls: vec![ToolCall {
                    id: "call_mock_1".into(),
                    name: "mock_tool".into(),
                    input: serde_json::json!({"q": "hello"}),
                }],
                attachments: Vec::new(),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            });
        }

        on_event(ModelStreamEvent::TextDelta {
            text: "你好".into(),
        });
        on_event(ModelStreamEvent::TextDelta {
            text: "，世界！".into(),
        });
        Ok(ModelResponse::Done {
            text: "你好，世界！".into(),
            attachments: Vec::new(),
            usage: Usage {
                input_tokens: 8,
                output_tokens: 4,
            },
        })
    }
}
