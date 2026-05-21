//! 复刻 desktop `chat.rs` / `title_gen.rs` 中的几个非流式 helpers，让 hebweb
//! 不依赖 desktop 在跑就能镜像 `compact_session / get_context_usage /
//! generate_session_title` 等命令。
//!
//! 这是 hebweb standalone 路线的一部分：bridge 在场时 invoke 走 bridge（desktop 真后端）；
//! 不在场时走这里的本地实现。两条路 v1 都保留。
//!
//! 复刻而非抽 agent-core 共享 crate，是按 surgical change 原则——desktop chat.rs 是
//! 核心文件，refactor 会牵动太多。当函数体确实"同构"时（如 send_once、context_usage），
//! 双份等价代码可接受。未来若引入 v2 surface_commands crate 再消除重复。

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
    let transcript =
        Transcript::from_session(session.system_prompt.clone(), &session.messages);
    let used = agent_core::context::budget::estimate_transcript_tokens(
        transcript.system.as_deref(),
        &transcript.entries,
    );
    let budget = match model_gateway::config::get(data_dir, &session.provider_id) {
        Ok(p) => model_gateway::context_window::resolve_context_window(&p, &session.model).await,
        Err(_) => 200_000,
    };
    Ok(ContextUsageDto { used_tokens: used, budget_tokens: budget })
}

/// 主动压缩当前 session（与 desktop `chat::compact_session` 等价）。
pub async fn compact_session(
    data_dir: &Path,
    session_id: &str,
    custom_instructions: Option<String>,
) -> Result<ContextUsageDto> {
    let session = sessions::load(data_dir, session_id).map_err(|e| anyhow!("{e}"))?;
    let provider = model_gateway::config::get(data_dir, &session.provider_id)
        .map_err(|e| anyhow!("{e}"))?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| anyhow!("OAuth token 刷新失败: {e}"))?;
    let model = session.model.clone();
    let budget_tokens =
        model_gateway::context_window::resolve_context_window(&provider, &model).await;
    let inner = model_gateway::build_client(provider).map_err(|e| anyhow!("{e}"))?;
    // 用 ForcedModelClient 把 request 的 model 字段强制改成本次 compact 用的 model
    let client: Arc<dyn ModelClient> = Arc::new(ForcedModelClient { inner, model });

    let transcript =
        Transcript::from_session(session.system_prompt.clone(), &session.messages);
    let result = agent_core::context::compaction::compact_with_llm(
        client.as_ref(),
        transcript.system.as_deref(),
        transcript.entries,
        custom_instructions.as_deref(),
    )
    .await
    .map_err(|e| anyhow!("压缩失败: {e}"))?;

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
    };
    sessions::append_message(data_dir, session_id, marker).map_err(|e| anyhow!("{e}"))?;

    Ok(ContextUsageDto {
        used_tokens: result.after_tokens,
        budget_tokens,
    })
}

// ─── 标题生成（复刻 desktop title_gen.rs）────────────────────────────────

const TITLE_SYSTEM_PROMPT: &str =
    "你是一个严格的标题生成器。阅读给定对话，用不超过 16 个汉字（或 8 个英文单词）总结出一个简短、具体、没有标点和引号的标题，直接输出标题本身，不要任何前后缀。";
const FALLBACK_LIMIT_CJK: usize = 10;
const FALLBACK_LIMIT_LATIN: usize = 15;

/// 走非流式 complete 一次模型，把对话头部交给标题生成器。
pub async fn try_generate_title(
    data_dir: &Path,
    provider: model_gateway::config::Provider,
    model: &str,
    messages: &[Message],
) -> Option<String> {
    let convo: Vec<&Message> = messages
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .take(8)
        .collect();
    if convo.is_empty() {
        return Some("新对话".to_string());
    }

    let mut bundle = String::from("请为以下对话生成标题：\n\n");
    for m in &convo {
        let role = match m.role {
            Role::User => "用户",
            Role::Assistant => "助手",
            _ => continue,
        };
        bundle.push_str(&format!("[{role}] "));
        let snippet: String = m.content.chars().take(200).collect();
        bundle.push_str(&snippet);
        if m.content.chars().count() > 200 {
            bundle.push('…');
        }
        bundle.push('\n');
    }

    let provider =
        model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
            .await
            .ok()?;
    let user_msg = Message {
        id: String::new(),
        role: Role::User,
        content: bundle,
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: 0,
        meta: None,
    };
    send_once(provider, model, Some(TITLE_SYSTEM_PROMPT), &[user_msg]).await.ok()
}

pub fn fallback_from_first_user(messages: &[Message]) -> String {
    let first_user = messages
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .map(|m| m.content.trim())
        .unwrap_or("");
    if first_user.is_empty() {
        return "新对话".to_string();
    }
    let limit = if first_user.chars().take(20).any(is_wide_char) {
        FALLBACK_LIMIT_CJK
    } else {
        FALLBACK_LIMIT_LATIN
    };
    let mut chars = first_user.chars();
    let head: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn is_wide_char(c: char) -> bool {
    c.len_utf8() >= 3
}

/// 简单的非流式 LLM 调用（与 desktop `chat::send_once` 等价）。
async fn send_once(
    provider: model_gateway::config::Provider,
    model: &str,
    system: Option<&str>,
    messages: &[Message],
) -> Result<String> {
    use model_gateway::types::{
        AssistantEntry, ModelRequest, TranscriptEntry, UserEntry,
    };

    let client = model_gateway::build_client(provider).map_err(|e| anyhow!("{e}"))?;
    let entries: Vec<TranscriptEntry> = messages
        .iter()
        .filter_map(|m| match m.role {
            Role::User => Some(TranscriptEntry::User(UserEntry {
                text: m.content.clone(),
                attachments: m.attachments.clone(),
            })),
            Role::Assistant => Some(TranscriptEntry::Assistant(AssistantEntry {
                text: m.content.clone(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
            })),
            _ => None,
        })
        .collect();
    let req = ModelRequest {
        model: model.to_string(),
        system: system.map(str::to_string),
        entries,
        tools: Vec::new(),
        max_tokens: 4096,
        reasoning: None,
    };
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    match client.complete(req, cancel).await.map_err(|e| anyhow!("{e}"))? {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => Ok(text),
    }
}
