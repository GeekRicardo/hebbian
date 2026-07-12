use crate::error::{AppError, AppResult};
use crate::hebisland_client::{HebislandClient, IslandCard, IslandOption, IslandQuestion};
use crate::hitl::HitlState;
use agent_core::storage::{
    sessions::{self, Message, MessageMeta, Role},
    settings as global_settings,
};
use agent_core::{
    context::transcript::Transcript,
    definition::AgentDefinition,
    edits::EditsWorktree,
    hooks::HookManager,
    permissions::PermissionStore,
    read_state::ReadStateTracker,
    tools::{hitl::HitlGate, skill::default_skill_dirs},
    types::AgentEvent,
    workspace::Workspace,
    Harness, Session as CoreSession, SessionConfig, TurnObserver, TurnOutcome,
};
use async_trait::async_trait;
use common::{
    attachments::MessageAttachment,
    runtime::{ConsumedPendingInputs, PendingInputs, PendingUserInput},
    CancelFlag,
};
use model_gateway::{self, config::Provider};
use protocol::{
    ApprovalDecision, EventPayload, PermissionKind, PermissionRequestId, QuestionOption, UserAnswer,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::Manager;
use tauri::{ipc::Channel, AppHandle, Emitter};

pub struct SendArgs {
    pub session_id: String,
    pub user_content: String,
    pub attachments: Vec<MessageAttachment>,
    /// 给落盘的 user message 附加的 meta（架构 §4.12.5）。idle 路径下 wakeup 通过
    /// send_message 触发新 run 时用——让物理 jsonl 里这条 user message 带
    /// `MessageMeta::SystemNotification` 标记，view 区别渲染。`None` = 普通用户输入。
    pub user_meta: Option<agent_core::storage::sessions::MessageMeta>,
    pub stream: bool,
    pub enabled_tools: Vec<String>,
    pub cancel_flag: CancelFlag,
    /// 运行时输入注入队列：前端「立即发送」会把 user message 推进来，
    /// agent_loop 在每次 model.request 之前 drain 出来加入 transcript。
    /// 测试场景传 `None`，相当于一个空队列。
    pub pending_inputs: Option<PendingInputs>,
    /// agent_loop 已消费的 pending input。Desktop 用它在 run 结束后按正确顺序落盘。
    pub consumed_pending_inputs: Option<ConsumedPendingInputs>,
    /// agent_loop 结束后关闭；late inject 看到 false 后回落到下一轮 run。
    pub pending_inputs_accepting: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// app 级 HITL 桥接。用 Tauri 的 `app.state::<Arc<HitlState>>()`
    /// 取出来塞进来。测试场景传 `None`。
    pub hitl: Option<Arc<HitlState>>,
    /// 全局共享的 PermissionStore（架构 §4.6）。`None` = 不启用持久化权限规则
    /// （AllowAndRemember(Global) 退化为 AllowOnce）。
    pub permission_store: Option<Arc<PermissionStore>>,
    /// `force_automode` 子开关当前值（架构 §4.4.4 / §8）。仅 RunMode=AutoMode
    /// 下生效：判官 Ask 折叠为 Deny。Desktop 用 `//force-automode` 命令切换，
    /// 状态由 `ForceAutomodeState` 进程级持有；测试场景传 `false`。
    pub force_automode: bool,
    pub request_id: Option<String>,
    /// 「继续」入口（架构 §4.3 / §7.3）：true = 不追加任何 user 消息，直接用当前
    /// transcript 原样再起一次 agent_loop。失败请求天然重发、截断让模型接着写。
    /// `user_content` 此时应为空。
    pub continue_run: bool,
    /// 工具白名单：`Some(names)` 时把 registry 过滤到只含这些工具（+ MCP），
    /// `None` = 全量。内置浏览器「元素对话」旁支会话用它把工具限制成只有 `PreviewStyle`，
    /// 否则 LLM 会看到 Bash/Edit 等内置工具——既危险又会在 `hitl=None` 下挂死。
    pub restrict_tools: Option<Vec<String>>,
}

impl SendArgs {
    /// 给 `restrict_tools` 默认 `None` 的便捷方法——避免每个构造点都写一遍。
    /// 现有构造点保持显式字段；新加这个仅为可读性，不强制使用。
    pub fn no_restrict() -> Option<Vec<String>> {
        None
    }
}

pub fn data_dir(_app: &AppHandle) -> AppResult<std::path::PathBuf> {
    // 架构 §6.1 / 决策 D10：CLI 与 Desktop 共享 ~/.hebbian/。
    // 不要走 Tauri 的 `app_data_dir`（macOS 下指向 ~/Library/Application Support/...），
    // 否则 send_message 会与 lib.rs::data_dir 写盘路径错位，create_session 写到
    // ~/.hebbian/sessions/<sid>/ 但 send_message 去 Tauri bundle 目录读，立刻 not found。
    Ok(agent_core::storage::default_data_dir())
}

fn matches_injected_user_message(
    msg: &Message,
    content: &str,
    attachments: &[MessageAttachment],
    meta: &Option<MessageMeta>,
) -> bool {
    msg.role == Role::User
        && msg.content == content
        && msg.attachments == attachments
        && msg.tool_calls.is_empty()
        && msg.parts.is_empty()
        && &msg.meta == meta
}

pub async fn send_and_save(
    app: &AppHandle,
    args: SendArgs,
    on_event: Channel<protocol::WireEvent>,
) -> AppResult<Message> {
    let dd = data_dir(app)?;
    let app_for_island = app.clone();
    let session_id_for_forward = args.session_id.clone();
    // 派生事件旁路（架构 §4.14.7）：标题 / 记忆在 run 收尾后才完成，走 per-message
    // Channel（invoke 返回即废弃）会丢。改走 app 级全局事件总线 `engine-derived-event`
    // ——与 `wakeup-fired` 同款 long-lived 出口，前端 listen 全局订阅。
    let app_for_derived = app.clone();
    let derived_sink: agent_core::agent_loop::EventSink = Arc::new(move |event: AgentEvent| {
        if let Some(ev) = protocol::to_wire(&event) {
            if let Err(e) = app_for_derived.emit("engine-derived-event", ev) {
                tracing::warn!(error = %e, "failed to emit engine-derived-event");
            }
        }
    });
    let result = send_and_save_in_data_dir(
        &dd,
        args,
        move |event| {
            if let Some(client) = app_for_island.try_state::<HebislandClient>() {
                push_engine_event_to_island(&client, &event);
            }
            // 机主离开电脑时把审批/问题转发到聊天渠道（微信）；在线且空闲才触发。
            crate::channel_forward::maybe_forward(&app_for_island, &session_id_for_forward, &event);
            let _ = on_event.send(event);
        },
        Some(derived_sink),
    )
    .await;
    // 整轮 run 真正结束才弹一次「回答完成」（多回合只弹一次；取消 / 失败不弹）。
    if result.is_ok() {
        if let Some(client) = app.try_state::<HebislandClient>() {
            client.show(IslandCard::new(
                format!("done-{}", chrono::Utc::now().timestamp_millis()),
                "info",
                "回答完成",
                "Agent 已完成本次回答",
            ));
        }
    }
    result
}

pub async fn send_and_save_in_data_dir(
    data_dir: &Path,
    args: SendArgs,
    emit_event: impl Fn(protocol::WireEvent) + Send + Sync + 'static,
    derived_sink: Option<agent_core::agent_loop::EventSink>,
) -> AppResult<Message> {
    // 预构建 vision client（async：需要刷新 OAuth token）。
    // 未配置 vision provider 时为 None，闭包里跳过包装。
    let vision_client = agent_core::vision_bridge::build_vision_client(data_dir)
        .await
        .map_err(|e| AppError::msg(format!("vision bridge: {e}")))?;
    // 闭包要 'static（client factory 会进 agent_loop），data_dir 借不进去——拷一份 owned。
    let dd = data_dir.to_path_buf();
    send_and_save_in_data_dir_with_client_factory(
        data_dir,
        args,
        emit_event,
        derived_sink,
        move |provider, model, reasoning| {
            // 带 data_dir：启用 401 自愈刷新（长 HITL 审批后 token 失效会自动续期重试）。
            let client = model_gateway::build_client_with_data_dir(provider, dd.clone())
                .map_err(|e| AppError::msg(format!("无法创建 ModelClient: {e}")))?;
            let client =
                agent_core::vision_bridge::wrap_with_vision_client(client, vision_client.clone());
            Ok(
                Arc::new(ModelWithName::with_reasoning(client, model, reasoning))
                    as Arc<dyn ModelClient>,
            )
        },
    )
    .await
}

pub async fn send_and_save_in_data_dir_with_client_factory(
    data_dir: &Path,
    args: SendArgs,
    emit_event: impl Fn(protocol::WireEvent) + Send + Sync + 'static,
    derived_sink: Option<agent_core::agent_loop::EventSink>,
    build_client: impl Fn(Provider, String, Option<common::ReasoningConfig>) -> AppResult<Arc<dyn ModelClient>>
        + Send
        + Sync,
) -> AppResult<Message> {
    // send 入口：先把上次中断残留的 partial 折叠进 session.jsonl，再读历史。
    // 内部的 sessions::load 是纯读路径（避免 turn 进行中误把活跃 partial 当成中断）。
    let prior_session = sessions::load_with_partial_recovery(data_dir, &args.session_id)?;

    let is_system_notification = args
        .user_meta
        .as_ref()
        .is_some_and(MessageMeta::is_system_notification);
    let already_persisted_notification = is_system_notification
        && prior_session.messages.iter().any(|msg| {
            matches_injected_user_message(
                msg,
                &args.user_content,
                &args.attachments,
                &args.user_meta,
            )
        });
    let mut initial_messages = prior_session.messages.clone();
    if already_persisted_notification {
        initial_messages.retain(|msg| {
            !matches_injected_user_message(
                msg,
                &args.user_content,
                &args.attachments,
                &args.user_meta,
            )
        });
    }
    // 「继续」入口（架构 §4.3）：不追加任何 user 消息，用当前 transcript 原样再跑。
    // 先把续作入口清掉——这一轮要么正常完成（agent_loop 收尾也会清），要么再次
    // 异常并由 agent_loop 重新写入新的 pending_continue。
    if args.continue_run && !already_persisted_notification {
        sessions::set_pending_continue(data_dir, &args.session_id, None)?;
    }
    let session = if already_persisted_notification || args.continue_run {
        prior_session.clone()
    } else {
        let user_msg = Message {
            id: sessions::new_id(),
            role: Role::User,
            content: args.user_content.clone(),
            attachments: args.attachments.clone(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: chrono::Utc::now().timestamp_millis(),
            meta: args.user_meta.clone(),
            subagent_call_id: None,
            run_duration_ms: None,
        };
        sessions::append_message(data_dir, &args.session_id, user_msg)?
    };

    let provider = model_gateway::config::get(data_dir, &session.provider_id)?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| AppError::msg(format!("OAuth token 刷新失败: {e}")))?;
    let model = session.model.clone();
    let reasoning = session.reasoning.clone();

    let ctx_window = model_gateway::context_window::effective_context_window_for(&provider, &model);
    let client = build_client(provider, model.clone(), reasoning)?;

    // Workspace：session 字段优先；没设则用全局设置；都没设则 ~/
    let settings = global_settings::load(data_dir);
    let workdir = session
        .workdir
        .clone()
        .or_else(|| settings.conversation.workdir.clone())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    // initial = session 起始时锁定的允许路径（首条 user message 之前可改的部分），
    // 缺省时回退到全局默认；runtime_announced 是已经通知模型的运行时追加；
    // runtime_pending 是这次 send_message 时要在 user message 头部补充宣告的部分。
    let initial_allowed_paths = session
        .allowed_paths
        .clone()
        .unwrap_or_else(|| settings.conversation.allowed_paths.clone());
    let workspace = Workspace::with_runtime_state(
        workdir.clone(),
        initial_allowed_paths,
        session.runtime_allowed_paths.clone(),
        session.pending_runtime_allowed_paths.clone(),
    );

    let configured_skill_dirs = session
        .skill_dirs
        .clone()
        .unwrap_or_else(|| settings.conversation.skill_dirs.clone());
    let skill_dirs: Vec<(agent_core::tools::skill::SkillSource, std::path::PathBuf)> =
        if configured_skill_dirs.is_empty() {
            default_skill_dirs(data_dir, &workdir)
        } else {
            configured_skill_dirs
                .into_iter()
                .map(|p| (agent_core::tools::skill::SkillSource::Global, p))
                .collect()
        };

    let hook_cfg = agent_core::hooks::load_hooks_config(data_dir, Some(workspace.workdir()));
    let external_hooks = agent_core::hooks::ExternalHook::from_config(hook_cfg);
    // 架构 §4.12.3：BashTool 转后台时把 stdout/stderr 落到 `<sid>/bg/<task_id>.log`。
    let bg_log_dir = Some(agent_core::storage::sessions_dir::bg_dir(
        data_dir,
        &args.session_id,
    ));
    // 架构 §4.12.4：phase channel 让挂起工具把"挂起请求"递给 agent_loop。
    let phase = agent_core::wakeup::new_phase_channel();
    // 架构 §4.12.2 修订：BgTaskRegistry 是 session-scoped，跨 chat() 调用通过
    // `registry_for_session` 拿同一份；不同 session 完全隔离。
    let shells = agent_core::tools::background::registry_for_session(&args.session_id);
    // 把本 session 的 shells 注册到 WakeupScheduler，BgFinishHook 才能扫到。
    agent_core::wakeup::WakeupScheduler::global()
        .register_session_shells(args.session_id.clone(), shells.clone());
    let read_state_tracker = Arc::new(ReadStateTracker::new());
    let edits_worktree = Arc::new(EditsWorktree::new(data_dir, &args.session_id, &workspace));
    let mut tools = agent_core::tools::default_tools_with_mcp(
        workspace.clone(),
        &skill_dirs,
        bg_log_dir,
        phase.clone(),
        shells,
        Some(data_dir.to_path_buf()),
        Some(args.session_id.clone()),
        Some(read_state_tracker),
        settings.general.shell.clone(),
        settings.general.edit_backend,
        agent_core::storage::mcp::load(data_dir).with_cwd(workspace.workdir().to_path_buf()),
    )
    .await;
    // 工具白名单（元素对话旁支会话用）：把 registry 限制到只含指定工具（MCP 工具放行，
    // 它们的暴露另由 enabled_tools 控）。普通 send restrict_tools=None，不过滤。
    if let Some(allow) = &args.restrict_tools {
        tools.retain(|t| {
            let n = t.name();
            n.starts_with("Mcp__") || allow.iter().any(|a| a == n)
        });
    }
    let harness = Arc::new(Harness::new(tools, HookManager::new(external_hooks)));
    // 按实际模型上下文窗口动态设定压缩预算（架构 §4.1.3：占 context_window 70% 触发）。
    let mut definition = AgentDefinition::default();
    definition.compaction_policy.token_budget = (ctx_window as f64 * 0.75) as usize;

    // 优先级：args（前端 send_message 显式传） > session.enabled_tools（非空） > 全局 settings
    // 注意：旧版本把 session.enabled_tools = Some([]) 当作"明确不启用工具"——实践中
    // 这条路径只造成"全局设了工具但当前对话用不上"的 UX bug；当前语义已改为
    // Some([]) 也下沉到全局，保留"明确清空"能力到 SessionSettingsDialog 的「恢复继承」按钮。
    let session_tools = session.enabled_tools.clone().unwrap_or_default();
    let effective_enabled_tools = if !args.enabled_tools.is_empty() {
        args.enabled_tools.clone()
    } else if !session_tools.is_empty() {
        session_tools
    } else {
        settings.conversation.enabled_tools.clone()
    };
    tracing::debug!(
        session_id = %args.session_id,
        effective_enabled_tools = ?effective_enabled_tools,
        global_enabled_tools = ?settings.conversation.enabled_tools,
        session_enabled_tools = ?session.enabled_tools,
        args_enabled_tools = ?args.enabled_tools,
        "send_message: resolved enabled_tools",
    );

    // HEBBIAN_DUMP_MODEL_IO=1 时把每次模型 request/response 落到
    // <data_dir>/sessions/<session_id>.model_io.jsonl，方便桌面调试 prompt / token / tool schema。
    let model_io_dump =
        agent_core::model_io_dump::open_for_session_if_enabled(data_dir, &args.session_id).await;

    // PermissionStore session 视图的「幂等初始化」（架构 §4.6.2）：
    // 用 ensure_session_view 而非 load_session_rules——后者是 HashMap::insert，
    // 会把前几轮累积的 AllowAndRemember(Session) 规则覆盖成空 vec，导致同对话内
    // 同一审批反复弹（这是 2026-05 之前的真实 bug）。ensure_session_view 只在
    // 该 session 还没有视图时才初始化空 vec，已存在则保留。
    if let Some(store) = &args.permission_store {
        store.ensure_session_view(&args.session_id);
    }

    let used_global_rules = session
        .global_rules
        .clone()
        .unwrap_or_else(|| settings.conversation.global_rules.clone());
    let used_rules_files = session.rules_files.clone();

    let mut core_session = CoreSession::new(
        harness,
        SessionConfig {
            definition,
            workspace: workspace.clone(),
            client,
            enabled_tools: effective_enabled_tools,
            initial_transcript: Transcript::from_session(
                session.system_prompt.clone(),
                &initial_messages,
            ),
            recorder: None,
            model_io_dump,
            permission_store: args.permission_store.clone(),
            session_id: Some(args.session_id.clone()),
            run_mode: prior_session.run_mode,
            model_id: Some(model.clone()),
            force_automode: args.force_automode,
            data_dir: Some(data_dir.to_path_buf()),
            phase: Some(phase.clone()),
            global_rules: used_global_rules,
            rules_files: used_rules_files,
            edits_worktree: Some(edits_worktree),
            derived_sink,
            // surface 主对话：tag=Main（前端不额外标记，§4.11）。
            call_tag: model_gateway::types::ModelCallTag::Main,
        },
    );
    if is_system_notification {
        if let Some(pending) = args.pending_inputs.as_ref() {
            pending.lock().unwrap().push(PendingUserInput {
                content: args.user_content.clone(),
                attachments: args.attachments.clone(),
                meta: None,
            });
        }
    } else if !args.continue_run {
        core_session.append_user(args.user_content.clone(), args.attachments);
    }

    // 架构 §4.12.6：用户发新消息时若本 session 有挂起态 checkpoint，走 resume
    // 路径（载入 RunResumeState、清调度器登记、emit RunResumed{UserMessageArrived}）。
    // 否则起新 Run。
    let resume_state = agent_core::storage::run_checkpoint::load(data_dir, &args.session_id)
        .ok()
        .flatten()
        .map(|ckpt| {
            // checkpoint 已经被新消息接管，清掉文件 + 摘除调度器对该 run 的登记，
            // 防止 cron/bg-task 之后又触发一次重复 resume。
            let _ = agent_core::storage::run_checkpoint::delete(data_dir, &args.session_id);
            agent_core::wakeup::WakeupScheduler::global()
                .discard_run(&args.session_id, &ckpt.run_id);
            agent_core::agent_loop::RunResumeState::from_checkpoint(
                ckpt,
                protocol::ResumeCause::UserMessageArrived,
            )
        });

    let mut handle = match resume_state {
        Some(rs) => core_session.resume_with_runtime_inputs(
            args.cancel_flag.clone(),
            args.pending_inputs.clone(),
            args.consumed_pending_inputs.clone(),
            args.pending_inputs_accepting.clone(),
            Some(phase.clone()),
            rs,
        ),
        None => core_session.run_with_runtime_inputs(
            args.cancel_flag.clone(),
            args.pending_inputs.clone(),
            args.consumed_pending_inputs.clone(),
            args.pending_inputs_accepting.clone(),
        ),
    };
    let hitl = handle.hitl().clone();
    if let (Some(state), Some(request_id)) = (&args.hitl, args.request_id.as_ref()) {
        state.track_run(request_id.clone(), args.cancel_flag.clone(), hitl.clone());
    }

    let mut observer = DesktopObserver::new(
        args.hitl.clone(),
        hitl.clone(),
        &emit_event,
        data_dir,
        &args.session_id,
    );
    let summary = handle.drive(&mut observer).await;
    if let Some(request_id) = args.request_id.as_deref() {
        common::runtime::close_pending_inputs(request_id);
    }
    if let Some(state) = &args.hitl {
        state.forget(&hitl);
    }

    // token_stats 由 agent_loop 在每次模型请求完成时 per-turn 落盘
    // （sessions::bump_token_stats）：run 进行中前端就能实时刷新 cache 指示器，
    // 中断/失败也保住已完成请求的扣费。这里不再 run-end 累加——否则与 per-turn 重复计数。

    // 把 workspace 运行时状态（已宣告 + 仍 pending）写回 session.json。
    // - append_user 已经把上一轮的 pending drain 进 announced，下一轮也能恢复
    // - 本轮 AllowAndRemember 审批新增的目录此时仍在 pending，下一条 user message 会通知模型
    persist_workspace_runtime_dirs(data_dir, &args.session_id, &workspace);

    if let Some(pending) = args.pending_inputs.as_ref() {
        pending.lock().unwrap().clear();
    }

    // 架构 §4.9.5 / §7.8.3：assistant 段 + 插队 user + partial sidecar 落盘全部收归
    // agent_core 的 RunPersister 串行 append，assistant message 也只在那一处累积。
    // desktop 不再自行重建 message，返回值直接取 RunPersister 落盘的最后一段（透传）。
    match summary.outcome {
        // Done / Suspended：agent_core 已落盘，返回最后落盘段（前端不消费此值，只为
        // 保持 send_message Tauri 命令的 Message 返回签名；无内容则回退空消息）。
        TurnOutcome::Done | TurnOutcome::Suspended => {
            Ok(summary.last_message.unwrap_or_else(empty_assistant_message))
        }
        // Cancelled / Failed：agent_core 的 finish_interrupted 已补落尾段 + Interrupted
        // marker，desktop 不再落盘，直接返回错误。
        TurnOutcome::Cancelled => Err(AppError::msg("请求已中断")),
        TurnOutcome::Failed(error) => Err(AppError::msg(error)),
    }
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

/// Desktop 端 [`TurnObserver`] 实现：把每个事件翻译成 `protocol::WireEvent` 推送给
/// React 渲染，并把 HITL pending 注册到全局桥接。
///
/// assistant 累积 + 落盘已收归 agent_core 唯一一份（架构 §7.8.3）：observer 不再重建
/// message，`send_message` 的返回值由 [`crate::chat::send_and_save`] 从 `TurnSummary`
/// 透传 RunPersister 落盘的最后一段。partial sidecar 同样由 RunPersister 维护。
struct DesktopObserver<'a> {
    hitl_state: Option<Arc<HitlState>>,
    hitl: Arc<HitlGate>,
    data_dir: PathBuf,
    session_id: String,
    emit: &'a (dyn Fn(protocol::WireEvent) + Send + Sync),
}

impl<'a> DesktopObserver<'a> {
    fn new(
        hitl_state: Option<Arc<HitlState>>,
        hitl: Arc<HitlGate>,
        emit: &'a (dyn Fn(protocol::WireEvent) + Send + Sync),
        data_dir: &Path,
        session_id: &str,
    ) -> Self {
        Self {
            hitl_state,
            hitl,
            data_dir: data_dir.to_path_buf(),
            session_id: session_id.to_string(),
            emit,
        }
    }
}

#[async_trait]
impl<'a> TurnObserver for DesktopObserver<'a> {
    fn on_event(&mut self, event: &AgentEvent) {
        // 审批拒绝事件单独落 session.jsonl（独立审计副作用，非 assistant 累积）：
        // automode 自动拒 / 用户手动拒都要在历史里留痕，供加载时重建审批结果块。
        match &event.payload {
            EventPayload::PermissionAutoJudged { decision, .. } if decision == "deny" => {
                if let Err(err) = sessions::append_event(&self.data_dir, &self.session_id, event) {
                    tracing::warn!(%err, "failed to persist automode denial event");
                }
            }
            EventPayload::PermissionResolved { decision, .. }
                if matches!(
                    decision,
                    ApprovalDecision::Deny | ApprovalDecision::DenyWithFeedback { .. }
                ) =>
            {
                if let Err(err) = sessions::append_event(&self.data_dir, &self.session_id, event) {
                    tracing::warn!(%err, "failed to persist permission denial event");
                }
            }
            _ => {}
        }

        // 渲染：所有事件（含子 NestedRun 的 subagent_call_id 事件）统一翻译成 WireEvent
        // 推给前端做流式 / 嵌套渲染。assistant message 的累积与落盘由 agent_core 的
        // RunPersister 一手负责（架构 §7.8.3），observer 不再触碰。
        if let Some(ev) = protocol::to_wire(event) {
            (self.emit)(ev);
        }
    }

    async fn on_permission_request(
        &mut self,
        request_id: &PermissionRequestId,
        _kind: &PermissionKind,
        _summary: &str,
    ) -> Option<ApprovalDecision> {
        if let Some(state) = &self.hitl_state {
            state.track(request_id.0.clone(), Arc::clone(&self.hitl));
        }
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
        if let Some(state) = &self.hitl_state {
            state.track(request_id.0.clone(), Arc::clone(&self.hitl));
        }
        None
    }
}

/// 当前会话的上下文用量。前端用来渲染输入框旁的环形进度条。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextUsageDto {
    pub used_tokens: usize,
    pub budget_tokens: usize,
}

/// 把 workspace 运行时状态写回 session.json。
/// `runtime_announced` = 已通知模型的运行时追加目录；`runtime_pending` = 还没通知的，
/// 下次 send_message 时再次构造的 workspace 会带着它们继续 drain 注入。
/// 失败不传染：仅尽力持久化，session 文件不可读时直接放弃。
fn persist_workspace_runtime_dirs(
    data_dir: &Path,
    session_id: &str,
    workspace: &agent_core::workspace::Workspace,
) {
    let _ = sessions::update_meta(data_dir, session_id, |session| {
        session.runtime_allowed_paths = workspace.runtime_announced_snapshot();
        session.pending_runtime_allowed_paths = workspace.runtime_pending_snapshot();
        Ok(())
    });
}

/// 计算指定 session 的上下文用量。优先从 /v1/models 获取模型的 context_length，
/// 拉不到时回退到预设查表。与发起 run 时看到的口径一致。
pub async fn context_usage(data_dir: &Path, session_id: &str) -> AppResult<ContextUsageDto> {
    let session = sessions::load(data_dir, session_id)?;
    let transcript = Transcript::from_session(session.system_prompt.clone(), &session.messages);
    let (last_real, last_estimated) = session
        .token_stats
        .map(|s| (s.last_input_tokens, s.last_estimated_tokens))
        .unwrap_or((0, 0));
    let used = agent_core::context::budget::calibrated_transcript_tokens(
        transcript.system.as_deref(),
        &transcript.entries,
        last_real,
        last_estimated,
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

/// 主动压缩：调一次模型把当前 transcript 浓缩成摘要，然后在 session 里
/// 追加一条 [`Role::Marker`] + [`MessageMeta::CompactBoundary`] 标记，
/// 后续读取该 session 时 `Transcript::from_session` 会跳过标记之前的所有消息。
pub async fn compact_session(
    data_dir: &Path,
    session_id: &str,
    custom_instructions: Option<String>,
) -> AppResult<ContextUsageDto> {
    let session = sessions::load(data_dir, session_id)?;
    let provider = model_gateway::config::get(data_dir, &session.provider_id)?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| AppError::msg(format!("OAuth token 刷新失败: {e}")))?;
    let model = session.model.clone();
    let budget_tokens =
        model_gateway::context_window::resolve_context_window(&provider, &model).await;
    let inner = model_gateway::build_client(provider)
        .map_err(|e| AppError::msg(format!("无法创建 ModelClient: {e}")))?;
    let client: Arc<dyn ModelClient> = Arc::new(ModelWithName::new(inner, model));

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
        AppError::msg(format!("压缩失败: {e}"))
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
        run_duration_ms: None,
    };
    sessions::append_message(data_dir, session_id, marker)?;

    Ok(ContextUsageDto {
        used_tokens: result.after_tokens,
        budget_tokens,
    })
}

/// 内置浏览器「元素对话」旁支会话的一轮发送（架构 §8.5）。
///
/// 与主对话最大的不同：**旁支会话不落盘**。它是用户在页面预览上临时调样式的工作台，
/// 关掉浏览器就该消失，没有任何持久化价值，却会污染会话列表。所以这里全程纯内存：
/// - `session_id` / `data_dir` / `permission_store` 全 `None` → CoreSession 短路所有
///   落盘与后台 task（标题生成 / 记忆抽取 / partial sidecar 都 gate 在 `session_id.is_some()`）。
/// - 多轮历史由调用方（browser 模块）持有 `Vec<Message>`，每轮把 user + 重建的 assistant
///   追加进去，下一轮用 [`Transcript::from_session`] 重建——浏览器实例关闭即一并丢弃。
///
/// 但旁支的模型 IO **要落进绑定主对话的 model_io.jsonl**（`kind="aside"`），让用户在主对话的
/// Model I/O 调试面板里看到这些临时调用。这靠 [`agent_core::model_io_dump::open_for_session_with_kind`]
/// 打开指向主对话文件、主调用标 `aside` 的 dump 实现——旁支行不参与 `"main"` 增量去重，
/// 不污染主对话 transcript 的增量重建。
///
/// 工具只暴露 Preview 系信号工具（无副作用），故即便走到审批 gate 也直接放行。
///
/// 返回更新后的内存历史（含本轮 user + assistant）+ 本轮 assistant message，
/// 调用方持有历史续接下一轮。
#[allow(clippy::too_many_arguments)]
pub async fn send_aside(
    data_dir: &Path,
    bound_session_id: &str,
    provider_id: &str,
    model: &str,
    system_prompt: String,
    history: Vec<Message>,
    user_content: String,
    attachments: Vec<common::attachments::MessageAttachment>,
    cancel_flag: CancelFlag,
    preview_bridge: Option<std::sync::Arc<dyn agent_core::preview_bridge::PreviewBridge>>,
    emit_event: impl Fn(protocol::WireEvent) + Send + Sync,
) -> AppResult<(Vec<Message>, Message)> {
    // 极简 harness：三个信号工具（写路径，经 inspector）+ 两个观察工具
    // （读路径，经 PreviewBridge/CDP；bridge 不可用时工具自带降级提示）。
    let harness = Arc::new(Harness::new(
        vec![
            Box::new(agent_core::tools::preview_style::PreviewStyleTool),
            Box::new(agent_core::tools::preview_mutate::PreviewMutateTool),
            Box::new(agent_core::tools::preview_act::PreviewActTool),
            Box::new(agent_core::tools::preview_capture::PreviewCaptureTool::new(
                preview_bridge.clone(),
            )),
            Box::new(agent_core::tools::preview_inspect::PreviewInspectTool::new(
                preview_bridge,
            )),
        ],
        HookManager::new(vec![]),
    ));
    let workspace = Workspace::new(
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
        Vec::new(),
    );
    let enabled_tools = vec![
        "PreviewStyle".to_string(),
        "PreviewMutate".to_string(),
        "PreviewAct".to_string(),
        "PreviewCapture".to_string(),
        "PreviewInspect".to_string(),
    ];
    // 旁支引擎已下沉 agent_core（事件出口为 protocol::WireEvent，三 surface 复用同一份）。
    agent_core::aside::run_aside(agent_core::aside::RunAsideArgs {
        data_dir,
        bound_session_id,
        provider_id,
        model,
        system_prompt,
        history,
        user_content,
        attachments,
        harness,
        workspace,
        enabled_tools,
        cancel_flag,
        emit_event,
    })
    .await
    .map_err(AppError::msg)
}

pub async fn send_once(
    provider: &Provider,
    model: &str,
    system: Option<&str>,
    messages: &[Message],
) -> AppResult<String> {
    use model_gateway::types::{
        AssistantEntry, ModelRequest, ModelResponse, TranscriptEntry, UserEntry,
    };

    let client = model_gateway::build_client(provider.clone())
        .map_err(|e| AppError::msg(format!("无法创建 ModelClient: {e}")))?;
    let entries = messages
        .iter()
        .filter_map(|m| match m.role {
            Role::User => Some(TranscriptEntry::User(UserEntry {
                text: m.content.clone(),
                attachments: m.attachments.clone(),
            })),
            Role::Assistant => Some(TranscriptEntry::Assistant(AssistantEntry {
                text: m.content.clone(),
                reasoning: String::new(),
                reasoning_signature: String::new(),
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
            compact_prompt_cache_key: None,
        meta: Default::default(),
    };
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    match client
        .complete(req, cancel)
        .await
        .map_err(|e| AppError::msg(e.to_string()))?
    {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => Ok(text),
    }
}

use model_gateway::{
    client::{DynModelClient, ModelClient},
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent, ReasoningConfig},
};

struct ModelWithName {
    inner: DynModelClient,
    model: String,
    /// 由 session 注入的推理配置；无则保持上游默认。
    reasoning: Option<ReasoningConfig>,
}

impl ModelWithName {
    fn new(inner: DynModelClient, model: String) -> Self {
        Self {
            inner,
            model,
            reasoning: None,
        }
    }

    fn with_reasoning(
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

// ── hebisland 通知桥接 ──

/// 将 agent_core protocol::WireEvent 翻译为 hebisland 推送/撤销通知。
pub fn push_engine_event_to_island(client: &HebislandClient, event: &protocol::WireEvent) {
    match event {
        protocol::WireEvent::PermissionRequested {
            request_id,
            kind,
            tool_name,
            input,
            summary,
            paths,
            auto_handled,
            ..
        } => {
            // AutoMode judge 接管的审批（auto_handled）不打扰用户：island 与前端审批框
            // 同步压住，等 judge 出结果。若 judge 判「仍需人工」会再经 PermissionAutoJudged
            // （requires_human=true）补推 island 卡片。
            if *auto_handled {
                return;
            }
            let body = approval_card_body(kind, tool_name, input, summary, paths);
            client.show(IslandCard::new(
                format!("perm-{request_id}"),
                "approval",
                "需要你的审批",
                body,
            ));
        }
        protocol::WireEvent::PermissionAutoJudged {
            request_id,
            tool_name,
            reason,
            requires_human,
            ..
        } => {
            // judge 判 ASK / 普通 AutoMode 命令类 DENY：被接管时压住的审批显形，
            // island 此时才需要用户注意。reason 是判官给的人话解释。
            if *requires_human {
                let body = match reason.as_deref() {
                    Some(r) if !r.is_empty() => format!("{tool_name}：{r}"),
                    _ => tool_name.clone(),
                };
                client.show(IslandCard::new(
                    format!("perm-{request_id}"),
                    "approval",
                    "需要你的审批",
                    body,
                ));
            }
        }
        protocol::WireEvent::UserQuestionRequested {
            request_id,
            question,
            options,
            multi,
            questions,
        } => {
            let mut card = IslandCard::new(
                format!("question-{request_id}"),
                "question",
                "需要你的回答",
                "",
            );
            if questions.is_empty() {
                // 单题：顶层 question / options / multi。
                card.body = question.clone();
                card.options = options
                    .iter()
                    .map(|o| IslandOption {
                        label: o.label.clone(),
                        desc: o.description.clone(),
                    })
                    .collect();
                card.multi_select = *multi;
            } else {
                // 多题：逐题铺开，island 限高滚动。
                card.questions = questions
                    .iter()
                    .map(|q| IslandQuestion {
                        title: q.title.clone(),
                        desc: q.description.clone(),
                        options: q
                            .options
                            .iter()
                            .map(|o| IslandOption {
                                label: o.label.clone(),
                                desc: o.description.clone(),
                            })
                            .collect(),
                        multi: q.multi,
                    })
                    .collect();
            }
            client.show(card);
        }
        protocol::WireEvent::PermissionResolved { request_id, .. }
        | protocol::WireEvent::UserQuestionAnswered { request_id, .. } => {
            client.dismiss(&format!("perm-{request_id}"));
            client.dismiss(&format!("question-{request_id}"));
        }
        _ => {}
    }
}

/// 审批卡正文：抽工具的关键参数，而非整坨 input JSON。
///
/// 不同工具看不同字段——Bash 看命令、Read/Edit/Write 看文件、Grep 看 pattern+路径，
/// 越界访问（PathAccess，input 为 null）看越界路径列表。提不出关键字段时退回 summary。
fn approval_card_body(
    kind: &str,
    tool_name: &str,
    input: &serde_json::Value,
    summary: &str,
    paths: &[String],
) -> String {
    let str_field = |k: &str| input.get(k).and_then(|v| v.as_str());
    let key = match tool_name {
        "Bash" | "BashOutput" | "KillShell" => str_field("command"),
        "Read" | "Edit" | "Write" => str_field("file_path"),
        "Grep" => match (str_field("pattern"), str_field("path")) {
            (Some(p), Some(path)) => return format!("{tool_name} {p} @ {path}"),
            (Some(p), None) => Some(p),
            _ => None,
        },
        _ => None,
    };
    if let Some(detail) = key {
        return format!("{tool_name} {detail}");
    }
    if !paths.is_empty() {
        return format!("{tool_name} 越界访问：{}", paths.join("、"));
    }
    // 兜底：summary 是 agent_core 给的人话短句（如「工具 X 请求执行」）。
    if !summary.is_empty() {
        summary.to_string()
    } else {
        let _ = kind;
        tool_name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::storage::sessions::MessageMeta;
    use model_gateway::{
        config::{AuthMode, ProviderKind, ProvidersFile},
        types::{ToolCall, ToolCallStreamDelta, TranscriptEntry, Usage, UserEntry},
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    fn temp_data_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hebbian-interrupted-output-test-{}",
            sessions::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn save_test_provider(data_dir: &std::path::Path) {
        model_gateway::config::save(
            data_dir,
            &ProvidersFile {
                providers: vec![Provider {
                    id: "openai".to_string(),
                    name: "OpenAI".to_string(),
                    kind: ProviderKind::Openai,
                    enabled: true,
                    auth_mode: AuthMode::ApiKey,
                    base_url: "https://example.test/v1".to_string(),
                    api_key: "test".to_string(),
                    refresh_token: None,
                    token_expires_at: None,
                    account_id: None,
                    extra_headers: BTreeMap::new(),
                    models: vec!["gpt-test".to_string()],
                    fetched_models: None,
                    model_context_windows: BTreeMap::new(),
                    default_model: Some("gpt-test".to_string()),
                    title_gen_enabled: false,
                    title_gen_model: None,
                    judge_provider_id: None,
                    judge_model: None,
                    claude_code_compat: false,
            openai_codex_mode: false,
                }],
                default_provider_id: Some("openai".to_string()),
                vision_provider_id: None,
                vision_model: None,
            },
        )
        .unwrap();
    }

    struct ContinueRunProbeClient {
        seen_messages: Arc<Mutex<Vec<TranscriptEntry>>>,
    }

    impl ContinueRunProbeClient {
        fn respond(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
            *self.seen_messages.lock().unwrap() = req.entries;
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "continued".to_string(),
                reasoning: String::new(),
                reasoning_signature: String::new(),
                attachments: Vec::new(),
                usage: Usage::default(),
            })
        }
    }

    #[async_trait]
    impl ModelClient for ContinueRunProbeClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        async fn complete(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            self.respond(req)
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            _on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            self.respond(req)
        }
    }

    #[test]
    fn continue_run_injects_continue_user_message_when_transcript_ends_with_assistant() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();
            sessions::append_message(
                &data_dir,
                &session.id,
                Message {
                    id: sessions::new_id(),
                    role: Role::User,
                    content: "上一轮问题".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    meta: None,
                    subagent_call_id: None,
                    run_duration_ms: None,
                },
            )
            .unwrap();
            sessions::append_message(
                &data_dir,
                &session.id,
                Message {
                    id: sessions::new_id(),
                    role: Role::Assistant,
                    content: "上一轮回答到一半".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    meta: None,
                    subagent_call_id: None,
                    run_duration_ms: None,
                },
            )
            .unwrap();

            let seen_messages = Arc::new(Mutex::new(Vec::new()));
            let seen_for_client = seen_messages.clone();
            send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: true,
                    session_id: session.id.clone(),
                    user_content: String::new(),
                    user_meta: None,
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: Vec::new(),
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: None,
                    consumed_pending_inputs: None,
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                move |_provider, _model, _reasoning| {
                    Ok(Arc::new(ContinueRunProbeClient {
                        seen_messages: seen_for_client.clone(),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            let user_texts: Vec<String> = seen_messages
                .lock()
                .unwrap()
                .iter()
                .filter_map(|entry| match entry {
                    TranscriptEntry::User(UserEntry { text, .. }) => Some(text.clone()),
                    _ => None,
                })
                .collect();
            // transcript: [user "上一轮问题"] [assistant "上一轮回答到一半"]
            // from_session 看到末尾是 assistant，自动补一条 "继续" user → 共 2 条
            assert_eq!(user_texts.len(), 2, "user_texts={user_texts:?}");
            assert!(user_texts[0].contains("上一轮问题"));
            assert_eq!(user_texts[1], "继续", "第二条应是自动注入的「继续」");
            // 不该在 DB 写空 user message
            assert!(!user_texts.iter().any(|text| text.is_empty()));

            let saved = sessions::load(&data_dir, &session.id).unwrap();
            assert!(!saved
                .messages
                .iter()
                .any(|msg| msg.role == Role::User && msg.content.is_empty()));

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    struct OrderedPartsClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for OrderedPartsClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "先说".to_string(),
                    });
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 0,
                        id: Some("call_a".to_string()),
                        name: Some("missing_a".to_string()),
                        arguments_delta: Some("{\"a\":1}".to_string()),
                    }));
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 1,
                        id: Some("call_b".to_string()),
                        name: Some("missing_b".to_string()),
                        arguments_delta: Some("{\"b\":2}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![
                            ToolCall {
                                id: "call_a".to_string(),
                                name: "missing_a".to_string(),
                                input: serde_json::json!({"a": 1}),
                            },
                            ToolCall {
                                id: "call_b".to_string(),
                                name: "missing_b".to_string(),
                                input: serde_json::json!({"b": 2}),
                            },
                        ],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "后说".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "后说".to_string(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct AutoModeProbeClient {
        calls: AtomicUsize,
        saw_auto_judge: Arc<std::sync::atomic::AtomicBool>,
        seen_models: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelClient for AutoModeProbeClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            self.seen_models.lock().unwrap().push(req.model.clone());
            self.saw_auto_judge
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "ALLOW".to_string(),
                reasoning: String::new(),
                reasoning_signature: String::new(),
                attachments: Vec::new(),
                usage: Usage::default(),
            })
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            self.seen_models.lock().unwrap().push(req.model.clone());
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 0,
                        id: Some("call_bash".to_string()),
                        name: Some("Bash".to_string()),
                        arguments_delta: Some(
                            "{\"command\":\"chmod 755 automode-ok\"}".to_string(),
                        ),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_bash".to_string(),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "chmod 755 automode-ok"}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => Ok(ModelResponse::Done {
                    finish: model_gateway::types::FinishReason::Stop,
                    text: "done".to_string(),
                    reasoning: String::new(),
                    reasoning_signature: String::new(),
                    attachments: Vec::new(),
                    usage: Usage::default(),
                }),
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct RepeatedLocalIndexClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for RepeatedLocalIndexClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第一段".to_string(),
                    });
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 0,
                        id: Some("call_first".to_string()),
                        name: Some("missing_first".to_string()),
                        arguments_delta: Some("{\"first\":true}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_first".to_string(),
                            name: "missing_first".to_string(),
                            input: serde_json::json!({"first": true}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第二段".to_string(),
                    });
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 0,
                        id: Some("call_second".to_string()),
                        name: Some("missing_second".to_string()),
                        arguments_delta: Some("{\"second\":true}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_second".to_string(),
                            name: "missing_second".to_string(),
                            input: serde_json::json!({"second": true}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                2 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "结束".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "结束".to_string(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct PendingInputOrderClient {
        calls: AtomicUsize,
        pending_inputs: PendingInputs,
    }

    #[async_trait]
    impl ModelClient for PendingInputOrderClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "正在输出".to_string(),
                    });
                    self.pending_inputs
                        .lock()
                        .unwrap()
                        .push(common::runtime::PendingUserInput {
                            content: "插队消息".to_string(),
                            attachments: Vec::new(),
                            meta: None,
                        });
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_missing".to_string(),
                            name: "missing_tool".to_string(),
                            input: serde_json::json!({}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => {
                    let saw_injected = req.entries.iter().any(|entry| {
                        matches!(
                            entry,
                            model_gateway::types::TranscriptEntry::User(user)
                                if user.text == "插队消息"
                        )
                    });
                    assert!(
                        saw_injected,
                        "second model request should see injected user input"
                    );
                    on_event(ModelStreamEvent::TextDelta {
                        text: "后续回答".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "后续回答".to_string(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct PendingInputDuringDoneClient {
        calls: AtomicUsize,
        pending_inputs: PendingInputs,
    }

    #[async_trait]
    impl ModelClient for PendingInputDuringDoneClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第一段".to_string(),
                    });
                    self.pending_inputs
                        .lock()
                        .unwrap()
                        .push(common::runtime::PendingUserInput {
                            content: "插队消息".to_string(),
                            attachments: Vec::new(),
                            meta: None,
                        });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "第一段".to_string(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => {
                    let saw_injected = req.entries.iter().any(|entry| {
                        matches!(
                            entry,
                            model_gateway::types::TranscriptEntry::User(user)
                                if user.text == "插队消息"
                        )
                    });
                    assert!(
                        saw_injected,
                        "second model request should see injected user input"
                    );
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第二段".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "第二段".to_string(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct PendingInputThenToolLoopClient {
        calls: AtomicUsize,
        pending_inputs: PendingInputs,
    }

    #[async_trait]
    impl ModelClient for PendingInputThenToolLoopClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "通知前".to_string(),
                    });
                    self.pending_inputs
                        .lock()
                        .unwrap()
                        .push(common::runtime::PendingUserInput {
                            content: "后台通知".to_string(),
                            attachments: Vec::new(),
                            meta: None,
                        });
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_before".to_string(),
                            name: "missing_before".to_string(),
                            input: serde_json::json!({}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => {
                    let saw_injected = req.entries.iter().any(|entry| {
                        matches!(
                            entry,
                            model_gateway::types::TranscriptEntry::User(user)
                                if user.text == "后台通知"
                        )
                    });
                    assert!(
                        saw_injected,
                        "notification input must reach the next request"
                    );
                    on_event(ModelStreamEvent::TextDelta {
                        text: "通知后工具".to_string(),
                    });
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_after".to_string(),
                            name: "missing_after".to_string(),
                            input: serde_json::json!({}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                2 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "通知后结束".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "通知后结束".to_string(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct IdleWakeupClient;

    #[async_trait]
    impl ModelClient for IdleWakeupClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            let saw_wakeup = req.entries.iter().any(|entry| {
                matches!(
                    entry,
                    model_gateway::types::TranscriptEntry::User(user)
                        if user.text.contains("<wakeup kind=\"bg_task_finished\"")
                )
            });
            assert!(
                saw_wakeup,
                "idle wakeup send path must include the notification in the model request"
            );
            on_event(ModelStreamEvent::TextDelta {
                text: "收到后台完成通知".to_string(),
            });
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "收到后台完成通知".to_string(),
                reasoning: String::new(),
                reasoning_signature: String::new(),
                attachments: Vec::new(),
                usage: Usage::default(),
            })
        }
    }

    #[test]
    fn saves_assistant_parts_in_stream_arrival_order() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id,
                    user_content: "run tools".to_string(),
                    user_meta: None,
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: vec!["missing_a".to_string(), "missing_b".to_string()],
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: None,
                    consumed_pending_inputs: None,
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                |_provider, _model, _reasoning| {
                    Ok(Arc::new(OrderedPartsClient {
                        calls: AtomicUsize::new(0),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "先说后说");
            assert_eq!(assistant.parts.len(), 4);
            assert!(matches!(
                &assistant.parts[0],
                agent_core::storage::sessions::MessagePart::Text { text } if text == "先说"
            ));
            assert!(matches!(
                &assistant.parts[1],
                agent_core::storage::sessions::MessagePart::ToolCall { name, .. } if name == "missing_a"
            ));
            assert!(matches!(
                &assistant.parts[2],
                agent_core::storage::sessions::MessagePart::ToolCall { name, .. } if name == "missing_b"
            ));
            assert!(matches!(
                &assistant.parts[3],
                agent_core::storage::sessions::MessagePart::Text { text } if text == "后说"
            ));

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    /// 跑一个 desktop 主对话场景，返回它经 `DesktopObserver` → `protocol::to_wire` emit
    /// 出去的**全部事件**序列化成的指纹（每行一个事件 JSON）。供改造前后逐字节对照。
    async fn run_scenario_fingerprint(
        label: &str,
        enabled_tools: Vec<String>,
        client_factory: impl Fn(Provider, String, Option<common::ReasoningConfig>) -> AppResult<Arc<dyn ModelClient>>
            + Send
            + Sync
            + 'static,
    ) -> String {
        let data_dir = temp_data_dir();
        save_test_provider(&data_dir);
        let session = sessions::create(
            &data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .unwrap();

        let events = Arc::new(Mutex::new(Vec::<protocol::WireEvent>::new()));
        let events_for_emit = events.clone();

        send_and_save_in_data_dir_with_client_factory(
            &data_dir,
            SendArgs {
                continue_run: false,
                session_id: session.id,
                user_content: "run tools".to_string(),
                user_meta: None,
                attachments: Vec::new(),
                stream: true,
                enabled_tools,
                cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                pending_inputs: None,
                consumed_pending_inputs: None,
                pending_inputs_accepting: None,
                hitl: None,
                permission_store: None,
                force_automode: false,
                request_id: None,
                restrict_tools: None,
            },
            move |event| {
                events_for_emit.lock().unwrap().push(event);
            },
            None,
            client_factory,
        )
        .await
        .unwrap();

        let captured = events.lock().unwrap();
        assert!(!captured.is_empty(), "场景 {label} 必须 emit 事件");
        let mut lines: Vec<String> = vec![format!("### SCENARIO: {label}")];
        for ev in captured.iter() {
            lines.push(serde_json::to_string(ev).unwrap());
        }
        std::fs::remove_dir_all(&data_dir).unwrap();
        lines.join("\n")
    }

    /// 步骤4 收口的 desktop 本体回归证据：跑**多个真实主对话场景**（纯文本+工具、
    /// Anthropic 块索引流式、重复本地工具索引），收集 `DesktopObserver` 经
    /// `protocol::to_wire` emit 给前端的全部事件，拼成一份多场景指纹打印 + 落盘。
    /// 改造前（emit 旧 `EngineEvent`）与改造后（emit `WireEvent`）各跑一次，归一化真实
    /// 耗时后两份指纹必须逐字节相同——直接证明 desktop 发给前端的事件流零漂移。
    /// 用 `cargo test ... -- --nocapture` 看指纹，落盘在 /tmp/desktop_event_fingerprint.txt。
    #[test]
    fn desktop_main_chat_event_stream_fingerprint() {
        tauri::async_runtime::block_on(async {
            let mut all = String::new();
            // 场景 1：文字 + 两个工具调用 + 多轮（OrderedPartsClient）
            all.push_str(
                &run_scenario_fingerprint(
                    "text+tools+multiround",
                    vec!["missing_a".to_string(), "missing_b".to_string()],
                    |_p, _m, _r| {
                        Ok(Arc::new(OrderedPartsClient {
                            calls: AtomicUsize::new(0),
                        }) as Arc<dyn ModelClient>)
                    },
                )
                .await,
            );
            all.push_str("\n");
            // 场景 2：Anthropic content block index 流式（AnthropicBlockIndexClient）
            all.push_str(
                &run_scenario_fingerprint(
                    "anthropic_block_index",
                    vec!["tool_a".to_string(), "tool_b".to_string()],
                    |_p, _m, _r| {
                        Ok(Arc::new(AnthropicBlockIndexClient {
                            calls: AtomicUsize::new(0),
                        }) as Arc<dyn ModelClient>)
                    },
                )
                .await,
            );
            all.push_str("\n");
            // 场景 3：重复本地工具索引（RepeatedLocalIndexClient）
            all.push_str(
                &run_scenario_fingerprint(
                    "repeated_local_index",
                    vec!["missing_first".to_string(), "missing_second".to_string()],
                    |_p, _m, _r| {
                        Ok(Arc::new(RepeatedLocalIndexClient {
                            calls: AtomicUsize::new(0),
                        }) as Arc<dyn ModelClient>)
                    },
                )
                .await,
            );

            println!("\n===DESKTOP_EVENT_FINGERPRINT_BEGIN===\n{all}\n===DESKTOP_EVENT_FINGERPRINT_END===");
            let _ = std::fs::write("/tmp/desktop_event_fingerprint.txt", &all);

            // 自身契约：三场景都得发出关键事件，防止 emit 整条断了还以为"指纹一致"。
            assert!(all.contains("\"type\":\"text_delta\""), "应含 text_delta");
            assert!(all.contains("\"type\":\"tool_start\""), "应含 tool_start");
            assert!(
                all.contains("\"type\":\"run_finished\""),
                "应含 run_finished"
            );
            assert_eq!(all.matches("### SCENARIO").count(), 3, "应覆盖 3 个场景");
        });
    }

    #[test]
    fn saves_repeated_local_tool_indexes_as_distinct_dispatches() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id,
                    user_content: "run tools".to_string(),
                    user_meta: None,
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: vec!["missing_first".to_string(), "missing_second".to_string()],
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: None,
                    consumed_pending_inputs: None,
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                |_provider, _model, _reasoning| {
                    Ok(Arc::new(RepeatedLocalIndexClient {
                        calls: AtomicUsize::new(0),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "第一段第二段结束");
            let tool_names: Vec<&str> = assistant
                .parts
                .iter()
                .filter_map(|part| match part {
                    agent_core::storage::sessions::MessagePart::ToolCall { name, .. } => {
                        Some(name.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(tool_names, vec!["missing_first", "missing_second"]);

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    /// 复现：Anthropic 透传 content block index（含 thinking/text 块）做 ToolCallDelta.index，
    /// 与上层「每 turn +1」的 dispatch_offset 体系语义不一致 → 跨 turn 撞 index →
    /// AssistantPartsRecorder.by_index 命中旧 part，新 tool_call 不新建 part 被丢，
    /// 紧跟的 text 黏进上一段。模型每 turn 一个工具，但 block index 随是否含正文在 1/0 间浮动。
    struct AnthropicBlockIndexClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for AnthropicBlockIndexClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            // 每个 turn：先 emit 正文 TextDelta，再 emit 一个 tool_use。
            // block_index 模拟 Anthropic：响应是 [text(0), tool_use(1)] → tool 在 block 1。
            // dispatch_offset 第 0 turn=0、第 1 turn=1。两者相加：1、2，本不撞；
            // 但若某 turn 无正文（[tool_use(0)]），block_index 回 0，offset+0 就会和
            // 前一个含正文 turn 的 offset+? 撞上。这里第 2 turn 用 [tool_use(0)] 制造碰撞。
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            match n {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第一段".to_string(),
                    });
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 1,
                        id: Some("call_a".to_string()),
                        name: Some("tool_a".to_string()),
                        arguments_delta: Some("{}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_a".to_string(),
                            name: "tool_a".to_string(),
                            input: serde_json::json!({}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第二段".to_string(),
                    });
                    // 本 turn 无 thinking/text 在 tool 之前的偏移差异：block_index=0。
                    // offset 此时为 1，1+0=1 → 与上一个 turn (offset 0 + block 1 = 1) 撞！
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 0,
                        id: Some("call_b".to_string()),
                        name: Some("tool_b".to_string()),
                        arguments_delta: Some("{}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_b".to_string(),
                            name: "tool_b".to_string(),
                            input: serde_json::json!({}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                2 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第三段结束".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "第三段结束".to_string(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    #[test]
    fn anthropic_block_index_does_not_drop_tool_parts() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id,
                    user_content: "run tools".to_string(),
                    user_meta: None,
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: vec!["tool_a".to_string(), "tool_b".to_string()],
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: None,
                    consumed_pending_inputs: None,
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                |_provider, _model, _reasoning| {
                    Ok(Arc::new(AnthropicBlockIndexClient {
                        calls: AtomicUsize::new(0),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            // 两个 tool_call 必须都落进 parts，且按时间序交错：
            // text(第一段) → tool_a → text(第二段) → tool_b → text(第三段结束)
            let tool_names: Vec<&str> = assistant
                .parts
                .iter()
                .filter_map(|part| match part {
                    agent_core::storage::sessions::MessagePart::ToolCall { name, .. } => {
                        Some(name.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                tool_names,
                vec!["tool_a", "tool_b"],
                "block index 碰撞不应丢 tool_call part"
            );

            // 两段正文不能被黏成一段：parts 里应有独立的「第二段」text part，
            // 且它出现在 tool_a 之后、tool_b 之前。
            let texts: Vec<&str> = assistant
                .parts
                .iter()
                .filter_map(|part| match part {
                    agent_core::storage::sessions::MessagePart::Text { text } => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert!(
                texts
                    .iter()
                    .any(|t| t.contains("第一段") && !t.contains("第二段")),
                "第一段不应吞并第二段，parts.texts={texts:?}"
            );

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    /// 回归（架构 §4.9.5，2026-06-25）：无插队的多 ToolStep run 在 session.jsonl 里
    /// 只落**一条** assistant message，而不是每个「模型请求 + 工具批次」一条。
    ///
    /// B 阶段把落盘收归 agent_core 后，若每个 ModelStep/ToolStep 都无条件 flush_segment，
    /// 一个正常 run 会被拆成多张 assistant 卡片（run 进行中前端显示一整块、reload 后裂开）。
    /// 修复：flush_segment 仅在有 pending 插队时调用，无插队时全 run 累积到 finish 一次落。
    ///
    /// A/B 翻转：flush 带 `has_pending` 守卫时本测试 pass；去掉守卫（每步都 flush）必 fail
    /// （会落 3 条 assistant：tool_a 一条、tool_b 一条、第三段一条）。
    #[test]
    fn no_pending_multi_tool_run_persists_single_assistant() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();

            send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id.clone(),
                    user_content: "run tools".to_string(),
                    user_meta: None,
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: vec!["tool_a".to_string(), "tool_b".to_string()],
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: None,
                    consumed_pending_inputs: None,
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                |_provider, _model, _reasoning| {
                    Ok(Arc::new(AnthropicBlockIndexClient {
                        calls: AtomicUsize::new(0),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            let saved = sessions::load(&data_dir, &session.id).unwrap();
            let assistant_count = saved
                .messages
                .iter()
                .filter(|m| m.role == Role::Assistant)
                .count();
            assert_eq!(
                assistant_count,
                1,
                "无插队的多 ToolStep run 应只落一条 assistant，实际落了 {assistant_count} 条：{:?}",
                saved
                    .messages
                    .iter()
                    .map(|m| (m.role, m.content.chars().take(12).collect::<String>()))
                    .collect::<Vec<_>>()
            );

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    #[test]
    fn desktop_send_passes_session_model_to_automode_judge() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let workdir = data_dir.join("workspace");
            std::fs::create_dir_all(&workdir).unwrap();
            let mut session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "claude-opus-4.7".to_string(),
                None,
                None,
            )
            .unwrap();
            session.workdir = Some(workdir);
            session.run_mode = agent_core::run_mode::RunMode::AutoMode;
            sessions::save(&data_dir, session.clone()).unwrap();

            let saw_auto_judge = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let seen_models = Arc::new(Mutex::new(Vec::new()));
            let events = Arc::new(Mutex::new(Vec::<protocol::WireEvent>::new()));
            let saw_auto_judge_for_factory = saw_auto_judge.clone();
            let seen_models_for_factory = seen_models.clone();
            let events_for_emit = events.clone();

            let result = tokio::time::timeout(
                Duration::from_secs(8),
                send_and_save_in_data_dir_with_client_factory(
                    &data_dir,
                    SendArgs {
                        continue_run: false,
                        session_id: session.id.clone(),
                        user_content: "run command".to_string(),
                        user_meta: None,
                        attachments: Vec::new(),
                        stream: true,
                        enabled_tools: vec!["Bash".to_string()],
                        cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        pending_inputs: None,
                        consumed_pending_inputs: None,
                        pending_inputs_accepting: None,
                        hitl: None,
                        permission_store: None,
                        force_automode: true,
                        request_id: None,
                        restrict_tools: None,
                    },
                    move |event| {
                        events_for_emit.lock().unwrap().push(event);
                    },
                    None,
                    move |_provider, _model, _reasoning| {
                        Ok(Arc::new(AutoModeProbeClient {
                            calls: AtomicUsize::new(0),
                            saw_auto_judge: saw_auto_judge_for_factory.clone(),
                            seen_models: seen_models_for_factory.clone(),
                        }) as Arc<dyn ModelClient>)
                    },
                ),
            )
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "AutoMode judge should resolve Bash approval without hanging; events={:?}",
                    events.lock().unwrap()
                )
            });

            result.unwrap();
            assert!(
                saw_auto_judge.load(std::sync::atomic::Ordering::SeqCst),
                "AutoMode judge should be called for the session model"
            );
            assert!(
                seen_models
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|model| model == "claude-opus-4.7"),
                "model requests should carry the selected session model"
            );

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    /// 架构 §4.12.5 修订：streaming 中 push 进 PendingInputs 的消息**不再**由 run 结束
    /// 时统一落盘——落盘必须走 inject_user_message 即写即落。这里的 test 模拟"老路径
    /// 仅 push pending（没调 inject 落盘）"场景，验证：
    /// - assistant 段仍按正确顺序落盘（model 在 in-memory transcript 看到了插队，输出了"后续回答"）
    /// - jsonl 里**不**出现没经 inject 落盘的插队 user 条目（避免行为漂移）
    #[test]
    fn pending_inputs_not_double_written_on_run_end() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();
            let pending_inputs: PendingInputs = Arc::new(std::sync::Mutex::new(Vec::new()));
            let consumed_pending_inputs: ConsumedPendingInputs =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            let pending_for_client = pending_inputs.clone();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id.clone(),
                    user_content: "第一条".to_string(),
                    user_meta: None,
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: vec!["missing_tool".to_string()],
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: Some(pending_inputs),
                    consumed_pending_inputs: Some(consumed_pending_inputs),
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                move |_provider, _model, _reasoning| {
                    Ok(Arc::new(PendingInputOrderClient {
                        calls: AtomicUsize::new(0),
                        pending_inputs: pending_for_client.clone(),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            // 架构 §4.9.5：agent_core persister 在 drain 边界落插队 user，所以 jsonl
            // 里有插队条目；desktop 返回值含全部累积内容（用于前端乐观渲染）。
            assert_eq!(assistant.content, "正在输出后续回答");
            let saved = sessions::load(&data_dir, &session.id).unwrap();
            let roles_and_content: Vec<(Role, String)> = saved
                .messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect();
            assert_eq!(
                roles_and_content,
                vec![
                    (Role::User, "第一条".to_string()),
                    (Role::Assistant, "正在输出".to_string()),
                    (Role::User, "插队消息".to_string()),
                    (Role::Assistant, "后续回答".to_string()),
                ]
            );

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    /// 架构 §4.12.5 修订：同 [`pending_inputs_not_double_written_on_run_end`]——
    /// 跨 turn 的 PendingInput drain 路径，validate run 结束不再二次落盘 pending。
    /// （inject_user_message 即写即落，由专门测试覆盖；这里仅证旧路径不再 double-write）
    #[test]
    fn pending_inputs_between_assistant_turns_not_double_written() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();
            let pending_inputs: PendingInputs = Arc::new(std::sync::Mutex::new(Vec::new()));
            let consumed_pending_inputs: ConsumedPendingInputs =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            let pending_for_client = pending_inputs.clone();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id.clone(),
                    user_content: "第一条".to_string(),
                    user_meta: None,
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: Vec::new(),
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: Some(pending_inputs),
                    consumed_pending_inputs: Some(consumed_pending_inputs),
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                move |_provider, _model, _reasoning| {
                    Ok(Arc::new(PendingInputDuringDoneClient {
                        calls: AtomicUsize::new(0),
                        pending_inputs: pending_for_client.clone(),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            // 架构 §4.9.5：agent_core persister 在 drain 边界落插队 user + assistant 段。
            // desktop 返回值含全部累积内容（"第一段"+"第二段"），jsonl 含插队条目。
            assert_eq!(assistant.content, "第一段第二段");
            let saved = sessions::load(&data_dir, &session.id).unwrap();
            let roles_and_content: Vec<(Role, String)> = saved
                .messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect();
            assert_eq!(
                roles_and_content,
                vec![
                    (Role::User, "第一条".to_string()),
                    (Role::Assistant, "第一段".to_string()),
                    (Role::User, "插队消息".to_string()),
                    (Role::Assistant, "第二段".to_string()),
                ]
            );

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    #[test]
    fn pending_input_does_not_split_every_followup_model_request() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();
            let pending_inputs: PendingInputs = Arc::new(std::sync::Mutex::new(Vec::new()));
            let consumed_pending_inputs: ConsumedPendingInputs =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            let pending_for_client = pending_inputs.clone();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id.clone(),
                    user_content: "第一条".to_string(),
                    user_meta: None,
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: vec!["missing_before".to_string(), "missing_after".to_string()],
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: Some(pending_inputs),
                    consumed_pending_inputs: Some(consumed_pending_inputs),
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                move |_provider, _model, _reasoning| {
                    Ok(Arc::new(PendingInputThenToolLoopClient {
                        calls: AtomicUsize::new(0),
                        pending_inputs: pending_for_client.clone(),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            // 架构 §4.9.5：返回值含全部累积内容（三段拼接），jsonl 由 agent_core persister
            // 按段边界落盘：每次 ToolStep/Done flush 一段，drain 边界落插队 user。
            assert_eq!(assistant.content, "通知前通知后工具通知后结束");
            let saved = sessions::load(&data_dir, &session.id).unwrap();
            let roles_and_content: Vec<(Role, String)> = saved
                .messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect();
            assert_eq!(
                roles_and_content,
                vec![
                    (Role::User, "第一条".to_string()),
                    (Role::Assistant, "通知前".to_string()),
                    (Role::User, "后台通知".to_string()),
                    (Role::Assistant, "通知后工具通知后结束".to_string()),
                ]
            );

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    #[test]
    fn idle_system_notification_starts_model_request_with_wakeup() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();
            sessions::append_message(
                &data_dir,
                &session.id,
                Message {
                    id: sessions::new_id(),
                    role: Role::User,
                    content: "启动后台任务".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    meta: None,
                    subagent_call_id: None,
                    run_duration_ms: None,
                },
            )
            .unwrap();
            sessions::append_message(
                &data_dir,
                &session.id,
                Message {
                    id: sessions::new_id(),
                    role: Role::Assistant,
                    content: "已启动后台任务：`bash_001`。".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    meta: None,
                    subagent_call_id: None,
                    run_duration_ms: None,
                },
            )
            .unwrap();

            let wakeup_xml = "[SYSTEM NOTIFICATION - NOT USER INPUT]\nThis is an automated background-task event, NOT a message from the user.\n\n<wakeup kind=\"bg_task_finished\" task_id=\"bash_001\" tool_use_id=\"call_bg\" exit_code=\"0\" duration_ms=\"5281\">\n后台任务已完成。\n</wakeup>";
            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id.clone(),
                    user_content: wakeup_xml.to_string(),
                    user_meta: Some(MessageMeta::SystemNotification {
                        kind: "bg_task_finished".to_string(),
                        task_id: Some("bash_001".to_string()),
                        tool_use_id: Some("call_bg".to_string()),
                    }),
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: Vec::new(),
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: Some(Arc::new(std::sync::Mutex::new(Vec::new()))),
                    consumed_pending_inputs: Some(Arc::new(std::sync::Mutex::new(Vec::new()))),
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                |_provider, _model, _reasoning| {
                    Ok(Arc::new(IdleWakeupClient) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "收到后台完成通知");
            let saved = sessions::load(&data_dir, &session.id).unwrap();
            let last = saved.messages.last().expect("assistant reply persisted");
            assert_eq!(last.role, Role::Assistant);
            assert_eq!(last.content, "收到后台完成通知");

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    #[test]
    fn pre_persisted_system_notification_is_not_appended_twice_on_resume_run() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();
            let wakeup_xml = "[SYSTEM NOTIFICATION - NOT USER INPUT]\nThis is an automated background-task event, NOT a message from the user.\n\n<wakeup kind=\"bg_task_finished\" task_id=\"bash_001\" tool_use_id=\"call_bg\" exit_code=\"0\" duration_ms=\"5281\">\n后台任务已完成。\n</wakeup>";
            let wakeup_meta = MessageMeta::SystemNotification {
                kind: "bg_task_finished".to_string(),
                task_id: Some("bash_001".to_string()),
                tool_use_id: Some("call_bg".to_string()),
            };
            sessions::append_message(
                &data_dir,
                &session.id,
                Message {
                    id: sessions::new_id(),
                    role: Role::User,
                    content: "启动后台任务".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    meta: None,
                    subagent_call_id: None,
                    run_duration_ms: None,
                },
            )
            .unwrap();
            sessions::append_message(
                &data_dir,
                &session.id,
                Message {
                    id: sessions::new_id(),
                    role: Role::User,
                    content: wakeup_xml.to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    meta: Some(wakeup_meta.clone()),
                    subagent_call_id: None,
                    run_duration_ms: None,
                },
            )
            .unwrap();
            sessions::append_message(
                &data_dir,
                &session.id,
                Message {
                    id: sessions::new_id(),
                    role: Role::Assistant,
                    content: "已启动后台任务：`bash_001`。".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    meta: None,
                    subagent_call_id: None,
                    run_duration_ms: None,
                },
            )
            .unwrap();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    continue_run: false,
                    session_id: session.id.clone(),
                    user_content: wakeup_xml.to_string(),
                    user_meta: Some(wakeup_meta),
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: Vec::new(),
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    pending_inputs: Some(Arc::new(std::sync::Mutex::new(Vec::new()))),
                    consumed_pending_inputs: Some(Arc::new(std::sync::Mutex::new(Vec::new()))),
                    pending_inputs_accepting: None,
                    hitl: None,
                    permission_store: None,
                    force_automode: false,
                    request_id: None,
                    restrict_tools: None,
                },
                |_| {},
                None,
                |_provider, _model, _reasoning| {
                    Ok(Arc::new(IdleWakeupClient) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "收到后台完成通知");
            let saved = sessions::load(&data_dir, &session.id).unwrap();
            let notification_count = saved
                .messages
                .iter()
                .filter(|m| {
                    m.role == Role::User && m.content == wakeup_xml && m.is_system_notification()
                })
                .count();
            assert_eq!(notification_count, 1);

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }
}
