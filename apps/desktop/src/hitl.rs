//! HITL（Human-in-the-Loop）审批桥接。
//!
//! `chat::send_and_save` 在调用 `Harness::run_with_gate` 时把 `Arc<PermissionGate>`
//! 注册到这个 state；UI 收到 `PermissionRequested` 事件时再把 `(request_id → gate)`
//! 登记进 `pending`；Tauri 命令 `approve_permission` 通过 request_id 找到 gate 并
//! 调 `gate.resolve(...)`。
//!
//! 当 run 结束（无论成功 / 失败 / 取消），`Drop` 自动清理。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_core::tools::permissions::PermissionGate;

#[derive(Default)]
pub struct PendingApprovals {
    by_request_id: HashMap<String, Arc<PermissionGate>>,
}

#[derive(Default)]
pub struct HitlState {
    inner: Mutex<PendingApprovals>,
}

impl HitlState {
    /// 把一个 request_id 关联到对应的 gate（在 chat.rs 的事件回调里调用）。
    pub fn register(&self, request_id: String, gate: Arc<PermissionGate>) {
        self.inner
            .lock()
            .unwrap()
            .by_request_id
            .insert(request_id, gate);
    }

    /// 由 Tauri 命令 `approve_permission` 调用：找到对应 gate，转发 decision。
    pub fn resolve(
        &self,
        request_id: &str,
        decision: protocol::ApprovalDecision,
    ) -> Result<(), String> {
        let gate = self
            .inner
            .lock()
            .unwrap()
            .by_request_id
            .remove(request_id)
            .ok_or_else(|| format!("找不到待审批的 request_id: {request_id}"))?;
        gate.resolve(
            &protocol::PermissionRequestId::from_raw(request_id),
            decision,
            None,
        );
        Ok(())
    }

    /// 清理一个 run 留下的所有 pending（run 结束时调用，避免泄漏）。
    /// 这里通过 gate 引用相等性来匹配。
    pub fn unregister_gate(&self, gate: &Arc<PermissionGate>) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .by_request_id
            .retain(|_id, g| !Arc::ptr_eq(g, gate));
    }
}
