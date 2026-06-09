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

/// Island 浮层快捷审批入口：支持 AllowOnce / Deny / AllowAndRemember。
/// `checked` 是用户在子命令列表中勾选的索引（空 = 全部勾选）。
pub fn resolve_hitl_from_island(
    app: &tauri::AppHandle,
    request_id: &str,
    decision_str: &str,
    _checked: Option<&[usize]>,
) {
    use std::sync::Arc;
    use tauri::Manager;

    let Some(state) = app.try_state::<Arc<HitlState>>() else {
        tracing::warn!("island_approve: HitlState not available");
        return;
    };
    let decision = match decision_str {
        "allow" => protocol::ApprovalDecision::AllowOnce,
        "deny" => protocol::ApprovalDecision::Deny,
        "allow_conversation" => protocol::ApprovalDecision::AllowAndRemember {
            scope: protocol::PermissionScope::Session,
            pattern: None,
            extra_patterns: vec![],
        },
        "allow_project" => protocol::ApprovalDecision::AllowAndRemember {
            scope: protocol::PermissionScope::Project,
            pattern: None,
            extra_patterns: vec![],
        },
        "allow_global" => protocol::ApprovalDecision::AllowAndRemember {
            scope: protocol::PermissionScope::Global,
            pattern: None,
            extra_patterns: vec![],
        },
        other => {
            tracing::warn!(decision = other, "island_approve: unknown decision");
            return;
        }
    };
    if let Err(e) = state.resolve_approval(request_id, decision) {
        tracing::warn!(error = %e, "island_approve: failed to resolve");
    }
}

/// Island 问答回答入口：支持 Selected / SelectedMulti / Custom / Cancelled。
/// `selected` 是用户选中的选项索引，`input` 是自由输入文本。
pub fn answer_question_from_island(
    app: &tauri::AppHandle,
    request_id: &str,
    action: &str,
    selected: Option<&[usize]>,
    input: Option<&str>,
) {
    use std::sync::Arc;
    use tauri::Manager;

    let Some(state) = app.try_state::<Arc<HitlState>>() else {
        tracing::warn!("island_answer: HitlState not available");
        return;
    };
    let answer = match action {
        "skip" => protocol::UserAnswer::Cancelled,
        "submit" => {
            // 优先用自由输入，其次用选中项
            if let Some(text) = input {
                protocol::UserAnswer::Custom {
                    text: text.to_string(),
                }
            } else if let Some(indices) = selected {
                if indices.len() == 1 {
                    // 单选：用索引占位，实际 label 由后端从 options 里取
                    protocol::UserAnswer::Selected {
                        label: format!("option_{}", indices[0]),
                    }
                } else {
                    protocol::UserAnswer::SelectedMulti {
                        labels: indices.iter().map(|i| format!("option_{i}")).collect(),
                    }
                }
            } else {
                protocol::UserAnswer::Cancelled
            }
        }
        other => {
            tracing::warn!(action = other, "island_answer: unknown action");
            return;
        }
    };
    if let Err(e) = state.answer_question(request_id, answer) {
        tracing::warn!(error = %e, "island_answer: failed to answer");
    }
}
