//! 工具派发器：把一组 tool call 跑出 `Vec<ToolResult>`。
//!
//! 接管以下职责（让 [`agent_loop`] 不必再 inline 200 行 closure）：
//! - 路径越界审批 → emit `PermissionRequested { kind: PathAccess }` + await
//! - 工具审批 → emit `PermissionRequested { kind: ToolCall }` + await
//! - 提问通路（`ask` 工具）→ emit `UserQuestionRequested` + await
//! - 工具执行（含超时输出截断）+ emit `ToolCallStarted` / `ToolCallFinished`
//!
//! 并发策略：`ReadOnly` 工具与同 turn 其他 ReadOnly 工具并发；需要 HITL
//! 的工具因 `await` 自然串行（无需特殊调度）。
//!
//! [`agent_loop`]: super::agent_loop

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::{join_all, BoxFuture};
use observability::{attr, metrics};
use protocol::{
    ApprovalDecision, EventPayload, PermissionKind, PermissionRequestId, QuestionOption, RiskLevel,
    UserAnswer,
};
use serde::Deserialize;
use tokio::sync::oneshot;
use tracing::{field::Empty, info, warn, Instrument};

use crate::{
    agent_loop::EventSink,
    edits::{metadata::EditEntry, EditsWorktree},
    effects::{analyze_effects, EffectClass},
    permissions::PermissionStore,
    run_state::RunState,
    tools::{
        hitl::{HitlGate, PermissionDecision},
        registry::ToolRegistry,
        ASK_TOOL_NAME,
    },
    workspace::Workspace,
};

fn effect_class_label(class: EffectClass) -> &'static str {
    match class {
        EffectClass::ReadOnly => "read_only",
        EffectClass::Network => "network",
        EffectClass::Mutating => "mutating",
        EffectClass::Destructive => "destructive",
        EffectClass::NeedsHumanInput => "needs_human_input",
    }
}

fn approval_decision_label(d: &ApprovalDecision) -> &'static str {
    match d {
        ApprovalDecision::AllowOnce => "allow_once",
        ApprovalDecision::AllowAndRemember { .. } => "allow_and_remember",
        ApprovalDecision::Deny => "deny",
        ApprovalDecision::DenyWithFeedback { .. } => "deny_with_feedback",
    }
}

use common::{runtime as cancellation, CancelFlag};
use model_gateway::types::{ModelError, ToolArtifact, ToolCall, ToolResult};

const MAX_TOOL_RESULT_INLINE: usize = 6_000;
/// 落 artifact 路径时给模型看的头部预览字节上限。
const ARTIFACT_HEAD_PREVIEW_BYTES: usize = 2_000;

/// `ask` 工具的输入。
#[derive(Debug, Deserialize)]
struct AskInput {
    question: String,
    options: Vec<QuestionOption>,
    /// 是否允许多选；缺省 false（单选）。
    #[serde(default)]
    multi: bool,
}

/// 一次越界路径审批的待解条目。
struct PathApproval {
    request_id: PermissionRequestId,
    paths: Vec<PathBuf>,
    waiter: oneshot::Receiver<ApprovalDecision>,
}

/// 派发一组 tool call 并发返回结果。所有依赖以 `Arc` 持有，clone 成本低。
pub struct ToolDispatcher {
    pub registry: Arc<ToolRegistry>,
    pub hitl: Arc<HitlGate>,
    pub workspace: Arc<Workspace>,
    pub state: Arc<RunState>,
    pub sink: EventSink,
    pub cancel: CancelFlag,
    /// 运行模式（架构 §4.4.3）。AutoMode 时在 NeedsApproval 路径上调一次 judge。
    pub run_mode: crate::run_mode::RunMode,
    /// 当前会话使用的模型 id（AutoMode judge 限定模型用，命中
    /// [`crate::automode::AUTOMODE_ALLOWED_MODELS`] 才发请求）。
    pub model_id: Option<String>,
    /// AutoMode judge 复用的 ModelClient（通常 = 主 client）。`None` 时降级 Ask。
    pub judge_client: Option<std::sync::Arc<dyn model_gateway::client::ModelClient>>,
    /// `force_automode` 子开关（架构 §4.4.4）。仅 [`RunMode::AutoMode`] 下生效：
    /// 判官返回 `Ask` 时直接折叠成 `Deny` 不打断用户；`Allow` / `Deny` 不变。
    /// 由 CLI flag `--force-automode` 或 REPL `/force-automode` 切换。
    pub force_automode: bool,
    /// Hook 管理器（架构 §4.8）。dispatch_one 内在工具调用前后 trigger PreToolUse /
    /// PostToolUse / PostToolUseFailure 让外部 hook 介入。
    pub hooks: std::sync::Arc<crate::hooks::HookManager>,
    /// 当前会话 id，PreToolUse / PostToolUse hook payload 用。
    pub session_id_for_hooks: Option<String>,
    /// hebbian 数据目录根（`~/.hebbian`）。dispatcher 用它把超阈值的工具结果落到
    /// `tool_results/<call_id>.txt`（架构 §4.4.9 / §4.12.11 Phase 2）。`None`
    /// 表示当前进程未挂数据目录（单测路径），跳过 materialize 走截断。
    pub data_dir_for_artifacts: Option<PathBuf>,
    /// 共享的 PermissionStore（用于路径越界检查纳入 Global 规则 + 路径审批后持久化）。
    pub permission_store: Option<Arc<PermissionStore>>,
    /// Edit 工具快照仓库（架构 §4.13）。`None` 时跳过快照，不阻塞 Edit。
    pub edits_worktree: Option<Arc<EditsWorktree>>,
}

impl ToolDispatcher {
    /// 派发整组 tool call。返回按 call_index 排序的 ToolResult。
    /// `dispatch_offset` 是这一轮在整个 run 内的全局起始 index。
    pub async fn run_calls(
        &self,
        calls: &[ToolCall],
        dispatch_offset: usize,
    ) -> Result<Vec<ToolResult>, ModelError> {
        let mut tasks: Vec<BoxFuture<'static, Result<(usize, ToolResult), ModelError>>> =
            Vec::with_capacity(calls.len());

        for (call_index, call) in calls.iter().enumerate() {
            if cancellation::is_cancelled(&self.cancel) {
                self.hitl.cancel_all_pending();
                break;
            }
            let dispatch_index = dispatch_offset + call_index;
            tasks.push(if call.name == ASK_TOOL_NAME {
                self.spawn_ask(call.clone(), call_index, dispatch_index)
            } else {
                self.spawn_tool(call.clone(), call_index, dispatch_index)
            });
        }

        let mut results = Vec::with_capacity(tasks.len());
        for outcome in join_all(tasks).await {
            results.push(outcome?);
        }
        results.sort_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, r)| r).collect())
    }

    /// 普通工具派发：路径审批 → 工具审批 → 执行。
    fn spawn_tool(
        &self,
        call: ToolCall,
        call_index: usize,
        dispatch_index: usize,
    ) -> BoxFuture<'static, Result<(usize, ToolResult), ModelError>> {
        let tool = self.registry.find(&call.name);
        let tool_found = tool.is_some();

        // effects 分析（架构 §4.4.2）：路径、命令指纹、风险类别都从这里来。
        // 对 Bash/PowerShell 这类 cwd 不一定显式给的工具，按 workspace.workdir 兜底，
        // 让越界检查命中正确的工作目录。
        let mut effects = analyze_effects(&call.name, &call.input);
        if matches!(call.name.as_str(), "Bash" | "PowerShell") && effects.paths.is_empty() {
            effects.paths.push(self.workspace.workdir().to_path_buf());
        }
        let class_label = effect_class_label(effects.class);
        let fingerprint = effects.command_fingerprint.clone();

        // 路径越界检查（同步）。
        // 当前 session 数据目录（`~/.hebbian/sessions/<sid>/`）下的文件是 agent
        // 自己的工具输出（line_trunc/、tool_results/、bg/ 等），永远视为在界内，
        // 不触发 PathAccess 审批。范围限定到 session 目录，避免配置文件被随便读。
        let out_of_scope: Vec<PathBuf> = effects
            .paths
            .iter()
            .filter(|p| !self.workspace.allows(p))
            .filter(|p| {
                !self
                    .data_dir_for_artifacts
                    .as_ref()
                    .zip(self.session_id_for_hooks.as_deref())
                    .map_or(false, |(dd, sid)| p.starts_with(&dd.join("sessions").join(sid)))
            })
            .filter(|p| {
                // 也查 PermissionStore：Session/Project/Global FilePath Allow 规则覆盖的路径免审批
                !self
                    .permission_store
                    .as_ref()
                    .map_or(false, |store| {
                        store.allows_path(
                            self.session_id_for_hooks.as_deref(),
                            Some(self.workspace.workdir()),
                            &p.to_string_lossy(),
                        )
                    })
            })
            .cloned()
            .collect();

        let path_pending = if out_of_scope.is_empty() {
            None
        } else {
            Some(self.request_path_approval(&call.name, out_of_scope))
        };

        // 工具审批
        let permission = self.hitl.check(&call.name, &effects);
        match &permission {
            PermissionDecision::Approved => {
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    class = class_label,
                    fingerprint = fingerprint.as_deref().unwrap_or(""),
                    "tool_call approved (auto)"
                );
            }
            PermissionDecision::Denied { reason } => {
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    class = class_label,
                    %reason,
                    "tool_call denied by policy"
                );
            }
            PermissionDecision::NeedsApproval { request_id, .. } => {
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    class = class_label,
                    request_id = %request_id,
                    fingerprint = fingerprint.as_deref().unwrap_or(""),
                    "tool_call needs human approval"
                );
                self.emit(EventPayload::PermissionRequested {
                    request_id: request_id.clone(),
                    kind: PermissionKind::ToolCall {
                        tool_name: call.name.clone(),
                        input: call.input.clone(),
                        fingerprint: fingerprint.clone(),
                    },
                    summary: format!("工具 {} 请求执行", call.name),
                    risk: RiskLevel::Medium,
                });
            }
        }

        let state = self.state.clone();
        let sink = self.sink.clone();
        let cancel = self.cancel.clone();
        let workspace = self.workspace.clone();
        let run_mode = self.run_mode;
        let judge_client = self.judge_client.clone();
        let model_id_for_judge = self.model_id.clone();
        let force_automode = self.force_automode;
        let effects_for_judge = effects.clone();
        let hitl_for_future = self.hitl.clone();
        let call_name_for_judge = call.name.clone();
        let call_input_for_judge = call.input.clone();
        let hooks_for_future = self.hooks.clone();
        let session_id_for_hooks = self.session_id_for_hooks.clone();
        let data_dir_for_artifacts = self.data_dir_for_artifacts.clone();
        let permission_store = self.permission_store.clone();
        let edits_worktree_for_snapshot = self.edits_worktree.clone();

        let tool_span = tracing::info_span!(
            "tool.call",
            otel.kind = "internal",
            hebbian.tool.name = %call.name,
            hebbian.tool.call_id = %call.id,
            hebbian.tool.class = class_label,
            hebbian.tool.outcome = Empty,
            hebbian.tool.truncated = Empty,
            hebbian.tool.result_bytes = Empty,
            hebbian.tool.duration_ms = Empty,
        );

        Box::pin(
            async move {
                // 路径审批（带 permission.check 子 span）
                if let Some(p) = path_pending {
                    let outcome = await_path_decision(
                        &sink,
                        &state,
                        &workspace,
                        &permission_store,
                        session_id_for_hooks.as_deref(),
                        p,
                    )
                    .await;
                    if let Err(reason) = outcome {
                        record_tool_outcome(attr::outcome::DENIED, &call.name, 0.0, false, 0);
                        return Ok(deny_tool(
                            call,
                            call_index,
                            dispatch_index,
                            &state,
                            &sink,
                            reason,
                        ));
                    }
                }

                // EditAutomatically 短路（架构 §4.4.3）：文件编辑类（Edit/Write）NeedsApproval
                // 直接 AllowOnce 短路；命令类（Bash/PowerShell）仍走原审批路径。
                if run_mode == crate::run_mode::RunMode::EditAutomatically {
                    if let PermissionDecision::NeedsApproval { request_id, .. } = &permission {
                        let is_edit = matches!(
                            call_name_for_judge.as_str(),
                            "Edit" | "edit"
                        );
                        if is_edit {
                            sink(state.event(protocol::EventPayload::PermissionAutoJudged {
                                tool_name: call_name_for_judge.clone(),
                                decision: "allow".to_string(),
                                reason: Some("EditAutomatically: 文件编辑自动放行".to_string()),
                            }));
                            hitl_for_future.resolve(request_id, ApprovalDecision::AllowOnce);
                        }
                    }
                }

                // AutoMode 短路（架构 §4.4.4）：destructive 工具进入 NeedsApproval 时，
                // 调一次 judge_auto_mode 决定 Allow / Deny / Ask。Allow/Deny 主动
                // resolve waiter，让 await_permission_decision 立即按 judge 结果返回；
                // Ask 则保持原流程让用户决策（PermissionRequested 已 emit）。
                if run_mode == crate::run_mode::RunMode::AutoMode {
                    if let PermissionDecision::NeedsApproval { request_id, .. } = &permission {
                        if let Some(judge) = judge_client.as_ref() {
                            let model_id_str = model_id_for_judge.as_deref().unwrap_or("");
                            // judge 必须看到 hebbian 静态分析的全量 effects（segments /
                            // write_targets / dangerous_kinds），不重复解析 shell。
                            let raw_decision = crate::automode::judge_auto_mode(
                                judge,
                                model_id_str,
                                &call_name_for_judge,
                                &call_input_for_judge,
                                &effects_for_judge,
                                &[],
                            )
                            .await;
                            // force_automode 子开关：把 Ask 折叠成 Deny + reason 头部
                            // 加 force-automode: 前缀。让"放手跑"模式不被 ASK 打断。
                            let decision = if force_automode {
                                raw_decision.collapse_ask_to_deny()
                            } else {
                                raw_decision
                            };
                            sink(state.event(protocol::EventPayload::PermissionAutoJudged {
                                tool_name: call_name_for_judge.clone(),
                                decision: decision.as_label().to_string(),
                                reason: decision.reason().map(str::to_string),
                            }));
                            match decision {
                                crate::automode::AutoModeDecision::Allow => {
                                    hitl_for_future
                                        .resolve(request_id, ApprovalDecision::AllowOnce);
                                }
                                crate::automode::AutoModeDecision::Deny(reason) => {
                                    hitl_for_future.resolve(
                                        request_id,
                                        ApprovalDecision::DenyWithFeedback { feedback: reason },
                                    );
                                }
                                crate::automode::AutoModeDecision::Ask(_) => {
                                    // 保留人工决策
                                }
                            }
                        }
                    }
                }

                // 工具审批
                match await_permission_decision(&sink, &state, permission).await {
                    Ok(()) => {}
                    Err(reason) => {
                        record_tool_outcome(attr::outcome::DENIED, &call.name, 0.0, false, 0);
                        return Ok(deny_tool(
                            call,
                            call_index,
                            dispatch_index,
                            &state,
                            &sink,
                            reason,
                        ));
                    }
                }

                if cancellation::is_cancelled(&cancel) {
                    return Err(ModelError::Cancelled);
                }

                // 工具入参可被 PreToolUse hook 改写（架构 §4.8.2 / §4.8.4）。
                let mut effective_input = call.input.clone();

                // PreToolUse hook（架构 §4.8.1 / §4.8.2）：允许外部 hook
                // (1) 阻断工具调用；(2) Modify { input } 改写工具入参。
                if !hooks_for_future.is_empty() {
                    let sid = session_id_for_hooks.clone().unwrap_or_default();
                    let hook_point = crate::hooks::HookPoint::PreToolUse {
                        session_id: sid,
                        tool_name: call.name.clone(),
                        input: effective_input.clone(),
                    };
                    match hooks_for_future.trigger(&hook_point).await {
                        crate::hooks::HookOutcome::Block(reason) => {
                            record_tool_outcome(attr::outcome::DENIED, &call.name, 0.0, false, 0);
                            return Ok(deny_tool(
                                call,
                                call_index,
                                dispatch_index,
                                &state,
                                &sink,
                                format!("PreToolUse hook blocked: {reason}"),
                            ));
                        }
                        crate::hooks::HookOutcome::Modify(patch) => {
                            if let Some(new_input) = patch.input {
                                effective_input = new_input;
                            }
                        }
                        _ => {}
                    }
                }

                // —— Edit 工具快照：执行前拍 before（架构 §4.13.2）——
                // 按真实文件路径加排他锁，确保 snapshot_before + execute + snapshot_after
                // 不被其他 run 对同一文件的 Edit 打断（架构 §4.13.4）。
                let _edit_lock = if call.name == "Edit" {
                    edits_worktree_for_snapshot.as_ref().and_then(|wt| {
                        effective_input["file_path"]
                            .as_str()
                            .map(Path::new)
                            .and_then(|fp| wt.lock_file(fp).ok())
                    })
                } else {
                    None
                };
                let edit_before = if call.name == "Edit" {
                    if let Some(wt) = edits_worktree_for_snapshot.as_ref() {
                        let fp = effective_input["file_path"]
                            .as_str()
                            .map(Path::new);
                        if let Some(fp) = fp {
                            wt.snapshot_before(&call.id, fp).await.unwrap_or(None)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // 执行
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    input = %effective_input,
                    "tool_call executing"
                );
                sink(state.event(EventPayload::ToolCallStarted {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    input: effective_input.clone(),
                }));

                let started = Instant::now();
                let (raw, exec_failed) = match tool {
                    Some(t) => match t.execute(effective_input.clone()).await {
                        Ok(s) => (s, false),
                        Err(e) => {
                            warn!(tool = %call.name, error = %e, "tool exec error");
                            (format!("工具执行错误: {e}"), true)
                        }
                    },
                    None => {
                        warn!(tool = %call.name, "tool not in registry");
                        (format!("未找到工具: {}", call.name), true)
                    }
                };
                let duration_ms = started.elapsed().as_millis() as u64;

                // —— Edit 工具快照：执行后拍 after + 写 metadata + emit 事件 ——
                if call.name == "Edit" && !exec_failed {
                    if let Some(wt) = edits_worktree_for_snapshot.as_ref() {
                        let fp = effective_input["file_path"]
                            .as_str()
                            .map(Path::new);
                        if let Some(fp) = fp {
                            if let Ok(Some(after)) =
                                wt.snapshot_after(&call.id, fp).await
                            {
                                let before_existed = edit_before
                                    .as_ref()
                                    .map_or(false, |b| b.file_bytes > 0);
                                let action =
                                    if effective_input["old_string"]
                                        .as_str()
                                        .map_or(false, |s| s.is_empty())
                                    {
                                        protocol::EditAction::Create
                                    } else if !before_existed {
                                        protocol::EditAction::Create
                                    } else {
                                        // overwrite = old_string 长度接近原文件大小
                                        let old_len =
                                            effective_input["old_string"]
                                                .as_str()
                                                .map_or(0, |s| s.len() as u64);
                                        let before_len = edit_before
                                            .as_ref()
                                            .map_or(0, |b| b.file_bytes);
                                        if old_len >= before_len.saturating_sub(10)
                                        {
                                            protocol::EditAction::Overwrite
                                        } else {
                                            protocol::EditAction::Modify
                                        }
                                    };
                                let snapshot_id =
                                    uuid::Uuid::new_v4().to_string();
                                let entry = EditEntry {
                                    snapshot_id: snapshot_id.clone(),
                                    call_id: call.id.clone(),
                                    tool: "Edit".into(),
                                    real_path: fp.to_string_lossy().to_string(),
                                    action,
                                    before_sha: edit_before
                                        .as_ref()
                                        .map_or("".into(), |b| b.sha.clone()),
                                    after_sha: after.sha.clone(),
                                    before_bytes: edit_before
                                        .as_ref()
                                        .map_or(0, |b| b.file_bytes),
                                    after_bytes: after.file_bytes,
                                    ts_ms: chrono::Utc::now()
                                        .timestamp_millis(),
                                    reverted: false,
                                    reverted_at_ms: None,
                                };
                                let _ = wt.append_entry(entry);
                                sink(state.event(
                                    EventPayload::EditSnapshotCreated {
                                        call_id: call.id.clone(),
                                        snapshot_id,
                                        file_path: fp
                                            .to_string_lossy()
                                            .to_string(),
                                        action,
                                        before_sha: edit_before
                                            .as_ref()
                                            .map_or(String::new(), |b| b.sha.clone()),
                                        after_sha: after.sha,
                                        before_bytes: edit_before
                                            .as_ref()
                                            .map_or(0, |b| b.file_bytes),
                                        after_bytes: after.file_bytes,
                                    },
                                ));
                            }
                        }
                    }
                }

                // 大输出统一落 artifact（架构 §4.4.9）：超 6 KB 写盘 + 给模型「头部预览 +
                // 工件指针」。失败路径（exec_failed）不走 materialize——错误文本通常很短，
                // 即便不短也不该把错误升格成需要 Read 的工件。
                // Read 是分页工具，自身已做单行截断 + 整体 6KB 输出截断 + offset/limit 提示，
                // 再走 materialize 会形成"Read → 落盘 → Read 落盘文件 → 再落盘"的死循环。
                let (materialized, artifact) = if exec_failed || call.name == "Read" {
                    (raw, None)
                } else {
                    materialize_tool_output(
                        raw,
                        &call.id,
                        session_id_for_hooks.as_deref(),
                        data_dir_for_artifacts.as_deref(),
                    )
                };
                // Read 自身的截断标志着"输出被裁减了，agent 应该改 offset/limit"，
                // 所以 truncated 只用 dispatch 侧的截断标记，不混用 Read 自己生成的消息。
                let (mut content, truncated) = if call.name == "Read" {
                    (materialized, false)
                } else {
                    truncate_tool_result(materialized)
                };
                let outcome = if !tool_found {
                    attr::outcome::NOT_FOUND
                } else if exec_failed {
                    attr::outcome::FAILED
                } else {
                    attr::outcome::OK
                };
                record_tool_outcome(
                    outcome,
                    &call.name,
                    duration_ms as f64,
                    truncated,
                    content.len(),
                );

                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    outcome,
                    duration_ms,
                    truncated,
                    bytes = content.len(),
                    "tool_call finished"
                );

                // PostToolUse / PostToolUseFailure hook（架构 §4.8.1 / §4.8.2）：
                // 成功路径下接受 Modify { result } 改写最终工具结果文本；失败路径仅观察。
                if !hooks_for_future.is_empty() {
                    let sid = session_id_for_hooks.clone().unwrap_or_default();
                    let hook_point = if exec_failed {
                        crate::hooks::HookPoint::PostToolUseFailure {
                            session_id: sid,
                            tool_name: call.name.clone(),
                            error: content.clone(),
                        }
                    } else {
                        crate::hooks::HookPoint::PostToolUse {
                            session_id: sid,
                            tool_name: call.name.clone(),
                            result: content.clone(),
                        }
                    };
                    if let crate::hooks::HookOutcome::Modify(patch) =
                        hooks_for_future.trigger(&hook_point).await
                    {
                        if !exec_failed {
                            if let Some(new_result) = patch.result {
                                content = new_result;
                            }
                        }
                    }
                }

                let artifact_path_str = artifact.as_ref().map(|a| a.path.display().to_string());
                sink(state.event(EventPayload::ToolCallFinished {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    result: content.clone(),
                    duration_ms,
                    truncated,
                    artifact_path: artifact_path_str,
                }));

                Ok((
                    call_index,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content,
                        artifact,
                    },
                ))
            }
            .instrument(tool_span),
        )
    }

    /// `ask` 工具派发：emit UserQuestionRequested + await UserAnswer。
    fn spawn_ask(
        &self,
        call: ToolCall,
        call_index: usize,
        dispatch_index: usize,
    ) -> BoxFuture<'static, Result<(usize, ToolResult), ModelError>> {
        let hitl = self.hitl.clone();
        let state = self.state.clone();
        let sink = self.sink.clone();
        let cancel = self.cancel.clone();

        let tool_span = tracing::info_span!(
            "tool.call",
            otel.kind = "internal",
            hebbian.tool.name = %call.name,
            hebbian.tool.call_id = %call.id,
            hebbian.tool.class = "needs_human_input",
            hebbian.tool.outcome = Empty,
        );

        Box::pin(
            async move {
                let (question, options, multi) = match parse_ask_input(&call.input) {
                    Ok(parts) => parts,
                    Err(err) => {
                        record_tool_outcome(attr::outcome::FAILED, &call.name, 0.0, false, 0);
                        return Ok(finish_ask_with_error(
                            call,
                            call_index,
                            dispatch_index,
                            &state,
                            &sink,
                            err,
                        ));
                    }
                };

                sink(state.event(EventPayload::ToolCallStarted {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                }));

                let (request_id, waiter) = hitl.open_question();
                sink(state.event(EventPayload::UserQuestionRequested {
                    request_id: request_id.clone(),
                    question,
                    options,
                    multi,
                }));

                // permission.check 子 span：记录 ask 等待时长
                let permission_span = tracing::info_span!(
                    "permission.check",
                    hebbian.permission.kind = "ask",
                    hebbian.permission.request_id = %request_id,
                    hebbian.permission.decision = Empty,
                );
                let wait_started = Instant::now();
                let answer = waiter
                    .instrument(permission_span.clone())
                    .await
                    .unwrap_or(UserAnswer::Cancelled);
                let wait_ms = wait_started.elapsed().as_millis() as f64;
                let answer_label = match &answer {
                    UserAnswer::Selected { .. } => "selected",
                    UserAnswer::SelectedMulti { .. } => "selected_multi",
                    UserAnswer::Custom { .. } => "custom",
                    UserAnswer::Cancelled => "cancelled",
                };
                permission_span.record(attr::PERMISSION_DECISION, answer_label);
                metrics::record_permission_wait("ask", answer_label, wait_ms);

                sink(state.event(EventPayload::UserQuestionAnswered {
                    request_id,
                    answer: answer.clone(),
                }));

                // 即使 run 已被外部 cancel（例如 desktop 关窗），仍然把「取消」当作正常的
                // ask 答案落到事件流里：emit ToolCallFinished + 推 ToolResult，让 surface
                // 看到 ask 已经收到「用户取消」答案；下一步 cancel 检查再让 agent_loop bail。
                let content = answer.to_agent_text();
                let outcome = if matches!(answer, UserAnswer::Cancelled) {
                    attr::outcome::DENIED
                } else {
                    attr::outcome::OK
                };
                record_tool_outcome(outcome, &call.name, wait_ms, false, content.len());
                sink(state.event(EventPayload::ToolCallFinished {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    result: content.clone(),
                    duration_ms: 0,
                    truncated: false,
                    artifact_path: None,
                }));

                if cancellation::is_cancelled(&cancel) {
                    return Err(ModelError::Cancelled);
                }

                Ok((
                    call_index,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content,
                        artifact: None,
                    },
                ))
            }
            .instrument(tool_span),
        )
    }

    /// 申请越界路径访问审批：开 pending + emit `PermissionRequested { kind: PathAccess }`。
    fn request_path_approval(&self, tool_name: &str, paths: Vec<PathBuf>) -> PathApproval {
        // 路径越界不在工具维度，AllowAndRemember 在外层把路径加进 workspace.allowed_paths，
        // 不通过 hitl learned 表，所以传 None。
        let (request_id, waiter) = self.hitl.open_approval(None, None);
        let path_strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let summary = if path_strings.len() == 1 {
            format!("工具 {tool_name} 想访问越界路径：{}", path_strings[0])
        } else {
            format!("工具 {tool_name} 想访问 {} 个越界路径", path_strings.len())
        };
        self.emit(EventPayload::PermissionRequested {
            request_id: request_id.clone(),
            kind: PermissionKind::PathAccess {
                tool_name: tool_name.to_string(),
                paths: path_strings,
            },
            summary,
            risk: RiskLevel::Medium,
        });
        PathApproval {
            request_id,
            paths,
            waiter,
        }
    }

    fn emit(&self, payload: EventPayload) {
        (self.sink)(self.state.event(payload));
    }
}

/// 等路径审批结果；批准则按 scope 累加到 workspace.allowed_paths +
/// 持久化到 PermissionStore（让其他 session 也能共享）。
async fn await_path_decision(
    sink: &EventSink,
    state: &Arc<RunState>,
    workspace: &Arc<Workspace>,
    permission_store: &Option<Arc<PermissionStore>>,
    session_id: Option<&str>,
    pending: PathApproval,
) -> Result<(), String> {
    let PathApproval {
        request_id,
        paths,
        waiter,
    } = pending;

    let permission_span = tracing::info_span!(
        "permission.check",
        hebbian.permission.kind = "path_access",
        hebbian.permission.request_id = %request_id,
        hebbian.permission.decision = Empty,
    );
    let wait_started = Instant::now();
    let decision_result = waiter.instrument(permission_span.clone()).await;
    let wait_ms = wait_started.elapsed().as_millis() as f64;
    let decision = decision_result.map_err(|_| "路径审批通道已关闭".to_string())?;
    let decision_label = approval_decision_label(&decision);
    permission_span.record(attr::PERMISSION_DECISION, decision_label);
    metrics::record_permission_wait("path_access", decision_label, wait_ms);

    sink(state.event(EventPayload::PermissionResolved {
        request_id,
        decision: decision.clone(),
    }));
    match decision {
        ApprovalDecision::AllowOnce => Ok(()),
        ApprovalDecision::AllowAndRemember { scope, .. } => {
            for p in &paths {
                workspace.add_allowed_path(p.clone());
                // 持久化到 PermissionStore：其他 session 通过 out-of-scope filter
                // 里的 allows_path() 检查共享这些规则。
                if let Some(store) = permission_store {
                    let (sid, workdir_for_rule) = match scope {
                        protocol::PermissionScope::Once => continue,
                        protocol::PermissionScope::Session => (session_id, None),
                        protocol::PermissionScope::Project => {
                            (None, Some(workspace.workdir().to_path_buf()))
                        }
                        protocol::PermissionScope::Global => (None, None),
                    };
                    if let Err(e) = store.add_path_rule(
                        sid,
                        workdir_for_rule,
                        p.to_string_lossy().to_string(),
                        scope,
                    ) {
                        tracing::warn!(error = %e, path = %p.display(), "路径规则持久化失败");
                    }
                }
            }
            Ok(())
        }
        ApprovalDecision::Deny => Err("用户拒绝路径访问".into()),
        ApprovalDecision::DenyWithFeedback { feedback } => Err(feedback),
    }
}

/// 等工具审批结果；emit `PermissionResolved`。
async fn await_permission_decision(
    sink: &EventSink,
    state: &Arc<RunState>,
    decision: PermissionDecision,
) -> Result<(), String> {
    match decision {
        PermissionDecision::Approved => Ok(()),
        PermissionDecision::Denied { reason } => Err(reason),
        PermissionDecision::NeedsApproval { request_id, waiter } => {
            let permission_span = tracing::info_span!(
                "permission.check",
                hebbian.permission.kind = "tool_call",
                hebbian.permission.request_id = %request_id,
                hebbian.permission.decision = Empty,
            );
            let wait_started = Instant::now();
            let outcome_result = waiter.instrument(permission_span.clone()).await;
            let wait_ms = wait_started.elapsed().as_millis() as f64;
            let outcome = outcome_result.map_err(|_| "审批通道已关闭".to_string())?;
            let decision_label = approval_decision_label(&outcome);
            permission_span.record(attr::PERMISSION_DECISION, decision_label);
            metrics::record_permission_wait("tool_call", decision_label, wait_ms);

            sink(state.event(EventPayload::PermissionResolved {
                request_id,
                decision: outcome.clone(),
            }));
            match outcome {
                ApprovalDecision::AllowOnce | ApprovalDecision::AllowAndRemember { .. } => Ok(()),
                ApprovalDecision::Deny => Err("用户拒绝".into()),
                ApprovalDecision::DenyWithFeedback { feedback } => Err(feedback),
            }
        }
    }
}

fn record_tool_outcome(outcome: &str, tool: &str, duration_ms: f64, truncated: bool, bytes: usize) {
    let span = tracing::Span::current();
    span.record(attr::TOOL_OUTCOME, outcome);
    span.record(attr::TOOL_TRUNCATED, truncated);
    span.record(attr::TOOL_RESULT_SIZE, bytes as i64);
    span.record(attr::TOOL_NAME, tool);
    metrics::record_tool_duration(tool, outcome, duration_ms);
}

/// 把"被拒"渲染为 ToolStarted/Finished + ToolResult，让 transcript 一致。
fn deny_tool(
    call: ToolCall,
    call_index: usize,
    dispatch_index: usize,
    state: &Arc<RunState>,
    sink: &EventSink,
    reason: String,
) -> (usize, ToolResult) {
    warn!(tool = %call.name, %reason, "tool denied");
    let denied = format!("工具调用被拒绝: {reason}");
    sink(state.event(EventPayload::ToolCallStarted {
        index: dispatch_index,
        call_id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
    }));
    sink(state.event(EventPayload::ToolCallFinished {
        index: dispatch_index,
        call_id: call.id.clone(),
        result: denied.clone(),
        duration_ms: 0,
        truncated: false,
        artifact_path: None,
    }));
    (
        call_index,
        ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: denied,
            artifact: None,
        },
    )
}

fn finish_ask_with_error(
    call: ToolCall,
    call_index: usize,
    dispatch_index: usize,
    state: &Arc<RunState>,
    sink: &EventSink,
    error: String,
) -> (usize, ToolResult) {
    sink(state.event(EventPayload::ToolCallStarted {
        index: dispatch_index,
        call_id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
    }));
    sink(state.event(EventPayload::ToolCallFinished {
        index: dispatch_index,
        call_id: call.id.clone(),
        result: error.clone(),
        duration_ms: 0,
        truncated: false,
        artifact_path: None,
    }));
    (
        call_index,
        ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: error,
            artifact: None,
        },
    )
}

/// 工具输出超阈值时落 artifact（架构 §4.4.9 / §4.12.11 Phase 2）：
/// - 小于 `MAX_TOOL_RESULT_INLINE` → 原样返回，`artifact` = None
/// - 大于阈值且 `data_dir + session_id` 可用 → 全量写到
///   `<data_dir>/sessions/<sid>/tool_results/<call_id>.txt`，inline 改为
///   「头部 ~2 KB 预览 + 工件路径指针」。模型看到指针后可以用 Read 翻页
///   （Read 默认 limit=2000 行，自带分块）
/// - 大于阈值但没 data_dir（CLI 单跑 / 单测）→ 回落到旧的截断路径
fn materialize_tool_output(
    raw: String,
    call_id: &str,
    session_id: Option<&str>,
    data_dir: Option<&Path>,
) -> (String, Option<ToolArtifact>) {
    if raw.len() <= MAX_TOOL_RESULT_INLINE {
        return (raw, None);
    }
    let (Some(sid), Some(dd)) = (session_id, data_dir) else {
        return (raw, None); // 调用方继续走 truncate_tool_result
    };

    let path = match crate::storage::tool_results::save_tool_result(dd, sid, call_id, &raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(call_id, error = %e, "materialize: save_tool_result failed; fallback truncate");
            return (raw, None);
        }
    };

    let total_bytes = raw.len() as u64;
    let line_count = raw.lines().count() as u32;
    let head = head_preview_bytes(&raw, ARTIFACT_HEAD_PREVIEW_BYTES);
    let inline = format!(
        "{head}\n…\n[输出 {total_bytes} 字节 / {line_count} 行，完整内容已落盘到：{path}\n用 Read 按 offset/limit 翻页读取。]",
        path = path.display(),
    );
    (
        inline,
        Some(ToolArtifact {
            path,
            bytes: total_bytes,
            line_count: Some(line_count),
        }),
    )
}

fn head_preview_bytes(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn truncate_tool_result(raw: String) -> (String, bool) {
    if raw.len() <= MAX_TOOL_RESULT_INLINE {
        return (raw, false);
    }

    let mut end = MAX_TOOL_RESULT_INLINE;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }

    (format!("{}…[已截断]", &raw[..end]), true)
}

fn parse_ask_input(
    input: &serde_json::Value,
) -> Result<(String, Vec<QuestionOption>, bool), String> {
    let parsed: AskInput = serde_json::from_value(input.clone())
        .map_err(|e| format!("ask 工具 input 解析失败：{e}"))?;
    if !(2..=5).contains(&parsed.options.len()) {
        return Err(format!(
            "ask 工具要求提供 2-5 个选项，实际给了 {} 个",
            parsed.options.len()
        ));
    }
    Ok((parsed.question, parsed.options, parsed.multi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_state::RunState;
    use crate::tools::bash::BashTool;
    use crate::tools::registry::ToolRegistry;
    use crate::workspace::Workspace;
    use model_gateway::types::ToolCall;
    use protocol::RunId;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;

    /// 端到端复现 desktop 路径：dispatch 一个 Bash destructive 调用 → emit 收到
    /// PermissionRequested → 模拟 surface 通过 hitl gate resolve → waiter 唤醒 → 命令执行。
    /// 用来兜底"审批后卡住"这类回归。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn destructive_bash_resolves_after_approval() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let registry = Arc::new(ToolRegistry::new(vec![Box::new(BashTool::new(
            workspace.clone(),
            crate::tools::background::BackgroundShells::new(),
            None,
        ))
            as Box<dyn crate::tools::Tool>]));
        let hitl = Arc::new(crate::tools::hitl::HitlGate::default());
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let sink: crate::agent_loop::EventSink = Arc::new(move |event| {
            let _ = tx.try_send(event);
        });
        let dispatcher = ToolDispatcher {
            registry,
            hitl: hitl.clone(),
            workspace,
            state: run_state,
            sink,
            cancel: Arc::new(AtomicBool::new(false)),
            run_mode: crate::run_mode::RunMode::AskBeforeEdits,
            model_id: None,
            judge_client: None,
            force_automode: false,
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
        };

        let call = ToolCall {
            id: "call_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({ "command": "echo hi && touch a.txt", "cwd": tmp.path() }),
        };

        // 模拟 surface：等到 PermissionRequested 事件到达后，调 hitl.resolve(AllowOnce)
        let hitl_for_surface = hitl.clone();
        let surface = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let EventPayload::PermissionRequested { request_id, .. } = &event.payload {
                    hitl_for_surface.resolve(request_id, ApprovalDecision::AllowOnce);
                    break;
                }
            }
        });

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("dispatch 在 5s 内应当完成（不应卡在审批）");

        let results = result.expect("dispatch 不应返回错误");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Bash");

        surface.await.unwrap();
    }

    #[test]
    fn truncate_tool_result_preserves_utf8_char_boundaries() {
        let raw = format!("{}中", "a".repeat(MAX_TOOL_RESULT_INLINE - 1));
        assert!(raw.len() > MAX_TOOL_RESULT_INLINE);
        assert!(!raw.is_char_boundary(MAX_TOOL_RESULT_INLINE));

        let (content, truncated) = truncate_tool_result(raw);

        assert!(truncated);
        assert_eq!(
            content,
            format!("{}…[已截断]", "a".repeat(MAX_TOOL_RESULT_INLINE - 1))
        );
    }

    /// 架构 §4.4.9：超阈值输出落 artifact + inline 换成「头部预览 + 工件指针」。
    #[test]
    fn materialize_above_threshold_writes_artifact_and_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let sid = "20260512-test1";
        std::fs::create_dir_all(data_dir.join("sessions").join(sid)).unwrap();
        // 7 KB > MAX_TOOL_RESULT_INLINE(6 KB)
        let raw = "x".repeat(7_000);
        let (inline, artifact) =
            materialize_tool_output(raw.clone(), "call_abc", Some(sid), Some(data_dir));
        let a = artifact.expect("artifact should be produced");
        assert!(a.path.ends_with("call_abc.txt"));
        assert_eq!(a.bytes, 7_000);
        let on_disk = std::fs::read_to_string(&a.path).unwrap();
        assert_eq!(on_disk, raw);
        assert!(inline.starts_with("xxxxxxx"), "head preview at start");
        assert!(inline.contains("已落盘到"));
        assert!(inline.contains("call_abc.txt"));
        assert!(inline.len() < raw.len(), "inline shrunk");
    }

    #[test]
    fn materialize_under_threshold_passes_through() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = "small".to_string();
        let (inline, artifact) =
            materialize_tool_output(raw.clone(), "c1", Some("sid"), Some(tmp.path()));
        assert!(artifact.is_none());
        assert_eq!(inline, raw);
    }

    #[test]
    fn materialize_without_data_dir_passes_through() {
        let raw = "y".repeat(7_000);
        let (inline, artifact) = materialize_tool_output(raw.clone(), "c1", None, None);
        // 没 data_dir → 不落盘，inline 保持原样（交给后续 truncate 收尾）
        assert!(artifact.is_none());
        assert_eq!(inline, raw);
    }
}
