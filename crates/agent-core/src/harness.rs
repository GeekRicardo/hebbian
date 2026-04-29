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
    recorder::Recorder,
    run_state::RunState,
    tools::{hitl::HitlGate, registry::ToolRegistry, Tool},
    workspace::Workspace,
};
use model_gateway::client::ModelClient;
use platform::CancelFlag;

/// 注册表里登记一次 run 的运行时控制点（供跨进程 `Op::Approve` / `Op::Interrupt` 反查）。
struct RunRegistration {
    hitl: Arc<HitlGate>,
    cancel: CancelFlag,
}

/// 启动一次 run 所需的全部上下文。
pub struct RunParams {
    pub agent: AgentRef,
    pub hitl: Arc<HitlGate>,
    /// 调用方组装好的完整 transcript（含 system + 历史 + 当前 user message）
    pub transcript: Transcript,
    pub enabled_tools: Vec<String>,
    pub compaction_policy: CompactionPolicy,
    /// 本对话的 workspace（workdir + allowed_dirs）。每个对话独立。
    pub workspace: Arc<Workspace>,
    pub stream: bool,
    pub cancel: CancelFlag,
    pub parent: Option<RunId>,
    /// 可选的事件持久化。给定后所有事件 fire-and-forget 追加进 jsonl。
    pub recorder: Option<Recorder>,
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
    submit_tx: mpsc::UnboundedSender<Submission>,
}

impl Harness {
    pub fn new(tools: Vec<Box<dyn Tool>>, hooks: HookManager) -> Self {
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<Submission>();

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

        // 注册到全局 runs 表，让 actor 能反查（处理 Op::Approve / Op::Interrupt）。
        self.runs.lock().unwrap().insert(
            run_id.clone(),
            Arc::new(RunRegistration {
                hitl: params.hitl.clone(),
                cancel: params.cancel.clone(),
            }),
        );

        // 双路 sink：本 run 独享 mpsc + 可选 jsonl 持久化。
        let (run_tx, run_rx) = mpsc::unbounded_channel::<Event>();
        let recorder = params.recorder.clone();
        let sink: EventSink = Arc::new(move |event: Event| {
            if let Some(rec) = &recorder {
                rec.write(&event);
            }
            let _ = run_tx.send(event);
        });

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
        } = params;

        let hitl_for_handle = hitl.clone();
        let cancel_for_handle = cancel.clone();

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
    pub fn submit(&self, submission: Submission) -> Result<SubmissionId, HarnessError> {
        let id = submission.id.clone();
        self.submit_tx
            .send(submission)
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
    events: mpsc::UnboundedReceiver<Event>,
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

    pub fn resolve_permission(
        &self,
        request_id: &PermissionRequestId,
        decision: ApprovalDecision,
    ) {
        self.hitl.resolve(request_id, decision, None);
    }

    pub fn answer_question(&self, request_id: &PermissionRequestId, answer: UserAnswer) {
        self.hitl.answer(request_id, answer);
    }

    pub fn interrupt(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::SeqCst);
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
                } => {
                    if let Some(answer) =
                        observer.on_question(request_id, question, options).await
                    {
                        self.answer_question(request_id, answer);
                    }
                }
                EventPayload::RunFinished {
                    total_input_tokens,
                    total_output_tokens,
                    ..
                } => {
                    return TurnSummary {
                        outcome: TurnOutcome::Done,
                        usage: Some(UsageTotals {
                            input: *total_input_tokens,
                            output: *total_output_tokens,
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
    mut submit_rx: mpsc::UnboundedReceiver<Submission>,
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
                    entry.hitl.resolve(&request_id, decision.clone(), None);
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
