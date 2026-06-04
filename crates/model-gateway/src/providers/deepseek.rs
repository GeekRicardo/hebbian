//! DeepSeek `chat.deepseek.com` web protocol provider。
//!
//! 与 ds2api 等价路径：
//! 1. POST `/api/v0/chat_session/create` 拿 session_id（按 provider 缓存一份）
//! 2. POST `/api/v0/chat/create_pow_challenge` 拿 PoW challenge
//! 3. 本地求解 → 算 `x-ds-pow-response`
//! 4. POST `/api/v0/chat/completion` 流式拉路径式 SSE
//! 5. 边解析边把 Text / Thinking / 出现在最终输出里的 `<tool_calls>` 块
//!    翻译成 `ModelStreamEvent`。
//!
//! 不在内存里跨请求保留 parent_message_id（每次发完整 transcript），与 ds2api 默认行为一致。

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use std::sync::Arc;

use crate::auth::deepseek_pow::{self, DeepseekChallenge};
use crate::config::Provider;
use crate::protocols::deepseek as proto;
use crate::types::{
    has_image_generation_tool, ModelError, ModelRequest, ModelResponse, ModelStreamEvent,
    ToolCallStreamDelta, Usage,
};
use crate::{client::ModelClient, providers::apply_auth};
use common::CancelFlag;

const SESSION_CREATE_URL: &str = "https://chat.deepseek.com/api/v0/chat_session/create";
const POW_URL: &str = "https://chat.deepseek.com/api/v0/chat/create_pow_challenge";
const COMPLETION_URL: &str = "https://chat.deepseek.com/api/v0/chat/completion";
const COMPLETION_TARGET_PATH: &str = "/api/v0/chat/completion";

pub struct DeepseekClient {
    provider: Provider,
    http: reqwest::Client,
    /// 缓存的 chat_session_id；多轮共用同一个 session 节省创建请求。
    session_id: Arc<Mutex<Option<String>>>,
}

impl DeepseekClient {
    pub fn new(provider: Provider) -> Result<Self, ModelError> {
        let http = super::build_http_client()?;
        Ok(Self {
            provider,
            http,
            session_id: Arc::new(Mutex::new(None)),
        })
    }

    async fn ensure_session(&self) -> Result<String, ModelError> {
        if let Some(id) = self.session_id.lock().clone() {
            return Ok(id);
        }
        let resp = apply_auth(self.http.post(SESSION_CREATE_URL), &self.provider)
            .json(&serde_json::json!({"agent": "chat"}))
            .send()
            .await?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        if status >= 400 {
            return Err(ModelError::Http { status, body: text });
        }
        let v: Value = serde_json::from_str(&text)?;
        let id = v
            .pointer("/data/biz_data/id")
            .and_then(Value::as_str)
            .or_else(|| {
                v.pointer("/data/biz_data/chat_session/id")
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| ModelError::Other("DeepSeek create_session 响应缺少 id".into()))?
            .to_string();
        *self.session_id.lock() = Some(id.clone());
        Ok(id)
    }

    async fn fetch_pow_header(&self) -> Result<String, ModelError> {
        let resp = apply_auth(self.http.post(POW_URL), &self.provider)
            .json(&serde_json::json!({"target_path": COMPLETION_TARGET_PATH}))
            .send()
            .await?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        if status >= 400 {
            return Err(ModelError::Http { status, body: text });
        }
        let v: Value = serde_json::from_str(&text)?;
        let challenge_v = v
            .pointer("/data/biz_data/challenge")
            .ok_or_else(|| ModelError::Other("DeepSeek pow 响应缺少 challenge".into()))?;
        let challenge: DeepseekChallenge = serde_json::from_value(challenge_v.clone())
            .map_err(|e| ModelError::Other(format!("PoW challenge 解析失败: {e}")))?;
        deepseek_pow::solve_and_build_header(&challenge)
            .map_err(|e| ModelError::Other(e.to_string()))
    }
}

#[async_trait]
impl ModelClient for DeepseekClient {
    fn provider_id(&self) -> &str {
        &self.provider.id
    }

    fn supports_streaming_tools(&self) -> bool {
        // 我们的 ToolCallDelta 是在 SSE 收完后从最终文本里一次性解析的，
        // 但仍在 stream 返回前发出，对 agent_loop 来说和原生流式 tool_call 等价。
        // 返回 true 让 agent_loop 在含工具的 turn 也走流式路径，否则
        // TextDelta / ReasoningDelta 会被 complete 路径吞掉。
        true
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        if has_image_generation_tool(&req.tools) {
            return Err(ModelError::Other(
                "DeepSeek web 协议不支持 image_generation".into(),
            ));
        }
        // 极少数路径（surface 显式调用 complete）：仍然走 SSE，
        // 但事件丢弃 —— Done.text 已经是完整正文（thinking 已被分流）。
        self.stream(req, cancel, &|_| {}).await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        if has_image_generation_tool(&req.tools) {
            return Err(ModelError::Other(
                "DeepSeek web 协议不支持 image_generation".into(),
            ));
        }

        let session_id = self.ensure_session().await?;
        let pow_header = self.fetch_pow_header().await?;

        let prompt = proto::build_prompt(&req, &req.tools);
        let body = proto::build_completion_body(&session_id, None, &prompt, &req.model);
        let thinking_enabled = proto::thinking_enabled_for(&req.model);

        let resp = apply_auth(self.http.post(COMPLETION_URL), &self.provider)
            .header("x-ds-pow-response", pow_header)
            .json(&body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await?;
            return Err(ModelError::Http { status, body });
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full = String::new();
        // 整轮累积的思维链——回填给 transcript，下一轮 build_prompt 时再发回模型
        let mut full_reasoning = String::new();
        let mut finished = false;
        let mut state = proto::DeepseekStreamState::default();
        // text 通道的 tool_calls XML sieve：边流边把 <tool_calls>…</tool_calls> 扣下不发。
        let mut sieve = proto::ToolCallSieve::new();

        loop {
            let chunk = match super::next_stream_chunk_or_cancel(&mut stream, &cancel).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(err) => {
                    // 流被取消 / 网络断开等：把 sieve 里残留的安全文本先吐给 surface，
                    // 再把错误传上去。这样 partial 内容不会在 buffer 里丢失。
                    let trailing = sieve.finalize();
                    if !trailing.is_empty() {
                        full.push_str(&trailing);
                        on_event(ModelStreamEvent::TextDelta { text: trailing });
                    }
                    return Err(err);
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                buf = buf[pos + 1..].to_string();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    finished = true;
                    break;
                }
                if std::env::var_os("HEBBIAN_DEEPSEEK_TRACE").is_some() {
                    eprintln!("[deepseek::sse] {data}");
                }
                if let Some(parsed) = proto::parse_sse_line(data, thinking_enabled, &mut state) {
                    let mut role_header_truncated = false;
                    for part in parsed.parts {
                        match part {
                            proto::DeepseekChunkPart::Text(s) => {
                                full.push_str(&s);
                                // 模型一旦开始伪造 "### User" / "### Assistant" /
                                // "### System" / "### Tool" 这类角色头（实测会在工具
                                // 链多轮里"续写整段对话脚本"），把 full 截到该位置、
                                // 丢掉 sieve pending 并停流，再走正常 finalize → 解析。
                                if let Some(cut) = proto::find_fake_role_header_cut(&full) {
                                    full.truncate(cut);
                                    let _ = sieve.finalize();
                                    finished = true;
                                    role_header_truncated = true;
                                    break;
                                }
                                let safe = sieve.push(&s);
                                if !safe.is_empty() {
                                    on_event(ModelStreamEvent::TextDelta { text: safe });
                                }
                            }
                            proto::DeepseekChunkPart::Thinking(s) => {
                                full_reasoning.push_str(&s);
                                on_event(ModelStreamEvent::ReasoningDelta { text: s });
                            }
                        }
                    }
                    if parsed.finished {
                        finished = true;
                    }
                    if role_header_truncated {
                        break;
                    }
                }
            }
            if finished {
                break;
            }
        }
        // 流结束：把 sieve 里残留的安全文本吐完，并补进 full 供后续 transcript 用
        let trailing = sieve.finalize();
        if !trailing.is_empty() {
            full.push_str(&trailing);
            on_event(ModelStreamEvent::TextDelta {
                text: trailing.clone(),
            });
        }

        // DeepSeek web 协议只暴露一个 `accumulated_token_usage`（input+output 合计，
        // 不分 cache）。把它落到 `input_tokens`（语义上"已用 token 总量"），output/cache
        // 保留 0——比当前全 0 强，且不撒谎说有 cache 命中。
        let usage = Usage {
            input_tokens: state.accumulated_token_usage,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };

        // 解析 tool_calls：在最终文本里找 `<tool_calls>` 块
        let (clean_text, tool_calls) = proto::extract_tool_calls(&full);
        if !tool_calls.is_empty() {
            // 把 tool_call 也以「流式 delta」形式补发，保证 surface 端能看到
            for (idx, call) in tool_calls.iter().enumerate() {
                on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                    index: idx,
                    id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    arguments_delta: Some(call.input.to_string()),
                }));
            }
            return Ok(ModelResponse::ToolCalls {
                text: clean_text,
                reasoning: full_reasoning,
                reasoning_signature: String::new(),
                calls: tool_calls,
                attachments: Vec::new(),
                usage,
            });
        }

        Ok(ModelResponse::Done {
            finish: crate::types::FinishReason::Stop,
            text: full,
            reasoning: full_reasoning,
            reasoning_signature: String::new(),
            attachments: Vec::new(),
            usage,
        })
    }
}
