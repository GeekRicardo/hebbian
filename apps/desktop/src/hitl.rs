//! HITL（Human-in-the-Loop）桥接：审批 + ask 提问。
//!
//! `chat::send_and_save` 在 spawn run 之前把 `Arc<PermissionGate>` /
//! `Arc<QuestionGate>` 关联到当前事件流；事件回调里看到
//! `PermissionRequested` / `UserQuestionRequested` 时把 `(request_id → gate)`
//! 注册进 `pending_*`；Tauri 命令 `approve_permission` /
//! `answer_question` 通过 request_id 找到对应 gate 并 resolve。
//!
//! Run 结束时通过 `unregister_*` 清掉残留映射，避免泄漏。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_core::tools::{permissions::PermissionGate, question::QuestionGate};

#[derive(Default)]
struct Inner {
    pending_approvals: HashMap<String, Arc<PermissionGate>>,
    pending_questions: HashMap<String, Arc<QuestionGate>>,
}

#[derive(Default)]
pub struct HitlState {
    inner: Mutex<Inner>,
}

impl HitlState {
    // ── 审批 ─────────────────────────────────────────────────

    pub fn register_approval(&self, request_id: String, gate: Arc<PermissionGate>) {
        self.inner
            .lock()
            .unwrap()
            .pending_approvals
            .insert(request_id, gate);
    }

    pub fn resolve_approval(
        &self,
        request_id: &str,
        decision: protocol::ApprovalDecision,
    ) -> Result<(), String> {
        let gate = self
            .inner
            .lock()
            .unwrap()
            .pending_approvals
            .remove(request_id)
            .ok_or_else(|| format!("找不到待审批的 request_id: {request_id}"))?;
        gate.resolve(
            &protocol::PermissionRequestId::from_raw(request_id),
            decision,
            None,
        );
        Ok(())
    }

    pub fn unregister_approval_gate(&self, gate: &Arc<PermissionGate>) {
        let mut inner = self.inner.lock().unwrap();
        inner.pending_approvals.retain(|_id, g| !Arc::ptr_eq(g, gate));
    }

    // ── Ask 提问 ─────────────────────────────────────────────

    pub fn register_question(&self, request_id: String, gate: Arc<QuestionGate>) {
        self.inner
            .lock()
            .unwrap()
            .pending_questions
            .insert(request_id, gate);
    }

    pub fn answer_question(
        &self,
        request_id: &str,
        answer: protocol::UserAnswer,
    ) -> Result<(), String> {
        let gate = self
            .inner
            .lock()
            .unwrap()
            .pending_questions
            .remove(request_id)
            .ok_or_else(|| format!("找不到待回答的 question request_id: {request_id}"))?;
        gate.answer(&protocol::PermissionRequestId::from_raw(request_id), answer);
        Ok(())
    }

    pub fn unregister_question_gate(&self, gate: &Arc<QuestionGate>) {
        let mut inner = self.inner.lock().unwrap();
        inner.pending_questions.retain(|_id, g| !Arc::ptr_eq(g, gate));
    }
}
