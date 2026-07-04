//! Session 运行时枢纽（架构 §7.8.5「单写者 + 多观察者」）。
//!
//! [`SessionHub`] 持有全部活 session 的运行时状态，是 hebcore 单核心进程的核心数据
//! 结构（§7.8.1）。每个 [`SessionRuntimeState`] 一个 `broadcast` 通道：跑 run 的那一方
//! 是**唯一写者**，所有 surface（desktop / heb / hebweb）`subscribe` 成为**观察者**，
//! 看同一份 [`WireEvent`] 流——这从根上消除"两进程怎么同步同一对话"的问题。
//!
//! 本结构下沉自 hebweb 的 `SessionRuntime`：抽出与 surface 无关的运行时状态
//! （事件 broadcast / HITL pending / cancel / pending inputs / run_mode），surface
//! 特有部分（provider/model、输入驱动、协议包装）留在各 surface。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use protocol::{ApprovalDecision, PermissionRequestId, UserAnswer, WireEvent};
use tokio::sync::broadcast;

use crate::run_mode::RunMode;
use crate::tools::hitl::HitlGate;
use common::runtime::{PendingInputs, PendingUserInput};

/// 单个 session 的运行时状态（架构 §7.8.5）。
///
/// 同一时刻只有一个写者（跑 run 的那条链路），其余 surface 通过 [`subscribe`] 只读观察。
///
/// [`subscribe`]: SessionRuntimeState::subscribe
pub struct SessionRuntimeState {
    pub session_id: String,

    /// 当前活 run 的 HITL 闸门（审批 / 提问的唯一结算点）。run 启动时 [`set_active`]
    /// 挂上 agent_loop 持有的真 [`HitlGate`]，surface 的审批回应经 [`resolve_approval`] /
    /// [`answer_question`] 直接戳它——不再自造第二层 oneshot gate（那会在 drive loop 里
    /// 阻塞 recv，让 AutoMode judge 后发的 `PermissionAutoJudged` pump 不出去而死锁）。
    ///
    /// [`set_active`]: SessionRuntimeState::set_active
    /// [`resolve_approval`]: SessionRuntimeState::resolve_approval
    /// [`answer_question`]: SessionRuntimeState::answer_question
    pub hitl: Mutex<Option<Arc<HitlGate>>>,

    /// 是否有 run 正在跑（活写者存在）。
    pub active_run: AtomicBool,
    /// 当前活 run 的 cancel 句柄（interrupt 用）。
    pub cancel_flag: Mutex<Option<Arc<AtomicBool>>>,
    /// 当前活 run 的插队输入队列（inject 用）。
    pub pending_inputs: Mutex<Option<PendingInputs>>,
    /// 是否还接受插队输入。agent_loop 在 drain 边界维护：run 开始 / 续跑时 true，run 收尾、
    /// 末次 drain 之后置 false（[`crate::agent_loop`] 的 `set_pending_inputs_accepting`）。
    /// inject 据此在 run 收尾窗口拒绝晚到注入（返回 false），让 surface 回落「起新 run」而不是
    /// push 进一个再也不会被 drain 的队列——否则消息既不进 transcript 也不落盘，客户端却收到
    /// Accepted，静默丢失（§4.2.3）。
    pub pending_inputs_accepting: Mutex<Option<Arc<AtomicBool>>>,

    /// 运行时 RunMode（surface 的 set_run_mode 即时改值）。
    pub run_mode: Mutex<RunMode>,
    /// 强制 auto-mode 开关（in-memory，重启回 false）。
    pub force_automode: AtomicBool,

    /// 唯一写者 → 多观察者的事件广播（§7.8.5）。无订阅者时 send 失败可忽略
    /// （fire-and-forget），有订阅者就逐 [`WireEvent`] 实时收。
    pub event_tx: broadcast::Sender<WireEvent>,
}

impl SessionRuntimeState {
    /// 新建一个 session 运行时状态。`capacity` 是 broadcast 通道容量（慢订阅者落后会丢早帧）。
    pub fn new(session_id: impl Into<String>, capacity: usize, run_mode: RunMode) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(capacity);
        Arc::new(Self {
            session_id: session_id.into(),
            hitl: Mutex::new(None),
            active_run: AtomicBool::new(false),
            cancel_flag: Mutex::new(None),
            pending_inputs: Mutex::new(None),
            pending_inputs_accepting: Mutex::new(None),
            run_mode: Mutex::new(run_mode),
            force_automode: AtomicBool::new(false),
            event_tx,
        })
    }

    /// 成为本 run 的观察者，逐 [`WireEvent`] 实时收。多 surface 可同时订阅同一 run。
    pub fn subscribe(&self) -> broadcast::Receiver<WireEvent> {
        self.event_tx.subscribe()
    }

    /// 广播一个事件给所有观察者。无订阅者时静默丢弃。
    pub fn emit(&self, event: WireEvent) {
        let _ = self.event_tx.send(event);
    }

    pub fn is_active(&self) -> bool {
        self.active_run.load(Ordering::SeqCst)
    }

    /// run 开始：登记 HITL 闸门 + cancel 句柄 + 插队队列，置活。
    pub fn set_active(
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

    /// run 结束：清 HITL 闸门 + cancel 句柄 + 插队队列 + accepting 标志，置闲。
    pub fn clear_active(&self) {
        *self.hitl.lock().unwrap() = None;
        *self.cancel_flag.lock().unwrap() = None;
        *self.pending_inputs.lock().unwrap() = None;
        *self.pending_inputs_accepting.lock().unwrap() = None;
        self.active_run.store(false, Ordering::SeqCst);
    }

    /// 把一条 user 输入插进当前活 run 的队列（agent_loop 下个 drain 边界消费）。
    /// 无活 run、或 run 已过末次 drain（accepting=false 的收尾窗口）返回 `false`，
    /// 让 surface 回落到「起新 run」，避免消息 push 进再也不会被 drain 的队列而静默丢失。
    pub fn inject(&self, input: PendingUserInput) -> bool {
        // run 收尾窗口（agent_loop 末次 drain 后置 accepting=false）拒绝晚到注入；
        // accepting 缺失（未接线）也按拒绝处理（§4.2.3）。
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
            inputs.lock().unwrap().push(input);
            true
        } else {
            false
        }
    }

    /// 中断当前 run：拉起 cancel flag + 把全部待结算 HITL 当拒绝 / 取消放掉。
    pub fn stop(&self) {
        if let Some(flag) = &*self.cancel_flag.lock().unwrap() {
            flag.store(true, Ordering::SeqCst);
        }
        if let Some(hitl) = &*self.hitl.lock().unwrap() {
            hitl.cancel_all_pending();
        }
    }

    /// 结算一条审批（surface 回应到达时）。返回是否命中一个待结算项。
    pub fn resolve_approval(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        let request_id = PermissionRequestId::from_raw(request_id);
        let guard = self.hitl.lock().unwrap();
        match &*guard {
            Some(hitl) if hitl.is_pending(&request_id) => {
                hitl.resolve(&request_id, decision);
                true
            }
            _ => false,
        }
    }

    /// 结算一条提问。返回是否命中一个待结算项。
    pub fn answer_question(&self, request_id: &str, answer: UserAnswer) -> bool {
        let request_id = PermissionRequestId::from_raw(request_id);
        let guard = self.hitl.lock().unwrap();
        match &*guard {
            Some(hitl) if hitl.is_pending(&request_id) => {
                hitl.answer(&request_id, answer);
                true
            }
            _ => false,
        }
    }

    pub fn run_mode(&self) -> RunMode {
        *self.run_mode.lock().unwrap()
    }

    pub fn set_run_mode(&self, mode: RunMode) {
        *self.run_mode.lock().unwrap() = mode;
    }
}

/// 全部活 session 的运行时枢纽（架构 §7.8.1）。hebcore 进程持唯一一份；当前各 surface
/// 各持一份（in-process），步骤③合并为常驻进程后成为单例。
#[derive(Default)]
pub struct SessionHub {
    sessions: Mutex<HashMap<String, Arc<SessionRuntimeState>>>,
}

impl SessionHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取已存在的 runtime（不自动创建）。
    pub fn get(&self, session_id: &str) -> Option<Arc<SessionRuntimeState>> {
        self.sessions.lock().unwrap().get(session_id).cloned()
    }

    /// 取或按需创建一个 runtime。已存在则返回旧的（保住活 broadcast 订阅者）。
    pub fn get_or_create(
        &self,
        session_id: &str,
        capacity: usize,
        run_mode: RunMode,
    ) -> Arc<SessionRuntimeState> {
        let mut guard = self.sessions.lock().unwrap();
        if let Some(rt) = guard.get(session_id) {
            return rt.clone();
        }
        let rt = SessionRuntimeState::new(session_id, capacity, run_mode);
        guard.insert(session_id.to_string(), rt.clone());
        rt
    }

    /// 显式登记一个已构造的 runtime。
    pub fn insert(&self, runtime: Arc<SessionRuntimeState>) {
        self.sessions
            .lock()
            .unwrap()
            .insert(runtime.session_id.clone(), runtime);
    }

    /// 移除一个 runtime（session 关闭 / detach）。
    pub fn remove(&self, session_id: &str) -> Option<Arc<SessionRuntimeState>> {
        self.sessions.lock().unwrap().remove(session_id)
    }

    /// 当前活 session id 列表。
    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_receives_emitted_events() {
        let rt = SessionRuntimeState::new("s1", 16, RunMode::Default);
        let mut rx = rt.subscribe();
        rt.emit(WireEvent::Error {
            message: "boom".into(),
        });
        let got = rx.try_recv().expect("应收到广播事件");
        assert!(matches!(got, WireEvent::Error { message } if message == "boom"));
    }

    #[test]
    fn multiple_observers_see_same_event() {
        let rt = SessionRuntimeState::new("s1", 16, RunMode::Default);
        let mut a = rt.subscribe();
        let mut b = rt.subscribe();
        rt.emit(WireEvent::Error {
            message: "x".into(),
        });
        assert!(matches!(a.try_recv(), Ok(WireEvent::Error { .. })));
        assert!(matches!(b.try_recv(), Ok(WireEvent::Error { .. })));
    }

    #[test]
    fn inject_requires_active_run() {
        let rt = SessionRuntimeState::new("s1", 16, RunMode::Default);
        assert!(
            !rt.inject(PendingUserInput {
                content: "hi".into(),
                attachments: Vec::new(),
                meta: None,
            }),
            "无活 run 时 inject 应失败"
        );
        let inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
        rt.set_active(
            Arc::new(HitlGate::default()),
            Arc::new(AtomicBool::new(false)),
            inputs.clone(),
            Arc::new(AtomicBool::new(true)),
        );
        assert!(rt.inject(PendingUserInput {
            content: "hi".into(),
            attachments: vec![common::attachments::MessageAttachment::Image {
                name: "p.png".into(),
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            }],
            meta: None,
        }));
        let queued = inputs.lock().unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].attachments.len(), 1);
    }

    /// 回归（#8 late-inject 静默丢消息）：run 收尾窗口（agent_loop 末次 drain 后置
    /// accepting=false）的注入必须被拒绝（返回 false），让 surface 回落起新 run，而不是
    /// push 进一个再也不会被 drain 的队列。
    #[test]
    fn inject_rejected_after_accepting_cleared() {
        let rt = SessionRuntimeState::new("s1", 16, RunMode::Default);
        let inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
        let accepting = Arc::new(AtomicBool::new(true));
        rt.set_active(
            Arc::new(HitlGate::default()),
            Arc::new(AtomicBool::new(false)),
            inputs.clone(),
            accepting.clone(),
        );
        assert!(
            rt.inject(PendingUserInput {
                content: "a".into(),
                attachments: Vec::new(),
                meta: None,
            }),
            "accepting=true 时应接受注入"
        );
        // 模拟 agent_loop 末次 drain 后置 accepting=false（run 收尾窗口）。
        accepting.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(
            !rt.inject(PendingUserInput {
                content: "b".into(),
                attachments: Vec::new(),
                meta: None,
            }),
            "accepting=false 时应拒绝注入（防 §4.2.3 消息静默丢失）"
        );
        assert_eq!(inputs.lock().unwrap().len(), 1, "被拒注入不该 push 进队列");
    }

    /// 回归（bug B 死锁根因）：审批结算直接戳活 run 的真 HitlGate，无第二层 oneshot
    /// 中转。无活 run 时返回 false，pending 命中时唤醒 agent_loop 侧 waiter 并返回 true。
    #[tokio::test]
    async fn resolve_approval_hits_live_hitl_gate() {
        let rt = SessionRuntimeState::new("s1", 16, RunMode::Default);
        // 无活 run：结算应失败（没有可命中的 gate）。
        assert!(!rt.resolve_approval("nope", ApprovalDecision::AllowOnce));

        let gate = Arc::new(HitlGate::default());
        let (request_id, waiter) = gate.open_approval(Some("Bash"), None);
        rt.set_active(
            gate.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(AtomicBool::new(true)),
        );

        // 未知 request_id 不命中，活 run 也返回 false。
        assert!(!rt.resolve_approval("unknown", ApprovalDecision::AllowOnce));
        // 命中真实 pending：返回 true 并唤醒 agent_loop 侧 waiter。
        assert!(rt.resolve_approval(request_id.as_str(), ApprovalDecision::AllowOnce));
        assert!(matches!(waiter.await, Ok(ApprovalDecision::AllowOnce)));
    }

    #[test]
    fn hub_get_or_create_is_idempotent() {
        let hub = SessionHub::new();
        let a = hub.get_or_create("s1", 16, RunMode::Default);
        let b = hub.get_or_create("s1", 16, RunMode::Default);
        assert!(Arc::ptr_eq(&a, &b), "同 id 应复用同一 runtime");
        assert_eq!(hub.session_ids(), vec!["s1".to_string()]);
    }
}
