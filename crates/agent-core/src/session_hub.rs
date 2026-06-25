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

use protocol::{ApprovalDecision, UserAnswer, WireEvent};
use tokio::sync::{broadcast, oneshot};

use crate::run_mode::RunMode;
use common::runtime::{PendingInputs, PendingUserInput};

/// 单个 session 的运行时状态（架构 §7.8.5）。
///
/// 同一时刻只有一个写者（跑 run 的那条链路），其余 surface 通过 [`subscribe`] 只读观察。
///
/// [`subscribe`]: SessionRuntimeState::subscribe
pub struct SessionRuntimeState {
    pub session_id: String,

    /// HITL 待结算通道：审批 / 提问 emit 后挂在这里，等 surface 回应。
    pub pending_approvals: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
    pub pending_questions: Mutex<HashMap<String, oneshot::Sender<UserAnswer>>>,

    /// 是否有 run 正在跑（活写者存在）。
    pub active_run: AtomicBool,
    /// 当前活 run 的 cancel 句柄（interrupt 用）。
    pub cancel_flag: Mutex<Option<Arc<AtomicBool>>>,
    /// 当前活 run 的插队输入队列（inject 用）。
    pub pending_inputs: Mutex<Option<PendingInputs>>,

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
            pending_approvals: Mutex::new(HashMap::new()),
            pending_questions: Mutex::new(HashMap::new()),
            active_run: AtomicBool::new(false),
            cancel_flag: Mutex::new(None),
            pending_inputs: Mutex::new(None),
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

    /// run 开始：登记 cancel 句柄 + 插队队列，置活。
    pub fn set_active(&self, cancel: Arc<AtomicBool>, inputs: PendingInputs) {
        *self.cancel_flag.lock().unwrap() = Some(cancel);
        *self.pending_inputs.lock().unwrap() = Some(inputs);
        self.active_run.store(true, Ordering::SeqCst);
    }

    /// run 结束：清 cancel 句柄 + 插队队列，置闲。
    pub fn clear_active(&self) {
        *self.cancel_flag.lock().unwrap() = None;
        *self.pending_inputs.lock().unwrap() = None;
        self.active_run.store(false, Ordering::SeqCst);
    }

    /// 把一条 user 输入插进当前活 run 的队列（agent_loop 下个 drain 边界消费）。
    /// 无活 run 返回 `false`。
    pub fn inject(&self, content: String) -> bool {
        if let Some(inputs) = &*self.pending_inputs.lock().unwrap() {
            inputs.lock().unwrap().push(PendingUserInput {
                content,
                attachments: Vec::new(),
            });
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
        for (_id, tx) in self.pending_approvals.lock().unwrap().drain() {
            let _ = tx.send(ApprovalDecision::Deny);
        }
        for (_id, tx) in self.pending_questions.lock().unwrap().drain() {
            let _ = tx.send(UserAnswer::Cancelled);
        }
    }

    /// 结算一条审批（surface 回应到达时）。返回是否命中一个待结算项。
    pub fn resolve_approval(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        if let Some(tx) = self.pending_approvals.lock().unwrap().remove(request_id) {
            tx.send(decision).is_ok()
        } else {
            false
        }
    }

    /// 结算一条提问。返回是否命中一个待结算项。
    pub fn answer_question(&self, request_id: &str, answer: UserAnswer) -> bool {
        if let Some(tx) = self.pending_questions.lock().unwrap().remove(request_id) {
            tx.send(answer).is_ok()
        } else {
            false
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
        rt.emit(WireEvent::Error { message: "x".into() });
        assert!(matches!(a.try_recv(), Ok(WireEvent::Error { .. })));
        assert!(matches!(b.try_recv(), Ok(WireEvent::Error { .. })));
    }

    #[test]
    fn inject_requires_active_run() {
        let rt = SessionRuntimeState::new("s1", 16, RunMode::Default);
        assert!(!rt.inject("hi".into()), "无活 run 时 inject 应失败");
        let inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
        rt.set_active(Arc::new(AtomicBool::new(false)), inputs.clone());
        assert!(rt.inject("hi".into()));
        assert_eq!(inputs.lock().unwrap().len(), 1);
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
