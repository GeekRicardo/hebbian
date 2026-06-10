use async_trait::async_trait;
use reqwest::RequestBuilder;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tracing::debug;

use crate::config::{AuthMode, Provider};
use crate::{
    client::ModelClient,
    protocols::anthropic as proto,
    providers::apply_auth,
    types::{
        has_image_generation_tool, ModelError, ModelRequest, ModelResponse, ModelStreamEvent,
        ToolCall, ToolCallStreamDelta, Usage,
    },
};
use common::reasoning::{anthropic_long_context_uses_beta, ANTHROPIC_LONG_CONTEXT_BETA};
use common::CancelFlag;

/// 给 Anthropic 请求按需附加 beta 特性头。当前只处理 1M context（其余 beta
/// 由 [`apply_auth`] 在 OAuth 分支里固定带上）。
fn apply_optional_betas(req: RequestBuilder, attach_long_context: bool) -> RequestBuilder {
    if !attach_long_context {
        return req;
    }
    // 重复传 anthropic-beta 是允许的：服务端对所有出现的值取并集。
    req.header("anthropic-beta", ANTHROPIC_LONG_CONTEXT_BETA)
}

/// 从 [`ModelRequest`] 算出本次请求是否需要 1M context beta header。
fn needs_long_context_beta(req: &ModelRequest) -> bool {
    let wants_long = req
        .reasoning
        .as_ref()
        .map(|r| r.wants_long_context())
        .unwrap_or(false);
    wants_long && anthropic_long_context_uses_beta(&req.model)
}

pub struct AnthropicClient {
    provider: Provider,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(provider: Provider) -> Result<Self, ModelError> {
        let http = super::build_http_client()?;
        Ok(Self { provider, http })
    }

    fn messages_url(&self) -> String {
        format!(
            "{}/v1/messages",
            self.provider.base_url.trim_end_matches('/')
        )
    }

    /// 走 `complete()` 拿到完整响应后，把 reasoning / text / tool_use 一次性 emit
    /// 成 stream events，让上层 (`agent_loop`) 看起来像走了 stream 路径。
    /// 用于 Opus 4.7 + thinking 这种 stream 模式服务端不发 thinking_delta 的场景。
    async fn complete_then_emit(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        tracing::info!(model = %req.model, "anthropic: 4.7 thinking → complete_then_emit");
        let resp = self.complete(req, cancel).await?;
        match &resp {
            ModelResponse::Done {
                text, reasoning, ..
            } => {
                tracing::info!(
                    reasoning_len = reasoning.len(),
                    text_len = text.len(),
                    "anthropic complete_then_emit: Done"
                );
                if !reasoning.is_empty() {
                    on_event(ModelStreamEvent::ReasoningDelta {
                        text: reasoning.clone(),
                    });
                }
                if !text.is_empty() {
                    on_event(ModelStreamEvent::TextDelta { text: text.clone() });
                }
            }
            ModelResponse::ToolCalls {
                text,
                reasoning,
                calls,
                ..
            } => {
                if !reasoning.is_empty() {
                    on_event(ModelStreamEvent::ReasoningDelta {
                        text: reasoning.clone(),
                    });
                }
                if !text.is_empty() {
                    on_event(ModelStreamEvent::TextDelta { text: text.clone() });
                }
                for (i, call) in calls.iter().enumerate() {
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: i,
                        id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                        arguments_delta: Some(call.input.to_string()),
                    }));
                }
            }
        }
        Ok(resp)
    }

    fn is_claude_code_oauth(&self) -> bool {
        matches!(self.provider.auth_mode, AuthMode::OauthClaudeCode)
            || self.provider.claude_code_compat
    }
}

#[async_trait]
impl ModelClient for AnthropicClient {
    fn provider_id(&self) -> &str {
        &self.provider.id
    }

    fn supports_streaming_tools(&self) -> bool {
        // Anthropic SSE 已经能流式发 tool_use（content_block_start + input_json_delta）；
        // 我们的 stream() 实现支持解析这两种，所以可以让 agent_loop 在带 tools 的 turn
        // 也走流式路径，从而实时拿到 thinking_delta。
        true
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        reject_image_generation_tool(&req)?;
        tracing::info!(model = %req.model, "anthropic complete: dispatched");
        let body = proto::build_body(
            &req,
            false,
            self.is_claude_code_oauth(),
            self.provider.account_id.as_deref(),
        )?;
        let attach_long = needs_long_context_beta(&req);

        if let Some(thinking) = body.get("thinking") {
            debug!(model = %req.model, thinking = %thinking, "anthropic complete: thinking field");
        }

        super::retry_request(cancel, || {
            let body = body.clone();
            async move {
                let resp = apply_optional_betas(
                    apply_auth(self.http.post(self.messages_url()), &self.provider),
                    attach_long,
                )
                .json(&body)
                .send()
                .await?;
                let status = resp.status().as_u16();
                let text = resp.text().await?;
                if status >= 400 {
                    return Err(ModelError::Http { status, body: text });
                }
                let v: Value = serde_json::from_str(&text)?;
                Ok(proto::parse_response(&v))
            }
        })
        .await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        // Opus 4.7 在 stream 模式下不发 thinking_delta（实测：只有 signature_delta），
        // 即使显式 display:"summarized" 也没用。要拿到 thinking 文本只能走 complete()。
        // 这里检查到「4.7 + thinking 启用」就回退到 complete，保证 thinking 能落地；
        // 拿到完整 reasoning 后一次性 emit ReasoningDelta，再分别 emit text 和 tool_use。
        if matches!(
            common::reasoning::anthropic_thinking_mode(&req.model),
            Some(common::reasoning::AnthropicThinkingMode::Opus47Adaptive)
        ) && req
            .reasoning
            .as_ref()
            .map(|r| r.is_enabled())
            .unwrap_or(false)
        {
            return self.complete_then_emit(req, cancel, on_event).await;
        }

        reject_image_generation_tool(&req)?;
        let body = proto::build_body(
            &req,
            true,
            self.is_claude_code_oauth(),
            self.provider.account_id.as_deref(),
        )?;
        let attach_long = needs_long_context_beta(&req);
        tracing::info!(
            model = %req.model,
            stream = %body.get("stream").map(|v| v.to_string()).unwrap_or_default(),
            thinking = %body.get("thinking").map(|v| v.to_string()).unwrap_or_else(|| "(none)".into()),
            output_config = %body.get("output_config").map(|v| v.to_string()).unwrap_or_else(|| "(none)".into()),
            "anthropic stream: dispatched"
        );

        let resp = super::retry_request(cancel.clone(), || {
            let body = body.clone();
            async move {
                let r = apply_optional_betas(
                    apply_auth(self.http.post(self.messages_url()), &self.provider),
                    attach_long,
                )
                .json(&body)
                .send()
                .await?;
                let status = r.status().as_u16();
                if status >= 400 {
                    let body = r.text().await?;
                    return Err(ModelError::Http { status, body });
                }
                Ok(r)
            }
        })
        .await?;

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full_text = String::new();
        let mut full_reasoning = String::new();
        let mut full_signature = String::new();
        let mut current_event_type = String::new();
        let mut thinking_deltas_seen: u64 = 0;

        // tool_use 累积：按 Anthropic content block index 跟踪每个 tool 的 id/name/args。
        // 用 BTreeMap 保留 index 顺序；ToolCallStreamDelta.index 同时透给上层。
        struct ToolAccum {
            id: String,
            name: String,
            args: String,
        }
        let mut tools: BTreeMap<usize, ToolAccum> = BTreeMap::new();
        let mut stop_reason: Option<String> = None;
        // Anthropic 流的 usage 分两次到：message_start 给输入 / 缓存命中 / 缓存写入，
        // message_delta 给最终 output_tokens。最后合并成完整 Usage 跟着 ModelResponse 回去。
        let mut usage = Usage::default();

        while let Some(chunk) = super::next_stream_chunk_or_cancel(&mut stream, &cancel).await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));

            // Anthropic SSE: event: <type>\ndata: <json>\n\n
            while let Some(pos) = buf.find("\n\n").or_else(|| buf.find("\r\n\r\n")) {
                let skip = if buf[pos..].starts_with("\r\n\r\n") {
                    4
                } else {
                    2
                };
                let frame = buf[..pos].to_string();
                buf = buf[pos + skip..].to_string();

                for line in frame.lines() {
                    let line = line.trim_end_matches('\r');
                    if let Some(event) = line.strip_prefix("event:") {
                        current_event_type = event.trim().to_string();
                    } else if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data.is_empty() {
                            continue;
                        }
                        // SSE `event: error` 帧（上游 overloaded / upstream_error 等）：HTTP 已是
                        // 200，错误只在流里。不拦就会被当未知事件忽略 → 流"正常"结束 → agent_loop
                        // 误判为成功（空回合），既不停也不弹续作。这里转成 Err 让上层正常收尾。
                        if current_event_type == "error" {
                            return Err(ModelError::Other(format!("模型流式返回错误：{data}")));
                        }
                        let Some(parsed) = proto::parse_stream_event(&current_event_type, data)
                        else {
                            tracing::trace!(
                                event_type = %current_event_type,
                                data = %data.chars().take(200).collect::<String>(),
                                "anthropic stream: unparsed event"
                            );
                            continue;
                        };
                        match parsed {
                            proto::AnthropicStreamEvent::Text { delta, .. } => {
                                on_event(ModelStreamEvent::TextDelta {
                                    text: delta.clone(),
                                });
                                full_text.push_str(&delta);
                            }
                            proto::AnthropicStreamEvent::Thinking { delta, .. } => {
                                if thinking_deltas_seen == 0 {
                                    debug!("anthropic stream: first thinking_delta arrived");
                                }
                                thinking_deltas_seen += 1;
                                full_reasoning.push_str(&delta);
                                on_event(ModelStreamEvent::ReasoningDelta { text: delta });
                            }
                            proto::AnthropicStreamEvent::Signature { signature, .. } => {
                                full_signature = signature.clone();
                                on_event(ModelStreamEvent::ReasoningSignature { signature });
                            }
                            proto::AnthropicStreamEvent::ToolUseStart { index, id, name } => {
                                tracing::info!(
                                    sse_index = index,
                                    tool_id = %id,
                                    tool_name = %name,
                                    "anthropic stream: ToolUseStart"
                                );
                                tools.insert(
                                    index,
                                    ToolAccum {
                                        id: id.clone(),
                                        name: name.clone(),
                                        args: String::new(),
                                    },
                                );
                                on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                                    index,
                                    id: Some(id),
                                    name: Some(name),
                                    arguments_delta: None,
                                }));
                            }
                            proto::AnthropicStreamEvent::ToolInputJsonDelta {
                                index,
                                partial_json,
                            } => {
                                if let Some(acc) = tools.get_mut(&index) {
                                    acc.args.push_str(&partial_json);
                                }
                                on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                                    index,
                                    id: None,
                                    name: None,
                                    arguments_delta: Some(partial_json),
                                }));
                            }
                            proto::AnthropicStreamEvent::MessageStart { usage: u } => {
                                usage = u;
                            }
                            proto::AnthropicStreamEvent::MessageDelta {
                                stop_reason: sr,
                                usage: u,
                            } => {
                                if sr.is_some() {
                                    stop_reason = sr;
                                }
                                if let Some(delta_usage) = u {
                                    // message_delta 带终态 output_tokens；输入 / 缓存沿用 message_start。
                                    if delta_usage.output_tokens > 0 {
                                        usage.output_tokens = delta_usage.output_tokens;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if body.get("thinking").is_some() {
            debug!(
                thinking_deltas_seen,
                reasoning_chars = full_reasoning.len(),
                "anthropic stream: finished"
            );
        }

        // 拼成 ModelResponse：stop_reason=tool_use 走 ToolCalls 分支。
        if stop_reason.as_deref() == Some("tool_use") && !tools.is_empty() {
            let calls: Vec<ToolCall> = tools
                .into_values()
                .map(|t| ToolCall {
                    id: t.id,
                    name: t.name,
                    // input_json_delta 拼回来的字符串是合法 JSON；解析失败时退化成
                    // 字符串 value，agent_loop 仍能把原文作为 arguments 传给 tool。
                    input: serde_json::from_str(&t.args).unwrap_or(json!(t.args)),
                })
                .collect();
            return Ok(ModelResponse::ToolCalls {
                text: full_text,
                reasoning: full_reasoning,
                reasoning_signature: full_signature,
                calls,
                attachments: Vec::new(),
                usage,
            });
        }

        Ok(ModelResponse::Done {
            finish: proto::map_anthropic_finish(stop_reason.as_deref().unwrap_or("")),
            text: full_text,
            reasoning: full_reasoning,
            reasoning_signature: full_signature,
            attachments: Vec::new(),
            usage,
        })
    }
}

fn reject_image_generation_tool(req: &ModelRequest) -> Result<(), ModelError> {
    if has_image_generation_tool(&req.tools) {
        return Err(ModelError::Other(
            "image_generation 工具只支持 OpenAI Responses；请切换到 OpenAI provider，或关闭生图工具。"
                .to_string(),
        ));
    }
    Ok(())
}
