//! 工具派发器：把一组 tool call 跑出 `Vec<ToolResult>`。
//!
//! 接管以下职责（让 [`agent_loop`] 不必再 inline 200 行 closure）：
//! - 路径越界审批 → emit `PermissionRequested { kind: PathAccess }` + await
//! - 工具审批 → emit `PermissionRequested { kind: ToolCall }` + await
//! - 提问通路（`ask` 工具）→ emit `UserQuestionRequested` + await
//! - 工具执行（含超时输出截断）+ emit `ToolCallStarted` / `ToolCallFinished`
//!
//! 并发策略（架构 §4.4.3）：只读 / 并发安全工具立即进后台并发池（同批最多
//! [`MAX_PARALLEL_TOOLS`] 个同时 poll，避免一次上百个 tool_call 打满 worker / 句柄）；
//! 会写 shell（Bash/PowerShell）走独立串行链，shell 之间严格按 call 顺序（共享 cwd
//! 不并发 + 审批记忆顺序）。两条链 join 同时驱动——**会写 shell 卡在审批时，只读池
//! 照常并发执行**，不再"一个审批卡住整批冻住"。同一文件的 Edit 由 edits-worktree 的
//! per-path 锁（架构 §4.13.4）天然串行，无需派发器额外处理。
//!
//! [`agent_loop`]: super::agent_loop

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::BoxFuture;
use futures_util::stream::{self, StreamExt};
use observability::attr;
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
    storage::{plan_comments, plans, sessions as session_store},
    tools::{
        exit_plan_mode::{self, EXIT_PLAN_MODE_TOOL_NAME},
        hitl::{HitlGate, PermissionDecision},
        registry::ToolRegistry,
        todo_write::{self, TODO_WRITE_TOOL_NAME},
        ToolCtx, ToolProgress, ASK_TOOL_NAME,
    },
    workspace::Workspace,
};

/// 把 dispatch 主事件 sink 包成 `ToolProgress`：拿到 chunk 后转成
/// `ToolCallOutputDelta` 事件喂回去。`dispatch_index` / `call_id` 在闭包构造时
/// 就锁死，工具内部不需要知道这些元信息。
struct ToolProgressEmitter {
    sink: EventSink,
    state: Arc<RunState>,
    dispatch_index: usize,
    call_id: String,
}

impl ToolProgress for ToolProgressEmitter {
    fn emit(&self, chunk: String) {
        if chunk.is_empty() {
            return;
        }
        (self.sink)(self.state.event(EventPayload::ToolCallOutputDelta {
            index: self.dispatch_index,
            call_id: self.call_id.clone(),
            chunk,
        }));
    }
}

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
use model_gateway::types::{ModelError, ToolArtifact, ToolCall, ToolResult, TranscriptEntry};

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
    /// Subagent / NestedRun 上下文（架构 §4.4.11）。`None` 表示当前进程不支持
    /// subagent 调度（单测路径 / 没有可用 subagent 定义时），spawn_task 直接拒绝。
    pub subagent_ctx: Option<Arc<crate::subagent::SubagentCtx>>,
    /// 父 Transcript 在「本轮 assistant tool_calls push 之前」的 entries 快照（架构 §4.4.11.3 inherit）。
    /// 仅当本轮 `calls` 含 `Task` 工具时由 agent_loop 抓取；否则 `None` 跳过克隆。
    /// `Arc` 共享给同 ToolStep 内所有 Task（parallel 启动看到同一形态）。
    pub parent_transcript_snapshot: Option<Arc<Vec<TranscriptEntry>>>,
}

impl ToolDispatcher {
    /// 派发整组 tool call。返回按 call_index 排序的 ToolResult。
    /// `dispatch_offset` 是这一轮在整个 run 内的全局起始 index。
    pub async fn run_calls(
        &self,
        calls: &[ToolCall],
        dispatch_offset: usize,
    ) -> Result<Vec<ToolResult>, ModelError> {
        // 分流（架构 §4.4.3）：只读 / 并发安全工具立刻 spawn 进后台并发池；会写
        // shell（Bash/PowerShell）收集索引走串行链。两者用 join 同时驱动——会写
        // shell 卡在审批时，只读池照常并发执行，不再"一个审批卡住整批冻住"。
        // shell 串行链顺序 spawn（而非一次性预创建 pending）保证：用户对上一条选
        // AllowAndRemember 后，同批后续 shell 先命中记忆规则，不重复弹审批。
        let mut concurrent: Vec<BoxFuture<'static, Result<(usize, ToolResult), ModelError>>> =
            Vec::with_capacity(calls.len());
        let mut shell_indices: Vec<usize> = Vec::new();

        for (call_index, call) in calls.iter().enumerate() {
            if cancellation::is_cancelled(&self.cancel) {
                self.hitl.cancel_all_pending();
                break;
            }
            let dispatch_index = dispatch_offset + call_index;
            if matches!(call.name.as_str(), "Bash" | "PowerShell") {
                shell_indices.push(call_index);
                continue;
            }
            let task = if call.name == ASK_TOOL_NAME {
                self.spawn_ask(call.clone(), call_index, dispatch_index)
            } else if call.name == TODO_WRITE_TOOL_NAME {
                self.spawn_todo_write(call.clone(), call_index, dispatch_index)
            } else if call.name == EXIT_PLAN_MODE_TOOL_NAME {
                self.spawn_exit_plan_mode(call.clone(), call_index, dispatch_index)
            } else if call.name == crate::tools::task::TASK_TOOL_NAME {
                self.spawn_task(call.clone(), call_index, dispatch_index)
            } else {
                self.spawn_tool(call.clone(), call_index, dispatch_index)
            };
            concurrent.push(task);
        }

        // 会写 shell 串行链：顺序 spawn + await，shell 之间严格按 call 顺序（共享 cwd
        // 不并发 + 审批记忆顺序）。审批 await 期间让出，下面的并发池继续推进。
        let shell_chain = async {
            let mut out: Vec<(usize, ToolResult)> = Vec::new();
            for &call_index in &shell_indices {
                if cancellation::is_cancelled(&self.cancel) {
                    self.hitl.cancel_all_pending();
                    break;
                }
                let dispatch_index = dispatch_offset + call_index;
                let task = self.spawn_tool(calls[call_index].clone(), call_index, dispatch_index);
                out.push(task.await?);
            }
            Ok::<Vec<(usize, ToolResult)>, ModelError>(out)
        };

        // 并发池：最多 MAX_PARALLEL_TOOLS 个同时 poll；单个工具报错不 cancel 同批其他。
        let concurrent_drain = async {
            let mut out: Vec<(usize, ToolResult)> = Vec::new();
            let mut stream = stream::iter(concurrent).buffer_unordered(MAX_PARALLEL_TOOLS);
            let mut first_err: Option<ModelError> = None;
            while let Some(outcome) = stream.next().await {
                match outcome {
                    Ok(result) => out.push(result),
                    Err(e) if first_err.is_none() => first_err = Some(e),
                    Err(_) => {}
                }
            }
            match first_err {
                Some(e) => Err(e),
                None => Ok(out),
            }
        };

        let (shell_out, concurrent_out) =
            futures_util::future::join(shell_chain, concurrent_drain).await;
        let mut results = shell_out?;
        results.extend(concurrent_out?);
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

        // —— 工具派发日志：effects 分析结果 ——
        {
            let segment_summary: Vec<String> = effects
                .segments
                .iter()
                .map(|s| {
                    if s.write_targets.is_empty() {
                        s.fingerprint.clone()
                    } else {
                        format!("{}[w={}]", s.fingerprint, s.write_targets.join(","))
                    }
                })
                .collect();
            let path_summary: Vec<String> = effects
                .paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            info!(
                tool = %call.name,
                call_id = %call.id,
                class = class_label,
                fingerprint = fingerprint.as_deref().unwrap_or(""),
                segments = segment_summary.join(" | "),
                dangerous_kinds = ?effects.dangerous_kinds,
                paths = path_summary.join(", "),
                "effects analysis"
            );
        }

        // 路径越界检查（同步）。
        // 当前 session 数据目录（`~/.hebbian/sessions/<sid>/`）下的文件是 agent
        // 自己的工具输出（line_trunc/、tool_results/、bg/ 等），永远视为在界内，
        // 不触发 PathAccess 审批。范围限定到 session 目录，避免配置文件被随便读。
        let mut out_of_scope: Vec<PathBuf> = Vec::new();
        for p in &effects.paths {
            if self.workspace.allows(p) {
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    path = %p.display(),
                    matched = true,
                    level = "workspace",
                    "[Permission:Path] workspace allowed path matched"
                );
                continue;
            }
            let session_artifact_allowed = self
                .data_dir_for_artifacts
                .as_ref()
                .zip(self.session_id_for_hooks.as_deref())
                .map_or(false, |(dd, sid)| {
                    p.starts_with(&dd.join("sessions").join(sid))
                });
            if session_artifact_allowed {
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    path = %p.display(),
                    matched = true,
                    level = "session_artifact",
                    "[Permission:Path] session artifact path allowed"
                );
                continue;
            }
            if let Some(hit) = self.permission_store.as_ref().and_then(|store| {
                store.allows_path_diagnostic(
                    self.session_id_for_hooks.as_deref(),
                    Some(self.workspace.workdir()),
                    &p.to_string_lossy(),
                )
            }) {
                let level = match hit.scope {
                    protocol::PermissionScope::Once => "once",
                    protocol::PermissionScope::Session => "session",
                    protocol::PermissionScope::Project => "project",
                    protocol::PermissionScope::Global => "global",
                };
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    path = %p.display(),
                    matched = true,
                    level,
                    pattern = %hit.pattern,
                    "[Permission:Path] PermissionStore path rule matched"
                );
                continue;
            }
            info!(
                tool = %call.name,
                call_id = %call.id,
                path = %p.display(),
                matched = false,
                result = "waiting_for_approval",
                "[Permission:Path] no allowed path matched"
            );
            out_of_scope.push(p.clone());
        }

        let path_pending = if out_of_scope.is_empty() {
            info!(
                tool = %call.name,
                call_id = %call.id,
                total = effects.paths.len(),
                "[Permission:Path] all paths in bounds"
            );
            None
        } else {
            let out_str: Vec<String> = out_of_scope
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            info!(
                tool = %call.name,
                call_id = %call.id,
                out_of_scope = out_str.join(", "),
                total = effects.paths.len(),
                result = "waiting_for_approval",
                "[Permission:Path] some paths out of bounds, waiting for approval"
            );
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
                    "[Permission:ToolCall] approved (auto)"
                );
            }
            PermissionDecision::Denied { reason } => {
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    class = class_label,
                    %reason,
                    "[Permission:ToolCall] denied by policy"
                );
            }
            PermissionDecision::NeedsApproval { request_id, .. } => {
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    class = class_label,
                    request_id = %request_id,
                    fingerprint = fingerprint.as_deref().unwrap_or(""),
                    "[Permission:ToolCall] needs human approval"
                );
                // UI 记忆勾选区只列「会写 + 可记忆 + 尚未审批过」的段：只读段、不可
                // 记忆段（rm…）、以及之前已记住的段（如记过的 cd）都不出现，用户只对
                // 本次真正新增的会写段决定是否记忆（架构 §4.4.2.3）。完整命令仍在
                // BashArgsPreview 里可见。
                let command_segments = self
                    .hitl
                    .unapproved_memorable_writable_segments(&call.name, &effects);
                // 完整段级状态（已白名单 / 待审 / rm 红禁选 / 只读）+ 整条是否禁记忆
                // （危险复合）。让弹窗逐段如实展示，rm 这类只标红不毒化良性段（架构 §4.4.2.3）。
                let segments = self.hitl.approval_segments(&call.name, &effects);
                let refuse_remember = effects.has_dangerous_pattern();
                self.emit(EventPayload::PermissionRequested {
                    request_id: request_id.clone(),
                    kind: PermissionKind::ToolCall {
                        tool_name: call.name.clone(),
                        input: call.input.clone(),
                        fingerprint: fingerprint.clone(),
                        command_segments,
                        segments,
                        refuse_remember,
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

        let tool_span_name = format!("tool.{}", call.name);
        let tool_span = tracing::info_span!(
            "tool.call",
            otel.name = %tool_span_name,
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
                        let is_edit = matches!(call_name_for_judge.as_str(), "Edit" | "edit");
                        if is_edit {
                            info!(
                                tool = %call_name_for_judge,
                                call_id = %call.id,
                                "EditAutomatically: NeedsApproval → AllowOnce (file edit shortcut)"
                            );
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
                        let model_id_str = model_id_for_judge.as_deref().unwrap_or("");
                        // 运行时读用户配置的 AutoMode 白名单（设置里改了即时生效，免重启）。
                        let automode_models = data_dir_for_artifacts
                            .as_deref()
                            .map(|d| crate::storage::settings::load(d).general.automode_models)
                            .unwrap_or_else(crate::storage::settings::default_automode_models);
                        if !crate::automode::is_allowed_model(model_id_str, &automode_models) {
                            // 模型不在白名单：不调判官，emit 一条 toast 提示并降级到普通审批
                            // （PermissionRequested 已 emit，保留人工决策）。dedup_key 让前端
                            // 对同一模型的多次提示只显示一个 toast，避免刷屏。
                            sink(state.event(protocol::EventPayload::Notice {
                                level: protocol::LogLevel::Warn,
                                message: format!(
                                    "当前模型「{model_id_str}」不在自动模式名单里，本次已转手动审批。可在「设置 → 自动模式」里把它加进去。"
                                ),
                                dedup_key: Some(format!("automode-unsupported:{model_id_str}")),
                            }));
                            info!(
                                target: "permission",
                                tool = %call_name_for_judge,
                                model = model_id_str,
                                allowlist = ?automode_models,
                                "[AutoMode] 模型不在白名单 → 弹 toast + 降级手动审批，不调判官"
                            );
                        } else if let Some(judge) = judge_client.as_ref() {
                            // judge 必须看到 hebbian 静态分析的全量 effects（segments /
                            // write_targets / dangerous_kinds），不重复解析 shell。
                            let prefix_outcome =
                                crate::automode::classify_bash_prefixes_for_automode(
                                    judge,
                                    model_id_str,
                                    &call_name_for_judge,
                                    &call_input_for_judge,
                                    &effects_for_judge,
                                )
                                .await;
                            let judge_effects = prefix_outcome.effects;
                            let raw_decision = crate::automode::judge_auto_mode(
                                judge,
                                model_id_str,
                                &call_name_for_judge,
                                &call_input_for_judge,
                                &judge_effects,
                                &[],
                            )
                            .await;
                            // force_automode 子开关：把 Ask 折叠成 Deny + reason 头部
                            // 加 force-automode: 前缀。让"放手跑"模式不被 ASK 打断。
                            let raw_label = raw_decision.as_label();
                            let decision = if force_automode {
                                raw_decision.collapse_ask_to_deny()
                            } else {
                                raw_decision
                            };
                            info!(
                                target: "permission",
                                tool = %call_name_for_judge,
                                call_id = %call.id,
                                model = model_id_str,
                                raw = raw_label,
                                final = decision.as_label(),
                                force_automode = force_automode,
                                reason = decision.reason().unwrap_or(""),
                                "[AutoMode] LLM 判官结果：{} → {}（{}）",
                                raw_label,
                                decision.as_label(),
                                decision.reason().unwrap_or("无理由")
                            );
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
                // 拿锁失败（如 30s 超时）直接跳过快照，不阻塞 Edit 本身。
                let _edit_lock = if call.name == "Edit" {
                    if let Some(wt) = edits_worktree_for_snapshot.as_ref() {
                        let fp = effective_input["file_path"].as_str().map(Path::new);
                        if let Some(fp) = fp {
                            wt.lock_file(fp).await.ok()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let edit_before = if call.name == "Edit" {
                    if let Some(wt) = edits_worktree_for_snapshot.as_ref() {
                        let fp = effective_input["file_path"].as_str().map(Path::new);
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
                    "[Permission:ToolCall] executing"
                );
                sink(state.event(EventPayload::ToolCallStarted {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    input: effective_input.clone(),
                }));

                let started = Instant::now();
                let progress: Arc<dyn ToolProgress> = Arc::new(ToolProgressEmitter {
                    sink: sink.clone(),
                    state: state.clone(),
                    dispatch_index,
                    call_id: call.id.clone(),
                });
                let tool_ctx = ToolCtx {
                    call_id: call.id.clone(),
                    progress: Some(progress),
                    session_id: session_id_for_hooks.clone(),
                    run_id: Some(state.run_id.to_string()),
                    cancel: Some(cancel.clone()),
                };
                let (raw, exec_failed) = match tool {
                    Some(t) => match t.execute_streaming(tool_ctx, effective_input.clone()).await {
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
                        let fp = effective_input["file_path"].as_str().map(Path::new);
                        if let Some(fp) = fp {
                            if let Ok(Some(after)) = wt.snapshot_after(&call.id, fp).await {
                                let before_existed =
                                    edit_before.as_ref().map_or(false, |b| b.file_bytes > 0);
                                let action = if effective_input["old_string"]
                                    .as_str()
                                    .map_or(false, |s| s.is_empty())
                                {
                                    protocol::EditAction::Create
                                } else if !before_existed {
                                    protocol::EditAction::Create
                                } else {
                                    // overwrite = old_string 长度接近原文件大小
                                    let old_len = effective_input["old_string"]
                                        .as_str()
                                        .map_or(0, |s| s.len() as u64);
                                    let before_len =
                                        edit_before.as_ref().map_or(0, |b| b.file_bytes);
                                    if old_len >= before_len.saturating_sub(10) {
                                        protocol::EditAction::Overwrite
                                    } else {
                                        protocol::EditAction::Modify
                                    }
                                };
                                let snapshot_id = uuid::Uuid::new_v4().to_string();
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
                                    before_bytes: edit_before.as_ref().map_or(0, |b| b.file_bytes),
                                    after_bytes: after.file_bytes,
                                    ts_ms: chrono::Utc::now().timestamp_millis(),
                                    reverted: false,
                                    reverted_at_ms: None,
                                };
                                let _ = wt.append_entry(entry);
                                sink(
                                    state.event(EventPayload::EditSnapshotCreated {
                                        call_id: call.id.clone(),
                                        snapshot_id,
                                        file_path: fp.to_string_lossy().to_string(),
                                        action,
                                        before_sha: edit_before
                                            .as_ref()
                                            .map_or(String::new(), |b| b.sha.clone()),
                                        after_sha: after.sha,
                                        before_bytes: edit_before
                                            .as_ref()
                                            .map_or(0, |b| b.file_bytes),
                                        after_bytes: after.file_bytes,
                                    }),
                                );
                            }
                        }
                    }
                }

                // 大输出统一落 artifact（架构 §4.4.9）：超 6 KB 写盘 + 给模型「头部预览 +
                // 工件指针」。失败路径（exec_failed）不走 materialize——错误文本通常很短，
                // 即便不短也不该把错误升格成需要 Read 的工件。
                // Read 是分页工具，自身已做单行截断 + 整体 6KB 输出截断 + offset/limit 提示，
                // 再走 materialize 会形成"Read → 落盘 → Read 落盘文件 → 再落盘"的死循环。
                let direct_result = matches!(call.name.as_str(), "Read" | "Skill");
                let (materialized, artifact) = if exec_failed || direct_result {
                    (raw, None)
                } else {
                    materialize_tool_output(
                        raw,
                        &call.id,
                        session_id_for_hooks.as_deref(),
                        data_dir_for_artifacts.as_deref(),
                    )
                };
                // Read 自身的截断标志着"输出被裁减了，agent 应该改 offset/limit"；Skill 是指令注入，
                // 也必须原样回填给模型。二者都不混用 dispatch 侧的截断标记。
                let (mut content, truncated) = if direct_result {
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
                    "[Permission:ToolCall] finished"
                );

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

        let tool_span_name = format!("tool.{}", call.name);
        let tool_span = tracing::info_span!(
            "tool.call",
            otel.name = %tool_span_name,
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
                let answer = waiter
                    .instrument(permission_span.clone())
                    .await
                    .unwrap_or(UserAnswer::Cancelled);
                let answer_label = match &answer {
                    UserAnswer::Selected { .. } => "selected",
                    UserAnswer::SelectedMulti { .. } => "selected_multi",
                    UserAnswer::Custom { .. } => "custom",
                    UserAnswer::Cancelled => "cancelled",
                };
                permission_span.record(attr::PERMISSION_DECISION, answer_label);

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
                record_tool_outcome(outcome, &call.name, 0.0, false, content.len());
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

    /// TodoWrite short-circuit（架构 §4.4.6）。
    ///
    /// 不进 HITL / hooks / artifact 通路——直接：
    /// 1. 解析 input.todos
    /// 2. 落盘到 session.jsonl 的 `MetaUpdate { todos }`（持久化，重启可恢复）
    /// 3. emit `TodoListUpdated` 让 surface 更新右 sidebar
    /// 4. emit `ToolCallStarted/Finished` 让 transcript 一致
    fn spawn_todo_write(
        &self,
        call: ToolCall,
        call_index: usize,
        dispatch_index: usize,
    ) -> BoxFuture<'static, Result<(usize, ToolResult), ModelError>> {
        let state = self.state.clone();
        let sink = self.sink.clone();
        let data_dir = self.data_dir_for_artifacts.clone();
        let session_id = self.session_id_for_hooks.clone();

        let tool_span = tracing::info_span!(
            "tool.call",
            otel.name = "tool.TodoWrite",
            otel.kind = "internal",
            hebbian.tool.name = TODO_WRITE_TOOL_NAME,
            hebbian.tool.call_id = %call.id,
            hebbian.tool.class = "read_only",
            hebbian.tool.outcome = Empty,
        );

        Box::pin(
            async move {
                sink(state.event(EventPayload::ToolCallStarted {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                }));

                let parsed = match todo_write::parse_input(call.input.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        let msg = format!("TodoWrite 入参解析失败: {e}");
                        record_tool_outcome(attr::outcome::FAILED, &call.name, 0.0, false, 0);
                        sink(state.event(EventPayload::ToolCallFinished {
                            index: dispatch_index,
                            call_id: call.id.clone(),
                            result: msg.clone(),
                            duration_ms: 0,
                            truncated: false,
                            artifact_path: None,
                        }));
                        return Ok((
                            call_index,
                            ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content: msg,
                                artifact: None,
                            },
                        ));
                    }
                };
                let todos = todo_write::normalize(parsed.todos);
                let summary_text = todo_write::summary(&todos);

                // 落盘：data_dir + session_id 都有时持久化；单测路径（None）跳过。
                // 整列表覆盖语义——模型负责自己维护任务集；不在 dispatcher 层做累积合并。
                if let (Some(dd), Some(sid)) = (data_dir.as_deref(), session_id.as_deref()) {
                    if let Err(e) = session_store::set_todos(dd, sid, todos.clone()) {
                        warn!(error = %e, "TodoWrite: 持久化 todos 失败");
                    }
                }

                // 通知 surface：右 sidebar 更新
                sink(state.event(EventPayload::TodoListUpdated {
                    todos: todos.clone(),
                }));

                record_tool_outcome(
                    attr::outcome::OK,
                    &call.name,
                    0.0,
                    false,
                    summary_text.len(),
                );
                sink(state.event(EventPayload::ToolCallFinished {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    result: summary_text.clone(),
                    duration_ms: 0,
                    truncated: false,
                    artifact_path: None,
                }));

                Ok((
                    call_index,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: summary_text,
                        artifact: None,
                    },
                ))
            }
            .instrument(tool_span),
        )
    }

    /// ExitPlanMode short-circuit（架构 §4.4.5）。
    ///
    /// 流程：
    /// 1. 解析 input → 落 `plans/plan-<ts>.md` → 持久化 active_plan
    /// 2. emit `ToolCallStarted` + `PlanReady`
    /// 3. 开 HITL approval + emit `PermissionRequested { kind: Plan { ... } }`
    /// 4. 等用户 `ApprovalDecision`：
    ///    - `AllowOnce` / `AllowAndRemember` → 切回 pre_plan_mode（+ emit
    ///      `RunModeChanged`）；tool result = "[Plan approved] ..." + 未消费评论
    ///    - `Deny` → 留 PlanMode；result = "[Plan rejected]"
    ///    - `DenyWithFeedback { feedback }` → 留 PlanMode；result 含反馈让
    ///       模型按反馈改 plan
    /// 5. 不论批/拒，emit `ToolCallFinished` 让 transcript 一致
    fn spawn_exit_plan_mode(
        &self,
        call: ToolCall,
        call_index: usize,
        dispatch_index: usize,
    ) -> BoxFuture<'static, Result<(usize, ToolResult), ModelError>> {
        let state = self.state.clone();
        let sink = self.sink.clone();
        let hitl = self.hitl.clone();
        let data_dir = self.data_dir_for_artifacts.clone();
        let session_id = self.session_id_for_hooks.clone();

        let tool_span = tracing::info_span!(
            "tool.call",
            otel.name = "tool.ExitPlanMode",
            otel.kind = "internal",
            hebbian.tool.name = EXIT_PLAN_MODE_TOOL_NAME,
            hebbian.tool.call_id = %call.id,
            hebbian.tool.class = "needs_human_input",
            hebbian.tool.outcome = Empty,
        );

        Box::pin(
            async move {
                sink(state.event(EventPayload::ToolCallStarted {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                }));

                let parsed = match exit_plan_mode::parse_input(call.input.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        let msg = format!("ExitPlanMode 入参解析失败: {e}");
                        record_tool_outcome(attr::outcome::FAILED, &call.name, 0.0, false, 0);
                        sink(state.event(EventPayload::ToolCallFinished {
                            index: dispatch_index,
                            call_id: call.id.clone(),
                            result: msg.clone(),
                            duration_ms: 0,
                            truncated: false,
                            artifact_path: None,
                        }));
                        return Ok((
                            call_index,
                            ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content: msg,
                                artifact: None,
                            },
                        ));
                    }
                };

                let (dd, sid) = match (data_dir.as_deref(), session_id.as_deref()) {
                    (Some(d), Some(s)) => (d, s),
                    _ => {
                        let msg =
                            "ExitPlanMode 需要 data_dir + session_id 才能落盘 / 审批".to_string();
                        record_tool_outcome(attr::outcome::FAILED, &call.name, 0.0, false, 0);
                        sink(state.event(EventPayload::ToolCallFinished {
                            index: dispatch_index,
                            call_id: call.id.clone(),
                            result: msg.clone(),
                            duration_ms: 0,
                            truncated: false,
                            artifact_path: None,
                        }));
                        return Ok((
                            call_index,
                            ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content: msg,
                                artifact: None,
                            },
                        ));
                    }
                };

                // 落盘 plan markdown
                let plan_path = match plans::save_plan(dd, sid, &parsed.plan_markdown) {
                    Ok(p) => p,
                    Err(e) => {
                        let msg = format!("ExitPlanMode 落盘失败: {e}");
                        warn!(error = %e, "ExitPlanMode save_plan failed");
                        record_tool_outcome(attr::outcome::FAILED, &call.name, 0.0, false, 0);
                        sink(state.event(EventPayload::ToolCallFinished {
                            index: dispatch_index,
                            call_id: call.id.clone(),
                            result: msg.clone(),
                            duration_ms: 0,
                            truncated: false,
                            artifact_path: None,
                        }));
                        return Ok((
                            call_index,
                            ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content: msg,
                                artifact: None,
                            },
                        ));
                    }
                };
                let plan_id = exit_plan_mode::plan_id_from_path(&plan_path);
                let plan_path_str = plan_path.display().to_string();

                // 持久化 active_plan
                if let Err(e) = session_store::set_active_plan(dd, sid, Some(plan_path_str.clone()))
                {
                    warn!(error = %e, "ExitPlanMode set_active_plan failed");
                }

                sink(state.event(EventPayload::PlanReady {
                    plan_id: plan_id.clone(),
                    plan_path: plan_path_str.clone(),
                    plan_markdown: parsed.plan_markdown.clone(),
                    summary: parsed.summary.clone(),
                }));

                // 开 HITL approval；workspace 维度（hitl 学习指纹不持久化 plan 审批）。
                let (request_id, waiter) = hitl.open_approval(None, None);
                sink(state.event(EventPayload::PermissionRequested {
                    request_id: request_id.clone(),
                    kind: PermissionKind::Plan {
                        plan_id: plan_id.clone(),
                        plan_path: plan_path_str.clone(),
                        plan_markdown: parsed.plan_markdown.clone(),
                        summary: parsed.summary.clone(),
                        steps: Vec::new(),
                    },
                    summary: if parsed.summary.is_empty() {
                        "计划待审批".to_string()
                    } else {
                        parsed.summary.clone()
                    },
                    risk: RiskLevel::Low,
                }));

                let permission_span = tracing::info_span!(
                    "permission.check",
                    hebbian.permission.kind = "plan",
                    hebbian.permission.request_id = %request_id,
                    hebbian.permission.decision = Empty,
                );
                let decision = waiter
                    .instrument(permission_span.clone())
                    .await
                    .unwrap_or(ApprovalDecision::Deny);
                let decision_label = approval_decision_label(&decision);
                permission_span.record(attr::PERMISSION_DECISION, decision_label);

                sink(state.event(EventPayload::PermissionResolved {
                    request_id,
                    decision: decision.clone(),
                }));

                // 拼未消费 plan_comments（不论通过/拒绝都拼——拒绝路径让模型也看到用户评论）
                let mut content = String::new();
                let unconsumed =
                    plan_comments::list_unconsumed(dd, sid, &plan_id).unwrap_or_default();

                match decision {
                    ApprovalDecision::AllowOnce | ApprovalDecision::AllowAndRemember { .. } => {
                        // 切回 pre_plan_mode（从 session 当前快照里读）
                        if let Ok(s) = session_store::load(dd, sid) {
                            let target_mode = s
                                .pre_plan_mode
                                .unwrap_or(crate::run_mode::RunMode::AskBeforeEdits);
                            if let Err(e) = session_store::set_run_mode(dd, sid, target_mode) {
                                warn!(error = %e, "ExitPlanMode: set_run_mode 失败");
                            } else {
                                sink(state.event(EventPayload::RunModeChanged {
                                    from: format!("{:?}", crate::run_mode::RunMode::PlanMode),
                                    to: format!("{:?}", target_mode),
                                }));
                            }
                            // 清空 pre_plan_mode（已消费）
                            let _ = session_store::set_pre_plan_mode(dd, sid, None);
                        }
                        content.push_str("[Plan approved] Proceeding with implementation.\n\n");
                        content.push_str(&parsed.plan_markdown);
                    }
                    ApprovalDecision::Deny => {
                        content.push_str(
                            "[Plan rejected by user] Stay in PlanMode and revise the plan.",
                        );
                    }
                    ApprovalDecision::DenyWithFeedback { ref feedback } => {
                        content.push_str(
                            "[Plan rejected by user — please revise]\n\nUser feedback:\n",
                        );
                        content.push_str(feedback);
                    }
                }

                // 评论拼接 + mark consumed
                if !unconsumed.is_empty() {
                    content.push_str("\n\n<plan_comments>\n");
                    for c in &unconsumed {
                        content.push_str(&format!("- [{}] {}\n", c.anchor, c.body));
                    }
                    content.push_str("</plan_comments>");
                    let ids: Vec<String> = unconsumed.iter().map(|c| c.id.clone()).collect();
                    if let Err(e) = plan_comments::mark_consumed(dd, sid, &plan_id, ids) {
                        warn!(error = %e, "ExitPlanMode: mark_consumed failed");
                    }
                }

                record_tool_outcome(attr::outcome::OK, &call.name, 0.0, false, content.len());
                sink(state.event(EventPayload::ToolCallFinished {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    result: content.clone(),
                    duration_ms: 0,
                    truncated: false,
                    artifact_path: None,
                }));

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

    /// Task short-circuit（架构 §4.4.11）：把子 NestedRun 委托给 [`SubagentRunner`]。
    /// 流程：
    /// 1. 解析 input
    /// 2. 取 `subagent_ctx`（缺失 → 返回错误）
    /// 3. 构造 [`SubagentRunner`]（共享父 client / hitl / workspace / edits-worktree /
    ///    cancel；过滤父 ToolRegistry 后给子用；EventSink 装饰器填 `subagent_call_id`）
    /// 4. 跑一次 isolated 同步 NestedRun（P2 范围；inherit / background 留 P3 / P4）
    /// 5. 把子终态文本回灌父 transcript 作为 ToolResult
    fn spawn_task(
        &self,
        call: ToolCall,
        call_index: usize,
        dispatch_index: usize,
    ) -> BoxFuture<'static, Result<(usize, ToolResult), ModelError>> {
        let state = self.state.clone();
        let sink = self.sink.clone();
        let subagent_ctx = self.subagent_ctx.clone();
        let registry = self.registry.clone();
        let workspace = self.workspace.clone();
        let hitl = self.hitl.clone();
        let cancel = self.cancel.clone();
        let edits_worktree = self.edits_worktree.clone();
        let parent_model_id = self.model_id.clone();
        let parent_transcript_snapshot = self.parent_transcript_snapshot.clone();

        let tool_span = tracing::info_span!(
            "tool.call",
            otel.name = "tool.Task",
            otel.kind = "internal",
            hebbian.tool.name = crate::tools::task::TASK_TOOL_NAME,
            hebbian.tool.call_id = %call.id,
            hebbian.tool.class = "subagent",
            hebbian.tool.outcome = Empty,
        );

        Box::pin(
            async move {
                let start = Instant::now();
                sink(state.event(EventPayload::ToolCallStarted {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                }));

                let finish_with = |content: String, ok: bool| -> (usize, ToolResult) {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    record_tool_outcome(
                        if ok {
                            attr::outcome::OK
                        } else {
                            attr::outcome::FAILED
                        },
                        &call.name,
                        0.0,
                        false,
                        content.len(),
                    );
                    sink(state.event(EventPayload::ToolCallFinished {
                        index: dispatch_index,
                        call_id: call.id.clone(),
                        result: content.clone(),
                        duration_ms,
                        truncated: false,
                        artifact_path: None,
                    }));
                    (
                        call_index,
                        ToolResult {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            content,
                            artifact: None,
                        },
                    )
                };

                let parsed = match crate::tools::task::parse_input(call.input.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        return Ok(finish_with(format!("Task 入参解析失败: {e}"), false));
                    }
                };

                let ctx = match subagent_ctx.as_ref() {
                    Some(c) => c.clone(),
                    None => {
                        return Ok(finish_with(
                            "Task 工具不可用：当前会话未注入 subagent 上下文".to_string(),
                            false,
                        ));
                    }
                };

                let runner = crate::subagent::SubagentRunner {
                    ctx: ctx.clone(),
                    parent_registry: registry,
                    parent_sink: sink.clone(),
                    parent_workspace: workspace,
                    parent_hitl: hitl,
                    parent_cancel: cancel,
                    parent_edits_worktree: edits_worktree,
                    parent_run_id: state.run_id.clone(),
                    parent_model_id: parent_model_id.clone(),
                    parent_task_call_id: call.id.clone(),
                    parent_transcript_snapshot,
                };

                match runner.execute(parsed).await {
                    Ok(text) => Ok(finish_with(text, true)),
                    Err(e) => Ok(finish_with(format!("Task 执行失败: {e}"), false)),
                }
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

/// 同一批普通工具的最大并发度。超出的 future 在前面有空位时才开始 poll，
/// 避免模型一次返回大量 tool_call 时把 tokio worker / 文件句柄打满。
const MAX_PARALLEL_TOOLS: usize = 8;

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
    let decision_result = waiter.instrument(permission_span.clone()).await;
    let decision = decision_result.map_err(|_| "路径审批通道已关闭".to_string())?;
    let decision_label = approval_decision_label(&decision);
    permission_span.record(attr::PERMISSION_DECISION, decision_label);
    info!(
        request_id = %request_id,
        decision = decision_label,
        "permission.approval: backend waiter received path approval decision"
    );

    sink(state.event(EventPayload::PermissionResolved {
        request_id,
        decision: decision.clone(),
    }));
    match decision {
        ApprovalDecision::AllowOnce => Ok(()),
        ApprovalDecision::AllowAndRemember { scope, .. } => {
            for p in &paths {
                workspace.add_allowed_path(p.clone());
                // 持久化到 PermissionStore.paths 段（架构 §6.1.2）：Project / Global 落盘，
                // Session / Once 仅 workspace 内存生效。
                if let Some(store) = permission_store {
                    let _ = session_id; // session paths 不持久化（workspace 内存即可）
                    let workdir_buf;
                    let (scope_for_path, workdir_for_path) = match scope {
                        protocol::PermissionScope::Once | protocol::PermissionScope::Session => {
                            continue
                        }
                        protocol::PermissionScope::Project => {
                            workdir_buf = workspace.workdir().to_path_buf();
                            (scope, Some(workdir_buf.as_path()))
                        }
                        protocol::PermissionScope::Global => (scope, None),
                    };
                    if let Err(e) = store.add_path(scope_for_path, workdir_for_path, p.clone()) {
                        tracing::warn!(error = %e, path = %p.display(), "paths 持久化失败");
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
            let outcome_result = waiter.instrument(permission_span.clone()).await;
            let outcome = outcome_result.map_err(|_| "审批通道已关闭".to_string())?;
            let decision_label = approval_decision_label(&outcome);
            permission_span.record(attr::PERMISSION_DECISION, decision_label);
            info!(
                request_id = %request_id,
                decision = decision_label,
                "permission.approval: backend waiter received tool approval decision"
            );

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

fn record_tool_outcome(
    outcome: &str,
    tool: &str,
    _duration_ms: f64,
    truncated: bool,
    bytes: usize,
) {
    let span = tracing::Span::current();
    span.record(attr::TOOL_OUTCOME, outcome);
    span.record(attr::TOOL_TRUNCATED, truncated);
    span.record(attr::TOOL_RESULT_SIZE, bytes as i64);
    span.record(attr::TOOL_NAME, tool);
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
    use async_trait::async_trait;
    use common::AppResult;
    use model_gateway::types::ToolCall;
    use model_gateway::types::{ModelRequest, ModelResponse, ModelStreamEvent, Usage};
    use protocol::RunId;
    use serde_json::Value;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;

    struct StaticAllowJudge;

    #[async_trait]
    impl model_gateway::client::ModelClient for StaticAllowJudge {
        fn provider_id(&self) -> &str {
            "test"
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: common::CancelFlag,
        ) -> Result<ModelResponse, model_gateway::types::ModelError> {
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "ALLOW".to_string(),
                reasoning: String::new(),
                attachments: Vec::new(),
                usage: Usage::default(),
                reasoning_signature: String::new(),
            })
        }

        async fn stream(
            &self,
            req: ModelRequest,
            cancel: common::CancelFlag,
            _on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, model_gateway::types::ModelError> {
            self.complete(req, cancel).await
        }
    }

    struct DestructiveNoopTool;

    #[async_trait]
    impl crate::tools::Tool for DestructiveNoopTool {
        fn name(&self) -> &str {
            "Bash"
        }

        fn description(&self) -> &str {
            "test destructive tool"
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _input: Value) -> AppResult<String> {
            Ok("executed".to_string())
        }
    }

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
            crate::tools::background::BgTaskRegistry::new(),
            None,
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

            subagent_ctx: None,
            parent_transcript_snapshot: None,
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn automode_allows_real_opus_model_id_without_human_resolution() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(DestructiveNoopTool) as Box<dyn crate::tools::Tool>
        ]));
        let hitl = Arc::new(crate::tools::hitl::HitlGate::default());
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let sink: crate::agent_loop::EventSink = Arc::new(move |event| {
            let _ = tx.try_send(event);
        });
        let dispatcher = ToolDispatcher {
            registry,
            hitl,
            workspace,
            state: run_state,
            sink,
            cancel: Arc::new(AtomicBool::new(false)),
            run_mode: crate::run_mode::RunMode::AutoMode,
            model_id: Some("claude-opus-4.7".to_string()),
            judge_client: Some(Arc::new(StaticAllowJudge)),
            force_automode: false,
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
        };

        let call = ToolCall {
            id: "call_automode".into(),
            name: "Bash".into(),
            input: serde_json::json!({ "command": "touch automode-ok" }),
        };

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("AutoMode should resolve approval without a human response")
            .expect("dispatch should succeed");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "executed");

        let mut saw_allow = false;
        while let Ok(event) = rx.try_recv() {
            if let EventPayload::PermissionAutoJudged { decision, .. } = event.payload {
                saw_allow = decision == "allow";
            }
        }
        assert!(saw_allow, "AutoMode judge should allow the supported model");
    }

    /// AutoMode 下模型不在白名单（data_dir=None → 默认白名单 opus-4-7/4-8/gpt-5.5，
    /// 这里用 sonnet-4-6 故意落空）：dispatcher 应 emit Notice(warn) 提示转手动审批，
    /// 且**不调判官**（无 PermissionAutoJudged），保留 NeedsApproval 走人工。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn automode_unsupported_model_emits_notice_and_falls_back_to_manual() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(DestructiveNoopTool) as Box<dyn crate::tools::Tool>
        ]));
        let hitl = Arc::new(crate::tools::hitl::HitlGate::default());
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let sink: crate::agent_loop::EventSink = Arc::new(move |event| {
            let _ = tx.try_send(event);
        });
        let hitl_for_resolve = hitl.clone();
        let dispatcher = ToolDispatcher {
            registry,
            hitl,
            workspace,
            state: run_state,
            sink,
            cancel: Arc::new(AtomicBool::new(false)),
            run_mode: crate::run_mode::RunMode::AutoMode,
            model_id: Some("claude-sonnet-4-6".to_string()), // 不在默认白名单
            // judge 给 StaticAllowJudge：若错误地调用了它，命令会被自动放行——
            // 用它来反证"不该调判官"（断言看到的是 NeedsApproval 而非 auto allow）。
            judge_client: Some(Arc::new(StaticAllowJudge)),
            force_automode: false,
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
        };

        let call = ToolCall {
            id: "call_unsupported".into(),
            name: "Bash".into(),
            input: serde_json::json!({ "command": "touch fallback-ok" }),
        };

        // 后台 surface：收到 NeedsApproval 就 deny（验证确实走了人工审批闸口）。
        let surface = tokio::spawn(async move {
            let mut saw_notice = false;
            let mut saw_auto_judged = false;
            while let Some(event) = rx.recv().await {
                match &event.payload {
                    EventPayload::Notice {
                        level, dedup_key, ..
                    } => {
                        saw_notice = matches!(level, protocol::LogLevel::Warn)
                            && dedup_key.as_deref()
                                == Some("automode-unsupported:claude-sonnet-4-6");
                    }
                    EventPayload::PermissionAutoJudged { .. } => saw_auto_judged = true,
                    EventPayload::PermissionRequested { request_id, .. } => {
                        hitl_for_resolve.resolve(request_id, ApprovalDecision::Deny);
                    }
                    _ => {}
                }
            }
            (saw_notice, saw_auto_judged)
        });

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("dispatch should complete")
            .expect("dispatch should succeed");
        // 人工审批被 deny → 工具不执行
        assert_eq!(result.len(), 1);
        assert_ne!(
            result[0].content, "executed",
            "应走人工审批且被拒，不该自动执行"
        );

        drop(dispatcher);
        let (saw_notice, saw_auto_judged) = surface.await.unwrap();
        assert!(saw_notice, "应 emit Notice(warn, dedup_key) 提示转手动审批");
        assert!(
            !saw_auto_judged,
            "不在白名单的模型不该调判官（无 PermissionAutoJudged）"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remember_first_compound_bash_auto_resolves_matching_pending_call() {
        use protocol::{EventPayload, PermissionKind, PermissionScope};

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let registry = Arc::new(ToolRegistry::new(vec![Box::new(BashTool::new(
            workspace.clone(),
            crate::tools::background::BgTaskRegistry::new(),
            None,
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

            subagent_ctx: None,
            parent_transcript_snapshot: None,
        };

        let calls = vec![
            ToolCall {
                id: "call_1".into(),
                name: "Bash".into(),
                input: serde_json::json!({
                    "command": "cd crates && cd agent-core && grep dispatch Cargo.toml | cat",
                    "cwd": tmp.path()
                }),
            },
            ToolCall {
                id: "call_2".into(),
                name: "Bash".into(),
                input: serde_json::json!({
                    "command": "cd crates && cd agent-core && grep package Cargo.toml | cat",
                    "cwd": tmp.path()
                }),
            },
        ];

        let hitl_for_surface = hitl.clone();
        let surface = tokio::spawn(async move {
            let mut requests = Vec::new();
            while let Some(event) = rx.recv().await {
                if let EventPayload::PermissionRequested {
                    request_id, kind, ..
                } = &event.payload
                {
                    if let PermissionKind::ToolCall {
                        command_segments, ..
                    } = kind
                    {
                        requests.push(command_segments.clone());
                    }
                    if requests.len() == 1 {
                        hitl_for_surface.resolve(
                            request_id,
                            ApprovalDecision::AllowAndRemember {
                                scope: PermissionScope::Session,
                                pattern: Some("cd".into()),
                                extra_patterns: vec!["grep".into(), "cat".into()],
                            },
                        );
                    } else if requests.len() > 1 {
                        panic!("second matching Bash approval should be auto-resolved");
                    }
                }
            }
            requests
        });

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&calls, 0))
            .await
            .expect("dispatch should complete after first approval");
        let results = result.expect("dispatch should not return errors");
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.name == "Bash"));

        drop(dispatcher);
        let requests = surface.await.unwrap();
        assert_eq!(requests.len(), 1);
        // command_segments 只含「会写可记忆」段：grep / cat 是只读段，已被过滤
        // （架构 §4.4.2）——UI 记忆勾选区不该出现它们。
        assert_eq!(requests[0], vec!["cd crates", "cd agent-core"]);
    }

    /// 回归测试：spawn_todo_write short-circuit 真的把 todos 落盘到 jsonl 的
    /// meta_update 行——这是"任务完成后右侧 sidebar 消失" bug 的根因区域，
    /// 用集成测试钉住。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn todo_write_short_circuit_persists_to_jsonl() {
        use crate::storage::sessions;
        use protocol::todo::TodoStatus;

        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        // 先建一个 session 让 jsonl 文件存在
        let session =
            sessions::create(&data_dir, "openai".into(), "gpt-x".into(), None, None).unwrap();
        let session_id = session.id.clone();

        let workspace = Workspace::new(&data_dir, Vec::new());
        let registry =
            Arc::new(ToolRegistry::new(vec![
                Box::new(crate::tools::todo_write::TodoWriteTool) as Box<dyn crate::tools::Tool>,
            ]));
        let hitl = Arc::new(crate::tools::hitl::HitlGate::default());
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let sink: crate::agent_loop::EventSink = Arc::new(move |event| {
            let _ = tx.try_send(event);
        });
        let dispatcher = ToolDispatcher {
            registry,
            hitl,
            workspace,
            state: run_state,
            sink,
            cancel: Arc::new(AtomicBool::new(false)),
            run_mode: crate::run_mode::RunMode::AskBeforeEdits,
            model_id: None,
            judge_client: None,
            force_automode: false,
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            // 关键：传 data_dir + session_id，让 short-circuit 走落盘分支
            session_id_for_hooks: Some(session_id.clone()),
            data_dir_for_artifacts: Some(data_dir.clone()),
            permission_store: None,
            edits_worktree: None,

            subagent_ctx: None,
            parent_transcript_snapshot: None,
        };

        let call = ToolCall {
            id: "call_1".into(),
            name: "TodoWrite".into(),
            input: serde_json::json!({
                "todos": [
                    { "id": "t1", "content": "写代码", "activeForm": "正在写代码", "status": "in_progress" },
                    { "id": "t2", "content": "跑测试", "activeForm": "正在跑测试", "status": "pending" }
                ]
            }),
        };

        // 收集事件做断言
        let collector = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(e) = rx.recv().await {
                events.push(e);
            }
            events
        });

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("dispatch 应在 5s 内完成");
        let results = result.expect("dispatch 不应返回错误");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "TodoWrite");

        // 断言 1：事件流里包含 TodoListUpdated
        drop(dispatcher);
        let events = collector.await.unwrap();
        let has_todo_event = events.iter().any(
            |e| matches!(&e.payload, EventPayload::TodoListUpdated { todos } if todos.len() == 2),
        );
        assert!(
            has_todo_event,
            "应有 TodoListUpdated 事件；实际事件: {:?}",
            events
                .iter()
                .map(|e| std::mem::discriminant(&e.payload))
                .collect::<Vec<_>>()
        );

        // 断言 2：jsonl 文件含 meta_update 行 + todos 字段
        let path = crate::storage::sessions_dir::session_jsonl_path(&data_dir, &session_id);
        let content = std::fs::read_to_string(&path).unwrap();
        let meta_update_lines: Vec<&str> = content
            .lines()
            .filter(|l| l.contains("\"type\":\"meta_update\"") && l.contains("todos"))
            .collect();
        assert!(
            !meta_update_lines.is_empty(),
            "session.jsonl 必须有 meta_update todos 行；实际内容:\n{content}"
        );

        // 断言 3：load 出来的 Session.todos 含 2 条
        let loaded = sessions::load(&data_dir, &session_id).unwrap();
        assert_eq!(loaded.todos.len(), 2);
        assert_eq!(loaded.todos[0].status, TodoStatus::InProgress);
        assert_eq!(loaded.todos[1].status, TodoStatus::Pending);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skill_tool_returns_full_large_content_without_artifact() {
        use crate::tools::skill::{Skill, SkillSource, SkillTool};

        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        let skills_dir = tmp.path().join("skills").join("big");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let large_body = "S".repeat(MAX_TOOL_RESULT_INLINE + 1);
        let skill_path = skills_dir.join("SKILL.md");
        std::fs::write(
            &skill_path,
            format!("---\nname: big\ndescription: Big skill\n---\n{large_body}"),
        )
        .unwrap();

        let workspace = Workspace::new(tmp.path(), Vec::new());
        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(SkillTool::new(vec![Skill {
                name: "big".to_string(),
                alias: None,
                description: "Big skill".to_string(),
                path: skill_path,
                source: SkillSource::Global,
                enabled: true,
                collection_id: None,
            }])) as Box<dyn crate::tools::Tool>,
        ]));
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let sink: crate::agent_loop::EventSink = Arc::new(move |event| {
            let _ = tx.try_send(event);
        });
        let dispatcher = ToolDispatcher {
            registry,
            hitl: Arc::new(crate::tools::hitl::HitlGate::default()),
            workspace,
            state: run_state,
            sink,
            cancel: Arc::new(AtomicBool::new(false)),
            run_mode: crate::run_mode::RunMode::AskBeforeEdits,
            model_id: None,
            judge_client: None,
            force_automode: false,
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: Some("sid-skill".to_string()),
            data_dir_for_artifacts: Some(data_dir.clone()),
            permission_store: None,
            edits_worktree: None,

            subagent_ctx: None,
            parent_transcript_snapshot: None,
        };

        let call = ToolCall {
            id: "call_skill".into(),
            name: "Skill".into(),
            input: serde_json::json!({ "skill": "big" }),
        };

        let results = dispatcher.run_calls(&[call], 0).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Skill");
        assert!(results[0].artifact.is_none());
        assert!(results[0].content.ends_with(&large_body));
        assert!(!results[0].content.contains("已落盘到"));
        assert!(!data_dir
            .join("sessions/sid-skill/tool_results/call_skill.txt")
            .exists());

        let mut finished = None;
        while let Ok(event) = rx.try_recv() {
            if let EventPayload::ToolCallFinished {
                result,
                truncated,
                artifact_path,
                ..
            } = event.payload
            {
                finished = Some((result, truncated, artifact_path));
            }
        }
        let (event_result, truncated, artifact_path) = finished.expect("Skill should finish");
        assert!(!truncated);
        assert_eq!(artifact_path, None);
        assert!(event_result.ends_with(&large_body));
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
