//! HITL 桥接：审批 / 提问 共用一张表。
//!
//! `chat::send_and_save` 在 spawn run 前后把 `Arc<HitlGate>` 关联到
//! 当前事件流；事件回调里看到 `PermissionRequested` / `UserQuestionRequested`
//! 时调 [`HitlState::track`] 把 `request_id → HitlGate` 注册进表。
//!
//! Tauri 命令 `approve_permission` / `answer_question` 通过 request_id
//! 找到对应 HitlGate 并 resolve / answer。
//!
//! Run 结束时调 [`HitlState::forget`] 清掉残留映射，避免泄漏。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_core::tools::hitl::HitlGate;

#[derive(Default)]
pub struct HitlState {
    pending: Mutex<HashMap<String, Arc<HitlGate>>>,
}

impl HitlState {
    /// 关联 `request_id` 到当前 run 的 HitlGate。
    pub fn track(&self, request_id: String, gate: Arc<HitlGate>) {
        self.pending.lock().unwrap().insert(request_id, gate);
    }

    pub fn resolve_approval(
        &self,
        request_id: &str,
        decision: protocol::ApprovalDecision,
    ) -> Result<(), String> {
        let gate = self
            .pending
            .lock()
            .unwrap()
            .remove(request_id)
            .ok_or_else(|| format!("找不到 request_id: {request_id}"))?;
        gate.resolve(
            &protocol::PermissionRequestId::from_raw(request_id),
            decision,
            None,
        );
        Ok(())
    }

    pub fn answer_question(
        &self,
        request_id: &str,
        answer: protocol::UserAnswer,
    ) -> Result<(), String> {
        let gate = self
            .pending
            .lock()
            .unwrap()
            .remove(request_id)
            .ok_or_else(|| format!("找不到 request_id: {request_id}"))?;
        gate.answer(&protocol::PermissionRequestId::from_raw(request_id), answer);
        Ok(())
    }

    /// run 结束时清掉指向该 gate 的所有残留映射。
    pub fn forget(&self, gate: &Arc<HitlGate>) {
        self.pending
            .lock()
            .unwrap()
            .retain(|_id, g| !Arc::ptr_eq(g, gate));
    }
}
