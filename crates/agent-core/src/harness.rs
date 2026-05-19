use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use protocol::{
    AgentRef, ApprovalDecision, Event, EventPayload, Op, PermissionKind, PermissionRequestId,
    QuestionOption, RunId, Submission, SubmissionId, UserAnswer,
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
    /// 运行时输入注入队列：surface 在 streaming 中「立即发送」时把 user message 推进来，
    /// agent_loop 每次 model.request 之前 drain 出来加入 transcript。`None` 表示禁用。
    pub pending_inputs: Option<PendingInputs>,
    /// 已被 agent_loop drain 的 pending input 副本。surface 可在 run 结束后按顺序落盘。
    pub consumed_pending_inputs: Option<ConsumedPendingInputs>,
    /// 运行模式（架构 §4.4.3）。默认 [`crate::run_mode::RunMode::AskBeforeEdits`]。
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
    /// 挂起请求通道（架构 §4.12.4）。Surface 与本 Run 关联的工具
    /// （WaitForTask / ScheduleWakeup）共享同一个 channel，由 `default_tools`
    /// 构造时塞进 BashTool 旁边的两个挂起工具。`None` 表示当前会话禁用挂起。
    pub phase: Option<crate::wakeup::PhaseChannel>,
    /// 从挂起态恢复时携带：agent_loop 用它初始化计数器 + emit `RunResumed`
    /// 而不是 `RunStarted`（架构 §4.12.6）。
    pub resume_from: Option<crate::agent_loop::RunResumeState>,
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

        // 双路 sink：本 run 独享 mpsc + 可选 jsonl 持久化。
        //
        // 通道改为 bounded(1024)。sink 是同步闭包（type EventSink），不能 await，
        // 因此先用 `try_send` 保持事件顺序；只有关键事件遇到满队列时才 spawn 一个
        // 极短 task 等空位，避免生命周期 / HITL 事件丢失。
        let (run_tx, run_rx) = mpsc::channel::<Event>(1024);
        let recorder = params.recorder.clone();
        let sink: EventSink = Arc::new(move |event: Event| {
            if let Some(rec) = &recorder {
                rec.write(&event);
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
            pending_inputs,
            consumed_pending_inputs,
            run_mode,
            model_id,
            force_automode,
            data_dir,
            session_id: loop_session_id,
            phase,
            resume_from,
        } = params;

        let hitl_for_handle = hitl.clone();
        let cancel_for_handle = cancel.clone();
        let judge_client = client.clone();

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
                pending_inputs,
                consumed_pending_inputs,
                run_mode,
                model_id,
                judge_client: Some(judge_client),
                force_automode,
                data_dir,
                session_id: loop_session_id,
                phase,
                resume_from,
            };
            if let Err(e) = agent_loop::run_loop(params, sink).await {
                warn!(error = %e, "run failed");
            }
            runs.lock().unwrap().remove(&run_id_for_task);
        });

        RunHandle {
            run_id,
            events: run_rx,
            hitl: hitl_for_handle,
            cancel: cancel_for_handle,
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
    pub async fn drive<O: TurnObserver>(&mut self, observer: &mut O) -> TurnSummary {
        loop {
            let Some(event) = self.recv().await else {
                return TurnSummary::failed("事件流意外关闭");
            };
            observer.on_event(&event);

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
                } => {
                    if let Some(answer) = observer
                        .on_question(request_id, question, options, *multi)
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
                    ..
                } => {
                    return TurnSummary {
                        outcome: TurnOutcome::Done,
                        usage: Some(UsageTotals {
                            input: *total_input_tokens,
                            output: *total_output_tokens,
                            cache_read: *total_cache_read_tokens,
                            cache_creation: *total_cache_creation_tokens,
                        }),
                    };
                }
                EventPayload::RunFailed { error } => {
                    return TurnSummary::failed(&error.message);
                }
                EventPayload::RunCancelled => {
                    return TurnSummary {
                        outcome: TurnOutcome::Cancelled,
                        usage: None,
                    };
                }
                _ => {}
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
    ) -> Option<UserAnswer>;
}

/// 一次 run 跑完后的总结。
#[derive(Debug, Clone)]
pub struct TurnSummary {
    pub outcome: TurnOutcome,
    pub usage: Option<UsageTotals>,
}

#[derive(Debug, Clone)]
pub enum TurnOutcome {
    Done,
    Failed(String),
    Cancelled,
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
                // 本期：actor 仅更新共享 mode + emit 事件，不强行替换 dispatcher
                // 已捕获的 run_mode 值（架构 §13 留尾巴：运行时真切要把 ToolDispatcher.run_mode
                // 改为 Arc<Mutex<RunMode>> 才能下一轮 dispatch 立刻拿到新值）。
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
fn is_critical_event(payload: &EventPayload) -> bool {
    matches!(
        payload,
        EventPayload::RunStarted { .. }
            | EventPayload::RunFinished { .. }
            | EventPayload::RunFailed { .. }
            | EventPayload::RunCancelled
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
    )
}
