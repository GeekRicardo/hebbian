//! HITL（Human-in-the-Loop）统一通道：审批 / 提问 / 路径越界 / 长 run 续跑共用一张
//! pending 表，按 `PermissionRequestId` 索引，用 oneshot waiter 解除挂起。
//!
//! 两种 pending 互不干扰：
//! - **审批**（[`HitlGate::check`] / [`HitlGate::open_approval`]）：用户回 [`ApprovalDecision`]
//! - **提问**（[`HitlGate::open_question`]）：用户回 [`UserAnswer`]
//!
//! Surface 端（CLI / Desktop / Server）只看到统一的 `request_id`，靠事件 payload 的
//! kind 字段判断该用审批 UI 还是提问 UI。
//!
//! ## 记忆粒度
//!
//! 审批被批准并选择"记住"时，按下面顺序生效：
//! - `pattern = Some(s)` → 记 `(tool_name, s)` 命令前缀对，下次同前缀直接放行
//! - `pattern = None` 且工具不在 [`NO_TOOL_LEVEL_MEMORY`] → 工具名级记忆（旧行为）
//! - `pattern = None` 且工具在 [`NO_TOOL_LEVEL_MEMORY`] → 不记忆（兜底为 `AllowOnce`）
//!
//! 前缀匹配按空白 token 边界（`fingerprint == prefix` 或 `fingerprint.starts_with(prefix + " ")`），
//! 避免 `"git statusbad"` 误中 `"git status"`。

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
    Approval {
        sender: oneshot::Sender<ApprovalDecision>,
        /// resolve 时若 decision = AllowAndRemember 用来定位记忆条目。
        /// 路径越界审批走 `open_approval(None, None)` 不写 learned 表。
        tool_name: Option<String>,
        fingerprint: Option<String>,
    },
    Question(oneshot::Sender<UserAnswer>),
}

/// 累计的会话级 "Allow & Remember" 规则。
#[derive(Debug, Clone, Default)]
struct LearnedRules {
    /// 工具名级允许（旧行为）。
    auto_approved_tools: Vec<String>,
    /// 命令前缀级允许：`(tool_name, prefix)`，按空白 token 边界匹配。
    auto_approved_patterns: Vec<(String, String)>,
    /// 工具名级永久拒绝。
    auto_denied_tools: Vec<String>,
}

/// 不允许做工具名级记忆的工具：粒度太粗会引入安全 bug。
///
/// 示例：用户对 `ls` 选了 "AllowAndRemember"（`pattern=None`），若按工具名记忆
/// 则之后所有 Bash 调用（包括 `rm -rf /`）都会自动放行。这些工具应当走命令前缀级
/// 记忆（`pattern=Some(...)`），UI 应只暴露前缀按钮。
const NO_TOOL_LEVEL_MEMORY: &[&str] = &["Bash"];

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
    /// `fingerprint` 是工具自报的命令级指纹（[`super::Tool::permission_fingerprint`]）。
    /// 当工具支持命令级记忆时传入，HitlGate 优先按 patterns 前缀匹配；缺省 `None` 退回
    /// 工具名级判断。
    ///
    /// 返回 `NeedsApproval` 时，调用方应：
    /// 1. emit `PermissionRequested` 事件（用 request_id）
    /// 2. await waiter
    /// 3. 收到 `ApprovalDecision` 后决定是否执行
    pub fn check(
        &self,
        tool_name: &str,
        class: &ToolClass,
        fingerprint: Option<&str>,
    ) -> PermissionDecision {
        // 1) NeedsHumanInput 不走审批（dispatcher 走 ask 路径）
        if matches!(class, ToolClass::NeedsHumanInput { .. }) {
            return PermissionDecision::Approved;
        }

        // 2) 用户显式"永久拒绝"优先于一切
        {
            let learned = self.learned.lock().unwrap();
            if learned.auto_denied_tools.iter().any(|n| n == tool_name) {
                return PermissionDecision::Denied {
                    reason: "用户已永久拒绝该工具".into(),
                };
            }
        }

        // 3) ReadOnly 永远放行：工具自报无副作用，always_ask 不该再拦。
        //    例如 BashTool 把 `ls` 解析成 ReadOnly 后即便 always_ask 含 "Bash" 也直接通过。
        if matches!(class, ToolClass::ReadOnly) {
            return PermissionDecision::Approved;
        }

        // 4) 用户累计的"允许并记住"——先看命令前缀，再看工具名
        {
            let learned = self.learned.lock().unwrap();
            if let Some(fp) = fingerprint {
                if learned
                    .auto_approved_patterns
                    .iter()
                    .any(|(t, prefix)| t == tool_name && fingerprint_matches(fp, prefix))
                {
                    return PermissionDecision::Approved;
                }
            }
            if learned.auto_approved_tools.iter().any(|n| n == tool_name) {
                return PermissionDecision::Approved;
            }
        }

        // 5) 静态策略按名命中
        if self.policy.auto_approve.iter().any(|n| n == tool_name) {
            return PermissionDecision::Approved;
        }
        if self.policy.always_ask.iter().any(|n| n == tool_name) {
            return self.needs_approval(tool_name, fingerprint);
        }

        // 6) 按 ToolClass 默认行为
        match class {
            ToolClass::Network | ToolClass::Mutating { .. } | ToolClass::Destructive { .. } => {
                match self.policy.default_action {
                    DefaultPermission::Auto => PermissionDecision::Approved,
                    DefaultPermission::Ask => self.needs_approval(tool_name, fingerprint),
                    DefaultPermission::Deny => PermissionDecision::Denied {
                        reason: "策略默认拒绝".into(),
                    },
                }
            }
            ToolClass::ReadOnly | ToolClass::NeedsHumanInput { .. } => {
                unreachable!("已在前面分支短路")
            }
        }
    }

    /// 显式开一张审批 pending（路径越界、长 run 续跑等无法用 `check` 表达的场景）。
    /// 跳过策略，调用方直接拿 `(id, waiter)` 自行 emit 事件并 await。
    ///
    /// 可选 `tool_name` / `fingerprint` 让 resolve 时能落 learned 表。路径越界不在
    /// 工具名维度，应传 `None`。
    pub fn open_approval(
        &self,
        tool_name: Option<&str>,
        fingerprint: Option<&str>,
    ) -> (PermissionRequestId, oneshot::Receiver<ApprovalDecision>) {
        let request_id = PermissionRequestId::new();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            request_id.clone(),
            Pending::Approval {
                sender: tx,
                tool_name: tool_name.map(str::to_owned),
                fingerprint: fingerprint.map(str::to_owned),
            },
        );
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
    /// 当 `decision` 为 `AllowAndRemember` 且 scope 是 Session/Run 时按 pending 中保存的
    /// tool_name / fingerprint 写入 learned 表。
    pub fn resolve(&self, request_id: &PermissionRequestId, decision: ApprovalDecision) {
        let entry = self.pending.lock().unwrap().remove(request_id);
        let Some(Pending::Approval {
            sender,
            tool_name,
            fingerprint,
        }) = entry
        else {
            return;
        };

        if let ApprovalDecision::AllowAndRemember { scope, pattern } = &decision {
            if matches!(scope, PermissionScope::Session | PermissionScope::Run) {
                if let Some(name) = &tool_name {
                    let mut learned = self.learned.lock().unwrap();
                    match (pattern.as_deref(), NO_TOOL_LEVEL_MEMORY.contains(&name.as_str())) {
                        (Some(prefix), _) => {
                            // 显式给了命令前缀 → 命令级记忆，工具名是否在黑名单都接受
                            let prefix = prefix.trim();
                            if !prefix.is_empty() {
                                let entry = (name.clone(), prefix.to_string());
                                if !learned.auto_approved_patterns.contains(&entry) {
                                    learned.auto_approved_patterns.push(entry);
                                }
                            }
                        }
                        (None, false) => {
                            // 没给前缀 + 工具允许工具名记忆 → 工具名级
                            if !learned.auto_approved_tools.contains(name) {
                                learned.auto_approved_tools.push(name.clone());
                            }
                        }
                        (None, true) => {
                            // 黑名单工具但没给前缀：保守起见这次不记忆，等价 AllowOnce
                            tracing::debug!(
                                tool = %name,
                                "AllowAndRemember 没有 pattern，工具在 NO_TOOL_LEVEL_MEMORY 黑名单，本次不记忆"
                            );
                        }
                    }
                    // fingerprint 仅做 debug 提示
                    let _ = fingerprint;
                }
            }
        }

        let _ = sender.send(decision);
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
                Pending::Approval { sender, .. } => {
                    let _ = sender.send(ApprovalDecision::Deny);
                }
                Pending::Question(tx) => {
                    let _ = tx.send(UserAnswer::Cancelled);
                }
            }
        }
    }

    fn needs_approval(
        &self,
        tool_name: &str,
        fingerprint: Option<&str>,
    ) -> PermissionDecision {
        let (request_id, waiter) = self.open_approval(Some(tool_name), fingerprint);
        PermissionDecision::NeedsApproval { request_id, waiter }
    }
}

impl Default for HitlGate {
    fn default() -> Self {
        Self::new(PermissionPolicy::default())
    }
}

/// 按空白 token 边界判定 fingerprint 是否命中 prefix：完全相等，或 prefix 后紧跟空白。
fn fingerprint_matches(fingerprint: &str, prefix: &str) -> bool {
    if fingerprint == prefix {
        return true;
    }
    if let Some(rest) = fingerprint.strip_prefix(prefix) {
        rest.starts_with(' ') || rest.starts_with('\t')
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::RiskLevel;

    fn destructive() -> ToolClass {
        ToolClass::Destructive {
            risk: RiskLevel::High,
        }
    }

    #[test]
    fn allow_and_remember_writes_tool_level_for_normal_tool() {
        let gate = HitlGate::default();
        let (id, _waiter) = gate.open_approval(Some("Write"), None);
        gate.resolve(
            &id,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: None,
            },
        );
        match gate.check("Write", &destructive(), None) {
            PermissionDecision::Approved => {}
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[test]
    fn allow_and_remember_without_pattern_does_not_remember_bash() {
        let gate = HitlGate::default();
        let (id, _waiter) = gate.open_approval(Some("Bash"), Some("git status"));
        gate.resolve(
            &id,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: None, // 故意不给 pattern
            },
        );
        // 黑名单工具 + 无 pattern → 不写记忆，下次仍审批
        match gate.check("Bash", &destructive(), Some("git status")) {
            PermissionDecision::NeedsApproval { .. } => {}
            other => panic!("expected NeedsApproval, got {other:?}"),
        }
    }

    #[test]
    fn allow_and_remember_with_pattern_matches_prefix() {
        let gate = HitlGate::default();
        let (id, _waiter) = gate.open_approval(Some("Bash"), Some("git status"));
        gate.resolve(
            &id,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: Some("git status".into()),
            },
        );
        // 完全相同
        assert!(matches!(
            gate.check("Bash", &destructive(), Some("git status")),
            PermissionDecision::Approved
        ));
        // 前缀 + 空格 + 后续 args
        assert!(matches!(
            gate.check("Bash", &destructive(), Some("git status -uno README.md")),
            PermissionDecision::Approved
        ));
    }

    #[test]
    fn allow_and_remember_pattern_does_not_match_unrelated_command() {
        let gate = HitlGate::default();
        let (id, _waiter) = gate.open_approval(Some("Bash"), Some("git status"));
        gate.resolve(
            &id,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: Some("git status".into()),
            },
        );
        // 不该匹配 "git statusbad"（token 边界）
        assert!(matches!(
            gate.check("Bash", &destructive(), Some("git statusbad")),
            PermissionDecision::NeedsApproval { .. }
        ));
        // 不该匹配 "git push"
        assert!(matches!(
            gate.check("Bash", &destructive(), Some("git push")),
            PermissionDecision::NeedsApproval { .. }
        ));
    }

    #[test]
    fn allow_root_pattern_matches_any_subcommand() {
        let gate = HitlGate::default();
        let (id, _waiter) = gate.open_approval(Some("Bash"), Some("git push"));
        gate.resolve(
            &id,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: Some("git".into()),
            },
        );
        // root pattern 命中所有 git 子命令
        for cmd in ["git status", "git push origin", "git log --oneline"] {
            assert!(
                matches!(
                    gate.check("Bash", &destructive(), Some(cmd)),
                    PermissionDecision::Approved
                ),
                "expected Approved for {cmd}"
            );
        }
    }

    #[test]
    fn readonly_class_is_always_approved() {
        let gate = HitlGate::default();
        match gate.check("Bash", &ToolClass::ReadOnly, Some("ls -la")) {
            PermissionDecision::Approved => {}
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[test]
    fn fingerprint_matches_token_boundary() {
        assert!(fingerprint_matches("git status", "git status"));
        assert!(fingerprint_matches("git status -uno", "git status"));
        assert!(fingerprint_matches("git status\t-uno", "git status"));
        assert!(!fingerprint_matches("git statusbad", "git status"));
        assert!(!fingerprint_matches("gits", "git"));
    }
}
