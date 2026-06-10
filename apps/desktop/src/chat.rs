use crate::engine::EngineEvent;
use crate::error::{AppError, AppResult};
use crate::hebisland_client::HebislandClient;
use crate::hitl::HitlState;
use agent_core::storage::{
    sessions::{
        self, Message, MessageMeta, MessagePart, MessageToolCall, Role, Session,
    },
    sessions_dir::{self as sessions_dir, PartialFragment},
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
    types::{AgentEvent, AgentEventPayload},
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
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::Manager;
use tauri::{ipc::Channel, AppHandle};

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
}

fn data_dir(_app: &AppHandle) -> AppResult<std::path::PathBuf> {
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
    on_event: Channel<EngineEvent>,
) -> AppResult<Message> {
    let dd = data_dir(app)?;
    let app_for_island = app.clone();
    let result = send_and_save_in_data_dir(&dd, args, move |event| {
        if let Some(client) = app_for_island.try_state::<HebislandClient>() {
            push_engine_event_to_island(&client, &event);
        }
        let _ = on_event.send(event);
    })
    .await;
    // 整轮 run 真正结束才弹一次「回答完成」（多回合只弹一次；取消 / 失败不弹）。
    if result.is_ok() {
        if let Some(client) = app.try_state::<HebislandClient>() {
            client.push(
                format!("done-{}", chrono::Utc::now().timestamp_millis()),
                "info",
                "回答完成",
                "Agent 已完成本次回答",
                None,
                None,
            );
        }
    }
    result
}

pub async fn send_and_save_in_data_dir(
    data_dir: &Path,
    args: SendArgs,
    emit_event: impl Fn(EngineEvent) + Send + Sync + 'static,
) -> AppResult<Message> {
    // 预构建 vision client（async：需要刷新 OAuth token）。
    // 未配置 vision provider 时为 None，闭包里跳过包装。
    let vision_client = agent_core::vision_bridge::build_vision_client(data_dir)
        .await
        .map_err(|e| AppError::msg(format!("vision bridge: {e}")))?;
    send_and_save_in_data_dir_with_client_factory(
        data_dir,
        args,
        emit_event,
        move |provider, model, reasoning| {
            let client = model_gateway::build_client(provider)
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
    emit_event: impl Fn(EngineEvent) + Send + Sync + 'static,
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
    let harness = Arc::new(Harness::new(
        agent_core::tools::default_tools_with_mcp(
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
        .await,
        HookManager::new(external_hooks),
    ));
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
        },
    );
    if is_system_notification {
        if let Some(pending) = args.pending_inputs.as_ref() {
            pending.lock().unwrap().push(PendingUserInput {
                content: args.user_content.clone(),
                attachments: args.attachments.clone(),
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

    let consumed_pending_seen_before_run = args
        .consumed_pending_inputs
        .as_ref()
        .map(|slot| slot.lock().unwrap().len())
        .unwrap_or(0);

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
        args.consumed_pending_inputs.clone(),
        consumed_pending_seen_before_run,
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

    observer.finish_current_segment();
    let DesktopObserver {
        parts,
        partial_output,
        tool_calls,
        mut segment_messages,
        output_attachments,
        partial_writer,
        ..
    } = observer;
    // pending / wakeup 插队消息已在 inject_user_message 即时落盘（架构 §4.12.5 修订），
    // run 结束这里**不再二次落盘**，避免 jsonl 出现重复条目。
    // 但仍需读 consumed_pending_inputs 判定本次 run 内是否真发生过插队 drain——
    // 发生过：assistant 已按 pending 分界切成多段（segment_messages 非空且各段独立），落盘要分段写；
    // 未发生：用全 run 的 parts 拼成单段 assistant（保持老行为，避免多 turn 但无插队
    // 时被无谓拆成多段卡片）。
    let had_pending_during_run = args
        .consumed_pending_inputs
        .as_ref()
        .map(|slot| slot.lock().unwrap().len() > consumed_pending_seen_before_run)
        .unwrap_or(false);
    if let Some(pending) = args.pending_inputs.as_ref() {
        pending.lock().unwrap().clear();
    }

    match summary.outcome {
        // 架构 §4.12.1：Suspended 是 Run 的合法中间态，下面跟 Done 走同一段
        // assistant 落盘逻辑——transcript 不进 checkpoint（§4.12.3），resume 时
        // agent_loop 从 session.jsonl 重建，所以本轮模型已经说过的话必须落盘。
        TurnOutcome::Done | TurnOutcome::Suspended => {}
        TurnOutcome::Cancelled => {
            persist_interrupted_assistant_output(
                data_dir,
                &args.session_id,
                &partial_output,
                &parts.parts,
                &tool_calls,
            )?;
            if let Some(pw) = partial_writer {
                pw.delete();
            }
            return Err(AppError::msg("请求已中断"));
        }
        TurnOutcome::Failed(error) => {
            persist_failed_assistant_output(
                data_dir,
                &args.session_id,
                &partial_output,
                &parts.parts,
                &tool_calls,
                &error,
            )?;
            if let Some(pw) = partial_writer {
                pw.delete();
            }
            return Err(AppError::msg(error));
        }
    }

    // Done：写 assistant 段。
    // - had_pending_during_run=true：run 内发生过 PendingInputs drain，assistant 被切成多段
    //   （segment_messages 含各段）→ 分段写。各段之间逻辑上夹着的插队 user message 已经在
    //   inject_user_message 时即写即落，物理 jsonl 已经有了，这里仅追加 assistant 段。
    // - had_pending_during_run=false：用全 run 累积的 parts 拼成单段 assistant 落盘，
    //   保持原有"一次 run = 一条 assistant message"语义（多 turn 但无插队的常态）。
    let assistant_msg = if had_pending_during_run {
        if segment_messages.is_empty() {
            segment_messages.push(assistant_message_from_recorded_parts(
                parts,
                partial_output,
                tool_calls,
                Vec::new(),
            ));
        }
        if let Some(last) = segment_messages.last_mut() {
            last.attachments = output_attachments;
        }
        for assistant in &segment_messages {
            sessions::append_message(data_dir, &args.session_id, assistant.clone())?;
        }
        segment_messages
            .last()
            .cloned()
            .unwrap_or_else(empty_assistant_message)
    } else {
        let m = assistant_message_from_recorded_parts(
            parts,
            partial_output,
            tool_calls,
            output_attachments,
        );
        sessions::append_message(data_dir, &args.session_id, m.clone())?;
        m
    };
    if let Some(pw) = partial_writer {
        pw.delete();
    }

    Ok(assistant_msg)
}

fn assistant_message_from_recorded_parts(
    mut parts: AssistantPartsRecorder,
    partial_output: String,
    tool_calls: Vec<MessageToolCall>,
    attachments: Vec<MessageAttachment>,
) -> Message {
    let final_text = parts.last_text_snapshot();
    parts.append_final_text_if_missing(&final_text);
    let assistant_parts = parts.parts.clone();
    let assistant_content = text_from_parts(&assistant_parts)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| {
            if final_text.is_empty() {
                partial_output
            } else {
                final_text
            }
        });

    Message {
        id: sessions::new_id(),
        role: Role::Assistant,
        content: assistant_content,
        attachments,
        tool_calls,
        parts: assistant_parts,
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
        subagent_call_id: None,
    }
}

fn empty_assistant_message() -> Message {
    assistant_message_from_recorded_parts(
        AssistantPartsRecorder::default(),
        String::new(),
        Vec::new(),
        Vec::new(),
    )
}

/// 流式增量写到 `partial/<msg_id>.partial.jsonl` 供崩溃/强退后恢复。
///
/// 每帧 delegate 到 [`sessions_dir::append_partial`]——后者经
/// [`crate::storage::lock::append_jsonl`] 走「open → write → fsync」，每帧落实到
/// 磁盘。不能用 `BufWriter` 包一层：进程被 SIGKILL / force-quit 时 Drop 根本不跑，
/// 内存缓冲整段丢，partial 文件就成了空壳——这是中断恢复反复失效的真因。
struct PartialFileWriter {
    data_dir: PathBuf,
    session_id: String,
    msg_id: String,
    wrote_text: bool,
}

impl PartialFileWriter {
    fn new(data_dir: &Path, session_id: &str, msg_id: String) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            session_id: session_id.to_string(),
            msg_id,
            wrote_text: false,
        }
    }

    fn append(&mut self, frag: &PartialFragment) {
        if matches!(frag, PartialFragment::Text { .. }) {
            self.wrote_text = true;
        }
        if let Err(e) =
            sessions_dir::append_partial(&self.data_dir, &self.session_id, &self.msg_id, frag)
        {
            tracing::warn!(error = %e, msg_id = %self.msg_id, "append_partial 失败");
        }
    }

    fn delete(self) {
        let _ = sessions_dir::delete_partial(&self.data_dir, &self.session_id, &self.msg_id);
    }
}

/// Desktop 端 [`TurnObserver`] 实现：累积 assistant parts / tool_calls / partial_output，
/// 把每个事件翻译成 `EngineEvent` 推送给 React，并把 HITL pending 注册到全局桥接。
struct DesktopObserver<'a> {
    parts: AssistantPartsRecorder,
    partial_output: String,
    tool_calls: Vec<MessageToolCall>,
    segment_parts: AssistantPartsRecorder,
    segment_partial_output: String,
    segment_tool_calls: Vec<MessageToolCall>,
    segment_messages: Vec<Message>,
    consumed_pending_inputs: Option<ConsumedPendingInputs>,
    consumed_pending_seen: usize,
    output_attachments: Vec<MessageAttachment>,
    hitl_state: Option<Arc<HitlState>>,
    hitl: Arc<HitlGate>,
    data_dir: PathBuf,
    session_id: String,
    emit: &'a (dyn Fn(EngineEvent) + Send + Sync),
    partial_writer: Option<PartialFileWriter>,
}

impl<'a> DesktopObserver<'a> {
    fn new(
        hitl_state: Option<Arc<HitlState>>,
        hitl: Arc<HitlGate>,
        emit: &'a (dyn Fn(EngineEvent) + Send + Sync),
        data_dir: &Path,
        session_id: &str,
        consumed_pending_inputs: Option<ConsumedPendingInputs>,
        consumed_pending_seen: usize,
    ) -> Self {
        let msg_id = sessions::new_id();
        let partial_writer = Some(PartialFileWriter::new(data_dir, session_id, msg_id));
        Self {
            parts: AssistantPartsRecorder::default(),
            partial_output: String::new(),
            tool_calls: Vec::new(),
            segment_parts: AssistantPartsRecorder::default(),
            segment_partial_output: String::new(),
            segment_tool_calls: Vec::new(),
            segment_messages: Vec::new(),
            consumed_pending_inputs,
            consumed_pending_seen,
            output_attachments: Vec::new(),
            hitl_state,
            hitl,
            data_dir: data_dir.to_path_buf(),
            session_id: session_id.to_string(),
            emit,
            partial_writer,
        }
    }

    fn finish_current_segment(&mut self) {
        if self.segment_parts.parts.is_empty()
            && self.segment_partial_output.is_empty()
            && self.segment_tool_calls.is_empty()
        {
            return;
        }
        let parts = std::mem::take(&mut self.segment_parts);
        let partial_output = std::mem::take(&mut self.segment_partial_output);
        let tool_calls = std::mem::take(&mut self.segment_tool_calls);
        self.segment_messages
            .push(assistant_message_from_recorded_parts(
                parts,
                partial_output,
                tool_calls,
                Vec::new(),
            ));
    }

    fn finish_segment_if_pending_was_consumed(&mut self) {
        let Some(consumed) = self.consumed_pending_inputs.as_ref() else {
            return;
        };
        let consumed_len = consumed.lock().unwrap().len();
        if consumed_len <= self.consumed_pending_seen {
            return;
        }
        self.consumed_pending_seen = consumed_len;
        self.finish_current_segment();
    }
}

#[async_trait]
impl<'a> TurnObserver for DesktopObserver<'a> {
    fn on_event(&mut self, event: &AgentEvent) {
        // 子 Subagent NestedRun 的事件（架构 §4.4.11.8）：装饰器已把 event.run_id 重写
        // 为父 RunId，但带 subagent_call_id 标识。父 observer 把这种事件**只**转发到 UI
        // （前端按 subagent_call_id 嵌套渲染到父 Task 卡片内部），**不**累积到父 parts /
        // tool_calls / partial sidecar——避免子内容串入父 transcript / 父 jsonl。
        // 子 session.jsonl 独立落盘由 P3.1c 接上；本期 P3.1b 阶段先保住"父 transcript 不串入"。
        if event.subagent_call_id.is_some() {
            if let Some(ev) = agent_event_to_engine_event(event) {
                (self.emit)(ev);
            }
            return;
        }

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

        if let EventPayload::TextDelta { text } = &event.payload {
            self.partial_output.push_str(text);
            self.segment_partial_output.push_str(text);
        }
        if let EventPayload::TextDone { full_text } = &event.payload {
            // complete 路径只发 TextDone 不发 TextDelta；补一次 append 避免落盘空文本。
            if !full_text.is_empty() {
                if !self
                    .parts
                    .last_text_snapshot()
                    .ends_with(full_text.as_str())
                {
                    self.parts.append_text(full_text);
                }
                if !self
                    .segment_parts
                    .last_text_snapshot()
                    .ends_with(full_text.as_str())
                {
                    self.segment_parts.append_text(full_text);
                }
                if self.partial_output.is_empty() {
                    self.partial_output.push_str(full_text);
                }
                if self.segment_partial_output.is_empty() {
                    self.segment_partial_output.push_str(full_text);
                }
            }
        }
        record_assistant_part_event(&mut self.parts, event);
        record_assistant_part_event(&mut self.segment_parts, event);
        record_tool_event(&mut self.tool_calls, event);
        record_tool_event(&mut self.segment_tool_calls, event);
        if let Some(ev) = agent_event_to_engine_event(event) {
            (self.emit)(ev);
        }

        // 实时写 partial 文件，供进程崩溃/强退后的下次加载恢复。
        if let Some(pw) = &mut self.partial_writer {
            match &event.payload {
                EventPayload::TextDelta { text } => {
                    pw.append(&PartialFragment::Text { text: text.clone() });
                }
                // non-streaming 路径只发 TextDone，没有 TextDelta
                EventPayload::TextDone { full_text } if !full_text.is_empty() && !pw.wrote_text => {
                    pw.append(&PartialFragment::Text {
                        text: full_text.clone(),
                    });
                }
                EventPayload::Reasoning { text } => {
                    pw.append(&PartialFragment::Reasoning { text: text.clone() });
                }
                EventPayload::ToolCallStarted {
                    index, name, input, ..
                } => {
                    let args = serde_json::to_string(input)
                        .ok()
                        .filter(|s| s != "null")
                        .unwrap_or_default();
                    pw.append(&PartialFragment::ToolCall {
                        index: *index as u32,
                        name: Some(name.clone()),
                        arguments_chunk: args,
                    });
                }
                EventPayload::ToolCallDelta {
                    index,
                    name,
                    arguments_delta,
                    ..
                } => {
                    if let Some(chunk) = arguments_delta {
                        pw.append(&PartialFragment::ToolCall {
                            index: *index as u32,
                            name: name.clone(),
                            arguments_chunk: chunk.clone(),
                        });
                    }
                }
                _ => {}
            }
        }

        if matches!(
            event.payload,
            EventPayload::TurnFinished { .. } | EventPayload::TurnStarted { .. }
        ) {
            self.finish_segment_if_pending_was_consumed();
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

fn persist_interrupted_assistant_output(
    data_dir: &std::path::Path,
    session_id: &str,
    partial_output: &str,
    parts: &[MessagePart],
    tool_calls: &[MessageToolCall],
) -> AppResult<Session> {
    let mut session = sessions::load(data_dir, session_id)?;
    if let Some(message) = assistant_message_from_partial(partial_output, parts, tool_calls) {
        session.messages.push(message);
    }
    session.messages.push(Message {
        id: sessions::new_id(),
        role: Role::Marker,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: Some(MessageMeta::Interrupted),
        subagent_call_id: None,
    });
    sessions::save(data_dir, session)
}

fn persist_failed_assistant_output(
    data_dir: &std::path::Path,
    session_id: &str,
    partial_output: &str,
    parts: &[MessagePart],
    tool_calls: &[MessageToolCall],
    error: &str,
) -> AppResult<Session> {
    let mut session = sessions::load(data_dir, session_id)?;
    session.messages.push(failed_assistant_message(
        partial_output,
        parts,
        tool_calls,
        error,
    ));
    sessions::save(data_dir, session)
}

fn assistant_message_from_partial(
    partial_output: &str,
    parts: &[MessagePart],
    tool_calls: &[MessageToolCall],
) -> Option<Message> {
    let assistant_parts = normalized_partial_parts(partial_output, parts);
    let assistant_content = text_from_parts(&assistant_parts)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| partial_output.to_string());

    if assistant_content.is_empty() && assistant_parts.is_empty() && tool_calls.is_empty() {
        return None;
    }

    Some(Message {
        id: sessions::new_id(),
        role: Role::Assistant,
        content: assistant_content,
        attachments: Vec::new(),
        tool_calls: tool_calls.to_vec(),
        parts: assistant_parts,
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
        subagent_call_id: None,
    })
}

fn failed_assistant_message(
    partial_output: &str,
    parts: &[MessagePart],
    tool_calls: &[MessageToolCall],
    error: &str,
) -> Message {
    let mut assistant_parts = normalized_partial_parts(partial_output, parts);
    let error_marker = format!("[请求失败：{}]", error.trim());
    if !error_marker.trim().is_empty() {
        if !assistant_parts.is_empty() {
            assistant_parts.push(MessagePart::Text {
                text: format!("\n\n{error_marker}"),
            });
        } else {
            assistant_parts.push(MessagePart::Text { text: error_marker });
        }
    }
    let assistant_content = text_from_parts(&assistant_parts)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| format_failed_assistant_content(partial_output, error));

    Message {
        id: sessions::new_id(),
        role: Role::Assistant,
        content: assistant_content,
        attachments: Vec::new(),
        tool_calls: tool_calls.to_vec(),
        parts: assistant_parts,
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
        subagent_call_id: None,
    }
}

fn normalized_partial_parts(partial_output: &str, parts: &[MessagePart]) -> Vec<MessagePart> {
    if !parts.is_empty() {
        return parts.to_vec();
    }
    if partial_output.is_empty() {
        Vec::new()
    } else {
        vec![MessagePart::Text {
            text: partial_output.to_string(),
        }]
    }
}

fn format_failed_assistant_content(partial_output: &str, error: &str) -> String {
    let partial = partial_output.trim();
    let error = error.trim();
    if partial.is_empty() {
        format!("请求失败：{error}")
    } else {
        format!("{partial}\n\n[请求失败：{error}]")
    }
}

#[derive(Default)]
struct AssistantPartsRecorder {
    parts: Vec<MessagePart>,
    by_index: HashMap<usize, usize>,
    by_id: HashMap<String, usize>,
}

impl AssistantPartsRecorder {
    fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        match self.parts.last_mut() {
            Some(MessagePart::Text { text: existing }) => existing.push_str(text),
            _ => self.parts.push(MessagePart::Text {
                text: text.to_string(),
            }),
        }
    }

    fn append_reasoning(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.parts.last_mut() {
            Some(MessagePart::Reasoning { text: existing }) => existing.push_str(text),
            _ => self.parts.push(MessagePart::Reasoning {
                text: text.to_string(),
            }),
        }
    }

    fn append_final_text_if_missing(&mut self, final_text: &str) {
        if final_text.is_empty() {
            return;
        }

        let current = text_from_parts(&self.parts).unwrap_or_default();
        if !current.ends_with(final_text) {
            self.append_text(final_text);
        }
    }

    /// 当前已累积文本（仅 Text part 的拼接）
    fn last_text_snapshot(&self) -> String {
        text_from_parts(&self.parts).unwrap_or_default()
    }

    fn apply_tool_delta(
        &mut self,
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        arguments_delta: Option<&str>,
    ) {
        let pos = self.tool_position(index, id, name);
        let MessagePart::ToolCall {
            id: existing_id,
            name: existing_name,
            arguments,
            ..
        } = &mut self.parts[pos]
        else {
            return;
        };

        if let Some(next_id) = id.filter(|value| !value.trim().is_empty()) {
            *existing_id = next_id.to_string();
            self.by_id.insert(next_id.to_string(), pos);
        }
        if let Some(next_name) = name.filter(|value| !value.trim().is_empty()) {
            *existing_name = next_name.to_string();
        }
        if let Some(delta) = arguments_delta.filter(|value| !value.is_empty()) {
            arguments.push_str(delta);
        }
    }

    fn start_tool(&mut self, index: usize, call_id: &str, name: &str, input: serde_json::Value) {
        let pos = self.tool_position(index, Some(call_id), Some(name));
        if let MessagePart::ToolCall {
            id,
            name: existing_name,
            input: existing_input,
            arguments,
            ..
        } = &mut self.parts[pos]
        {
            *id = call_id.to_string();
            *existing_name = name.to_string();
            *existing_input = input.clone();
            if arguments.is_empty() {
                *arguments = input.to_string();
            }
            self.by_id.insert(call_id.to_string(), pos);
        }
    }

    fn finish_tool(&mut self, index: usize, call_id: &str, result: &str, duration_ms: u64) {
        let pos = self.tool_position(index, Some(call_id), None);
        if let MessagePart::ToolCall {
            id,
            result: existing_result,
            duration_ms: existing_duration_ms,
            ..
        } = &mut self.parts[pos]
        {
            *id = call_id.to_string();
            *existing_result = Some(result.to_string());
            *existing_duration_ms = Some(duration_ms);
            self.by_id.insert(call_id.to_string(), pos);
        }
    }

    fn tool_position(&mut self, index: usize, id: Option<&str>, name: Option<&str>) -> usize {
        if let Some(pos) = id
            .filter(|value| !value.trim().is_empty())
            .and_then(|value| self.by_id.get(value).copied())
        {
            self.by_index.entry(index).or_insert(pos);
            return pos;
        }

        if let Some(pos) = self.by_index.get(&index).copied() {
            return pos;
        }

        let pos = self.parts.len();
        let clean_id = id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default()
            .to_string();
        self.parts.push(MessagePart::ToolCall {
            id: clean_id.clone(),
            name: name
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_default()
                .to_string(),
            input: serde_json::json!({}),
            arguments: String::new(),
            result: None,
            duration_ms: None,
        });
        self.by_index.insert(index, pos);
        if !clean_id.is_empty() {
            self.by_id.insert(clean_id, pos);
        }
        pos
    }
}

fn record_assistant_part_event(parts: &mut AssistantPartsRecorder, event: &AgentEvent) {
    match &event.payload {
        AgentEventPayload::TextDelta { text } => parts.append_text(text),
        AgentEventPayload::Reasoning { text } => parts.append_reasoning(text),
        AgentEventPayload::ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } => parts.apply_tool_delta(
            *index,
            id.as_deref(),
            name.as_deref(),
            arguments_delta.as_deref(),
        ),
        AgentEventPayload::ToolCallStarted {
            index,
            call_id,
            name,
            input,
        } => parts.start_tool(*index, call_id, name, input.clone()),
        AgentEventPayload::ToolCallFinished {
            index,
            call_id,
            result,
            duration_ms,
            ..
        } => parts.finish_tool(*index, call_id, result, *duration_ms),
        _ => {}
    }
}

fn text_from_parts(parts: &[MessagePart]) -> Option<String> {
    let mut out = String::new();
    for part in parts {
        if let MessagePart::Text { text } = part {
            out.push_str(text);
        }
    }
    Some(out)
}

fn record_tool_event(tool_calls: &mut Vec<MessageToolCall>, event: &AgentEvent) {
    match &event.payload {
        AgentEventPayload::ToolCallDelta {
            index, id, name, ..
        } => {
            // 流式传输中填充 tool_calls 字段，确保中断时 tool calls 能落盘。
            if tool_calls.len() <= *index {
                tool_calls.resize_with(*index + 1, empty_tool_call);
            }
            let call = &mut tool_calls[*index];
            if let Some(id) = id {
                call.id.clone_from(id);
            }
            if let Some(name) = name {
                call.name.clone_from(name);
            }
        }
        AgentEventPayload::ToolCallStarted {
            index,
            call_id,
            name,
            input,
        } => {
            upsert_tool_call(tool_calls, *index, call_id, name, input.clone());
        }
        AgentEventPayload::ToolCallFinished {
            index,
            call_id,
            result,
            duration_ms,
            ..
        } => {
            if tool_calls.len() <= *index {
                tool_calls.resize_with(*index + 1, empty_tool_call);
            }
            let call = &mut tool_calls[*index];
            call.id = call_id.clone();
            call.result = Some(result.clone());
            call.duration_ms = Some(*duration_ms);
        }
        _ => {}
    }
}

fn upsert_tool_call(
    calls: &mut Vec<MessageToolCall>,
    index: usize,
    call_id: &str,
    name: &str,
    input: serde_json::Value,
) {
    if calls.len() <= index {
        calls.resize_with(index + 1, empty_tool_call);
    }

    let call = &mut calls[index];
    call.id = call_id.to_string();
    call.name = name.to_string();
    call.input = input;
}

fn empty_tool_call() -> MessageToolCall {
    MessageToolCall {
        id: String::new(),
        name: String::new(),
        input: serde_json::json!({}),
        result: None,
        duration_ms: None,
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
    };
    sessions::append_message(data_dir, session_id, marker)?;

    Ok(ContextUsageDto {
        used_tokens: result.after_tokens,
        budget_tokens,
    })
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

/// 构造一份「真实发给模型的 payload」预览,用于桌面 UI 的「显示原始 JSON」。
///
/// 复刻 [`agent_loop`] 进入模型调用之前的所有拼装动作:workspace XML、内置工具、
/// 用户启用的工具、session 历史 transcript,但**不真正发起请求、不修改 session**。
/// 输出统一为 OpenAI 风格的 `{model, messages, tools, ...}`,前端用 JsonView 渲染。
pub async fn build_preview_payload(
    data_dir: &Path,
    session_id: &str,
    upto_message_id: Option<&str>,
) -> AppResult<serde_json::Value> {
    use agent_core::system_prompt::{compose_system_prompt, EnvironmentSnapshot};
    use agent_core::tools::{
        ask_only_definitions, hosted_tool_definitions, registry::ToolRegistry, BUILTIN_TOOL_NAMES,
        CONDITIONAL_TOOL_NAMES,
    };

    let session = sessions::load(data_dir, session_id)?;
    let settings = global_settings::load(data_dir);

    let workdir = session
        .workdir
        .clone()
        .or_else(|| settings.conversation.workdir.clone())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let initial_allowed_paths = session
        .allowed_paths
        .clone()
        .unwrap_or_else(|| settings.conversation.allowed_paths.clone());
    let workspace = Workspace::with_runtime_state(
        workdir.clone(),
        initial_allowed_paths.clone(),
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

    // preview 用同样的优先级链：session 非空 → session；否则全局
    let session_enabled_tools = {
        let s = session.enabled_tools.clone().unwrap_or_default();
        if s.is_empty() {
            settings.conversation.enabled_tools.clone()
        } else {
            s
        }
    };

    // 工具定义:ask + 内置 + 用户开的本地工具 + provider hosted 工具。
    // 预览路径不会真发命令,bg_log_dir + phase 都用占位 None / 空 channel。
    // BgTaskRegistry 用临时本地实例（预览只生成 tool schema，不真跑命令）。
    let registry = ToolRegistry::new(
        agent_core::tools::default_tools_with_mcp(
            workspace.clone(),
            &skill_dirs,
            None,
            agent_core::wakeup::new_phase_channel(),
            agent_core::tools::background::BgTaskRegistry::new(),
            None,
            None,
            None,
            settings.general.shell.clone(),
            settings.general.edit_backend,
            agent_core::storage::mcp::load(data_dir).with_cwd(workspace.workdir().to_path_buf()),
        )
        .await,
    );
    let mut tool_defs = ask_only_definitions();
    let mut all_filter: Vec<String> = BUILTIN_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
    all_filter.extend(CONDITIONAL_TOOL_NAMES.iter().map(|s| s.to_string()));
    all_filter.extend(session_enabled_tools.iter().cloned());
    tool_defs.extend(registry.definitions(&all_filter));
    tool_defs.extend(registry.mcp_definitions());
    if !session_enabled_tools.is_empty() {
        tool_defs.extend(hosted_tool_definitions(&session_enabled_tools));
    }

    // system = BASE prompt + 用户 persona + rules（与 agent_loop 一致）
    let combined_system = {
        let mut s = compose_system_prompt(session.system_prompt.as_deref());
        let used_global_rules_for_system = session
            .global_rules
            .clone()
            .unwrap_or_else(|| settings.conversation.global_rules.clone());
        let rules_content_for_system = agent_core::rules::resolve_injection_files(
            &used_global_rules_for_system,
            session.rules_files.as_deref(),
            &workdir,
            &initial_allowed_paths,
        );
        let rules_block_for_system = agent_core::rules::format_injection(&rules_content_for_system);
        if !rules_block_for_system.is_empty() {
            s.push('\n');
            s.push_str(&rules_block_for_system);
        }
        s
    };

    // 首条 user message 头部要追加 <environment> 块（与 Session::append_user 一致），
    // preview 时按同一逻辑还原，确保「显示 JSON」与实际发给模型的 payload 一致。
    let extra_paths_preview = PermissionStore::open(data_dir)
        .map(|s| s.effective_paths(Some(&workdir)))
        .unwrap_or_default();
    let env_snapshot =
        EnvironmentSnapshot::from_workspace(&workspace).with_extra_paths(extra_paths_preview);
    let env_block = env_snapshot.render();

    let mut first_user_pending = true;

    let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({
        "role": "system",
        "content": combined_system,
    })];
    for m in &session.messages {
        match m.role {
            Role::Marker | Role::System => {}
            Role::User => {
                let mut value = preview_user_content(m);
                if first_user_pending {
                    prepend_environment_to_preview(&mut value, &env_block);
                    first_user_pending = false;
                }
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": value,
                }))
            }
            Role::Assistant => preview_push_assistant(&mut messages, m),
        }
        if upto_message_id.is_some_and(|id| m.id == id) {
            break;
        }
    }

    let tools: Vec<serde_json::Value> = tool_defs
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect();

    Ok(serde_json::json!({
        "model": session.model,
        "messages": messages,
        "tools": tools,
        "_workspace": {
            "workdir": workdir.display().to_string(),
            "initial_allowed_paths": initial_allowed_paths,
            "runtime_allowed_paths": session.runtime_allowed_paths,
            "pending_runtime_allowed_paths": session.pending_runtime_allowed_paths,
            "skill_dirs": skill_dirs.iter().map(|(_, p)| p.display().to_string()).collect::<Vec<_>>(),
        }
    }))
}

/// 把 `<environment>` 块前置到 preview 的 user content 上。
/// content 是 string 时直接拼前缀；是 array（含 attachments）时拼到首个 text block 前，
/// 没有 text block 就插一个新的 text block 在最前。
fn prepend_environment_to_preview(value: &mut serde_json::Value, env_block: &str) {
    if env_block.is_empty() {
        return;
    }
    match value {
        serde_json::Value::String(s) => {
            *s = format!("{env_block}{s}");
        }
        serde_json::Value::Array(blocks) => {
            if let Some(first_text) = blocks
                .iter_mut()
                .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("text"))
            {
                if let Some(text) = first_text.get_mut("text").and_then(|v| v.as_str()) {
                    let merged = format!("{env_block}{text}");
                    first_text["text"] = serde_json::Value::String(merged);
                }
            } else {
                blocks.insert(0, serde_json::json!({"type": "text", "text": env_block}));
            }
        }
        _ => {}
    }
}

fn preview_user_content(m: &Message) -> serde_json::Value {
    if m.attachments.is_empty() {
        return serde_json::Value::String(m.content.clone());
    }
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    if !m.content.is_empty() {
        blocks.push(serde_json::json!({"type": "text", "text": m.content}));
    }
    for a in &m.attachments {
        match a {
            MessageAttachment::Image {
                media_type, data, ..
            } => blocks.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{}", media_type, data) },
            })),
            MessageAttachment::TextFile {
                name,
                media_type,
                content,
            } => blocks.push(serde_json::json!({
                "type": "text",
                "text": format!(
                    "<file name=\"{name}\" media_type=\"{media_type}\">\n{content}\n</file>"
                ),
            })),
        }
    }
    serde_json::Value::Array(blocks)
}

fn preview_push_assistant(out: &mut Vec<serde_json::Value>, m: &Message) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut tool_results: Vec<serde_json::Value> = Vec::new();

    let push_call = |list: &mut Vec<serde_json::Value>, id: &str, name: &str, args: String| {
        list.push(serde_json::json!({
            "id": id,
            "type": "function",
            "function": { "name": name, "arguments": args },
        }));
    };

    if !m.parts.is_empty() {
        for p in &m.parts {
            match p {
                MessagePart::Text { text } => text_parts.push(text.clone()),
                MessagePart::Reasoning { text } => reasoning_parts.push(text.clone()),
                MessagePart::ToolCall {
                    id,
                    name,
                    input,
                    arguments,
                    result,
                    ..
                } => {
                    let args = if !arguments.is_empty() {
                        arguments.clone()
                    } else {
                        serde_json::to_string(input).unwrap_or_else(|_| "{}".into())
                    };
                    push_call(&mut tool_calls, id, name, args);
                    if let Some(res) = result {
                        tool_results.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": res,
                        }));
                    }
                }
            }
        }
    } else if !m.tool_calls.is_empty() {
        if !m.content.is_empty() {
            text_parts.push(m.content.clone());
        }
        for tc in &m.tool_calls {
            let args = serde_json::to_string(&tc.input).unwrap_or_else(|_| "{}".into());
            push_call(&mut tool_calls, &tc.id, &tc.name, args);
            if let Some(res) = &tc.result {
                tool_results.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "content": res,
                }));
            }
        }
    } else if !m.content.is_empty() {
        text_parts.push(m.content.clone());
    }

    let mut assistant = serde_json::json!({
        "role": "assistant",
        "content": text_parts.join(""),
    });
    let map = assistant.as_object_mut().expect("json object");
    if !reasoning_parts.is_empty() {
        map.insert(
            "reasoning".into(),
            serde_json::Value::String(reasoning_parts.join("")),
        );
    }
    if !tool_calls.is_empty() {
        map.insert("tool_calls".into(), serde_json::Value::Array(tool_calls));
    }
    out.push(assistant);
    out.extend(tool_results);
}

fn agent_event_to_engine_event(event: &AgentEvent) -> Option<EngineEvent> {
    use agent_core::types::AgentEventPayload::*;
    let subagent = event.subagent_call_id.clone();
    match &event.payload {
        TextDelta { text } => Some(EngineEvent::TextDelta {
            text: text.clone(),
            subagent_call_id: subagent.clone(),
        }),
        TextDone { full_text } => Some(EngineEvent::TextDone {
            full_text: full_text.clone(),
            subagent_call_id: subagent.clone(),
        }),
        Reasoning { text } => Some(EngineEvent::Reasoning {
            text: text.clone(),
            subagent_call_id: subagent.clone(),
        }),
        ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } => Some(EngineEvent::ToolCallDelta {
            index: *index,
            id: id.clone(),
            name: name.clone(),
            arguments_delta: arguments_delta.clone(),
            subagent_call_id: subagent.clone(),
        }),
        ToolCallStarted {
            index,
            call_id,
            name,
            input,
        } => Some(EngineEvent::ToolStart {
            index: *index,
            id: call_id.clone(),
            name: name.clone(),
            input: input.clone(),
            subagent_call_id: subagent.clone(),
        }),
        ToolCallFinished {
            index,
            call_id,
            result,
            duration_ms,
            artifact_path,
            ..
        } => Some(EngineEvent::ToolDone {
            index: *index,
            id: call_id.clone(),
            result: result.clone(),
            duration_ms: *duration_ms,
            artifact_path: artifact_path.clone(),
            subagent_call_id: subagent.clone(),
        }),
        ToolCallOutputDelta {
            index,
            call_id,
            chunk,
        } => Some(EngineEvent::ToolOutputDelta {
            index: *index,
            id: call_id.clone(),
            chunk: chunk.clone(),
            subagent_call_id: subagent.clone(),
        }),
        RunFinished { duration_ms, .. } => Some(EngineEvent::RunFinished {
            duration_ms: *duration_ms,
        }),
        Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        } => Some(EngineEvent::Usage {
            input_tokens: *input_tokens,
            output_tokens: *output_tokens,
            cache_read_tokens: *cache_read_tokens,
            cache_creation_tokens: *cache_creation_tokens,
        }),
        RunFailed { error } => Some(EngineEvent::Error {
            message: error.message.clone(),
        }),
        RunSuspended {
            reason,
            resumes_at_ms,
            waiting_for_task_ids,
        } => Some(EngineEvent::RunSuspended {
            reason: match reason {
                protocol::SuspendReason::BackgroundTask => "background_task".into(),
                protocol::SuspendReason::Cron => "cron".into(),
                protocol::SuspendReason::Manual => "manual".into(),
            },
            resumes_at_ms: *resumes_at_ms,
            waiting_for_task_ids: waiting_for_task_ids.clone(),
        }),
        RunResumed { cause } => Some(EngineEvent::RunResumed {
            cause: match cause {
                protocol::ResumeCause::BgTaskFinished { task_id, .. } => {
                    format!("bg_task_finished:{task_id}")
                }
                protocol::ResumeCause::CronFired { original_reason } => {
                    format!("cron_fired:{original_reason}")
                }
                protocol::ResumeCause::UserMessageArrived => "user_message_arrived".into(),
                protocol::ResumeCause::ManualResume => "manual_resume".into(),
            },
        }),
        PermissionRequested {
            request_id,
            kind,
            summary,
            risk,
        } => {
            let (
                kind_str,
                tool_name,
                tool_input,
                paths,
                fingerprint,
                command_segments,
                segments,
                refuse_remember,
                plan,
            ) = match kind {
                agent_core::types::PermissionKind::ToolCall {
                    tool_name,
                    input,
                    fingerprint,
                    command_segments,
                    segments,
                    refuse_remember,
                } => (
                    "tool_call",
                    tool_name.clone(),
                    input.clone(),
                    Vec::<String>::new(),
                    fingerprint.clone(),
                    command_segments.clone(),
                    segments.clone(),
                    *refuse_remember,
                    None,
                ),
                agent_core::types::PermissionKind::PathAccess { tool_name, paths } => (
                    "path_access",
                    tool_name.clone(),
                    serde_json::Value::Null,
                    paths.clone(),
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                    None,
                ),
                agent_core::types::PermissionKind::Plan {
                    plan_id,
                    plan_path,
                    plan_markdown,
                    summary: plan_summary,
                    steps: _,
                } => (
                    "plan",
                    String::new(),
                    serde_json::Value::Null,
                    Vec::new(),
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                    Some(crate::engine::PlanPermissionDto {
                        plan_id: plan_id.clone(),
                        plan_path: plan_path.clone(),
                        plan_markdown: plan_markdown.clone(),
                        summary: plan_summary.clone(),
                    }),
                ),
                agent_core::types::PermissionKind::ContinueLongRun { .. } => (
                    "continue_long_run",
                    String::new(),
                    serde_json::Value::Null,
                    Vec::new(),
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                    None,
                ),
            };
            Some(EngineEvent::PermissionRequested {
                request_id: request_id.0.clone(),
                kind: kind_str.into(),
                tool_name,
                input: tool_input,
                summary: summary.clone(),
                risk: format!("{risk:?}").to_lowercase(),
                paths,
                fingerprint,
                command_segments,
                segments,
                refuse_remember,
                plan,
            })
        }
        PermissionResolved {
            request_id,
            decision,
        } => Some(EngineEvent::PermissionResolved {
            request_id: request_id.0.clone(),
            decision: match decision {
                agent_core::types::ApprovalDecision::AllowOnce => "allow_once".into(),
                agent_core::types::ApprovalDecision::AllowAndRemember { .. } => {
                    "allow_and_remember".into()
                }
                agent_core::types::ApprovalDecision::Deny => "deny".into(),
                agent_core::types::ApprovalDecision::DenyWithFeedback { .. } => {
                    "deny_with_feedback".into()
                }
            },
        }),
        PermissionAutoJudged {
            request_id,
            tool_name,
            decision,
            reason,
        } => Some(EngineEvent::PermissionAutoJudged {
            request_id: request_id
                .as_ref()
                .map(|id| id.0.clone())
                .unwrap_or_default(),
            tool_name: tool_name.clone(),
            decision: decision.clone(),
            reason: reason.clone(),
        }),
        Notice {
            level,
            message,
            dedup_key,
        } => Some(EngineEvent::Notice {
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
        StepStarted {
            step_kind,
            step_index,
        } => Some(EngineEvent::StepStarted {
            step_kind: match step_kind {
                protocol::StepKind::Model => "model".to_string(),
                protocol::StepKind::Tool => "tool".to_string(),
            },
            step_index: *step_index,
        }),
        StepFinished {
            step_kind,
            step_index,
        } => Some(EngineEvent::StepFinished {
            step_kind: match step_kind {
                protocol::StepKind::Model => "model".to_string(),
                protocol::StepKind::Tool => "tool".to_string(),
            },
            step_index: *step_index,
        }),
        ModelRetry {
            attempt,
            max,
            delay_ms,
            reason,
        } => Some(EngineEvent::ModelRetry {
            attempt: *attempt,
            max: *max,
            delay_ms: *delay_ms,
            reason: reason.clone(),
        }),
        ContextCompacted {
            before_tokens,
            after_tokens,
        } => Some(EngineEvent::ContextCompacted {
            before_tokens: *before_tokens,
            after_tokens: *after_tokens,
        }),
        RunModeChanged { from, to } => Some(EngineEvent::RunModeChanged {
            from: from.clone(),
            to: to.clone(),
        }),
        TurnFinished { stop_reason, .. } => Some(EngineEvent::TurnFinished {
            stop_reason: match stop_reason {
                protocol::StopReason::EndTurn => "end_turn",
                protocol::StopReason::MaxIterations => "max_iterations",
                protocol::StopReason::PermissionDenied => "permission_denied",
                protocol::StopReason::Cancelled => "cancelled",
                protocol::StopReason::Failed => "failed",
            }
            .to_string(),
        }),
        UserQuestionRequested {
            request_id,
            question,
            options,
            multi,
            questions,
        } => Some(EngineEvent::UserQuestionRequested {
            request_id: request_id.0.clone(),
            question: question.clone(),
            options: options.iter().cloned().map(Into::into).collect(),
            multi: *multi,
            questions: questions.iter().cloned().map(Into::into).collect(),
        }),
        UserQuestionAnswered { request_id, answer } => {
            let (kind, text) = match answer {
                protocol::UserAnswer::Selected { label } => ("selected", label.clone()),
                protocol::UserAnswer::SelectedMulti { labels } => {
                    ("selected_multi", labels.join("、"))
                }
                protocol::UserAnswer::Custom { text } => ("custom", text.clone()),
                protocol::UserAnswer::Cancelled => ("cancelled", String::new()),
                protocol::UserAnswer::Multi { items } => {
                    let text = items
                        .iter()
                        .map(|item| format!("{}: {}", item.title, item.answer.to_agent_text()))
                        .collect::<Vec<_>>()
                        .join("；");
                    ("multi", text)
                }
            };
            Some(EngineEvent::UserQuestionAnswered {
                request_id: request_id.0.clone(),
                kind: kind.to_string(),
                text,
            })
        }
        TurnEditsCommitted {
            turn_id,
            turn,
            files,
        } => Some(EngineEvent::TurnEditsCommitted {
            turn_id: turn_id.0.clone(),
            turn: *turn,
            files: files.clone(),
        }),
        TurnEditsReverted { turn_id } => Some(EngineEvent::TurnEditsReverted {
            turn_id: turn_id.0.clone(),
        }),
        TurnEditsRevertFailed {
            turn_id,
            file_path,
            error,
        } => Some(EngineEvent::TurnEditsRevertFailed {
            turn_id: turn_id.0.clone(),
            file_path: file_path.clone(),
            error: error.clone(),
        }),
        SessionTitleChanged { session_id, title } => Some(EngineEvent::SessionTitleChanged {
            session_id: session_id.clone(),
            title: title.clone(),
        }),
        TodoListUpdated { todos } => Some(EngineEvent::TodoListUpdated {
            todos: todos.iter().cloned().map(Into::into).collect(),
        }),
        PlanReady {
            plan_id,
            plan_path,
            plan_markdown,
            summary,
        } => Some(EngineEvent::PlanReady {
            plan_id: plan_id.clone(),
            plan_path: plan_path.clone(),
            plan_markdown: plan_markdown.clone(),
            summary: summary.clone(),
        }),
        PlanCommentAdded { plan_id, comment } => Some(EngineEvent::PlanCommentAdded {
            plan_id: plan_id.clone(),
            comment: comment.clone().into(),
        }),
        MemoryExtracted { session_id, items } => Some(EngineEvent::MemoryExtracted {
            session_id: session_id.clone(),
            items: items.clone(),
        }),
        MemoryExtractionFailed { session_id, reason } => {
            Some(EngineEvent::MemoryExtractionFailed {
                session_id: session_id.clone(),
                reason: reason.clone(),
            })
        }
        _ => None,
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

/// 将 agent_core EngineEvent 翻译为 hebisland 推送/撤销通知。
fn push_engine_event_to_island(client: &HebislandClient, event: &EngineEvent) {
    match event {
        EngineEvent::PermissionRequested {
            request_id,
            tool_name,
            input,
            ..
        } => {
            let summary: String = input.to_string().chars().take(80).collect();
            // TODO: 推送子命令勾选列表（需要 EngineEvent 携带 subcommands 数据）
            client.push(
                format!("perm-{request_id}"),
                "approval",
                "需要你的审批",
                &format!("{tool_name} {summary}"),
                None,
                None,
            );
        }
        EngineEvent::UserQuestionRequested {
            request_id,
            question,
            options,
            multi,
            ..
        } => {
            // 构建 options JSON
            let options_json: Vec<String> = options
                .iter()
                .map(|opt| {
                    format!(
                        r#"{{"label":"{}","desc":"{}"}}"#,
                        opt.label.replace('"', r#"\""#),
                        opt.description.replace('"', r#"\""#)
                    )
                })
                .collect();
            let extra = if options_json.is_empty() {
                format!(r#","multiSelect":{}"#, multi)
            } else {
                format!(
                    r#","options":[{}],"multiSelect":{}"#,
                    options_json.join(","),
                    multi
                )
            };
            client.push(
                format!("question-{request_id}"),
                "question",
                "需要你的回答",
                question,
                None,
                Some(&extra),
            );
        }
        EngineEvent::PermissionResolved { request_id, .. }
        | EngineEvent::UserQuestionAnswered { request_id, .. } => {
            client.dismiss(&format!("perm-{request_id}"));
            client.dismiss(&format!("question-{request_id}"));
        }
        _ => {}
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

    /// 回归测试：进程在流式写到一半被 SIGKILL / force-quit 时，PartialFileWriter
    /// 的 Drop 不会跑。本测试用 `std::mem::forget` 跳过 Drop，模拟这一场景。
    ///
    /// 旧实现（BufWriter 包 File）：缓冲在进程内存里，Drop 不跑就丢，文件为空
    /// → recover 不到内容。
    /// 修复后：每帧 delegate 到 sessions_dir::append_partial（write + fsync），
    /// Drop 不跑也不影响——内容已经在磁盘上 → recover 能拿回完整文本。
    #[test]
    fn partial_writer_survives_process_kill_without_drop() {
        let dir = temp_data_dir();
        let sid = "kill-test-session";
        sessions_dir::ensure_session_dirs(&dir, sid).unwrap();

        let mut pw = PartialFileWriter::new(&dir, sid, "msg-x".into());
        pw.append(&PartialFragment::Text { text: "hel".into() });
        pw.append(&PartialFragment::Text { text: "lo".into() });
        pw.append(&PartialFragment::Reasoning {
            text: "思考片段".into(),
        });
        pw.append(&PartialFragment::ToolCall {
            index: 0,
            name: Some("Bash".into()),
            arguments_chunk: r#"{"cmd""#.into(),
        });
        pw.append(&PartialFragment::ToolCall {
            index: 0,
            name: None,
            arguments_chunk: r#":"ls"}"#.into(),
        });

        // 模拟进程被 SIGKILL：Drop 不跑，writer 状态全部丢
        std::mem::forget(pw);

        // 此刻文件必须已经在磁盘上有完整内容——不依赖任何 flush
        let recovered = sessions_dir::recover_interrupted_partials(&dir, sid).unwrap();
        assert_eq!(recovered.len(), 1, "应恢复一个 partial 文件");
        let r = &recovered[0];
        assert_eq!(r.msg_id, "msg-x");
        assert_eq!(r.text, "hello", "TextDelta 必须完整保留");
        assert_eq!(r.reasoning, "思考片段");
        let tc = r.tool_calls.get(&0).expect("tool_call 0 应在");
        assert_eq!(tc.0.as_deref(), Some("Bash"));
        assert_eq!(tc.1, r#"{"cmd":"ls"}"#, "ToolCallDelta 必须完整拼回");
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
                    claude_code_compat: false,
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
    fn continue_run_does_not_append_empty_user_message_to_model_request() {
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
                },
                |_| {},
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
            assert_eq!(user_texts.len(), 1);
            assert!(user_texts[0].contains("上一轮问题"));
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
                        arguments_delta: Some("{\"command\":\"touch automode-ok\"}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        calls: vec![ToolCall {
                            id: "call_bash".to_string(),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "touch automode-ok"}),
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
    fn persist_interrupted_output_appends_partial_assistant_then_marker() {
        let data_dir = temp_data_dir();
        let session = sessions::create(
            &data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .unwrap();

        persist_interrupted_assistant_output(&data_dir, &session.id, "partial answer", &[], &[])
            .unwrap();

        let saved = sessions::load(&data_dir, &session.id).unwrap();
        assert_eq!(saved.messages.len(), 2);
        assert_eq!(saved.messages[0].role, Role::Assistant);
        assert_eq!(saved.messages[0].content, "partial answer");
        assert_eq!(saved.messages[1].role, Role::Marker);
        assert!(matches!(
            saved.messages[1].meta,
            Some(MessageMeta::Interrupted)
        ));

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn persist_failed_output_appends_assistant_error_message() {
        let data_dir = temp_data_dir();
        let session = sessions::create(
            &data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .unwrap();

        persist_failed_assistant_output(
            &data_dir,
            &session.id,
            "partial answer",
            &[],
            &[],
            "HTTP 400: missing name",
        )
        .unwrap();

        let saved = sessions::load(&data_dir, &session.id).unwrap();
        assert_eq!(saved.messages.len(), 1);
        assert_eq!(saved.messages[0].role, Role::Assistant);
        assert!(saved.messages[0].content.contains("partial answer"));
        assert!(saved.messages[0].content.contains("HTTP 400: missing name"));

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn persist_failed_output_preserves_structured_parts_and_tool_calls() {
        let data_dir = temp_data_dir();
        let session = sessions::create(
            &data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .unwrap();
        let parts = vec![
            MessagePart::Text {
                text: "准备执行".to_string(),
            },
            MessagePart::ToolCall {
                id: "call_bash".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "pwd"}),
                arguments: "{\"command\":\"pwd\"}".to_string(),
                result: Some("/tmp\n".to_string()),
                duration_ms: Some(12),
            },
        ];
        let calls = vec![MessageToolCall {
            id: "call_bash".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "pwd"}),
            result: Some("/tmp\n".to_string()),
            duration_ms: Some(12),
        }];

        persist_failed_assistant_output(
            &data_dir,
            &session.id,
            "准备执行",
            &parts,
            &calls,
            "provider refused follow-up",
        )
        .unwrap();

        let saved = sessions::load(&data_dir, &session.id).unwrap();
        assert_eq!(saved.messages.len(), 1);
        let message = &saved.messages[0];
        assert_eq!(message.role, Role::Assistant);
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].name, "Bash");
        assert_eq!(message.parts.len(), 3);
        assert!(matches!(
            &message.parts[1],
            MessagePart::ToolCall { name, result, .. }
                if name == "Bash" && result.as_deref() == Some("/tmp\n")
        ));
        assert!(message.content.contains("准备执行"));
        assert!(message.content.contains("provider refused follow-up"));

        std::fs::remove_dir_all(data_dir).unwrap();
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
                },
                |_| {},
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
                },
                |_| {},
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
            let events = Arc::new(Mutex::new(Vec::<EngineEvent>::new()));
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
                    },
                    move |event| {
                        events_for_emit.lock().unwrap().push(event);
                    },
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
                },
                |_| {},
                move |_provider, _model, _reasoning| {
                    Ok(Arc::new(PendingInputOrderClient {
                        calls: AtomicUsize::new(0),
                        pending_inputs: pending_for_client.clone(),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "后续回答");
            let saved = sessions::load(&data_dir, &session.id).unwrap();
            let roles_and_content: Vec<(Role, String)> = saved
                .messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect();
            // 新设计：pending push 不自动落盘，jsonl 里没有未经 inject 的插队 user 条目。
            // 但 assistant 仍按 model invoke 顺序被切成多段写入，证明 in-memory drain 正常工作。
            assert_eq!(
                roles_and_content,
                vec![
                    (Role::User, "第一条".to_string()),
                    (Role::Assistant, "正在输出".to_string()),
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
                },
                |_| {},
                move |_provider, _model, _reasoning| {
                    Ok(Arc::new(PendingInputDuringDoneClient {
                        calls: AtomicUsize::new(0),
                        pending_inputs: pending_for_client.clone(),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "第二段");
            let saved = sessions::load(&data_dir, &session.id).unwrap();
            let roles_and_content: Vec<(Role, String)> = saved
                .messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect();
            // 新设计：pending push 路径不再自动落盘 user 插队条目。
            // assistant 仍按 turn 切成两段写入——证 in-memory drain 让 model 在 turn 2 看到了插队。
            assert_eq!(
                roles_and_content,
                vec![
                    (Role::User, "第一条".to_string()),
                    (Role::Assistant, "第一段".to_string()),
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
                },
                |_| {},
                move |_provider, _model, _reasoning| {
                    Ok(Arc::new(PendingInputThenToolLoopClient {
                        calls: AtomicUsize::new(0),
                        pending_inputs: pending_for_client.clone(),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "通知后工具通知后结束");
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
                },
                |_| {},
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
                },
                |_| {},
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
