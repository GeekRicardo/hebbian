//! 多 session 运行时：每个 SessionRuntime 独立持有 HITL 通道、cancel flag、
//! pending inputs、broadcast 通道。多个 WS 连接可以同时订阅不同 SessionRuntime，
//! 互不阻塞——这是 hebweb "多 AI 各看各的对话"的核心保证。
//!
//! 与 [`apps/cli/src/daemon.rs`] 对照：daemon 把事件 print 到 stdout，
//! 这里改为通过 `tokio::sync::broadcast::Sender<WsServerMessage>` 推给 WS 订阅者。
//! 其余 run_turn 逻辑（构建 model client、Workspace、ReadStateTracker、EditsWorktree、
//! HookManager、CoreSession、HITL oneshot、token stats 累加、partial 持久化）一致。
//!
//! v2 计划：与 daemon.rs 共享同一个 surface_session 模块，消除重复。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use agent_core::{
    context::transcript::Transcript,
    definition::AgentDefinition,
    edits::EditsWorktree,
    hooks::HookManager,
    permissions::PermissionStore,
    read_state::ReadStateTracker,
    run_mode::RunMode,
    storage::{
        sessions::{self as sessions, Message, MessagePart, MessageToolCall, Role, TokenStats},
        sessions_dir, settings as settings_store,
    },
    tools::{background, skill::default_skill_dirs},
    workspace::Workspace,
    Harness, Session as CoreSession, SessionConfig, TurnObserver, TurnOutcome,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use common::runtime::{PendingInputs, PendingUserInput};
use model_gateway::{
    client::{DynModelClient, ModelClient},
    config as providers,
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use protocol::{
    ApprovalDecision, Event as AgentEvent, PermissionKind, PermissionRequestId, QuestionOption,
    UserAnswer,
};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::events::translate as translate_event;
use crate::protocol::WsServerMessage;

/// 每个 session 一份的运行时状态
pub struct SessionRuntime {
    pub session_id: String,
    pub data_dir: PathBuf,
    pub provider_id: String,
    pub model: String,
    pub reasoning: Option<common::ReasoningConfig>,

    pub pending_approvals: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
    pub pending_questions: Mutex<HashMap<String, oneshot::Sender<UserAnswer>>>,

    pub active_run: AtomicBool,
    pub cancel_flag: Mutex<Option<Arc<AtomicBool>>>,
    pub pending_inputs: Mutex<Option<PendingInputs>>,

    pub run_mode: Mutex<RunMode>,
    /// 强制 auto-mode 开关（in-memory，重启回 false，对齐 desktop ForceAutomodeState 语义）
    pub force_automode: AtomicBool,

    pub input_tx: mpsc::UnboundedSender<String>,
    pub event_tx: broadcast::Sender<WsServerMessage>,

    pub permission_store: Option<Arc<PermissionStore>>,
}

impl SessionRuntime {
    pub fn is_active(&self) -> bool {
        self.active_run.load(Ordering::SeqCst)
    }

    pub fn set_active(&self, cancel: Arc<AtomicBool>, inputs: PendingInputs) {
        *self.cancel_flag.lock().unwrap() = Some(cancel);
        *self.pending_inputs.lock().unwrap() = Some(inputs);
        self.active_run.store(true, Ordering::SeqCst);
    }

    pub fn clear_active(&self) {
        *self.cancel_flag.lock().unwrap() = None;
        *self.pending_inputs.lock().unwrap() = None;
        self.active_run.store(false, Ordering::SeqCst);
    }

    pub fn inject(&self, text: String) -> bool {
        if let Some(inputs) = &*self.pending_inputs.lock().unwrap() {
            inputs.lock().unwrap().push(PendingUserInput {
                content: text,
                attachments: Vec::new(),
            });
            true
        } else {
            false
        }
    }

    pub fn stop(&self) {
        if let Some(flag) = &*self.cancel_flag.lock().unwrap() {
            flag.store(true, Ordering::SeqCst);
        }
        for (_id, tx) in self.pending_approvals.lock().unwrap().drain() {
            let _ = tx.send(ApprovalDecision::Deny);
        }
        for (_id, tx) in self.pending_questions.lock().unwrap().drain() {
            let _ = tx.send(UserAnswer::Cancelled);
        }
    }

    pub fn broadcast(&self, msg: WsServerMessage) {
        // 没有订阅者时 send 会失败，忽略——事件流是 fire-and-forget
        let _ = self.event_tx.send(msg);
    }

    pub fn emit_engine_event(&self, ev: crate::events::EngineEvent) {
        let payload = match serde_json::to_value(&ev) {
            Ok(v) => v,
            Err(_) => return,
        };
        self.broadcast(WsServerMessage::Event {
            session_id: self.session_id.clone(),
            name: "engine-event".to_string(),
            payload,
        });
    }
}

// ─── per-turn 数据 ─────────────────────────────────────────────────────────

struct TurnData {
    full_text: String,
    tool_calls: Vec<MessageToolCall>,
    parts: Vec<MessagePart>,
    pending_tools: HashMap<String, (String, Value)>,
}

impl TurnData {
    fn new() -> Self {
        Self {
            full_text: String::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            pending_tools: HashMap::new(),
        }
    }

    fn handle_event(&mut self, payload: &protocol::EventPayload) {
        use protocol::EventPayload::*;
        match payload {
            Reasoning { text } => {
                self.parts
                    .push(MessagePart::Reasoning { text: text.clone() });
            }
            TextDone { full_text } => {
                self.full_text = full_text.clone();
                self.parts
                    .retain(|p| !matches!(p, MessagePart::Text { .. }));
                self.parts.push(MessagePart::Text {
                    text: full_text.clone(),
                });
            }
            ToolCallStarted {
                call_id,
                name,
                input,
                ..
            } => {
                self.pending_tools
                    .insert(call_id.clone(), (name.clone(), input.clone()));
            }
            ToolCallFinished {
                call_id,
                result,
                duration_ms,
                ..
            } => {
                if let Some((name, input)) = self.pending_tools.remove(call_id) {
                    let tc = MessageToolCall {
                        id: call_id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                        result: Some(result.clone()),
                        duration_ms: Some(*duration_ms),
                    };
                    self.tool_calls.push(tc.clone());
                    self.parts.push(MessagePart::ToolCall {
                        id: call_id.clone(),
                        name,
                        input,
                        arguments: String::new(),
                        result: Some(result.clone()),
                        duration_ms: Some(*duration_ms),
                    });
                }
            }
            _ => {}
        }
    }

    fn build_message(self) -> Option<Message> {
        if self.full_text.is_empty() && self.tool_calls.is_empty() {
            return None;
        }
        Some(Message {
            id: sessions::new_id(),
            role: Role::Assistant,
            content: self.full_text,
            attachments: Vec::new(),
            tool_calls: self.tool_calls,
            parts: self.parts,
            created_at: Utc::now().timestamp_millis(),
            meta: None,
            subagent_call_id: None,
        })
    }
}

// ─── Observer ──────────────────────────────────────────────────────────────

struct WebObserver {
    runtime: Arc<SessionRuntime>,
    turn: TurnData,
}

#[async_trait]
impl TurnObserver for WebObserver {
    fn on_event(&mut self, event: &AgentEvent) {
        // 子 subagent NestedRun 事件不进父 turn 聚合（架构 §4.4.11.8），仅推到 WS 让前端
        // 按 subagent_call_id 嵌套渲染。子事件落到子 session.jsonl 由 P3.1c 单独接上。
        if event.subagent_call_id.is_some() {
            if let Some(ev) = translate_event(event) {
                self.runtime.emit_engine_event(ev);
            }
            return;
        }
        self.turn.handle_event(&event.payload);
        if let Some(ev) = translate_event(event) {
            self.runtime.emit_engine_event(ev);
        }
    }

    async fn on_permission_request(
        &mut self,
        request_id: &PermissionRequestId,
        _kind: &PermissionKind,
        _summary: &str,
    ) -> Option<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.runtime
            .pending_approvals
            .lock()
            .unwrap()
            .insert(request_id.as_str().to_string(), tx);
        rx.await.ok()
    }

    async fn on_question(
        &mut self,
        request_id: &PermissionRequestId,
        _question: &str,
        _options: &[QuestionOption],
        _multi: bool,
        _questions: &[protocol::AskQuestion],
    ) -> Option<UserAnswer> {
        let (tx, rx) = oneshot::channel();
        self.runtime
            .pending_questions
            .lock()
            .unwrap()
            .insert(request_id.as_str().to_string(), tx);
        rx.await.ok()
    }
}

// ─── NamedModelClient ──────────────────────────────────────────────────────

struct NamedModelClient {
    inner: DynModelClient,
    model: String,
    reasoning: Option<common::ReasoningConfig>,
}

impl NamedModelClient {
    fn new(
        inner: DynModelClient,
        model: String,
        reasoning: Option<common::ReasoningConfig>,
    ) -> Self {
        Self {
            inner,
            model,
            reasoning,
        }
    }

    fn patch(&self, mut req: ModelRequest) -> ModelRequest {
        req.model = self.model.clone();
        if req.reasoning.is_none() {
            req.reasoning = self.reasoning.clone();
        }
        req
    }
}

#[async_trait]
impl ModelClient for NamedModelClient {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }
    fn supports_streaming_tools(&self) -> bool {
        self.inner.supports_streaming_tools()
    }
    async fn complete(
        &self,
        req: ModelRequest,
        cancel: common::CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        self.inner.complete(self.patch(req), cancel).await
    }
    async fn stream(
        &self,
        req: ModelRequest,
        cancel: common::CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        self.inner.stream(self.patch(req), cancel, on_event).await
    }
}

// ─── run_turn ──────────────────────────────────────────────────────────────

pub async fn run_turn(runtime: Arc<SessionRuntime>, user_text: String) -> Result<()> {
    let data_dir = &runtime.data_dir;
    let session_id = &runtime.session_id;

    // send 入口：先把上次中断残留的 partial 折叠进 jsonl 再读历史（同 chat::send_and_save）。
    let prior = sessions::load_with_partial_recovery(data_dir, session_id)?;

    // 持久化 user message
    let user_msg = Message {
        id: sessions::new_id(),
        role: Role::User,
        content: user_text.clone(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: Utc::now().timestamp_millis(),
        meta: None,
        subagent_call_id: None,
    };
    sessions::append_message(data_dir, session_id, user_msg)?;

    // model client
    let providers_file = providers::load(data_dir)?;
    let provider = providers_file
        .providers
        .iter()
        .find(|p| p.id == runtime.provider_id)
        .ok_or_else(|| anyhow!("provider {} 不存在", runtime.provider_id))?
        .clone();
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| anyhow!("OAuth token 刷新失败: {e}"))?;
    let ctx_window =
        model_gateway::context_window::effective_context_window_for(&provider, &runtime.model);
    let vision = agent_core::vision_bridge::build_vision_client(data_dir)
        .await
        .map_err(|e| anyhow!("vision bridge: {e}"))?;
    let inner = model_gateway::build_client(provider)
        .map_err(|e| anyhow!("构建 model client 失败: {e}"))?;
    let inner = agent_core::vision_bridge::wrap_with_vision_client(inner, vision);
    let client: Arc<dyn ModelClient> = Arc::new(NamedModelClient::new(
        inner,
        runtime.model.clone(),
        runtime.reasoning.clone(),
    ));

    // workspace
    let settings = settings_store::load(data_dir);
    let workdir = prior
        .workdir
        .clone()
        .or_else(|| settings.conversation.workdir.clone())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let allowed_paths = prior
        .allowed_paths
        .clone()
        .unwrap_or_else(|| settings.conversation.allowed_paths.clone());
    let workspace = Workspace::with_runtime_state(
        workdir.clone(),
        allowed_paths,
        prior.runtime_allowed_paths.clone(),
        prior.pending_runtime_allowed_paths.clone(),
    );

    let skill_dirs: Vec<(agent_core::tools::skill::SkillSource, std::path::PathBuf)> = {
        let configured = prior
            .skill_dirs
            .clone()
            .unwrap_or_else(|| settings.conversation.skill_dirs.clone());
        if configured.is_empty() {
            default_skill_dirs(data_dir, &workdir)
        } else {
            configured
                .into_iter()
                .map(|p| (agent_core::tools::skill::SkillSource::Global, p))
                .collect()
        }
    };

    let phase = agent_core::wakeup::new_phase_channel();
    let shells = background::registry_for_session(session_id);
    agent_core::wakeup::WakeupScheduler::global()
        .register_session_shells(session_id.clone(), shells.clone());

    let hook_cfg = agent_core::hooks::load_hooks_config(data_dir, Some(workspace.workdir()));
    let external_hooks = agent_core::hooks::ExternalHook::from_config(hook_cfg);

    let bg_log_dir = Some(sessions_dir::bg_dir(data_dir, session_id));
    let read_state_tracker = Arc::new(ReadStateTracker::new());
    let edits_worktree = Arc::new(EditsWorktree::new(data_dir, session_id, &workspace));

    let harness = Arc::new(Harness::new(
        agent_core::tools::default_tools_with_mcp(
            workspace.clone(),
            &skill_dirs,
            bg_log_dir,
            phase.clone(),
            shells,
            Some(data_dir.to_path_buf()),
            Some(session_id.clone()),
            Some(read_state_tracker),
            settings.general.shell.clone(),
            settings.general.edit_backend,
            agent_core::storage::mcp::load(data_dir).with_cwd(workspace.workdir().to_path_buf()),
        )
        .await,
        HookManager::new(external_hooks),
    ));

    let model_io_dump =
        agent_core::model_io_dump::open_for_session_if_enabled(data_dir, session_id).await;

    if let Some(store) = &runtime.permission_store {
        store.ensure_session_view(session_id);
    }

    let run_mode = *runtime.run_mode.lock().unwrap();
    let enabled_tools = {
        let s = prior.enabled_tools.clone().unwrap_or_default();
        if s.is_empty() {
            settings.conversation.enabled_tools.clone()
        } else {
            s
        }
    };
    let global_rules = prior
        .global_rules
        .clone()
        .unwrap_or_else(|| settings.conversation.global_rules.clone());
    let rules_files = prior.rules_files.clone();

    let mut core_session = CoreSession::new(
        harness,
        SessionConfig {
            definition: {
                let mut d = AgentDefinition::default();
                d.compaction_policy.token_budget = (ctx_window as f64 * 0.75) as usize;
                d
            },
            workspace: workspace.clone(),
            client,
            enabled_tools,
            initial_transcript: Transcript::from_session(
                prior.system_prompt.clone(),
                &prior.messages,
            ),
            recorder: None,
            model_io_dump,
            permission_store: runtime.permission_store.clone(),
            session_id: Some(session_id.clone()),
            run_mode,
            model_id: Some(runtime.model.clone()),
            force_automode: false,
            data_dir: Some(data_dir.to_path_buf()),
            phase: Some(phase),
            global_rules,
            rules_files,
            edits_worktree: Some(edits_worktree),
        },
    );
    core_session.append_user(user_text, Vec::new());

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let pending_inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
    let consumed_inputs = Arc::new(Mutex::new(Vec::new()));
    runtime.set_active(cancel_flag.clone(), pending_inputs.clone());

    let mut handle = core_session.run_with_runtime_inputs(
        cancel_flag,
        Some(pending_inputs),
        Some(consumed_inputs.clone()),
        None,
    );

    let mut observer = WebObserver {
        runtime: runtime.clone(),
        turn: TurnData::new(),
    };
    let summary = handle.drive(&mut observer).await;

    runtime.clear_active();

    // 累加 token stats
    if let Some(usage) = summary.usage {
        let delta = TokenStats {
            input_tokens: usage.input,
            output_tokens: usage.output,
            cache_read_tokens: usage.cache_read,
            cache_creation_tokens: usage.cache_creation,
            run_count: 1,
            ..Default::default()
        };
        let _ = sessions::update_meta(data_dir, session_id, |sess| {
            let mut stats = sess.token_stats.unwrap_or_default();
            stats.accumulate(delta);
            sess.token_stats = Some(stats);
            Ok(())
        });
    }

    let consumed: Vec<_> = consumed_inputs.lock().unwrap().drain(..).collect();

    match summary.outcome {
        // 架构 §4.12.1：Suspended 与 Done 走同一段落盘——transcript 从 jsonl 重建
        // （§4.12.3），本轮 assistant 段和 pending drained 的 user message 都要持久化；
        // 不发 Error event 让 web 前端正常显示挂起态，等 wakeup resume。
        TurnOutcome::Done | TurnOutcome::Suspended => {
            if let Some(msg) = observer.turn.build_message() {
                sessions::append_message(data_dir, session_id, msg)?;
            }
            for input in consumed {
                sessions::append_message(
                    data_dir,
                    session_id,
                    Message {
                        id: sessions::new_id(),
                        role: Role::User,
                        content: input.content,
                        attachments: input.attachments,
                        tool_calls: Vec::new(),
                        parts: Vec::new(),
                        created_at: Utc::now().timestamp_millis(),
                        meta: None,
                        subagent_call_id: None,
                    },
                )?;
            }
        }
        TurnOutcome::Cancelled => {
            runtime.emit_engine_event(crate::events::EngineEvent::Error {
                message: "run 已取消".to_string(),
            });
        }
        TurnOutcome::Failed(err) => {
            runtime.emit_engine_event(crate::events::EngineEvent::Error { message: err });
        }
    }

    Ok(())
}
