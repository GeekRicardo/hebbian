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
use common::runtime::CancelFlag;

#[derive(Default)]
pub struct HitlState {
    pending: Mutex<HashMap<String, Arc<HitlGate>>>,
    runs: Mutex<HashMap<String, (CancelFlag, Arc<HitlGate>)>>,
}

impl HitlState {
    /// 关联一次前端 request_id 到当前 run 的取消标志和 HitlGate。
    pub fn track_run(&self, request_id: String, cancel: CancelFlag, gate: Arc<HitlGate>) {
        self.runs.lock().unwrap().insert(request_id, (cancel, gate));
    }

    /// 关联 `request_id` 到当前 run 的 HitlGate。
    pub fn track(&self, request_id: String, gate: Arc<HitlGate>) {
        self.pending.lock().unwrap().insert(request_id, gate);
    }

    pub fn cancel_run(&self, request_id: &str) -> bool {
        let Some((cancel, gate)) = self.runs.lock().unwrap().get(request_id).cloned() else {
            return false;
        };
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        gate.cancel_all_pending();
        true
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
        self.runs
            .lock()
            .unwrap()
            .retain(|_id, (_cancel, g)| !Arc::ptr_eq(g, gate));
    }

    /// 把所有 pending 的审批 / 提问按"取消"resolve；用于 desktop 关窗等场景，
    /// 让正在 await 的 spawn_ask / spawn_tool 即刻醒来收尾。
    /// 返回被取消的 gate 唯一数量（去重后）。
    pub fn cancel_all_pending(&self) -> usize {
        let gates: Vec<Arc<HitlGate>> = {
            let mut seen: Vec<Arc<HitlGate>> = Vec::new();
            let mut pending = self.pending.lock().unwrap();
            for gate in pending.values() {
                if !seen.iter().any(|g| Arc::ptr_eq(g, gate)) {
                    seen.push(gate.clone());
                }
            }
            pending.clear();
            drop(pending);

            let mut runs = self.runs.lock().unwrap();
            for (cancel, gate) in runs.values() {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                if !seen.iter().any(|g| Arc::ptr_eq(g, gate)) {
                    seen.push(gate.clone());
                }
            }
            runs.clear();
            seen
        };
        let count = gates.len();
        for gate in gates {
            gate.cancel_all_pending();
        }
        count
    }

    /// 当前是否有未 resolve 的 HITL 请求。
    pub fn has_pending(&self) -> bool {
        !self.pending.lock().unwrap().is_empty()
    }
}
