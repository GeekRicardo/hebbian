//! Daemon 主体：启动后持守一个 Unix socket，接受 IPC 命令，同时通过 surface-session 驱动共享 core。
//!
//! 事件输出：全部以 NDJSON 行写到 stdout，AI 调试工具可直接 tail 读取。
//! IPC 通信：每条连接读一行 JSON → 执行 → 回一行 JSON → 断开。

use std::path::PathBuf;
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;

use agent_core::{
    permissions::PermissionStore,
    run_mode::RunMode,
    storage::{sessions, sessions_dir},
};
use anyhow::{anyhow, Result};
use model_gateway::config as providers;
use protocol::{ApprovalDecision, PermissionScope, UserAnswer, WireEvent};
use surface_session::{RuntimeRegistry, SurfaceHooks, TurnInput, TurnStatus};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

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

struct DaemonState {
    session_id: String,
    data_dir: PathBuf,
    runtime: Arc<surface_session::SessionRuntime>,
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
        emit_event(event);
    }
}

fn emit_event(event: &DaemonEvent) {
    if let Ok(line) = serde_json::to_string(event) {
        println!("{line}");
    }
}

fn emit_started(session_id: &str) {
    emit_event(&DaemonEvent::Started {
        session_id: session_id.to_string(),
    });
}

#[derive(Default)]
struct UsageTotals {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
}

fn translate_wire_event(event: &WireEvent, usage: &mut UsageTotals) -> Option<DaemonEvent> {
    match event {
        WireEvent::TextDelta {
            text,
            subagent_call_id,
        } => Some(DaemonEvent::TextDelta {
            text: text.clone(),
            subagent_call_id: subagent_call_id.clone(),
        }),
        WireEvent::TextDone {
            full_text,
            subagent_call_id,
        } => Some(DaemonEvent::TextDone {
            full_text: full_text.clone(),
            subagent_call_id: subagent_call_id.clone(),
        }),
        WireEvent::Reasoning {
            text,
            subagent_call_id,
        } => Some(DaemonEvent::Reasoning {
            text: text.clone(),
            subagent_call_id: subagent_call_id.clone(),
        }),
        WireEvent::ToolStart {
            id,
            name,
            input,
            subagent_call_id,
            ..
        } => Some(DaemonEvent::ToolStart {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
            subagent_call_id: subagent_call_id.clone(),
        }),
        WireEvent::ToolOutputDelta {
            id,
            chunk,
            subagent_call_id,
            ..
        } => Some(DaemonEvent::ToolOutputDelta {
            id: id.clone(),
            chunk: chunk.clone(),
            subagent_call_id: subagent_call_id.clone(),
        }),
        WireEvent::ToolDone {
            id,
            result,
            duration_ms,
            is_error,
            subagent_call_id,
            ..
        } => Some(DaemonEvent::ToolDone {
            id: id.clone(),
            result: result.chars().take(500).collect(),
            duration_ms: *duration_ms,
            is_error: *is_error,
            subagent_call_id: subagent_call_id.clone(),
        }),
        WireEvent::RunStarted {
            run_id,
            trigger,
            mode,
        } => Some(DaemonEvent::RunStarted {
            run_id: run_id.clone(),
            trigger: trigger.clone(),
            mode: mode.clone(),
        }),
        WireEvent::MessageAppended { message } => Some(DaemonEvent::MessageAppended {
            message: message.clone(),
        }),
        WireEvent::RunFinished { duration_ms, .. } => {
            let ev = DaemonEvent::RunFinished {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                duration_ms: *duration_ms,
            };
            *usage = UsageTotals::default();
            Some(ev)
        }
        WireEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            ..
        } => {
            usage.input_tokens += *input_tokens;
            usage.output_tokens += *output_tokens;
            usage.cache_read_tokens += *cache_read_tokens;
            None
        }
        WireEvent::RunSuspended { reason, .. } => Some(DaemonEvent::RunSuspended {
            reason: reason.clone(),
        }),
        WireEvent::RunResumed { cause, .. } => Some(DaemonEvent::RunResumed {
            cause: cause.clone(),
        }),
        WireEvent::PermissionRequested {
            request_id,
            kind,
            tool_name,
            input,
            summary,
            risk,
            paths,
            fingerprint,
            command_segments,
            auto_handled,
            call_id,
            ..
        } => Some(DaemonEvent::PermissionRequested {
            request_id: request_id.clone(),
            kind: kind.clone(),
            tool_name: tool_name.clone(),
            summary: summary.clone(),
            risk: risk.clone(),
            fingerprint: fingerprint.clone(),
            command_segments: command_segments.clone(),
            input: Some(input.clone()),
            paths: paths.clone(),
            auto_handled: *auto_handled,
            call_id: call_id.clone(),
        }),
        WireEvent::PermissionResolved {
            request_id,
            decision,
        } => Some(DaemonEvent::PermissionResolved {
            request_id: request_id.clone(),
            decision: decision.clone(),
        }),
        WireEvent::PermissionAutoJudged {
            request_id,
            tool_name,
            decision,
            reason,
            requires_human,
        } => Some(DaemonEvent::PermissionAutoJudged {
            request_id: Some(request_id.clone()).filter(|s| !s.is_empty()),
            tool_name: tool_name.clone(),
            decision: decision.clone(),
            reason: reason.clone(),
            requires_human: *requires_human,
        }),
        WireEvent::Notice {
            level,
            message,
            dedup_key,
        } => Some(DaemonEvent::Notice {
            level: level.clone(),
            message: message.clone(),
            dedup_key: dedup_key.clone(),
        }),
        WireEvent::UserQuestionRequested {
            request_id,
            question,
            options,
            multi,
            questions,
        } => Some(DaemonEvent::QuestionRequested {
            request_id: request_id.clone(),
            question: question.clone(),
            options: options.iter().cloned().map(Into::into).collect(),
            multi: *multi,
            questions: questions.iter().cloned().map(Into::into).collect(),
        }),
        WireEvent::UserQuestionAnswered { request_id, .. } => Some(DaemonEvent::QuestionAnswered {
            request_id: request_id.clone(),
        }),
        WireEvent::RunModeChanged { from, to } => Some(DaemonEvent::RunModeChanged {
            from: from.clone(),
            to: to.clone(),
        }),
        WireEvent::SessionTitleChanged { session_id, title } => {
            Some(DaemonEvent::SessionTitleChanged {
                session_id: session_id.clone(),
                title: title.clone(),
            })
        }
        WireEvent::SessionTitleGenerationFailed { session_id, reason } => {
            Some(DaemonEvent::SessionTitleGenerationFailed {
                session_id: session_id.clone(),
                reason: reason.clone(),
            })
        }
        WireEvent::RunEditsCommitted { run_id, files } => Some(DaemonEvent::RunEditsCommitted {
            run_id: run_id.clone(),
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
        WireEvent::MemoryExtracted { session_id, items } => Some(DaemonEvent::MemoryExtracted {
            session_id: session_id.clone(),
            items: items.clone(),
        }),
        WireEvent::MemoryExtractionFailed { session_id, reason } => {
            Some(DaemonEvent::MemoryExtractionFailed {
                session_id: session_id.clone(),
                reason: reason.clone(),
            })
        }
        WireEvent::Error { message } => Some(DaemonEvent::Error {
            message: message.clone(),
        }),
        _ => None,
    }
}

fn spawn_event_pump(runtime: Arc<surface_session::SessionRuntime>) {
    let mut rx = runtime.state.subscribe();
    tokio::spawn(async move {
        let mut usage = UsageTotals::default();
        loop {
            match rx.recv().await {
                Ok(envelope) => {
                    // 信封 seq 供未来 DaemonEvent::Envelope 透传（P4）；当前仍翻内层 WireEvent。
                    if let Some(ev) = translate_wire_event(&envelope.event, &mut usage) {
                        emit_event(&ev);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    emit_event(&DaemonEvent::Notice {
                        level: "warn".to_string(),
                        message: format!("事件订阅落后，已跳过 {skipped} 条事件"),
                        dedup_key: None,
                    });
                }
            }
        }
    });
}

fn cli_derived_sink() -> agent_core::agent_loop::EventSink {
    let usage = Arc::new(std::sync::Mutex::new(UsageTotals::default()));
    Arc::new(move |event| {
        if let Some(wire) = protocol::to_wire(&event) {
            if let Some(ev) = translate_wire_event(&wire, &mut usage.lock().unwrap()) {
                emit_event(&ev);
            }
        }
    })
}

fn auto_resolve_hooks(
    stats: Arc<AutoResolveStats>,
    status_tx: Arc<std::sync::Mutex<Option<oneshot::Sender<TurnStatus>>>>,
) -> SurfaceHooks {
    SurfaceHooks {
        derived_sink: Some(cli_derived_sink()),
        on_status: Some(Arc::new(move |status| {
            if let Some(tx) = status_tx.lock().unwrap().take() {
                let _ = tx.send(status);
            }
        })),
        on_permission_request: Some({
            let stats = stats.clone();
            Arc::new(move |_, _, _| {
                stats
                    .denied_approvals
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(ApprovalDecision::DenyWithFeedback {
                    feedback: "无人值守模式：该操作需要人工审批，但当前没有人能批准，已自动拒绝。请改用工作区内的安全做法，或换一种不需要审批的方式。".to_string(),
                })
            })
        }),
        on_question: Some({
            let stats = stats.clone();
            Arc::new(move |_, _, _, _, _| {
                stats
                    .cancelled_questions
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(UserAnswer::Cancelled)
            })
        }),
        ..SurfaceHooks::default()
    }
}

fn interactive_hooks() -> SurfaceHooks {
    SurfaceHooks {
        derived_sink: Some(cli_derived_sink()),
        ..SurfaceHooks::default()
    }
}

// ─── IPC 命令处理 ──────────────────────────────────────────────────────────

async fn handle_command(state: Arc<DaemonState>, cmd: IpcCommand) -> IpcResponse {
    match cmd {
        IpcCommand::Send { text } => {
            let input = TurnInput::text(text.clone()).with_hooks(interactive_hooks());
            if state.runtime.is_active() && state.runtime.inject(input.clone()) {
                IpcResponse::ok()
            } else if state.runtime.input_tx.send(input).is_ok() {
                IpcResponse::ok()
            } else {
                IpcResponse::err("daemon 输入通道已关闭")
            }
        }
        IpcCommand::Inject { text } => {
            if state.runtime.inject(TurnInput::text(text)) {
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
            if state.runtime.state.resolve_approval(&request_id, decision) {
                IpcResponse::ok()
            } else {
                IpcResponse::err(format!("未找到 request_id: {request_id}"))
            }
        }
        IpcCommand::Deny { request_id } => {
            if state
                .runtime
                .state
                .resolve_approval(&request_id, ApprovalDecision::Deny)
            {
                IpcResponse::ok()
            } else {
                IpcResponse::err(format!("未找到 request_id: {request_id}"))
            }
        }
        IpcCommand::DenyWithFeedback {
            request_id,
            feedback,
        } => {
            if state
                .runtime
                .state
                .resolve_approval(&request_id, ApprovalDecision::DenyWithFeedback { feedback })
            {
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
            if state.runtime.state.answer_question(&request_id, answer) {
                IpcResponse::ok()
            } else {
                IpcResponse::err(format!("未找到 request_id: {request_id}"))
            }
        }
        IpcCommand::Stop => {
            state.runtime.stop();
            IpcResponse::ok()
        }
        IpcCommand::Mode { mode } => match RunMode::parse(&mode) {
            Some(m) => {
                if let Err(e) = sessions::set_run_mode(&state.data_dir, &state.session_id, m) {
                    return IpcResponse::err(format!("写入 run mode 失败：{e}"));
                }
                agent_core::run_mode::LiveRunModeRegistry::global().set(&state.session_id, m);
                state.runtime.state.set_run_mode(m);
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
            sessions::load(&data_dir, &id).map_err(|e| anyhow!("session {id} 不存在：{e}"))?;
            id
        }
        None => {
            let mut session = sessions::create_with_source(
                &data_dir,
                provider_id.clone(),
                model.clone(),
                None,
                None,
                "cli".to_string(),
            )?;
            if let Some(wd) = args.workdir.clone() {
                session.workdir = Some(wd);
                session = sessions::save(&data_dir, session)?;
            }
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
    sessions::set_run_mode(&data_dir, &session_id, run_mode)?;

    Ok(PreparedSession {
        data_dir,
        session_id,
        run_mode,
    })
}

pub async fn run(args: DaemonArgs) -> Result<()> {
    let PreparedSession {
        data_dir,
        session_id,
        run_mode,
    } = prepare_session(&args)?;

    let permission_store = PermissionStore::open(&data_dir).ok().map(Arc::new);
    let runtimes = RuntimeRegistry::new();
    let runtime = runtimes
        .ensure(&data_dir, permission_store.clone(), &session_id)
        .await?;
    runtime.state.set_run_mode(run_mode);
    agent_core::run_mode::LiveRunModeRegistry::global().set(&session_id, run_mode);
    surface_session::register_wakeup_resume_handler(
        data_dir.clone(),
        permission_store.clone(),
        runtimes.clone(),
    );
    spawn_event_pump(runtime.clone());

    let socket_dir = data_dir.join("cli-sockets");
    std::fs::create_dir_all(&socket_dir)?;
    let sock_path = socket_path(&session_id);
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;

    let state = Arc::new(DaemonState {
        session_id: session_id.clone(),
        data_dir: data_dir.clone(),
        runtime,
    });

    state.emit(&DaemonEvent::RunModeChanged {
        from: "default".to_string(),
        to: run_mode.as_str().to_string(),
    });
    emit_started(&session_id);

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let s = state.clone();
                tokio::spawn(handle_connection(stream, s));
            }
            Err(e) => {
                tracing::error!(error = %e, "Unix socket accept 失败");
                break;
            }
        }
    }

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
    let runtimes = RuntimeRegistry::new();
    let runtime = runtimes
        .ensure(&data_dir, permission_store.clone(), &session_id)
        .await?;
    runtime.state.set_run_mode(prepared.run_mode);
    agent_core::run_mode::LiveRunModeRegistry::global().set(&session_id, prepared.run_mode);
    surface_session::register_wakeup_resume_handler(data_dir.clone(), permission_store, runtimes);
    spawn_event_pump(runtime.clone());
    emit_started(&session_id);

    let auto_resolve = Arc::new(AutoResolveStats::default());
    let (status_tx, status_rx) = oneshot::channel();
    let status_tx = Arc::new(std::sync::Mutex::new(Some(status_tx)));
    let input = TurnInput::text(task)
        .with_hooks(auto_resolve_hooks(auto_resolve.clone(), status_tx.clone()));

    let started = std::time::Instant::now();
    runtime
        .input_tx
        .send(input)
        .map_err(|_| anyhow!("daemon 输入通道已关闭"))?;
    let wait_status = async { status_rx.await.map_err(|_| anyhow!("run 状态通道已关闭")) };
    let (outcome, timed_out) = match timeout_secs {
        Some(secs) => match tokio::time::timeout(Duration::from_secs(secs), wait_status).await {
            Ok(status) => (status?, false),
            Err(_) => {
                runtime.stop();
                (TurnStatus::Cancelled, true)
            }
        },
        None => (wait_status.await?, false),
    };

    let exit_code = match &outcome {
        TurnStatus::Done | TurnStatus::Suspended => 0,
        TurnStatus::Failed(_) => 1,
        TurnStatus::Cancelled if timed_out => 2,
        TurnStatus::Cancelled => 130,
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
        let denied = auto_resolve.denied_approvals.load(Ordering::Relaxed);
        let cancelled = auto_resolve.cancelled_questions.load(Ordering::Relaxed);
        let outcome_label = match &outcome {
            TurnStatus::Done => "完成",
            TurnStatus::Suspended => "挂起（等待后台任务）",
            TurnStatus::Failed(_) => "失败",
            TurnStatus::Cancelled if timed_out => "超时中断",
            TurnStatus::Cancelled => "已取消",
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
    outcome: &TurnStatus,
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
                .find(|m| matches!(m.role, sessions::Role::Assistant))
                .map(|m| (m.content.clone(), m.tool_calls.len()))
        })
        .unwrap_or_default();

    let files_changed = read_run_edits_files(data_dir, session_id);

    let outcome_label = match outcome {
        TurnStatus::Done => "done",
        TurnStatus::Suspended => "suspended",
        TurnStatus::Failed(_) => "failed",
        TurnStatus::Cancelled => "cancelled",
    };
    let error = match outcome {
        TurnStatus::Failed(e) => Some(e.clone()),
        _ => None,
    };

    serde_json::json!({
        "session_id": session_id,
        "outcome": outcome_label,
        "exit_code": exit_code,
        "final_text": final_text,
        "tool_calls": tool_calls,
        "files_changed": files_changed,
        "denied_approvals": auto_resolve.denied_approvals.load(Ordering::Relaxed),
        "cancelled_questions": auto_resolve.cancelled_questions.load(Ordering::Relaxed),
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

    /// 步骤4 收口回归（架构 §3.1.1）：cli 的 DaemonEvent 业务事件字段必须复用 protocol 的
    /// 集中 mapper，与 desktop/web 的 to_wire 输出逐字段一致。
    #[test]
    fn daemon_event_risk_and_reason_match_wire_canonical_form() {
        let wire = WireEvent::PermissionRequested {
            request_id: "r1".into(),
            kind: "tool_call".into(),
            tool_name: "Write".into(),
            input: serde_json::json!({"file_path": "/tmp/x"}),
            summary: "写文件".into(),
            risk: "critical".into(),
            paths: vec![],
            fingerprint: None,
            command_segments: vec![],
            segments: vec![],
            refuse_remember: false,
            plan: None,
            auto_handled: false,
            call_id: "c1".into(),
        };
        let mut usage = UsageTotals::default();
        let de = translate_wire_event(&wire, &mut usage).expect("permission_requested 应翻译");
        let json = serde_json::to_value(&de).unwrap();
        assert_eq!(
            json["risk"], "critical",
            "risk 必须是小写规范形态，与 to_wire 一致"
        );
        assert_eq!(json["event"], "permission_requested");

        let wire = WireEvent::RunSuspended {
            reason: "cron".into(),
            resumes_at_ms: None,
            waiting_for_task_ids: vec![],
        };
        let de = translate_wire_event(&wire, &mut usage).expect("run_suspended 应翻译");
        let json = serde_json::to_value(&de).unwrap();
        assert_eq!(
            json["reason"], "cron",
            "suspend reason 必须走 protocol mapper，与 to_wire 一致"
        );
    }
}
