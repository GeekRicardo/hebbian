use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use crate::attachments::MessageAttachment;

pub type CancelFlag = Arc<AtomicBool>;

/// 运行时注入的 user message。streaming 中前端"立即发送"会推一条进
/// 当前 run 的 pending 队列；agent_loop 在每次 model.request 之前 drain 出来
/// 作为新的 user message 加入 transcript，让模型在下一次 iteration 立刻看到。
#[derive(Debug, Clone)]
pub struct PendingUserInput {
    pub content: String,
    pub attachments: Vec<MessageAttachment>,
}

pub type PendingInputs = Arc<Mutex<Vec<PendingUserInput>>>;

/// 已被 agent_loop 从 [`PendingInputs`] 消费的插队输入。
///
/// Desktop 需要等 Run 结束后再把这些 user message 落盘，保证历史顺序是：
/// 正在输出的 assistant → 插队 user → 后续 assistant。
pub type ConsumedPendingInputs = Arc<Mutex<Vec<PendingUserInput>>>;

/// 一次 run 的运行时控制点：取消标志 + pending 输入队列。
#[derive(Debug, Clone)]
pub struct RuntimeHandle {
    pub session_id: Option<String>,
    pub cancel: CancelFlag,
    pub pending_inputs: PendingInputs,
    pub consumed_pending_inputs: ConsumedPendingInputs,
    pub accepting_pending_inputs: Arc<AtomicBool>,
}

static REGISTRY: std::sync::OnceLock<Mutex<HashMap<String, RuntimeHandle>>> =
    std::sync::OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, RuntimeHandle>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register(request_id: String) -> RuntimeHandle {
    register_for_session(request_id, None)
}

pub fn register_for_session(request_id: String, session_id: Option<String>) -> RuntimeHandle {
    let handle = RuntimeHandle {
        session_id,
        cancel: Arc::new(AtomicBool::new(false)),
        pending_inputs: Arc::new(Mutex::new(Vec::new())),
        consumed_pending_inputs: Arc::new(Mutex::new(Vec::new())),
        accepting_pending_inputs: Arc::new(AtomicBool::new(true)),
    };
    registry()
        .lock()
        .unwrap()
        .insert(request_id, handle.clone());
    handle
}

pub fn cancel(request_id: &str) -> bool {
    if let Some(handle) = registry().lock().unwrap().get(request_id) {
        handle.cancel.store(true, Ordering::SeqCst);
        return true;
    }
    false
}

/// 把所有当前注册的 run 标记为 cancelled。
/// 用于 desktop 关窗等需要批量中断的场景；返回被标记的 run 数量。
pub fn cancel_all() -> usize {
    let registry = registry().lock().unwrap();
    for handle in registry.values() {
        handle.cancel.store(true, Ordering::SeqCst);
    }
    registry.len()
}

/// 当前是否有已注册（in-flight）的 run。
pub fn has_active_runs() -> bool {
    !registry().lock().unwrap().is_empty()
}

/// Whether a specific session/request pair is still registered as an in-flight run.
pub fn has_active_run_for_session(request_id: &str, session_id: &str) -> bool {
    registry()
        .lock()
        .unwrap()
        .get(request_id)
        .is_some_and(|handle| handle.session_id.as_deref() == Some(session_id))
}

/// 把一条 pending input 推入指定 request_id 的运行时队列。
/// 返回 `false` 表示 request_id 已经不存在（run 结束 / 还没注册）。
pub fn inject_pending_input(request_id: &str, input: PendingUserInput) -> bool {
    if let Some(handle) = registry().lock().unwrap().get(request_id) {
        if !handle.accepting_pending_inputs.load(Ordering::SeqCst) {
            return false;
        }
        handle.pending_inputs.lock().unwrap().push(input);
        return true;
    }
    false
}

pub fn close_pending_inputs(request_id: &str) -> bool {
    if let Some(handle) = registry().lock().unwrap().get(request_id) {
        close_pending_inputs_handle(handle);
        return true;
    }
    false
}

pub fn close_pending_inputs_handle(handle: &RuntimeHandle) {
    handle
        .accepting_pending_inputs
        .store(false, Ordering::SeqCst);
}

pub fn unregister(request_id: &str) {
    registry().lock().unwrap().remove(request_id);
}

pub fn is_cancelled(flag: &CancelFlag) -> bool {
    flag.load(Ordering::SeqCst)
}
