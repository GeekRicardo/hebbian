//! `ModelClient` 的可观测性装饰器。
//!
//! 把任意 `ModelClient` 包一层，自动：
//! - 创建 `model.request` span（带 GenAI semantic conventions 属性）
//! - 记录 usage（input / output / cache_read / cache_creation tokens）
//! - 上报 `hebbian.model.duration_ms` 直方图与 token 计数器
//!
//! 用法：上层只需调 [`crate::build_client`]，已经自动包过。

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use observability::{attr, metrics};
use tracing::{field::Empty, Instrument};

use crate::{
    client::ModelClient,
    types::{
        AssistantEntry, ModelError, ModelRequest, ModelResponse, ModelStreamEvent, ToolResult,
        TranscriptEntry, Usage, UserEntry,
    },
};
use common::CancelFlag;

/// `ModelClient` 装饰器：在每次 complete/stream 周围创建 span 与 metrics。
pub struct InstrumentedClient {
    inner: Arc<dyn ModelClient>,
    /// provider 系统名（OpenTelemetry GenAI semantic conventions：小写厂商名）
    system: &'static str,
}

impl InstrumentedClient {
    pub fn new(inner: Arc<dyn ModelClient>, system: &'static str) -> Self {
        Self { inner, system }
    }

    fn make_span(&self, req: &ModelRequest, streaming: bool) -> tracing::Span {
        let input = model_request_input(req);
        let parameters = model_parameters(req, streaming);
        // 字段先声明为 Empty，拿到 response 后用 record() 填回。
        tracing::info_span!(
            "model.request",
            otel.kind = "client",
            gen_ai.system = self.system,
            gen_ai.operation.name = "chat",
            gen_ai.request.model = %req.model,
            gen_ai.request.max_tokens = req.max_tokens,
            hebbian.model.streaming = streaming,
            langfuse.observation.type = "generation",
            langfuse.observation.model.name = %req.model,
            langfuse.observation.model.parameters = %parameters,
            langfuse.observation.input = %input,
            langfuse.observation.output = Empty,
            langfuse.observation.usage_details = Empty,
            gen_ai.prompt = %input,
            gen_ai.completion = Empty,
            gen_ai.usage.input_tokens = Empty,
            gen_ai.usage.output_tokens = Empty,
            gen_ai.usage.cache_read_tokens = Empty,
            gen_ai.usage.cache_creation_tokens = Empty,
            gen_ai.response.finish_reasons = Empty,
        )
    }
}

#[async_trait]
impl ModelClient for InstrumentedClient {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn supports_streaming_tools(&self) -> bool {
        self.inner.supports_streaming_tools()
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        let span = self.make_span(&req, false);
        let model = req.model.clone();
        let system = self.system;
        let inner = self.inner.clone();
        async move {
            let start = Instant::now();
            let result = inner.complete(req, cancel).await;
            finish_span_and_metrics(system, &model, false, start, &result);
            result
        }
        .instrument(span)
        .await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        let span = self.make_span(&req, true);
        let model = req.model.clone();
        let system = self.system;
        let inner = self.inner.clone();
        async move {
            let start = Instant::now();
            let result = inner.stream(req, cancel, on_event).await;
            finish_span_and_metrics(system, &model, true, start, &result);
            result
        }
        .instrument(span)
        .await
    }
}

fn finish_span_and_metrics(
    system: &str,
    model: &str,
    streaming: bool,
    start: Instant,
    result: &Result<ModelResponse, ModelError>,
) {
    let duration_ms = start.elapsed().as_millis() as f64;
    metrics::record_model_call(system, model, streaming, duration_ms);

    let span = tracing::Span::current();
    match result {
        Ok(resp) => {
            let (usage, finish, output) = match resp {
                ModelResponse::Done { text, usage, .. } => (usage, "stop", text.clone()),
                ModelResponse::ToolCalls {
                    text, usage, calls, ..
                } => (usage, "tool_calls", model_response_tool_output(text, calls)),
            };
            record_output_on_span(&span, &output);
            record_usage_on_span(&span, usage, finish);
            record_usage_metrics(system, model, usage);
        }
        Err(err) => {
            span.record(
                attr::GEN_AI_RESPONSE_FINISH_REASONS,
                error_finish_reason(err),
            );
        }
    }
}

fn record_output_on_span(span: &tracing::Span, output: &str) {
    let output = truncate_for_langfuse(output);
    span.record(attr::LANGFUSE_OBSERVATION_OUTPUT, output.as_str());
    span.record(attr::GEN_AI_COMPLETION, output.as_str());
}

fn record_usage_on_span(span: &tracing::Span, usage: &Usage, finish: &str) {
    let usage_details = usage_details_json(usage);
    span.record(
        attr::LANGFUSE_OBSERVATION_USAGE_DETAILS,
        usage_details.as_str(),
    );
    span.record(attr::GEN_AI_USAGE_INPUT_TOKENS, usage.input_tokens);
    span.record(attr::GEN_AI_USAGE_OUTPUT_TOKENS, usage.output_tokens);
    span.record(
        attr::GEN_AI_USAGE_CACHE_READ_TOKENS,
        usage.cache_read_tokens,
    );
    span.record(
        attr::GEN_AI_USAGE_CACHE_CREATION_TOKENS,
        usage.cache_creation_tokens,
    );
    span.record(attr::GEN_AI_RESPONSE_FINISH_REASONS, finish);
}

fn record_usage_metrics(system: &str, model: &str, usage: &Usage) {
    metrics::record_token_usage(system, model, "input", usage.input_tokens);
    metrics::record_token_usage(system, model, "output", usage.output_tokens);
    metrics::record_token_usage(system, model, "cache_read", usage.cache_read_tokens);
    metrics::record_token_usage(system, model, "cache_creation", usage.cache_creation_tokens);
}

fn model_request_input(req: &ModelRequest) -> String {
    let mut messages = Vec::new();
    if let Some(system) = req.system.as_ref().filter(|s| !s.is_empty()) {
        messages.push(serde_json::json!({
            "role": "system",
            "content": system,
        }));
    }
    for entry in &req.entries {
        match entry {
            TranscriptEntry::User(user) => messages.push(user_entry_json(user)),
            TranscriptEntry::Assistant(assistant) => messages.push(assistant_entry_json(assistant)),
            TranscriptEntry::ToolResults(results) => {
                for result in results {
                    messages.push(tool_result_json(result));
                }
            }
        }
    }
    truncate_for_langfuse(&serde_json::to_string(&messages).unwrap_or_default())
}

fn user_entry_json(entry: &UserEntry) -> serde_json::Value {
    let attachments: Vec<_> = entry
        .attachments
        .iter()
        .map(|attachment| match attachment {
            common::attachments::MessageAttachment::TextFile {
                name,
                media_type,
                content,
            } => serde_json::json!({
                "kind": "text_file",
                "name": name,
                "media_type": media_type,
                "content": truncate_for_langfuse(content),
            }),
            common::attachments::MessageAttachment::Image {
                name,
                media_type,
                data,
            } => serde_json::json!({
                "kind": "image",
                "name": name,
                "media_type": media_type,
                "bytes_base64": data.len(),
            }),
        })
        .collect();
    serde_json::json!({
        "role": "user",
        "content": entry.text,
        "attachments": attachments,
    })
}

fn assistant_entry_json(entry: &AssistantEntry) -> serde_json::Value {
    let tool_calls: Vec<_> = entry
        .tool_calls
        .iter()
        .map(|call| {
            serde_json::json!({
                "id": call.id,
                "name": call.name,
                "input": call.input,
            })
        })
        .collect();
    serde_json::json!({
        "role": "assistant",
        "content": entry.text,
        "reasoning": entry.reasoning,
        "tool_calls": tool_calls,
    })
}

fn tool_result_json(result: &ToolResult) -> serde_json::Value {
    serde_json::json!({
        "role": "tool",
        "tool_call_id": result.call_id,
        "name": result.name,
        "content": result.content,
        "artifact": result.artifact.as_ref().map(|artifact| serde_json::json!({
            "path": artifact.path.display().to_string(),
            "bytes": artifact.bytes,
            "line_count": artifact.line_count,
        })),
    })
}

fn model_response_tool_output(text: &str, calls: &[crate::types::ToolCall]) -> String {
    serde_json::to_string(&serde_json::json!({
        "content": text,
        "tool_calls": calls
            .iter()
            .map(|call| serde_json::json!({
                "id": call.id,
                "name": call.name,
                "input": call.input,
            }))
            .collect::<Vec<_>>(),
    }))
    .unwrap_or_else(|_| text.to_string())
}

fn model_parameters(req: &ModelRequest, streaming: bool) -> String {
    serde_json::to_string(&serde_json::json!({
        "max_tokens": req.max_tokens,
        "streaming": streaming,
        "tools": req.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        "reasoning": req.reasoning,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

fn usage_details_json(usage: &Usage) -> String {
    serde_json::to_string(&serde_json::json!({
        "input": usage.input_tokens,
        "output": usage.output_tokens,
        "total": usage.total(),
        "cache_read": usage.cache_read_tokens,
        "cache_creation": usage.cache_creation_tokens,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

fn truncate_for_langfuse(value: &str) -> String {
    const MAX_CHARS: usize = 32_000;
    let mut iter = value.char_indices();
    match iter.nth(MAX_CHARS) {
        Some((idx, _)) => format!(
            "{}\n…[truncated {} chars]",
            &value[..idx],
            value.chars().count() - MAX_CHARS
        ),
        None => value.to_string(),
    }
}

fn error_finish_reason(err: &ModelError) -> &'static str {
    match err {
        ModelError::Cancelled => "cancelled",
        ModelError::Suspended => "suspended",
        ModelError::Http { .. } => "http_error",
        ModelError::Request(_) => "network_error",
        ModelError::Json(_) => "parse_error",
        ModelError::Other(_) => "error",
    }
}
