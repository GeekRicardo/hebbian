use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc, Mutex};

use async_trait::async_trait;
use protocol::{
    AgentRef, ApprovalDecision, AskQuestion, Event, EventPayload, Op, PermissionKind,
    PermissionRequestId, QuestionOption, RunId, Submission, SubmissionId, UserAnswer,
};
use tokio::sync::mpsc;
use tracing::warn;

use crate::{
    agent_loop::{self, EventSink, LoopParams},
    context::transcript::Transcript,
    definition::CompactionPolicy,
    hooks::HookManager,
    model_io_dump::ModelIoDump,
    recorder::Recorder,
    run_state::RunState,
    tools::{hitl::HitlGate, registry::ToolRegistry, Tool},
    workspace::Workspace,
};
use common::{
    runtime::{ConsumedPendingInputs, PendingInputs},
    CancelFlag,
};
use model_gateway::client::ModelClient;

/// 注册表里登记一次 run 的运行时控制点（供跨进程 `Op::Approve` / `Op::Interrupt` /
/// `Op::SwitchRunMode` 反查）。
struct RunRegistration {
    hitl: Arc<HitlGate>,
    cancel: CancelFlag,
    /// run 的事件 sink（克隆 from spawn_run），actor 处理 `Op::SwitchRunMode` 时
    /// 用来 emit `RunModeChanged`。
    sink: EventSink,
    state: Arc<RunState>,
    /// 当前 run mode 的共享视图。`Op::SwitchRunMode` 写入这里供 dispatcher
    /// 下次循环读取（本期 actor 仅 emit 事件，不真切运行时 mode——架构 §13 留尾巴）。
    run_mode: Arc<Mutex<crate::run_mode::RunMode>>,
}

/// 启动一次 run 所需的全部上下文。
pub struct RunParams {
    pub agent: AgentRef,
    pub hitl: Arc<HitlGate>,
    /// 调用方组装好的完整 transcript（含 system + 历史 + 当前 user message）
    pub transcript: Transcript,
    pub enabled_tools: Vec<String>,
    pub compaction_policy: CompactionPolicy,
    /// 本对话的 workspace（workdir + allowed_paths）。每个对话独立。
    pub workspace: Arc<Workspace>,
    pub stream: bool,
    pub cancel: CancelFlag,
    pub parent: Option<RunId>,
    /// 可选的事件持久化。给定后所有事件 fire-and-forget 追加进 jsonl。
    pub recorder: Option<Recorder>,
    /// 可选的模型 IO dump：每次 model 调用前后写一条 `{request, response}` 到 jsonl。
    /// 由环境变量 `HEBBIAN_DUMP_MODEL_IO` 触发，surface 决定路径。
    pub model_io_dump: Option<ModelIoDump>,
    /// 本 run 的模型调用 tag（§4.11）。主对话 Main；aside / subagent 由创建方显式传，
    /// 让 `[model]` 日志 + model_io 落盘据此区分。
    pub call_tag: model_gateway::types::ModelCallTag,
    /// 运行时输入注入队列：surface 在 streaming 中「立即发送」时把 user message 推进来，
    /// agent_loop 每次 model.request 之前 drain 出来加入 transcript。`None` 表示禁用。
    pub pending_inputs: Option<PendingInputs>,
    /// 已被 agent_loop drain 的 pending input 副本。surface 可在 run 结束后按顺序落盘。
    pub consumed_pending_inputs: Option<ConsumedPendingInputs>,
    /// run 结束前为 true；terminal/suspended 后由 agent_loop 置 false。
    pub pending_inputs_accepting: Option<Arc<AtomicBool>>,
    /// 运行模式（架构 §4.4.3）。默认 [`crate::run_mode::RunMode::Default`]。
    pub run_mode: crate::run_mode::RunMode,
    /// 当前会话使用的模型 id（如 `"claude-opus-4-7"`）。AutoMode judge 用作模型限定。
    pub model_id: Option<String>,
    /// `force_automode` 子开关（架构 §4.4.4）。仅 AutoMode 下生效：判官 Ask 折叠为 Deny。
    pub force_automode: bool,
    /// 数据目录路径。microcompact 把被压缩的 tool result 落到
    /// `<data_dir>/sessions/<sid>/tool_results/<call_id>.txt`。
    pub data_dir: Option<std::path::PathBuf>,
    /// 会话 id（格式 `{yyyymmddHHmm}-{shortUuid}`）。配合 `data_dir` 用于工件落盘路径。
    pub session_id: Option<String>,
    /// 挂起请求通道（架构 §4.12.4）。Surface 与本 Run 关联的挂起工具共享同一个
    /// channel，由 `default_tools` 构造时传给 ScheduleWakeup。`None` 表示当前会话禁用挂起。
    pub phase: Option<crate::wakeup::PhaseChannel>,
    /// 从挂起态恢复时携带：agent_loop 用它初始化计数器 + emit `RunResumed`
    /// 而不是 `RunStarted`（架构 §4.12.6）。
    pub resume_from: Option<crate::agent_loop::RunResumeState>,
    /// Edit 工具快照仓库（架构 §4.13）。`None` 时跳过快照。
    pub edits_worktree: Option<Arc<crate::edits::EditsWorktree>>,
    /// 工具迭代次数上限。`None` 表示不限制。
    pub max_tool_iterations: Option<u32>,
    /// 规则文件渲染后的 `<system-reminder>` 块，追加到 system prompt 末尾。
    /// 由 Session 在 spawn_run 前解析，注入 system 段保证每轮都可见。
    pub system_rules: Option<String>,
    /// Subagent / NestedRun 上下文（架构 §4.4.11）。`Some` 让 ToolDispatcher 把
    /// `Task` 工具路由到 [`crate::subagent::SubagentRunner`]。Session 在 spawn_run
    /// 前按当前 workspace 加载可用 subagent 定义构造；CLI 单跑 / 单测填 None。
    pub subagent_ctx: Option<Arc<crate::subagent::SubagentCtx>>,
    /// 派生事件旁路（架构 §4.14.7）。标题 / 记忆等 detached task 在 `RunFinished`
    /// **之后**才完成，其事件若走 run 级 mpsc 会被 `drain_trailing_events` 的 trailing
    /// window 卡死丢失。给定后这些事件改走本 sink——一条独立于单个 run 生命周期、由
    /// surface 接到自身 long-lived 出口（Desktop 的 `app.emit` 全局总线）的通道。
    /// `None` 时回退 run 级 sink（未接入的 surface 行为不变）。
    pub derived_sink: Option<EventSink>,
}

/// Core 对外门面。
///
/// `spawn_run` 返回 [`RunHandle`]，在它上面 `recv()` 拿事件、调 `resolve_permission` /
/// `answer_question` / `interrupt` 控制 run。每个 run 独享一条 mpsc，事件按时间顺序到达，
/// 不需要按 `run_id` 过滤。
///
/// 跨进程协议入口走 [`Harness::submit`]：actor 处理 `Op::Approve / AnswerQuestion / Interrupt`。
pub struct Harness {
    registry: Arc<ToolRegistry>,
    hooks: Arc<HookManager>,
    runs: Arc<Mutex<HashMap<RunId, Arc<RunRegistration>>>>,
    submit_tx: mpsc::Sender<Submission>,
}

impl Harness {
    /// 暴露 HookManager 引用给 Session 等下游，让它们在自己的生命周期点 trigger
    /// SessionStart / UserPromptSubmit / SessionEnd 等外部 hook 点位。
    pub fn hooks(&self) -> Arc<HookManager> {
        self.hooks.clone()
    }

    pub fn new(tools: Vec<Box<dyn Tool>>, hooks: HookManager) -> Self {
        let (submit_tx, submit_rx) = mpsc::channel::<Submission>(1024);

        let harness = Self {
            registry: Arc::new(ToolRegistry::new(tools)),
            hooks: Arc::new(hooks),
            runs: Arc::new(Mutex::new(HashMap::new())),
            submit_tx,
        };

        let runs = harness.runs.clone();
        tokio::spawn(async move {
            run_actor_loop(submit_rx, runs).await;
        });

        harness
    }

    /// 启动一个 run，立即返回独享句柄。
    pub fn spawn_run(&self, client: Arc<dyn ModelClient>, params: RunParams) -> RunHandle {
        let run_id = RunId::new();
        let state = Arc::new(RunState::new(run_id.clone()));
        let run_mode_shared = Arc::new(Mutex::new(params.run_mode));

        // 注册到全局 LiveRunModeRegistry，让 surface 的 set_run_mode 能即时更新运行中的 run。
        if let Some(sid) = &params.session_id {
            crate::run_mode::LiveRunModeRegistry::global()
                .register(sid.clone(), run_mode_shared.clone());
            // 新 Run 启动 = 用户有动作，取消该 session 待触发的 idle 深睡哨兵（架构 §3.1：
            // 「计时器重置」）。本 Run 结束后浅睡抽取再重新 arm。
            crate::wakeup::WakeupScheduler::global().cancel_idle(sid);
        }

        // 双路 sink：本 run 独享 mpsc + 可选 jsonl 持久化。
        //
        // 通道改为 bounded(1024)。sink 是同步闭包（type EventSink），不能 await，
        // 因此先用 `try_send` 保持事件顺序；只有关键事件遇到满队列时才 spawn 一个
        // 极短 task 等空位，避免生命周期 / HITL 事件丢失。
        let (run_tx, run_rx) = mpsc::channel::<Event>(1024);
        let recorder = params.recorder.clone();
        // Run 落盘协调器（架构 §4.9.5）：data_dir + session_id 都给定时构造。本体随 run task
        // 移进 LoopParams.persister 在 agent_loop 主体单点落盘；handle（sink 端 clone）插进
        // core_sink，对每个 Event 做纯内存累积 + partial 写帧。`None` 时整条落盘链跳过。
        let persister = match (&params.data_dir, &params.session_id) {
            (Some(dd), Some(sid)) => Some(crate::run_persister::RunPersister::new(
                dd.clone(),
                sid.clone(),
            )),
            _ => None,
        };
        let persister_handle = persister.as_ref().map(|p| p.handle());
        let last_message_handle = persister
            .as_ref()
            .map(|p| p.last_message_handle())
            .unwrap_or_default();
        let core_sink: EventSink = Arc::new(move |event: Event| {
            if let Some(rec) = &recorder {
                rec.write(&event);
            }
            if let Some(h) = &persister_handle {
                h.observe(&event);
            }
            if let Err(e) = run_tx.try_send(event) {
                let error_label = e.to_string();
                let event = e.into_inner();
                if is_critical_event(&event.payload) {
                    let tx = run_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = tx.send(event).await {
                            tracing::warn!(error = %e, "run event channel closed while sending critical event");
                        }
                    });
                } else {
                    tracing::warn!(error = %error_label, "run event channel full, dropping non-critical event");
                }
            }
        });

        // 标题生成挂钩（架构 §4.9.x）：本 Run 内首次看到 TurnFinished 时异步 spawn 一个
        // 短调用 task，调 session_titler::generate_for_session。task 内部判断 session
        // 当前 title 是否仍是默认值（"新对话"）—— 是才生成 / 落盘 / emit 事件，从而保证
        // 用户已重命名 / fork / resume 等场景下不会被自动覆盖。
        //
        // 事件 emit 走同一份 sink。task 持有 sink 的 Arc，确保通道 send 端在 task 完成前
        // 不被 drop；接收端由 surface 通过 [`RunHandle::drive`] 的 trailing window 消费。
        let title_triggered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let title_data_dir = params.data_dir.clone();
        let title_session_id = params.session_id.clone();
        let title_state = state.clone();
        // 派生事件优先走旁路 sink（§4.14.7）：标题 / 记忆在 run 收尾后才完成，走 run 级
        // sink 会被 trailing window 关掉的通道丢弃。derived_sink 由 surface 接到 long-lived
        // 出口；缺省回退 core_sink（run 级，未接入 surface 行为不变）。
        let derived_sink = params
            .derived_sink
            .clone()
            .unwrap_or_else(|| core_sink.clone());
        let title_sink = derived_sink.clone();
        // 记忆抽取挂钩（架构 §4.14）：本 Run 的 agent_loop 跑完（RunFinished）后异步 spawn
        // 一个 task 调 memory_extract::extract_for_session。一个 Run = 用户语义的「一个 turn
        // 结束」，故用 RunFinished 而非 TurnFinished（后者一个 Run 内会出现多次）。
        // 成功 emit MemoryExtracted（surface 渲染「本轮写入 N 条」），fallback 链全失败
        // emit MemoryExtractionFailed（surface 弹 toast）；游标由抽取内部按成败推进 / 保留。
        let mem_data_dir = params.data_dir.clone();
        let mem_session_id = params.session_id.clone();
        let mem_state = state.clone();
        let mem_sink = derived_sink.clone();
        let sink: EventSink = Arc::new(move |event: Event| {
            let trigger_title = matches!(event.payload, EventPayload::TurnFinished { .. })
                && title_data_dir.is_some()
                && title_session_id.is_some()
                && !title_triggered.swap(true, std::sync::atomic::Ordering::SeqCst);
            let trigger_memory = matches!(event.payload, EventPayload::RunFinished { .. })
                && mem_data_dir.is_some()
                && mem_session_id.is_some();
            core_sink(event);
            if trigger_title {
                let dd = title_data_dir.clone().unwrap();
                let sid = title_session_id.clone().unwrap();
                let task_state = title_state.clone();
                let task_sink = title_sink.clone();
                tokio::spawn(async move {
                    use crate::session_titler::TitleOutcome;
                    match crate::session_titler::generate_for_session(&dd, &sid).await {
                        TitleOutcome::Generated(title) => {
                            let ev = task_state.event(EventPayload::SessionTitleChanged {
                                session_id: sid,
                                title,
                            });
                            task_sink(ev);
                        }
                        TitleOutcome::Failed(reason) => {
                            let ev = task_state.event(EventPayload::SessionTitleGenerationFailed {
                                session_id: sid,
                                reason,
                            });
                            task_sink(ev);
                        }
                        TitleOutcome::Skipped => {}
                    }
                });
            }
            if trigger_memory {
                let dd = mem_data_dir.clone().unwrap();
                let sid = mem_session_id.clone().unwrap();
                let task_state = mem_state.clone();
                let task_sink = mem_sink.clone();
                tokio::spawn(async move {
                    emit_memory_extraction(&dd, &sid, &task_state, &task_sink).await;
                    // 浅睡抽完后挂 idle 哨兵（架构 §3.1）：空闲 T 分钟没新输入就深睡整理。
                    // 覆盖式 arm——同 session 上一个哨兵被清，实现「每次 Run 结束重排计时器」，
                    // 连续干活永不触发。T 来自设置（idle_consolidate_minutes，默认 10，0=关）。
                    arm_idle_after_run(&dd, &sid);
                });
            }
        });

        // 注册到全局 runs 表，让 actor 能反查（处理 Op::Approve / Op::Interrupt / Op::SwitchRunMode）。
        self.runs.lock().unwrap().insert(
            run_id.clone(),
            Arc::new(RunRegistration {
                hitl: params.hitl.clone(),
                cancel: params.cancel.clone(),
                sink: sink.clone(),
                state: state.clone(),
                run_mode: run_mode_shared.clone(),
            }),
        );

        let registry = self.registry.clone();
        let hooks = self.hooks.clone();
        let runs = self.runs.clone();
        let run_id_for_task = run_id.clone();
        let RunParams {
            agent,
            hitl,
            mut transcript,
            enabled_tools,
            compaction_policy,
            workspace,
            stream,
            cancel,
            parent,
            recorder: _,
            model_io_dump,
            call_tag,
            pending_inputs,
            consumed_pending_inputs,
            pending_inputs_accepting,
            run_mode: _,
            model_id,
            force_automode,
            data_dir,
            session_id: loop_session_id,
            phase,
            resume_from,
            edits_worktree,
            max_tool_iterations,
            system_rules,
            subagent_ctx,
            derived_sink: _,
        } = params;

        let hitl_for_handle = hitl.clone();
        let cancel_for_handle = cancel.clone();
        let judge_client = client.clone();
        let session_id_for_cleanup = loop_session_id.clone();

        // force_automode（hands-off「全自动」）共享句柄：与 run_mode_shared 对称，注册到
        // 全局表让 surface 的 set_force_automode 能在 run 中途即时改值（架构 §4.4.4）。
        let force_automode_shared: crate::run_mode::SharedForceAutomode =
            Arc::new(std::sync::atomic::AtomicBool::new(force_automode));
        if let Some(sid) = &session_id_for_cleanup {
            crate::run_mode::LiveForceAutomodeRegistry::global()
                .register(sid.clone(), force_automode_shared.clone());
        }

        tokio::spawn(async move {
            let params = LoopParams {
                client: client.as_ref(),
                registry,
                hitl,
                hooks,
                transcript: &mut transcript,
                enabled_tools: &enabled_tools,
                compaction_policy: &compaction_policy,
                workspace,
                stream,
                cancel,
                state,
                agent,
                parent,
                model_io_dump,
                call_tag,
                pending_inputs,
                consumed_pending_inputs,
                pending_inputs_accepting,
                run_mode: run_mode_shared.clone(),
                model_id,
                judge_client: Some(judge_client),
                force_automode: force_automode_shared,
                data_dir,
                session_id: loop_session_id,
                phase,
                resume_from,
                edits_worktree,
                max_tool_iterations,
                system_rules,
                subagent_ctx,
                subagent_bypass: false,
                persister,
            };
            if let Err(e) = agent_loop::run_loop(params, sink).await {
                warn!(error = %e, "run failed");
            }
            runs.lock().unwrap().remove(&run_id_for_task);
            if let Some(sid) = &session_id_for_cleanup {
                crate::run_mode::LiveRunModeRegistry::global().unregister(sid);
            }
        });

        RunHandle {
            run_id,
            events: run_rx,
            hitl: hitl_for_handle,
            cancel: cancel_for_handle,
            last_message: last_message_handle,
        }
    }

    /// 投递一个协议指令。当前 actor 处理 `Approve` / `AnswerQuestion` / `Interrupt`。
    /// `StartRun` 等需要 surface 自行解析后调 `spawn_run`。
    ///
    /// 满载时返回 `Closed`——submit 通道承载控制信号（审批/取消/回答），
    /// 满载意味着 actor 严重落后，调用方应自行回退或重试。
    pub fn submit(&self, submission: Submission) -> Result<SubmissionId, HarnessError> {
        let id = submission.id.clone();
        self.submit_tx
            .try_send(submission)
            .map_err(|_| HarnessError::Closed)?;
        Ok(id)
    }
}

/// 一次 run 的本地句柄：独享事件流 + 控制方法。
///
/// 由 `Harness::spawn_run` 返回，在 `recv()` 上消费事件直到 `RunFinished` /
/// `RunFailed` / `RunCancelled`。
pub struct RunHandle {
    run_id: RunId,
    events: mpsc::Receiver<Event>,
    hitl: Arc<HitlGate>,
    cancel: CancelFlag,
    /// 本 run 最后落盘 assistant message 的只读句柄（架构 §7.8.3）。`drive` 在收到
    /// `RunFinished` 时读出填进 [`TurnSummary::last_message`]，surface 不再自行累积。
    last_message: crate::run_persister::LastMessageHandle,
}

impl RunHandle {
    pub fn id(&self) -> &RunId {
        &self.run_id
    }

    /// 拉取下一个事件。run 结束（task drop sink）后返回 `None`。
    pub async fn recv(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    pub fn resolve_permission(&self, request_id: &PermissionRequestId, decision: ApprovalDecision) {
        self.hitl.resolve(request_id, decision);
    }

    pub fn answer_question(&self, request_id: &PermissionRequestId, answer: UserAnswer) {
        self.hitl.answer(request_id, answer);
    }

    pub fn interrupt(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        self.hitl.cancel_all_pending();
    }

    /// 暴露 hitl 给 surface 注册到全局桥接（如 Desktop 的 `HitlState`）。
    pub fn hitl(&self) -> &Arc<HitlGate> {
        &self.hitl
    }

    /// 把事件循环的全部样板（recv + filter + 终止判定 + HITL 路由）交给 driver，
    /// surface 只实现 [`TurnObserver`]：渲染事件 + 给 HITL 回应。
    ///
    /// **trailing 事件窗口**：收到 terminal 事件（RunFinished/Failed/Cancelled）后，
    /// 本方法不会立即返回，而是继续 `recv` 直到事件通道关闭或最多 5 秒。这给
    /// `SessionTitleChanged` 等「主流程结束后才完成的后台 task」一个送达窗口。
    /// 正常情况下 channel 在 task 完成后立即关闭，不会真的等满 5 秒。
    pub async fn drive<O: TurnObserver>(&mut self, observer: &mut O) -> TurnSummary {
        let summary = loop {
            let Some(event) = self.recv().await else {
                return TurnSummary::failed("事件流意外关闭");
            };
            if self.cancelled_hitl_request(&event.payload) {
                self.hitl.cancel_all_pending();
                // 仍推给 observer，让前端能收到 PermissionRequested / UserQuestionRequested
                // 事件以及后续的 TurnFinished / RunCancelled，避免前端残留悬空状态。
                observer.on_event(&event);
                continue;
            }
            if self.is_stale_hitl_request(&event.payload) {
                continue;
            }
            observer.on_event(&event);

            // 子 NestedRun（subagent）的 Run* 生命周期事件经装饰器转发进父 sink，带
            // subagent_call_id=Some(parent_task_call_id)。这些事件已交给 observer 做
            // nested 累积 / 渲染，但**不能**参与父 turn 的终态判定——否则第一个并发子
            // emit 的 RunFinished 会被误当成父 Run 结束，提前 break，把其它并发子和父
            // agent_loop 一起丢掉（架构 §4.4.11.8）。子的生命周期对父 driver 透明。
            //
            // 注意只跳过子的 Run* 事件：子的 HITL 事件（PermissionRequested /
            // UserQuestionRequested）也带 subagent_call_id，但它们必须走下面的 match →
            // on_permission_request / on_question，让 surface 把 request_id 注册进 HitlState，
            // 否则前端审批回流时按 request_id 找不到 gate → 报「审批回应失败」。
            if event.subagent_call_id.is_some() && is_run_lifecycle_event(&event.payload) {
                continue;
            }

            match &event.payload {
                EventPayload::PermissionRequested {
                    request_id,
                    kind,
                    summary,
                    ..
                } => {
                    if let Some(decision) = observer
                        .on_permission_request(request_id, kind, summary)
                        .await
                    {
                        self.resolve_permission(request_id, decision);
                    }
                }
                EventPayload::UserQuestionRequested {
                    request_id,
                    question,
                    options,
                    multi,
                    questions,
                } => {
                    if let Some(answer) = observer
                        .on_question(request_id, question, options, *multi, questions)
                        .await
                    {
                        self.answer_question(request_id, answer);
                    }
                }
                EventPayload::RunFinished {
                    total_input_tokens,
                    total_output_tokens,
                    total_cache_read_tokens,
                    total_cache_creation_tokens,
                    duration_ms,
                } => {
                    break TurnSummary {
                        outcome: TurnOutcome::Done,
                        usage: Some(UsageTotals {
                            input: *total_input_tokens,
                            output: *total_output_tokens,
                            cache_read: *total_cache_read_tokens,
                            cache_creation: *total_cache_creation_tokens,
                        }),
                        duration_ms: Some(*duration_ms),
                        // RunFinished emit 于 agent_loop 收尾 persister.finish() 之后（同 task
                        // 先 finish 落盘再 emit），故此刻最后落盘段已就绪可读（架构 §7.8.3）。
                        last_message: self.last_message.get(),
                    };
                }
                EventPayload::RunFailed { error } => {
                    break TurnSummary::failed(&error.message);
                }
                EventPayload::RunCancelled => {
                    break TurnSummary {
                        outcome: TurnOutcome::Cancelled,
                        usage: None,
                        duration_ms: None,
                        last_message: None,
                    };
                }
                // 架构 §4.12.1 / §4.12.5：Suspended 是 Run 的合法中间态——agent_loop
                // 已经 emit TurnFinished(EndTurn) 收尾，但不会发 RunFinished（等 wakeup
                // 再 resume）。channel 在 task 退出后会关闭，driver 必须把 RunSuspended
                // 视为终态收 turn，否则下面的 `recv` 拿到 None → 误报"事件流意外关闭"。
                //
                // usage 字段不在本 outcome 暴露：token 总数已经写进 RunCheckpoint，
                // 等 wakeup resume 时由 agent_loop 继续累；如果在这里也吐一份会让
                // surface 把同一段消耗重复累进 session.token_stats。
                EventPayload::RunSuspended { .. } => {
                    break TurnSummary {
                        outcome: TurnOutcome::Suspended,
                        usage: None,
                        duration_ms: None,
                        last_message: None,
                    };
                }
                _ => {}
            }
        };

        self.drain_trailing_events(observer, std::time::Duration::from_secs(5))
            .await;
        summary
    }

    fn cancelled_hitl_request(&self, payload: &EventPayload) -> bool {
        matches!(
            payload,
            EventPayload::PermissionRequested { .. } | EventPayload::UserQuestionRequested { .. }
        ) && self.cancel.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn is_stale_hitl_request(&self, payload: &EventPayload) -> bool {
        match payload {
            EventPayload::PermissionRequested { request_id, .. }
            | EventPayload::UserQuestionRequested { request_id, .. } => {
                !self.hitl.is_pending(request_id)
            }
            _ => false,
        }
    }

    /// terminal 事件之后继续 recv 直到通道关闭或超时——把可能晚到的 trailing 事件
    /// （如 [`EventPayload::SessionTitleChanged`]）转给 observer。
    async fn drain_trailing_events<O: TurnObserver>(
        &mut self,
        observer: &mut O,
        timeout: std::time::Duration,
    ) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline - now;
            match tokio::time::timeout(remaining, self.events.recv()).await {
                Ok(Some(event)) => {
                    if self.cancelled_hitl_request(&event.payload) {
                        self.hitl.cancel_all_pending();
                    } else if !self.is_stale_hitl_request(&event.payload) {
                        observer.on_event(&event);
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    }
}

/// Surface 接入 [`RunHandle::drive`] 的统一回调点。
///
/// `on_permission_request` / `on_question` 返回 `Some(decision)` 由 driver 自动 resolve，
/// 返回 `None` 表示 surface 自己异步处理（如 Desktop 通过 Tauri command 链路）。
#[async_trait]
pub trait TurnObserver: Send {
    /// 任意事件的渲染 / 累积。终止事件（RunFinished/Failed/Cancelled）也会先回调一次。
    fn on_event(&mut self, event: &Event);

    async fn on_permission_request(
        &mut self,
        request_id: &PermissionRequestId,
        kind: &PermissionKind,
        summary: &str,
    ) -> Option<ApprovalDecision>;

    async fn on_question(
        &mut self,
        request_id: &PermissionRequestId,
        question: &str,
        options: &[QuestionOption],
        multi: bool,
        questions: &[AskQuestion],
    ) -> Option<UserAnswer>;
}

/// 一次 run 跑完后的总结。
#[derive(Debug, Clone)]
pub struct TurnSummary {
    pub outcome: TurnOutcome,
    pub usage: Option<UsageTotals>,
    /// 本 Run 总耗时（毫秒），取自 `RunFinished`。surface 落盘时写进本轮最后一条
    /// assistant message 的 `run_duration_ms`。仅 `Done` 有值；其它 outcome 为 `None`。
    pub duration_ms: Option<u64>,
    /// 本 Run 最后落盘的 assistant message（架构 §7.8.3）：assistant 累积 + 落盘已
    /// 收归 agent_core 唯一一份，surface 不再自行累积，收尾返回值直接取此。仅 `Done`
    /// 时由 `drive` 从 [`RunHandle::last_message`] 读出；其它 outcome 为 `None`。
    pub last_message: Option<crate::storage::sessions::Message>,
}

#[derive(Debug, Clone)]
pub enum TurnOutcome {
    Done,
    Failed(String),
    Cancelled,
    /// Run 进入 Suspended 中间态（架构 §4.12.1）——agent_loop 已 emit RunSuspended +
    /// 落 RunCheckpoint，等 WakeupScheduler 唤醒后再 resume。surface 处理上跟 Done
    /// 几乎一致（要把当前累积的 assistant 段落盘到 jsonl，因为 transcript 不进
    /// checkpoint，resume 时从 jsonl 重建——§4.12.3）；区别是**不报错、不结束 run**。
    Suspended,
}

#[derive(Debug, Clone, Copy)]
pub struct UsageTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TurnSummary {
    fn failed(msg: &str) -> Self {
        Self {
            outcome: TurnOutcome::Failed(msg.to_string()),
            usage: None,
            duration_ms: None,
            last_message: None,
        }
    }
}

async fn run_actor_loop(
    mut submit_rx: mpsc::Receiver<Submission>,
    runs: Arc<Mutex<HashMap<RunId, Arc<RunRegistration>>>>,
) {
    while let Some(submission) = submit_rx.recv().await {
        match submission.op {
            Op::Approve {
                request_id,
                decision,
            } => {
                let entries: Vec<_> = runs.lock().unwrap().values().cloned().collect();
                for entry in entries {
                    entry.hitl.resolve(&request_id, decision.clone());
                }
            }
            Op::AnswerQuestion { request_id, answer } => {
                let entries: Vec<_> = runs.lock().unwrap().values().cloned().collect();
                for entry in entries {
                    entry.hitl.answer(&request_id, answer.clone());
                }
            }
            Op::Interrupt { run_id } => {
                let entry = runs.lock().unwrap().get(&run_id).cloned();
                if let Some(entry) = entry {
                    entry
                        .cancel
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    entry.hitl.cancel_all_pending();
                }
            }
            Op::SwitchRunMode { run_id, new_mode } => {
                let entry = runs.lock().unwrap().get(&run_id).cloned();
                let Some(entry) = entry else {
                    tracing::debug!(%run_id, "actor: SwitchRunMode 未找到 run");
                    continue;
                };
                let Some(next) = crate::run_mode::RunMode::parse(&new_mode) else {
                    tracing::warn!(%new_mode, "actor: SwitchRunMode 无法解析 RunMode 字符串");
                    continue;
                };
                // actor 更新共享 mode + emit 事件。ToolDispatcher 每次 tool call
                // 通过 Arc<Mutex<RunMode>> 读最新值，下一轮 dispatch 立刻生效。
                let prev = {
                    let mut guard = entry.run_mode.lock().unwrap();
                    let prev = *guard;
                    *guard = next;
                    prev
                };
                (entry.sink)(entry.state.event(protocol::EventPayload::RunModeChanged {
                    from: prev.as_str().to_string(),
                    to: next.as_str().to_string(),
                }));
            }
            other => {
                tracing::debug!(?other, "actor: op 由 surface 自行处理");
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("run 不存在或已结束")]
    RunNotFound,
    #[error("Harness 已关闭")]
    Closed,
}

/// 区分事件是否必须送达。生命周期 / HITL / Turn 边界 / 上下文压缩通知是关键事件，
/// surface 漏掉会卡死或状态不一致；流式增量类的事件丢弃后影响仅限于渲染。
/// 子 NestedRun 的 Run* 生命周期事件——对父 driver 的 turn 终态判定透明（架构 §4.4.11.8）。
/// `drive` 用它把子的这些事件挡在父 turn 状态机之外，避免子结束被误当父结束；
/// 子的非生命周期事件（HITL / 工具 / 文本）仍正常进 match。
fn is_run_lifecycle_event(payload: &EventPayload) -> bool {
    matches!(
        payload,
        EventPayload::RunStarted { .. }
            | EventPayload::RunFinished { .. }
            | EventPayload::RunFailed { .. }
            | EventPayload::RunCancelled
            | EventPayload::RunSuspended { .. }
            | EventPayload::RunResumed { .. }
    )
}

fn is_critical_event(payload: &EventPayload) -> bool {
    matches!(
        payload,
        EventPayload::RunStarted { .. }
            | EventPayload::RunFinished { .. }
            | EventPayload::RunFailed { .. }
            | EventPayload::RunCancelled
            | EventPayload::RunSuspended { .. }
            | EventPayload::RunResumed { .. }
            | EventPayload::TurnStarted { .. }
            | EventPayload::TurnFinished { .. }
            | EventPayload::PermissionRequested { .. }
            | EventPayload::PermissionResolved { .. }
            | EventPayload::UserQuestionRequested { .. }
            | EventPayload::UserQuestionAnswered { .. }
            | EventPayload::ToolCallStarted { .. }
            | EventPayload::ToolCallFinished { .. }
            | EventPayload::ContextCompacted { .. }
            | EventPayload::TextDone { .. }
            | EventPayload::SessionTitleChanged { .. }
            | EventPayload::SessionTitleGenerationFailed { .. }
            | EventPayload::MemoryExtracted { .. }
            | EventPayload::MemoryExtractionFailed { .. }
    )
}

/// 后台记忆抽取（架构 §4.14）：跑一次抽取并把结果 emit 回 run sink。
/// 成功（含写 0 条）→ MemoryExtracted；fallback 链耗尽 → MemoryExtractionFailed
/// （游标已由抽取内部保留，下个 Run 补抽）；无需抽取 / 其他错误 → 静默。
async fn emit_memory_extraction(
    data_dir: &std::path::Path,
    session_id: &str,
    state: &Arc<crate::run_state::RunState>,
    sink: &EventSink,
) {
    use crate::memory_extract::{self, ExtractError};
    match memory_extract::extract_for_session(data_dir, session_id).await {
        Ok(Some(result)) => {
            let items: Vec<protocol::MemoryWriteItem> = result
                .written
                .into_iter()
                .map(|w| protocol::MemoryWriteItem {
                    id: w.id,
                    summary: w.summary,
                    scope: match w.scope {
                        crate::storage::memory::MemoryScope::Project => "project".to_string(),
                        crate::storage::memory::MemoryScope::Global => "global".to_string(),
                    },
                })
                .collect();
            // 写入 >0 条才落 marker——抽取在 RunFinished 之后异步完成，本轮 assistant
            // 早已落盘，故把摘要单独作为一条 Role::Marker 消息 append 到 jsonl 末尾，
            // 随会话持久化，重启后从同一条 marker 重建。0 条不落盘（无摘要可显示）。
            if !items.is_empty() {
                let marker = crate::storage::sessions::Message {
                    id: crate::storage::sessions::new_id(),
                    role: crate::storage::sessions::Role::Marker,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    parts: Vec::new(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    meta: Some(crate::storage::sessions::MessageMeta::MemoryWrites {
                        items: items.clone(),
                    }),
                    subagent_call_id: None,
                    run_duration_ms: None,
                };
                if let Err(e) =
                    crate::storage::sessions::append_message(data_dir, session_id, marker)
                {
                    tracing::warn!(error = %e, "记忆摘要 marker 落盘失败，仅内存态可见");
                }
            }
            sink(state.event(EventPayload::MemoryExtracted {
                session_id: session_id.to_string(),
                items,
            }));
        }
        Ok(None) => {}
        Err(ExtractError::AllModelsFailed(reason)) => {
            sink(state.event(EventPayload::MemoryExtractionFailed {
                session_id: session_id.to_string(),
                reason,
            }));
        }
        Err(e) => {
            tracing::warn!(error = %e, "记忆抽取失败（非模型链原因）");
        }
    }
}

/// 浅睡抽取后挂 idle 哨兵（架构 §3.1）。读设置拿 T 分钟与 last_msg_id，覆盖式 arm
/// 到 `WakeupScheduler`：空闲 T 分钟无新输入则触发深睡 [`consolidate_for_session`]。
/// 记忆系统未启用 / T=0 → 不挂（等同关闭 idle 深睡）。idle_handler 幂等注册一次。
fn arm_idle_after_run(data_dir: &std::path::Path, session_id: &str) {
    let app_settings = crate::storage::settings::load(data_dir);
    if !app_settings.memory.active() {
        return;
    }
    let t_min = app_settings.memory.idle_consolidate_minutes;
    if t_min == 0 {
        return; // 关闭 idle 深睡
    }
    let scheduler = crate::wakeup::WakeupScheduler::global();
    // idle_handler 覆盖式注册：到点在 scheduler 自己的 runtime 里跑深睡（不 resume 对话）。
    // data_dir 随闭包捕获——多 session 共享同一 data_dir，注册一次即可。
    let handler_dd = data_dir.to_path_buf();
    scheduler.set_idle_handler(Arc::new(move |ev: crate::wakeup::IdleElapsed| {
        let dd = handler_dd.clone();
        let threshold = {
            // 到点时重读设置——用户中途可能改了 T；关了就当没配（深度判 None 会跳过）。
            let s = crate::storage::settings::load(&dd);
            s.memory.idle_consolidate_minutes as f64
        };
        tokio::spawn(async move {
            crate::memory_consolidate::consolidate_for_session(
                &dd,
                &ev.session_id,
                ev.idle_minutes,
                threshold,
            )
            .await;
        });
    }));
    let last_msg_id = crate::storage::memory::read_cursor(data_dir, session_id);
    scheduler.arm_idle(session_id.to_string(), last_msg_id, (t_min as i64) * 60_000);
    tracing::info!(target: "memory", "[Memory:Sleep] 已挂 idle 哨兵 session={session_id} T={t_min}min");
}

#[cfg(test)]
mod tests {
    //! `RunHandle::drive` 在 Suspended 路径的回归测试。
    //!
    //! 复现历史 bug（架构 §4.12.5）：模型调挂起工具后，
    //! agent_loop emit `RunSuspended` → emit `TurnFinished(EndTurn)` → 退出（不 emit
    //! `RunFinished`），event channel 在 task drop 后关闭。旧版 `drive` 拿到 `recv()
    //! → None` 直接判 `TurnSummary::failed("事件流意外关闭")`，三个 surface 都把它
    //! 透传成报错——前端就是用户看到的「请求失败：事件流意外关闭」。
    //!
    //! 现在 `drive` 必须把 `RunSuspended` 视为合法终态、return
    //! `TurnOutcome::Suspended`。
    use super::*;
    use protocol::{ResumeCause, SuspendReason};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn make_handle(events: mpsc::Receiver<Event>) -> RunHandle {
        make_handle_with_hitl(events, Arc::new(HitlGate::default()))
    }

    fn make_handle_with_hitl(events: mpsc::Receiver<Event>, hitl: Arc<HitlGate>) -> RunHandle {
        RunHandle {
            run_id: RunId::new(),
            events,
            hitl,
            cancel: Arc::new(AtomicBool::new(false)),
            last_message: crate::run_persister::LastMessageHandle::default(),
        }
    }

    fn ev(run_id: &RunId, seq: u64, payload: EventPayload) -> Event {
        Event::now(run_id.clone(), seq, payload)
    }

    struct NoopObserver;

    #[async_trait]
    impl TurnObserver for NoopObserver {
        fn on_event(&mut self, _event: &Event) {}
        async fn on_permission_request(
            &mut self,
            _request_id: &PermissionRequestId,
            _kind: &PermissionKind,
            _summary: &str,
        ) -> Option<ApprovalDecision> {
            None
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

    struct CountingObserver {
        permission_requests: Arc<AtomicUsize>,
        questions: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl TurnObserver for CountingObserver {
        fn on_event(&mut self, _event: &Event) {}
        async fn on_permission_request(
            &mut self,
            _request_id: &PermissionRequestId,
            _kind: &PermissionKind,
            _summary: &str,
        ) -> Option<ApprovalDecision> {
            self.permission_requests.fetch_add(1, Ordering::SeqCst);
            None
        }
        async fn on_question(
            &mut self,
            _request_id: &PermissionRequestId,
            _question: &str,
            _options: &[QuestionOption],
            _multi: bool,
            _questions: &[AskQuestion],
        ) -> Option<UserAnswer> {
            self.questions.fetch_add(1, Ordering::SeqCst);
            None
        }
    }

    /// 回归（subagent 一停全停，架构 §4.4.11.8）：父 Run 内并发跑子 NestedRun 时，
    /// 子 agent_loop 结束会 emit 自己的 `RunFinished`，经装饰器重写 run_id 为父、
    /// 带上 `subagent_call_id=Some(parent_task_call_id)` 转发进父 sink。`drive` 的终态
    /// 判定若只看 `payload` 不看 `subagent_call_id`，就会把**第一个子的 RunFinished**
    /// 误当成父 Run 结束 → 提前 break → 其它并发子 + 父 agent_loop 全被丢弃（用户报的
    /// 「一个 subagent 跑完，其它和主 loop 也停了」）。
    ///
    /// 正确语义：带 `subagent_call_id` 的 Run* 终态事件是子的，drive 必须忽略，只认
    /// 顶层（`subagent_call_id=None`）的 RunFinished 收 turn。
    #[tokio::test]
    async fn drive_ignores_subagent_run_finished_and_waits_for_parent() {
        let (tx, rx) = mpsc::channel::<Event>(8);
        let mut handle = make_handle(rx);
        let rid = handle.run_id.clone();

        // 子 NestedRun 结束：装饰器已把 run_id 重写为父、带 subagent_call_id。
        tx.send(Event::now_subagent(
            rid.clone(),
            0,
            "call-task-1",
            EventPayload::RunFinished {
                total_input_tokens: 1,
                total_output_tokens: 1,
                total_cache_read_tokens: 0,
                total_cache_creation_tokens: 0,
                duration_ms: 5,
            },
        ))
        .await
        .unwrap();
        // 父 Run 真正结束：顶层事件，subagent_call_id=None，带可识别的 token 数。
        tx.send(ev(
            &rid,
            1,
            EventPayload::RunFinished {
                total_input_tokens: 42,
                total_output_tokens: 7,
                total_cache_read_tokens: 0,
                total_cache_creation_tokens: 0,
                duration_ms: 100,
            },
        ))
        .await
        .unwrap();
        drop(tx);

        let summary = handle.drive(&mut NoopObserver).await;
        match summary.outcome {
            TurnOutcome::Done => {}
            other => panic!("expected Done, got {other:?}"),
        }
        // 必须是父的 usage（input=42），不是子的（input=1）——证明没在子 RunFinished 处提前收尾。
        assert_eq!(
            summary.usage.expect("usage").input,
            42,
            "drive 收到的应是父 Run 的 RunFinished，而非第一个子的"
        );
    }

    /// 回归（subagent 审批「回应失败」）：子 NestedRun 触发的 HITL 事件
    /// （`PermissionRequested` / `UserQuestionRequested`）也带 `subagent_call_id`，
    /// 但它们**必须**走 drive 的 match → `on_permission_request` / `on_question`，
    /// 让 surface 的 HitlState 把 `request_id → gate` 注册进表，否则前端审批回流时
    /// 按 request_id 找不到 gate → 报「审批回应失败」。
    ///
    /// 即：`subagent_call_id.is_some()` 不能无差别跳过——只能跳过子的 Run* 生命周期
    /// 终态事件（避免误终止父），子的 HITL 事件要放行。
    #[tokio::test]
    async fn drive_routes_subagent_permission_request_to_observer() {
        let (tx, rx) = mpsc::channel::<Event>(8);
        // 子审批走父 HitlGate（runner 复用 parent_hitl）：dispatcher 已 open_approval，
        // gate 里有此 pending，drive 的 is_stale 过滤才不会误判它 stale。
        let hitl = Arc::new(HitlGate::default());
        let (request_id, _waiter) = hitl.open_approval(Some("Bash"), Some("rm x"));
        let mut handle = make_handle_with_hitl(rx, hitl);
        let rid = handle.run_id.clone();

        // 子 NestedRun 里会写工具触发审批：装饰器已打上 subagent_call_id。
        tx.send(Event::now_subagent(
            rid.clone(),
            0,
            "call-task-1",
            EventPayload::PermissionRequested {
                request_id: request_id.clone(),
                kind: PermissionKind::ToolCall {
                    tool_name: "Bash".to_string(),
                    input: serde_json::json!({ "command": "rm x" }),
                    fingerprint: Some("rm x".to_string()),
                    command_segments: vec!["rm x".to_string()],
                    segments: Vec::new(),
                    refuse_remember: false,
                },
                summary: "[subagent: coder] 工具 Bash 请求执行".to_string(),
                risk: protocol::RiskLevel::Medium,
                auto_handled: false,
                call_id: "call-sub-tool".to_string(),
            },
        ))
        .await
        .unwrap();
        // 父 Run 结束收 turn。
        tx.send(ev(
            &rid,
            1,
            EventPayload::RunFinished {
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_cache_read_tokens: 0,
                total_cache_creation_tokens: 0,
                duration_ms: 0,
            },
        ))
        .await
        .unwrap();
        drop(tx);

        let permission_requests = Arc::new(AtomicUsize::new(0));
        let questions = Arc::new(AtomicUsize::new(0));
        let mut observer = CountingObserver {
            permission_requests: permission_requests.clone(),
            questions,
        };
        let summary = handle.drive(&mut observer).await;
        match summary.outcome {
            TurnOutcome::Done => {}
            other => panic!("expected Done, got {other:?}"),
        }
        // 子审批必须被路由给 observer（surface 据此 track request_id），否则前端无从回应。
        assert_eq!(
            permission_requests.load(Ordering::SeqCst),
            1,
            "子 NestedRun 的 PermissionRequested 必须走 on_permission_request"
        );
    }

    #[tokio::test]
    async fn drive_skips_permission_request_that_is_no_longer_pending() {
        let (tx, rx) = mpsc::channel::<Event>(8);
        let hitl = Arc::new(HitlGate::default());
        let (request_id, _waiter) = hitl.open_approval(Some("Bash"), Some("cargo check"));
        hitl.resolve(&request_id, ApprovalDecision::AllowOnce);
        let mut handle = make_handle_with_hitl(rx, hitl);
        let rid = handle.run_id.clone();

        tx.send(ev(
            &rid,
            0,
            EventPayload::PermissionRequested {
                request_id,
                kind: PermissionKind::ToolCall {
                    tool_name: "Bash".to_string(),
                    input: serde_json::json!({ "command": "cargo check" }),
                    fingerprint: Some("cargo check".to_string()),
                    command_segments: vec!["cargo check".to_string()],
                    segments: Vec::new(),
                    refuse_remember: false,
                },
                summary: "工具 Bash 请求执行".to_string(),
                risk: protocol::RiskLevel::Medium,
                auto_handled: false,
                call_id: "call-test".to_string(),
            },
        ))
        .await
        .unwrap();
        tx.send(ev(
            &rid,
            1,
            EventPayload::RunFinished {
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_cache_read_tokens: 0,
                total_cache_creation_tokens: 0,
                duration_ms: 0,
            },
        ))
        .await
        .unwrap();
        drop(tx);

        let permission_requests = Arc::new(AtomicUsize::new(0));
        let questions = Arc::new(AtomicUsize::new(0));
        let mut observer = CountingObserver {
            permission_requests: permission_requests.clone(),
            questions,
        };

        let summary = handle.drive(&mut observer).await;
        match summary.outcome {
            TurnOutcome::Done => {}
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(permission_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn drive_cancels_pending_permission_before_observer_when_cancelled() {
        let (tx, rx) = mpsc::channel::<Event>(8);
        let hitl = Arc::new(HitlGate::default());
        let (request_id, waiter) = hitl.open_approval(Some("Bash"), Some("cargo check"));
        let mut handle = make_handle_with_hitl(rx, hitl);
        handle.cancel.store(true, Ordering::SeqCst);
        let rid = handle.run_id.clone();

        tx.send(ev(
            &rid,
            0,
            EventPayload::PermissionRequested {
                request_id,
                kind: PermissionKind::ToolCall {
                    tool_name: "Bash".to_string(),
                    input: serde_json::json!({ "command": "cargo check" }),
                    fingerprint: Some("cargo check".to_string()),
                    command_segments: vec!["cargo check".to_string()],
                    segments: Vec::new(),
                    refuse_remember: false,
                },
                summary: "工具 Bash 请求执行".to_string(),
                risk: protocol::RiskLevel::Medium,
                auto_handled: false,
                call_id: "call-test".to_string(),
            },
        ))
        .await
        .unwrap();
        tx.send(ev(&rid, 1, EventPayload::RunCancelled))
            .await
            .unwrap();
        drop(tx);

        let permission_requests = Arc::new(AtomicUsize::new(0));
        let questions = Arc::new(AtomicUsize::new(0));
        let mut observer = CountingObserver {
            permission_requests: permission_requests.clone(),
            questions,
        };

        let summary = handle.drive(&mut observer).await;
        match summary.outcome {
            TurnOutcome::Cancelled => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
        assert_eq!(permission_requests.load(Ordering::SeqCst), 0);
        assert!(matches!(waiter.await.unwrap(), ApprovalDecision::Deny));
    }

    #[tokio::test]
    async fn drive_skips_question_request_that_is_no_longer_pending() {
        let (tx, rx) = mpsc::channel::<Event>(8);
        let hitl = Arc::new(HitlGate::default());
        let (request_id, _waiter) = hitl.open_question();
        hitl.answer(&request_id, UserAnswer::Cancelled);
        let mut handle = make_handle_with_hitl(rx, hitl);
        let rid = handle.run_id.clone();

        tx.send(ev(
            &rid,
            0,
            EventPayload::UserQuestionRequested {
                request_id,
                question: "继续吗？".to_string(),
                options: vec![],
                multi: false,
                questions: vec![],
            },
        ))
        .await
        .unwrap();
        tx.send(ev(
            &rid,
            1,
            EventPayload::RunFinished {
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_cache_read_tokens: 0,
                total_cache_creation_tokens: 0,
                duration_ms: 0,
            },
        ))
        .await
        .unwrap();
        drop(tx);

        let permission_requests = Arc::new(AtomicUsize::new(0));
        let questions = Arc::new(AtomicUsize::new(0));
        let mut observer = CountingObserver {
            permission_requests,
            questions: questions.clone(),
        };

        let summary = handle.drive(&mut observer).await;
        match summary.outcome {
            TurnOutcome::Done => {}
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(questions.load(Ordering::SeqCst), 0);
    }

    /// channel 在 RunSuspended 之后被关闭——这正是 agent_loop 走 ModelError::Suspended
    /// 时的行为。drive 必须返回 Suspended，而不是 Failed("事件流意外关闭")。
    #[tokio::test]
    async fn drive_treats_run_suspended_as_terminal() {
        let (tx, rx) = mpsc::channel::<Event>(8);
        let mut handle = make_handle(rx);
        let rid = handle.run_id.clone();

        tx.send(ev(
            &rid,
            0,
            EventPayload::RunSuspended {
                reason: SuspendReason::BackgroundTask,
                resumes_at_ms: None,
                waiting_for_task_ids: vec!["bash_002".to_string()],
            },
        ))
        .await
        .unwrap();
        drop(tx); // 模拟 agent_loop task 退出 → send 端 drop → recv 之后返回 None

        let summary = handle.drive(&mut NoopObserver).await;
        match summary.outcome {
            TurnOutcome::Suspended => {}
            other => panic!("expected Suspended, got {other:?}"),
        }
        assert!(
            summary.usage.is_none(),
            "Suspended 不应吐 usage，否则 surface 会重复累加"
        );
    }

    /// 历史 bug 的反向用例：如果 drive 漏掉 RunSuspended 终态识别，
    /// channel 关闭就会被误报为 "事件流意外关闭"。本测试钉住正确行为。
    #[tokio::test]
    async fn drive_does_not_report_stream_closed_after_suspended() {
        let (tx, rx) = mpsc::channel::<Event>(8);
        let mut handle = make_handle(rx);
        let rid = handle.run_id.clone();

        // 先发一些正常事件，再发 Suspended，再关 channel——模拟真实 agent_loop。
        tx.send(ev(
            &rid,
            0,
            EventPayload::TurnStarted {
                turn_id: protocol::TurnId::new(),
                turn: 0,
            },
        ))
        .await
        .unwrap();
        tx.send(ev(
            &rid,
            1,
            EventPayload::TurnFinished {
                turn_id: protocol::TurnId::new(),
                turn: 0,
                stop_reason: protocol::StopReason::EndTurn,
            },
        ))
        .await
        .unwrap();
        tx.send(ev(
            &rid,
            2,
            EventPayload::RunSuspended {
                reason: SuspendReason::Cron,
                resumes_at_ms: Some(0),
                waiting_for_task_ids: vec![],
            },
        ))
        .await
        .unwrap();
        drop(tx);

        let summary = handle.drive(&mut NoopObserver).await;
        match summary.outcome {
            TurnOutcome::Failed(msg) => {
                panic!("RunSuspended 之后 channel 关闭被误报为 Failed: {msg}")
            }
            TurnOutcome::Suspended => {}
            other => panic!("expected Suspended, got {other:?}"),
        }
    }

    /// 健壮性：channel 直接关闭（既没发 RunFinished 也没发 RunSuspended）仍应判为
    /// Failed("事件流意外关闭")——这条路径表明 agent_loop 异常退出，与 Suspended 路径
    /// 完全不同，不能被新分支误吞掉。
    #[tokio::test]
    async fn drive_still_reports_failed_when_channel_drops_silently() {
        let (tx, rx) = mpsc::channel::<Event>(8);
        let mut handle = make_handle(rx);
        drop(tx);

        let summary = handle.drive(&mut NoopObserver).await;
        match summary.outcome {
            TurnOutcome::Failed(msg) if msg.contains("事件流意外关闭") => {}
            other => panic!("expected Failed(事件流意外关闭), got {other:?}"),
        }
    }

    /// 看一眼 RunResumed 也应是 critical event（满载时不丢）——
    /// resume 后没有这条事件 surface 永远停在挂起 UI。
    #[test]
    fn run_suspended_and_resumed_are_critical() {
        assert!(is_critical_event(&EventPayload::RunSuspended {
            reason: SuspendReason::BackgroundTask,
            resumes_at_ms: None,
            waiting_for_task_ids: vec![],
        }));
        assert!(is_critical_event(&EventPayload::RunResumed {
            cause: ResumeCause::UserMessageArrived,
        }));
    }
}
