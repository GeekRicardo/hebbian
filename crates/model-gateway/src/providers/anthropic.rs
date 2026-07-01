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
    /// 数据目录（含 providers.json）。`Some` 时启用 401 自愈：模型请求被判凭证无效
    /// 会强制刷新 OAuth token 并用新凭证重试一次。主对话路径（`build_client_with_data_dir`）
    /// 传 `Some`；健康检查 / 标题生成 / 测试等传 `None`（不需要长跑期间续期）。
    data_dir: Option<std::path::PathBuf>,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(provider: Provider) -> Result<Self, ModelError> {
        Self::with_data_dir(provider, None)
    }

    pub fn with_data_dir(
        provider: Provider,
        data_dir: Option<std::path::PathBuf>,
    ) -> Result<Self, ModelError> {
        let http = super::build_http_client()?;
        Ok(Self {
            provider,
            data_dir,
            http,
        })
    }

    fn messages_url(&self) -> String {
        format!(
            "{}/v1/messages",
            self.provider.base_url.trim_end_matches('/')
        )
    }

    fn is_claude_code_oauth(&self) -> bool {
        matches!(self.provider.auth_mode, AuthMode::OauthClaudeCode)
            || self.provider.claude_code_compat
    }

    /// 发一次 Messages 请求拿到成功响应；内含 401 自愈：首次被判凭证无效（401）且
    /// 本 client 带了 data_dir，就强制刷新 OAuth token、用新凭证重发一次。长时间
    /// HITL 审批等待后 token 过期的 401 由此自动救回。
    async fn send_with_refresh(
        &self,
        req: &ModelRequest,
        stream: bool,
        cancel: &CancelFlag,
    ) -> Result<reqwest::Response, ModelError> {
        let attach_long = needs_long_context_beta(req);
        let oauth = self.is_claude_code_oauth();
        let direct = proto::is_direct_anthropic(&self.provider.base_url);
        let url = self.messages_url();

        let body = proto::build_body(
            req,
            stream,
            oauth,
            self.provider.account_id.as_deref(),
            direct,
        )?;
        // 诊断「模型请求串账号」：每次请求打出实际用的 provider / account / token 末 4 位。
        // 复现「切换后才串」时 grep 这行——切到 B 后若仍出现 A 的 provider_id/account，即串。
        let key = &self.provider.api_key;
        tracing::info!(
            provider_id = %self.provider.id,
            account = self.provider.account_id.as_deref().unwrap_or("-"),
            token_tail = %&key[key.len().saturating_sub(4)..],
            model = %req.model,
            stream,
            thinking = %body.get("thinking").map(|v| v.to_string()).unwrap_or_else(|| "(none)".into()),
            output_config = %body.get("output_config").map(|v| v.to_string()).unwrap_or_else(|| "(none)".into()),
            "anthropic request dispatched"
        );
        let first =
            post_messages(&self.http, &url, &self.provider, &body, attach_long, cancel).await;

        // 仅 401 + 带 data_dir 才走自愈；其它错误（含别的 4xx/5xx）原样返回。
        let Err(ModelError::Http { status: 401, .. }) = &first else {
            return first;
        };
        let Some(dd) = self.data_dir.as_deref() else {
            return first;
        };
        let fresh =
            match crate::auth::refresh::force_refresh_provider_token(dd, self.provider.clone())
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "401 后强制刷新 OAuth token 失败");
                    return first;
                }
            };
        tracing::info!("模型请求收到 401，已强制刷新 OAuth token，用新凭证重试");
        let body2 = proto::build_body(req, stream, oauth, fresh.account_id.as_deref(), direct)?;
        post_messages(&self.http, &url, &fresh, &body2, attach_long, cancel).await
    }
}

/// 发 Messages 请求（含瞬时重试），返回成功响应；HTTP >= 400 转成 `ModelError::Http`。
async fn post_messages(
    http: &reqwest::Client,
    url: &str,
    provider: &Provider,
    body: &Value,
    attach_long: bool,
    cancel: &CancelFlag,
) -> Result<reqwest::Response, ModelError> {
    super::retry_request(cancel.clone(), || async {
        let r = apply_optional_betas(apply_auth(http.post(url), provider), attach_long)
            .json(body)
            .send()
            .await?;
        let status = r.status().as_u16();
        if status >= 400 {
            let body = r.text().await?;
            return Err(ModelError::Http { status, body });
        }
        Ok(r)
    })
    .await
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
        let resp = self.send_with_refresh(&req, false, &cancel).await?;
        let text = resp.text().await?;
        let v: Value = serde_json::from_str(&text)?;
        Ok(proto::parse_response(&v))
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        // Opus 4.7/4.8 + OAuth 直连官方时，thinking block 的文本被官方清空（只回
        // signature），但 `content_block_start/stop` 边界仍正常到达——靠这对边界算
        // 思考墙钟时长（emit ReasoningDuration），让 UI 显示「思考用时 N 秒」。
        // 故这里不再回退到非流式：流式既能拿到（代理路径的）thinking_delta，又能拿到
        // 时长边界，还能让正文逐字流，全面优于一次性 complete。
        reject_image_generation_tool(&req)?;
        let resp = self.send_with_refresh(&req, true, &cancel).await?;

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full_text = String::new();
        let mut full_reasoning = String::new();
        let mut full_signature = String::new();
        let mut current_event_type = String::new();
        let mut thinking_deltas_seen: u64 = 0;
        // thinking block 的开始墙钟时刻（按 block index 记）。收到对应 content_block_stop
        // 时算出时长并 emit 一次 ReasoningDuration。只对 thinking 块计时，其余块忽略。
        let mut thinking_started_at: BTreeMap<usize, std::time::Instant> = BTreeMap::new();

        // tool_use 累积：按 Anthropic content block index 跟踪每个 tool 的 id/name/args。
        // 用 BTreeMap 保留 index 顺序。
        struct ToolAccum {
            id: String,
            name: String,
            args: String,
        }
        let mut tools: BTreeMap<usize, ToolAccum> = BTreeMap::new();
        // Anthropic SSE 的 content block index 含 thinking / text 块（tool_use 可能落在
        // block 0 / 1 / 2…，随前面有没有正文浮动）。但上层（agent_loop）期望
        // ToolCallStreamDelta.index 是「本次响应内第几个 tool_use」（从 0 连续递增，
        // 只数工具，与 OpenAI 协议一致），它会再叠加 dispatch_offset 还原全局序号。
        // 直接透传 block index 会让相邻 turn 的 (offset + 浮动 block index) 撞号，
        // 导致落盘 parts 丢 tool_call。这里把 block index 归一成 tool 序号再透出。
        let mut block_to_ordinal: BTreeMap<usize, usize> = BTreeMap::new();
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
                            proto::AnthropicStreamEvent::ThinkingStart { index } => {
                                thinking_started_at.insert(index, std::time::Instant::now());
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
                            proto::AnthropicStreamEvent::BlockStop { index } => {
                                // 只对 thinking 块计时；收到其结束边界时算墙钟时长并 emit 一次。
                                if let Some(started) = thinking_started_at.remove(&index) {
                                    on_event(ModelStreamEvent::ReasoningDuration {
                                        ms: started.elapsed().as_millis() as u64,
                                    });
                                }
                            }
                            proto::AnthropicStreamEvent::ToolUseStart { index, id, name } => {
                                tracing::info!(
                                    sse_index = index,
                                    tool_id = %id,
                                    tool_name = %name,
                                    "anthropic stream: ToolUseStart"
                                );
                                let ordinal = block_to_ordinal.len();
                                block_to_ordinal.insert(index, ordinal);
                                tools.insert(
                                    index,
                                    ToolAccum {
                                        id: id.clone(),
                                        name: name.clone(),
                                        args: String::new(),
                                    },
                                );
                                on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                                    index: ordinal,
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
                                // 用 ToolUseStart 时建立的 block→ordinal 映射透出归一序号，
                                // 保证 start 与后续 delta 落在上层同一个 tool part 上。
                                if let Some(&ordinal) = block_to_ordinal.get(&index) {
                                    on_event(ModelStreamEvent::ToolCallDelta(
                                        ToolCallStreamDelta {
                                            index: ordinal,
                                            id: None,
                                            name: None,
                                            arguments_delta: Some(partial_json),
                                        },
                                    ));
                                }
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

        if thinking_deltas_seen > 0 {
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
