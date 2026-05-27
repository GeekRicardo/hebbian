//! `ModelClient` 的可观测性装饰器。
//!
//! 当前职责：在每次 complete/stream 周围创建 `model.request` span，给本地
//! stderr 日志加结构化前缀（`model.request{gen_ai.request.model=... streaming=true}: ...`）。
//!
//! 之前还做 OTLP usage 上报 + tracing→OTel mirror，2026-05-22 changelog 拆掉了。
//! 真要看模型 IO 走 `~/.hebbian/sessions/<sid>/model_io.jsonl`。

use std::sync::Arc;

use async_trait::async_trait;
use tracing::Instrument;

use crate::{
    client::ModelClient,
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use common::CancelFlag;

/// `ModelClient` 装饰器：在每次 complete/stream 周围创建 span。
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
        tracing::info_span!(
            "model.request",
            gen_ai.system = self.system,
            gen_ai.request.model = %req.model,
            gen_ai.request.max_tokens = req.max_tokens,
            hebbian.model.streaming = streaming,
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
        self.inner.complete(req, cancel).instrument(span).await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        let span = self.make_span(&req, true);
        self.inner
            .stream(req, cancel, on_event)
            .instrument(span)
            .await
    }
}
