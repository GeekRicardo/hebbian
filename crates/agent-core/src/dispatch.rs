//! 工具派发器：把一组 tool call 跑出 `Vec<ToolResult>`。
//!
//! 接管以下职责（让 [`agent_loop`] 不必再 inline 200 行 closure）：
//! - 路径越界审批 → emit `PermissionRequested { kind: PathAccess }` + await
//! - 工具审批 → emit `PermissionRequested { kind: ToolCall }` + await
//! - 提问通路（`ask` 工具）→ emit `UserQuestionRequested` + await
//! - 工具执行（含超时输出截断）+ emit `ToolCallStarted` / `ToolCallFinished`
//!
//! 并发策略（架构 §4.4.3）：所有工具（含 Bash/PowerShell）统一进并发池，同批最多
//! [`MAX_PARALLEL_TOOLS`] 个同时 poll（避免上百个 tool_call 打满 worker / 句柄）。
//! Bash 命令都是独立 `bash -lc` 子进程，cwd 天然隔离；审批记忆由 HITL 的
//! `resolve_matching_pending_after_remember` 保障（AllowAndRemember 落盘后自动
//! 遍历其他 pending 审批、匹配新规则的补发 AllowOnce）。同一文件的 Edit 由
//! edits-worktree per-path 锁（架构 §4.13.4）天然串行，无需派发器额外处理。
//!
//! [`agent_loop`]: super::agent_loop

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::BoxFuture;
use futures_util::stream::{self, StreamExt};
use observability::attr;
use protocol::{
    ApprovalDecision, AskQuestion, EventPayload, PermissionKind, PermissionRequestId,
    QuestionOption, RiskLevel, UserAnswer,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;
use tracing::{field::Empty, info, warn, Instrument};

use crate::{
    agent_loop::EventSink,
    automode::AutoModeDecision,
    edits::EditsWorktree,
    effects::{analyze_effects, EffectClass, Effects},
    model_io_dump::{self, DumpEntry, ModelIoDump},
    permissions::PermissionStore,
    run_state::RunState,
    storage::{plan_comments, plans, sessions as session_store},
    tools::{
        hitl::{HitlGate, PermissionDecision},
        plan_mode::{self, PLAN_MODE_TOOL_NAME},
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

/// `ask` 工具的输入。两种形态二选一：
///
/// - **单题**：填 `question / options / multi`
/// - **多题**：填 `questions`（数组），此时三个老字段被忽略
#[derive(Debug, Deserialize)]
struct AskInput {
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    options: Vec<QuestionOption>,
    /// 是否允许多选；缺省 false（单选）。仅单题模式生效。
    #[serde(default)]
    multi: bool,
    /// 多题模式：每道子题独立 title / description / options / multi。
    #[serde(default)]
    questions: Vec<AskQuestion>,
}

/// 解析后的 ask 形态。
enum AskShape {
    Single {
        question: String,
        options: Vec<QuestionOption>,
        multi: bool,
    },
    Multi(Vec<AskQuestion>),
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
    /// 共享引用：agent_loop / harness 各持一份，SwitchRunMode 实时更新。
    pub run_mode: Arc<std::sync::Mutex<crate::run_mode::RunMode>>,
    /// 当前会话使用的模型 id。provider 未配置专属 judge 模型时，AutoMode 判官
    /// 复用它（架构 §4.4.4 判官模型选择）。
    pub model_id: Option<String>,
    /// AutoMode judge 兜底的 ModelClient（通常 = 主 client）。`None` 时降级 Ask。
    /// 会话 provider 配置了专属 judge 模型时，dispatch 会另建专属 client 替换它。
    pub judge_client: Option<std::sync::Arc<dyn model_gateway::client::ModelClient>>,
    /// `force_automode`（hands-off「全自动」）子开关（架构 §4.4.4）。仅 [`RunMode::AutoMode`]
    /// 下生效：判官返回 `Ask` 时折叠成 `Deny`、命令类 `Deny` 也自动拒不弹审批。
    /// **共享句柄**：surface 的 `set_force_automode` 改它后，run 中途下一个工具调用即生效
    /// （由 [`crate::run_mode::LiveForceAutomodeRegistry`] 管理）。
    pub force_automode: crate::run_mode::SharedForceAutomode,
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
    /// Edit/Bash 文件快照所属 Run（架构 §4.13）。整个 agent_loop 共享一个 run_id。
    pub current_run_id: Option<String>,
    /// Subagent / NestedRun 上下文（架构 §4.4.11）。`None` 表示当前进程不支持
    /// subagent 调度（单测路径 / 没有可用 subagent 定义时），spawn_task 直接拒绝。
    pub subagent_ctx: Option<Arc<crate::subagent::SubagentCtx>>,
    /// 父 Transcript 在「本轮 assistant tool_calls push 之前」的 entries 快照（架构 §4.4.11.3 inherit）。
    /// 仅当本轮 `calls` 含 `Task` 工具时由 agent_loop 抓取；否则 `None` 跳过克隆。
    /// `Arc` 共享给同 ToolStep 内所有 Task（parallel 启动看到同一形态）。
    pub parent_transcript_snapshot: Option<Arc<Vec<TranscriptEntry>>>,
    /// 模型 IO dump 句柄。AutoMode 判官请求记入 model_io.jsonl（`kind: "judge"`）。
    pub model_io_dump: Option<ModelIoDump>,
    /// 子 NestedRun 的 `permission=Bypass`（架构 §4.4.11.4）：子在 tools 白名单内自主放行、
    /// 不弹审批，仅危险红线（dangerous pattern）仍走 hitl。父 dispatcher 恒 `false`。
    pub subagent_bypass: bool,
}

/// 文件编辑免审判定（架构 §4.4.3 Default / Yolo 模式）。
///
/// `Edit`/`Write` 写「工作区内、非 git 元数据」的文件直接放行——edits-worktree 在执行前
/// 拍 before 快照，整个 Run 的界内写入都能一键回退，免审是安全的。三类不可还原写入不在
/// 此列，仍按原审批强度：
/// - 界外文件（`paths_in_bounds == false` → 已走 PathAccess 审批）
/// - git 元数据（改 `.git/hooks` 后下次 git 操作即执行，worktree 兜不住 → 走工具审批）
/// - 命令副作用（Bash/PowerShell 不是文件编辑，直接 false）
///
/// `Default` / `AutoMode` / `Yolo` 同档命中：界内编辑能被整 Run 回退兜底，三种模式都
/// 免审。AutoMode 的判官价值聚焦在不可回退的 Bash 命令副作用与越界路径，对能一键回退
/// 的界内编辑不必每次多耗一次 LLM 调用。Yolo 的命令/越界红线在审批主分支单独处理。
///
/// 抽成纯函数让生产派发路径与历史重放测试共用同一份判定，杜绝复现偏差。
pub(crate) fn edit_auto_allowed(
    tool_name: &str,
    run_mode: crate::run_mode::RunMode,
    paths_in_bounds: bool,
    touches_git_meta: bool,
) -> bool {
    matches!(tool_name, "Edit" | "Write")
        && matches!(
            run_mode,
            crate::run_mode::RunMode::Default
                | crate::run_mode::RunMode::AutoMode
                | crate::run_mode::RunMode::Yolo
        )
        && paths_in_bounds
        && !touches_git_meta
}

/// Bash/PowerShell「安全会写」自动放行（架构 §4.4.2.3 safe 档）：命令的每个会写段都是纯创建
/// 命令（mkdir/touch/mkfifo，[`SegmentEffect::safe_write`]），且界内、非危险复合 → 连首次都
/// 免审/免判官。与 [`edit_auto_allowed`] 同源：创建目标已采进 `effects.paths` 由路径闸兜越界
/// （`paths_in_bounds`），这里只管「命令本身不跑代码、可回退」。任一会写段既非只读也非
/// safe_write（含 rm 等不可记忆段）、或越界、或危险复合 → 不放行，走原审批链。mode 无关
/// （纯创建始终安全，与 ReadOnly 同档）；PlanMode 已在更上层过滤掉 Bash。
pub(crate) fn bash_safe_write_allowed(
    tool_name: &str,
    effects: &Effects,
    paths_in_bounds: bool,
) -> bool {
    matches!(tool_name, "Bash" | "PowerShell")
        && paths_in_bounds
        && !effects.has_dangerous_pattern()
        && !effects.segments.is_empty()
        && effects.segments.iter().any(|s| s.safe_write)
        && effects
            .segments
            .iter()
            .all(|s| s.is_readonly || s.safe_write)
}

/// Yolo 模式（架构 §4.4.3）的工具决策：界内 + 非危险 → 放行；命中红线 → 自动拒。
///
/// Yolo 是无人值守模式，红线**不弹审批等人**（现场没人点批准），而是直接拒 + reason
/// 回灌 agent。红线 = 危险复合模式（§4.4.2.2）∪ 写界外路径 ∪ 触 git 元数据。返回
/// `None` 表示当前不是 Yolo（调用方走原有审批链）。
///
/// 与 `edit_auto_allowed` 互补：界内 Edit/Write 已被前者放行，本函数兜命令类与一切红线。
fn yolo_decision(
    run_mode: crate::run_mode::RunMode,
    has_dangerous_pattern: bool,
    paths_in_bounds: bool,
    touches_git_meta: bool,
    dangerous_kinds: &[String],
) -> Option<PermissionDecision> {
    if run_mode != crate::run_mode::RunMode::Yolo {
        return None;
    }
    if !paths_in_bounds {
        return Some(PermissionDecision::Denied {
            reason: "全速模式拦截：该操作要写工作区外的路径，无人值守下不放行。\
                     如需访问，请把目标目录加入项目允许路径，或用 --workdir 纳入工作区。"
                .to_string(),
        });
    }
    if touches_git_meta {
        return Some(PermissionDecision::Denied {
            reason: "全速模式拦截：该操作要改 git 元数据（.git/hooks、.git/config 等），\
                     不可逆且 worktree 兜不住，无人值守下不放行。"
                .to_string(),
        });
    }
    if has_dangerous_pattern {
        let kinds = if dangerous_kinds.is_empty() {
            "危险操作".to_string()
        } else {
            dangerous_kinds.join(", ")
        };
        return Some(PermissionDecision::Denied {
            reason: format!(
                "全速模式拦截不可逆 / 跨界操作（{kinds}）。换一个工作区内的安全做法。"
            ),
        });
    }
    Some(PermissionDecision::Approved)
}

impl ToolDispatcher {
    /// 派发整组 tool call。返回按 call_index 排序的 ToolResult。
    /// `dispatch_offset` 是这一轮在整个 run 内的全局起始 index。
    // 并发执行（架构 §4.4.3）：所有工具（含 Bash/PowerShell）统一进并发池。
    // 每个 Bash 命令是独立 `bash -lc` 子进程，cwd 天然隔离，不存在"共享 cwd"冲突。
    // 审批记忆：HITL 的 `resolve_matching_pending_after_remember` 在 AllowAndRemember
    // 落盘后自动遍历其他 pending 审批、匹配新规则的补发 AllowOnce——同批并发 shell
    // 不会重复弹审批。
    pub async fn run_calls(
        &self,
        calls: &[ToolCall],
        dispatch_offset: usize,
    ) -> Result<Vec<ToolResult>, ModelError> {
        let mut futures: Vec<BoxFuture<'static, Result<(usize, ToolResult), ModelError>>> =
            Vec::with_capacity(calls.len());

        for (call_index, call) in calls.iter().enumerate() {
            if cancellation::is_cancelled(&self.cancel) {
                self.hitl.cancel_all_pending();
                break;
            }
            let dispatch_index = dispatch_offset + call_index;
            let task = if call.name == ASK_TOOL_NAME {
                self.spawn_ask(call.clone(), call_index, dispatch_index)
            } else if call.name == TODO_WRITE_TOOL_NAME {
                self.spawn_todo_write(call.clone(), call_index, dispatch_index)
            } else if call.name == PLAN_MODE_TOOL_NAME {
                self.spawn_plan_mode(call.clone(), call_index, dispatch_index)
            } else if call.name == crate::tools::task::TASK_TOOL_NAME {
                self.spawn_task(call.clone(), call_index, dispatch_index)
            } else {
                self.spawn_tool(call.clone(), call_index, dispatch_index)
            };
            futures.push(task);
        }

        // 最多 MAX_PARALLEL_TOOLS 个同时 poll；单个工具报错不 cancel 同批其他。
        // Edit 同文件由 edits-worktree per-path 锁串行，不同文件并发；Bash 独立
        // 子进程无需额外串行化。
        let mut out: Vec<(usize, ToolResult)> = Vec::new();
        let mut stream = stream::iter(futures).buffer_unordered(MAX_PARALLEL_TOOLS);
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
            None => {
                out.sort_by_key(|(index, _)| *index);
                Ok(out.into_iter().map(|(_, r)| r).collect())
            }
        }
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
        if matches!(call.name.as_str(), "Bash" | "PowerShell") {
            // Bash 写/删目标的相对路径按 **Bash cwd** 解析，再交路径闸把关——否则
            // `Workspace::allows` 用 `canonicalize_lossy` 按派发器进程 CWD 解析，会把工作区内的
            // `mkdir build` / `touch a.txt` / `echo > out` / `rm x` 误判越界（架构 §4.4.2）。
            // effects 层是纯静态分析（不知 cwd），解析只能在 dispatch 持 Workspace 上下文处做。
            let cwd_raw = self
                .workspace
                .resolve_cwd(call.input.get("cwd").and_then(|v| v.as_str()));
            let cwd = if cwd_raw.is_relative() {
                self.workspace.workdir().join(&cwd_raw)
            } else {
                cwd_raw
            };
            for p in effects.paths.iter_mut() {
                if p.is_relative() {
                    *p = cwd.join(&*p);
                }
            }
            if effects.paths.is_empty() {
                effects.paths.push(cwd);
            }
        }

        // cd-git-compound 误报消解（架构 §4.4.2.2）：`cd <在工作区内的目录>; git commit` 是
        // agent 在自己工作区里的常规操作（项目强制的 heredoc commit 形态就长这样），cd 目标
        // 已受信，不存在「cd 到不可信目录跑其 .git/hooks」风险——移除该 dangerous_kind 让命令
        // 落回正常 allow 匹配（命中用户已存的 Bash(git commit) 规则即放行，不再强制审批/判官）。
        // cd 目标有任一在工作区外则保留危险判定（真有越界跑 hooks 风险）。
        if matches!(call.name.as_str(), "Bash" | "PowerShell")
            && effects.dangerous_kinds.iter().any(|k| k == "cd-git-compound")
        {
            if let Some(cmd_str) = call.input.get("command").and_then(|v| v.as_str()) {
                if let Ok(parsed) = crate::tools::shell_parse::parse(cmd_str) {
                    let targets = crate::tools::shell_parse::cd_targets(&parsed.commands);
                    let all_in_ws = !targets.is_empty()
                        && targets.iter().all(|t| {
                            let p = if std::path::Path::new(t).is_absolute() {
                                std::path::PathBuf::from(t)
                            } else {
                                self.workspace.workdir().join(t)
                            };
                            self.workspace.allows(&p)
                        });
                    if all_in_ws {
                        effects.dangerous_kinds.retain(|k| k != "cd-git-compound");
                    }
                }
            }
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
            // 只读工具读 data_dir（~/.hebbian/）整树免 PathAccess：跨 session 的
            // jsonl / model_io、logs/ 是 agent 自查自己历史的主战场，每次弹框反成噪音。
            // 严格限定 ReadOnly class——写工具（Edit/Bash 重定向）改 providers.json 等
            // 明文凭证仍走审批，避免一次 Read 之外的写入泄漏 / 篡改 key。
            let read_only_data_dir_allowed = matches!(effects.class, EffectClass::ReadOnly)
                && self
                    .data_dir_for_artifacts
                    .as_ref()
                    .map_or(false, |dd| p.starts_with(dd));
            if read_only_data_dir_allowed {
                info!(
                    tool = %call.name,
                    call_id = %call.id,
                    path = %p.display(),
                    matched = true,
                    level = "data_dir_readonly",
                    "[Permission:Path] read-only access to data dir allowed"
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

        let current_run_mode = *self.run_mode.lock().unwrap();
        let is_yolo = current_run_mode == crate::run_mode::RunMode::Yolo;
        let paths_in_bounds = out_of_scope.is_empty();

        // path_pending：越界路径要走 PathAccess 审批闸口。Yolo 模式（无人值守）下越界由
        // yolo_decision 直接自动拒，**不能**调 request_path_approval——那会 emit
        // PermissionRequested 弹审批，无人在场会永久挂起（架构 §4.4.3）。
        let path_pending = if paths_in_bounds {
            info!(
                tool = %call.name,
                call_id = %call.id,
                total = effects.paths.len(),
                "[Permission:Path] all paths in bounds"
            );
            None
        } else if is_yolo {
            info!(
                tool = %call.name,
                call_id = %call.id,
                out_of_scope = out_of_scope
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", "),
                result = "yolo_auto_deny",
                "[Permission:Path] out of bounds under yolo, auto-deny (no prompt)"
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
            Some(self.request_path_approval(&call.name, &call.id, out_of_scope))
        };

        // 文件编辑免审判定（架构 §4.4.3 Default / Yolo 模式）：见 [`edit_auto_allowed`]。
        let touches_git_meta = effects
            .paths
            .iter()
            .any(|p| crate::tools::shell_parse::is_git_meta_path(&p.to_string_lossy()));

        // Yolo 模式（架构 §4.4.3）：无人值守，红线自动拒不弹审批。界内非危险 → 放行；
        // 越界 / git-meta / 危险复合模式 → Denied + reason 回灌 agent（不挂起）。
        let yolo = yolo_decision(
            current_run_mode,
            effects.has_dangerous_pattern(),
            paths_in_bounds,
            touches_git_meta,
            &effects.dangerous_kinds,
        );

        let edit_allowed = edit_auto_allowed(
            &call.name,
            current_run_mode,
            paths_in_bounds,
            touches_git_meta,
        );
        let bash_safe_write = bash_safe_write_allowed(&call.name, &effects, paths_in_bounds);

        // 工具审批
        let permission = if let Some(decision) = yolo {
            match &decision {
                PermissionDecision::Approved => info!(
                    tool = %call.name,
                    call_id = %call.id,
                    "[Permission:ToolCall] yolo auto-allowed (界内非危险，无人值守)"
                ),
                PermissionDecision::Denied { reason } => info!(
                    tool = %call.name,
                    call_id = %call.id,
                    %reason,
                    "[Permission:ToolCall] yolo redline auto-denied"
                ),
                PermissionDecision::NeedsApproval { .. } => {}
            }
            decision
        } else if edit_allowed {
            info!(
                tool = %call.name,
                call_id = %call.id,
                "[Permission:ToolCall] in-workspace file edit auto-allowed (worktree-backed)"
            );
            PermissionDecision::Approved
        } else if bash_safe_write {
            info!(
                tool = %call.name,
                call_id = %call.id,
                "[Permission:ToolCall] in-workspace safe-write auto-allowed (mkdir/touch/mkfifo，路径闸已兜越界)"
            );
            PermissionDecision::Approved
        } else if self.subagent_bypass && !effects.has_dangerous_pattern() {
            // 子 NestedRun permission=Bypass（架构 §4.4.11.4）：父调 Task + tools 白名单
            // 即整体授权，子在白名单内自主跑、不弹审批打断用户。危险红线（rm -rf / 覆盖
            // 重定向等）走下面 else，仍交父 hitl，免审 ≠ 免红线。
            info!(
                tool = %call.name,
                call_id = %call.id,
                "[Permission:ToolCall] subagent bypass auto-approved (白名单内自主，非危险红线)"
            );
            PermissionDecision::Approved
        } else {
            self.hitl.check(&call.name, &effects)
        };
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
                // 这条审批是否会被 AutoMode judge 接管：与下方 async 块的判定一致
                // （RunMode=AutoMode + judge 可用）。true 时 surface 不弹框，
                // 等 judge 异步出结果，避免「弹一下又消失」的闪现（架构 §4.4.4）。
                let auto_handled = self.automode_will_handle();
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
                    auto_handled,
                    call_id: call.id.clone(),
                });
            }
        }

        let state = self.state.clone();
        let sink = self.sink.clone();
        let cancel = self.cancel.clone();
        let workspace = self.workspace.clone();
        let run_mode = self.run_mode.clone();
        let judge_client = self.judge_client.clone();
        let model_id_for_judge = self.model_id.clone();
        // 实时读 force_automode：run 中途用户切「全自动」开关，下一个工具调用即生效。
        let force_automode = self
            .force_automode
            .load(std::sync::atomic::Ordering::Relaxed);
        let effects_for_judge = effects.clone();
        let hitl_for_future = self.hitl.clone();
        let call_name_for_judge = call.name.clone();
        let call_input_for_judge = call.input.clone();
        let hooks_for_future = self.hooks.clone();
        let session_id_for_hooks = self.session_id_for_hooks.clone();
        let data_dir_for_artifacts = self.data_dir_for_artifacts.clone();
        let permission_store = self.permission_store.clone();
        let edits_worktree_for_snapshot = self.edits_worktree.clone();
        let current_run_id_for_snapshot = self.current_run_id.clone();
        let workspace_for_snapshot = self.workspace.clone();
        let model_io_dump_for_judge = self.model_io_dump.clone();
        // AutoMode judge 的 recent_transcript：用父 transcript 快照推断用户意图。
        // 缺失（None）时退化为空——但 agent_loop 现已无条件填充（架构 §4.4.4）。
        let transcript_for_judge = self.parent_transcript_snapshot.clone();

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
                // 实时读取当前 mode：用户在 agent_loop 运行中切 mode 时下一轮 dispatch
                // 立即生效。Default 的界内编辑免审已在同步段（dispatch 入口）处理，这里
                // 只剩 AutoMode 的 judge 短路。
                let current_run_mode = *run_mode.lock().unwrap();

                // AutoMode 判官模型选择（架构 §4.4.4）：会话 provider 配置了专属 judge
                // 模型则建专属 client，否则回退主 client + 主模型。每次审批时解析，
                // 设置里改了即时生效（免重启）。
                let judge_override: Option<(
                    Arc<dyn model_gateway::client::ModelClient>,
                    String,
                )> = if current_run_mode == crate::run_mode::RunMode::AutoMode {
                    judge_client.as_ref().map(|jc| {
                        resolve_judge_for_call(
                            data_dir_for_artifacts.as_deref(),
                            jc,
                            model_id_for_judge.as_deref().unwrap_or(""),
                        )
                    })
                } else {
                    None
                };

                // 路径越界审批（带 permission.check 子 span）。AutoMode 下先过 judge：
                // judge ALLOW 自动放行越界路径（低风险目标如 /tmp 不打断用户）、DENY 自动
                // 拒、ASK 才落到人工弹框（架构 §4.4.4）。与 ToolCall 链对称。
                if let Some(p) = path_pending {
                    if let Some((judge, judge_model)) = judge_override.as_ref() {
                        let judge_language = data_dir_for_artifacts
                            .as_deref()
                            .map(|d| crate::storage::settings::load(d).general.language)
                            .unwrap_or_default();
                        {
                            let decision = judge_automode_request(AutoModeJudgeRequest {
                                sink: &sink,
                                state: &state,
                                judge_client: judge,
                                model_id: judge_model,
                                tool_name: &call_name_for_judge,
                                tool_input: &call_input_for_judge,
                                effects: &effects_for_judge,
                                transcript: transcript_for_judge
                                    .as_deref()
                                    .map(Vec::as_slice)
                                    .unwrap_or(&[]),
                                hitl: &hitl_for_future,
                                model_io_dump: model_io_dump_for_judge.as_ref(),
                                request_id: &p.request_id,
                                force_automode,
                                is_path_access: true,
                                language: judge_language,
                                cancel: cancel.clone(),
                            })
                            .await;
                            // 路径访问没有「命令保留人工」一说：ALLOW / DENY 都直接 resolve，
                            // 让 await_path_decision 走自动放行 / 自动拒分支；ASK 不 resolve，
                            // 落到下方人工弹框。
                            match decision {
                                AutoModeDecision::Allow => {
                                    hitl_for_future
                                        .resolve(&p.request_id, ApprovalDecision::AllowOnce);
                                }
                                AutoModeDecision::Deny(reason) => {
                                    hitl_for_future.resolve(
                                        &p.request_id,
                                        ApprovalDecision::DenyWithFeedback { feedback: reason },
                                    );
                                }
                                AutoModeDecision::Ask(_) => {}
                            }
                        }
                    }

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

                // AutoMode 短路（架构 §4.4.4）：destructive 工具进入 NeedsApproval 时，
                // 调一次 judge 决定 Allow / Deny / Ask；Allow 自动执行，Deny 按工具类型
                // 拒绝或转人工，Ask 默认保留人工审批。
                if let Some((judge, judge_model)) = judge_override.as_ref() {
                    if let PermissionDecision::NeedsApproval { request_id, .. } = &permission {
                        let judge_language = data_dir_for_artifacts
                            .as_deref()
                            .map(|d| crate::storage::settings::load(d).general.language)
                            .unwrap_or_default();
                        {
                            let decision = judge_automode_request(AutoModeJudgeRequest {
                                sink: &sink,
                                state: &state,
                                judge_client: judge,
                                model_id: judge_model,
                                tool_name: &call_name_for_judge,
                                tool_input: &call_input_for_judge,
                                effects: &effects_for_judge,
                                transcript: transcript_for_judge
                                    .as_deref()
                                    .map(Vec::as_slice)
                                    .unwrap_or(&[]),
                                hitl: &hitl_for_future,
                                model_io_dump: model_io_dump_for_judge.as_ref(),
                                request_id,
                                force_automode,
                                is_path_access: false,
                                language: judge_language,
                                cancel: cancel.clone(),
                            })
                            .await;
                            match decision {
                                AutoModeDecision::Allow => {
                                    // P0-1（架构 §4.4.4）：判官放行的会写段沉淀到 session，
                                    // 让「判一次」覆盖整个对话——下条同 fingerprint 命令命中
                                    // session 规则直接放行，不再烧判官 LLM。egress / 不可记忆
                                    // / 已白名单段不沉淀（见 persist_judge_allowed_segments）。
                                    hitl_for_future.persist_judge_allowed_segments(
                                        &call_name_for_judge,
                                        &effects_for_judge,
                                    );
                                    hitl_for_future
                                        .resolve(request_id, ApprovalDecision::AllowOnce);
                                }
                                AutoModeDecision::Deny(reason) => {
                                    let is_command = matches!(
                                        call_name_for_judge.as_str(),
                                        "Bash" | "PowerShell"
                                    );
                                    if is_command && !force_automode {
                                        // 普通 AutoMode：命令类拒绝需要用户最终确认，保留既有
                                        // PermissionRequested 弹窗，把判官 reason 展示在审批框里。
                                    } else {
                                        // hands-off（force_automode）下命令类也直接拒，不弹——
                                        // 判官说了算，把「为什么拒」作为 tool_result 回给 agent，
                                        // 让它自己换思路，从不打扰用户（架构 §4.4.4）。其它工具
                                        // 在两种模式下都直接拒。
                                        hitl_for_future.resolve(
                                            request_id,
                                            ApprovalDecision::DenyWithFeedback { feedback: reason },
                                        );
                                    }
                                }
                                AutoModeDecision::Ask(_) => {
                                    // 保留人工决策（force_automode 下 ASK 已折叠为 Deny，走不到
                                    // 这里；普通 AutoMode 的 ASK 留人工审批）。
                                }
                            }
                        }
                    }
                }

                // judge 阶段可能耗时（LLM 调用）；若用户在此期间点了中断，judge 的
                // complete 已被 cancel 唤醒返回，这里直接短路——不再弹人工审批阻塞用户。
                if cancellation::is_cancelled(&cancel) {
                    return Err(ModelError::Cancelled);
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

                // —— Edits 快照（架构 §4.13）：本 Run 首次触达某文件时拍 before ——
                // 触达路径 = effects.paths（Edit 的 file_path + Bash 写目标 + rm/rmdir 删除目标）。
                // 在工具执行前拍：删除类命令执行后文件就没了。按真实路径加锁，保证
                // ensure_run_before 与执行串行。post-hook 用 effective_input 重新解析路径。
                let mut _edit_locks: Vec<crate::edits::FileLockGuard> = Vec::new();
                if let (Some(wt), Some(run_id)) = (
                    edits_worktree_for_snapshot.as_ref(),
                    current_run_id_for_snapshot.as_deref(),
                ) {
                    let touched = analyze_effects(&call.name, &effective_input).paths;
                    // 同 path 去重：本循环用 _edit_locks 累积持有每把 per-path 锁不释放，
                    // 同一 path 第二次 lock_file 会在已持有的 async Mutex 上自死锁。effects
                    // 层已去重，这里再兜一道，让锁的使用方自洽、不依赖上游不变量。
                    let mut snapshotted = std::collections::HashSet::new();
                    for path in touched {
                        if !snapshotted.insert(path.clone()) {
                            continue;
                        }
                        if !workspace_for_snapshot.allows(&path) {
                            continue; // 越界路径由 PathAccess 审批把关，未授权不快照
                        }
                        if let Ok(guard) = wt.lock_file(&path).await {
                            _edit_locks.push(guard);
                        }
                        let _ = wt.ensure_run_before(run_id, &path).await;
                    }
                }

                // 执行（对外通信：Bash 子进程 / web 网络 / Edit 文件等都从这里出，§4.4）
                info!(
                    target: "tool",
                    session_id = session_id_for_hooks.as_deref().unwrap_or("-"),
                    run_id = %state.run_id,
                    tool = %call.name,
                    call_id = %call.id,
                    "[Tool:Exec] 执行工具"
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
                let (raw, attachments, exec_failed, semantic_failed) = match tool {
                    Some(t) => match t.execute_rich(tool_ctx, effective_input.clone()).await {
                        Ok(out) => (out.text, out.attachments, false, out.is_error),
                        Err(e) => {
                            warn!(tool = %call.name, error = %e, "tool exec error");
                            (format!("工具执行错误: {e}"), Vec::new(), true, false)
                        }
                    },
                    None => {
                        warn!(tool = %call.name, "tool not in registry");
                        (format!("未找到工具: {}", call.name), Vec::new(), true, false)
                    }
                };
                // 给 surface 的失败口径：执行层故障 + 工具自报语义失败（如 Bash 退出码非 0）。
                // exec_failed 继续单独驱动 materialize 跳过 / PostToolUseFailure hook——
                // 语义失败的输出仍是正常工具产物，照常落 artifact、走 PostToolUse。
                let is_error = exec_failed || semantic_failed;
                let duration_ms = started.elapsed().as_millis() as u64;
                info!(
                    target: "tool",
                    session_id = session_id_for_hooks.as_deref().unwrap_or("-"),
                    tool = %call.name,
                    call_id = %call.id,
                    outcome = if is_error { "error" } else { "ok" },
                    duration_ms,
                    result_bytes = raw.len(),
                    "[Tool:Done] 工具执行完成"
                );

                // Run 级 edits-worktree 在 RunFinished 前统一拍 after；这里不写 per-Edit metadata。

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
                    is_error,
                }));

                Ok((
                    call_index,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content,
                        artifact,
                        attachments,
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
                let shape = match parse_ask_input(&call.input) {
                    Ok(s) => s,
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
                let request_event = match &shape {
                    AskShape::Single {
                        question,
                        options,
                        multi,
                    } => EventPayload::UserQuestionRequested {
                        request_id: request_id.clone(),
                        question: question.clone(),
                        options: options.clone(),
                        multi: *multi,
                        questions: Vec::new(),
                    },
                    AskShape::Multi(questions) => EventPayload::UserQuestionRequested {
                        request_id: request_id.clone(),
                        question: String::new(),
                        options: Vec::new(),
                        multi: false,
                        questions: questions.clone(),
                    },
                };
                sink(state.event(request_event));

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
                    UserAnswer::Multi { .. } => "multi",
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
                    // 用户取消是主动行为，不算工具失败。
                    is_error: false,
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
                        attachments: Vec::new(),
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
                            is_error: true,
                        }));
                        return Ok((
                            call_index,
                            ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content: msg,
                                artifact: None,
                                attachments: Vec::new(),
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
                    is_error: false,
                }));

                Ok((
                    call_index,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: summary_text,
                        artifact: None,
                        attachments: Vec::new(),
                    },
                ))
            }
            .instrument(tool_span),
        )
    }

    /// PlanMode short-circuit（架构 §4.4.5）：处理 `enter` / `update` / `submit` 三个 action。
    ///
    /// - `enter`：切到 PlanMode（落盘 + 更新运行中 Arc）、建 plan 草稿、emit `PlanReady`
    /// - `update`：覆盖写当前 plan，刷新 UI，不走审批
    /// - `submit`：落盘定稿 + 走 HITL 审批：
    ///   - `AllowOnce` / `AllowAndRemember` → 切回 pre_plan_mode（落盘 + Arc）；
    ///     result = "[Plan approved] ..." + 未消费评论
    ///   - `Deny` → 留 PlanMode；result = "[Plan rejected]"
    ///   - `DenyWithFeedback { feedback }` → 留 PlanMode；result 含反馈让模型按反馈改 plan
    ///
    /// plan 落盘路径的归属真源是 `Session.workdir`（有项目 → 项目级，无 → 全局），
    /// 与 surface 的 plan 命令读同一字段，保证两端路径一致。
    fn spawn_plan_mode(
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
            otel.name = "tool.PlanMode",
            otel.kind = "internal",
            hebbian.tool.name = PLAN_MODE_TOOL_NAME,
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

                // 统一的错误出口：emit ToolCallFinished(is_error) + 返回 ToolResult。
                let finish_err = |msg: String| {
                    record_tool_outcome(attr::outcome::FAILED, &call.name, 0.0, false, 0);
                    sink(state.event(EventPayload::ToolCallFinished {
                        index: dispatch_index,
                        call_id: call.id.clone(),
                        result: msg.clone(),
                        duration_ms: 0,
                        truncated: false,
                        artifact_path: None,
                        is_error: true,
                    }));
                    Ok((
                        call_index,
                        ToolResult {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            content: msg,
                            artifact: None,
                            attachments: Vec::new(),
                        },
                    ))
                };

                let parsed = match plan_mode::parse_input(call.input.clone()) {
                    Ok(p) => p,
                    Err(e) => return finish_err(format!("PlanMode 入参解析失败: {e}")),
                };
                let (dd, sid) = match (data_dir.as_deref(), session_id.as_deref()) {
                    (Some(d), Some(s)) => (d, s),
                    _ => {
                        return finish_err(
                            "PlanMode 需要 data_dir + session_id 才能落盘 / 审批".to_string(),
                        )
                    }
                };
                let session = match session_store::load(dd, sid) {
                    Ok(s) => s,
                    Err(e) => return finish_err(format!("PlanMode 读取会话失败: {e}")),
                };
                let workdir = session.workdir.clone();

                use plan_mode::PlanAction;
                match parsed.action {
                    PlanAction::Enter => {
                        // 切 PlanMode：落盘（自动记 pre_plan_mode）+ 更新运行中 Arc，让下一轮
                        // agent_loop 立即收缩工具集。
                        let from_mode = session.run_mode;
                        if let Err(e) = session_store::set_run_mode(
                            dd,
                            sid,
                            crate::run_mode::RunMode::PlanMode,
                        ) {
                            return finish_err(format!("PlanMode 进入失败: {e}"));
                        }
                        crate::run_mode::LiveRunModeRegistry::global()
                            .set(sid, crate::run_mode::RunMode::PlanMode);
                        sink(state.event(EventPayload::RunModeChanged {
                            from: format!("{from_mode:?}"),
                            to: format!("{:?}", crate::run_mode::RunMode::PlanMode),
                        }));

                        let initial = if parsed.plan_markdown.trim().is_empty() {
                            "# Plan\n\n_(drafting…)_\n".to_string()
                        } else {
                            parsed.plan_markdown.clone()
                        };
                        let plan_path = match plans::save_plan(
                            dd,
                            workdir.as_deref(),
                            sid,
                            &initial,
                        ) {
                            Ok(p) => p,
                            Err(e) => return finish_err(format!("PlanMode 草稿落盘失败: {e}")),
                        };
                        let plan_id = plan_mode::plan_id_from_path(&plan_path);
                        let plan_path_str = plan_path.display().to_string();
                        if let Err(e) =
                            session_store::set_active_plan(dd, sid, Some(plan_path_str.clone()))
                        {
                            warn!(error = %e, "PlanMode enter set_active_plan failed");
                        }
                        sink(state.event(EventPayload::PlanReady {
                            plan_id,
                            plan_path: plan_path_str,
                            plan_markdown: initial,
                            summary: parsed.summary.clone(),
                        }));

                        let content = "[Entered Plan Mode] You are now in read-only \
                             investigation. Editing files and running commands are \
                             disabled. Use PlanMode `action:\"update\"` to refine the \
                             plan, and `action:\"submit\"` to submit it for the user's \
                             approval."
                            .to_string();
                        record_tool_outcome(attr::outcome::OK, &call.name, 0.0, false, content.len());
                        sink(state.event(EventPayload::ToolCallFinished {
                            index: dispatch_index,
                            call_id: call.id.clone(),
                            result: content.clone(),
                            duration_ms: 0,
                            truncated: false,
                            artifact_path: None,
                            is_error: false,
                        }));
                        Ok((
                            call_index,
                            ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content,
                                artifact: None,
                                attachments: Vec::new(),
                            },
                        ))
                    }
                    PlanAction::Update => {
                        if parsed.plan_markdown.trim().is_empty() {
                            return finish_err(
                                "PlanMode update 需要非空 plan_markdown".to_string(),
                            );
                        }
                        // 覆盖当前 active_plan；无则新建一份。
                        let plan_path = if let Some(ap) = session.active_plan.as_deref() {
                            let pid = plan_mode::plan_id_from_path(Path::new(ap));
                            plans::update_plan(dd, workdir.as_deref(), sid, &pid, &parsed.plan_markdown)
                        } else {
                            plans::save_plan(dd, workdir.as_deref(), sid, &parsed.plan_markdown)
                        };
                        let plan_path = match plan_path {
                            Ok(p) => p,
                            Err(e) => return finish_err(format!("PlanMode 更新落盘失败: {e}")),
                        };
                        let plan_id = plan_mode::plan_id_from_path(&plan_path);
                        let plan_path_str = plan_path.display().to_string();
                        if let Err(e) =
                            session_store::set_active_plan(dd, sid, Some(plan_path_str.clone()))
                        {
                            warn!(error = %e, "PlanMode update set_active_plan failed");
                        }
                        sink(state.event(EventPayload::PlanReady {
                            plan_id,
                            plan_path: plan_path_str,
                            plan_markdown: parsed.plan_markdown.clone(),
                            summary: parsed.summary.clone(),
                        }));
                        let content =
                            "[Plan updated] The plan has been saved. Keep refining with \
                             `action:\"update\"`, or submit it with `action:\"submit\"`."
                                .to_string();
                        record_tool_outcome(attr::outcome::OK, &call.name, 0.0, false, content.len());
                        sink(state.event(EventPayload::ToolCallFinished {
                            index: dispatch_index,
                            call_id: call.id.clone(),
                            result: content.clone(),
                            duration_ms: 0,
                            truncated: false,
                            artifact_path: None,
                            is_error: false,
                        }));
                        Ok((
                            call_index,
                            ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content,
                                artifact: None,
                                attachments: Vec::new(),
                            },
                        ))
                    }
                    PlanAction::Submit => {
                        Self::run_plan_submit(
                            call,
                            call_index,
                            dispatch_index,
                            dd,
                            sid,
                            workdir.as_deref(),
                            &session,
                            parsed,
                            &state,
                            &sink,
                            &hitl,
                        )
                        .await
                    }
                }
            }
            .instrument(tool_span),
        )
    }

    /// PlanMode `submit` 分支：落盘定稿 → emit `PlanReady` → 走 HITL 审批 → 据决定切回
    /// pre_plan_mode 或留在 PlanMode，最后拼未消费的 plan_comments。
    #[allow(clippy::too_many_arguments)]
    async fn run_plan_submit(
        call: ToolCall,
        call_index: usize,
        dispatch_index: usize,
        dd: &Path,
        sid: &str,
        workdir: Option<&Path>,
        session: &session_store::Session,
        parsed: plan_mode::PlanInput,
        state: &Arc<RunState>,
        sink: &EventSink,
        hitl: &Arc<HitlGate>,
    ) -> Result<(usize, ToolResult), ModelError> {
        // 定稿内容：给了就用 input，否则读现有 active_plan 文件。
        let plan_markdown = if !parsed.plan_markdown.trim().is_empty() {
            parsed.plan_markdown.clone()
        } else {
            session
                .active_plan
                .as_deref()
                .and_then(|ap| std::fs::read_to_string(ap).ok())
                .unwrap_or_default()
        };

        // 落盘：有 active_plan 覆盖它，否则新建。
        let plan_path = if let Some(ap) = session.active_plan.as_deref() {
            let pid = plan_mode::plan_id_from_path(Path::new(ap));
            plans::update_plan(dd, workdir, sid, &pid, &plan_markdown)
        } else {
            plans::save_plan(dd, workdir, sid, &plan_markdown)
        };
        let plan_path = match plan_path {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("PlanMode 定稿落盘失败: {e}");
                warn!(error = %e, "PlanMode submit save failed");
                record_tool_outcome(attr::outcome::FAILED, &call.name, 0.0, false, 0);
                sink(state.event(EventPayload::ToolCallFinished {
                    index: dispatch_index,
                    call_id: call.id.clone(),
                    result: msg.clone(),
                    duration_ms: 0,
                    truncated: false,
                    artifact_path: None,
                    is_error: true,
                }));
                return Ok((
                    call_index,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: msg,
                        artifact: None,
                        attachments: Vec::new(),
                    },
                ));
            }
        };
        let plan_id = plan_mode::plan_id_from_path(&plan_path);
        let plan_path_str = plan_path.display().to_string();

        if let Err(e) = session_store::set_active_plan(dd, sid, Some(plan_path_str.clone())) {
            warn!(error = %e, "PlanMode submit set_active_plan failed");
        }

        sink(state.event(EventPayload::PlanReady {
            plan_id: plan_id.clone(),
            plan_path: plan_path_str.clone(),
            plan_markdown: plan_markdown.clone(),
            summary: parsed.summary.clone(),
        }));

        // 开 HITL approval；workspace 维度（hitl 学习指纹不持久化 plan 审批）。
        let (request_id, waiter) = hitl.open_approval(None, None);
        sink(state.event(EventPayload::PermissionRequested {
            request_id: request_id.clone(),
            kind: PermissionKind::Plan {
                plan_id: plan_id.clone(),
                plan_path: plan_path_str.clone(),
                plan_markdown: plan_markdown.clone(),
                summary: parsed.summary.clone(),
                steps: Vec::new(),
            },
            summary: if parsed.summary.is_empty() {
                "计划待审批".to_string()
            } else {
                parsed.summary.clone()
            },
            risk: RiskLevel::Low,
            // 计划审批走 PlanMode 流程，不被 AutoMode judge 接管。
            auto_handled: false,
            call_id: String::new(),
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
            plan_comments::list_unconsumed(dd, workdir, sid, &plan_id).unwrap_or_default();

        match decision {
            ApprovalDecision::AllowOnce | ApprovalDecision::AllowAndRemember { .. } => {
                // 切回 pre_plan_mode：落盘 + 更新运行中 Arc，让下一轮 agent_loop 恢复工具集。
                let target_mode = session
                    .pre_plan_mode
                    .unwrap_or(crate::run_mode::RunMode::Default);
                if let Err(e) = session_store::set_run_mode(dd, sid, target_mode) {
                    warn!(error = %e, "PlanMode submit: set_run_mode 失败");
                } else {
                    crate::run_mode::LiveRunModeRegistry::global().set(sid, target_mode);
                    sink(state.event(EventPayload::RunModeChanged {
                        from: format!("{:?}", crate::run_mode::RunMode::PlanMode),
                        to: format!("{target_mode:?}"),
                    }));
                }
                // 清空 pre_plan_mode（已消费）
                let _ = session_store::set_pre_plan_mode(dd, sid, None);
                content.push_str("[Plan approved] Proceeding with implementation.\n\n");
                content.push_str(&plan_markdown);
            }
            ApprovalDecision::Deny => {
                content
                    .push_str("[Plan rejected by user] Stay in PlanMode and revise the plan.");
            }
            ApprovalDecision::DenyWithFeedback { ref feedback } => {
                content.push_str("[Plan rejected by user — please revise]\n\nUser feedback:\n");
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
            if let Err(e) = plan_comments::mark_consumed(dd, workdir, sid, &plan_id, ids) {
                warn!(error = %e, "PlanMode submit: mark_consumed failed");
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
            is_error: false,
        }));

        Ok((
            call_index,
            ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content,
                artifact: None,
                attachments: Vec::new(),
            },
        ))
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
        let parent_run_mode = *self.run_mode.lock().unwrap();
        let parent_force_automode = self.force_automode.clone();
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
                        is_error: !ok,
                    }));
                    (
                        call_index,
                        ToolResult {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            content,
                            artifact: None,
                            attachments: Vec::new(),
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
                    parent_run_mode,
                    parent_force_automode,
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
    fn request_path_approval(
        &self,
        tool_name: &str,
        call_id: &str,
        paths: Vec<PathBuf>,
    ) -> PathApproval {
        // 路径越界不在工具维度，AllowAndRemember 在外层把路径加进 workspace.allowed_paths，
        // 不通过 hitl learned 表，所以传 None。
        let (request_id, waiter) = self.hitl.open_approval(None, None);
        let path_strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let summary = if path_strings.len() == 1 {
            format!("工具 {tool_name} 想访问越界路径：{}", path_strings[0])
        } else {
            format!("工具 {tool_name} 想访问 {} 个越界路径", path_strings.len())
        };
        // AutoMode 时 judge 会接管这条路径审批（judge ALLOW 自动放行低风险目标、
        // ASK 才弹人工）——与 ToolCall 链对称，surface 据 auto_handled 决定先压住框等
        // judge，避免闪现（架构 §4.4.4）。
        let auto_handled = self.automode_will_handle();
        self.emit(EventPayload::PermissionRequested {
            request_id: request_id.clone(),
            kind: PermissionKind::PathAccess {
                tool_name: tool_name.to_string(),
                paths: path_strings,
            },
            summary,
            risk: RiskLevel::Medium,
            auto_handled,
            call_id: call_id.to_string(),
        });
        PathApproval {
            request_id,
            paths,
            waiter,
        }
    }

    /// 当前工具审批是否会被 AutoMode judge 接管——决定 surface 要不要立即弹审批框。
    ///
    /// 条件与 async 块里的实际 judge 短路判定一致：RunMode=AutoMode + judge_client 可用。
    /// 任一不满足都返回 `false`，让 surface 正常弹框，避免「标了接管但其实没接管 →
    /// 该弹的不弹卡死」。
    fn automode_will_handle(&self) -> bool {
        *self.run_mode.lock().unwrap() == crate::run_mode::RunMode::AutoMode
            && self.judge_client.is_some()
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
        ApprovalDecision::Deny => Err("用户主动拒绝路径访问，未填写原因。".into()),
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
                ApprovalDecision::Deny => Err("用户主动拒绝，未填写原因。".into()),
                ApprovalDecision::DenyWithFeedback { feedback } => Err(feedback),
            }
        }
    }
}

/// 解析本次审批用的判官 client + model（架构 §4.4.4 判官模型选择）。
/// 会话 provider 配置了专属 judge 模型 → 专属 client + judge_model；
/// 未配置 / 构建失败 / 无 data_dir（单测路径）→ 回退主 client + 会话主模型。
fn resolve_judge_for_call(
    data_dir: Option<&std::path::Path>,
    fallback_client: &Arc<dyn model_gateway::client::ModelClient>,
    fallback_model: &str,
) -> (Arc<dyn model_gateway::client::ModelClient>, String) {
    if let Some(dd) = data_dir {
        let session_provider_id = fallback_client.provider_id().to_string();
        if let Some(cfg) = crate::automode::resolve_judge_config(dd, &session_provider_id) {
            return (cfg.client, cfg.model);
        }
    }
    (fallback_client.clone(), fallback_model.to_string())
}

/// AutoMode judge 判定一次审批请求（ToolCall 与 PathAccess 两条链共用）的入参。
/// 所有依赖以值 / `Arc` 传入，便于在 `'static` future 内调用。
struct AutoModeJudgeRequest<'a> {
    sink: &'a EventSink,
    state: &'a Arc<RunState>,
    judge_client: &'a Arc<dyn model_gateway::client::ModelClient>,
    model_id: &'a str,
    tool_name: &'a str,
    tool_input: &'a Value,
    effects: &'a Effects,
    transcript: &'a [model_gateway::types::TranscriptEntry],
    hitl: &'a Arc<HitlGate>,
    model_io_dump: Option<&'a ModelIoDump>,
    request_id: &'a PermissionRequestId,
    force_automode: bool,
    /// 这条审批是否走路径越界链（PathAccess）。`true` 时 DENY 直接拒不保留人工
    /// （路径就是不让碰）；`false`（ToolCall）时命令类 DENY 保留用户最终拍板权。
    is_path_access: bool,
    language: crate::storage::settings::AppLanguage,
    cancel: CancelFlag,
}

/// 调 AutoMode judge 判定一次审批，emit `PermissionAutoJudged` + 落 model_io。
///
/// 返回 judge 的最终决策（已按 `force_automode` 折叠 Ask→Deny）。调用方据此决定
/// resolve 还是保留人工弹框——ToolCall 与 PathAccess 的差异只在这一步，判定本身一致。
/// 判官 client / model 由 [`resolve_judge_for_call`] 决定（架构 §4.4.4 判官模型选择）。
/// AutoMode 判官单次判定的 wall-clock 超时上限。判官是审批的辅助快速决策（本应秒级
/// 返回）；provider 抖动 / DeepSeek 同 chat session 并发挂起时，判官两次 LLM 调用
/// （Classifier A 段前缀 + judge 决策）即便各自有 client read_timeout 兜底，也可能
/// 累计卡上几分钟，让整条 AutoMode 工具链「黄呼吸」卡死、run 永不前进。超过这个上限
/// 就降级 Ask（转人工审批），把"无限卡死"变成"超时后弹框让用户拍板"。用户中断
/// （cancel）仍随时立即生效，这是中断之外的自动兜底（架构 §4.4.4）。
#[cfg(not(test))]
fn judge_decision_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(25)
}
#[cfg(test)]
fn judge_decision_timeout() -> std::time::Duration {
    // 远大于任何 mock judge（纯返回 / 多段 classify）的真实耗时——避免全量并发高负载下
    // 误触发、把正常测试的 Allow 误降级成 Ask；又远小于 HangingJudge 的挂起时长，复现
    // 测试仍能区分超时路径。
    std::time::Duration::from_secs(3)
}

async fn judge_automode_request(req: AutoModeJudgeRequest<'_>) -> AutoModeDecision {
    // judge 必须看到 hebbian 静态分析的全量 effects（segments / write_targets /
    // dangerous_kinds），不重复解析 shell。Bash 段前缀分类只对命令类生效。
    let judge_start = Instant::now();
    // 判官单次 LLM 决策调用包一个 wall-clock 超时：provider 抖动 / 同 session 并发挂起时即便
    // client 有 read_timeout 也可能卡住工具链，超时降级 Ask 转人工（架构 §4.4.4）。判官内部
    // 仍透传 cancel，用户中断随时立即生效。（Classifier A 逐段前缀分类已停用，见下方注释。）
    let judge_eval = async {
        // Classifier A（每个 AST 段各打一次 LLM 做前缀分类）**已停用**（2026-06-27）：它对
        // N 段命令**串行**打 N 次 LLM——实测 session 202606270532 一条 7 段 kubectl 循环判官
        // 阶段打了 8 次串行 sonnet 调用 ≈ 30s，是 AutoMode 判官「卡住」的根因。静态 tree-sitter
        // effects 已给出 `kubectl get` / `git commit` 级别的段 fingerprint，判官还拿到完整命令
        // 文本，逐段 LLM 精修（识别 `gg`=git 这类别名）收益微薄却烧 N 次；命令注入由静态层
        // 的 ast-too-complex 兜底（架构 §4.4.4）。判官改为只用静态 effects、单次 LLM 调用。
        let judge_effects = req.effects;
        // 标注哪些段已被用户 allow 规则 / session 记忆覆盖，喂给判官让它对「用户先前
        // 已批准过」的命令放心 ALLOW（架构 §4.4.4）。
        let whitelisted_fingerprints: Vec<String> = req
            .hitl
            .approval_segments(req.tool_name, judge_effects)
            .into_iter()
            .filter(|s| matches!(s.status, protocol::ApprovalSegmentStatus::Whitelisted))
            .map(|s| s.fingerprint)
            .collect();
        crate::automode::judge_auto_mode(
            req.judge_client,
            req.model_id,
            req.tool_name,
            req.tool_input,
            judge_effects,
            req.transcript,
            &whitelisted_fingerprints,
            req.language,
            req.cancel.clone(),
        )
        .await
    };
    let raw_decision = match tokio::time::timeout(judge_decision_timeout(), judge_eval).await {
        Ok(decision) => decision,
        Err(_) => {
            warn!(
                target: "permission",
                tool = %req.tool_name,
                timeout_ms = judge_decision_timeout().as_millis() as u64,
                "[AutoMode] 判官超时未返回（provider 可能抖动），降级 Ask 转人工审批"
            );
            AutoModeDecision::Ask("AutoMode 判官响应超时（模型可能在抖动），已转人工审批".to_string())
        }
    };
    let judge_duration_ms = judge_start.elapsed().as_millis() as u64;
    let raw_label = raw_decision.as_label();
    // force_automode 子开关：把 ASK 收紧为 Deny；普通 AutoMode 保留 ASK 走人工审批。
    let decision = if req.force_automode {
        raw_decision.collapse_ask_to_deny()
    } else {
        raw_decision
    };
    info!(
        target: "permission",
        tool = %req.tool_name,
        model = req.model_id,
        raw = raw_label,
        final = decision.as_label(),
        force_automode = req.force_automode,
        reason = decision.reason().unwrap_or(""),
        "[AutoMode] LLM 判官结果：{} → {}（{}）",
        raw_label,
        decision.as_label(),
        decision.reason().unwrap_or("无理由")
    );
    // judge 出结果后这条审批是否仍需用户拍板（surface 据此把被接管的框显形）：
    // ASK 永远要人工；ToolCall 链命令类被判 DENY 时保留用户推翻权（普通 AutoMode，
    // 非 force）；其余（ALLOW、自动拒）不需要。与下方调用点的 resolve 策略严格对齐。
    let requires_human = match &decision {
        AutoModeDecision::Ask(_) => true,
        AutoModeDecision::Deny(_) => {
            !req.is_path_access
                && matches!(req.tool_name, "Bash" | "PowerShell")
                && !req.force_automode
        }
        AutoModeDecision::Allow => false,
    };
    (req.sink)(req.state.event(protocol::EventPayload::PermissionAutoJudged {
        request_id: Some(req.request_id.clone()),
        tool_name: req.tool_name.to_string(),
        decision: decision.as_label().to_string(),
        reason: decision.reason().map(str::to_string),
        requires_human,
    }));
    // 判官请求记入 model_io.jsonl（kind="judge"），前端蓝色标签渲染。
    if let Some(dump) = req.model_io_dump {
        dump.record(DumpEntry {
            ts: model_io_dump::iso_now(),
            run_id: req.state.run_id.to_string(),
            turn: 0,
            model: req.model_id.to_string(),
            request: serde_json::json!({
                "tool": req.tool_name,
                "input": req.tool_input,
                "language": req.language,
            }),
            response: serde_json::json!({
                "raw": raw_label,
                "final": decision.as_label(),
                "reason": decision.reason(),
            }),
            duration_ms: judge_duration_ms,
            kind: "judge".to_string(),
        });
    }
    decision
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
        is_error: true,
    }));
    (
        call_index,
        ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: denied,
            artifact: None,
            attachments: Vec::new(),
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
        is_error: true,
    }));
    (
        call_index,
        ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: error,
            artifact: None,
            attachments: Vec::new(),
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

fn parse_ask_input(input: &serde_json::Value) -> Result<AskShape, String> {
    let parsed: AskInput = serde_json::from_value(input.clone())
        .map_err(|e| format!("ask 工具 input 解析失败：{e}"))?;

    // 多题：questions 非空时走多题分支，老顶层字段被忽略
    if !parsed.questions.is_empty() {
        if !(1..=5).contains(&parsed.questions.len()) {
            return Err(format!(
                "ask 工具 questions 长度需在 1-5 之间，实际给了 {} 个",
                parsed.questions.len()
            ));
        }
        for (i, q) in parsed.questions.iter().enumerate() {
            if q.title.trim().is_empty() {
                return Err(format!("ask 工具 questions[{i}].title 不能为空"));
            }
            if !(2..=5).contains(&q.options.len()) {
                return Err(format!(
                    "ask 工具 questions[{i}] 要求提供 2-5 个选项，实际给了 {} 个",
                    q.options.len()
                ));
            }
        }
        return Ok(AskShape::Multi(parsed.questions));
    }

    // 单题
    let question = parsed
        .question
        .ok_or_else(|| "ask 工具需要 question 或 questions 字段".to_string())?;
    if !(2..=5).contains(&parsed.options.len()) {
        return Err(format!(
            "ask 工具要求提供 2-5 个选项，实际给了 {} 个",
            parsed.options.len()
        ));
    }
    Ok(AskShape::Single {
        question,
        options: parsed.options,
        multi: parsed.multi,
    })
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

    #[test]
    fn bash_safe_write_allowed_only_for_in_bounds_pure_create() {
        use crate::effects::analyze_effects;
        // 纯创建 + 界内 → 放行
        let e = analyze_effects("Bash", &serde_json::json!({"command": "mkdir build && touch a.txt"}));
        assert!(bash_safe_write_allowed("Bash", &e, true));
        // 同命令但越界（paths_in_bounds=false）→ 不放行（路径闸接管）
        assert!(!bash_safe_write_allowed("Bash", &e, false));
        // 混入会跑代码的命令（cargo build）→ 不放行
        let e2 = analyze_effects("Bash", &serde_json::json!({"command": "mkdir build && cargo build"}));
        assert!(!bash_safe_write_allowed("Bash", &e2, true));
        // 混入 rm（不可记忆，非 safe_write）→ 不放行
        let e3 = analyze_effects("Bash", &serde_json::json!({"command": "mkdir build && rm -rf old"}));
        assert!(!bash_safe_write_allowed("Bash", &e3, true));
        // 纯只读 → 不放行（没有 safe_write 段，交 ReadOnly 短路处理，不归 safe-write 档）
        let e4 = analyze_effects("Bash", &serde_json::json!({"command": "ls -la"}));
        assert!(!bash_safe_write_allowed("Bash", &e4, true));
        // mkdir + 只读 ls → 放行（会写段全是 safe_write，只读段免匹配）
        let e5 = analyze_effects("Bash", &serde_json::json!({"command": "mkdir build && ls build"}));
        assert!(bash_safe_write_allowed("Bash", &e5, true));
        // 非命令工具 → 不放行
        assert!(!bash_safe_write_allowed("Edit", &e, true));
    }

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

    /// 判官恒输出 DENY，用于验证 hands-off（force_automode）下命令类拒绝的处置。
    struct StaticDenyJudge;

    #[async_trait]
    impl model_gateway::client::ModelClient for StaticDenyJudge {
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
                text: "DENY: judged unsafe in test".to_string(),
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

    /// 判官 mock：模拟「judge 跑到一半用户点中断」——complete 内把传入的 cancel 置位
    /// 并返回 `Cancelled`。固化「中断按钮在 AutoMode 自动审批阶段失效」回归：旧代码给
    /// judge 传独立的 `AtomicBool::new(false)` 假 flag，judge 内即便置位也影响不到
    /// dispatcher 的真实 cancel → 后续 `is_cancelled` 检测不到 → 工具照常执行。
    struct CancelAwareJudge;

    #[async_trait]
    impl model_gateway::client::ModelClient for CancelAwareJudge {
        fn provider_id(&self) -> &str {
            "test"
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            cancel: common::CancelFlag,
        ) -> Result<ModelResponse, model_gateway::types::ModelError> {
            // 模拟用户在 judge 运行期间点了中断：置位真实 cancel 并报取消。
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            Err(model_gateway::types::ModelError::Cancelled)
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

    /// 判官 mock：LLM 调用 sleep 远超判官超时（模拟 provider 抖动 / DeepSeek 同
    /// chat session 并发挂起导致 SSE 迟迟不来）。用于复现「判官迟迟不返回 →
    /// AutoMode 工具链无限黄呼吸卡死」。future 被判官超时 drop 时 sleep 随之取消。
    struct HangingJudge;

    #[async_trait]
    impl model_gateway::client::ModelClient for HangingJudge {
        fn provider_id(&self) -> &str {
            "test"
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: common::CancelFlag,
        ) -> Result<ModelResponse, model_gateway::types::ModelError> {
            // sleep 远超判官超时 + 测试外层超时：模拟判官永久挂起（hang），不会"sleep
            // 完就放行"而漏掉复现。future 被判官超时 drop 时 sleep 随之取消，干净退出。
            tokio::time::sleep(Duration::from_secs(30)).await;
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

    /// 测试用 Edit 工具：name="Edit"，effects 走 `Effects::mutating(file_path)`。
    /// 不真正落盘，只验证 dispatcher 的免审 / 审批决策。
    struct NoopEditTool;

    #[async_trait]
    impl crate::tools::Tool for NoopEditTool {
        fn name(&self) -> &str {
            "Edit"
        }

        fn description(&self) -> &str {
            "test edit tool"
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _input: Value) -> AppResult<String> {
            Ok("edited".to_string())
        }
    }

    struct NoopReadTool;

    #[async_trait]
    impl crate::tools::Tool for NoopReadTool {
        fn name(&self) -> &str {
            "Read"
        }

        fn description(&self) -> &str {
            "test read tool"
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _input: Value) -> AppResult<String> {
            Ok("file contents".to_string())
        }
    }

    /// 构造一个 Default-mode dispatcher，挂上指定 data_dir / session_id，注册 Read +
    /// Edit 两个桩工具。data_dir 只读豁免测试用——验证只读工具读 data_dir 整树免
    /// PathAccess、写工具仍审批。
    fn data_dir_dispatcher(
        workspace: Arc<Workspace>,
        data_dir: PathBuf,
        session_id: String,
    ) -> (ToolDispatcher, tokio::sync::mpsc::Receiver<protocol::Event>) {
        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(NoopReadTool) as Box<dyn crate::tools::Tool>,
            Box::new(NoopEditTool) as Box<dyn crate::tools::Tool>,
        ]));
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
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
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
            model_id: None,
            judge_client: None,
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: Some(session_id),
            data_dir_for_artifacts: Some(data_dir),
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };
        (dispatcher, rx)
    }

    /// 架构 §4.4.2：只读工具读 data_dir（~/.hebbian/）下「非当前 session」的路径——
    /// 别的 session 的 jsonl、logs/ ——免 PathAccess 审批，不 emit PermissionRequested。
    /// 修前（无 data_dir 只读豁免）这里必然弹审批，A/B 翻转可复现。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_only_access_to_data_dir_skips_path_approval() {
        use protocol::EventPayload;

        let workdir = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        // data_dir 下「别的 session」+ logs/ 的真实文件，模拟 agent 自查历史。
        let other_session = data_dir.path().join("sessions/other-sid");
        std::fs::create_dir_all(&other_session).unwrap();
        let target = other_session.join("session.jsonl");
        std::fs::write(&target, "{}\n").unwrap();

        let workspace = Workspace::new(workdir.path(), Vec::new());
        let (dispatcher, mut rx) =
            data_dir_dispatcher(workspace, data_dir.path().to_path_buf(), "sid-current".into());

        let call = ToolCall {
            id: "call_read_other_session".into(),
            name: "Read".into(),
            input: serde_json::json!({ "file_path": target.to_string_lossy() }),
        };

        let results = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("只读访问 data_dir 应直接执行，不卡审批")
            .expect("dispatch 不应报错");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Read");

        while let Ok(event) = rx.try_recv() {
            if let EventPayload::PermissionRequested { .. } = event.payload {
                panic!("只读访问 data_dir 不应 emit PermissionRequested");
            }
        }
    }

    /// 安全底线：写工具（Edit）写 data_dir 下的文件——即便只读工具对同路径免审——
    /// 仍走 PathAccess 审批。保护 providers.json 等明文凭证不被一次写入篡改 / 泄漏。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_access_to_data_dir_still_requires_approval() {
        use protocol::EventPayload;

        let workdir = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(workdir.path(), Vec::new());
        let (dispatcher, mut rx) =
            data_dir_dispatcher(workspace, data_dir.path().to_path_buf(), "sid-current".into());

        // 直指 data_dir 根下的敏感配置——只读会放行，写入必须审批。
        let target = data_dir.path().join("providers.json");
        let call = ToolCall {
            id: "call_edit_providers".into(),
            name: "Edit".into(),
            input: serde_json::json!({ "file_path": target.to_string_lossy() }),
        };

        let hitl_for_surface = dispatcher.hitl.clone();
        let saw_prompt = Arc::new(AtomicBool::new(false));
        let saw_prompt_for_task = saw_prompt.clone();
        let surface = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let EventPayload::PermissionRequested { request_id, .. } = &event.payload {
                    saw_prompt_for_task.store(true, std::sync::atomic::Ordering::SeqCst);
                    hitl_for_surface.resolve(
                        request_id,
                        ApprovalDecision::DenyWithFeedback {
                            feedback: "test deny".into(),
                        },
                    );
                    break;
                }
            }
        });

        let _ = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("写 data_dir 应在 5s 内被拒绝返回");
        surface.await.unwrap();
        assert!(
            saw_prompt.load(std::sync::atomic::Ordering::SeqCst),
            "写 data_dir 必须 emit PermissionRequested"
        );
    }

    /// 构造一个最小 Default-mode dispatcher（Edit 工具 + 给定 workspace），返回
    /// dispatcher 与事件接收端。回归测试 §4.4.3 界内编辑免审用。
    fn default_mode_edit_dispatcher(
        workspace: Arc<Workspace>,
    ) -> (ToolDispatcher, tokio::sync::mpsc::Receiver<protocol::Event>) {
        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(NoopEditTool) as Box<dyn crate::tools::Tool>
        ]));
        let hitl = Arc::new(crate::tools::hitl::HitlGate::default());
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
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
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
            model_id: None,
            judge_client: None,
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };
        (dispatcher, rx)
    }

    /// 架构 §4.4.3：Default 模式下，写工作区内、非 git 元数据的文件 → 直接执行，
    /// **不** emit PermissionRequested。修前（EditAutomatically 删除前的 AskBeforeEdits
    /// 默认）这里必然弹审批，A/B 翻转可复现。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn default_mode_in_workspace_edit_auto_allowed_without_prompt() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let (dispatcher, mut rx) = default_mode_edit_dispatcher(workspace);

        let target = tmp.path().join("src/main.rs");
        let call = ToolCall {
            id: "call_edit_in".into(),
            name: "Edit".into(),
            input: serde_json::json!({ "file_path": target.to_string_lossy() }),
        };

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("界内编辑不应卡在审批")
            .expect("dispatch 不应返回错误");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "edited");

        while let Ok(event) = rx.try_recv() {
            if let EventPayload::PermissionRequested { .. } = event.payload {
                panic!("界内编辑不应 emit PermissionRequested");
            }
        }
    }

    /// 架构 §4.4.3：Default 模式下，写工作区外的文件 → PathAccess 审批，必 emit
    /// PermissionRequested。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn default_mode_out_of_workspace_edit_requires_approval() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let (dispatcher, mut rx) = default_mode_edit_dispatcher(workspace);

        let target = outside.path().join("evil.rs");
        let call = ToolCall {
            id: "call_edit_out".into(),
            name: "Edit".into(),
            input: serde_json::json!({ "file_path": target.to_string_lossy() }),
        };

        // 模拟 surface：收到审批请求即拒绝，让 dispatch 尽快返回。
        let hitl_for_surface = dispatcher.hitl.clone();
        let saw_prompt = Arc::new(AtomicBool::new(false));
        let saw_prompt_for_task = saw_prompt.clone();
        let surface = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let EventPayload::PermissionRequested { request_id, .. } = &event.payload {
                    saw_prompt_for_task.store(true, std::sync::atomic::Ordering::SeqCst);
                    hitl_for_surface.resolve(
                        request_id,
                        ApprovalDecision::DenyWithFeedback {
                            feedback: "test deny".into(),
                        },
                    );
                    break;
                }
            }
        });

        let _ = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("界外编辑应在 5s 内被拒绝返回");
        surface.await.unwrap();
        assert!(
            saw_prompt.load(std::sync::atomic::Ordering::SeqCst),
            "界外编辑必须 emit PermissionRequested"
        );
    }

    /// 架构 §4.4.3：Default 模式下，写界内但命中 git 元数据（.git/config）的文件 →
    /// 仍走工具审批（worktree 兜不住，不可逆），必 emit PermissionRequested。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn default_mode_git_meta_edit_requires_approval() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let (dispatcher, mut rx) = default_mode_edit_dispatcher(workspace);

        let target = tmp.path().join(".git/config");
        let call = ToolCall {
            id: "call_edit_gitmeta".into(),
            name: "Edit".into(),
            input: serde_json::json!({ "file_path": target.to_string_lossy() }),
        };

        let hitl_for_surface = dispatcher.hitl.clone();
        let saw_prompt = Arc::new(AtomicBool::new(false));
        let saw_prompt_for_task = saw_prompt.clone();
        let surface = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let EventPayload::PermissionRequested { request_id, .. } = &event.payload {
                    saw_prompt_for_task.store(true, std::sync::atomic::Ordering::SeqCst);
                    hitl_for_surface.resolve(
                        request_id,
                        ApprovalDecision::DenyWithFeedback {
                            feedback: "test deny".into(),
                        },
                    );
                    break;
                }
            }
        });

        let _ = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("git 元数据编辑应在 5s 内返回");
        surface.await.unwrap();
        assert!(
            saw_prompt.load(std::sync::atomic::Ordering::SeqCst),
            "写 .git/config 必须 emit PermissionRequested"
        );
    }

    // ── Yolo 模式（架构 §4.4.3）─────────────────────────────────────────────

    /// 纯函数判定（架构 §4.4.3）：非 Yolo 返回 None；Yolo 界内非危险 → Approved；
    /// 越界 / git-meta / 危险复合模式 → Denied。dispatcher 集成测试与本表共用一份逻辑。
    #[test]
    fn yolo_decision_table() {
        use crate::run_mode::RunMode;
        // 非 Yolo：本函数不接管
        assert!(yolo_decision(RunMode::Default, false, true, false, &[]).is_none());
        assert!(yolo_decision(RunMode::AutoMode, true, false, true, &[]).is_none());
        // Yolo 界内非危险 → 放行
        assert!(matches!(
            yolo_decision(RunMode::Yolo, false, true, false, &[]),
            Some(PermissionDecision::Approved)
        ));
        // 越界 → 拒（优先于其它）
        assert!(matches!(
            yolo_decision(RunMode::Yolo, false, false, false, &[]),
            Some(PermissionDecision::Denied { .. })
        ));
        // git-meta → 拒
        assert!(matches!(
            yolo_decision(RunMode::Yolo, false, true, true, &[]),
            Some(PermissionDecision::Denied { .. })
        ));
        // 危险复合模式 → 拒，reason 带 kind
        match yolo_decision(
            RunMode::Yolo,
            true,
            true,
            false,
            &["rm-rf-root".to_string()],
        ) {
            Some(PermissionDecision::Denied { reason }) => {
                assert!(reason.contains("rm-rf-root"), "reason 应带 dangerous_kind: {reason}")
            }
            other => panic!("危险模式应 Denied，实际 {other:?}"),
        }
    }

    #[test]
    fn edit_auto_allowed_table() {
        use crate::run_mode::RunMode;
        // 界内非 git-meta 的 Edit/Write：Default / AutoMode / Yolo 三档都免审
        for mode in [RunMode::Default, RunMode::AutoMode, RunMode::Yolo] {
            assert!(
                edit_auto_allowed("Edit", mode, true, false),
                "{mode:?} 界内编辑应免审"
            );
            assert!(
                edit_auto_allowed("Write", mode, true, false),
                "{mode:?} 界内写入应免审"
            );
        }
        // PlanMode 下不免审（编辑工具本就被过滤，纯函数也守住）
        assert!(!edit_auto_allowed("Edit", RunMode::PlanMode, true, false));
        // 界外 / git-meta：任何模式都不走本函数放行（交 PathAccess / 工具审批）
        assert!(!edit_auto_allowed("Edit", RunMode::AutoMode, false, false));
        assert!(!edit_auto_allowed("Edit", RunMode::AutoMode, true, true));
        // 非编辑工具一律 false
        assert!(!edit_auto_allowed("Bash", RunMode::AutoMode, true, false));
    }

    /// 造一个指定 run_mode 的 dispatcher，挂 NoopEditTool（Edit）+ DestructiveNoopTool（Bash）。
    fn mode_dispatcher(
        workspace: Arc<Workspace>,
        run_mode: crate::run_mode::RunMode,
    ) -> (ToolDispatcher, tokio::sync::mpsc::Receiver<protocol::Event>) {
        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(NoopEditTool) as Box<dyn crate::tools::Tool>,
            Box::new(DestructiveNoopTool) as Box<dyn crate::tools::Tool>,
        ]));
        let hitl = Arc::new(crate::tools::hitl::HitlGate::default());
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
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
            run_mode: Arc::new(std::sync::Mutex::new(run_mode)),
            model_id: None,
            judge_client: None,
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };
        (dispatcher, rx)
    }

    /// Yolo（架构 §4.4.3）：界内普通命令（`ls`）直接执行，不弹审批——无人值守一气呵成。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yolo_in_workspace_command_auto_allowed() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let (dispatcher, mut rx) = mode_dispatcher(workspace, crate::run_mode::RunMode::Yolo);

        let call = ToolCall {
            id: "call_yolo_ls".into(),
            name: "Bash".into(),
            input: serde_json::json!({ "command": "ls -la" }),
        };

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("Yolo 界内命令不应卡在审批")
            .expect("dispatch 不应返回错误");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "executed");

        while let Ok(event) = rx.try_recv() {
            if let EventPayload::PermissionRequested { .. } = event.payload {
                panic!("Yolo 界内命令不应 emit PermissionRequested");
            }
        }
    }

    /// Yolo 红线（架构 §4.4.3）：`rm -rf` 命中危险复合模式 → **自动拒、不挂起、不弹审批**，
    /// reason 作为 tool_result 回灌 agent。这是 Yolo 与 Default 的本质区别（Default 会弹审批
    /// 等人，无人值守下会永久挂起）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yolo_redline_command_auto_denied_without_prompt() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let (dispatcher, mut rx) = mode_dispatcher(workspace, crate::run_mode::RunMode::Yolo);

        let call = ToolCall {
            id: "call_yolo_rmrf".into(),
            name: "Bash".into(),
            input: serde_json::json!({ "command": "rm -rf /" }),
        };

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("Yolo 红线应自动拒、不挂起")
            .expect("dispatch 不应返回错误");
        assert_eq!(result.len(), 1);
        assert!(
            result[0].content.contains("被拒绝") && result[0].content.contains("全速模式"),
            "tool_result 应回灌拦截原因：{}",
            result[0].content
        );

        while let Ok(event) = rx.try_recv() {
            if let EventPayload::PermissionRequested { .. } = event.payload {
                panic!("Yolo 红线必须自动拒，不应 emit PermissionRequested");
            }
        }
    }

    /// Yolo 红线：写工作区外的文件 → 自动拒、不挂起、不弹审批（无人值守不放界外）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yolo_out_of_workspace_edit_auto_denied_without_prompt() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let (dispatcher, mut rx) = mode_dispatcher(workspace, crate::run_mode::RunMode::Yolo);

        let target = outside.path().join("evil.rs");
        let call = ToolCall {
            id: "call_yolo_edit_out".into(),
            name: "Edit".into(),
            input: serde_json::json!({ "file_path": target.to_string_lossy() }),
        };

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("Yolo 界外编辑应自动拒、不挂起")
            .expect("dispatch 不应返回错误");
        assert_eq!(result.len(), 1);
        assert!(
            result[0].content.contains("被拒绝") && result[0].content.contains("全速模式"),
            "界外编辑应回灌拦截原因：{}",
            result[0].content
        );

        while let Ok(event) = rx.try_recv() {
            if let EventPayload::PermissionRequested { .. } = event.payload {
                panic!("Yolo 界外编辑必须自动拒，不应 emit PermissionRequested");
            }
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
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
            model_id: None,
            judge_client: None,
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,

            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };

        let call = ToolCall {
            id: "call_1".into(),
            name: "Bash".into(),
            // chmod 是真·会写命令（非 safe-write），仍需审批——验证审批后正常 resolve。
            // （touch/mkdir 现归 safe-write 档自动放行，§4.4.2.3，不再适合做"需审批"用例。）
            input: serde_json::json!({ "command": "echo hi && chmod 755 a.txt", "cwd": tmp.path() }),
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
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::AutoMode)),
            model_id: Some("claude-opus-4.7".to_string()),
            judge_client: Some(Arc::new(StaticAllowJudge)),
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };

        let call = ToolCall {
            id: "call_automode".into(),
            name: "Bash".into(),
            // chmod 是真·会写命令（非 safe-write），AutoMode 下仍交判官——验证判官 allow 后
            // 无需人工即放行。（touch/mkdir 现归 safe-write 档自动放行不进判官，§4.4.2.3。）
            input: serde_json::json!({ "command": "chmod 755 automode-ok" }),
        };

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("AutoMode should resolve approval without a human response")
            .expect("dispatch should succeed");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "executed");

        let mut saw_allow = false;
        let mut saw_auto_handled = false;
        while let Ok(event) = rx.try_recv() {
            if let EventPayload::PermissionAutoJudged { decision, .. } = &event.payload {
                saw_allow = decision == "allow";
            }
            // 审批框闪现修复（架构 §4.4.4）：AutoMode + 白名单模型时，PermissionRequested
            // 必须带 auto_handled=true，让 surface 不弹框、等 judge。与 judge 实际接管一致。
            if let EventPayload::PermissionRequested { auto_handled, .. } = &event.payload {
                saw_auto_handled = *auto_handled;
            }
        }
        assert!(saw_allow, "AutoMode judge should allow the supported model");
        assert!(
            saw_auto_handled,
            "AutoMode + 白名单模型应在 PermissionRequested 标 auto_handled=true（前端据此不弹框）"
        );
    }

    /// 架构 §4.4.4 hands-off（force_automode）：判官 DENY 命令类（Bash）时，**不弹**
    /// PermissionRequested，直接自动拒，把拒绝 reason 作为 tool_result 回给 agent。
    /// 修前（force_automode 下 Bash 仍保留弹窗）这里会卡在审批超时，A/B 翻转可复现。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hands_off_auto_denies_command_without_prompt() {
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
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::AutoMode)),
            model_id: Some("claude-opus-4.7".to_string()),
            judge_client: Some(Arc::new(StaticDenyJudge)),
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(true)), // hands-off 开启
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };

        let call = ToolCall {
            id: "call_handsoff".into(),
            // 无界外路径的会写命令：避免先卡在 PathAccess 审批，直达 AutoMode judge。
            input: serde_json::json!({ "command": "git push --force origin main", "cwd": tmp.path() }),
            name: "Bash".into(),
        };

        // 不挂任何 surface 响应——hands-off 必须自己把命令拒掉，否则会卡审批超时。
        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("hands-off 下命令类 DENY 应自动拒，不卡审批")
            .expect("dispatch 不应返回错误");

        // 工具未执行（被拒），结果是拒绝反馈
        assert_eq!(result.len(), 1);
        assert_ne!(result[0].content, "executed", "命令不该被执行");

        // 关键：PermissionRequested 会先 emit（架构上 judge 在它之后异步判），但
        // hands-off 下被判官 DENY 自动 resolve——**无需任何人工响应** dispatch 就完成
        // （上面 timeout 没挂 surface 也跑通即证明）。同时应看到 deny 的 AutoJudged。
        let mut saw_deny = false;
        while let Ok(event) = rx.try_recv() {
            if let EventPayload::PermissionAutoJudged { decision, .. } = event.payload {
                if decision == "deny" {
                    saw_deny = true;
                }
            }
        }
        assert!(
            saw_deny,
            "应有 deny 的 PermissionAutoJudged（判官自动拒，未询问用户）"
        );
    }

    /// 回归：AutoMode 自动审批阶段点中断必须生效（架构 §4.4.4）。
    /// dispatcher 的 cancel 预先置位 → judge 的 LLM 调用应收到**真实** cancel 并返回
    /// Cancelled → dispatch 整体返回 `ModelError::Cancelled`，工具不执行、不弹审批。
    /// 修前 judge 用独立假 flag（`AtomicBool::new(false)`），收不到中断，judge 照跑完。
    /// 载体用界内 Bash 命令：界内 Edit/Write 已免判官（§4.4.3），只有命令类仍进 judge。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn automode_judge_respects_cancel_during_auto_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(DestructiveNoopTool) as Box<dyn crate::tools::Tool>
        ]));
        let hitl = Arc::new(crate::tools::hitl::HitlGate::default());
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, _rx) = tokio::sync::mpsc::channel(1024);
        let sink: crate::agent_loop::EventSink = Arc::new(move |event| {
            let _ = tx.try_send(event);
        });
        // cancel 初始 false（run_calls 入口不短路）；judge 运行中模拟用户点中断。
        let cancel = Arc::new(AtomicBool::new(false));
        let dispatcher = ToolDispatcher {
            registry,
            hitl,
            workspace,
            state: run_state,
            sink,
            cancel,
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::AutoMode)),
            model_id: Some("claude-opus-4.7".to_string()),
            judge_client: Some(Arc::new(CancelAwareJudge)),
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };

        let call = ToolCall {
            id: "call_cancel".into(),
            // 界内会写命令：judge 仍接管命令类（界内编辑已免判官）；cwd 界内避免先卡 PathAccess。
            // 用 chmod（真·会写、非 safe-write）——touch/mkdir 现归 safe-write 自动放行不进判官。
            input: serde_json::json!({ "command": "chmod 755 cancel-probe", "cwd": tmp.path() }),
            name: "Bash".into(),
        };

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&[call], 0))
            .await
            .expect("中断后应快速返回，不卡审批");

        // judge 收到 cancel → 整体返回 Cancelled；工具绝不执行。
        assert!(
            matches!(result, Err(ModelError::Cancelled)),
            "AutoMode judge 阶段中断应让 dispatch 返回 Cancelled，实际：{result:?}"
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
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
            model_id: None,
            judge_client: None,
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,

            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
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
            // 只人工批**第一条**审批（AllowAndRemember cd/grep/cat）。第二条结构相同的命令
            // 靠 remember 自动放行——worker_threads=2 并发 race 下它可能抢在 remember 前已 emit
            // PermissionRequested 并短暂 pending，但会被 resolve_matching_pending_after_remember
            // 自动 resolve，不需要人再批。验证「人工只批 1 次 + 两条都执行成功」比验证「只
            // emit 1 个事件」健壮——后者依赖 call_2 是否抢在 remember 前 check 的并发时序，本就
            // flaky（若 remember 真没生效，call_2 永挂 → dispatch 5s timeout → fail，仍抓得住）。
            let mut first_segments: Option<Vec<String>> = None;
            let mut human_approvals = 0;
            while let Some(event) = rx.recv().await {
                if let EventPayload::PermissionRequested {
                    request_id, kind, ..
                } = &event.payload
                {
                    if let PermissionKind::ToolCall {
                        command_segments, ..
                    } = kind
                    {
                        if human_approvals == 0 {
                            first_segments = Some(command_segments.clone());
                            human_approvals += 1;
                            hitl_for_surface.resolve(
                                request_id,
                                ApprovalDecision::AllowAndRemember {
                                    scope: PermissionScope::Session,
                                    pattern: Some("cd".into()),
                                    extra_patterns: vec!["grep".into(), "cat".into()],
                                },
                            );
                        }
                    }
                }
            }
            (first_segments, human_approvals)
        });

        let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.run_calls(&calls, 0))
            .await
            .expect("dispatch should complete after first approval");
        let results = result.expect("dispatch should not return errors");
        // 两条都执行成功 = 第二条靠 remember 自动放行（核心断言：remember 跨 call 生效）。
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.name == "Bash"));

        drop(dispatcher);
        let (first_segments, human_approvals) = surface.await.unwrap();
        // 人工只批 1 次——第二条同结构命令不再打扰用户。
        assert_eq!(human_approvals, 1);
        // command_segments 只含「会写可记忆」段：grep / cat 是只读段，已被过滤
        // （架构 §4.4.2）——UI 记忆勾选区不该出现它们。
        assert_eq!(first_segments.unwrap(), vec!["cd crates", "cd agent-core"]);
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
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
            model_id: None,
            judge_client: None,
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            // 关键：传 data_dir + session_id，让 short-circuit 走落盘分支
            session_id_for_hooks: Some(session_id.clone()),
            data_dir_for_artifacts: Some(data_dir.clone()),
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,

            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
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
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
            model_id: None,
            judge_client: None,
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: Some("sid-skill".to_string()),
            data_dir_for_artifacts: Some(data_dir.clone()),
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,

            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
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
    fn parse_ask_input_accepts_multi_question_shape() {
        let shape = parse_ask_input(&serde_json::json!({
            "questions": [
                {
                    "title": "选择范围",
                    "description": "决定本次改动覆盖面",
                    "options": [
                        {"label": "仅核心"},
                        {"label": "含前端", "description": "同步更新 UI"}
                    ]
                },
                {
                    "title": "是否多选",
                    "multi": true,
                    "options": [
                        {"label": "A"},
                        {"label": "B"}
                    ]
                }
            ]
        }))
        .expect("questions shape should parse");

        match shape {
            AskShape::Multi(questions) => {
                assert_eq!(questions.len(), 2);
                assert_eq!(questions[0].title, "选择范围");
                assert!(questions[1].multi);
            }
            AskShape::Single { .. } => panic!("expected multi-question shape"),
        }
    }

    #[test]
    fn parse_ask_input_accepts_legacy_single_question_shape() {
        let shape = parse_ask_input(&serde_json::json!({
            "question": "怎么处理？",
            "options": [{"label": "A"}, {"label": "B"}],
            "multi": true
        }))
        .expect("single shape should parse");

        match shape {
            AskShape::Single {
                question,
                options,
                multi,
            } => {
                assert_eq!(question, "怎么处理？");
                assert_eq!(options.len(), 2);
                assert!(multi);
            }
            AskShape::Multi(_) => panic!("expected single-question shape"),
        }
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

    /// 历史审批重放（手动跑：`cargo test -p agent-core --lib replay_historical -- --ignored --nocapture`）。
    ///
    /// 读 `~/.hebbian/sessions/*/session.jsonl` 里所有 Edit/Write 工具调用，用**生产代码**
    /// （`analyze_effects` + `Workspace::allows` + `edit_auto_allowed`）重放新决策，统计：
    /// - 新逻辑免审（界内、非 git-meta）：老逻辑这些全弹审批，现在零打扰
    /// - 仍弹·界外 / 仍弹·git-meta：保留审批（worktree 兜不住）
    /// 重点核对「仍弹·界外」是否全为真界外（无误拦），以及有无被错放的不可逆写入。
    #[test]
    #[ignore]
    fn replay_historical_edit_approvals() {
        use crate::run_mode::RunMode;
        use std::io::{BufRead, BufReader};
        use std::path::PathBuf;

        let home = std::env::var("HOME").expect("HOME");
        let sessions_dir = PathBuf::from(&home).join(".hebbian/sessions");
        let entries = std::fs::read_dir(&sessions_dir).expect("read sessions dir");

        let mut total_edits = 0usize;
        let mut auto_allowed = 0usize;
        let mut still_prompt_out_of_bounds = 0usize;
        let mut still_prompt_git_meta = 0usize;
        let mut no_path = 0usize;
        let mut out_samples: Vec<String> = Vec::new();
        let mut gitmeta_samples: Vec<String> = Vec::new();
        // 反向核对：被免审的路径若看起来在 .hebbian / 系统目录则可疑（潜在误放）
        let mut suspicious_allowed: Vec<String> = Vec::new();

        for entry in entries.flatten() {
            let sdir = entry.path();
            let jsonl = sdir.join("session.jsonl");
            if !jsonl.exists() {
                continue;
            }
            // workdir 从 meta 行（首行 type=meta）拿；allowed_paths 用空（最严格——
            // 只有 workdir 内算界内，比生产更保守，宁可多算「仍弹」也不误判免审）。
            let file = match std::fs::File::open(&jsonl) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let reader = BufReader::new(file);
            let mut workdir: Option<PathBuf> = None;
            let mut workspace: Option<std::sync::Arc<Workspace>> = None;

            for line in reader.lines().map_while(Result::ok) {
                let v: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if ty == "meta" {
                    if let Some(wd) = v.get("workdir").and_then(|w| w.as_str()) {
                        let wd = PathBuf::from(wd);
                        workspace = Some(Workspace::new(wd.clone(), Vec::new()));
                        workdir = Some(wd);
                    }
                    continue;
                }
                if ty != "message" {
                    continue;
                }
                let Some(tool_calls) = v.get("tool_calls").and_then(|t| t.as_array()) else {
                    continue;
                };
                for tc in tool_calls {
                    let name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    if !matches!(name, "Edit" | "Write") {
                        continue;
                    }
                    total_edits += 1;
                    let input = tc.get("input").cloned().unwrap_or(serde_json::Value::Null);
                    let fp = input.get("file_path").and_then(|p| p.as_str());
                    let Some(fp) = fp else {
                        no_path += 1;
                        continue;
                    };
                    // 真实生产判定：effects → paths_in_bounds → git_meta → edit_auto_allowed
                    let effects = crate::effects::analyze_effects(name, &input);
                    let ws = match &workspace {
                        Some(w) => w.clone(),
                        // 无 workdir 的老 session：用 file_path 父目录无从判界内，跳过
                        None => {
                            no_path += 1;
                            continue;
                        }
                    };
                    let in_bounds = effects.paths.iter().all(|p| ws.allows(p));
                    let git_meta = effects
                        .paths
                        .iter()
                        .any(|p| crate::tools::shell_parse::is_git_meta_path(&p.to_string_lossy()));
                    let allowed = edit_auto_allowed(name, RunMode::Default, in_bounds, git_meta);
                    if allowed {
                        auto_allowed += 1;
                        // 误放体检：免审路径不该落在 ~/.hebbian 配置区 / ~/.ssh / /etc
                        let lossy = fp.to_string();
                        if lossy.contains("/.hebbian/")
                            || lossy.contains("/.ssh/")
                            || lossy.starts_with("/etc/")
                        {
                            if suspicious_allowed.len() < 20 {
                                suspicious_allowed.push(format!(
                                    "{} (workdir={})",
                                    lossy,
                                    workdir
                                        .as_ref()
                                        .map(|w| w.display().to_string())
                                        .unwrap_or_default()
                                ));
                            }
                        }
                    } else if git_meta {
                        still_prompt_git_meta += 1;
                        if gitmeta_samples.len() < 20 {
                            gitmeta_samples.push(fp.to_string());
                        }
                    } else {
                        still_prompt_out_of_bounds += 1;
                        if out_samples.len() < 30 {
                            out_samples.push(format!(
                                "{} (workdir={})",
                                fp,
                                workdir
                                    .as_ref()
                                    .map(|w| w.display().to_string())
                                    .unwrap_or_default()
                            ));
                        }
                    }
                }
            }
        }

        println!("\n===== 历史 Edit/Write 审批重放（生产代码，Default 模式）=====");
        println!("总 Edit/Write 调用: {total_edits}");
        println!("  ✅ 新逻辑免审（界内非 git-meta）: {auto_allowed}  ← 老逻辑这些全弹审批");
        println!("  ⚠️ 仍弹·界外:      {still_prompt_out_of_bounds}");
        println!("  ⚠️ 仍弹·git 元数据: {still_prompt_git_meta}");
        println!("  ·  无 workdir/路径跳过: {no_path}");
        println!("\n--- 仍弹·界外 样本（核对是否全为真界外，无误拦）---");
        for s in &out_samples {
            println!("  {s}");
        }
        if !gitmeta_samples.is_empty() {
            println!("\n--- 仍弹·git 元数据 样本 ---");
            for s in &gitmeta_samples {
                println!("  {s}");
            }
        }
        println!("\n--- 误放体检：免审路径落在 .hebbian/.ssh/etc 的（应为空）---");
        if suspicious_allowed.is_empty() {
            println!("  （空，无可疑误放）");
        } else {
            for s in &suspicious_allowed {
                println!("  ⚠️ {s}");
            }
        }
        // 硬断言：不能有任何免审路径落在敏感配置区
        assert!(
            suspicious_allowed.is_empty(),
            "发现 {} 条免审路径落在 .hebbian/.ssh/etc，策略有漏洞",
            suspicious_allowed.len()
        );
    }

    /// 会 sleep 的 Bash 桩工具：name="Bash"，execute 里 sleep 给定毫秒。
    /// 用于验证同批多个 Bash 是否真并发——并发则总耗时 ≈ 单次，串行则 ≈ N 倍。
    struct SleepyBashTool {
        sleep_ms: u64,
    }

    #[async_trait]
    impl crate::tools::Tool for SleepyBashTool {
        fn name(&self) -> &str {
            "Bash"
        }

        fn description(&self) -> &str {
            "sleepy test bash"
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _input: Value) -> AppResult<String> {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            Ok("slept".to_string())
        }
    }

    /// 架构 §4.4.3 / §4.13.4：同一 ToolStep 内的多个 Bash 必须并发执行。
    ///
    /// **必须挂真实 `edits_worktree`** 才能复现真凶——根因是 effects.paths 无条件含
    /// cwd，dispatch 执行前对每个 allows 的 path 拿 edits-worktree per-path 锁且持有到
    /// execute 结束；两个 Bash 都触达同一 workdir(cwd) → 抢同一把锁 → 串行。
    /// `edits_worktree=None` 的版本测不出此 bug（会假并发）。
    ///
    /// 复现：两个各 sleep 300ms 的 Bash，并发总耗时应 < 500ms，串行则 ≈ 600ms。
    /// 修前（cwd 进快照锁循环）此断言必 fail。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multiple_bash_calls_run_concurrently() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let edits_wt = Arc::new(crate::edits::EditsWorktree::new(
            data_dir.path(),
            "sid-concurrent",
            &workspace,
        ));
        if !edits_wt.enabled().await {
            eprintln!("git 不可用，跳过 edits-worktree 并发测试");
            return;
        }
        let run_id = "run-concurrent";
        edits_wt.begin_run(run_id).await;

        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(SleepyBashTool { sleep_ms: 300 }) as Box<dyn crate::tools::Tool>,
        ]));
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, _rx) = tokio::sync::mpsc::channel(1024);
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
            // Yolo：界内非危险命令自动放行，不卡审批，纯测执行并发性。
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Yolo)),
            model_id: None,
            judge_client: None,
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: Some(edits_wt),
            current_run_id: Some(run_id.to_string()),
            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };

        let calls = vec![
            ToolCall {
                id: "bash_a".into(),
                name: "Bash".into(),
                input: serde_json::json!({ "command": "echo a" }),
            },
            ToolCall {
                id: "bash_b".into(),
                name: "Bash".into(),
                input: serde_json::json!({ "command": "echo b" }),
            },
        ];

        let started = Instant::now();
        let results = dispatcher
            .run_calls(&calls, 0)
            .await
            .expect("dispatch 不应报错");
        let elapsed = started.elapsed();

        assert_eq!(results.len(), 2);
        assert!(
            elapsed < Duration::from_millis(500),
            "两个各 300ms 的 Bash 并发总耗时应 < 500ms，实测 {elapsed:?}——\
             说明 edits-worktree cwd 锁把它们串行化了"
        );
    }

    /// 回归（判官超时兜底，架构 §4.4.4）：AutoMode 下判官 LLM 迟迟不返回（provider
    /// 抖动 / DeepSeek 同 chat session 并发挂起）时，dispatch 必须在判官超时后降级
    /// Ask、emit `PermissionAutoJudged { requires_human: true }` 把审批转人工，而不是
    /// 让工具卡在判官 LLM 上无限黄呼吸、run 永不前进。
    ///
    /// A/B：修前（判官两次 LLM await 裸调、无 wall-clock 超时）→ run_calls 一直等
    /// `HangingJudge`（hang），外层 6s 超时触发、测试 fail。修后（judge_decision_timeout
    /// 兜底，test 档 3s）→ 判官超时转人工 → surface 批准 → 工具执行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn judge_timeout_falls_back_to_human_instead_of_hanging() {
        use protocol::EventPayload;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path(), Vec::new());
        let registry = Arc::new(ToolRegistry::new(vec![
            Box::new(DestructiveNoopTool) as Box<dyn crate::tools::Tool>,
        ]));
        let hitl = Arc::new(crate::tools::hitl::HitlGate::default());
        let run_state = Arc::new(RunState::new(RunId::new()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let sink: crate::agent_loop::EventSink = Arc::new(move |event| {
            let _ = tx.try_send(event);
        });
        // surface：判官超时转人工后，收到 requires_human=true 的 AutoJudged → 批准放行。
        // 修前判官永不出 AutoJudged，这个 resolve 永不触发，run_calls 卡到外层超时。
        let hitl_for_surface = hitl.clone();
        let surface = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let EventPayload::PermissionAutoJudged {
                    request_id: Some(rid),
                    requires_human: true,
                    ..
                } = &event.payload
                {
                    hitl_for_surface.resolve(rid, ApprovalDecision::AllowOnce);
                    break;
                }
            }
        });
        let dispatcher = ToolDispatcher {
            registry,
            hitl,
            workspace,
            state: run_state,
            sink,
            cancel: Arc::new(AtomicBool::new(false)),
            run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::AutoMode)),
            model_id: Some("claude-opus-4.7".to_string()),
            judge_client: Some(Arc::new(HangingJudge)),
            force_automode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hooks: Arc::new(crate::hooks::HookManager::empty()),
            session_id_for_hooks: None,
            data_dir_for_artifacts: None,
            permission_store: None,
            edits_worktree: None,
            current_run_id: None,
            subagent_ctx: None,
            parent_transcript_snapshot: None,
            model_io_dump: None,
            subagent_bypass: false,
        };

        let call = ToolCall {
            id: "call_judge_timeout".into(),
            // 界内会写命令：走 AutoMode judge（界内编辑已免判官，只命令类进判官）；
            // cwd 界内避免先卡 PathAccess。
            input: serde_json::json!({ "command": "git push --force origin main", "cwd": tmp.path() }),
            name: "Bash".into(),
        };

        // 判官超时(test=3s) + resolve + 执行应在 6s 内；修前判官 hang(sleep 30s) → run_calls
        // 卡到外层 6s 超时 fail。超时阈值远大于 mock judge 正常耗时，不会误触发误降级。
        let result = tokio::time::timeout(Duration::from_secs(6), dispatcher.run_calls(&[call], 0))
            .await
            .expect("判官超时应转人工放行，run_calls 不该卡在判官 LLM 上")
            .expect("dispatch 不应返回错误");

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].content, "executed",
            "判官超时转人工、用户批准后工具应执行"
        );
        surface.await.unwrap();
    }
}
