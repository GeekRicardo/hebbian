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
    /// 架构 §7.8.6 步骤⑥：run 移到 hebcore 进程后，HITL gate 在 hebcore 进程里，desktop
    /// 够不到本地 gate——改记 `request_id → session_id`，approve / answer 命令据此经
    /// hebcore 的 Approve / Answer 协议代理结算（见 [`HitlState::remote_session_of`]）。
    remote: Mutex<HashMap<String, String>>,
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

    /// 登记一条远端（hebcore）待结算 HITL：`request_id → session_id`（§7.8.6）。
    pub fn track_remote(&self, request_id: String, session_id: String) {
        self.remote.lock().unwrap().insert(request_id, session_id);
    }

    /// 查某 request_id 对应的远端 session（approve / answer 经 hebcore 代理时用）。**只读**
    /// 不消费——代理成功后才由 [`forget_remote`](Self::forget_remote) 移除，否则一次瞬时
    /// IPC 失败会让映射被消费、用户重试时找不到、请求在 hebcore 侧 gate 永挂（§7.8.6）。
    pub fn remote_session_of(&self, request_id: &str) -> Option<String> {
        self.remote.lock().unwrap().get(request_id).cloned()
    }

    /// 移除一条远端 HITL 映射（代理结算成功后调）。auto_handled 的请求由 sink 端不 track 规避。
    pub fn forget_remote(&self, request_id: &str) {
        self.remote.lock().unwrap().remove(request_id);
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
    // 架构 §7.8.6：run 在 hebcore 进程，HITL gate 也在那——先经 hebcore 的 Approve 协议代理
    // 结算（与主窗 approve_permission 同路径）；找不到远端映射才回退本地 gate。此前 island
    // 只走本地 resolve_approval、漏了 remote 分支，导致 run 移 hebcore 后 island 审批必然
    // 「找不到 request_id」（日志 island_approve: failed to resolve）。
    if let Some(session_id) = state.remote_session_of(request_id) {
        match crate::data_dir(app) {
            Ok(dd) => {
                match crate::hebcore_client::approve(&dd, &session_id, request_id, decision) {
                    Ok(()) => state.forget_remote(request_id),
                    Err(e) => tracing::warn!(error = %e, "island_approve: hebcore 代理结算失败"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "island_approve: data_dir 不可用"),
        }
        return;
    }
    if let Err(e) = state.resolve_approval(request_id, decision) {
        tracing::warn!(error = %e, "island_approve: failed to resolve");
    }
}

/// Island 问答回答入口。
///
/// `action`：`"submit"` → 用户提交，`answer` 携带 [`protocol::UserAnswer`] 的 wire JSON
/// （island 自己拼好真实 label / 多题 items，与主窗口走同一类型）；`"skip"` → 取消。
pub fn answer_question_from_island(
    app: &tauri::AppHandle,
    request_id: &str,
    action: &str,
    answer: Option<serde_json::Value>,
) {
    use std::sync::Arc;
    use tauri::Manager;

    let Some(state) = app.try_state::<Arc<HitlState>>() else {
        tracing::warn!("island_answer: HitlState not available");
        return;
    };
    let answer = match parse_island_answer(action, answer) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, action, "island_answer: 无法解析回传");
            return;
        }
    };
    // 架构 §7.8.6：同 island_approve——先经 hebcore 的 Answer 协议代理，回退本地 gate。
    if let Some(session_id) = state.remote_session_of(request_id) {
        match crate::data_dir(app) {
            Ok(dd) => {
                match crate::hebcore_client::answer(&dd, &session_id, request_id, answer) {
                    Ok(()) => state.forget_remote(request_id),
                    Err(e) => tracing::warn!(error = %e, "island_answer: hebcore 代理结算失败"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "island_answer: data_dir 不可用"),
        }
        return;
    }
    if let Err(e) = state.answer_question(request_id, answer) {
        tracing::warn!(error = %e, "island_answer: failed to answer");
    }
}

/// 把 island 回传的 `action` + `answer` JSON 规约成 [`protocol::UserAnswer`]。
///
/// island 端按 `protocol::UserAnswer` 的 wire 形态自己拼好答案（真实 label、多题 items），
/// 这里只做反序列化——不再有 `option_N` 索引占位需要后端翻译。
fn parse_island_answer(
    action: &str,
    answer: Option<serde_json::Value>,
) -> Result<protocol::UserAnswer, String> {
    match action {
        "skip" => Ok(protocol::UserAnswer::Cancelled),
        "submit" => match answer {
            Some(v) => serde_json::from_value::<protocol::UserAnswer>(v)
                .map_err(|e| format!("answer 反序列化失败: {e}")),
            None => Ok(protocol::UserAnswer::Cancelled),
        },
        other => Err(format!("未知 action: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_island_answer;
    use serde_json::json;

    /// island 单选回传须解析成带真实 label 的 Selected（而非历史的 option_N 占位）。
    #[test]
    fn island_single_selected_carries_real_label() {
        let ans = parse_island_answer("submit", Some(json!({"type": "selected", "label": "右上角"})))
            .unwrap();
        match ans {
            protocol::UserAnswer::Selected { label } => assert_eq!(label, "右上角"),
            other => panic!("期望 Selected，得到 {other:?}"),
        }
    }

    /// island 多题回传须解析成 Multi，每项带 title + 子答案。
    #[test]
    fn island_multi_answer_roundtrips() {
        let payload = json!({
            "type": "multi",
            "items": [
                {"title": "策略", "answer": {"type": "selected", "label": "A"}},
                {"title": "范围", "answer": {"type": "selected_multi", "labels": ["x", "y"]}},
                {"title": "备注", "answer": {"type": "custom", "text": "随便写写"}},
            ]
        });
        let ans = parse_island_answer("submit", Some(payload)).unwrap();
        match ans {
            protocol::UserAnswer::Multi { items } => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].title, "策略");
                assert!(matches!(&items[1].answer, protocol::SingleAnswer::SelectedMulti { labels } if labels == &["x", "y"]));
            }
            other => panic!("期望 Multi，得到 {other:?}"),
        }
    }

    #[test]
    fn island_skip_is_cancelled() {
        let ans = parse_island_answer("skip", None).unwrap();
        assert!(matches!(ans, protocol::UserAnswer::Cancelled));
    }
}
