//! 复刻 desktop `chat.rs` 中的几个非流式 helpers（context_usage / compact_session），
//! 让 hebweb 不依赖 desktop 在跑就能镜像相应命令。
//!
//! 这是 hebweb standalone 路线的一部分：bridge 在场时 invoke 走 bridge（desktop 真后端）；
//! 不在场时走这里的本地实现。两条路 v1 都保留。
//!
//! 历史包袱：原来标题生成也复刻在这里，已下沉到 `agent_core::session_titler`。

use std::path::Path;
use std::sync::Arc;

use agent_core::{
    context::transcript::Transcript,
    storage::sessions::{self, Message, MessageMeta, Role},
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use model_gateway::{
    client::{DynModelClient, ModelClient},
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use serde::Serialize;

/// 把请求的 `model` 字段强制改成 caller 指定的，再代理给 inner client。
/// 用于 compact_session 等需要"用 session.model 而不是 provider.default_model"的场景。
struct ForcedModelClient {
    inner: DynModelClient,
    model: String,
}

#[async_trait]
impl ModelClient for ForcedModelClient {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }
    fn supports_streaming_tools(&self) -> bool {
        self.inner.supports_streaming_tools()
    }
    async fn complete(
        &self,
        mut req: ModelRequest,
        cancel: common::CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        req.model = self.model.clone();
        self.inner.complete(req, cancel).await
    }
    async fn stream(
        &self,
        mut req: ModelRequest,
        cancel: common::CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        req.model = self.model.clone();
        self.inner.stream(req, cancel, on_event).await
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextUsageDto {
    pub used_tokens: usize,
    pub budget_tokens: usize,
}

/// 计算 session 当前上下文用量（与 desktop `chat::context_usage` 等价）。
pub async fn context_usage(data_dir: &Path, session_id: &str) -> Result<ContextUsageDto> {
    let session = sessions::load(data_dir, session_id).map_err(|e| anyhow!("{e}"))?;
    let transcript = Transcript::from_session(session.system_prompt.clone(), &session.messages);
    let used = agent_core::context::budget::estimate_transcript_tokens(
        transcript.system.as_deref(),
        &transcript.entries,
    );
    let budget = match model_gateway::config::get(data_dir, &session.provider_id) {
        Ok(p) => model_gateway::context_window::resolve_context_window(&p, &session.model).await,
        Err(_) => 200_000,
    };
    Ok(ContextUsageDto {
        used_tokens: used,
        budget_tokens: budget,
    })
}

/// 主动压缩当前 session（与 desktop `chat::compact_session` 等价）。
pub async fn compact_session(
    data_dir: &Path,
    session_id: &str,
    custom_instructions: Option<String>,
) -> Result<ContextUsageDto> {
    let session = sessions::load(data_dir, session_id).map_err(|e| anyhow!("{e}"))?;
    let provider =
        model_gateway::config::get(data_dir, &session.provider_id).map_err(|e| anyhow!("{e}"))?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| anyhow!("OAuth token 刷新失败: {e}"))?;
    let model = session.model.clone();
    let budget_tokens =
        model_gateway::context_window::resolve_context_window(&provider, &model).await;
    let inner = model_gateway::build_client(provider).map_err(|e| anyhow!("{e}"))?;
    // 用 ForcedModelClient 把 request 的 model 字段强制改成本次 compact 用的 model
    let client: Arc<dyn ModelClient> = Arc::new(ForcedModelClient { inner, model });

    let transcript = Transcript::from_session(session.system_prompt.clone(), &session.messages);
    let (before_tokens, req) = agent_core::context::compaction::build_compaction_request(
        transcript.system.as_deref(),
        transcript.entries,
        custom_instructions.as_deref(),
    );
    tracing::info!(
        session_id,
        before_tokens,
        entries = req.entries.len(),
        "manual compaction started"
    );
    let dump = agent_core::model_io_dump::open_for_session_if_enabled(data_dir, session_id).await;
    let req_snapshot = dump.as_ref().map(|_| req.clone());
    let started = std::time::Instant::now();
    let outcome = agent_core::context::compaction::compact_request_with_llm(
        client.as_ref(),
        req,
        before_tokens,
    )
    .await;
    let duration_ms = started.elapsed().as_millis() as u64;

    if let (Some(dump), Some(req)) = (dump.as_ref(), req_snapshot) {
        let response = match &outcome {
            Ok(result) => serde_json::json!({
                "type": "Done",
                "text": result.summary,
                "before_tokens": result.before_tokens,
                "after_tokens": result.after_tokens,
            }),
            Err(e) => serde_json::json!({
                "type": "Error",
                "error": e.to_string(),
            }),
        };
        dump.record(agent_core::model_io_dump::DumpEntry {
            ts: agent_core::model_io_dump::iso_now(),
            run_id: "manual-compact".to_string(),
            turn: 0,
            model: client.provider_id().to_string(),
            request: agent_core::model_io_dump::request_to_json(&req, client.provider_id()),
            response,
            duration_ms,
            kind: "compaction".to_string(),
        });
        if let Err(e) = dump.flush().await {
            tracing::warn!(session_id, error = %e, "manual compaction model_io flush failed");
        }
    }

    let result = outcome.map_err(|e| {
        tracing::error!(session_id, error = %e, "manual compaction failed");
        anyhow!("压缩失败: {e}")
    })?;
    tracing::info!(
        session_id,
        before_tokens = result.before_tokens,
        after_tokens = result.after_tokens,
        duration_ms,
        "manual compaction finished"
    );

    let marker = Message {
        id: sessions::new_id(),
        role: Role::Marker,
        content: result.summary.clone(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: Some(MessageMeta::CompactBoundary {
            summary: result.summary.clone(),
            before_tokens: result.before_tokens,
            after_tokens: result.after_tokens,
        }),
        subagent_call_id: None,
    };
    sessions::append_message(data_dir, session_id, marker).map_err(|e| anyhow!("{e}"))?;

    Ok(ContextUsageDto {
        used_tokens: result.after_tokens,
        budget_tokens,
    })
}

// 标题生成已下沉到 agent_core::session_titler——hebweb 的 cmd_generate_session_title
// 直接调 regenerate_session_title。本文件不再保留 try_generate_title / fallback 等
// 复刻代码。
