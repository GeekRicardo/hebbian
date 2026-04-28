use std::collections::HashMap;
use std::sync::Mutex;

use protocol::{ApprovalDecision, PermissionRequestId, PermissionScope};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::definition::{DefaultPermission, PermissionPolicy};

/// 单次工具调用的权限决策结果
#[derive(Debug)]
pub enum PermissionDecision {
    /// 自动批准，直接执行
    Approved,
    /// 被策略拒绝，不执行
    Denied { reason: String },
    /// 需要用户交互确认。waiter 在用户回应后被 resolve。
    NeedsApproval {
        request_id: PermissionRequestId,
        waiter: oneshot::Receiver<ApprovalDecision>,
    },
}

/// 累计的会话级"Allow & Remember"规则
#[derive(Debug, Clone, Default)]
struct LearnedRules {
    /// (tool_name, scope) → AllowOnce 等价（即直接 approved）
    auto_approved: Vec<String>,
    auto_denied: Vec<String>,
}

/// 权限门：根据策略与累计规则决定工具能否执行。
///
/// 三态语义：
/// - 策略明确允许 → Approved
/// - 策略明确拒绝 → Denied
/// - 策略说"问"或未匹配且默认 Ask → NeedsApproval（携带 oneshot waiter）
///
/// NeedsApproval 时会同时在 `pending` 中登记 sender，等待 `resolve()` 唤醒。
pub struct PermissionGate {
    policy: PermissionPolicy,
    pending: Mutex<HashMap<PermissionRequestId, oneshot::Sender<ApprovalDecision>>>,
    learned: Mutex<LearnedRules>,
}

impl PermissionGate {
    pub fn new(policy: PermissionPolicy) -> Self {
        Self {
            policy,
            pending: Mutex::new(HashMap::new()),
            learned: Mutex::new(LearnedRules::default()),
        }
    }

    /// 评估一次工具调用。
    ///
    /// 返回 `NeedsApproval` 时，调用方应：
    /// 1. emit `PermissionRequested` 事件（用 request_id）
    /// 2. await waiter
    /// 3. 收到 `ApprovalDecision` 后决定是否执行
    pub fn check(&self, tool_name: &str, _input: &Value) -> PermissionDecision {
        // 1. 先看会话级累计规则
        {
            let learned = self.learned.lock().unwrap();
            if learned.auto_approved.iter().any(|n| n == tool_name) {
                return PermissionDecision::Approved;
            }
            if learned.auto_denied.iter().any(|n| n == tool_name) {
                return PermissionDecision::Denied {
                    reason: "用户已永久拒绝该工具".into(),
                };
            }
        }

        // 2. 静态策略
        if self.policy.auto_approve.iter().any(|n| n == tool_name) {
            return PermissionDecision::Approved;
        }
        if self.policy.always_ask.iter().any(|n| n == tool_name) {
            return self.create_pending();
        }
        match self.policy.default_action {
            DefaultPermission::Auto => PermissionDecision::Approved,
            DefaultPermission::Ask => self.create_pending(),
            DefaultPermission::Deny => PermissionDecision::Denied {
                reason: "策略默认拒绝".into(),
            },
        }
    }

    fn create_pending(&self) -> PermissionDecision {
        let request_id = PermissionRequestId::new();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), tx);
        PermissionDecision::NeedsApproval {
            request_id,
            waiter: rx,
        }
    }

    /// 由 Harness 在收到 `Op::Approve` 时调用，解锁等待中的 waiter。
    /// 同时根据 decision 累计 learned rules。
    pub fn resolve(&self, request_id: &PermissionRequestId, decision: ApprovalDecision, tool_name: Option<&str>) {
        // 累计学习规则
        if let Some(name) = tool_name {
            match &decision {
                ApprovalDecision::AllowAndRemember { scope } => {
                    if matches!(scope, PermissionScope::Session | PermissionScope::Run) {
                        self.learned
                            .lock()
                            .unwrap()
                            .auto_approved
                            .push(name.to_string());
                    }
                }
                _ => {}
            }
        }

        if let Some(tx) = self.pending.lock().unwrap().remove(request_id) {
            let _ = tx.send(decision);
        }
    }

    /// 取消所有未决审批（run 被 interrupt 时调用）
    pub fn cancel_all_pending(&self) {
        let mut pending = self.pending.lock().unwrap();
        for (_id, tx) in pending.drain() {
            let _ = tx.send(ApprovalDecision::Deny);
        }
    }
}

impl Default for PermissionGate {
    fn default() -> Self {
        Self::new(PermissionPolicy::default())
    }
}
