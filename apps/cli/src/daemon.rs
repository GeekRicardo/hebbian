//! Daemon 主体：启动后持守一个 Unix socket，接受 IPC 命令，同时驱动 agent_core。
//!
//! 事件输出：全部以 NDJSON 行写到 stdout，AI 调试工具可直接 tail 读取。
//! IPC 通信：每条连接读一行 JSON → 执行 → 回一行 JSON → 断开。

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use agent_core::{
    context::transcript::Transcript,
    definition::AgentDefinition,
    edits::EditsWorktree,
    hooks::HookManager,
    permissions::PermissionStore,
    read_state::ReadStateTracker,
    run_mode::RunMode,
    storage::{
        sessions::{self as sessions, Message, Role},
        sessions_dir, settings as settings_store,
    },
    tools::{background, hitl::HitlGate, skill::default_skill_dirs},
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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

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

    /// 当前活 run 的 HITL 闸门（审批 / 提问唯一结算点，§7.8.5）。run 启动 set_active 挂上
    /// agent_loop 持有的真 HitlGate；observer 不再自造 oneshot 阻塞 drive loop（那会让
    /// AutoMode judge 后发的 PermissionAutoJudged pump 不出去而死锁）。heb allow/answer
    /// 命令经 resolve_approval / answer_question 直接戳它。
    hitl: Mutex<Option<Arc<HitlGate>>>,

    // 当前 run 的控制点
    active_run: AtomicBool,
    cancel_flag: Mutex<Option<Arc<AtomicBool>>>,
    pending_inputs: Mutex<Option<PendingInputs>>,
    /// 是否还接受插队输入（agent_loop 末次 drain / run 收尾置 false）。inject 据此在收尾
    /// 窗口拒绝晚到注入，让 Send 回落起新 run 而不是丢消息（§4.2.3）。
    pending_inputs_accepting: Mutex<Option<Arc<AtomicBool>>>,

    // 当前 run mode（每次 run_turn 读取，heb mode 命令更新）
    run_mode: Mutex<RunMode>,

    // 新 turn 输入通道
    input_tx: mpsc::UnboundedSender<TurnInput>,

    permission_store: Option<Arc<PermissionStore>>,

    /// 无人值守自动结算（`heb run` 一次性命令用）。`Some` 时 observer 不挂 pending、
    /// 不等交互回应：审批一律自动拒 + 把 reason 回灌 agent、提问一律自动取消，并计数。
    /// `None`（普通 daemon）保持原交互行为——挂 pending 等 `heb allow/answer`。
    auto_resolve: Option<Arc<AutoResolveStats>>,
}

/// `heb run` 无人值守跑任务时，被自动结算掉的 HITL 计数（架构 §4.4.3 Yolo 配套）。
/// 结尾 summary 把它报给用户/评测框架，说明「N 次审批被自动拒、M 个提问被自动取消」。
#[derive(Default)]
struct AutoResolveStats {
    denied_approvals: std::sync::atomic::AtomicU64,
    cancelled_questions: std::sync::atomic::AtomicU64,
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

    fn set_active(
        &self,
        hitl: Arc<HitlGate>,
        cancel: Arc<AtomicBool>,
        inputs: PendingInputs,
        accepting: Arc<AtomicBool>,
    ) {
        *self.hitl.lock().unwrap() = Some(hitl);
        *self.cancel_flag.lock().unwrap() = Some(cancel);
        *self.pending_inputs.lock().unwrap() = Some(inputs);
        *self.pending_inputs_accepting.lock().unwrap() = Some(accepting);
        self.active_run.store(true, Ordering::SeqCst);
    }

    fn clear_active(&self) {
        *self.hitl.lock().unwrap() = None;
        *self.cancel_flag.lock().unwrap() = None;
        *self.pending_inputs.lock().unwrap() = None;
        *self.pending_inputs_accepting.lock().unwrap() = None;
        self.active_run.store(false, Ordering::SeqCst);
    }

    fn inject(&self, text: String) -> bool {
        // run 收尾窗口（agent_loop 末次 drain 后置 accepting=false）拒绝晚到注入，让 Send
        // 回落起新 run 而不是 push 进死队列静默丢失（§4.2.3）。
        let accepting = self
            .pending_inputs_accepting
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|f| f.load(Ordering::SeqCst));
        if !accepting {
            return false;
        }
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
        if let Some(hitl) = &*self.hitl.lock().unwrap() {
            hitl.cancel_all_pending();
        }
    }

    /// 结算一条审批（heb allow/deny 命令）：直接戳活 run 的 HitlGate。返回是否命中 pending。
    fn resolve_approval(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        let request_id = PermissionRequestId::from_raw(request_id);
        match &*self.hitl.lock().unwrap() {
            Some(hitl) if hitl.is_pending(&request_id) => {
                hitl.resolve(&request_id, decision);
                true
            }
            _ => false,
        }
    }

    /// 结算一条提问（heb answer 命令）：直接戳活 run 的 HitlGate。返回是否命中 pending。
    fn answer_question(&self, request_id: &str, answer: UserAnswer) -> bool {
        let request_id = PermissionRequestId::from_raw(request_id);
        match &*self.hitl.lock().unwrap() {
            Some(hitl) if hitl.is_pending(&request_id) => {
                hitl.answer(&request_id, answer);
                true
            }
            _ => false,
        }
    }
}

// ─── Observer ──────────────────────────────────────────────────────────────

struct DaemonObserver {
    state: Arc<DaemonState>,
}

impl DaemonObserver {
    fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl TurnObserver for DaemonObserver {
    fn on_event(&mut self, event: &AgentEvent) {
        // assistant 累积 + 落盘已收归 agent_core 唯一一份（架构 §7.8.3）：observer 只把
        // 事件翻译成 NDJSON 推到 stdout 做渲染，不再自行重建 message。子 subagent NestedRun
        // 事件同样只转发渲染（架构 §4.4.11.8），父过程累积由 agent_core persister 负责。
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
        // 无人值守（heb run）：不挂 pending、不等交互，直接自动拒 + 计数。把「为什么拒」
        // 回灌 agent 让它换路子（与 Yolo 红线同理）。Yolo 模式下 dispatcher 已自行处置
        // 红线、不会走到这里；这条兜的是非 Yolo 模式 / 越过 Yolo 放行面的审批。
        if let Some(stats) = &self.state.auto_resolve {
            stats
                .denied_approvals
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(ApprovalDecision::DenyWithFeedback {
                feedback: "无人值守模式：该操作需要人工审批，但当前没有人能批准，已自动拒绝。\
                           请改用工作区内的安全做法，或换一种不需要审批的方式。"
                    .to_string(),
            });
        }
        // 交互 daemon：不在 drive loop 里阻塞等审批（那会让 AutoMode judge 后发的
        // PermissionAutoJudged pump 不出去而死锁，§7.8.5）。审批通道是活 run 的 HitlGate
        // （set_active 已挂），heb allow/deny 命令经 resolve_approval 直接戳它。返回 None
        // 让 drive 立即继续 recv。
        let _ = request_id;
        None
    }

    async fn on_question(
        &mut self,
        request_id: &PermissionRequestId,
        _question: &str,
        _options: &[QuestionOption],
        _multi: bool,
        _questions: &[protocol::AskQuestion],
    ) -> Option<UserAnswer> {
        // 无人值守：提问无人可答，自动取消 + 计数。
        if let Some(stats) = &self.state.auto_resolve {
            stats
                .cancelled_questions
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Some(UserAnswer::Cancelled);
        }
        // 同 on_permission_request：不阻塞 drive loop，提问回应经 heb answer →
        // answer_question 直接戳活 run 的 HitlGate。
        let _ = request_id;
        None
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
            reason: protocol::suspend_reason_str(reason).to_string(),
        }),
        EventPayload::RunResumed { cause } => Some(DaemonEvent::RunResumed {
            cause: protocol::resume_cause_str(cause),
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
            is_error,
            ..
        } => Some(DaemonEvent::ToolDone {
            id: call_id.clone(),
            result: result.chars().take(500).collect(),
            duration_ms: *duration_ms,
            is_error: *is_error,
            subagent_call_id: subagent.clone(),
        }),
        EventPayload::PermissionRequested {
            request_id,
            kind,
            summary,
            risk,
            auto_handled,
            call_id,
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
                risk: protocol::risk_str(risk),
                fingerprint,
                command_segments,
                input,
                paths,
                auto_handled: *auto_handled,
                call_id: call_id.clone(),
            })
        }
        EventPayload::PermissionResolved {
            request_id,
            decision,
        } => Some(DaemonEvent::PermissionResolved {
            request_id: request_id.as_str().to_string(),
            decision: protocol::approval_decision_str(decision).to_string(),
        }),
        EventPayload::PermissionAutoJudged {
            request_id,
            tool_name,
            decision,
            reason,
            requires_human,
        } => Some(DaemonEvent::PermissionAutoJudged {
            request_id: request_id.as_ref().map(|r| r.as_str().to_string()),
            tool_name: tool_name.clone(),
            decision: decision.clone(),
            reason: reason.clone(),
            requires_human: *requires_human,
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
        EventPayload::SessionTitleGenerationFailed { session_id, reason } => {
            Some(DaemonEvent::SessionTitleGenerationFailed {
                session_id: session_id.clone(),
                reason: reason.clone(),
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

/// 一次完整的用户输入 → agent run → assistant 持久化流程。
///
/// 返回本轮 [`TurnOutcome`]——daemon 主循环忽略它（行为不变），`heb run` 一次性命令
/// 据它决定退出码。
async fn run_turn(state: Arc<DaemonState>, input: TurnInput) -> Result<TurnOutcome> {
    let data_dir = &state.data_dir;
    let session_id = &state.session_id;

    // 单写者闸口（架构 §7.8.5，#9）：抢 session 级 run 锁，持有到本函数返回。抢不到 = 同一
    // session 已有活 run（另一 surface 进程 / hebcore 共享数据目录）——拒绝并发起 run，避免
    // 两个 run 双写 session.jsonl 造成 transcript 交错。daemon 内部 input 循环本就串行，这层
    // 主要兜跨进程。
    let _run_guard = sessions_dir::SessionRunGuard::try_acquire(data_dir, session_id)
        .ok_or_else(|| anyhow!("session {session_id} 已有活跃 run，拒绝并发启动"))?;

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
                run_duration_ms: None,
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
            // surface 主对话：tag=Main（前端不额外标记，§4.11）。
            call_tag: model_gateway::types::ModelCallTag::Main,
            data_dir: Some(data_dir.to_path_buf()),
            phase: Some(phase),
            global_rules,
            rules_files,
            edits_worktree: Some(edits_worktree),
            // 派生事件旁路（架构 §4.14.7）：标题 / 记忆在 run 收尾后才完成，走 run 级
            // sink 会被 trailing window 关掉的通道丢弃。heb 的 stdout 是进程级 long-lived
            // 出口——捕获 state 翻译成 DaemonEvent 直接 emit，绕过 run channel。
            derived_sink: {
                let state = state.clone();
                Some(std::sync::Arc::new(move |event: AgentEvent| {
                    if let Some(ev) = translate_event(&event) {
                        state.emit(&ev);
                    }
                }))
            },
        },
    );
    if let TurnInput::User(user_text) = &input {
        core_session.append_user(user_text.clone(), Vec::new());
    }
    // Resume 路径上 wakeup user message 已在 jsonl 里，prior 已包含；不再 in-memory 重复 append。

    // 运行时控制点
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let pending_inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
    let pending_inputs_accepting = Arc::new(AtomicBool::new(true));
    let consumed_inputs = Arc::new(Mutex::new(Vec::new()));
    let mut handle = core_session.run_with_runtime_inputs(
        cancel_flag.clone(),
        Some(pending_inputs.clone()),
        Some(consumed_inputs.clone()),
        Some(pending_inputs_accepting.clone()),
    );
    // 把活 run 的真 HitlGate 挂进 DaemonState：heb allow/answer 经 resolve_approval 直接
    // 戳它（§7.8.5），observer 不再自造 oneshot gate 阻塞 drive loop。
    state.set_active(
        handle.hitl().clone(),
        cancel_flag,
        pending_inputs,
        pending_inputs_accepting,
    );

    let mut observer = DaemonObserver::new(state.clone());
    let summary = handle.drive(&mut observer).await;

    state.clear_active();

    // token_stats 由 agent_loop per-turn 落盘（sessions::bump_token_stats），不再 run-end 累加。

    // 架构 §4.12.5 修订：插队 user message（含 wakeup notification）已经在 wakeup
    // resume_handler / 主动 inject 路径即写即落到 jsonl，run 结束不再二次落盘 consumed，
    // 避免 jsonl 出现重复条目（跟 desktop chat.rs 的修订对齐）。drain 干净避免 leak。
    consumed_inputs.lock().unwrap().clear();

    match &summary.outcome {
        // 架构 §4.9.5：assistant 段落盘已收归 agent_core（agent_loop 段边界 / run 收尾单点
        // append）。daemon 不再落 assistant，避免双落。Suspended 同 Done 由 agent_core 落。
        TurnOutcome::Done | TurnOutcome::Suspended => {}
        TurnOutcome::Cancelled => {
            state.emit(&DaemonEvent::Error {
                message: "run 已取消".to_string(),
            });
        }
        TurnOutcome::Failed(err) => {
            state.emit(&DaemonEvent::Error {
                message: err.clone(),
            });
        }
    }

    Ok(summary.outcome)
}

// ─── IPC 命令处理 ──────────────────────────────────────────────────────────

async fn handle_command(state: Arc<DaemonState>, cmd: IpcCommand) -> IpcResponse {
    match cmd {
        IpcCommand::Send { text } => {
            // 活 run 在跑 → 优先注入当前 run；但 run 收尾窗口 inject 会拒（accepting=false），
            // 此时回落到「起新 run」而不是报错丢消息（§4.2.3）。
            if state.is_active() && state.inject(text.clone()) {
                IpcResponse::ok()
            } else if state.input_tx.send(TurnInput::User(text)).is_ok() {
                IpcResponse::ok()
            } else {
                IpcResponse::err("daemon 输入通道已关闭")
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
            if state.resolve_approval(&request_id, decision) {
                IpcResponse::ok()
            } else {
                IpcResponse::err(format!("未找到 request_id: {request_id}"))
            }
        }
        IpcCommand::Deny { request_id } => {
            if state.resolve_approval(&request_id, ApprovalDecision::Deny) {
                IpcResponse::ok()
            } else {
                IpcResponse::err(format!("未找到 request_id: {request_id}"))
            }
        }
        IpcCommand::DenyWithFeedback {
            request_id,
            feedback,
        } => {
            if state.resolve_approval(&request_id, ApprovalDecision::DenyWithFeedback { feedback }) {
                IpcResponse::ok()
            } else {
                IpcResponse::err(format!("未找到 request_id: {request_id}"))
            }
        }
        IpcCommand::Answer {
            request_id,
            kind,
            value,
        } => {
            let answer = match kind.as_str() {
                "cancelled" => UserAnswer::Cancelled,
                "custom" => UserAnswer::Custom { text: value },
                _ => UserAnswer::Selected { label: value },
            };
            if state.answer_question(&request_id, answer) {
                IpcResponse::ok()
            } else {
                IpcResponse::err(format!("未找到 request_id: {request_id}"))
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
                "无效 mode：{mode}（default | plan-mode | auto-mode | yolo）"
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

/// `run` 与 `run_once` 共用的前置装配结果：解析好 provider/model、建好（或连上）session。
struct PreparedSession {
    data_dir: PathBuf,
    provider_id: String,
    model: String,
    session_id: String,
    run_mode: RunMode,
}

/// 解析 data_dir / provider / model，建新 session 或连已有 session（架构 §7 CoreClient）。
/// `run`（daemon）与 `run_once`（heb run 一次性）走同一份装配，避免逻辑漂移。
fn prepare_session(args: &DaemonArgs) -> Result<PreparedSession> {
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
    let session_id = match args.session_id.clone() {
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

    let run_mode = RunMode::parse(&args.run_mode).unwrap_or(RunMode::Default);

    Ok(PreparedSession {
        data_dir,
        provider_id,
        model,
        session_id,
        run_mode,
    })
}

pub async fn run(args: DaemonArgs) -> Result<()> {
    let PreparedSession {
        data_dir,
        provider_id,
        model,
        session_id,
        run_mode,
    } = prepare_session(&args)?;

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
        hitl: Mutex::new(None),
        active_run: AtomicBool::new(false),
        cancel_flag: Mutex::new(None),
        pending_inputs: Mutex::new(None),
        pending_inputs_accepting: Mutex::new(None),
        run_mode: Mutex::new(run_mode),
        input_tx,
        permission_store,
        auto_resolve: None,
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
                run_duration_ms: None,
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

// ─── heb run：一次性无人值守跑一个完整任务 ──────────────────────────────────

/// `heb run` 的启动参数：在 [`DaemonArgs`] 基础上加任务文本 / 超时 / 输出形态。
pub struct RunOnceArgs {
    pub base: DaemonArgs,
    /// 要跑的任务（作为首条 user message）。
    pub task: String,
    /// 整个 run 的墙钟超时（秒）；`None` 不限时。超时 → cancel + 退出码 2。
    pub timeout_secs: Option<u64>,
    /// `true` 时结尾额外打一行结构化结果 JSON（给评测框架 `tail -n1 | jq`）。
    pub json: bool,
}

/// 一次性跑完一个 agent 任务并退出（架构 §4.4.3 Yolo 配套 / 评测 surface）。
///
/// 与 daemon 的区别：**不监听 socket、不持久**——起 in-process、跑一个 run_turn、终态即退。
/// 审批 / 提问无人值守（`auto_resolve = Some`）：审批自动拒 + reason 回灌 agent，提问自动
/// 取消。事件流照常吐 NDJSON 到 stdout（与 daemon 一致），`--json` 时结尾多一行结果对象。
///
/// 返回进程退出码：Done/Suspended→0、Failed→1、超时→2、Cancelled→130。
pub async fn run_once(args: RunOnceArgs) -> Result<i32> {
    let RunOnceArgs {
        base,
        task,
        timeout_secs,
        json,
    } = args;

    let prepared = prepare_session(&base)?;
    let data_dir = prepared.data_dir.clone();
    let session_id = prepared.session_id.clone();
    let permission_store = PermissionStore::open(&data_dir).ok().map(Arc::new);
    let auto_resolve = Arc::new(AutoResolveStats::default());

    let (input_tx, _input_rx) = mpsc::unbounded_channel::<TurnInput>();
    let state = Arc::new(DaemonState {
        session_id: session_id.clone(),
        data_dir: data_dir.clone(),
        provider_id: prepared.provider_id,
        model: prepared.model.clone(),
        reasoning: None,
        hitl: Mutex::new(None),
        active_run: AtomicBool::new(false),
        cancel_flag: Mutex::new(None),
        pending_inputs: Mutex::new(None),
        pending_inputs_accepting: Mutex::new(None),
        run_mode: Mutex::new(prepared.run_mode),
        input_tx,
        permission_store,
        auto_resolve: Some(auto_resolve.clone()),
    });

    state.emit(&DaemonEvent::Started {
        session_id: session_id.clone(),
    });

    let started = std::time::Instant::now();
    let turn = run_turn(state.clone(), TurnInput::User(task));
    let outcome = match timeout_secs {
        Some(secs) => match tokio::time::timeout(Duration::from_secs(secs), turn).await {
            Ok(res) => res?,
            Err(_) => {
                // 超时：设 cancel flag 让正在跑的 run 尽快停（已 detach，下一个检查点退出）。
                state.stop();
                TurnOutcome::Cancelled
            }
        },
        None => turn.await?,
    };
    let timed_out = timeout_secs.is_some() && matches!(outcome, TurnOutcome::Cancelled);

    let exit_code = match &outcome {
        TurnOutcome::Done | TurnOutcome::Suspended => 0,
        TurnOutcome::Failed(_) => 1,
        TurnOutcome::Cancelled if timed_out => 2,
        TurnOutcome::Cancelled => 130,
    };

    if json {
        let summary = build_run_summary(
            &data_dir,
            &session_id,
            &outcome,
            &auto_resolve,
            started.elapsed().as_millis() as u64,
            exit_code,
        );
        println!("{summary}");
    } else {
        let denied = auto_resolve
            .denied_approvals
            .load(std::sync::atomic::Ordering::Relaxed);
        let cancelled = auto_resolve
            .cancelled_questions
            .load(std::sync::atomic::Ordering::Relaxed);
        let outcome_label = match &outcome {
            TurnOutcome::Done => "完成",
            TurnOutcome::Suspended => "挂起（等待后台任务）",
            TurnOutcome::Failed(_) => "失败",
            TurnOutcome::Cancelled if timed_out => "超时中断",
            TurnOutcome::Cancelled => "已取消",
        };
        eprintln!(
            "\n[heb run] {outcome_label}；自动拒审批 {denied} 次、自动取消提问 {cancelled} 次（exit {exit_code}）"
        );
    }

    Ok(exit_code)
}

/// 跑完后从 session.jsonl 读最终 assistant 段 + edits-worktree metadata，拼成单行结果 JSON。
fn build_run_summary(
    data_dir: &std::path::Path,
    session_id: &str,
    outcome: &TurnOutcome,
    auto_resolve: &AutoResolveStats,
    duration_ms: u64,
    exit_code: i32,
) -> String {
    let (final_text, tool_calls) = sessions::load(data_dir, session_id)
        .ok()
        .and_then(|s| {
            s.messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::Assistant))
                .map(|m| (m.content.clone(), m.tool_calls.len()))
        })
        .unwrap_or_default();

    let files_changed = read_run_edits_files(data_dir, session_id);

    let outcome_label = match outcome {
        TurnOutcome::Done => "done",
        TurnOutcome::Suspended => "suspended",
        TurnOutcome::Failed(_) => "failed",
        TurnOutcome::Cancelled => "cancelled",
    };
    let error = match outcome {
        TurnOutcome::Failed(e) => Some(e.clone()),
        _ => None,
    };

    serde_json::json!({
        "session_id": session_id,
        "outcome": outcome_label,
        "exit_code": exit_code,
        "final_text": final_text,
        "tool_calls": tool_calls,
        "files_changed": files_changed,
        "denied_approvals": auto_resolve.denied_approvals.load(std::sync::atomic::Ordering::Relaxed),
        "cancelled_questions": auto_resolve.cancelled_questions.load(std::sync::atomic::Ordering::Relaxed),
        "duration_ms": duration_ms,
        "error": error,
    })
    .to_string()
}

/// 读 edits-worktree metadata，汇总本 session 所有 Run 触达的真实文件路径（去重）。
/// 评测框架据此核对「agent 改了哪些文件」。无 git / 无改动时返回空。
fn read_run_edits_files(data_dir: &std::path::Path, session_id: &str) -> Vec<String> {
    use agent_core::edits::metadata;
    let worktree = metadata::worktree_dir(data_dir, session_id);
    let Ok(meta) = metadata::load_metadata(&worktree) else {
        return Vec::new();
    };
    let mut files = std::collections::BTreeSet::new();
    for run in &meta.runs {
        for f in &run.files {
            files.insert(f.real_path.clone());
        }
    }
    files.into_iter().collect()
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

#[cfg(test)]
mod translate_tests {
    use super::*;
    use protocol::{
        Event as AgentEvent, EventPayload, PermissionKind, PermissionRequestId, RiskLevel, RunId,
        SuspendReason,
    };

    /// 步骤4 收口回归（架构 §3.1.1）：cli 的 DaemonEvent 业务事件字段必须复用 protocol 的
    /// 集中 mapper，与 desktop/web 的 to_wire 输出**逐字段一致**。改前 cli 用 `format!("{:?}")`
    /// 产出 `"Critical"`/`"Cron"`（Debug 形态），与 WireEvent 的 `"critical"`/`"cron"` 不一致；
    /// 本测试钉死统一后的小写规范形态，任一处退回 Debug format 立即 fail。
    #[test]
    fn daemon_event_risk_and_reason_match_wire_canonical_form() {
        // risk: Critical → "critical"（不是 Debug 的 "Critical"）
        let perm = AgentEvent::now(
            RunId::new(),
            0,
            EventPayload::PermissionRequested {
                request_id: PermissionRequestId("r1".into()),
                kind: PermissionKind::ToolCall {
                    tool_name: "Write".into(),
                    input: serde_json::json!({"file_path": "/tmp/x"}),
                    fingerprint: None,
                    command_segments: vec![],
                    segments: vec![],
                    refuse_remember: false,
                },
                summary: "写文件".into(),
                risk: RiskLevel::Critical,
                auto_handled: false,
                call_id: "c1".into(),
            },
        );
        let de = translate_event(&perm).expect("permission_requested 应翻译");
        let json = serde_json::to_value(&de).unwrap();
        assert_eq!(json["risk"], "critical", "risk 必须是小写规范形态，与 to_wire 一致");
        assert_eq!(json["event"], "permission_requested");

        // suspend reason: Cron → "cron"（不是 Debug 的 "Cron"）
        let susp = AgentEvent::now(
            RunId::new(),
            1,
            EventPayload::RunSuspended {
                reason: SuspendReason::Cron,
                resumes_at_ms: None,
                waiting_for_task_ids: vec![],
            },
        );
        let de = translate_event(&susp).expect("run_suspended 应翻译");
        let json = serde_json::to_value(&de).unwrap();
        assert_eq!(json["reason"], "cron", "suspend reason 必须走 protocol mapper，与 to_wire 一致");

        // 与 to_wire 交叉验证：同一 payload 两侧 risk 字段逐字节一致
        let wire = protocol::to_wire(&perm).unwrap();
        let wire_json = serde_json::to_value(&wire).unwrap();
        assert_eq!(json_risk(&wire_json), "critical");
        assert_eq!(
            json_risk(&wire_json),
            serde_json::to_value(&translate_event(&perm).unwrap()).unwrap()["risk"]
        );
    }

    fn json_risk(v: &serde_json::Value) -> &str {
        v["risk"].as_str().unwrap_or("")
    }
}
