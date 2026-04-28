//! Agent 主动提问通路（与 PermissionGate 平行）。
//!
//! Agent loop 在派发 `ask` 工具时不走 `ToolRegistry`，而是：
//! 1. `QuestionGate::create_pending()` 拿到 `(request_id, waiter)`
//! 2. emit `Event::UserQuestionRequested { request_id, question, options }`
//! 3. await `waiter`，期间 surface 通过 `gate.answer(...)` 唤醒
//! 4. 把 `UserAnswer` 转成 `tool_result.content` 回灌 transcript
//!
//! 取消语义：surface 显式回 `UserAnswer::Cancelled`，或 run 被 interrupt 时
//! `cancel_all_pending()` 把所有未决 question 标 Cancelled。

use std::collections::HashMap;
use std::sync::Mutex;

use protocol::{PermissionRequestId, UserAnswer};
use tokio::sync::oneshot;

pub struct QuestionGate {
    pending: Mutex<HashMap<PermissionRequestId, oneshot::Sender<UserAnswer>>>,
}

impl QuestionGate {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// 创建一个待回应的 question，返回 (id, waiter)。
    /// agent_loop 拿到 id 后 emit `UserQuestionRequested` 事件，再 await waiter。
    pub fn create_pending(&self) -> (PermissionRequestId, oneshot::Receiver<UserAnswer>) {
        let request_id = PermissionRequestId::new();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), tx);
        (request_id, rx)
    }

    /// 用户回应。如果 request_id 不存在（已被取消或 resolve 过），无操作。
    pub fn answer(&self, request_id: &PermissionRequestId, answer: UserAnswer) {
        if let Some(tx) = self.pending.lock().unwrap().remove(request_id) {
            let _ = tx.send(answer);
        }
    }

    /// 取消所有未决（run 被 interrupt 时调用）
    pub fn cancel_all_pending(&self) {
        let mut pending = self.pending.lock().unwrap();
        for (_id, tx) in pending.drain() {
            let _ = tx.send(UserAnswer::Cancelled);
        }
    }
}

impl Default for QuestionGate {
    fn default() -> Self {
        Self::new()
    }
}
