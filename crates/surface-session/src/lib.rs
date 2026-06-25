//! Surface 对话运行时（架构 §7.8.5 / §7.8.6 步骤③）：每个 session 一份的运行时状态
//! ([`SessionRuntime`]) + 跑一个 turn 的完整逻辑 ([`run_turn`])。
//!
//! 通用运行时（事件 broadcast / HITL pending / cancel / pending inputs / run_mode）由
//! agent_core 的 [`agent_core::session_hub::SessionRuntimeState`] 承载；本 crate 在其上补齐
//! 「构建 model client / Workspace / tools / CoreSession，驱动 agent_loop，把 WireEvent 推
//! broadcast」的 surface 侧运行逻辑。hebcore 常驻进程与 hebweb 共用同一份——消除重复
//! （session.rs 注释里早记的 "v2 与 daemon 共享 surface_session 模块" 落地）。
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc, Mutex};

use agent_core::{
    context::transcript::Transcript,
    definition::AgentDefinition,
    edits::EditsWorktree,
    hooks::HookManager,
    permissions::PermissionStore,
    read_state::ReadStateTracker,
    session_hub::SessionRuntimeState,
    storage::{
        sessions::{self as sessions, Message, Role},
        sessions_dir, settings as settings_store,
    },
    tools::{background, skill::default_skill_dirs},
    workspace::Workspace,
    Harness, Session as CoreSession, SessionConfig, TurnObserver, TurnOutcome,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use common::runtime::PendingInputs;
use tokio::sync::RwLock;
use std::collections::HashMap;
use model_gateway::{
    client::{DynModelClient, ModelClient},
    config as providers,
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use protocol::{
    ApprovalDecision, Event as AgentEvent, PermissionKind, PermissionRequestId, QuestionOption,
    UserAnswer,
};
use tokio::sync::{mpsc, oneshot};

/// 每个 session 一份的运行时状态。
///
/// 通用运行时（事件 broadcast / HITL pending / cancel / pending inputs / run_mode）下沉到
/// agent_core 的 [`SessionRuntimeState`]（架构 §7.8.5），hebweb 这层只保留 surface 特有
/// 部分（provider/model、输入驱动 input_tx、WS 协议包装 emit_engine_event）。
pub struct SessionRuntime {
    pub session_id: String,
    pub data_dir: PathBuf,
    pub provider_id: String,
    pub model: String,
    pub reasoning: Option<common::ReasoningConfig>,

    pub input_tx: mpsc::UnboundedSender<String>,
    pub permission_store: Option<Arc<PermissionStore>>,

    /// 下沉到 agent_core 的通用运行时状态（§7.8.5「单写者 + 多观察者」）。
    pub state: Arc<SessionRuntimeState>,
}

impl SessionRuntime {
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    pub fn set_active(&self, cancel: Arc<AtomicBool>, inputs: PendingInputs) {
        self.state.set_active(cancel, inputs);
    }

    pub fn clear_active(&self) {
        self.state.clear_active();
    }

    pub fn inject(&self, text: String) -> bool {
        self.state.inject(text)
    }

    pub fn stop(&self) {
        self.state.stop();
    }

    /// 把 [`WireEvent`] 广播给所有订阅本 session 的观察者（§7.8.5）。事件流统一在
    /// agent_core 的 broadcast 通道里走 WireEvent；WS 层（handle_ws）订阅后再包成
    /// [`WsServerMessage`] 发给浏览器。
    pub fn emit_engine_event(&self, ev: protocol::WireEvent) {
        self.state.emit(ev);
    }
}

/// broadcast 通道容量（慢订阅者落后会丢早帧）。
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// 活 session 运行时表（架构 §7.8.5）：`session_id → SessionRuntime`，多 surface 共享
/// 同一进程内的同一份活状态。hebweb 的 ServerState 与 hebcore 进程都用它管理对话 session。
#[derive(Default, Clone)]
pub struct RuntimeRegistry {
    sessions: Arc<RwLock<HashMap<String, Arc<SessionRuntime>>>>,
}

impl RuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前已 attach 的 session id 列表。
    pub async fn session_ids(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }

    /// 取已 attach 的 runtime（不自动创建）。
    pub async fn get(&self, session_id: &str) -> Option<Arc<SessionRuntime>> {
        self.sessions.read().await.get(session_id).cloned()
    }

    /// 移除一个 runtime（session 关闭）。
    pub async fn remove(&self, session_id: &str) -> Option<Arc<SessionRuntime>> {
        self.sessions.write().await.remove(session_id)
    }

    /// 取或按 `session.json` 自动 attach 一个 runtime（§7.8.5）。attach 时 spawn 一条
    /// input 循环：从 `input_tx` 收 user 文本，依次 [`run_turn`]——同一 session 串行跑 turn，
    /// 事件经 [`SessionRuntimeState`] 的 broadcast 推给全部订阅者。失败 emit `Error` 事件。
    pub async fn ensure(
        &self,
        data_dir: &std::path::Path,
        permission_store: Option<Arc<PermissionStore>>,
        session_id: &str,
    ) -> Result<Arc<SessionRuntime>> {
        if let Some(rt) = self.sessions.read().await.get(session_id).cloned() {
            return Ok(rt);
        }
        let mut guard = self.sessions.write().await;
        if let Some(rt) = guard.get(session_id).cloned() {
            return Ok(rt);
        }

        let session = sessions::load(data_dir, session_id)
            .map_err(|e| anyhow!("session {session_id} 不存在：{e}"))?;
        sessions_dir::ensure_session_dirs(data_dir, session_id)?;

        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let state = SessionRuntimeState::new(session_id, EVENT_CHANNEL_CAPACITY, session.run_mode);
        let runtime = Arc::new(SessionRuntime {
            session_id: session_id.to_string(),
            data_dir: data_dir.to_path_buf(),
            provider_id: session.provider_id.clone(),
            model: session.model.clone(),
            reasoning: None,
            input_tx,
            permission_store,
            state,
        });

        let rt_for_loop = runtime.clone();
        tokio::spawn(async move {
            while let Some(text) = input_rx.recv().await {
                if let Err(e) = run_turn(rt_for_loop.clone(), text).await {
                    rt_for_loop.emit_engine_event(protocol::WireEvent::Error {
                        message: e.to_string(),
                    });
                }
            }
        });

        guard.insert(session_id.to_string(), runtime.clone());
        Ok(runtime)
    }
}

// ─── Observer ──────────────────────────────────────────────────────────────

struct WebObserver {
    runtime: Arc<SessionRuntime>,
}

#[async_trait]
impl TurnObserver for WebObserver {
    fn on_event(&mut self, event: &AgentEvent) {
        // assistant 累积 + 落盘已收归 agent_core 唯一一份（架构 §7.8.3）：observer 只把
        // 事件翻译成 WireEvent 推到 WS 做渲染，不再自行重建 message。子 subagent NestedRun
        // 事件同样只推 WS 嵌套渲染（架构 §4.4.11.8），父过程累积由 agent_core persister 负责。
        if let Some(ev) = protocol::to_wire(event) {
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
            .state
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
            .state
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
        run_duration_ms: None,
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
    let inner = model_gateway::build_client_with_data_dir(provider, data_dir.to_path_buf())
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

    let run_mode = runtime.state.run_mode();
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
            derived_sink: None,
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
    };
    let summary = handle.drive(&mut observer).await;

    runtime.clear_active();

    // token_stats 由 agent_loop per-turn 落盘（sessions::bump_token_stats），不再 run-end 累加。
    // assistant 段 + 插队 user 的落盘已收归 agent_core（架构 §4.9.5）：agent_loop 在段边界 /
    // drain 边界 / run 收尾单点串行 append，surface 不再落盘（避免双落）。consumed_inputs
    // 仅 drain 清空避免 leak，不再据它补落 user。
    consumed_inputs.lock().unwrap().clear();

    match summary.outcome {
        // 架构 §4.12.1：Suspended 与 Done 都由 agent_core 落盘；不发 Error 让 web 前端
        // 正常显示挂起态，等 wakeup resume。
        TurnOutcome::Done | TurnOutcome::Suspended => {}
        TurnOutcome::Cancelled => {
            runtime.emit_engine_event(protocol::WireEvent::Error {
                message: "run 已取消".to_string(),
            });
        }
        TurnOutcome::Failed(err) => {
            runtime.emit_engine_event(protocol::WireEvent::Error { message: err });
        }
    }

    Ok(())
}
