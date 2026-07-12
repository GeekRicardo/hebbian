//! Surface 对话运行时（架构 §7）：每个 session 一份的运行时状态
//! ([`SessionRuntime`]) + 跑一个 turn 的完整逻辑 ([`run_turn`])。
//!
//! 通用运行时（事件 broadcast / HITL pending / cancel / pending inputs / run_mode）由
//! agent_core 的 [`agent_core::session_hub::SessionRuntimeState`] 承载；本 crate 在其上补齐
//! 「构建 model client / Workspace / tools / CoreSession，驱动 agent_loop，把 WireEvent 推
//! broadcast」的 surface 侧运行逻辑。Desktop / heb CLI / hebweb 必须复用这条入口，避免
//! 各 surface 复制 runner 后出现行为漂移。`transport` 模块仅保留为实验性远程 core 入口，
//! 不是默认路径。

pub mod transport;

use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc, Mutex};

use agent_core::{
    agent_loop::RunResumeState,
    context::transcript::Transcript,
    definition::AgentDefinition,
    edits::EditsWorktree,
    hooks::HookManager,
    permissions::PermissionStore,
    read_state::ReadStateTracker,
    session_hub::SessionRuntimeState,
    storage::{
        run_checkpoint,
        sessions::{self as sessions, Message, MessageMeta, Role},
        sessions_dir, settings as settings_store,
    },
    tools::{background, skill::default_skill_dirs},
    wakeup::WakeupScheduler,
    workspace::Workspace,
    Harness, Session as CoreSession, SessionConfig, TurnObserver, TurnOutcome,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use common::{attachments::MessageAttachment, runtime::PendingInputs};
use model_gateway::{
    client::{DynModelClient, ModelClient},
    config as providers,
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use protocol::{
    ApprovalDecision, AskQuestion, Event as AgentEvent, PermissionKind, PermissionRequestId,
    QuestionOption, ResumeCause, UserAnswer,
};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

/// surface 在 run 中观察到的状态变化。
#[derive(Debug, Clone)]
pub enum TurnStatus {
    Done,
    Suspended,
    Cancelled,
    Failed(String),
}

/// 每轮 run 的 surface 回调。核心事件仍先广播到 [`SessionRuntimeState`]；hook 只承载
/// surface 私有副作用，如 Desktop 的灵动岛、渠道转发、派生事件出口和完成提示。
#[derive(Clone, Default)]
pub struct SurfaceHooks {
    pub on_event: Option<Arc<dyn Fn(&protocol::WireEvent) + Send + Sync>>,
    pub derived_sink: Option<agent_core::agent_loop::EventSink>,
    pub on_status: Option<Arc<dyn Fn(TurnStatus) + Send + Sync>>,
    pub on_permission_request: Option<
        Arc<
            dyn Fn(&PermissionRequestId, &PermissionKind, &str) -> Option<ApprovalDecision>
                + Send
                + Sync,
        >,
    >,
    pub on_question: Option<
        Arc<
            dyn Fn(
                    &PermissionRequestId,
                    &str,
                    &[QuestionOption],
                    bool,
                    &[AskQuestion],
                ) -> Option<UserAnswer>
                + Send
                + Sync,
        >,
    >,
}

/// 一轮用户输入。文本与附件必须一起流经 surface-session；否则 Desktop / hebweb
/// 发送图片时，UI 会显示附件，但 agent_core 构造模型请求时只看见纯文本。
#[derive(Clone)]
pub struct TurnInput {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
    pub meta: Option<MessageMeta>,
    pub continue_run: bool,
    pub enabled_tools: Vec<String>,
    pub restrict_tools: Option<Vec<String>>,
    pub hooks: SurfaceHooks,
}

impl std::fmt::Debug for TurnInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnInput")
            .field("text", &self.text)
            .field("attachments", &self.attachments)
            .field("meta", &self.meta)
            .field("continue_run", &self.continue_run)
            .field("enabled_tools", &self.enabled_tools)
            .field("restrict_tools", &self.restrict_tools)
            .finish_non_exhaustive()
    }
}

impl TurnInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(text, Vec::new())
    }

    pub fn new(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self {
            text: text.into(),
            attachments,
            meta: None,
            continue_run: false,
            enabled_tools: Vec::new(),
            restrict_tools: None,
            hooks: SurfaceHooks::default(),
        }
    }

    pub fn with_meta(mut self, meta: Option<MessageMeta>) -> Self {
        self.meta = meta;
        self
    }

    pub fn with_continue_run(mut self, continue_run: bool) -> Self {
        self.continue_run = continue_run;
        self
    }

    pub fn with_enabled_tools(mut self, enabled_tools: Vec<String>) -> Self {
        self.enabled_tools = enabled_tools;
        self
    }

    pub fn with_restrict_tools(mut self, restrict_tools: Option<Vec<String>>) -> Self {
        self.restrict_tools = restrict_tools;
        self
    }

    pub fn with_hooks(mut self, hooks: SurfaceHooks) -> Self {
        self.hooks = hooks;
        self
    }
}

/// 每个 session 一份的运行时状态。
///
/// 通用运行时（事件 broadcast / HITL pending / cancel / pending inputs / run_mode）下沉到
/// agent_core 的 [`SessionRuntimeState`]（架构 §7），surface-session 只保留 surface 共享
/// 的输入驱动 input_tx 与事件转发入口。provider / model / reasoning 每轮从最新 session
/// 元数据读取，避免输入框切模型后活 runtime 继续使用旧快照。
pub struct SessionRuntime {
    pub session_id: String,
    pub data_dir: PathBuf,

    pub input_tx: mpsc::UnboundedSender<TurnInput>,
    pub permission_store: Option<Arc<PermissionStore>>,

    /// 常驻 stop flag：`run_turn` 每次复用它作为 cancel flag，而非每次新建。
    /// `stop()` 随时可设——不依赖 `set_active` 是否已被调用，消除「点了 Stop 但
    /// run 还没初始化完，stop() 变空操作」的竞态窗口。
    pub stop_flag: Arc<AtomicBool>,

    /// 下沉到 agent_core 的通用运行时状态（§7「单写者 + 多观察者」）。
    pub state: Arc<SessionRuntimeState>,
}

impl SessionRuntime {
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    pub fn set_active(
        &self,
        hitl: Arc<agent_core::tools::hitl::HitlGate>,
        cancel: Arc<AtomicBool>,
        inputs: PendingInputs,
        accepting: Arc<AtomicBool>,
    ) {
        self.state.set_active(hitl, cancel, inputs, accepting);
    }

    pub fn clear_active(&self) {
        self.state.clear_active();
    }

    pub fn inject(&self, input: TurnInput) -> bool {
        let meta = input.meta.map(|m| match m {
            agent_core::storage::sessions::MessageMeta::SystemNotification {
                kind,
                task_id,
                tool_use_id,
            } => protocol::PendingMessageMeta::SystemNotification {
                kind,
                task_id,
                tool_use_id,
            },
            _ => protocol::PendingMessageMeta::SystemNotification {
                kind: "other".to_string(),
                task_id: None,
                tool_use_id: None,
            },
        });
        self.state.inject(common::runtime::PendingUserInput {
            content: input.text,
            attachments: input.attachments,
            meta,
        })
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.state.stop();
    }

    /// 把 [`WireEvent`] 广播给所有订阅本 session 的观察者（§7）。事件流统一在
    /// agent_core 的 broadcast 通道里走 WireEvent；各 surface 订阅后再转成自己的出口。
    pub fn emit_engine_event(&self, ev: protocol::WireEvent) {
        self.state.emit(ev);
    }
}

/// broadcast 通道容量（慢订阅者落后会丢早帧）。
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// 活 session 运行时表（架构 §7）：`session_id → SessionRuntime`。每个 surface 进程持有
/// 自己的 registry；同一进程内同 session 串行跑 turn，跨进程并发由 SessionRunGuard 兜底。
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

    /// 是否有任一 session 正在跑活跃 run。实验性远程 transport 关停前用它做安全闸：
    /// 有 run 不许关停，护住 §4.9.2 partial 落盘。
    pub async fn has_active_run(&self) -> bool {
        self.sessions.read().await.values().any(|rt| rt.is_active())
    }

    /// 按用户主动退出语义取消全部活跃 run，然后等待它们完成持久化收尾。
    pub async fn cancel_active_runs_and_wait(&self) {
        let runtimes: Vec<_> = self.sessions.read().await.values().cloned().collect();
        for rt in &runtimes {
            if rt.is_active() {
                rt.stop();
            }
        }

        while runtimes.iter().any(|rt| rt.is_active()) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// 移除一个 runtime（session 关闭）。
    pub async fn remove(&self, session_id: &str) -> Option<Arc<SessionRuntime>> {
        self.sessions.write().await.remove(session_id)
    }

    /// 取或按 `session.json` 自动 attach 一个 runtime（§7）。attach 时 spawn 一条
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

        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<TurnInput>();
        let state = SessionRuntimeState::new(session_id, EVENT_CHANNEL_CAPACITY, session.run_mode);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let runtime = Arc::new(SessionRuntime {
            session_id: session_id.to_string(),
            data_dir: data_dir.to_path_buf(),
            input_tx,
            permission_store,
            stop_flag: stop_flag.clone(),
            state,
        });

        let rt_for_loop = runtime.clone();
        tokio::spawn(async move {
            while let Some(input) = input_rx.recv().await {
                // 竞态防护：点了 Stop 但 run 还没调 set_active → stop_flag 已设，
                // 跳过本轮（不调 run_turn），清除 flag 让后续 run 正常走。
                if stop_flag.load(Ordering::SeqCst) {
                    stop_flag.store(false, Ordering::SeqCst);
                    continue;
                }
                if let Err(e) = run_turn(rt_for_loop.clone(), input).await {
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

/// 注册 wakeup resume handler（架构 §4.12.5 / §7）。后台任务完成 / cron 到点的 wakeup
/// 事件在**run 所在进程**的 [`agent_core::wakeup::WakeupScheduler`] 触发，handler 也必须在
/// 同一进程注册，否则挂起 run 收不到 resume。
///
/// 回调按 `event.session_id()` 找对应 session 运行时：
/// - 活 run 在跑 → [`SessionRuntime::inject`] 进 PendingInputs，agent_loop 下个 ModelStep 前 drain；
/// - 无活 run → `input_tx` 触发新 run（input 循环的 `run_turn` 会 append_user 落 wakeup_xml）。
///   两条路径都只落一次，**不预落**避免与 run_turn 重复。
///
/// 每个 surface 进程启动时调一次。实验性 transport 可继续通过
/// [`register_wakeup_resume_handler_for_transport`] 包装调用本函数。
pub fn register_wakeup_resume_handler(
    data_dir: PathBuf,
    permission_store: Option<Arc<PermissionStore>>,
    runtimes: RuntimeRegistry,
) {
    let scheduler = agent_core::wakeup::WakeupScheduler::global();
    let recovery_data_dir = data_dir.clone();
    scheduler.set_resume_handler(Arc::new(move |event| {
        let data_dir = data_dir.clone();
        let permission_store = permission_store.clone();
        let runtimes = runtimes.clone();
        // ResumeHandler 是同步闭包，而按 session 取运行时是 async（RwLock）——spawn 一条
        // 短 task 完成注入。handler 在 WakeupDispatcher 的 tokio task 里被调，spawn 安全。
        tokio::spawn(async move {
            let sid = event.session_id().to_string();
            let rt = match runtimes.get(&sid).await {
                Some(rt) => rt,
                None => match runtimes.ensure(&data_dir, permission_store, &sid).await {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::warn!(session = %sid, error = %e, "wakeup resume: attach session 失败");
                        return;
                    }
                },
            };
            let wakeup_xml = agent_core::wakeup::wakeup_xml(&event);
            let wakeup_input = TurnInput::text(wakeup_xml.clone()).with_meta(Some(event.message_meta()));
            tracing::info!(
                target: "wakeup",
                session_id = %sid,
                active = rt.is_active(),
                "[Wakeup:Resume] 后台任务 / cron 唤醒续跑"
            );
            if !rt.inject(wakeup_input.clone()) {
                if let Err(e) = rt.input_tx.send(wakeup_input) {
                    tracing::warn!(session = %sid, error = %e, "wakeup resume: input_tx 已关闭");
                }
            }
        });
    }));
    // 进程重启后恢复挂起的 cron（必须在 set_resume_handler 之后，否则到点无 handler）
    scheduler.recover_pending_crons(&recovery_data_dir);
}

/// 实验性远程 transport 的兼容包装。默认 surface 不应依赖 transport ctx。
pub fn register_wakeup_resume_handler_for_transport(ctx: Arc<crate::transport::TransportCtx>) {
    register_wakeup_resume_handler(
        ctx.data_dir.clone(),
        ctx.permission_store.clone(),
        ctx.runtimes.clone(),
    );
}

// ─── Observer ──────────────────────────────────────────────────────────────

struct WebObserver {
    runtime: Arc<SessionRuntime>,
    hooks: SurfaceHooks,
}

#[async_trait]
impl TurnObserver for WebObserver {
    fn on_event(&mut self, event: &AgentEvent) {
        // assistant 累积 + 落盘已收归 agent_core 唯一一份（架构 §7）：observer 只把
        // 事件翻译成 WireEvent 推给 surface 渲染，不再自行重建 message。子 subagent NestedRun
        // 事件同样只推给 surface 嵌套渲染（架构 §4.4.11.8），父过程累积由 agent_core persister 负责。
        if let Some(ev) = protocol::to_wire(event) {
            self.runtime.emit_engine_event(ev.clone());
            if let Some(hook) = &self.hooks.on_event {
                hook(&ev);
            }
        }
    }

    async fn on_permission_request(
        &mut self,
        request_id: &PermissionRequestId,
        kind: &PermissionKind,
        summary: &str,
    ) -> Option<ApprovalDecision> {
        if let Some(hook) = &self.hooks.on_permission_request {
            if let Some(decision) = hook(request_id, kind, summary) {
                return Some(decision);
            }
        }
        // 不在 drive loop 里等审批结果（那会阻塞单线程 recv，让 AutoMode judge 后发的
        // PermissionAutoJudged pump 不出去而死锁）。审批通道是 agent_loop 持有的真
        // HitlGate（run 启动时已 set_active 挂到 SessionRuntimeState），surface 的回应经
        // 统一控制入口 → state.resolve_approval 直接戳它。这里返回 None 让 drive 立即继续 recv。
        None
    }

    async fn on_question(
        &mut self,
        request_id: &PermissionRequestId,
        question: &str,
        options: &[QuestionOption],
        multi: bool,
        questions: &[AskQuestion],
    ) -> Option<UserAnswer> {
        if let Some(hook) = &self.hooks.on_question {
            if let Some(answer) = hook(request_id, question, options, multi, questions) {
                return Some(answer);
            }
        }
        // 同 on_permission_request：不阻塞 drive loop，提问回应经统一控制入口 →
        // state.answer_question 直接戳活 run 的 HitlGate。
        None
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

pub async fn run_turn(runtime: Arc<SessionRuntime>, input: TurnInput) -> Result<()> {
    let data_dir = &runtime.data_dir;
    let session_id = &runtime.session_id;
    let TurnInput {
        text: user_text,
        attachments,
        meta,
        continue_run,
        enabled_tools: input_enabled_tools,
        restrict_tools,
        hooks,
    } = input;

    // 单写者闸口（架构 §7）：抢 session 级 run 锁，持有到本函数返回（_run_guard 在栈上）。
    // 抢不到 = 同一 session 已有活 run（本进程另一通路 / 另一 surface 进程共享数据目录）——
    // 直接拒绝，绝不起第二个并发 run，否则两个 run 各自 append user / persist assistant 到同一
    // session.jsonl 造成 transcript 交错、HITL/cancel 句柄互相覆盖（#9）。有活 run 时「发消息」
    // 应走 inject 插队，而非再起 run_turn。
    let Some(_run_guard) = sessions_dir::SessionRunGuard::try_acquire(data_dir, session_id) else {
        return Err(anyhow!(
            "session {session_id} 已有活跃 run，拒绝并发启动（请用插队 / 等当前 run 结束）"
        ));
    };

    // send 入口：先把上次中断残留的 partial 折叠进 jsonl 再读历史（同 chat::send_and_save）。
    let prior = sessions::load_with_partial_recovery(data_dir, session_id)?;

    // 任何新 run 起步（普通用户消息 / continue_run / wakeup resume）都要清掉旧的
    // pending_continue。它只属于上一轮非正常结束留下的续作入口；一旦用户或系统已经
    // 启动了下一轮，再把旧 chip 留在 session 里只会让 surface 误以为当前挂起/新轮
    // 仍可继续上一轮。
    sessions::set_pending_continue(data_dir, session_id, None)?;

    if continue_run {
    } else {
        let user_msg = Message {
            id: sessions::new_id(),
            role: Role::User,
            content: user_text.clone(),
            attachments: attachments.clone(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: Utc::now().timestamp_millis(),
            meta: meta.clone(),
            subagent_call_id: None,
            run_duration_ms: None,
        };
        sessions::append_message(data_dir, session_id, user_msg)?;
    }

    // model client
    let providers_file = providers::load(data_dir)?;
    let provider = providers_file
        .providers
        .iter()
        .find(|p| p.id == prior.provider_id)
        .ok_or_else(|| anyhow!("provider {} 不存在", prior.provider_id))?
        .clone();
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| anyhow!("OAuth token 刷新失败: {e}"))?;
    let ctx_window =
        model_gateway::context_window::effective_context_window_for(&provider, &prior.model);
    let vision = agent_core::vision_bridge::build_vision_client(data_dir)
        .await
        .map_err(|e| anyhow!("vision bridge: {e}"))?;
    let inner = model_gateway::build_client_with_data_dir(provider, data_dir.to_path_buf())
        .map_err(|e| anyhow!("构建 model client 失败: {e}"))?;
    let inner = agent_core::vision_bridge::wrap_with_vision_client(inner, vision);
    let client: Arc<dyn ModelClient> = Arc::new(NamedModelClient::new(
        inner,
        prior.model.clone(),
        prior.reasoning.clone(),
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

    let mut tools = agent_core::tools::default_tools_with_mcp(
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
    .await;
    if let Some(allow) = &restrict_tools {
        tools.retain(|tool| {
            let name = tool.name();
            name.starts_with("Mcp__") || allow.iter().any(|allowed| allowed == name)
        });
    }

    let harness = Arc::new(Harness::new(tools, HookManager::new(external_hooks)));

    let model_io_dump =
        agent_core::model_io_dump::open_for_session_if_enabled(data_dir, session_id).await;

    if let Some(store) = &runtime.permission_store {
        store.ensure_session_view(session_id);
    }

    let run_mode = runtime.state.run_mode();
    let enabled_tools = {
        let s = prior.enabled_tools.clone().unwrap_or_default();
        if !input_enabled_tools.is_empty() {
            input_enabled_tools
        } else if s.is_empty() {
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
            model_id: Some(prior.model.clone()),
            force_automode: agent_core::run_mode::LiveForceAutomodeRegistry::global()
                .get(session_id),
            // surface 主对话：tag=Main（前端不额外标记，§4.11）。
            call_tag: model_gateway::types::ModelCallTag::Main,
            data_dir: Some(data_dir.to_path_buf()),
            phase: Some(phase.clone()),
            global_rules,
            rules_files,
            edits_worktree: Some(edits_worktree),
            derived_sink: hooks.derived_sink.clone(),
        },
    );
    if !continue_run {
        core_session.append_user(user_text, attachments);
    }

    // 使用 runtime 的常驻 stop_flag 而非每次新建，消除「Stop 先到但 run_turn
    // 还没跑完初始化 → stop() 空操作」的竞态窗口。
    let cancel_flag = runtime.stop_flag.clone();
    // 清上次残留的 stop 信号（上一次 run 被 stop 后 flag 可能仍为 true）
    cancel_flag.store(false, Ordering::SeqCst);
    let pending_inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
    // accepting=true 起步；agent_loop 在末次 drain / run 收尾置 false，inject 据此在收尾
    // 窗口拒绝晚到注入（§4.2.3），让 surface 回落「起新 run」而不是把消息丢进死队列。
    let pending_inputs_accepting = Arc::new(AtomicBool::new(true));
    let consumed_inputs = Arc::new(Mutex::new(Vec::new()));

    // 架构 §4.12.6：本 session 若有挂起 checkpoint，走 resume 路径（进程重启后亦生效）。
    // 与 desktop chat.rs:418-451 行为对称。
    let resume_state = run_checkpoint::load(data_dir, session_id)
        .ok()
        .flatten()
        .map(|ckpt| {
            // checkpoint 已被本 turn 接管，删文件 + 摘除调度器对该 run 的登记，
            // 防止 cron/bg-task 之后又触发一次重复 resume。
            let _ = run_checkpoint::delete(data_dir, session_id);
            WakeupScheduler::global().discard_run(session_id, &ckpt.run_id);
            let cause = match &ckpt.phase {
                agent_core::storage::run_checkpoint::RunPhase::AwaitingCron { reason, .. } => {
                    ResumeCause::CronFired {
                        original_reason: reason.clone(),
                    }
                }
                agent_core::storage::run_checkpoint::RunPhase::AwaitingBackgroundTask {
                    task_id,
                    ..
                } => ResumeCause::BgTaskFinished {
                    task_id: task_id.clone(),
                    exit_code: None,
                },
            };
            RunResumeState::from_checkpoint(ckpt, cause)
        });

    let mut handle = match resume_state {
        Some(rs) => core_session.resume_with_runtime_inputs(
            cancel_flag.clone(),
            Some(pending_inputs.clone()),
            Some(consumed_inputs.clone()),
            Some(pending_inputs_accepting.clone()),
            Some(phase.clone()),
            rs,
        ),
        None => core_session.run_with_runtime_inputs(
            cancel_flag.clone(),
            Some(pending_inputs.clone()),
            Some(consumed_inputs.clone()),
            Some(pending_inputs_accepting.clone()),
        ),
    };

    // 把活 run 的真 HitlGate 挂进运行时状态：surface 的审批 / 提问回应经统一控制入口
    // 直接戳它，observer 不再自造 oneshot gate 阻塞 drive loop。
    runtime.set_active(
        handle.hitl().clone(),
        cancel_flag,
        pending_inputs,
        pending_inputs_accepting,
    );

    let mut observer = WebObserver {
        runtime: runtime.clone(),
        hooks: hooks.clone(),
    };
    let summary = handle.drive(&mut observer).await;

    runtime.clear_active();
    // 清除本轮可能被 stop() 设上的 flag，让输入循环的下一轮 recv 不会把它当
    // 成「新 run 启动前的 stop 请求」而跳过用户刚发的新消息。
    runtime.stop_flag.store(false, Ordering::SeqCst);

    // token_stats 由 agent_loop per-turn 落盘（sessions::bump_token_stats），不再 run-end 累加。
    // assistant 段 + 插队 user 的落盘已收归 agent_core（架构 §4.9.5）：agent_loop 在段边界 /
    // drain 边界 / run 收尾单点串行 append，surface 不再落盘（避免双落）。consumed_inputs
    // 仅 drain 清空避免 leak，不再据它补落 user。
    consumed_inputs.lock().unwrap().clear();

    match summary.outcome {
        TurnOutcome::Done => {
            if let Some(hook) = &hooks.on_status {
                hook(TurnStatus::Done);
            }
        }
        TurnOutcome::Suspended => {
            if let Some(hook) = &hooks.on_status {
                hook(TurnStatus::Suspended);
            }
        }
        TurnOutcome::Cancelled => {
            if let Some(hook) = &hooks.on_status {
                hook(TurnStatus::Cancelled);
            }
            runtime.emit_engine_event(protocol::WireEvent::Error {
                message: "run 已取消".to_string(),
            });
        }
        TurnOutcome::Failed(err) => {
            if let Some(hook) = &hooks.on_status {
                hook(TurnStatus::Failed(err.clone()));
            }
            runtime.emit_engine_event(protocol::WireEvent::Error { message: err });
        }
    }

    Ok(())
}
