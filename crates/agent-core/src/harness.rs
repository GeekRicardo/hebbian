use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use protocol::{AgentRef, Event, EventPayload, Op, RunId, Submission};
use tokio::sync::{broadcast, mpsc};
use tracing::warn;

use crate::{
    agent_loop::{self, EventSink, LoopParams},
    context::transcript::Transcript,
    definition::CompactionPolicy,
    hooks::HookManager,
    run_state::RunState,
    tools::{permissions::PermissionGate, registry::ToolRegistry, Tool},
};
use model_gateway::client::ModelClient;
use platform::CancelFlag;

/// 一个进行中的 run 的运行时句柄
struct RunHandle {
    #[allow(dead_code)]
    state: Arc<RunState>,
    gate: Arc<PermissionGate>,
    cancel: CancelFlag,
}

/// 启动一次 run 所需的全部上下文。
pub struct RunParams {
    pub agent: AgentRef,
    pub gate: Arc<PermissionGate>,
    /// 调用方组装好的完整 transcript（含 system + 历史 + 当前 user message）
    pub transcript: Transcript,
    pub enabled_tools: Vec<String>,
    pub compaction_policy: CompactionPolicy,
    pub stream: bool,
    pub cancel: CancelFlag,
    pub parent: Option<RunId>,
}

/// Harness 是 Core 对外的门面。
///
/// 单一交互范式：
/// - **`subscribe()`** 订阅事件流（broadcast，多 surface 可同时订阅）
/// - **`spawn_run(client, params)`** 异步启动一个 run，立刻返回 `RunId`，事件走广播
/// - **`submit(submission)`** 用协议 `Op` 投递控制指令（Approve / Interrupt 等）
///
/// 调用约定：**先 `subscribe()` 再 `spawn_run()`**，否则可能错过早期事件
/// （broadcast 容量 1024 通常够用，但务必先订阅）。
pub struct Harness {
    registry: Arc<ToolRegistry>,
    hooks: Arc<HookManager>,
    runs: Arc<Mutex<HashMap<RunId, Arc<RunHandle>>>>,
    submit_tx: mpsc::UnboundedSender<Submission>,
    event_tx: broadcast::Sender<Event>,
}

impl Harness {
    pub fn new(tools: Vec<Box<dyn Tool>>, hooks: HookManager) -> Self {
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<Submission>();
        let (event_tx, _) = broadcast::channel::<Event>(1024);

        let harness = Self {
            registry: Arc::new(ToolRegistry::new(tools)),
            hooks: Arc::new(hooks),
            runs: Arc::new(Mutex::new(HashMap::new())),
            submit_tx,
            event_tx: event_tx.clone(),
        };

        let runs = harness.runs.clone();
        let event_tx_for_actor = event_tx;
        tokio::spawn(async move {
            run_actor_loop(submit_rx, runs, event_tx_for_actor).await;
        });

        harness
    }

    /// 订阅事件总线。返回的 receiver 收到所有 run 的所有事件；
    /// 调用方按 `event.run_id` 过滤。
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// 异步启动一个 run。立刻返回 `RunId`，事件流走 `subscribe()`。
    ///
    /// `client` 是本次 run 用的模型；不同 session 可以传不同 client。
    /// `params.gate` 让调用方提前持有 gate Arc，便于外部 `gate.resolve()` 接入 HITL。
    pub fn spawn_run(&self, client: Arc<dyn ModelClient>, params: RunParams) -> RunId {
        let run_id = RunId::new();
        let state = Arc::new(RunState::new(run_id.clone()));

        let handle = Arc::new(RunHandle {
            state: state.clone(),
            gate: params.gate.clone(),
            cancel: params.cancel.clone(),
        });
        self.runs.lock().unwrap().insert(run_id.clone(), handle);

        let event_tx = self.event_tx.clone();
        let sink: EventSink = Arc::new(move |event: Event| {
            let _ = event_tx.send(event);
        });

        let registry = self.registry.clone();
        let hooks = self.hooks.clone();
        let runs = self.runs.clone();
        let run_id_for_task = run_id.clone();
        let RunParams {
            agent,
            gate,
            mut transcript,
            enabled_tools,
            compaction_policy,
            stream,
            cancel,
            parent,
        } = params;

        tokio::spawn(async move {
            let params = LoopParams {
                client: client.as_ref(),
                registry,
                gate,
                hooks,
                transcript: &mut transcript,
                enabled_tools: &enabled_tools,
                compaction_policy: &compaction_policy,
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

        run_id
    }

    /// 投递一个控制指令。当前 actor 处理 `Approve` / `Interrupt`；
    /// `StartRun` 等需要 surface 自行解析后调 `spawn_run`。
    pub fn submit(&self, submission: Submission) -> Result<protocol::SubmissionId, HarnessError> {
        let id = submission.id.clone();
        self.submit_tx
            .send(submission)
            .map_err(|_| HarnessError::Closed)?;
        Ok(id)
    }

    /// 直接 resolve 某次审批（不走 submit 队列，常用于桌面 surface 持有 gate Arc 的场景）。
    pub fn resolve_permission(
        &self,
        run_id: &RunId,
        request_id: &protocol::PermissionRequestId,
        decision: protocol::ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let handle = self
            .runs
            .lock()
            .unwrap()
            .get(run_id)
            .cloned()
            .ok_or(HarnessError::RunNotFound)?;
        handle.gate.resolve(request_id, decision, None);
        Ok(())
    }

    /// 中断某个 run（设置 cancel flag + 解除所有挂起 waiter）
    pub fn interrupt(&self, run_id: &RunId) -> Result<(), HarnessError> {
        let handle = self
            .runs
            .lock()
            .unwrap()
            .get(run_id)
            .cloned()
            .ok_or(HarnessError::RunNotFound)?;
        handle
            .cancel
            .store(true, std::sync::atomic::Ordering::SeqCst);
        handle.gate.cancel_all_pending();
        Ok(())
    }
}

async fn run_actor_loop(
    mut submit_rx: mpsc::UnboundedReceiver<Submission>,
    runs: Arc<Mutex<HashMap<RunId, Arc<RunHandle>>>>,
    event_tx: broadcast::Sender<Event>,
) {
    while let Some(submission) = submit_rx.recv().await {
        match submission.op {
            Op::Approve {
                request_id,
                decision,
            } => {
                // request_id 全局唯一；遍历所有 run 让对应的 gate 处理（其他 gate 找不到会无操作）
                let handles: Vec<_> = runs.lock().unwrap().values().cloned().collect();
                for handle in handles {
                    handle.gate.resolve(&request_id, decision.clone(), None);
                }
            }
            Op::Interrupt { run_id } => {
                let handle = runs.lock().unwrap().get(&run_id).cloned();
                if let Some(handle) = handle {
                    handle
                        .cancel
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    handle.gate.cancel_all_pending();
                    let _ = event_tx.send(Event::now(
                        run_id,
                        u64::MAX,
                        EventPayload::RunCancelled,
                    ));
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
