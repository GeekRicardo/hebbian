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

/// 一次 run 的运行时控制点：取消标志 + pending 输入队列。
#[derive(Debug, Clone)]
pub struct RuntimeHandle {
    pub cancel: CancelFlag,
    pub pending_inputs: PendingInputs,
}

static REGISTRY: std::sync::OnceLock<Mutex<HashMap<String, RuntimeHandle>>> =
    std::sync::OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, RuntimeHandle>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register(request_id: String) -> RuntimeHandle {
    let handle = RuntimeHandle {
        cancel: Arc::new(AtomicBool::new(false)),
        pending_inputs: Arc::new(Mutex::new(Vec::new())),
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

/// 把一条 pending input 推入指定 request_id 的运行时队列。
/// 返回 `false` 表示 request_id 已经不存在（run 结束 / 还没注册）。
pub fn inject_pending_input(request_id: &str, input: PendingUserInput) -> bool {
    if let Some(handle) = registry().lock().unwrap().get(request_id) {
        handle.pending_inputs.lock().unwrap().push(input);
        return true;
    }
    false
}

pub fn unregister(request_id: &str) {
    registry().lock().unwrap().remove(request_id);
}

pub fn is_cancelled(flag: &CancelFlag) -> bool {
    flag.load(Ordering::SeqCst)
}
