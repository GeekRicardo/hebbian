//! 旁支会话引擎（架构 §4.3 / §8.5）：纯内存、不落盘、流式回事件的一轮 agent 跑动。
//!
//! 「旁支」= 从主对话 fork 出来的临时讨论（旁支对话 tab）/ 元素对话 / 浏览器旁支，
//! 共用这一个引擎。与主对话最大的不同：**不落盘**（session_id/recorder/permission_store
//! 全 None → CoreSession 短路持久化与后台 task）；多轮历史由调用方持有，每轮把 user +
//! 重建的 assistant 追加，下一轮用 [`Transcript::from_session`] 重建。
//!
//! 模型 IO 仍写进 `bound_session_id` 的 model_io.jsonl（kind=aside），供调试面板查看。
//!
//! 事件出口是 `emit_event: Fn(WireEvent)` 闭包——surface 拿到 [`protocol::WireEvent`]
//! 再按自己的传输投递（desktop Tauri Channel / hebweb WS broadcast）。引擎内部用
//! [`AssistantAccumulator`] 累积 assistant message、用 [`protocol::to_wire`] 翻译事件，
//! 三 surface 复用同一份，不再各写一套。

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use common::CancelFlag;
use model_gateway::client::{DynModelClient, ModelClient};
use model_gateway::types::{
    ModelError, ModelRequest, ModelResponse, ModelStreamEvent, ReasoningConfig,
};
use protocol::{
    ApprovalDecision, AskQuestion, Event, PermissionKind, PermissionRequestId, QuestionOption,
    UserAnswer, WireEvent,
};

use crate::context::transcript::Transcript;
use crate::definition::AgentDefinition;
use crate::harness::{TurnObserver, TurnOutcome};
use crate::storage::sessions::{self, Message, Role};
use crate::turn_accumulator::AssistantAccumulator;
use crate::workspace::Workspace;
use crate::{Harness, Session as CoreSession, SessionConfig};

/// 给 `ModelClient` 套一层：用 session 指定的 model 名覆盖请求里的 model，
/// 并在上游未设时注入 session 的推理配置。旁支 / 主对话都用它把"选了哪个模型"落到请求。
pub struct ModelWithName {
    inner: DynModelClient,
    model: String,
    reasoning: Option<ReasoningConfig>,
}

impl ModelWithName {
    pub fn new(inner: DynModelClient, model: String) -> Self {
        Self {
            inner,
            model,
            reasoning: None,
        }
    }

    pub fn with_reasoning(
        inner: DynModelClient,
        model: String,
        reasoning: Option<ReasoningConfig>,
    ) -> Self {
        Self {
            inner,
            model,
            reasoning,
        }
    }

    fn patch_model(&self, mut req: ModelRequest) -> ModelRequest {
        req.model = self.model.clone();
        if req.reasoning.is_none() {
            req.reasoning = self.reasoning.clone();
        }
        req
    }
}

#[async_trait]
impl ModelClient for ModelWithName {
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
        self.inner.complete(self.patch_model(req), cancel).await
    }
    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        self.inner
            .stream(self.patch_model(req), cancel, on_event)
            .await
    }
}

/// [`run_aside`] 的入参集合——纯内存旁支引擎的所有可变参数。
///
/// 工具集 / workspace / enabled_tools 由调用方按场景注入：
/// - 元素对话：Preview 信号工具 + home workspace
/// - 代码旁支（旁支对话 tab）：只读 Read/Grep + 主对话 workspace
pub struct RunAsideArgs<'a, F: Fn(WireEvent) + Send + Sync> {
    pub data_dir: &'a Path,
    /// 模型 IO 落盘归属的主对话 id（旁支自己不建 session、不落 transcript）。
    pub bound_session_id: &'a str,
    pub provider_id: &'a str,
    pub model: &'a str,
    pub system_prompt: String,
    /// 调用方持有的多轮内存历史（首轮为空 / fork 的主对话历史）。
    pub history: Vec<Message>,
    pub user_content: String,
    pub attachments: Vec<common::attachments::MessageAttachment>,
    pub harness: Arc<Harness>,
    pub workspace: Arc<Workspace>,
    pub enabled_tools: Vec<String>,
    pub cancel_flag: CancelFlag,
    /// 事件出口：surface 注入，拿到 [`WireEvent`] 后按自己的传输投递。
    pub emit_event: F,
}

/// 旁支会话一轮的通用引擎。返回更新后的内存历史（含本轮 user + assistant）+ 本轮 assistant message。
pub async fn run_aside<F: Fn(WireEvent) + Send + Sync>(
    args: RunAsideArgs<'_, F>,
) -> Result<(Vec<Message>, Message), String> {
    let RunAsideArgs {
        data_dir,
        bound_session_id,
        provider_id,
        model,
        system_prompt,
        mut history,
        user_content,
        attachments,
        harness,
        workspace,
        enabled_tools,
        cancel_flag,
        emit_event,
    } = args;

    let provider = model_gateway::config::get(data_dir, provider_id).map_err(|e| e.to_string())?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| format!("OAuth token 刷新失败: {e}"))?;
    let client = model_gateway::build_client_with_data_dir(provider, data_dir.to_path_buf())
        .map_err(|e| format!("无法创建 ModelClient: {e}"))?;
    let client: Arc<dyn ModelClient> = Arc::new(ModelWithName::new(client, model.to_string()));

    // 旁支模型 IO 写进绑定主对话的面板（kind=aside），无需为旁支单独建 session。
    let model_io_dump =
        crate::model_io_dump::open_for_session_with_kind(data_dir, bound_session_id, "aside").await;
    let dump_for_flush = model_io_dump.clone();

    let user_msg = Message {
        id: sessions::new_id(),
        role: Role::User,
        content: user_content,
        attachments,
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
        subagent_call_id: None,
        run_duration_ms: None,
    };

    // 先用 fork 历史重建 transcript，再显式 push 本轮 user，保证请求末尾永远是 user
    // （fork 历史以 CompactBoundary 结尾时 from_session 会注入占位 assistant，不能落末尾）。
    let mut transcript = Transcript::from_session(Some(system_prompt), &history);
    transcript.push_user(user_msg.content.clone(), user_msg.attachments.clone());
    history.push(user_msg);

    let core_session = CoreSession::new(
        harness,
        SessionConfig {
            definition: AgentDefinition::default(),
            workspace,
            client,
            enabled_tools,
            initial_transcript: transcript,
            recorder: None,
            model_io_dump,
            permission_store: None,
            session_id: None,
            run_mode: Default::default(),
            model_id: Some(model.to_string()),
            force_automode: false,
            data_dir: None,
            phase: None,
            global_rules: Vec::new(),
            rules_files: None,
            edits_worktree: None,
            derived_sink: None,
        },
    );

    let mut handle = core_session.run_with(cancel_flag);
    let mut observer = AsideObserver {
        acc: AssistantAccumulator::new(),
        emit: &emit_event,
    };
    let summary = handle.drive(&mut observer).await;

    // 短命调用方不会等 actor 异步写完——显式 flush 旁支 model_io，避免丢最后一条 entry。
    if let Some(dump) = &dump_for_flush {
        if let Err(e) = dump.flush().await {
            tracing::warn!(error = %e, "aside model_io flush failed");
        }
    }

    match summary.outcome {
        TurnOutcome::Done | TurnOutcome::Suspended => {}
        TurnOutcome::Cancelled => return Err("请求已中断".to_string()),
        TurnOutcome::Failed(error) => return Err(error),
    }

    let assistant_msg = observer
        .acc
        .build()
        .unwrap_or_else(|| empty_assistant_message());
    history.push(assistant_msg.clone());
    Ok((history, assistant_msg))
}

fn empty_assistant_message() -> Message {
    Message {
        id: sessions::new_id(),
        role: Role::Assistant,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
        subagent_call_id: None,
        run_duration_ms: None,
    }
}

/// 旁支观察者：用 [`AssistantAccumulator`] 累积 assistant message + 经 `to_wire` 把事件
/// 转成 [`WireEvent`] 下发给 surface。不做 partial sidecar / 落盘（主对话持久化才需要）。
struct AsideObserver<'a> {
    acc: AssistantAccumulator,
    emit: &'a (dyn Fn(WireEvent) + Send + Sync),
}

#[async_trait]
impl<'a> TurnObserver for AsideObserver<'a> {
    fn on_event(&mut self, event: &Event) {
        self.acc.on_event(event);
        if let Some(wire) = protocol::to_wire(event) {
            (self.emit)(wire);
        }
    }

    async fn on_permission_request(
        &mut self,
        _request_id: &PermissionRequestId,
        _kind: &PermissionKind,
        _summary: &str,
    ) -> Option<ApprovalDecision> {
        // 旁支只挂只读 / 信号工具，不会走到审批；真到这里直接放行一次。
        Some(ApprovalDecision::AllowOnce)
    }

    async fn on_question(
        &mut self,
        _request_id: &PermissionRequestId,
        _question: &str,
        _options: &[QuestionOption],
        _multi: bool,
        _questions: &[AskQuestion],
    ) -> Option<UserAnswer> {
        None
    }
}
