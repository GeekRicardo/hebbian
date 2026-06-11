//! Daemon 主体：启动后持守一个 Unix socket，接受 IPC 命令，同时驱动 agent_core。
//!
//! 事件输出：全部以 NDJSON 行写到 stdout，AI 调试工具可直接 tail 读取。
//! IPC 通信：每条连接读一行 JSON → 执行 → 回一行 JSON → 断开。

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
        sessions::{self as sessions, Message, MessagePart, MessageToolCall, Role},
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
    ApprovalDecision, Event as AgentEvent, EventPayload, PermissionKind, PermissionRequestId,
    PermissionScope, QuestionOption, UserAnswer,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use crate::client::socket_path;
use crate::ipc::{DaemonEvent, IpcCommand, IpcResponse};

// ─── 启动参数 ───────────────────────────────────────────────────────────────

pub struct DaemonArgs {
    pub session_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub workdir: Option<PathBuf>,
    pub run_mode: String,
    pub data_dir: Option<PathBuf>,
}

// ─── 共享状态 ───────────────────────────────────────────────────────────────

/// daemon 输入 channel 的载荷。
///
/// - `User`：用户通过 ipc 发来的一条文本，run_turn 需要先把它 append 到 session.jsonl 再开 run。
/// - `Resume`：wakeup handler 触发——`<wakeup>` user message **已经**被 handler append 过，
///   这里只需要开新 run 让模型读到它。否则会重复 append。
enum TurnInput {
    User(String),
    Resume,
}

struct DaemonState {
    session_id: String,
    data_dir: PathBuf,
    provider_id: String,
    model: String,
    reasoning: Option<common::ReasoningConfig>,

    // HITL 挂起审批：request_id → oneshot Sender
    pending_approvals: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
    // HITL 挂起提问：request_id → oneshot Sender
    pending_questions: Mutex<HashMap<String, oneshot::Sender<UserAnswer>>>,

    // 当前 run 的控制点
    active_run: AtomicBool,
    cancel_flag: Mutex<Option<Arc<AtomicBool>>>,
    pending_inputs: Mutex<Option<PendingInputs>>,

    // 当前 run mode（每次 run_turn 读取，heb mode 命令更新）
    run_mode: Mutex<RunMode>,

    // 新 turn 输入通道
    input_tx: mpsc::UnboundedSender<TurnInput>,

    permission_store: Option<Arc<PermissionStore>>,
}

impl DaemonState {
    fn emit(&self, event: &DaemonEvent) {
        if let Ok(line) = serde_json::to_string(event) {
            println!("{line}");
        }
    }

    fn is_active(&self) -> bool {
        self.active_run.load(Ordering::SeqCst)
    }

    fn set_active(&self, cancel: Arc<AtomicBool>, inputs: PendingInputs) {
        *self.cancel_flag.lock().unwrap() = Some(cancel);
        *self.pending_inputs.lock().unwrap() = Some(inputs);
        self.active_run.store(true, Ordering::SeqCst);
    }

    fn clear_active(&self) {
        *self.cancel_flag.lock().unwrap() = None;
        *self.pending_inputs.lock().unwrap() = None;
        self.active_run.store(false, Ordering::SeqCst);
    }

    fn inject(&self, text: String) -> bool {
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

    fn stop(&self) {
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
}

// ─── Observer ──────────────────────────────────────────────────────────────

/// turn 级数据（每次 run_turn 新建）——追踪 assistant 输出以便落盘
struct TurnData {
    full_text: String,
    tool_calls: Vec<MessageToolCall>,
    parts: Vec<MessagePart>,
    // 当前正在收集的工具调用（call_id → (name, input)）
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

    fn handle_event(&mut self, payload: &EventPayload) {
        match payload {
            EventPayload::Reasoning { text } => {
                self.parts
                    .push(MessagePart::Reasoning { text: text.clone() });
            }
            EventPayload::TextDone { full_text } => {
                self.full_text = full_text.clone();
                // 把最终文字同步进 parts
                self.parts
                    .retain(|p| !matches!(p, MessagePart::Text { .. }));
                self.parts.push(MessagePart::Text {
                    text: full_text.clone(),
                });
            }
            EventPayload::ToolCallStarted {
                call_id,
                name,
                input,
                ..
            } => {
                self.pending_tools
                    .insert(call_id.clone(), (name.clone(), input.clone()));
            }
            EventPayload::ToolCallFinished {
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

struct DaemonObserver {
    state: Arc<DaemonState>,
    turn: TurnData,
}

impl DaemonObserver {
    fn new(state: Arc<DaemonState>) -> Self {
        Self {
            state,
            turn: TurnData::new(),
        }
    }
}

#[async_trait]
impl TurnObserver for DaemonObserver {
    fn on_event(&mut self, event: &AgentEvent) {
        // 子 subagent NestedRun 事件不进父 turn 聚合，仅转发到 daemon stdout 让 UI 嵌套渲染
        // （架构 §4.4.11.8）。父 transcript / 父 jsonl 不被串入子内容；子事件落到子 session.jsonl
        // 由 P3.1c 单独接上。
        if event.subagent_call_id.is_some() {
            if let Some(ev) = translate_event(event) {
                self.state.emit(&ev);
            }
            return;
        }
        self.turn.handle_event(&event.payload);
        if let Some(ev) = translate_event(event) {
            self.state.emit(&ev);
        }
    }

    async fn on_permission_request(
        &mut self,
        request_id: &PermissionRequestId,
        _kind: &PermissionKind,
        _summary: &str,
    ) -> Option<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.state
            .pending_approvals
            .lock()
            .unwrap()
            .insert(request_id.as_str().to_string(), tx);
        // PermissionRequested event 已由 on_event 推到 stdout，这里只等待回应
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
        self.state
            .pending_questions
            .lock()
            .unwrap()
            .insert(request_id.as_str().to_string(), tx);
        rx.await.ok()
    }
}

fn translate_event(event: &AgentEvent) -> Option<DaemonEvent> {
    let subagent = event.subagent_call_id.clone();
    match &event.payload {
        EventPayload::RunStarted { .. } => Some(DaemonEvent::RunStarted),
        EventPayload::RunFinished {
            total_input_tokens,
            total_output_tokens,
            total_cache_read_tokens,
            duration_ms,
            ..
        } => Some(DaemonEvent::RunFinished {
            input_tokens: *total_input_tokens,
            output_tokens: *total_output_tokens,
            cache_read_tokens: *total_cache_read_tokens,
            duration_ms: *duration_ms,
        }),
        EventPayload::RunFailed { error } => Some(DaemonEvent::RunFailed {
            error: error.message.clone(),
        }),
        EventPayload::RunCancelled => Some(DaemonEvent::RunCancelled),
        EventPayload::RunSuspended { reason, .. } => Some(DaemonEvent::RunSuspended {
            reason: format!("{reason:?}"),
        }),
        EventPayload::RunResumed { cause } => Some(DaemonEvent::RunResumed {
            cause: format!("{cause:?}"),
        }),
        EventPayload::TextDelta { text } => Some(DaemonEvent::TextDelta {
            text: text.clone(),
            subagent_call_id: subagent.clone(),
        }),
        EventPayload::TextDone { full_text } => Some(DaemonEvent::TextDone {
            full_text: full_text.clone(),
            subagent_call_id: subagent.clone(),
        }),
        EventPayload::Reasoning { text } => Some(DaemonEvent::Reasoning {
            text: text.clone(),
            subagent_call_id: subagent.clone(),
        }),
        EventPayload::ToolCallStarted {
            call_id,
            name,
            input,
            ..
        } => Some(DaemonEvent::ToolStart {
            id: call_id.clone(),
            name: name.clone(),
            input: input.clone(),
            subagent_call_id: subagent.clone(),
        }),
        EventPayload::ToolCallOutputDelta { call_id, chunk, .. } => {
            Some(DaemonEvent::ToolOutputDelta {
                id: call_id.clone(),
                chunk: chunk.clone(),
                subagent_call_id: subagent.clone(),
            })
        }
        EventPayload::ToolCallFinished {
            call_id,
            result,
            duration_ms,
            ..
        } => Some(DaemonEvent::ToolDone {
            id: call_id.clone(),
            result: result.chars().take(500).collect(),
            duration_ms: *duration_ms,
            subagent_call_id: subagent.clone(),
        }),
        EventPayload::PermissionRequested {
            request_id,
            kind,
            summary,
            risk,
            ..
        } => {
            let (tool_name, kind_str, fingerprint, command_segments, input, paths) = match kind {
                PermissionKind::ToolCall {
                    tool_name,
                    input,
                    fingerprint,
                    command_segments,
                    ..
                } => (
                    tool_name.clone(),
                    "tool_call".to_string(),
                    fingerprint.clone(),
                    command_segments.clone(),
                    Some(input.clone()),
                    Vec::new(),
                ),
                PermissionKind::PathAccess { tool_name, paths } => (
                    tool_name.clone(),
                    "path_access".to_string(),
                    None,
                    Vec::new(),
                    None,
                    paths.clone(),
                ),
                PermissionKind::Plan { .. } => (
                    "plan".to_string(),
                    "plan".to_string(),
                    None,
                    Vec::new(),
                    None,
                    Vec::new(),
                ),
                PermissionKind::ContinueLongRun { .. } => (
                    "continue_long_run".to_string(),
                    "continue_long_run".to_string(),
                    None,
                    Vec::new(),
                    None,
                    Vec::new(),
                ),
            };
            Some(DaemonEvent::PermissionRequested {
                request_id: request_id.as_str().to_string(),
                kind: kind_str,
                tool_name,
                summary: summary.clone(),
                risk: format!("{risk:?}"),
                fingerprint,
                command_segments,
                input,
                paths,
            })
        }
        EventPayload::PermissionResolved {
            request_id,
            decision,
        } => Some(DaemonEvent::PermissionResolved {
            request_id: request_id.as_str().to_string(),
            decision: format!("{decision:?}"),
        }),
        EventPayload::UserQuestionRequested {
            request_id,
            question,
            options,
            multi,
            questions,
        } => Some(DaemonEvent::QuestionRequested {
            request_id: request_id.as_str().to_string(),
            question: question.clone(),
            options: options.iter().cloned().map(Into::into).collect(),
            multi: *multi,
            questions: questions.iter().cloned().map(Into::into).collect(),
        }),
        EventPayload::UserQuestionAnswered { request_id, .. } => {
            Some(DaemonEvent::QuestionAnswered {
                request_id: request_id.as_str().to_string(),
            })
        }
        EventPayload::RunModeChanged { from, to } => Some(DaemonEvent::RunModeChanged {
            from: from.clone(),
            to: to.clone(),
        }),
        EventPayload::SessionTitleChanged { session_id, title } => {
            Some(DaemonEvent::SessionTitleChanged {
                session_id: session_id.clone(),
                title: title.clone(),
            })
        }
        EventPayload::MemoryExtracted { session_id, items } => Some(DaemonEvent::MemoryExtracted {
            session_id: session_id.clone(),
            items: items.clone(),
        }),
        EventPayload::MemoryExtractionFailed { session_id, reason } => {
            Some(DaemonEvent::MemoryExtractionFailed {
                session_id: session_id.clone(),
                reason: reason.clone(),
            })
        }
        EventPayload::Notice {
            level,
            message,
            dedup_key,
        } => Some(DaemonEvent::Notice {
            level: match level {
                protocol::LogLevel::Trace
                | protocol::LogLevel::Debug
                | protocol::LogLevel::Info => "info",
                protocol::LogLevel::Warn => "warn",
                protocol::LogLevel::Error => "error",
            }
            .to_string(),
            message: message.clone(),
            dedup_key: dedup_key.clone(),
        }),
        EventPayload::RunEditsCommitted { run_id, files } => Some(DaemonEvent::RunEditsCommitted {
            run_id: run_id.as_str().to_string(),
            files: files
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "real_path": f.real_path,
                        "action": format!("{:?}", f.action).to_lowercase(),
                        "before_bytes": f.before_bytes,
                        "after_bytes": f.after_bytes,
                    })
                })
                .collect(),
        }),
        _ => None,
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

/// 一次完整的用户输入 → agent run → assistant 持久化流程
async fn run_turn(state: Arc<DaemonState>, input: TurnInput) -> Result<()> {
    let data_dir = &state.data_dir;
    let session_id = &state.session_id;

    // 加载 session（transcript 从 jsonl 重建）。
    // 走带 partial 恢复的入口：把上次进程中断时残留在 partial sidecar 里的流式输出
    // 折叠成 Assistant + Interrupted marker 落进 jsonl，再读最终视图。
    let prior = sessions::load_with_partial_recovery(data_dir, session_id)?;

    // 用户输入需要先 append 一条 user message；wakeup resume 路径上 message
    // 已被 wakeup handler 即写即落，跳过避免重复。
    match &input {
        TurnInput::User(user_text) => {
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
        }
        TurnInput::Resume => {}
    }

    // 构建 model client
    let providers_file = providers::load(data_dir)?;
    let provider = providers_file
        .providers
        .iter()
        .find(|p| p.id == state.provider_id)
        .ok_or_else(|| anyhow!("provider {} 不存在，请先在 desktop 配置", state.provider_id))?
        .clone();
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| anyhow!("OAuth token 刷新失败: {e}"))?;
    let ctx_window =
        model_gateway::context_window::effective_context_window_for(&provider, &state.model);
    let vision = agent_core::vision_bridge::build_vision_client(data_dir)
        .await
        .map_err(|e| anyhow!("vision bridge: {e}"))?;
    let inner = model_gateway::build_client_with_data_dir(provider, data_dir.to_path_buf())
        .map_err(|e| anyhow!("构建 model client 失败: {e}"))?;
    let inner = agent_core::vision_bridge::wrap_with_vision_client(inner, vision);
    let client: Arc<dyn ModelClient> = Arc::new(NamedModelClient::new(
        inner,
        state.model.clone(),
        state.reasoning.clone(),
    ));

    // Workspace
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

    // Skill dirs
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

    // Phase channel + background shells
    let phase = agent_core::wakeup::new_phase_channel();
    let shells = background::registry_for_session(session_id);
    agent_core::wakeup::WakeupScheduler::global()
        .register_session_shells(session_id.clone(), shells.clone());

    // Hooks
    let hook_cfg = agent_core::hooks::load_hooks_config(data_dir, Some(workspace.workdir()));
    let external_hooks = agent_core::hooks::ExternalHook::from_config(hook_cfg);

    // bg log dir + ReadStateTracker + EditsWorktree
    let bg_log_dir = Some(sessions_dir::bg_dir(data_dir, session_id));
    let read_state_tracker = Arc::new(ReadStateTracker::new());
    let edits_worktree = Arc::new(EditsWorktree::new(data_dir, session_id, &workspace));

    // Harness
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

    // model IO dump
    let model_io_dump =
        agent_core::model_io_dump::open_for_session_if_enabled(data_dir, session_id).await;

    // PermissionStore session view（幂等初始化，不覆盖已有规则）
    if let Some(store) = &state.permission_store {
        store.ensure_session_view(session_id);
    }

    let run_mode = *state.run_mode.lock().unwrap();
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
            permission_store: state.permission_store.clone(),
            session_id: Some(session_id.clone()),
            run_mode,
            model_id: Some(state.model.clone()),
            force_automode: false,
            data_dir: Some(data_dir.to_path_buf()),
            phase: Some(phase),
            global_rules,
            rules_files,
            edits_worktree: Some(edits_worktree),
        },
    );
    if let TurnInput::User(user_text) = &input {
        core_session.append_user(user_text.clone(), Vec::new());
    }
    // Resume 路径上 wakeup user message 已在 jsonl 里，prior 已包含；不再 in-memory 重复 append。

    // 运行时控制点
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let pending_inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
    let consumed_inputs = Arc::new(Mutex::new(Vec::new()));
    state.set_active(cancel_flag.clone(), pending_inputs.clone());

    let mut handle = core_session.run_with_runtime_inputs(
        cancel_flag,
        Some(pending_inputs),
        Some(consumed_inputs.clone()),
        None,
    );

    let mut observer = DaemonObserver::new(state.clone());
    let summary = handle.drive(&mut observer).await;

    state.clear_active();

    // token_stats 由 agent_loop per-turn 落盘（sessions::bump_token_stats），不再 run-end 累加。

    // 架构 §4.12.5 修订：插队 user message（含 wakeup notification）已经在 wakeup
    // resume_handler / 主动 inject 路径即写即落到 jsonl，run 结束不再二次落盘 consumed，
    // 避免 jsonl 出现重复条目（跟 desktop chat.rs 的修订对齐）。drain 干净避免 leak。
    consumed_inputs.lock().unwrap().clear();

    match summary.outcome {
        // 架构 §4.12.1：Suspended 是 Run 的合法中间态，落 assistant 段跟 Done 一致——
        // transcript 不进 checkpoint（§4.12.3），resume 时从 jsonl 重建本轮 assistant。
        TurnOutcome::Done | TurnOutcome::Suspended => {
            if let Some(msg) = observer.turn.build_message() {
                sessions::append_message(data_dir, session_id, msg)?;
            }
        }
        TurnOutcome::Cancelled => {
            state.emit(&DaemonEvent::Error {
                message: "run 已取消".to_string(),
            });
        }
        TurnOutcome::Failed(err) => {
            state.emit(&DaemonEvent::Error { message: err });
        }
    }

    Ok(())
}

// ─── IPC 命令处理 ──────────────────────────────────────────────────────────

async fn handle_command(state: Arc<DaemonState>, cmd: IpcCommand) -> IpcResponse {
    match cmd {
        IpcCommand::Send { text } => {
            if state.is_active() {
                // 注入当前 run
                if state.inject(text) {
                    IpcResponse::ok()
                } else {
                    IpcResponse::err("注入失败：无活跃 pending_inputs")
                }
            } else {
                // 发给输入 channel，启动新 run
                if state.input_tx.send(TurnInput::User(text)).is_ok() {
                    IpcResponse::ok()
                } else {
                    IpcResponse::err("daemon 输入通道已关闭")
                }
            }
        }
        IpcCommand::Inject { text } => {
            if state.inject(text) {
                IpcResponse::ok()
            } else {
                IpcResponse::err("无活跃 run，无法注入")
            }
        }
        IpcCommand::Allow {
            request_id,
            scope,
            pattern,
            extra_patterns,
        } => {
            let tx = state.pending_approvals.lock().unwrap().remove(&request_id);
            match tx {
                None => IpcResponse::err(format!("未找到 request_id: {request_id}")),
                Some(tx) => {
                    let decision = match scope.as_str() {
                        "session" => ApprovalDecision::AllowAndRemember {
                            scope: PermissionScope::Session,
                            pattern,
                            extra_patterns,
                        },
                        "project" => ApprovalDecision::AllowAndRemember {
                            scope: PermissionScope::Project,
                            pattern,
                            extra_patterns,
                        },
                        "global" => ApprovalDecision::AllowAndRemember {
                            scope: PermissionScope::Global,
                            pattern,
                            extra_patterns,
                        },
                        _ => ApprovalDecision::AllowOnce,
                    };
                    let _ = tx.send(decision);
                    IpcResponse::ok()
                }
            }
        }
        IpcCommand::Deny { request_id } => {
            let tx = state.pending_approvals.lock().unwrap().remove(&request_id);
            match tx {
                None => IpcResponse::err(format!("未找到 request_id: {request_id}")),
                Some(tx) => {
                    let _ = tx.send(ApprovalDecision::Deny);
                    IpcResponse::ok()
                }
            }
        }
        IpcCommand::DenyWithFeedback {
            request_id,
            feedback,
        } => {
            let tx = state.pending_approvals.lock().unwrap().remove(&request_id);
            match tx {
                None => IpcResponse::err(format!("未找到 request_id: {request_id}")),
                Some(tx) => {
                    let _ = tx.send(ApprovalDecision::DenyWithFeedback { feedback });
                    IpcResponse::ok()
                }
            }
        }
        IpcCommand::Answer {
            request_id,
            kind,
            value,
        } => {
            let tx = state.pending_questions.lock().unwrap().remove(&request_id);
            match tx {
                None => IpcResponse::err(format!("未找到 request_id: {request_id}")),
                Some(tx) => {
                    let answer = match kind.as_str() {
                        "cancelled" => UserAnswer::Cancelled,
                        "custom" => UserAnswer::Custom { text: value },
                        _ => UserAnswer::Selected { label: value },
                    };
                    let _ = tx.send(answer);
                    IpcResponse::ok()
                }
            }
        }
        IpcCommand::Stop => {
            state.stop();
            IpcResponse::ok()
        }
        IpcCommand::Mode { mode } => match RunMode::parse(&mode) {
            Some(m) => {
                *state.run_mode.lock().unwrap() = m;
                IpcResponse::ok()
            }
            None => IpcResponse::err(format!(
                "无效 mode：{mode}（ask-before-edits | edit-automatically | plan-mode | auto-mode）"
            )),
        },
        IpcCommand::Ping => {
            IpcResponse::with_data(serde_json::json!({ "session_id": state.session_id }))
        }
        IpcCommand::ListModelIo => {
            match agent_core::storage::model_io::read_session(&state.data_dir, &state.session_id) {
                Ok(entries) => IpcResponse::with_data(serde_json::json!({ "entries": entries })),
                Err(e) => IpcResponse::err(format!("读 model_io.jsonl 失败：{e}")),
            }
        }
    }
}

// ─── socket 单连接处理 ─────────────────────────────────────────────────────

async fn handle_connection(stream: UnixStream, state: Arc<DaemonState>) {
    let (reader, mut writer) = stream.into_split();
    let mut buf = BufReader::new(reader);
    let mut line = String::new();

    if buf.read_line(&mut line).await.is_err() {
        return;
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    let response = match serde_json::from_str::<IpcCommand>(trimmed) {
        Ok(cmd) => handle_command(state, cmd).await,
        Err(e) => IpcResponse::err(format!("命令解析失败：{e}")),
    };

    if let Ok(resp_line) = serde_json::to_string(&response) {
        let _ = writer.write_all(resp_line.as_bytes()).await;
        let _ = writer.write_all(b"\n").await;
        let _ = writer.flush().await;
    }
}

// ─── 入口 ──────────────────────────────────────────────────────────────────

pub async fn run(args: DaemonArgs) -> Result<()> {
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(agent_core::storage::default_data_dir);
    std::fs::create_dir_all(&data_dir)?;

    // ── 解析 provider / model ──
    let providers_file = providers::load(&data_dir)?;
    let (provider_id, model) = resolve_provider_model(
        &providers_file,
        args.provider.as_deref(),
        args.model.as_deref(),
    )?;

    // ── session ──
    let session_id = match args.session_id {
        Some(id) => {
            // 验证 session 存在
            sessions::load(&data_dir, &id).map_err(|e| anyhow!("session {id} 不存在：{e}"))?;
            id
        }
        None => {
            // 创建新 session
            let mut session = sessions::create_with_source(
                &data_dir,
                provider_id.clone(),
                model.clone(),
                None,
                None,
                "cli".to_string(),
            )?;
            // 把 --workdir 写进 session（session.workdir 是 Option<PathBuf>）
            if let Some(wd) = args.workdir.clone() {
                session.workdir = Some(wd);
                session = sessions::save(&data_dir, session)?;
            }
            // 初始化 session 目录结构
            sessions_dir::ensure_session_dirs(&data_dir, &session.id)?;
            sessions_dir::save_meta(
                &data_dir,
                &sessions_dir::SessionDirMeta {
                    session_id: session.id.clone(),
                    created_at: session.created_at,
                    agent: session.prompt_id.clone().unwrap_or_default(),
                    workdir: session.workdir.clone(),
                    provider: session.provider_id.clone(),
                    model: session.model.clone(),
                    last_interrupted_at: None,
                },
            )?;
            session.id
        }
    };

    // ── run mode ──
    let run_mode = RunMode::parse(&args.run_mode).unwrap_or(RunMode::Default);

    // ── permission store ──
    let permission_store = PermissionStore::open(&data_dir).ok().map(Arc::new);

    // ── socket ──
    let socket_dir = data_dir.join("cli-sockets");
    std::fs::create_dir_all(&socket_dir)?;
    let sock_path = socket_path(&session_id);
    // 清理旧 socket 文件（进程退出残留）
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;

    // ── 输入通道 ──
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<TurnInput>();

    let state = Arc::new(DaemonState {
        session_id: session_id.clone(),
        data_dir: data_dir.clone(),
        provider_id,
        model,
        reasoning: None,
        pending_approvals: Mutex::new(HashMap::new()),
        pending_questions: Mutex::new(HashMap::new()),
        active_run: AtomicBool::new(false),
        cancel_flag: Mutex::new(None),
        pending_inputs: Mutex::new(None),
        run_mode: Mutex::new(run_mode),
        input_tx,
        permission_store,
    });

    // ── 宣告启动 ──
    state.emit(&DaemonEvent::Started {
        session_id: session_id.clone(),
    });

    // ── 注册 wakeup resume_handler（架构 §4.12.5 修订）──
    // BgFinishHook 检测到 bash_xxx 进入终态 → 投递 BgTaskFinished event。
    // 这里把 wakeup XML 即写即落到 session.jsonl（带 SystemNotification meta），
    // 同时 push 到 PendingInputs in-memory 队列（如果 run 在跑则 agent_loop drain 看到）。
    // 跟 desktop inject_user_message 行为对称——cancel / 崩溃也不丢，下次 run 启动时
    // jsonl rebuild 自然把这条 user message 纳入 transcript。
    {
        let handler_state = state.clone();
        agent_core::wakeup::WakeupScheduler::global().set_resume_handler(Arc::new(move |event| {
            // 只处理本 daemon 的 session（同 session_id 才落盘到本进程的 jsonl）
            if event.session_id() != handler_state.session_id {
                return;
            }
            let wakeup_xml = agent_core::wakeup::wakeup_xml(&event);
            let meta = event.message_meta();
            let user_msg = sessions::Message {
                id: sessions::new_id(),
                role: sessions::Role::User,
                content: wakeup_xml.clone(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: chrono::Utc::now().timestamp_millis(),
                meta: Some(meta),
                subagent_call_id: None,
            };
            // 1) 即写即落 jsonl（崩溃 / cancel 都不丢）
            if let Err(e) = sessions::append_message(
                &handler_state.data_dir,
                &handler_state.session_id,
                user_msg,
            ) {
                tracing::warn!(error = %e, "wakeup: append_message failed");
                return;
            }
            // 2) 路由：active run 期间 push 到 PendingInputs，agent_loop 在 ModelStep
            //    之前 drain；无 active run 时投 input_tx::Resume 触发新 run——
            //    message 已落盘，run_turn 跳过 append，让模型读到 wakeup 并响应。
            //    没这条 Resume，后台 Task(run_in_background=true) 等的 BgTaskFinished
            //    通知只会静默落盘，模型再也不被唤起。
            let active_inject = handler_state
                .pending_inputs
                .lock()
                .unwrap()
                .as_ref()
                .map(|slot| {
                    slot.lock()
                        .unwrap()
                        .push(common::runtime::PendingUserInput {
                            content: wakeup_xml.clone(),
                            attachments: Vec::new(),
                        });
                })
                .is_some();
            if !active_inject {
                if let Err(e) = handler_state.input_tx.send(TurnInput::Resume) {
                    tracing::warn!(error = %e, "wakeup: input_tx send Resume failed");
                }
            }
        }));
    }

    // ── 接收 IPC 连接（独立 task）──
    let state_for_ipc = state.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let s = state_for_ipc.clone();
                    tokio::spawn(handle_connection(stream, s));
                }
                Err(e) => {
                    tracing::error!(error = %e, "Unix socket accept 失败");
                    break;
                }
            }
        }
    });

    // ── 主循环：依次处理每条输入 ──
    while let Some(input) = input_rx.recv().await {
        if let Err(e) = run_turn(state.clone(), input).await {
            state.emit(&DaemonEvent::Error {
                message: e.to_string(),
            });
        }
    }

    // 清理 socket 文件
    let _ = std::fs::remove_file(&sock_path);
    Ok(())
}

// ─── 辅助：解析 provider/model ─────────────────────────────────────────────

fn resolve_provider_model(
    file: &model_gateway::config::ProvidersFile,
    provider_arg: Option<&str>,
    model_arg: Option<&str>,
) -> Result<(String, String)> {
    let provider_key = match provider_arg {
        Some(arg) => arg.rsplit_once('/').map(|(p, _)| p).unwrap_or(arg),
        None => file
            .default_provider_id
            .as_deref()
            .ok_or_else(|| anyhow!("未指定 --provider 且无默认 provider（先在 desktop 配置）"))?,
    };
    let model_from_arg = provider_arg.and_then(|a| a.rsplit_once('/').map(|(_, m)| m));

    let provider = file
        .providers
        .iter()
        .find(|p| p.id == provider_key || p.name == provider_key)
        .ok_or_else(|| anyhow!("provider 不存在：{provider_key}"))?;

    let model = model_arg
        .map(str::to_string)
        .or_else(|| model_from_arg.map(str::to_string))
        .or_else(|| provider.default_model.clone())
        .ok_or_else(|| {
            anyhow!(
                "未指定 model（用 --model 或 --provider {}/model_id）",
                provider.name
            )
        })?;

    Ok((provider.id.clone(), model))
}
