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
    types::{ModelCallMeta, ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use common::CancelFlag;

/// 模型调用发起日志（`target:"model"`，所有模型调用统一从这里出，§4.11）。带 session_id /
/// message_id / tag / run_id / turn，便于按会话、按消息、按调用类别（主 chat / judge / 旁支…）
/// 串日志。tag=main 是主对话，其余是 agent 替用户跑的子调用。
fn log_model_request(provider: &str, model: &str, meta: &ModelCallMeta, streaming: bool) {
    tracing::info!(
        target: "model",
        session_id = meta.session_id.as_deref().unwrap_or("-"),
        message_id = meta.message_id.as_deref().unwrap_or("-"),
        run_id = meta.run_id.as_deref().unwrap_or("-"),
        turn = meta.turn,
        tag = %meta.tag,
        provider,
        model,
        streaming,
        "[Model:Request] 发起模型请求"
    );
}

/// 模型调用响应日志。outcome = done / tool_calls / error；附 token 用量或错误摘要 + 耗时。
fn log_model_response(
    model: &str,
    meta: &ModelCallMeta,
    elapsed_ms: u64,
    result: &Result<ModelResponse, ModelError>,
) {
    let (outcome, detail) = match result {
        Ok(ModelResponse::Done { usage, .. }) => (
            "done",
            format!("in={} out={}", usage.input_tokens, usage.output_tokens),
        ),
        Ok(ModelResponse::ToolCalls { calls, usage, .. }) => (
            "tool_calls",
            format!(
                "calls={} in={} out={}",
                calls.len(),
                usage.input_tokens,
                usage.output_tokens
            ),
        ),
        Err(e) => ("error", e.to_string()),
    };
    tracing::info!(
        target: "model",
        session_id = meta.session_id.as_deref().unwrap_or("-"),
        message_id = meta.message_id.as_deref().unwrap_or("-"),
        run_id = meta.run_id.as_deref().unwrap_or("-"),
        turn = meta.turn,
        tag = %meta.tag,
        model,
        outcome,
        duration_ms = elapsed_ms,
        detail = %detail,
        "[Model:Response] 模型响应返回"
    );
}

/// `ModelClient` 装饰器：在每次 complete/stream 周围创建 span。
pub struct InstrumentedClient {
    inner: Arc<dyn ModelClient>,
    /// provider 系统名（OpenTelemetry GenAI semantic conventions：小写厂商名）
    system: &'static str,
    /// 模型 IO 落盘目录。`Some` 时按 `ModelRequest.meta` 把每次调用落 model_io.jsonl
    /// （请求 / 响应两条 + call_id 关联，任务2）；`None`（健康检查等）不落盘。
    data_dir: Option<std::path::PathBuf>,
}

impl InstrumentedClient {
    pub fn new(
        inner: Arc<dyn ModelClient>,
        system: &'static str,
        data_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            inner,
            system,
            data_dir,
        }
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
        let meta = req.meta.clone();
        let model = req.model.clone();
        log_model_request(self.system, &model, &meta, false);
        // 请求发起即落盘（拿 call_id），在 req 被 move 进 inner 前——崩溃 / 取消也留请求痕迹。
        let call_id = self
            .data_dir
            .as_ref()
            .and_then(|dd| crate::model_io::record_request(dd, &meta, &model, &req));
        let started = std::time::Instant::now();
        let result = self.inner.complete(req, cancel).instrument(span).await;
        let elapsed = started.elapsed().as_millis() as u64;
        log_model_response(&model, &meta, elapsed, &result);
        if let (Some(dd), Some(cid), Some(sid)) = (
            self.data_dir.as_ref(),
            call_id.as_deref(),
            meta.session_id.as_deref(),
        ) {
            crate::model_io::record_response(dd, sid, cid, &result, elapsed);
        }
        result
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        let span = self.make_span(&req, true);
        let meta = req.meta.clone();
        let model = req.model.clone();
        log_model_request(self.system, &model, &meta, true);
        let call_id = self
            .data_dir
            .as_ref()
            .and_then(|dd| crate::model_io::record_request(dd, &meta, &model, &req));
        let started = std::time::Instant::now();
        let result = self
            .inner
            .stream(req, cancel, on_event)
            .instrument(span)
            .await;
        let elapsed = started.elapsed().as_millis() as u64;
        log_model_response(&model, &meta, elapsed, &result);
        if let (Some(dd), Some(cid), Some(sid)) = (
            self.data_dir.as_ref(),
            call_id.as_deref(),
            meta.session_id.as_deref(),
        ) {
            crate::model_io::record_response(dd, sid, cid, &result, elapsed);
        }
        result
    }
}
