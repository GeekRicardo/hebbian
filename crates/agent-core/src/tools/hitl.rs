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
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use protocol::{ApprovalDecision, PermissionRequestId, PermissionScope, UserAnswer};
use tokio::sync::oneshot;
use tracing::info;

use crate::definition::{DefaultPermission, PermissionPolicy};
use crate::effects::{EffectClass, Effects};
use crate::permissions::{PermissionStore, RuleEffect};

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
        effects: Option<Effects>,
        /// 危险复合模式（架构 §4.4.2.2）触发的审批：resolve 时即使收到
        /// AllowAndRemember 也**不**落 learned 表 / PermissionStore——cd-git-compound
        /// 这类不可信复合不该一键放行同类后续命令。
        refuse_remember: bool,
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
///
/// `permission_store` + `session_id` + `workdir` 可选：当 surface 启动 PermissionStore
/// 时挂上来，resolve 时把 AllowAndRemember(Session/Project/Global) 翻成
/// [`PermissionRule`] 落盘。`workdir` 仅 `Project` scope 必填（架构 §4.5.4）。
/// 不挂时仅在 in-memory `learned` 表里记忆（保留旧行为）。
pub struct HitlGate {
    policy: PermissionPolicy,
    pending: Mutex<HashMap<PermissionRequestId, Pending>>,
    learned: Mutex<LearnedRules>,
    permission_store: Option<Arc<PermissionStore>>,
    session_id: Option<String>,
    workdir: Option<PathBuf>,
}

impl HitlGate {
    pub fn new(policy: PermissionPolicy) -> Self {
        Self {
            policy,
            pending: Mutex::new(HashMap::new()),
            learned: Mutex::new(LearnedRules::default()),
            permission_store: None,
            session_id: None,
            workdir: None,
        }
    }

    /// 挂上 PermissionStore + 当前 session_id + workdir，让 AllowAndRemember 真正落盘。
    /// `workdir` 用于 `PermissionScope::Project` 规则（架构 §4.5.4）。
    pub fn with_store(
        mut self,
        store: Arc<PermissionStore>,
        session_id: impl Into<String>,
        workdir: Option<PathBuf>,
    ) -> Self {
        self.permission_store = Some(store);
        self.session_id = Some(session_id.into());
        self.workdir = workdir;
        self
    }

    /// 共享的 PermissionStore（用于 dispatcher 级别的路径越界检查也纳入全局规则）。
    pub fn permission_store(&self) -> Option<&Arc<PermissionStore>> {
        self.permission_store.as_ref()
    }

    /// 评估一次工具调用：依 effects 默认行为 + 用户累计规则 + 策略规则三层判断。
    ///
    /// `effects` 由 [`crate::effects::analyze_effects`] 解析得到（架构 §4.4.2）。
    /// 其中 `command_fingerprint` 用于命令级记忆——支持的工具（Bash/PowerShell）
    /// 优先按前缀匹配；其它工具退回工具名级判断。
    ///
    /// 返回 `NeedsApproval` 时，调用方应：
    /// 1. emit `PermissionRequested` 事件（用 request_id）
    /// 2. await waiter
    /// 3. 收到 `ApprovalDecision` 后决定是否执行
    pub fn check(&self, tool_name: &str, effects: &Effects) -> PermissionDecision {
        let fingerprint = effects.command_fingerprint.as_deref();

        if let Some(decision) = self.check_without_policy(tool_name, effects) {
            return decision;
        }

        // 6) 静态策略按名命中
        if self.policy.auto_approve.iter().any(|n| n == tool_name) {
            info!(
                tool = %tool_name,
                "permission: policy.auto_approve → Approved"
            );
            return PermissionDecision::Approved;
        }
        if self.policy.always_ask.iter().any(|n| n == tool_name) {
            info!(
                tool = %tool_name,
                "permission: policy.always_ask → NeedsApproval"
            );
            return self.needs_approval(tool_name, fingerprint, effects);
        }

        // 7) 按 effects 默认行为
        let decision = match effects.class {
            EffectClass::Network | EffectClass::Mutating | EffectClass::Destructive => {
                match self.policy.default_action {
                    DefaultPermission::Auto => {
                        info!(
                            tool = %tool_name,
                            class = ?effects.class,
                            "permission: default_action=Auto → Approved"
                        );
                        PermissionDecision::Approved
                    }
                    DefaultPermission::Ask => {
                        info!(
                            tool = %tool_name,
                            class = ?effects.class,
                            "permission: default_action=Ask → NeedsApproval"
                        );
                        self.needs_approval(tool_name, fingerprint, effects)
                    }
                    DefaultPermission::Deny => {
                        info!(
                            tool = %tool_name,
                            class = ?effects.class,
                            "permission: default_action=Deny → Denied"
                        );
                        PermissionDecision::Denied {
                            reason: "策略默认拒绝".into(),
                        }
                    }
                }
            }
            EffectClass::ReadOnly | EffectClass::NeedsHumanInput => {
                unreachable!("已在前面分支短路")
            }
        };
        decision
    }

    fn check_without_policy(
        &self,
        tool_name: &str,
        effects: &Effects,
    ) -> Option<PermissionDecision> {
        let fingerprint = effects.command_fingerprint.as_deref();
        let has_segments = !effects.segments.is_empty();

        let fp_display = fingerprint.unwrap_or("");
        let class_label = match effects.class {
            EffectClass::ReadOnly => "ReadOnly",
            EffectClass::Mutating => "Mutating",
            EffectClass::Destructive => "Destructive",
            EffectClass::Network => "Network",
            EffectClass::NeedsHumanInput => "NeedsHumanInput",
        };

        // 1) NeedsHumanInput 不走审批（dispatcher 走 ask 路径）
        if matches!(effects.class, EffectClass::NeedsHumanInput) {
            info!(
                tool = %tool_name,
                "permission.check: NeedsHumanInput → Approved (ask path)"
            );
            return Some(PermissionDecision::Approved);
        }

        // 2) 用户显式"永久拒绝"优先于一切
        {
            let learned = self.learned.lock().unwrap();
            if learned.auto_denied_tools.iter().any(|n| n == tool_name) {
                info!(
                    tool = %tool_name,
                    "permission.check: auto_denied_tools → Denied (用户已永久拒绝)"
                );
                return Some(PermissionDecision::Denied {
                    reason: "用户已永久拒绝该工具".into(),
                });
            }
        }

        // 3) 危险复合模式（架构 §4.4.2.2）：强制走人工审批，覆盖一切 allow 规则。
        if effects.has_dangerous_pattern() {
            info!(
                tool = %tool_name,
                kinds = ?effects.dangerous_kinds,
                "permission.check: dangerous pattern → NeedsApproval (no remember)"
            );
            return Some(self.needs_approval_no_remember(tool_name, fingerprint, effects));
        }

        // 4) ReadOnly 永远放行：effects 自报无副作用，always_ask 不该再拦。
        if matches!(effects.class, EffectClass::ReadOnly) {
            info!(
                tool = %tool_name,
                fingerprint = fp_display,
                "permission.check: ReadOnly → Approved (auto)"
            );
            return Some(PermissionDecision::Approved);
        }

        // 5) 用户累计的"允许并记住"——Bash/PowerShell 走段级匹配，其它工具退回工具名级。
        {
            let learned = self.learned.lock().unwrap();
            let patterns_count = learned.auto_approved_patterns.len();
            let tools_count = learned.auto_approved_tools.len();
            if has_segments {
                let mut all_seg_allowed = true;
                for seg in &effects.segments {
                    let seg_matched = learned.auto_approved_patterns.iter().any(|(t, prefix)| {
                        t == tool_name && fingerprint_matches(&seg.fingerprint, prefix)
                    });
                    if seg_matched {
                        info!(
                            tool = %tool_name,
                            segment = %seg.fingerprint,
                            "permission.check: segment matched learned pattern → allowed"
                        );
                    } else {
                        info!(
                            tool = %tool_name,
                            segment = %seg.fingerprint,
                            patterns_count = patterns_count,
                            "permission.check: segment NOT matched in learned patterns"
                        );
                        all_seg_allowed = false;
                    }
                }
                if all_seg_allowed {
                    info!(
                        tool = %tool_name,
                        n_segments = effects.segments.len(),
                        "permission.check: all segments matched learned patterns → Approved"
                    );
                    return Some(PermissionDecision::Approved);
                }
            } else if let Some(fp) = fingerprint {
                let pat_matched = learned
                    .auto_approved_patterns
                    .iter()
                    .any(|(t, prefix)| t == tool_name && fingerprint_matches(fp, prefix));
                if pat_matched {
                    info!(
                        tool = %tool_name,
                        fingerprint = %fp,
                        "permission.check: fingerprint matched learned pattern → Approved"
                    );
                    return Some(PermissionDecision::Approved);
                } else {
                    info!(
                        tool = %tool_name,
                        fingerprint = %fp,
                        patterns_count = patterns_count,
                        "permission.check: fingerprint NOT matched in learned patterns"
                    );
                }
            }
            let tool_matched = learned.auto_approved_tools.iter().any(|n| n == tool_name);
            if tool_matched {
                info!(
                    tool = %tool_name,
                    "permission.check: tool-level learned rule → Approved"
                );
                return Some(PermissionDecision::Approved);
            }
            if !has_segments && fingerprint.is_none() {
                info!(
                    tool = %tool_name,
                    patterns_count = patterns_count,
                    tools_count = tools_count,
                    class = class_label,
                    "permission.check: no learned rules matched (no fingerprint to match)"
                );
            }
        }

        // 5b) PermissionStore（Session → Project → Global）—— 长期规则
        if let Some(store) = &self.permission_store {
            let sid = self.session_id.as_deref();
            let wd = self.workdir.as_deref();
            let store_decision = if has_segments {
                let seg_pairs: Vec<(String, Vec<String>)> = effects
                    .segments
                    .iter()
                    .map(|s| (s.fingerprint.clone(), s.write_targets.clone()))
                    .collect();
                let r = store.find_for_segments(sid, wd, tool_name, &seg_pairs);
                info!(
                    tool = %tool_name,
                    result = ?r.map(|r| format!("{:?}", r)),
                    segments = ?seg_pairs.iter().map(|(f,_)| f).collect::<Vec<_>>(),
                    "permission.check: PermissionStore.find_for_segments"
                );
                r
            } else if !effects.paths.is_empty() {
                let path_strs: Vec<String> = effects
                    .paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                let r = store.find_for_paths(sid, wd, tool_name, fingerprint, &path_strs);
                info!(
                    tool = %tool_name,
                    result = ?r.map(|r| format!("{:?}", r)),
                    paths = path_strs.join(", "),
                    "permission.check: PermissionStore.find_for_paths"
                );
                r
            } else {
                let r = store.find(sid, wd, tool_name, fingerprint, None);
                info!(
                    tool = %tool_name,
                    result = ?r.map(|r| format!("{:?}", r)),
                    "permission.check: PermissionStore.find"
                );
                r
            };
            if let Some(dec) = store_decision {
                let label = match dec {
                    RuleEffect::Allow => "Allow",
                    RuleEffect::Deny => "Deny",
                };
                info!(
                    tool = %tool_name,
                    effect = label,
                    "permission.check: PermissionStore hit → {}",
                    label
                );
                return Some(match dec {
                    RuleEffect::Allow => PermissionDecision::Approved,
                    RuleEffect::Deny => PermissionDecision::Denied {
                        reason: "PermissionStore 规则拒绝".into(),
                    },
                });
            } else {
                info!(
                    tool = %tool_name,
                    class = class_label,
                    "permission.check: PermissionStore miss (no matching rule)"
                );
            }
        } else {
            info!(
                tool = %tool_name,
                class = class_label,
                "permission.check: no PermissionStore configured"
            );
        }

        None
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
        self.open_approval_inner(tool_name, fingerprint, None, false)
    }

    /// 同 [`open_approval`](Self::open_approval)，但 resolve 时即使收到 `AllowAndRemember`
    /// 也**不**写 learned 表 / PermissionStore（架构 §4.4.2.2 危险复合模式）。
    pub fn open_approval_no_remember(
        &self,
        tool_name: Option<&str>,
        fingerprint: Option<&str>,
    ) -> (PermissionRequestId, oneshot::Receiver<ApprovalDecision>) {
        self.open_approval_inner(tool_name, fingerprint, None, true)
    }

    fn open_tool_approval(
        &self,
        tool_name: &str,
        fingerprint: Option<&str>,
        effects: &Effects,
        refuse_remember: bool,
    ) -> PermissionDecision {
        let (request_id, waiter) = self.open_approval_inner(
            Some(tool_name),
            fingerprint,
            Some(effects.clone()),
            refuse_remember,
        );
        PermissionDecision::NeedsApproval { request_id, waiter }
    }

    fn open_approval_inner(
        &self,
        tool_name: Option<&str>,
        fingerprint: Option<&str>,
        effects: Option<Effects>,
        refuse_remember: bool,
    ) -> (PermissionRequestId, oneshot::Receiver<ApprovalDecision>) {
        let request_id = PermissionRequestId::new();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            request_id.clone(),
            Pending::Approval {
                sender: tx,
                tool_name: tool_name.map(str::to_owned),
                fingerprint: fingerprint.map(str::to_owned),
                effects,
                refuse_remember,
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

    /// request_id 是否仍在本 gate 的 pending 表里。
    pub fn is_pending(&self, request_id: &PermissionRequestId) -> bool {
        self.pending.lock().unwrap().contains_key(request_id)
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
            effects: _,
            refuse_remember,
        }) = entry
        else {
            return;
        };

        if let ApprovalDecision::AllowAndRemember {
            scope,
            pattern,
            extra_patterns,
        } = &decision
        {
            if refuse_remember {
                tracing::debug!(
                    tool = tool_name.as_deref().unwrap_or(""),
                    fingerprint = fingerprint.as_deref().unwrap_or(""),
                    "AllowAndRemember 被拒绝落盘：审批是由危险复合模式触发的（架构 §4.4.2.2）"
                );
            } else if let Some(name) = &tool_name {
                self.remember(*scope, name, pattern.as_deref(), fingerprint.as_deref());
                // compound 命令场景：额外段前缀逐一落盘，让 `cd /tmp && touch x` 类
                // 命令一次审批就能让段级判定（架构 §4.4.2）的"全段 allow"条件满足。
                for extra in extra_patterns {
                    self.remember(*scope, name, Some(extra.as_str()), fingerprint.as_deref());
                }
                self.resolve_matching_pending_after_remember(request_id);
            }
        }

        let _ = sender.send(decision);
    }

    fn resolve_matching_pending_after_remember(&self, current_request_id: &PermissionRequestId) {
        let mut auto_resolved: Vec<oneshot::Sender<ApprovalDecision>> = Vec::new();
        {
            let mut pending = self.pending.lock().unwrap();
            let matching_ids: Vec<PermissionRequestId> = pending
                .iter()
                .filter_map(|(id, entry)| {
                    if id == current_request_id {
                        return None;
                    }
                    let Pending::Approval {
                        tool_name: Some(tool_name),
                        effects: Some(effects),
                        refuse_remember: false,
                        ..
                    } = entry
                    else {
                        return None;
                    };
                    if matches!(
                        self.check_without_policy(tool_name, effects),
                        Some(PermissionDecision::Approved)
                    ) {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            for id in matching_ids {
                if let Some(Pending::Approval { sender, .. }) = pending.remove(&id) {
                    auto_resolved.push(sender);
                }
            }
        }

        for sender in auto_resolved {
            let _ = sender.send(ApprovalDecision::AllowOnce);
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
        effects: &Effects,
    ) -> PermissionDecision {
        self.open_tool_approval(tool_name, fingerprint, effects, false)
    }

    /// 同 [`needs_approval`](Self::needs_approval)，但 pending 带 `refuse_remember=true`。
    /// 用于危险复合模式触发的强制审批（架构 §4.4.2.2）。
    fn needs_approval_no_remember(
        &self,
        tool_name: &str,
        fingerprint: Option<&str>,
        effects: &Effects,
    ) -> PermissionDecision {
        self.open_tool_approval(tool_name, fingerprint, effects, true)
    }

    /// 把 AllowAndRemember 翻成对应 scope 的记忆。
    ///
    /// 行为分级：
    /// - `Once`：不写任何地方
    /// - `Session`：
    ///   - 优先写 in-memory learned 表（兼容旧 dispatch 命中路径）
    ///   - 若挂了 PermissionStore，再写一条 [`PermissionRule`] 进 session 内存视图
    /// - `Project`：写一条 PermissionRule 进 ~/.hebbian/permissions.json，
    ///   带 `workdir = self.workdir`。未挂 PermissionStore 或 workdir 缺失 → warn 退回 AllowOnce
    /// - `Global`：写一条 PermissionRule 进 ~/.hebbian/permissions.json
    ///   未挂时打 warn，"按钮"等同 AllowOnce
    fn remember(
        &self,
        scope: PermissionScope,
        tool_name: &str,
        pattern: Option<&str>,
        fingerprint: Option<&str>,
    ) {
        let _ = fingerprint; // 仅 debug 备用
        let _no_tool_level = NO_TOOL_LEVEL_MEMORY.contains(&tool_name);
        match scope {
            PermissionScope::Once => {}
            PermissionScope::Session => {
                // (1) in-memory learned（旧路径，立即对本 run 后续工具调用生效）
                let mut learned = self.learned.lock().unwrap();
                match (pattern, _no_tool_level) {
                    (Some(prefix), _) => {
                        let prefix = prefix.trim();
                        if !prefix.is_empty() {
                            let entry = (tool_name.to_string(), prefix.to_string());
                            if !learned.auto_approved_patterns.contains(&entry) {
                                learned.auto_approved_patterns.push(entry);
                            }
                        }
                    }
                    (None, false) => {
                        let name = tool_name.to_string();
                        if !learned.auto_approved_tools.contains(&name) {
                            learned.auto_approved_tools.push(name);
                        }
                    }
                    (None, true) => {
                        tracing::debug!(
                            tool = tool_name,
                            "AllowAndRemember(Session) 没有 pattern，工具在 NO_TOOL_LEVEL_MEMORY 黑名单，本次不记忆"
                        );
                    }
                }
                drop(learned);
                // (2) PermissionStore：让 session.jsonl 也能回放出同样的规则
                if let (Some(store), Some(sid)) = (&self.permission_store, &self.session_id) {
                    if let Some(pat) = build_pattern(tool_name, pattern) {
                        if let Err(e) = store.add(
                            PermissionScope::Session,
                            Some(sid.as_str()),
                            None,
                            RuleEffect::Allow,
                            pat,
                        ) {
                            tracing::warn!(error = %e, "PermissionStore.add(Session) 失败");
                        }
                    }
                }
            }
            PermissionScope::Project => {
                let Some(store) = self.permission_store.as_ref() else {
                    tracing::warn!(
                        tool = tool_name,
                        "AllowAndRemember(Project) 但未挂 PermissionStore，等同 AllowOnce"
                    );
                    return;
                };
                let Some(wd) = self.workdir.clone() else {
                    tracing::warn!(
                        tool = tool_name,
                        "AllowAndRemember(Project) 但 HitlGate 未持 workdir，等同 AllowOnce"
                    );
                    return;
                };
                if let Some(pat) = build_pattern(tool_name, pattern) {
                    if let Err(e) = store.add(
                        PermissionScope::Project,
                        None,
                        Some(wd.as_path()),
                        RuleEffect::Allow,
                        pat,
                    ) {
                        tracing::warn!(error = %e, "PermissionStore.add(Project) 失败");
                    }
                }
            }
            PermissionScope::Global => {
                let Some(store) = self.permission_store.as_ref() else {
                    tracing::warn!(
                        tool = tool_name,
                        "AllowAndRemember(Global) 但未挂 PermissionStore，等同 AllowOnce"
                    );
                    return;
                };
                if let Some(pat) = build_pattern(tool_name, pattern) {
                    if let Err(e) =
                        store.add(PermissionScope::Global, None, None, RuleEffect::Allow, pat)
                    {
                        tracing::warn!(error = %e, "PermissionStore.add(Global) 失败");
                    }
                }
            }
        }
    }
}

/// 把 (tool_name, command/path pattern) 拼成 PermissionStore 字符串 pattern 语法。
/// 例：("Bash", Some("git status")) → "Bash(git status)"；("Read", None) → "Read"。
fn build_pattern(tool_name: &str, pattern: Option<&str>) -> Option<String> {
    match pattern {
        None => Some(tool_name.to_string()),
        Some(p) => {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            Some(format!("{tool_name}({p})"))
        }
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

    fn destructive_effects(fingerprint: Option<&str>) -> Effects {
        Effects {
            paths: Vec::new(),
            command_fingerprint: fingerprint.map(str::to_owned),
            network: false,
            domain: None,
            risk: RiskLevel::High,
            class: EffectClass::Destructive,
            is_concurrent_safe: false,
            segments: Vec::new(),
            dangerous_kinds: Vec::new(),
        }
    }

    fn readonly_effects(fingerprint: Option<&str>) -> Effects {
        Effects {
            paths: Vec::new(),
            command_fingerprint: fingerprint.map(str::to_owned),
            network: false,
            domain: None,
            risk: RiskLevel::Low,
            class: EffectClass::ReadOnly,
            is_concurrent_safe: true,
            segments: Vec::new(),
            dangerous_kinds: Vec::new(),
        }
    }

    fn dangerous_effects(fingerprint: Option<&str>, kinds: Vec<&str>) -> Effects {
        Effects {
            paths: Vec::new(),
            command_fingerprint: fingerprint.map(str::to_owned),
            network: false,
            domain: None,
            risk: RiskLevel::High,
            class: EffectClass::Destructive,
            is_concurrent_safe: false,
            segments: Vec::new(),
            dangerous_kinds: kinds.into_iter().map(str::to_owned).collect(),
        }
    }

    #[test]
    fn allow_and_remember_writes_tool_level_for_normal_tool() {
        let gate = HitlGate::default();
        let (id, _waiter) = gate.open_approval(Some("Edit"), None);
        gate.resolve(
            &id,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: None,
                extra_patterns: Vec::new(),
            },
        );
        match gate.check("Edit", &destructive_effects(None)) {
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
                extra_patterns: Vec::new(),
            },
        );
        // 黑名单工具 + 无 pattern → 不写记忆，下次仍审批
        match gate.check("Bash", &destructive_effects(Some("git status"))) {
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
                extra_patterns: Vec::new(),
            },
        );
        // 完全相同
        assert!(matches!(
            gate.check("Bash", &destructive_effects(Some("git status"))),
            PermissionDecision::Approved
        ));
        // 前缀 + 空格 + 后续 args
        assert!(matches!(
            gate.check(
                "Bash",
                &destructive_effects(Some("git status -uno README.md"))
            ),
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
                extra_patterns: Vec::new(),
            },
        );
        // 不该匹配 "git statusbad"（token 边界）
        assert!(matches!(
            gate.check("Bash", &destructive_effects(Some("git statusbad"))),
            PermissionDecision::NeedsApproval { .. }
        ));
        // 不该匹配 "git push"
        assert!(matches!(
            gate.check("Bash", &destructive_effects(Some("git push"))),
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
                extra_patterns: Vec::new(),
            },
        );
        // root pattern 命中所有 git 子命令
        for cmd in ["git status", "git push origin", "git log --oneline"] {
            assert!(
                matches!(
                    gate.check("Bash", &destructive_effects(Some(cmd))),
                    PermissionDecision::Approved
                ),
                "expected Approved for {cmd}"
            );
        }
    }

    #[test]
    fn readonly_class_is_always_approved() {
        let gate = HitlGate::default();
        match gate.check("Bash", &readonly_effects(Some("ls -la"))) {
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

    /// 回归：Edit/Write 类工具，用户审批后选「整个目录 + 本项目」记忆，
    /// 同一目录下不同子文件不应再次触发审批。
    /// 修前 HitlGate::check 把 path 传 None 给 store.find，`Edit(/dir/)` 规则永远不命中。
    #[test]
    fn project_directory_rule_matches_subfile_without_reapproval() {
        let dir =
            std::env::temp_dir().join(format!("hebbian-hitl-subdir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(PermissionStore::open(&dir).unwrap());
        let workdir = PathBuf::from("/Users/x/proj");
        let gate = HitlGate::new(PermissionPolicy::default()).with_store(
            store,
            "sess-1",
            Some(workdir.clone()),
        );

        // 第一次：Edit /foo/bar/a.rs → NeedsApproval；用户选 AllowAndRemember(Project, "/foo/bar/")
        let edit_a = Effects {
            paths: vec![PathBuf::from("/foo/bar/a.rs")],
            command_fingerprint: None,
            network: false,
            domain: None,
            risk: RiskLevel::Medium,
            class: EffectClass::Mutating,
            is_concurrent_safe: false,
            segments: Vec::new(),
            dangerous_kinds: Vec::new(),
        };
        let decision = gate.check("Edit", &edit_a);
        let req_id = match decision {
            PermissionDecision::NeedsApproval { request_id, .. } => request_id,
            other => panic!("expected NeedsApproval, got {other:?}"),
        };
        gate.resolve(
            &req_id,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Project,
                pattern: Some("/foo/bar/".into()),
                extra_patterns: Vec::new(),
            },
        );

        // 第二次：Edit /foo/bar/sub/b.rs（同目录不同子文件）→ 应当直接 Approved
        let edit_b = Effects {
            paths: vec![PathBuf::from("/foo/bar/sub/b.rs")],
            ..edit_a.clone()
        };
        match gate.check("Edit", &edit_b) {
            PermissionDecision::Approved => {}
            other => panic!("expected Approved for subdirectory file, got {other:?}"),
        }

        // 第三次：Edit /elsewhere/c.rs（不在前缀下）→ 仍审批
        let edit_c = Effects {
            paths: vec![PathBuf::from("/elsewhere/c.rs")],
            ..edit_a
        };
        match gate.check("Edit", &edit_c) {
            PermissionDecision::NeedsApproval { .. } => {}
            other => panic!("expected NeedsApproval for path outside rule, got {other:?}"),
        }
    }

    #[test]
    fn remember_resolves_already_pending_matching_bash_approval_for_all_scopes() {
        for scope in [
            PermissionScope::Session,
            PermissionScope::Project,
            PermissionScope::Global,
        ] {
            let dir =
                std::env::temp_dir().join(format!("hebbian-hitl-pending-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let store = Arc::new(PermissionStore::open(&dir).unwrap());
            let gate = HitlGate::new(PermissionPolicy::default()).with_store(
                store,
                "sess-1",
                Some(PathBuf::from("/Users/x/proj")),
            );

            let first = gate.check("Bash", &destructive_effects(Some("git status")));
            let second = gate.check("Bash", &destructive_effects(Some("git status -uno")));
            let first_id = match first {
                PermissionDecision::NeedsApproval { request_id, .. } => request_id,
                other => panic!("expected first NeedsApproval for {scope:?}, got {other:?}"),
            };
            let mut second_waiter = match second {
                PermissionDecision::NeedsApproval { waiter, .. } => waiter,
                other => panic!("expected second NeedsApproval for {scope:?}, got {other:?}"),
            };

            gate.resolve(
                &first_id,
                ApprovalDecision::AllowAndRemember {
                    scope,
                    pattern: Some("git status".into()),
                    extra_patterns: Vec::new(),
                },
            );

            assert!(matches!(
                second_waiter.try_recv(),
                Ok(ApprovalDecision::AllowOnce)
            ));
        }
    }

    #[test]
    fn remember_resolves_already_pending_matching_edit_approval_for_all_scopes() {
        for scope in [
            PermissionScope::Session,
            PermissionScope::Project,
            PermissionScope::Global,
        ] {
            let dir = std::env::temp_dir().join(format!(
                "hebbian-hitl-edit-pending-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let store = Arc::new(PermissionStore::open(&dir).unwrap());
            let gate = HitlGate::new(PermissionPolicy::default()).with_store(
                store,
                "sess-1",
                Some(PathBuf::from("/Users/x/proj")),
            );
            let edit_a = Effects {
                paths: vec![PathBuf::from("/foo/bar/a.rs")],
                command_fingerprint: None,
                network: false,
                domain: None,
                risk: RiskLevel::Medium,
                class: EffectClass::Mutating,
                is_concurrent_safe: false,
                segments: Vec::new(),
                dangerous_kinds: Vec::new(),
            };
            let edit_b = Effects {
                paths: vec![PathBuf::from("/foo/bar/b.rs")],
                ..edit_a.clone()
            };

            let first = gate.check("Edit", &edit_a);
            let second = gate.check("Edit", &edit_b);
            let first_id = match first {
                PermissionDecision::NeedsApproval { request_id, .. } => request_id,
                other => panic!("expected first NeedsApproval for {scope:?}, got {other:?}"),
            };
            let mut second_waiter = match second {
                PermissionDecision::NeedsApproval { waiter, .. } => waiter,
                other => panic!("expected second NeedsApproval for {scope:?}, got {other:?}"),
            };

            gate.resolve(
                &first_id,
                ApprovalDecision::AllowAndRemember {
                    scope,
                    pattern: Some("/foo/bar/".into()),
                    extra_patterns: Vec::new(),
                },
            );

            assert!(matches!(
                second_waiter.try_recv(),
                Ok(ApprovalDecision::AllowOnce)
            ));
        }
    }

    /// 危险复合模式：即使有 allow 规则也强制审批，且 resolve(AllowAndRemember)
    /// 不被记忆——下次再来同样命令仍然要审批。
    #[test]
    fn dangerous_pattern_forces_approval_and_refuses_remember() {
        let gate = HitlGate::default();
        // 先给一条 `Bash(git)` allow & remember——会让普通 `git push` 自动放行
        let (id_seed, _waiter_seed) = gate.open_approval(Some("Bash"), Some("git push"));
        gate.resolve(
            &id_seed,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: Some("git".into()),
                extra_patterns: Vec::new(),
            },
        );
        // 普通 git push fingerprint 段命中 allow（无危险模式）→ Approved
        assert!(matches!(
            gate.check("Bash", &destructive_effects(Some("git push"))),
            PermissionDecision::Approved
        ));
        // 现在带 cd-git-compound 危险模式：即使 fingerprint 命中 git allow，也必须审批
        let decision = gate.check(
            "Bash",
            &dangerous_effects(Some("git status"), vec!["cd-git-compound"]),
        );
        let req_id = match decision {
            PermissionDecision::NeedsApproval { request_id, .. } => request_id,
            other => panic!("expected NeedsApproval, got {other:?}"),
        };
        // 用户即便选 AllowAndRemember(pattern=git status)，也不该写入 learned
        gate.resolve(
            &req_id,
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: Some("git status".into()),
                extra_patterns: Vec::new(),
            },
        );
        // 验证：再次出现独立的 `git status` 危险模式仍然审批
        assert!(matches!(
            gate.check(
                "Bash",
                &dangerous_effects(Some("git status"), vec!["cd-git-compound"])
            ),
            PermissionDecision::NeedsApproval { .. }
        ));
    }
}
