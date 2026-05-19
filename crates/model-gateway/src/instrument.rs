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
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent, Usage},
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
        // 字段先声明为 Empty，拿到 response 后用 record() 填回。
        tracing::info_span!(
            "model.request",
            otel.kind = "client",
            gen_ai.system = self.system,
            gen_ai.operation.name = "chat",
            gen_ai.request.model = %req.model,
            gen_ai.request.max_tokens = req.max_tokens,
            hebbian.model.streaming = streaming,
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
            let (usage, finish) = match resp {
                ModelResponse::Done { usage, .. } => (usage, "stop"),
                ModelResponse::ToolCalls { usage, .. } => (usage, "tool_calls"),
            };
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

fn record_usage_on_span(span: &tracing::Span, usage: &Usage, finish: &str) {
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
