//! HITL（Human-in-the-Loop）统一通道：审批 / 提问 / 路径越界 / 长 run 续跑共用一张
//! pending 表，按 `PermissionRequestId` 索引，用 oneshot waiter 解除挂起。
//!
//! 两种 pending 互不干扰：
//! - **审批**（[`HitlGate::check`] / [`HitlGate::open_approval`]）：用户回 [`ApprovalDecision`]
//! - **提问**（[`HitlGate::open_question`]）：用户回 [`UserAnswer`]
//!
//! Surface 端（CLI / Desktop / Server）只看到统一的 `request_id`，靠事件 payload 的
//! kind 字段判断该用审批 UI 还是提问 UI。

use std::collections::HashMap;
use std::sync::Mutex;

use protocol::{ApprovalDecision, PermissionRequestId, PermissionScope, UserAnswer};
use tokio::sync::oneshot;

use crate::definition::{DefaultPermission, PermissionPolicy};
use crate::tools::ToolClass;

/// 单次工具调用的权限决策结果。
#[derive(Debug)]
pub enum PermissionDecision {
    /// 自动批准，直接执行。
    Approved,
    /// 被策略拒绝，不执行。
    Denied { reason: String },
    /// 需要用户交互确认。waiter 在用户回应后被 resolve。
    NeedsApproval {
        request_id: PermissionRequestId,
        waiter: oneshot::Receiver<ApprovalDecision>,
    },
}

/// 内部 pending 条目：审批与提问共用一张表。
enum Pending {
    Approval(oneshot::Sender<ApprovalDecision>),
    Question(oneshot::Sender<UserAnswer>),
}

/// 累计的会话级"Allow & Remember"规则。
#[derive(Debug, Clone, Default)]
struct LearnedRules {
    auto_approved: Vec<String>,
    auto_denied: Vec<String>,
}

/// HITL 统一闸门。
pub struct HitlGate {
    policy: PermissionPolicy,
    pending: Mutex<HashMap<PermissionRequestId, Pending>>,
    learned: Mutex<LearnedRules>,
}

impl HitlGate {
    pub fn new(policy: PermissionPolicy) -> Self {
        Self {
            policy,
            pending: Mutex::new(HashMap::new()),
            learned: Mutex::new(LearnedRules::default()),
        }
    }

    /// 评估一次工具调用：依 `ToolClass` 默认行为 + 用户累计规则 + 策略规则三层判断。
    ///
    /// 返回 `NeedsApproval` 时，调用方应：
    /// 1. emit `PermissionRequested` 事件（用 request_id）
    /// 2. await waiter
    /// 3. 收到 `ApprovalDecision` 后决定是否执行
    pub fn check(&self, tool_name: &str, class: &ToolClass) -> PermissionDecision {
        // 1) NeedsHumanInput 不走审批（dispatcher 走 ask 路径）
        if matches!(class, ToolClass::NeedsHumanInput { .. }) {
            return PermissionDecision::Approved;
        }

        // 2) 用户累计规则优先
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

        // 3) 静态策略按名命中
        if self.policy.auto_approve.iter().any(|n| n == tool_name) {
            return PermissionDecision::Approved;
        }
        if self.policy.always_ask.iter().any(|n| n == tool_name) {
            return self.needs_approval();
        }

        // 4) 按 ToolClass 默认行为
        match class {
            ToolClass::ReadOnly => PermissionDecision::Approved,
            ToolClass::Network | ToolClass::Mutating { .. } | ToolClass::Destructive { .. } => {
                match self.policy.default_action {
                    DefaultPermission::Auto => PermissionDecision::Approved,
                    DefaultPermission::Ask => self.needs_approval(),
                    DefaultPermission::Deny => PermissionDecision::Denied {
                        reason: "策略默认拒绝".into(),
                    },
                }
            }
            ToolClass::NeedsHumanInput { .. } => unreachable!("已在第 1 步短路"),
        }
    }

    /// 显式开一张审批 pending（路径越界、长 run 续跑等无法用 `check` 表达的场景）。
    /// 跳过策略，调用方直接拿 `(id, waiter)` 自行 emit 事件并 await。
    pub fn open_approval(&self) -> (PermissionRequestId, oneshot::Receiver<ApprovalDecision>) {
        let request_id = PermissionRequestId::new();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), Pending::Approval(tx));
        (request_id, rx)
    }

    /// 开一张提问 pending（ask 工具）。
    pub fn open_question(&self) -> (PermissionRequestId, oneshot::Receiver<UserAnswer>) {
        let request_id = PermissionRequestId::new();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), Pending::Question(tx));
        (request_id, rx)
    }

    /// Surface 提交审批结果，唤醒对应 waiter。
    /// 当 `decision` 为 `AllowAndRemember` 且 scope 是 Session/Run 时累计 learned 规则。
    pub fn resolve(
        &self,
        request_id: &PermissionRequestId,
        decision: ApprovalDecision,
        tool_name: Option<&str>,
    ) {
        if let (Some(name), ApprovalDecision::AllowAndRemember { scope }) = (tool_name, &decision) {
            if matches!(scope, PermissionScope::Session | PermissionScope::Run) {
                self.learned
                    .lock()
                    .unwrap()
                    .auto_approved
                    .push(name.to_string());
            }
        }

        if let Some(Pending::Approval(tx)) = self.pending.lock().unwrap().remove(request_id) {
            let _ = tx.send(decision);
        }
    }

    /// Surface 提交提问回应，唤醒对应 waiter。
    pub fn answer(&self, request_id: &PermissionRequestId, answer: UserAnswer) {
        if let Some(Pending::Question(tx)) = self.pending.lock().unwrap().remove(request_id) {
            let _ = tx.send(answer);
        }
    }

    /// 取消所有未决（run 被 interrupt 时调用）。
    /// 审批默认 Deny；提问默认 Cancelled。
    pub fn cancel_all_pending(&self) {
        let mut pending = self.pending.lock().unwrap();
        for (_id, entry) in pending.drain() {
            match entry {
                Pending::Approval(tx) => {
                    let _ = tx.send(ApprovalDecision::Deny);
                }
                Pending::Question(tx) => {
                    let _ = tx.send(UserAnswer::Cancelled);
                }
            }
        }
    }

    fn needs_approval(&self) -> PermissionDecision {
        let (request_id, waiter) = self.open_approval();
        PermissionDecision::NeedsApproval { request_id, waiter }
    }
}

impl Default for HitlGate {
    fn default() -> Self {
        Self::new(PermissionPolicy::default())
    }
}
