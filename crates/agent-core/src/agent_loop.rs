use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Instant;

use observability::attr;
use protocol::{AgentRef, ErrorReport, Event, EventPayload, LogLevel, RunId, StopReason};
use tracing::{debug, field::Empty, info, Instrument};

use crate::{
    context::{
        budget,
        compaction::{build_compaction_request, needs_compaction},
        microcompact::{microcompact, MicrocompactPolicy},
        tool_xml_leak::sanitize_tool_xml_leak,
        transcript::Transcript,
    },
    definition::CompactionPolicy,
    dispatch::ToolDispatcher,
    hooks::{HookManager, HookOutcome, HookPoint},
    model_io_dump::{self, DumpEntry, ModelIoDump},
    run_state::RunState,
    system_prompt::compose_system_prompt,
    tools::{
        ask_only_definitions, hitl::HitlGate, hosted_tool_definitions, registry::ToolRegistry,
        BUILTIN_TOOL_NAMES, CONDITIONAL_TOOL_NAMES,
    },
    workspace::Workspace,
};
use common::{
    runtime::{self as cancellation, ConsumedPendingInputs, PendingInputs, PendingUserInput},
    CancelFlag,
};
use model_gateway::{
    client::ModelClient,
    types::{
        AssistantOutput, FinishReason, ModelError, ModelRequest, ModelResponse, ModelStreamEvent,
    },
};
use protocol::ResumeCause;

/// Stop hook 在一个 Run 内最多注入多少次 reminder（架构 §4.8.3）。超过即放弃注入正常出 turn。
/// 防 cargo check 永远修不好把 loop 跑爆。
const MAX_STOP_INJECTIONS: u32 = 3;

/// 工具调用 XML 漏进正文（架构 §4.3.3）的自愈续跑上限。命中残骸时清洗 + 注入纠错
/// 提示续跑一次；连续 N 次仍漏说明模型这轮陷得深，停止续跑让残骸照常收尾、交回用户。
const MAX_TOOL_XML_LEAK_RECOVERIES: u32 = 2;

/// 单次 ModelStep 非正常退出后的自动重试上限（架构 §4.3）。指数退避，每次 emit toast。
/// 与 model-gateway 的 `retry_request`（包初始 HTTP 发送的快速瞬时重试）正交：这一层
/// 是「整轮模型调用」的用户可见重试，覆盖 SSE 流内 error / 上游 overloaded 等场景。
const MAX_MODEL_RETRIES: u32 = 5;

/// 第 `attempt`（从 1 起）次重试前的退避时长：1s / 2s / 4s / 8s / 16s 封顶。
/// 一次模型请求完成后的统一记账，三件事一起做：
/// 1. 打 cache 日志到专属 `cache` target（info 级，各 surface `cache=info` 白名单放行，
///    每次请求命中情况「始终可见、可一键 `grep cache`」——和 memory/permission 同套路）；
/// 2. emit `Usage` 事件让前端实时刷新 cache 指示器（不必等整个 run 结束）；
/// 3. per-turn 落盘到 session.token_stats（run 进行中就累加，崩溃/取消也保住已扣费部分）。
fn record_request_usage<F: Fn(EventPayload)>(
    usage: &model_gateway::types::Usage,
    estimated_tokens: u64,
    emit: &F,
    data_dir: Option<&std::path::Path>,
    session_id: Option<&str>,
) {
    let hit_pct = if usage.input_tokens > 0 {
        usage.cache_read_tokens * 100 / usage.input_tokens
    } else {
        0
    };
    info!(
        target: "cache",
        input = usage.input_tokens,
        cache_read = usage.cache_read_tokens,
        cache_write = usage.cache_creation_tokens,
        hit_pct,
        "[Cache] 命中 {hit_pct}% · read {} / input {} · write {}",
        usage.cache_read_tokens,
        usage.input_tokens,
        usage.cache_creation_tokens,
    );
    emit(EventPayload::Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
    });
    if let (Some(dd), Some(sid)) = (data_dir, session_id) {
        crate::storage::sessions::bump_token_stats(
            dd,
            sid,
            crate::storage::sessions::TokenStats {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                cache_creation_tokens: usage.cache_creation_tokens,
                run_count: 1,
                last_estimated_tokens: estimated_tokens,
                ..Default::default()
            },
        );
    }
}

fn model_retry_delay(attempt: u32) -> std::time::Duration {
    let secs = 1u64 << attempt.saturating_sub(1).min(4);
    std::time::Duration::from_secs(secs)
}

/// 可取消的退避：每 200ms 检查一次 cancel。返回 `false` 表示中途被取消（应立即放弃重试）。
async fn backoff_or_cancel(delay: std::time::Duration, cancel: &CancelFlag) -> bool {
    let step = std::time::Duration::from_millis(200);
    let mut slept = std::time::Duration::ZERO;
    while slept < delay {
        if cancellation::is_cancelled(cancel) {
            return false;
        }
        let chunk = step.min(delay - slept);
        tokio::time::sleep(chunk).await;
        slept += chunk;
    }
    !cancellation::is_cancelled(cancel)
}

/// 这个模型错误值不值得自动重试（架构 §4.3）。取消 / 挂起不是错误；JSON 解析失败
/// 重试也修不好；其余（流内 error、上游 overloaded、网络断、5xx/429）属瞬时，可重试。
fn is_retryable_model_error(e: &ModelError) -> bool {
    match e {
        ModelError::Cancelled | ModelError::Suspended => false,
        ModelError::Json(_) => false,
        ModelError::Http { status, .. } => *status == 429 || *status >= 500,
        ModelError::Request(_) | ModelError::Other(_) => true,
    }
}

/// 把 run 收尾态归一成「续作入口」（架构 §4.3）。`None` = 正常完成：不弹 toast、
/// 清空残留 pending_continue。返回 `Some((kind, 给用户看的一句话))` 时 surface 弹
/// toast 并落 pending_continue，让输入框上方的 ContinueBar 重启后仍可见。
fn continue_for_outcome(
    result: &Result<AssistantOutput, ModelError>,
    last_finish: &FinishReason,
) -> Option<(crate::storage::sessions::ContinueKind, String)> {
    use crate::storage::sessions::ContinueKind;
    match result {
        Ok(_) => match last_finish {
            FinishReason::Stop => None,
            FinishReason::Length => Some((
                ContinueKind::Truncated,
                "回答被长度上限截断了，点「继续」让模型接着写".to_string(),
            )),
            FinishReason::Refusal => {
                Some((ContinueKind::Refused, "模型这次拒绝了回答".to_string()))
            }
            FinishReason::ContentFilter => Some((
                ContinueKind::Filtered,
                "这次内容被安全策略拦了下来".to_string(),
            )),
            FinishReason::Other(s) => Some((
                ContinueKind::Other,
                format!("模型异常结束（{s}），点「继续」再试一次"),
            )),
        },
        // 用户主动取消 / 挂起等唤醒——不算异常，不留续作入口。
        Err(ModelError::Cancelled) | Err(ModelError::Suspended) => None,
        Err(e) => Some((
            ContinueKind::NetworkError,
            format!("这次请求没成功（{e}），点「继续」重试"),
        )),
    }
}

/// Run 从挂起态恢复时携带的初始状态（架构 §4.12.6）。Harness 在 spawn_run 时
/// 把它放进 [`LoopParams`]——agent_loop 入口据此恢复计数器，并 emit
/// `RunResumed { cause }` 而不是 `RunStarted`。
#[derive(Debug, Clone)]
pub struct RunResumeState {
    pub cause: ResumeCause,
    pub iteration: u32,
    pub model_step_index: u32,
    pub tool_step_index: u32,
    pub tool_call_dispatch_offset: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
}

impl RunResumeState {
    /// 把磁盘上的 [`crate::storage::run_checkpoint::RunCheckpoint`] 还原成
    /// agent_loop 启动时初始化用的状态（架构 §4.12.6）。`cause` 决定 surface
    /// 看到的 `RunResumed { cause }` 标签。
    pub fn from_checkpoint(
        ckpt: crate::storage::run_checkpoint::RunCheckpoint,
        cause: ResumeCause,
    ) -> Self {
        Self {
            cause,
            iteration: ckpt.iteration,
            model_step_index: ckpt.model_step_index,
            tool_step_index: ckpt.tool_step_index,
            tool_call_dispatch_offset: ckpt.tool_call_dispatch_offset,
            total_input_tokens: ckpt.total_input_tokens,
            total_output_tokens: ckpt.total_output_tokens,
            total_cache_read_tokens: ckpt.total_cache_read_tokens,
            total_cache_creation_tokens: ckpt.total_cache_creation_tokens,
        }
    }
}

/// 运行 agent loop 的入参集合
pub struct LoopParams<'a> {
    pub client: &'a dyn ModelClient,
    pub registry: Arc<ToolRegistry>,
    pub hitl: Arc<HitlGate>,
    pub hooks: Arc<HookManager>,
    pub transcript: &'a mut Transcript,
    pub enabled_tools: &'a [String],
    pub compaction_policy: &'a CompactionPolicy,
    pub workspace: Arc<Workspace>,
    pub stream: bool,
    pub cancel: CancelFlag,
    pub state: Arc<RunState>,
    pub agent: AgentRef,
    pub parent: Option<RunId>,
    /// 可选的模型 IO dump：每轮 `client.complete/stream` 前后写一条 jsonl。
    pub model_io_dump: Option<ModelIoDump>,
    /// 运行时输入注入队列：surface 在 streaming 中「立即发送」时推一条进来，
    /// 每次 model.request 之前 drain 出来作为新的 user message 加入 transcript。
    pub pending_inputs: Option<PendingInputs>,
    /// 已被 drain 的 pending input 副本。surface 用它在 run 结束后按正确顺序落盘。
    pub consumed_pending_inputs: Option<ConsumedPendingInputs>,
    /// run 到达 terminal/suspended 后关闭，让 surface 侧的 late inject 能回落到新 run。
    pub pending_inputs_accepting: Option<Arc<AtomicBool>>,
    /// 运行模式（架构 §4.4.3）。共享引用：surface 在 run 期间切换 mode 后，
    /// 下一次 dispatch 立即读到新值。
    pub run_mode: crate::run_mode::SharedRunMode,
    /// 当前模型 id（AutoMode judge 用作模型限定）。
    pub model_id: Option<String>,
    /// AutoMode judge 用的 client。通常 = 主 client，便于复用 OAuth/重试链。
    /// `None` 时 AutoMode 直接降级为 Ask。
    pub judge_client: Option<Arc<dyn ModelClient>>,
    /// `force_automode`（hands-off「全自动」）子开关（架构 §4.4.4）。仅
    /// [`crate::run_mode::RunMode::AutoMode`] 下生效：判官 `Ask` 折叠成 `Deny`、命令类
    /// `Deny` 也自动拒不弹。**共享句柄**，surface 改后 run 中途即生效。
    pub force_automode: crate::run_mode::SharedForceAutomode,
    /// 数据目录路径，用于把 microcompact 压缩的原文落 txt（架构 §4.7 / Step 9）。
    pub data_dir: Option<std::path::PathBuf>,
    /// 会话 id（格式 `{yyyymmddHHmm}-{shortUuid}`）。与 `data_dir` 拼成
    /// `<data_dir>/sessions/<sid>/tool_results/<call_id>.txt`。
    pub session_id: Option<String>,
    /// 工具与 agent_loop 之间共享的"挂起请求"槽（架构 §4.12.4）。ScheduleWakeup
    /// 调用时把 `RunPhase` 写进来；agent_loop 在每次 ToolStep 完成后
    /// 取出处理：emit RunSuspended → 落 RunCheckpoint → 注册到 WakeupScheduler → return。
    pub phase: Option<crate::wakeup::PhaseChannel>,
    /// 从挂起态恢复时由 Harness 注入：agent_loop 据此恢复计数器并 emit
    /// `RunResumed { cause }` 而不是 `RunStarted`（架构 §4.12.6）。`None` 表示
    /// 普通新起 Run。
    pub resume_from: Option<RunResumeState>,
    /// Edit 工具快照仓库（架构 §4.13）。`None` 时跳过快照。
    pub edits_worktree: Option<Arc<crate::edits::EditsWorktree>>,
    /// 工具迭代次数上限。`None` 表示不限制（主 agent 默认行为）；
    /// `Some(n)` 在达到 n 次后中断循环（subagent 场景可按需启用）。
    pub max_tool_iterations: Option<u32>,
    /// 规则文件渲染后的 `<system-reminder>` 块，追加到 system prompt 末尾。
    pub system_rules: Option<String>,
    /// Subagent / NestedRun 上下文（架构 §4.4.11）。`Some` → ToolDispatcher 可以
    /// 路由 `Task` 工具到 [`crate::subagent::SubagentRunner`]；`None` → Task 调用
    /// 落到兜底错误（CLI 单跑 / 单测路径）。
    pub subagent_ctx: Option<Arc<crate::subagent::SubagentCtx>>,
    /// 子 NestedRun 的 `permission=Bypass`（架构 §4.4.11.4）：子在 tools 白名单内自主放行、
    /// 不弹审批，仅危险红线仍拦。父 Run 恒 `false`；由 [`crate::subagent::SubagentRunner`]
    /// 按 `def.permission` 计算后传入。
    pub subagent_bypass: bool,
    /// Run 落盘协调器（架构 §4.9.5）。`Some` 时 agent_loop 在段边界 / drain 边界 / run
    /// 收尾把 assistant 段 + 插队 user 单点串行 append 到 session.jsonl；`None` 时全跳过
    /// （CLI 单跑 / subagent / 单测路径不落盘）。由 Harness::spawn_run 用 data_dir +
    /// session_id 构造后塞入。
    pub persister: Option<crate::run_persister::RunPersister>,
    /// 本 run 的模型调用 tag（架构 §4.11）。主对话 = Main；aside / subagent / nested run 由
    /// 创建方显式传入，让 `[model]` 日志 + model_io 落盘据此区分（替代已停用的 model_io
    /// main_kind 推断）。
    pub call_tag: model_gateway::types::ModelCallTag,
}

/// 把 [`compose_system_prompt`] 重新导出为旧名字，方便其它 crate 沿用。
/// 内部已经不再混入 workspace XML——环境信息走第一条 user message 的 `<environment>` 块。
pub use crate::system_prompt::compose_system_prompt as build_system_prompt;

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

async fn commit_run_edits(
    edits_worktree: &Option<Arc<crate::edits::EditsWorktree>>,
    state: &Arc<RunState>,
    sink: &EventSink,
    run_id: &str,
) {
    let Some(wt) = edits_worktree.as_ref() else {
        return;
    };
    match wt.finalize_run(run_id).await {
        Ok(Some(entry)) => {
            let files = entry.files.into_iter().map(Into::into).collect();
            sink(state.event(EventPayload::RunEditsCommitted {
                run_id: state.run_id.clone(),
                files,
            }));
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, "finalize run edits failed");
            sink(state.event(EventPayload::Notice {
                level: LogLevel::Warn,
                message: format!("保存本轮文件修改记录失败：{e}"),
                dedup_key: None,
            }));
        }
    }
}

fn has_pending(pending_inputs: Option<&PendingInputs>) -> bool {
    pending_inputs
        .map(|slot| !slot.lock().unwrap().is_empty())
        .unwrap_or(false)
}

fn drain_pending_inputs(
    pending_inputs: Option<&PendingInputs>,
    consumed_pending_inputs: Option<&ConsumedPendingInputs>,
    persister: Option<&crate::run_persister::RunPersister>,
    transcript: &mut Transcript,
) -> usize {
    let Some(slot) = pending_inputs else {
        return 0;
    };
    let drained: Vec<PendingUserInput> = std::mem::take(&mut *slot.lock().unwrap());
    let drained_len = drained.len();
    if drained_len == 0 {
        return 0;
    }
    for input in drained {
        if let Some(consumed) = consumed_pending_inputs {
            consumed.lock().unwrap().push(input.clone());
        }
        // 插队 user 落盘收归 agent_core（架构 §4.9.5）：给定 persister 时由这里单点 append，
        // surface 端不再即写即落（双落防护）。
        if let Some(p) = persister {
            p.append_user(input.content.clone(), input.attachments.clone(), None);
        }
        transcript.push_user(input.content, input.attachments);
    }
    drained_len
}

fn set_pending_inputs_accepting(flag: Option<&Arc<AtomicBool>>, accepting: bool) {
    if let Some(flag) = flag {
        flag.store(accepting, Ordering::SeqCst);
    }
}

/// 把一次 `//goal` 裁决结果作为 `Role::Marker` append 到 session.jsonl（架构 §4.8.3）。
/// 随会话持久化、重启可重建渲染；transcript rebuild 跳过 Marker，模型看不到。
/// 落盘失败仅 warn，不阻断 goal 续跑 / 出 turn。
fn append_goal_outcome_marker(
    data_dir: &std::path::Path,
    session_id: &str,
    kind: &str,
    condition: &str,
    reason: &str,
    iteration: u32,
) {
    use crate::storage::sessions::{self as sess_store, Message, MessageMeta, Role};
    let marker = Message {
        id: sess_store::new_id(),
        role: Role::Marker,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: Some(MessageMeta::GoalOutcome {
            kind: kind.to_string(),
            condition: condition.to_string(),
            reason: reason.to_string(),
            iteration,
        }),
        subagent_call_id: None,
        run_duration_ms: None,
    };
    if let Err(e) = sess_store::append_message(data_dir, session_id, marker) {
        tracing::warn!(error = %e, kind, "goal 裁决 marker 落盘失败，仅事件态可见");
    }
}

/// 把一次 Stop hook 执行结果作为 `Role::Marker` append 到 session.jsonl（架构 §4.8.3）。
/// 让消息流显示「跑了哪个 verify、过没过」，与裁决 marker 同走串行落盘流。
fn append_hook_outcome_marker(
    data_dir: &std::path::Path,
    session_id: &str,
    event: &str,
    status: &str,
    detail: &str,
) {
    use crate::storage::sessions::{self as sess_store, Message, MessageMeta, Role};
    let marker = Message {
        id: sess_store::new_id(),
        role: Role::Marker,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: Some(MessageMeta::HookOutcome {
            event: event.to_string(),
            status: status.to_string(),
            detail: detail.to_string(),
        }),
        subagent_call_id: None,
        run_duration_ms: None,
    };
    if let Err(e) = sess_store::append_message(data_dir, session_id, marker) {
        tracing::warn!(error = %e, status, "Stop hook marker 落盘失败，仅事件态可见");
    }
}

/// 「目标已设」marker（架构 §4.8.3）：用户刚设目标时 set_active_goal 置了
/// pending_set_marker。在触发它的 `Goal set` user 消息已落盘后（run 启动 / 插队 drain）
/// 落一条 set marker（物理排在该 user 消息之后），并清标志避免重复落。两条 user 落盘
/// 路径（首条开新 run / 插队进当前 run）都调一次，set marker 始终紧跟它的 user 消息。
fn maybe_emit_pending_set_marker(data_dir: Option<&std::path::Path>, session_id: Option<&str>) {
    let (Some(dd), Some(sid)) = (data_dir, session_id) else {
        return;
    };
    let Ok(Some(goal)) = crate::storage::sessions::load(dd, sid).map(|s| s.active_goal) else {
        return;
    };
    if !goal.pending_set_marker {
        return;
    }
    append_goal_outcome_marker(dd, sid, "set", &goal.condition, "", 0);
    let cleared = crate::storage::sessions::ActiveGoal {
        pending_set_marker: false,
        ..goal
    };
    if let Err(e) = crate::storage::sessions::set_active_goal(dd, sid, Some(cleared)) {
        tracing::warn!(error = %e, "清 pending_set_marker 失败");
    }
}

#[tracing::instrument(
    name = "run",
    level = "info",
    skip_all,
    fields(
        hebbian.run.id = %params.state.run_id,
        hebbian.agent.id = %params.agent,
        hebbian.run.parent_id = ?params.parent,
        hebbian.run.outcome = Empty,
        hebbian.run.iterations = Empty,
    )
)]
pub async fn run_loop(
    params: LoopParams<'_>,
    on_event: EventSink,
) -> Result<AssistantOutput, ModelError> {
    let LoopParams {
        client,
        registry,
        hitl,
        hooks,
        transcript,
        enabled_tools,
        compaction_policy,
        workspace,
        stream,
        cancel,
        state,
        agent,
        parent,
        model_io_dump,
        pending_inputs,
        consumed_pending_inputs,
        pending_inputs_accepting,
        run_mode,
        model_id,
        judge_client,
        force_automode,
        data_dir,
        session_id,
        phase,
        resume_from,
        edits_worktree,
        max_tool_iterations,
        system_rules,
        subagent_ctx,
        subagent_bypass,
        persister,
        call_tag,
    } = params;

    let emit = |payload: EventPayload| on_event(state.event(payload));
    let run_span = tracing::Span::current();

    // Edits 跟踪（架构 §4.13）：以 Run 为单位。subagent（parent.is_some()）的文件改动
    // 归属父 Run——共用 parent_run_id 累积进父的 active run，子 loop 不 begin/finalize
    // （否则会覆盖父的单槽 active run）。顶层 Run 才负责 begin/finalize。
    let is_nested_run = parent.is_some();
    let edits_run_id = parent
        .clone()
        .unwrap_or_else(|| state.run_id.clone())
        .to_string();

    // 入口：resume_from 给定时 emit `RunResumed`（架构 §4.12.6），否则 `RunStarted`。
    // 计数器从 checkpoint 起步，保证 max_tool_iterations 累积、Step index 单调。
    let (
        mut iteration,
        mut tool_call_dispatch_offset,
        mut model_step_index,
        mut tool_step_index,
        mut total_input_tokens,
        mut total_output_tokens,
        mut total_cache_read_tokens,
        mut total_cache_creation_tokens,
    ) = if let Some(ref rs) = resume_from {
        info!(
            target: "run",
            session_id = session_id.as_deref().unwrap_or("-"),
            run_id = %state.run_id,
            cause = ?rs.cause,
            "[Run:Resumed] run 从 checkpoint 恢复，继续向 surface 发事件流"
        );
        emit(EventPayload::RunResumed {
            cause: rs.cause.clone(),
        });
        (
            rs.iteration,
            rs.tool_call_dispatch_offset,
            rs.model_step_index,
            rs.tool_step_index,
            rs.total_input_tokens,
            rs.total_output_tokens,
            rs.total_cache_read_tokens,
            rs.total_cache_creation_tokens,
        )
    } else {
        info!(
            target: "run",
            session_id = session_id.as_deref().unwrap_or("-"),
            run_id = %state.run_id,
            model = model_id.as_deref().unwrap_or("-"),
            "[Run:Started] run 开始，向 surface 发事件流"
        );
        emit(EventPayload::RunStarted {
            agent: agent.clone(),
            parent,
        });
        (0u32, 0usize, 0u32, 0u32, 0u64, 0u64, 0u64, 0u64)
    };

    // resume 成功进入 loop 之前清除 checkpoint，避免重复 resume 同一份。
    if resume_from.is_some() {
        if let (Some(dd), Some(sid)) = (data_dir.as_ref(), session_id.as_deref()) {
            if let Err(e) = crate::storage::run_checkpoint::delete(dd, sid) {
                tracing::warn!(error = %e, "resume: delete checkpoint failed");
            }
        }
    }

    // 「目标已设」marker：首条 user（开新 run）已落盘，落 set marker（若有 pending）。
    if resume_from.is_none() {
        maybe_emit_pending_set_marker(data_dir.as_deref(), session_id.as_deref());
    }

    // 估算校准样本：最近一次请求的服务端真值 input_tokens 与其配对的本地估算。
    // 启动时从持久化 token_stats 播种——已加载的长会话首轮就能用上次真值校准，
    // 不必等本会话采到第一个样本（否则恢复一个已逼近上限的会话，首请求仍会 400）。
    // 每轮请求后用本轮新样本覆盖，供下一轮 needs_compaction 使用。
    let (mut calib_real, mut calib_estimated) = match (data_dir.as_deref(), session_id.as_deref()) {
        (Some(dd), Some(sid)) => crate::storage::sessions::load_token_stats(dd, sid)
            .map(|s| (s.last_input_tokens, s.last_estimated_tokens))
            .unwrap_or((0, 0)),
        _ => (0, 0),
    };

    // 登记本 Run 的 edits 跟踪（架构 §4.13）：整个 agent_loop 生命周期内（含插队、
    // 多 turn、resume）触达的文件，Run 结束时统一对比净变化。resume 用同一 run_id 续跑。
    // 嵌套子 Run 不另开 active run（归属父 Run）。
    if !is_nested_run {
        if let Some(wt) = edits_worktree.as_ref() {
            wt.begin_run(&edits_run_id).await;
        }
    }

    let run_start = Instant::now();
    let mut output_attachments = Vec::new();
    // Stop hook 已经在本 Run 注入了几次（架构 §4.8.3 防死循环），上限
    // `MAX_STOP_INJECTIONS` 后即使脚本继续 inject 也忽略，turn 正常出。
    let mut stop_hook_injections: u32 = 0;
    // goal 续跑次数。与 stop_hook_injections 解耦——goal 是「不达目标不停」，
    // 无上限（架构 §4.8.3）；防失控靠 judge 判 impossible / turn 出错 / cancel 三道熔断。
    let mut goal_iterations: u32 = 0;
    // 工具调用 XML 漏进正文（架构 §4.3.3）的自愈续跑次数。上限到了就不再续跑、
    // 让残骸文本照常收尾（surface 会弹「继续」让用户接管），防模型一直抽风把 loop 跑爆。
    let mut tool_xml_leak_recoveries: u32 = 0;
    // 最后一个 ModelStep 的归一结束原因（架构 §4.11.4）。run 正常收尾后据此判断
    // 是否要在 surface 弹 toast + 写 pending_continue（架构 §4.3）。
    let mut last_finish = FinishReason::Stop;

    let result: Result<AssistantOutput, ModelError> = loop {
        if cancellation::is_cancelled(&cancel) {
            // Stop 时如果有排队待注入的 user message，先 drain 进 transcript + 落盘再 cancel——
            // 否则 calling surface 侧的 pending_inputs.clear() 会把它们丢掉。drain 后的消息
            // 留在 transcript 里（route §7.8.6 的 consumed_pending_inputs 由 surface 读取），
            // surface 据此判断 cancel 后是否立刻起新 run 处理这些插队输入。
            drain_pending_inputs(
                pending_inputs.as_ref(),
                consumed_pending_inputs.as_ref(),
                persister.as_ref(),
                transcript,
            );
            debug!("run cancelled");
            hitl.cancel_all_pending();
            // 架构 §4.8.1 修订：cancel 走 Notification(level="cancel")，
            // 不再占用 Stop 点位（Stop 现表示"turn 自然结束 + 后置 verify"）。
            // fire-and-forget：通知外部 hook 但不等结果。
            if !hooks.is_empty() {
                let sid = session_id.clone().unwrap_or_default();
                let hooks_for_cancel = hooks.clone();
                tokio::spawn(async move {
                    let _ = hooks_for_cancel
                        .trigger(&HookPoint::Notification {
                            session_id: sid,
                            level: "cancel".to_string(),
                            message: "user_cancelled".to_string(),
                        })
                        .await;
                });
            }
            break Err(ModelError::Cancelled);
        }

        // Turn 边界兜底：ToolStep 后已经 drain 过一次；如果 surface 在
        // ToolStep drain 之后、下一轮 ModelStep 构造请求之前才注入引导消息，
        // 这里保证它仍然进入本轮请求，而不是晚一轮或等当前 Run 结束。
        drain_pending_inputs(
            pending_inputs.as_ref(),
            consumed_pending_inputs.as_ref(),
            persister.as_ref(),
            transcript,
        );
        // 插队的 `Goal set` 已落盘 → 落 set marker（若 run 跑时设了 goal）。
        maybe_emit_pending_set_marker(data_dir.as_deref(), session_id.as_deref());

        // Microcompact：每轮模型请求前先把超阈值的老 tool_result 压缩为占位符。
        // 不消耗模型调用，只改 transcript entries，幂等。
        let mc_report = microcompact(&mut transcript.entries, &MicrocompactPolicy::default());
        // 把被压缩的原文落 txt（架构 §4.7 / Step 9）：data_dir + session_id 都给定时
        // 才落，否则只是 in-memory 占位符。占位符里写了 call_id，LLM 可用 Read
        // `<data_dir>/sessions/<sid>/tool_results/<call_id>.txt` 按需检索原始内容。
        if !mc_report.shadowed_artifacts.is_empty() {
            if let (Some(dd), Some(sid)) = (data_dir.as_ref(), session_id.as_deref()) {
                for (call_id, content) in &mc_report.shadowed_artifacts {
                    if let Err(e) =
                        crate::storage::tool_results::save_tool_result(dd, sid, call_id, content)
                    {
                        tracing::warn!(error = %e, call_id, "compaction artifact save failed");
                    }
                }
            }
        }
        if mc_report.shadowed_count > 0 {
            tracing::info_span!(
                "microcompact",
                hebbian.microcompact.shadowed = mc_report.shadowed_count,
                hebbian.microcompact.kept = mc_report.kept_count,
                hebbian.microcompact.total = mc_report.total_compactable,
            )
            .in_scope(|| {
                tracing::info!(
                    shadowed = mc_report.shadowed_count,
                    kept = mc_report.kept_count,
                    total = mc_report.total_compactable,
                    "microcompact shadowed old tool results"
                );
            });
        }

        if needs_compaction(
            transcript.system.as_deref(),
            &transcript.entries,
            compaction_policy,
            calib_real,
            calib_estimated,
        ) {
            let compaction_span = tracing::info_span!(
                "compaction",
                hebbian.compaction.before_tokens = Empty,
                hebbian.compaction.after_tokens = Empty,
            );
            let _enter = compaction_span.enter();

            // L2 自动压缩：与手动 /compact 同一个 compact_with_llm 函数——调一次 LLM
            // 把整段历史浓缩成接力摘要。绝不用纯结构化裁剪（会把长对话砍到几十 token）。
            let compact_start = Instant::now();
            let (compact_before_tokens, compact_req) = build_compaction_request(
                transcript.system.as_deref(),
                transcript.entries.clone(),
                None,
            );
            let compact_req_snapshot = model_io_dump.as_ref().map(|_| compact_req.clone());
            info!(
                before_tokens = compact_before_tokens,
                entries = compact_req.entries.len(),
                "context compaction started"
            );
            emit(EventPayload::ContextCompactionStarted {
                before_tokens: compact_before_tokens,
            });
            let compaction_outcome = crate::context::compaction::compact_request_with_llm_progress(
                client,
                compact_req,
                compact_before_tokens,
                cancel.clone(),
                |output_tokens| {
                    emit(EventPayload::ContextCompactionProgress { output_tokens });
                },
            )
            .await;
            let compact_duration_ms = compact_start.elapsed().as_millis() as u64;

            if let (Some(dump), Some(req)) = (model_io_dump.as_ref(), compact_req_snapshot) {
                let response = match &compaction_outcome {
                    Ok(result) => serde_json::json!({
                        "type": "Done",
                        "text": result.summary,
                        "before_tokens": result.before_tokens,
                        "after_tokens": result.after_tokens,
                    }),
                    Err(e) => serde_json::json!({
                        "type": "Error",
                        "error": e.to_string(),
                    }),
                };
                dump.record(DumpEntry {
                    ts: model_io_dump::iso_now(),
                    run_id: state.run_id.to_string(),
                    turn: state.current_turn(),
                    model: client.provider_id().to_string(),
                    request: model_io_dump::request_to_json(&req, client.provider_id()),
                    response,
                    duration_ms: compact_duration_ms,
                    kind: "compaction".to_string(),
                });
            }

            match compaction_outcome {
                Ok(compaction_result) => {
                    let before_tokens = compaction_result.before_tokens;
                    let after_tokens = compaction_result.after_tokens;
                    let summary = compaction_result.summary.clone();
                    compaction_span.record(attr::COMPACTION_BEFORE_TOKENS, before_tokens);
                    compaction_span.record(attr::COMPACTION_AFTER_TOKENS, after_tokens);
                    info!(
                        before_tokens,
                        after_tokens, "context compacted (llm summary)"
                    );
                    transcript.entries = compaction_result.entries;
                    drop(_enter);

                    if let Some(p) = persister.as_ref() {
                        p.flush_segment().await;
                    }

                    // 写 compact_boundary marker 到 session.jsonl，前端渲染压缩分隔线 +
                    // 可展开的摘要（与手动 /compact 落的 marker 同形态）。
                    if let (Some(dd), Some(sid)) = (data_dir.as_ref(), session_id.as_deref()) {
                        use crate::storage::sessions::{
                            self as sess_store, Message, MessageMeta, Role,
                        };
                        let marker = Message {
                            id: sess_store::new_id(),
                            role: Role::Marker,
                            content: summary.clone(),
                            attachments: Vec::new(),
                            tool_calls: Vec::new(),
                            parts: Vec::new(),
                            created_at: chrono::Utc::now().timestamp_millis(),
                            meta: Some(MessageMeta::CompactBoundary {
                                summary,
                                before_tokens,
                                after_tokens,
                            }),
                            subagent_call_id: None,
                            run_duration_ms: None,
                        };
                        if let Err(e) = sess_store::append_message(dd, sid, marker) {
                            tracing::warn!(error = %e, "L2 压缩：写 compact_boundary marker 失败，忽略");
                        }
                    }

                    emit(EventPayload::ContextCompacted {
                        before_tokens,
                        after_tokens,
                    });
                    hooks
                        .trigger(&HookPoint::OnCompaction {
                            before_tokens,
                            after_tokens,
                        })
                        .await;
                }
                Err(ModelError::Cancelled) => {
                    drop(_enter);
                    return Err(ModelError::Cancelled);
                }
                Err(e) => {
                    // LLM 压缩失败（供应商不可用 / 网络断 / 返回空摘要等）：
                    // 绝不丢上下文——保留原 transcript 继续这一轮，下一轮再试。
                    drop(_enter);
                    tracing::error!(error = %e, "L2 自动压缩失败，保留原上下文继续");
                    emit(EventPayload::Notice {
                        level: LogLevel::Warn,
                        message: format!("上下文自动压缩失败（{e}），本轮保留原文继续"),
                        dedup_key: Some("auto_compaction_failed".to_string()),
                    });
                }
            }
        }

        let turn_index = state.next_turn();
        let turn_id = protocol::TurnId::new();
        let turn_span = tracing::info_span!(
            "agent.iteration",
            hebbian.turn.index = turn_index,
            hebbian.turn.id = %turn_id,
            hebbian.turn.stop_reason = Empty,
            hebbian.turn.tool_calls = Empty,
        );
        emit(EventPayload::TurnStarted {
            turn_id: turn_id.clone(),
            turn: turn_index,
        });

        // 内置工具每轮都自动注入：ask + Bash/Read/Write/Grep/Skill。
        // 用户可选工具按 enabled_tools 过滤。条件注入工具（如 Task）一律加进白名单，
        // registry 没注册的会被自然忽略，让 default_tools 的条件注入决策真正生效。
        let mut tool_defs = ask_only_definitions();
        let mut all_filter: Vec<String> =
            BUILTIN_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
        all_filter.extend(CONDITIONAL_TOOL_NAMES.iter().map(|s| s.to_string()));
        all_filter.extend(enabled_tools.iter().cloned());
        tool_defs.extend(registry.definitions(&all_filter));
        tool_defs.extend(registry.mcp_definitions());
        if !enabled_tools.is_empty() {
            tool_defs.extend(hosted_tool_definitions(enabled_tools));
        }
        // PlanMode 工具集（架构 §4.4.3 / §4.4.5）。PlanMode 工具常驻注入，但按当前
        // 运行模式定制 description/schema、暴露不同 action：
        // - 非 PlanMode：只暴露 `enter`，让 agent 能自主进入计划模式
        // - PlanMode：删除会改外界的工具（强制只读探索），PlanMode 工具收为 `update`/`submit`
        let current_run_mode = *run_mode.lock().unwrap();
        let plan_active = current_run_mode == crate::run_mode::RunMode::PlanMode;
        if plan_active {
            let mutating = ["Bash", "PowerShell", "Edit"];
            tool_defs.retain(|t| !mutating.contains(&t.name.as_str()));
        }
        for def in tool_defs.iter_mut() {
            if def.name == crate::tools::plan_mode::PLAN_MODE_TOOL_NAME {
                if plan_active {
                    def.description = crate::tools::plan_mode::active_description().to_string();
                    def.parameters = crate::tools::plan_mode::active_schema();
                } else {
                    def.description = crate::tools::plan_mode::enter_description().to_string();
                    def.parameters = crate::tools::plan_mode::enter_schema();
                }
            }
        }
        let has_tools = !tool_defs.is_empty();

        // system prompt = BASE 常量 + 用户 persona + rules。
        // 环境信息（cwd / allowed_paths）走 user message 的 `<environment>` 块保 cache；
        // rules 内容（CLAUDE.md 等）本身变化极少，进 system 段保证每轮都有权威约束力。
        let combined_system = {
            let mut s = compose_system_prompt(transcript.system.as_deref());
            if let Some(r) = &system_rules {
                if !r.is_empty() {
                    s.push('\n');
                    s.push_str(r);
                }
            }
            s
        };

        let req = ModelRequest {
            model: model_id.clone().unwrap_or_default(),
            system: Some(combined_system),
            entries: transcript.entries.clone(),
            tools: tool_defs,
            max_tokens: 8192,
            reasoning: None,
            // 主 chat：tag 由创建方显式传入（call_tag），主对话 = Main（前端不额外标记）；
            // 带 session/run/turn/assistant-msg-id，让 `[model]` 日志 + model_io 串起来。
            meta: model_gateway::types::ModelCallMeta {
                session_id: session_id.clone(),
                run_id: Some(state.run_id.to_string()),
                turn: turn_index as u32,
                message_id: persister.as_ref().map(|p| p.msg_id().to_string()),
                tag: call_tag,
            },
        };

        // 与本轮请求配对的本地估算值（surface `context_usage` 同款口径）。采样点紧贴
        // 请求构建：此后到拿 usage 之间 transcript 不变。它与服务端 `usage.input_tokens`
        // 真值一起落进 token_stats，比值用于校准估算（见 `calibrated_transcript_tokens`）。
        let request_estimated_tokens =
            budget::estimate_transcript_tokens(transcript.system.as_deref(), &transcript.entries)
                as u64;

        debug!(iteration, "calling model");
        hooks
            .trigger(&HookPoint::BeforeModelCall { turn: turn_index })
            .await;
        let call_start = Instant::now();

        // 启用 dump 时先 clone 一份 ModelRequest（含完整 transcript），
        // 调用结束后落盘。未启用时 zero-cost。
        let dump_request = model_io_dump.as_ref().map(|_| req.clone());

        let stream_tool_call_offset = tool_call_dispatch_offset;
        let stream_tool_delta_count = Arc::new(AtomicUsize::new(0));
        let stream_tool_delta_with_id_count = Arc::new(AtomicUsize::new(0));
        let stream_tool_delta_with_name_count = Arc::new(AtomicUsize::new(0));
        let stream_tool_delta_argument_bytes = Arc::new(AtomicUsize::new(0));
        let stream_tool_delta_count_for_log = stream_tool_delta_count.clone();
        let stream_tool_delta_with_id_count_for_log = stream_tool_delta_with_id_count.clone();
        let stream_tool_delta_with_name_count_for_log = stream_tool_delta_with_name_count.clone();
        let stream_tool_delta_argument_bytes_for_log = stream_tool_delta_argument_bytes.clone();
        let stream_tool_delta_count_for_cb = stream_tool_delta_count.clone();
        let stream_tool_delta_with_id_count_for_cb = stream_tool_delta_with_id_count.clone();
        let stream_tool_delta_with_name_count_for_cb = stream_tool_delta_with_name_count.clone();
        let stream_tool_delta_argument_bytes_for_cb = stream_tool_delta_argument_bytes.clone();
        let on_event_for_stream = on_event.clone();
        let state_for_stream = state.clone();
        // 走 stream 的条件：调用方要求流式 + (本轮无工具 || provider 支持流式工具调用)。
        // anthropic / gemini 默认不支持流式工具调用，含工具时只能用 complete 路径。
        let used_stream_path = stream && (!has_tools || client.supports_streaming_tools());
        let model_span = tracing::info_span!(
            parent: &turn_span,
            "model",
            hebbian.turn.index = turn_index,
            hebbian.model.streaming = used_stream_path,
        );
        model_step_index += 1;
        emit(EventPayload::StepStarted {
            step_kind: protocol::StepKind::Model,
            step_index: model_step_index,
        });
        // 流式回调定义一次，重试各 attempt 复用（架构 §4.3）。
        let stream_cb = move |stream_event: ModelStreamEvent| {
            let payload = match stream_event {
                ModelStreamEvent::TextDelta { text } => EventPayload::TextDelta { text },
                ModelStreamEvent::ReasoningDelta { text } => EventPayload::Reasoning { text },
                ModelStreamEvent::ReasoningSignature { .. } => return,
                ModelStreamEvent::ReasoningDuration { ms } => {
                    EventPayload::ReasoningDuration { ms }
                }
                ModelStreamEvent::ToolCallDelta(delta) => {
                    stream_tool_delta_count_for_cb.fetch_add(1, Ordering::Relaxed);
                    if delta.id.as_deref().is_some_and(|id| !id.trim().is_empty()) {
                        stream_tool_delta_with_id_count_for_cb.fetch_add(1, Ordering::Relaxed);
                    }
                    if delta.name.as_deref().is_some_and(|name| !name.trim().is_empty()) {
                        stream_tool_delta_with_name_count_for_cb.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Some(arguments) = delta.arguments_delta.as_deref() {
                        stream_tool_delta_argument_bytes_for_cb
                            .fetch_add(arguments.len(), Ordering::Relaxed);
                    }
                    EventPayload::ToolCallDelta {
                        index: stream_tool_call_offset + delta.index,
                        id: delta.id,
                        name: delta.name,
                        arguments_delta: delta.arguments_delta,
                    }
                }
            };
            on_event_for_stream(state_for_stream.event(payload));
        };
        // 模型调用 + 用户可见自动重试（架构 §4.3）：可重试错误退避后重试，每次 emit
        // ModelRetry 让 surface 内联显示进度（并清掉上次残留的流式 partial）；耗尽
        // MAX_MODEL_RETRIES 才把 Err 交给下游收尾（→ RunFailed + pending_continue）。
        let mut retry_attempt = 0u32;
        let response_result = loop {
            let r = if used_stream_path {
                client
                    .stream(req.clone(), cancel.clone(), &stream_cb)
                    .instrument(model_span.clone())
                    .await
            } else {
                client
                    .complete(req.clone(), cancel.clone())
                    .instrument(model_span.clone())
                    .await
            };
            match &r {
                Err(e) if retry_attempt < MAX_MODEL_RETRIES && is_retryable_model_error(e) => {
                    retry_attempt += 1;
                    let delay = model_retry_delay(retry_attempt);
                    emit(EventPayload::ModelRetry {
                        attempt: retry_attempt,
                        max: MAX_MODEL_RETRIES,
                        delay_ms: delay.as_millis() as u64,
                        reason: e.to_string(),
                    });
                    info!(
                        attempt = retry_attempt,
                        max = MAX_MODEL_RETRIES,
                        stream_tool_delta_count = stream_tool_delta_count_for_log.load(Ordering::Relaxed),
                        stream_tool_delta_with_id_count = stream_tool_delta_with_id_count_for_log.load(Ordering::Relaxed),
                        stream_tool_delta_with_name_count = stream_tool_delta_with_name_count_for_log.load(Ordering::Relaxed),
                        stream_tool_delta_argument_bytes = stream_tool_delta_argument_bytes_for_log.load(Ordering::Relaxed),
                        error = %e,
                        "model call retry after partial stream"
                    );
                    if !backoff_or_cancel(delay, &cancel).await {
                        break Err(ModelError::Cancelled);
                    }
                    continue;
                }
                _ => break r,
            }
        };

        let call_duration_ms = call_start.elapsed().as_millis() as u64;
        emit(EventPayload::StepFinished {
            step_kind: protocol::StepKind::Model,
            step_index: model_step_index,
        });

        if let (Some(dump), Some(req)) = (model_io_dump.as_ref(), dump_request) {
            dump.record(DumpEntry {
                ts: model_io_dump::iso_now(),
                run_id: state.run_id.to_string(),
                turn: turn_index,
                model: client.provider_id().to_string(),
                request: model_io_dump::request_to_json(&req, client.provider_id()),
                response: model_io_dump::response_to_json(&response_result),
                duration_ms: call_duration_ms,
                kind: dump.main_kind().to_string(),
            });
        }

        let response = match response_result {
            Ok(response) => response,
            Err(e) => {
                turn_span.record(attr::STOP_REASON, "failed");
                emit(EventPayload::TurnFinished {
                    turn_id: turn_id.clone(),
                    turn: turn_index,
                    stop_reason: StopReason::Failed,
                });
                break Err(e);
            }
        };

        match response {
            ModelResponse::Done {
                text,
                reasoning,
                attachments,
                usage,
                finish,
                reasoning_signature: _,
            } => {
                info!(
                    duration_ms = call_duration_ms,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    text_len = text.len(),
                    stream_tool_delta_count = stream_tool_delta_count.load(Ordering::Relaxed),
                    stream_tool_delta_with_id_count = stream_tool_delta_with_id_count.load(Ordering::Relaxed),
                    stream_tool_delta_with_name_count = stream_tool_delta_with_name_count.load(Ordering::Relaxed),
                    stream_tool_delta_argument_bytes = stream_tool_delta_argument_bytes.load(Ordering::Relaxed),
                    finish = ?finish,
                    "model done"
                );
                last_finish = finish;
                record_request_usage(
                    &usage,
                    request_estimated_tokens,
                    &emit,
                    data_dir.as_deref(),
                    session_id.as_deref(),
                );
                (calib_real, calib_estimated) = (usage.input_tokens, request_estimated_tokens);
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
                total_cache_read_tokens += usage.cache_read_tokens;
                total_cache_creation_tokens += usage.cache_creation_tokens;

                // 工具调用 XML 漏进正文的自愈（架构 §4.3.3）。展示层按既定取舍可留脏，
                // 但**进 transcript（下一轮喂模型）的文本必须干净**，否则模型把残骸当范例
                // 模仿，雪球越滚越大。检测到残骸且未超续跑上限：清洗后注入纠错 user 续跑，
                // 不 emit TextDone / TurnFinished、不跑 Stop hook（这一轮不算自然结束）。
                let leak = sanitize_tool_xml_leak(&text);
                if leak.detected && tool_xml_leak_recoveries < MAX_TOOL_XML_LEAK_RECOVERIES {
                    tool_xml_leak_recoveries += 1;
                    info!(
                        attempt = tool_xml_leak_recoveries,
                        max = MAX_TOOL_XML_LEAK_RECOVERIES,
                        "tool-call XML leaked into content; sanitized and resuming turn",
                    );
                    // UI 仍收到原始（脏）文本——用户能一眼看出模型抽风；进 transcript 的是
                    // 清洗版，模型下一轮看不到残骸。
                    if !used_stream_path && !text.is_empty() {
                        emit(EventPayload::TextDelta { text: text.clone() });
                    }
                    transcript.push_assistant_with_reasoning(leak.text, reasoning, Vec::new());
                    transcript.push_user(
                        "[SYSTEM NOTIFICATION - NOT USER INPUT]\n<tool-format-error>\n\
                         上一条回复里出现了未被执行的工具调用文本（`<invoke>` / `<function_calls>` XML）。\
                         工具调用必须走结构化 function-calling 通道，绝不能把这种 XML 写进正文。\
                         请用正确的工具调用方式重新执行刚才想做的操作。\n</tool-format-error>"
                            .to_string(),
                        Vec::new(),
                    );
                    continue;
                }

                // 非流式路径：reasoning 一次性带回，需要补发一个 Reasoning 事件让 UI 渲染。
                // 流式路径下 stream provider 已经分段 emit 过 ReasoningDelta，这里 reasoning
                // 通常是空字符串，跳过即可。
                if !used_stream_path && !reasoning.is_empty() {
                    emit(EventPayload::Reasoning {
                        text: reasoning.clone(),
                    });
                }
                emit(EventPayload::TextDone {
                    full_text: text.clone(),
                });
                turn_span.record(attr::STOP_REASON, "end_turn");
                emit(EventPayload::TurnFinished {
                    turn_id,
                    turn: turn_index,
                    stop_reason: StopReason::EndTurn,
                });

                // 段边界落盘（架构 §4.9.5）：仅当有 pending 插队时才先落本段，再 drain——
                // 保证 session.jsonl 里 assistant 段在插队 user 之前。无插队时不切段，
                // 全 run 累积到 finish 一次性落 = 一条 assistant message（保持原有多 turn
                // 无插队时「一个 run 一张卡片」的 UX）。
                if has_pending(pending_inputs.as_ref()) {
                    if let Some(p) = persister.as_ref() {
                        p.flush_segment().await;
                    }
                }

                // 续跑上限耗尽仍漏：残骸照常收尾（UI 留脏），但进 transcript / 返回上层的
                // 文本仍清洗，杜绝残骸沉淀进历史继续污染。
                transcript.push_assistant_with_reasoning(leak.text.clone(), reasoning, Vec::new());
                let mut all_attachments = output_attachments;
                all_attachments.extend(attachments);
                set_pending_inputs_accepting(pending_inputs_accepting.as_ref(), false);
                if drain_pending_inputs(
                    pending_inputs.as_ref(),
                    consumed_pending_inputs.as_ref(),
                    persister.as_ref(),
                    transcript,
                ) > 0
                {
                    // 用户在本 turn 内插了新消息——turn 实质未"自然结束"，
                    // 不跑 Stop hook，直接续跑（与原逻辑一致）。
                    set_pending_inputs_accepting(pending_inputs_accepting.as_ref(), true);
                    output_attachments = all_attachments;
                    continue;
                }

                // 架构 §4.8.3：turn 自然结束 → 跑 Stop hook 做后置 verify。
                // 拿到 InjectFollowup 时 push 一条 `<hook-feedback>` user message，
                // 不退出 loop，进入下一轮让模型修复。
                // 上限 MAX_STOP_INJECTIONS 防 cargo check 永远修不好把 loop 跑爆。
                if !hooks.is_empty() && stop_hook_injections < MAX_STOP_INJECTIONS {
                    let sid = session_id.clone().unwrap_or_default();
                    let wd = workspace.workdir().to_string_lossy().into_owned();
                    let outcome = hooks
                        .trigger(&HookPoint::Stop {
                            session_id: sid,
                            reason: "end_turn".to_string(),
                            workdir: Some(wd),
                        })
                        .await;
                    // hook 跑了就落一条 marker（通过/注入都显示）——先 flush assistant 段，
                    // 保证 marker 物理排在它该回应的 assistant 之后。
                    if let (Some(dd), Some(sid)) = (data_dir.as_deref(), session_id.as_deref()) {
                        if let Some(p) = persister.as_ref() {
                            p.flush_segment().await;
                        }
                        let (status, detail) = match &outcome {
                            HookOutcome::InjectFollowup(r) if !r.trim().is_empty() => {
                                ("injected", r.trim())
                            }
                            HookOutcome::Block(r) => ("blocked", r.as_str()),
                            _ => ("passed", ""),
                        };
                        append_hook_outcome_marker(dd, sid, "Stop", status, detail);
                    }
                    if let HookOutcome::InjectFollowup(reminder) = outcome {
                        let trimmed = reminder.trim();
                        if !trimmed.is_empty() {
                            stop_hook_injections += 1;
                            info!(
                                attempt = stop_hook_injections,
                                max = MAX_STOP_INJECTIONS,
                                reminder_len = trimmed.len(),
                                "Stop hook injected follow-up; resuming turn",
                            );
                            let wrapped = format!(
                                "[SYSTEM NOTIFICATION - NOT USER INPUT]\n<hook-feedback source=\"Stop\">\n{trimmed}\n</hook-feedback>",
                            );
                            transcript.push_user(wrapped, Vec::new());
                            set_pending_inputs_accepting(pending_inputs_accepting.as_ref(), true);
                            output_attachments = all_attachments;
                            continue;
                        }
                    }
                }
                // 架构 §4.8.3：外部 Stop hook（cargo check 等 verify）放行后，
                // 若会话挂了 //goal 目标，跑 judge 判 transcript 是否满足完成条件。
                // judge 用会话主 client+主模型（judge_client 在 AutoMode 未配置时可能为
                // None，此时无法裁决——保留目标但本 run 不再自动续跑，避免静默放行）。
                // 前三步只是取 (dd, sid, goal) 三元组，任一步缺失/失败都按"跳过裁决、
                // 正常收尾"处理——压平成单个表达式，避免深层嵌套（dd/sid 是借用，goal owned）。
                let goal_ctx = data_dir
                    .as_ref()
                    .zip(session_id.as_deref())
                    .and_then(|(dd, sid)| {
                        crate::storage::sessions::load(dd, sid)
                            .ok()
                            .map(|s| (dd, sid, s))
                    })
                    .and_then(|(dd, sid, s)| s.active_goal.map(|g| (dd, sid, g)));
                if let Some((dd, sid, goal)) = goal_ctx {
                    // 裁决要落 goal marker——先把本轮累积的 assistant 段落盘（不盖 run 耗时，
                    // 续跑时这是中间段；run 耗时由收尾 finish 只盖末段），保证 marker 物理排在
                    // 它该回应的 assistant 之后，而非倒挂到前面。flush 后累积器清空，下方 run
                    // 收尾的 finish() 不会重复落这段（无新段时回填耗时到这条已落盘的末段）。
                    if let Some(p) = persister.as_ref() {
                        p.flush_segment().await;
                    }
                    match judge_client.as_ref() {
                        None => {
                            tracing::warn!(
                                "active_goal 存在但 judge client 未配置，无法裁决，本 run 不续跑"
                            );
                        }
                        Some(jc) => {
                            let model = model_id.clone().unwrap_or_default();
                            let verdict = crate::goal::judge_goal(
                                jc,
                                &model,
                                &goal.condition,
                                &transcript.entries,
                                cancel.clone(),
                                model_io_dump.as_ref(),
                                &state.run_id.to_string(),
                                turn_index,
                            )
                            .await;
                            match verdict {
                                crate::goal::GoalVerdict::Achieved(reason) => {
                                    if let Err(e) =
                                        crate::storage::sessions::set_active_goal(dd, sid, None)
                                    {
                                        tracing::warn!(error = %e, "goal 达成后清目标落盘失败");
                                    }
                                    append_goal_outcome_marker(
                                        dd,
                                        sid,
                                        "achieved",
                                        &goal.condition,
                                        &reason,
                                        goal_iterations,
                                    );
                                    emit(EventPayload::GoalAchieved {
                                        condition: goal.condition.clone(),
                                        reason,
                                    });
                                    // 目标达成 → 正常出 turn（落到下方 break）。
                                }
                                crate::goal::GoalVerdict::Impossible(reason) => {
                                    if let Err(e) =
                                        crate::storage::sessions::set_active_goal(dd, sid, None)
                                    {
                                        tracing::warn!(error = %e, "goal 判定不可达后清目标落盘失败");
                                    }
                                    append_goal_outcome_marker(
                                        dd,
                                        sid,
                                        "impossible",
                                        &goal.condition,
                                        &reason,
                                        goal_iterations,
                                    );
                                    emit(EventPayload::GoalImpossible {
                                        condition: goal.condition.clone(),
                                        reason,
                                    });
                                    // 熔断1：判不可能 → 清目标、正常出 turn。
                                }
                                crate::goal::GoalVerdict::NotYet(reason) => {
                                    // 用户在 turn 末尾 cancel 会被 judge 归一成 NotYet——这里先拦掉，
                                    // 避免虚增计数 / 注入无用 feedback / 多发 GoalProgress。
                                    if cancellation::is_cancelled(&cancel) {
                                        break Ok(AssistantOutput {
                                            text: leak.text,
                                            attachments: all_attachments,
                                        });
                                    }
                                    goal_iterations += 1;
                                    // 落盘更新 iterations + last_reason（跨重启可见）。
                                    let updated = crate::storage::sessions::ActiveGoal {
                                        condition: goal.condition.clone(),
                                        created_at: goal.created_at,
                                        iterations: goal_iterations,
                                        last_reason: Some(reason.clone()),
                                        pending_set_marker: false,
                                    };
                                    if let Err(e) = crate::storage::sessions::set_active_goal(
                                        dd,
                                        sid,
                                        Some(updated),
                                    ) {
                                        tracing::warn!(error = %e, "goal 续跑进度落盘失败");
                                    }
                                    // marker 必须在 emit 之前落盘：前端收到 GoalProgress
                                    // 事件会立即 reload session，此刻 marker 须已在盘上，
                                    // 否则 reload 读不到、marker 不显示（与 achieved/
                                    // impossible 分支顺序保持一致）。
                                    append_goal_outcome_marker(
                                        dd,
                                        sid,
                                        "progress",
                                        &goal.condition,
                                        &reason,
                                        goal_iterations,
                                    );
                                    emit(EventPayload::GoalProgress {
                                        iteration: goal_iterations,
                                        reason: reason.clone(),
                                    });
                                    info!(iteration = goal_iterations, "goal 尚未达成，续跑");
                                    let wrapped = format!(
                                        "[SYSTEM NOTIFICATION - NOT USER INPUT]\n<goal-feedback>\n目标尚未达成。{reason}\n继续推进，达成后会自动结束。\n</goal-feedback>"
                                    );
                                    transcript.push_user(wrapped, Vec::new());
                                    set_pending_inputs_accepting(
                                        pending_inputs_accepting.as_ref(),
                                        true,
                                    );
                                    output_attachments = all_attachments;
                                    continue;
                                }
                            }
                        }
                    }
                }
                break Ok(AssistantOutput {
                    text: leak.text,
                    attachments: all_attachments,
                });
            }
            ModelResponse::ToolCalls {
                text,
                reasoning,
                calls,
                attachments,
                usage,
                reasoning_signature: _,
            } => {
                info!(
                    duration_ms = call_duration_ms,
                    calls_count = calls.len(),
                    stream_tool_delta_count = stream_tool_delta_count.load(Ordering::Relaxed),
                    stream_tool_delta_with_id_count = stream_tool_delta_with_id_count.load(Ordering::Relaxed),
                    stream_tool_delta_with_name_count = stream_tool_delta_with_name_count.load(Ordering::Relaxed),
                    stream_tool_delta_argument_bytes = stream_tool_delta_argument_bytes.load(Ordering::Relaxed),
                    "model requested tool calls"
                );
                record_request_usage(
                    &usage,
                    request_estimated_tokens,
                    &emit,
                    data_dir.as_deref(),
                    session_id.as_deref(),
                );
                (calib_real, calib_estimated) = (usage.input_tokens, request_estimated_tokens);
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
                total_cache_read_tokens += usage.cache_read_tokens;
                total_cache_creation_tokens += usage.cache_creation_tokens;

                // 走 stream 路径时，TextDelta 已经一段段经 provider 流出来了；
                // 再 emit 一次会把整段正文重复喷给 surface（且 provider 端的
                // sieve 等增量过滤也会被绕开）。仅在 complete 路径下补发。
                if !used_stream_path && !reasoning.is_empty() {
                    emit(EventPayload::Reasoning {
                        text: reasoning.clone(),
                    });
                }
                if !used_stream_path && !text.is_empty() {
                    emit(EventPayload::TextDelta { text: text.clone() });
                }
                output_attachments.extend(attachments);

                // 父当前 transcript 副本：在 push 触发 turn 之前抓快照——看到的形态
                // 截止上一 turn 结束，不含触发它的 assistant tool_call（避免子 transcript
                // 出现无对应 ToolResult 的 self-reference）。两个消费方：
                // 1. inherit 模式 Task 子 agent（架构 §4.4.11.3）
                // 2. AutoMode judge——需要 recent_transcript 推断用户意图，否则永远判
                //    「no user intent」误杀（架构 §4.4.4）。
                // 同 ToolStep 内的 parallel Task 共享同一份 Arc，看到一致形态。
                let parent_transcript_snapshot = Some(Arc::new(transcript.entries.clone()));

                transcript.push_assistant_with_reasoning(text, reasoning, calls.clone());

                if let Some(max) = max_tool_iterations {
                    if iteration >= max {
                        let msg = format!("已达到最大工具调用轮数 {max}");
                        tracing::warn!(max_iterations = max, "max iterations");
                        turn_span.record(attr::STOP_REASON, "max_iterations");
                        emit(EventPayload::TurnFinished {
                            turn_id: turn_id.clone(),
                            turn: turn_index,
                            stop_reason: StopReason::MaxIterations,
                        });
                        break Err(ModelError::Other(msg));
                    }
                }
                iteration += 1;
                turn_span.record("hebbian.turn.tool_calls", calls.len());

                let dispatcher = ToolDispatcher {
                    registry: registry.clone(),
                    hitl: hitl.clone(),
                    workspace: workspace.clone(),
                    state: state.clone(),
                    sink: on_event.clone(),
                    cancel: cancel.clone(),
                    run_mode: run_mode.clone(),
                    model_id: model_id.clone(),
                    judge_client: judge_client.clone(),
                    force_automode: force_automode.clone(),
                    hooks: hooks.clone(),
                    session_id_for_hooks: session_id.clone(),
                    data_dir_for_artifacts: data_dir.clone(),
                    permission_store: hitl.permission_store().cloned(),
                    edits_worktree: edits_worktree.clone(),
                    current_run_id: Some(edits_run_id.clone()),
                    subagent_ctx: subagent_ctx.clone(),
                    parent_transcript_snapshot,
                    model_io_dump: model_io_dump.clone(),
                    subagent_bypass,
                };

                let tools_span = tracing::info_span!(
                    parent: &turn_span,
                    "tools",
                    hebbian.turn.index = turn_index,
                    hebbian.turn.tool_calls = calls.len(),
                );
                tool_step_index += 1;
                emit(EventPayload::StepStarted {
                    step_kind: protocol::StepKind::Tool,
                    step_index: tool_step_index,
                });
                let results = match dispatcher
                    .run_calls(&calls, tool_call_dispatch_offset)
                    .instrument(tools_span.clone())
                    .await
                {
                    Ok(results) => results,
                    Err(e) => {
                        turn_span.record(attr::STOP_REASON, "cancelled");
                        emit(EventPayload::TurnFinished {
                            turn_id: turn_id.clone(),
                            turn: turn_index,
                            stop_reason: StopReason::Cancelled,
                        });
                        break Err(e);
                    }
                };

                transcript.push_tool_results(results);
                tool_call_dispatch_offset += calls.len();
                emit(EventPayload::StepFinished {
                    step_kind: protocol::StepKind::Tool,
                    step_index: tool_step_index,
                });
                // 段边界落盘（架构 §4.9.5）：仅当有 pending 插队时才先落 assistant 段，
                // 再 drain 插队 user，保证物理序 = emit 序。无插队不切段。
                if has_pending(pending_inputs.as_ref()) {
                    if let Some(p) = persister.as_ref() {
                        p.flush_segment().await;
                    }
                }
                drain_pending_inputs(
                    pending_inputs.as_ref(),
                    consumed_pending_inputs.as_ref(),
                    persister.as_ref(),
                    transcript,
                );

                // 架构 §4.12.5：ToolStep 跑完后检查 phase channel。模型本 ToolStep
                // 调过挂起工具时，phase 已被工具写入；这里：
                // 1. emit RunSuspended（surface 据此渲染 BackgroundTaskPanel 占位）
                // 2. 落 RunCheckpoint
                // 3. 在 WakeupScheduler 上 arm 对应 wakeup（bg-task / cron）
                // 4. emit TurnFinished(EndTurn) 收尾本 turn
                // 5. break loop —— agent_loop task 结束，等 scheduler 喊醒后由
                //    Harness::resume_run 重新 spawn
                if let Some(p) = phase.as_ref() {
                    let phase_pending = p.lock().unwrap().take();
                    if let Some(ph) = phase_pending {
                        use crate::storage::run_checkpoint::{self as ck, RunPhase};
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        let (reason_evt, resumes_at_ms, waiting_for_task_ids) = match &ph {
                            RunPhase::AwaitingBackgroundTask { task_id, .. } => (
                                protocol::SuspendReason::BackgroundTask,
                                None,
                                vec![task_id.clone()],
                            ),
                            RunPhase::AwaitingCron { fire_at_ms, .. } => {
                                (protocol::SuspendReason::Cron, Some(*fire_at_ms), Vec::new())
                            }
                        };
                        emit(EventPayload::RunSuspended {
                            reason: reason_evt,
                            resumes_at_ms,
                            waiting_for_task_ids: waiting_for_task_ids.clone(),
                        });
                        if let (Some(dd), Some(sid)) = (data_dir.as_ref(), session_id.as_deref()) {
                            let checkpoint = ck::RunCheckpoint {
                                run_id: state.run_id.to_string(),
                                session_id: sid.to_string(),
                                agent: agent.0.to_string(),
                                run_mode: format!("{:?}", *run_mode.lock().unwrap()),
                                model_id: model_id.clone(),
                                iteration,
                                model_step_index,
                                tool_step_index,
                                tool_call_dispatch_offset,
                                total_input_tokens,
                                total_output_tokens,
                                total_cache_read_tokens,
                                total_cache_creation_tokens,
                                phase: ph.clone(),
                                suspended_at_ms: now_ms,
                            };
                            if let Err(e) = ck::save(dd, &checkpoint) {
                                tracing::warn!(error = %e, "RunCheckpoint save failed");
                            }
                        }
                        // 注册唤醒条件到进程内 scheduler（架构 §4.12.2）。
                        let scheduler = crate::wakeup::WakeupScheduler::global();
                        let sid_for_arm = session_id.clone().unwrap_or_default();
                        let run_id_for_arm = state.run_id.to_string();
                        match ph {
                            RunPhase::AwaitingBackgroundTask { task_id, .. } => {
                                // 兼容旧 checkpoint：老版本可通过显式后台任务挂起进入这里。
                                // 新版本后台任务只走自动 arm 路径；ScheduleWakeup 仍用 phase。
                                scheduler.arm_bg_task(sid_for_arm, run_id_for_arm, task_id, None);
                            }
                            RunPhase::AwaitingCron {
                                fire_at_ms, reason, ..
                            } => {
                                scheduler.arm_cron(sid_for_arm, run_id_for_arm, fire_at_ms, reason);
                            }
                        }
                        // 用 EndTurn 收 turn，但**不**走下文的 RunFinished 路径——
                        // 直接走 break 让 result = Err(Cancelled-like) 不合适。
                        // 取巧：emit TurnFinished 后 break 出循环，result 保持
                        // 上一个 step 的状态；外层把 Suspended 视为 Run 没结束。
                        turn_span.record(attr::STOP_REASON, "suspended");
                        emit(EventPayload::TurnFinished {
                            turn_id: turn_id.clone(),
                            turn: turn_index,
                            stop_reason: StopReason::EndTurn,
                        });
                        break Err(model_gateway::types::ModelError::Suspended);
                    }
                }

                turn_span.record(attr::STOP_REASON, "end_turn");
                emit(EventPayload::TurnFinished {
                    turn_id,
                    turn: turn_index,
                    stop_reason: StopReason::EndTurn,
                });
            }
        }
    };

    let duration_ms = run_start.elapsed().as_millis() as u64;
    set_pending_inputs_accepting(pending_inputs_accepting.as_ref(), false);
    run_span.record("hebbian.run.iterations", iteration);
    // 本 Run 结束（非挂起、非嵌套子 Run）→ 对比 Run 开始至今所有触达文件的净变化，
    // emit RunEditsCommitted。挂起态 Run 仍 Active，留到真正终结时再 finalize。
    if !is_nested_run && !matches!(result, Err(ModelError::Suspended)) {
        commit_run_edits(&edits_worktree, &state, &on_event, &edits_run_id).await;
    }
    match &result {
        Ok(_) => {
            run_span.record("hebbian.run.outcome", attr::run_outcome::DONE);
            // run 收尾落盘（架构 §4.9.5）：补落最后一段 assistant + 删 partial。
            if let Some(p) = persister.as_ref() {
                p.finish(duration_ms).await;
            }
            emit(EventPayload::RunFinished {
                total_input_tokens,
                total_output_tokens,
                total_cache_read_tokens,
                total_cache_creation_tokens,
                duration_ms,
            });
        }
        Err(ModelError::Cancelled) => {
            run_span.record("hebbian.run.outcome", attr::run_outcome::CANCELLED);
            // cancel 收尾（架构 §4.9.5）：补落残留尾段 + Interrupted marker，删 partial。
            if let Some(p) = persister.as_ref() {
                p.finish_interrupted().await;
            }
            emit(EventPayload::RunCancelled);
        }
        Err(ModelError::Suspended) => {
            // 挂起态：本 task 退出，但 Run 仍 Active。不发 RunFinished / RunCancelled——
            // RunSuspended 已在 break 前 emit；resume_run 时由 Harness 复活同一个 Run。
            run_span.record("hebbian.run.outcome", "suspended");
            // 挂起也算一段达边界：补落本段 assistant + 删 partial（架构 §4.9.5）。
            if let Some(p) = persister.as_ref() {
                p.finish(duration_ms).await;
            }
        }
        Err(e) => {
            run_span.record("hebbian.run.outcome", attr::run_outcome::FAILED);
            // fail 收尾（架构 §4.9.5）：补落残留尾段 + Interrupted marker，删 partial。
            if let Some(p) = persister.as_ref() {
                p.finish_interrupted().await;
            }
            emit(EventPayload::RunFailed {
                error: ErrorReport::other(e.to_string()),
            });
        }
    }

    // 架构 §4.3：非正常结束 → 弹 toast + 落 pending_continue；正常完成 → 清空残留。
    // 挂起态不在此处理（Run 未结束，稍后会复活）。
    if !matches!(result, Err(ModelError::Suspended)) {
        let pending = continue_for_outcome(&result, &last_finish);
        if let Some((kind, ref message)) = pending {
            // kind 编进 dedup_key：surface 据此当场把 pending_continue 同步进内存态
            // （让 ContinueBar 立刻出现），无需等磁盘重载。`{kind:?}` 是 snake_case 之外的
            // 形态，前端按枚举值小写匹配即可。
            emit(EventPayload::Notice {
                level: LogLevel::Warn,
                message: message.clone(),
                dedup_key: Some(format!(
                    "pending-continue-{}-{}",
                    crate::storage::sessions::continue_kind_str(kind),
                    state.run_id
                )),
            });
        }
        if let (Some(dd), Some(sid)) = (data_dir.as_ref(), session_id.as_deref()) {
            let to_write =
                pending.map(
                    |(kind, message)| crate::storage::sessions::PendingContinue {
                        at: chrono::Utc::now().timestamp_millis(),
                        kind,
                        message,
                    },
                );
            if let Err(e) = crate::storage::sessions::set_pending_continue(dd, sid, to_write) {
                tracing::warn!(error = %e, "persist pending_continue failed");
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use common::AppResult;
    use model_gateway::types::{
        ModelRequest, ModelResponse, ModelStreamEvent, ToolCall, TranscriptEntry, Usage,
    };
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    };

    #[test]
    fn continue_outcome_normal_stop_is_silent() {
        use crate::storage::sessions::ContinueKind;
        let ok: Result<AssistantOutput, ModelError> = Ok(AssistantOutput::default());
        assert!(continue_for_outcome(&ok, &FinishReason::Stop).is_none());
        // 截断 → 续写
        let (kind, _) = continue_for_outcome(&ok, &FinishReason::Length).unwrap();
        assert_eq!(kind, ContinueKind::Truncated);
        // 取消 / 挂起不留续作
        let cancelled: Result<AssistantOutput, ModelError> = Err(ModelError::Cancelled);
        assert!(continue_for_outcome(&cancelled, &FinishReason::Stop).is_none());
        // 网络失败 → 重试
        let failed: Result<AssistantOutput, ModelError> = Err(ModelError::Other("boom".into()));
        let (kind, _) = continue_for_outcome(&failed, &FinishReason::Stop).unwrap();
        assert_eq!(kind, ContinueKind::NetworkError);
    }

    struct StubTool;

    #[async_trait]
    impl crate::tools::Tool for StubTool {
        fn name(&self) -> &str {
            "Stub"
        }

        fn description(&self) -> &str {
            "stub tool"
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }

        async fn execute(&self, _input: Value) -> AppResult<String> {
            Ok("工具结果".repeat(12_000))
        }
    }

    struct FailingModelClient;

    #[async_trait]
    impl ModelClient for FailingModelClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            Err(ModelError::Other("model rejected request".to_string()))
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
            _on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            Err(ModelError::Other("model rejected request".to_string()))
        }
    }

    struct DoneOnceClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for DoneOnceClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
            _on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "完成".to_string(),
                reasoning: String::new(),
                attachments: Vec::new(),
                usage: Usage::default(),
                reasoning_signature: String::new(),
            })
        }
    }

    struct PendingInputDuringDoneClient {
        calls: AtomicUsize,
        pending_inputs: PendingInputs,
    }

    #[async_trait]
    impl ModelClient for PendingInputDuringDoneClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第一段".to_string(),
                    });
                    self.pending_inputs
                        .lock()
                        .unwrap()
                        .push(common::runtime::PendingUserInput {
                            content: "插队消息".to_string(),
                            attachments: Vec::new(),
                        });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "第一段".to_string(),
                        reasoning: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                        reasoning_signature: String::new(),
                    })
                }
                1 => {
                    let saw_injected = req.entries.iter().any(|entry| {
                        matches!(
                            entry,
                            TranscriptEntry::User(user) if user.text == "插队消息"
                        )
                    });
                    assert!(
                        saw_injected,
                        "second model request should see injected user input"
                    );
                    on_event(ModelStreamEvent::TextDelta {
                        text: "引导后的回答".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "引导后的回答".to_string(),
                        reasoning: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                        reasoning_signature: String::new(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    /// 复现 issue #68354 / session 202606160757-eeb33d38：模型把工具调用 XML 漏进正文。
    /// 第 0 次回 Done 但 text 是「游离 court + <invoke> 残骸」，第 1 次回干净回答。
    struct LeakedToolXmlClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for LeakedToolXmlClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            _on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(ModelResponse::Done {
                    finish: model_gateway::types::FinishReason::Other("tool_use".to_string()),
                    text: "现在改文件。\ncourt\n<invoke name=\"Edit\">\n<parameter name=\"file_path\">/tmp/a.ts</parameter>\n</invoke>"
                        .to_string(),
                    reasoning: String::new(),
                    attachments: Vec::new(),
                    usage: Usage::default(),
                    reasoning_signature: String::new(),
                }),
                1 => {
                    // 续跑请求里必须能看到纠错提示，且历史里那条 assistant 已被清洗——
                    // 不含任何 <invoke> 残骸（自我强化的燃料被掐断）。
                    let saw_correction = req.entries.iter().any(|entry| {
                        matches!(entry, TranscriptEntry::User(u) if u.text.contains("tool-format-error"))
                    });
                    assert!(saw_correction, "续跑请求应注入 tool-format-error 纠错提示");
                    let leaked_in_history = req.entries.iter().any(|entry| {
                        matches!(entry, TranscriptEntry::Assistant(a) if a.text.contains("<invoke"))
                    });
                    assert!(!leaked_in_history, "历史里的 assistant 文本必须已清洗，不得残留 <invoke>");
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "已用正确方式改好文件。".to_string(),
                        reasoning: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                        reasoning_signature: String::new(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct AutoCompactOrderingClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for AutoCompactOrderingClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            assert_eq!(req.meta.tag, model_gateway::types::ModelCallTag::Compaction);
            Ok(ModelResponse::Done {
                finish: model_gateway::types::FinishReason::Stop,
                text: "压缩摘要".to_string(),
                reasoning: String::new(),
                attachments: Vec::new(),
                usage: Usage::default(),
                reasoning_signature: String::new(),
            })
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            if req.meta.tag == model_gateway::types::ModelCallTag::Compaction {
                on_event(ModelStreamEvent::TextDelta {
                    text: "压缩摘要".to_string(),
                });
                return Ok(ModelResponse::Done {
                    finish: model_gateway::types::FinishReason::Stop,
                    text: "压缩摘要".to_string(),
                    reasoning: String::new(),
                    attachments: Vec::new(),
                    usage: Usage::default(),
                    reasoning_signature: String::new(),
                });
            }
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "压缩前输出".to_string(),
                    });
                    Ok(ModelResponse::ToolCalls {
                        text: "压缩前输出".to_string(),
                        reasoning: String::new(),
                        calls: vec![ToolCall {
                            id: "call_stub".to_string(),
                            name: "Stub".to_string(),
                            input: json!({}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                        reasoning_signature: String::new(),
                    })
                }
                1 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "压缩后输出".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "压缩后输出".to_string(),
                        reasoning: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                        reasoning_signature: String::new(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct PendingInputAfterTurnFinishedClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for PendingInputAfterTurnFinishedClient {
        fn provider_id(&self) -> &str {
            "test"
        }

        fn supports_streaming_tools(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: ModelRequest,
            _cancel: CancelFlag,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!("test uses streaming")
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancelFlag,
            on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        ) -> Result<ModelResponse, ModelError> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "先查工具".to_string(),
                    });
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        reasoning: String::new(),
                        calls: vec![model_gateway::types::ToolCall {
                            id: "call_missing".to_string(),
                            name: "missing_tool".to_string(),
                            input: serde_json::json!({}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                        reasoning_signature: String::new(),
                    })
                }
                1 => {
                    let saw_injected = req.entries.iter().any(|entry| {
                        matches!(
                            entry,
                            TranscriptEntry::User(user) if user.text == "ToolStep 后的引导"
                        )
                    });
                    assert!(
                        saw_injected,
                        "next model request after TurnFinished should drain pending user input"
                    );
                    on_event(ModelStreamEvent::TextDelta {
                        text: "已按引导继续".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        finish: model_gateway::types::FinishReason::Stop,
                        text: "已按引导继续".to_string(),
                        reasoning: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                        reasoning_signature: String::new(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    fn assistant_message_has_text(message: &crate::storage::sessions::Message, needle: &str) -> bool {
        message.role == crate::storage::sessions::Role::Assistant
            && (message.content.contains(needle)
                || message.parts.iter().any(|part| {
                    matches!(part, crate::storage::sessions::MessagePart::Text { text } if text.contains(needle))
                }))
    }

    #[tokio::test]
    async fn automatic_compaction_marker_splits_persisted_run_segments() {
        let data_dir = tempfile::tempdir().unwrap();
        let session = crate::storage::sessions::create(
            data_dir.path(),
            "test".into(),
            "test-model".into(),
            None,
            None,
        )
        .unwrap();
        let session_id = session.id.clone();
        let mut transcript = Transcript::new(None);
        transcript.push_user("请运行工具后继续".to_string(), Vec::new());
        crate::storage::sessions::append_message(
            data_dir.path(),
            &session_id,
            crate::storage::sessions::Message {
                id: crate::storage::sessions::new_id(),
                role: crate::storage::sessions::Role::User,
                content: "请运行工具后继续".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: chrono::Utc::now().timestamp_millis(),
                meta: None,
                subagent_call_id: None,
                run_duration_ms: None,
            },
        )
        .unwrap();

        let client = AutoCompactOrderingClient {
            calls: AtomicUsize::new(0),
        };
        let state = Arc::new(RunState::new(RunId::new()));
        let workspace = Workspace::new(data_dir.path().to_path_buf(), Vec::new());
        let persister = crate::run_persister::RunPersister::new(
            data_dir.path().to_path_buf(),
            session_id.clone(),
        );
        let persister_handle = persister.handle();
        let mut policy = CompactionPolicy::default();
        policy.token_budget = 10_500;

        let result = run_loop(
            LoopParams {
                client: &client,
                registry: Arc::new(ToolRegistry::new(vec![Box::new(StubTool)])),
                hitl: Arc::new(HitlGate::default()),
                hooks: Arc::new(HookManager::empty()),
                transcript: &mut transcript,
                enabled_tools: &["Stub".to_string()],
                compaction_policy: &policy,
                workspace,
                stream: true,
                cancel: Arc::new(AtomicBool::new(false)),
                state,
                agent: AgentRef::new("test"),
                parent: None,
                model_io_dump: None,
                pending_inputs: None,
                consumed_pending_inputs: None,
                pending_inputs_accepting: None,
                run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
                model_id: None,
                judge_client: None,
                force_automode: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                data_dir: Some(data_dir.path().to_path_buf()),
                session_id: Some(session_id.clone()),
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,
                subagent_ctx: None,
                subagent_bypass: false,
                persister: Some(persister),
                call_tag: Default::default(),
            },
            Arc::new(move |event| {
                persister_handle.observe(&event);
            }),
        )
        .await
        .expect("run should finish");

        assert_eq!(result.text, "压缩后输出");
        let loaded = crate::storage::sessions::load(data_dir.path(), &session_id).unwrap();
        let before_idx = loaded
            .messages
            .iter()
            .position(|m| assistant_message_has_text(m, "压缩前输出"))
            .expect("压缩前 assistant 段应先落盘");
        let boundary_idx = loaded
            .messages
            .iter()
            .position(|m| matches!(m.meta, Some(crate::storage::sessions::MessageMeta::CompactBoundary { .. })))
            .expect("应落 compact boundary");
        let after_idx = loaded
            .messages
            .iter()
            .position(|m| assistant_message_has_text(m, "压缩后输出"))
            .expect("压缩后 assistant 段应落盘");

        assert!(
            before_idx < boundary_idx && boundary_idx < after_idx,
            "压缩 marker 必须切开同一 run 的前后输出，实际顺序: before={before_idx}, boundary={boundary_idx}, after={after_idx}"
        );
    }

    #[tokio::test]
    async fn model_call_error_emits_run_failed_before_returning() {
        let mut transcript = Transcript::new(None);
        transcript.push_user("hi".to_string(), Vec::new());

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_sink = Arc::clone(&events);
        let state = Arc::new(RunState::new(RunId::new()));
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path().to_path_buf(), Vec::new());

        let result = run_loop(
            LoopParams {
                client: &FailingModelClient,
                registry: Arc::new(ToolRegistry::new(Vec::new())),
                hitl: Arc::new(HitlGate::default()),
                hooks: Arc::new(HookManager::empty()),
                transcript: &mut transcript,
                enabled_tools: &[],
                compaction_policy: &CompactionPolicy::default(),
                workspace,
                stream: true,
                cancel: Arc::new(AtomicBool::new(false)),
                state,
                agent: AgentRef::new("test"),
                parent: None,
                model_io_dump: None,
                pending_inputs: None,
                consumed_pending_inputs: None,
                pending_inputs_accepting: None,
                run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
                model_id: None,
                judge_client: None,
                force_automode: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,

                subagent_ctx: None,
                subagent_bypass: false,
                persister: None,
                call_tag: Default::default(),
            },
            Arc::new(move |event| {
                events_for_sink.lock().unwrap().push(event.payload);
            }),
        )
        .await;

        assert!(matches!(result, Err(ModelError::Other(_))));
        let events = events.lock().unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, EventPayload::RunFailed { .. })));
    }

    #[tokio::test]
    async fn normal_done_does_not_start_an_extra_model_step_or_compaction() {
        let mut transcript = Transcript::new(None);
        transcript.push_user("hi".to_string(), Vec::new());

        let client = DoneOnceClient {
            calls: AtomicUsize::new(0),
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_sink = Arc::clone(&events);
        let state = Arc::new(RunState::new(RunId::new()));
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path().to_path_buf(), Vec::new());
        let policy = CompactionPolicy::default();

        let result = run_loop(
            LoopParams {
                client: &client,
                registry: Arc::new(ToolRegistry::new(Vec::new())),
                hitl: Arc::new(HitlGate::default()),
                hooks: Arc::new(HookManager::empty()),
                transcript: &mut transcript,
                enabled_tools: &[],
                compaction_policy: &policy,
                workspace,
                stream: true,
                cancel: Arc::new(AtomicBool::new(false)),
                state,
                agent: AgentRef::new("test"),
                parent: None,
                model_io_dump: None,
                pending_inputs: None,
                consumed_pending_inputs: None,
                pending_inputs_accepting: None,
                run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
                model_id: None,
                judge_client: None,
                force_automode: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,
                subagent_ctx: None,
                subagent_bypass: false,
                persister: None,
                call_tag: Default::default(),
            },
            Arc::new(move |event| {
                events_for_sink.lock().unwrap().push(event.payload);
            }),
        )
        .await
        .expect("run should finish normally");

        assert_eq!(result.text, "完成");
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        let events = events.lock().unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, EventPayload::RunFinished { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, EventPayload::ContextCompacted { .. })));
    }

    #[tokio::test]
    async fn pending_input_during_final_model_step_continues_same_run() {
        let mut transcript = Transcript::new(None);
        transcript.push_user("hi".to_string(), Vec::new());

        let pending_inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
        let consumed_pending_inputs: ConsumedPendingInputs = Arc::new(Mutex::new(Vec::new()));
        let client = PendingInputDuringDoneClient {
            calls: AtomicUsize::new(0),
            pending_inputs: pending_inputs.clone(),
        };
        let state = Arc::new(RunState::new(RunId::new()));
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path().to_path_buf(), Vec::new());

        let result = run_loop(
            LoopParams {
                client: &client,
                registry: Arc::new(ToolRegistry::new(Vec::new())),
                hitl: Arc::new(HitlGate::default()),
                hooks: Arc::new(HookManager::empty()),
                transcript: &mut transcript,
                enabled_tools: &[],
                compaction_policy: &CompactionPolicy::default(),
                workspace,
                stream: true,
                cancel: Arc::new(AtomicBool::new(false)),
                state,
                agent: AgentRef::new("test"),
                parent: None,
                model_io_dump: None,
                pending_inputs: Some(pending_inputs),
                consumed_pending_inputs: Some(consumed_pending_inputs.clone()),
                pending_inputs_accepting: None,
                run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
                model_id: None,
                judge_client: None,
                force_automode: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,

                subagent_ctx: None,
                subagent_bypass: false,
                persister: None,
                call_tag: Default::default(),
            },
            Arc::new(|_| {}),
        )
        .await
        .expect("run should complete after following injected input");

        assert_eq!(result.text, "引导后的回答");
        assert_eq!(client.calls.load(Ordering::SeqCst), 2);
        let consumed = consumed_pending_inputs.lock().unwrap();
        assert_eq!(consumed.len(), 1);
        assert_eq!(consumed[0].content, "插队消息");
        assert!(matches!(
            transcript.entries.as_slice(),
            [
                TranscriptEntry::User(_),
                TranscriptEntry::Assistant(first),
                TranscriptEntry::User(user),
                TranscriptEntry::Assistant(second)
            ] if first.text == "第一段"
                && user.text == "插队消息"
                && second.text == "引导后的回答"
        ));
    }

    /// 回归（架构 §4.3.3 / issue #68354）：模型把工具调用 XML 漏进正文时，agent_loop
    /// 自动清洗 + 注入纠错续跑；下一轮请求看不到残骸（自我强化被根治），最终返回干净文本。
    #[tokio::test]
    async fn leaked_tool_xml_is_sanitized_and_turn_resumes() {
        let mut transcript = Transcript::new(None);
        transcript.push_user("改个文件".to_string(), Vec::new());

        let client = LeakedToolXmlClient {
            calls: AtomicUsize::new(0),
        };
        let state = Arc::new(RunState::new(RunId::new()));
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path().to_path_buf(), Vec::new());

        let result = run_loop(
            LoopParams {
                client: &client,
                registry: Arc::new(ToolRegistry::new(Vec::new())),
                hitl: Arc::new(HitlGate::default()),
                hooks: Arc::new(HookManager::empty()),
                transcript: &mut transcript,
                enabled_tools: &[],
                compaction_policy: &CompactionPolicy::default(),
                workspace,
                stream: true,
                cancel: Arc::new(AtomicBool::new(false)),
                state,
                agent: AgentRef::new("test"),
                parent: None,
                model_io_dump: None,
                pending_inputs: None,
                consumed_pending_inputs: None,
                pending_inputs_accepting: None,
                run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
                model_id: None,
                judge_client: None,
                force_automode: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,
                subagent_ctx: None,
                subagent_bypass: false,
                persister: None,
                call_tag: Default::default(),
            },
            Arc::new(|_| {}),
        )
        .await
        .expect("run should recover from leaked tool xml");

        assert_eq!(result.text, "已用正确方式改好文件。");
        assert_eq!(client.calls.load(Ordering::SeqCst), 2);
        // 返回上层的文本不含残骸。
        assert!(!result.text.contains("<invoke"));
        // 历史里那条漏出残骸的 assistant 已被清洗成纯净前导文本。
        let cleaned = transcript.entries.iter().any(
            |entry| matches!(entry, TranscriptEntry::Assistant(a) if a.text == "现在改文件。"),
        );
        assert!(
            cleaned,
            "漏出残骸的 assistant 文本应被清洗为「现在改文件。」"
        );
        let any_leak = transcript.entries.iter().any(
            |entry| matches!(entry, TranscriptEntry::Assistant(a) if a.text.contains("<invoke")),
        );
        assert!(
            !any_leak,
            "transcript 任何 assistant 文本都不得残留 <invoke>"
        );
    }

    #[tokio::test]
    async fn pending_input_after_tool_step_checkpoint_is_drained_before_next_model_request() {
        let mut transcript = Transcript::new(None);
        transcript.push_user("hi".to_string(), Vec::new());

        let pending_inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
        let consumed_pending_inputs: ConsumedPendingInputs = Arc::new(Mutex::new(Vec::new()));
        let client = PendingInputAfterTurnFinishedClient {
            calls: AtomicUsize::new(0),
        };
        let state = Arc::new(RunState::new(RunId::new()));
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path().to_path_buf(), Vec::new());
        let pending_for_sink = pending_inputs.clone();
        let injected_once = Arc::new(AtomicBool::new(false));
        let injected_once_for_sink = injected_once.clone();

        let result = run_loop(
            LoopParams {
                client: &client,
                registry: Arc::new(ToolRegistry::new(Vec::new())),
                hitl: Arc::new(HitlGate::default()),
                hooks: Arc::new(HookManager::empty()),
                transcript: &mut transcript,
                enabled_tools: &[],
                compaction_policy: &CompactionPolicy::default(),
                workspace,
                stream: true,
                cancel: Arc::new(AtomicBool::new(false)),
                state,
                agent: AgentRef::new("test"),
                parent: None,
                model_io_dump: None,
                pending_inputs: Some(pending_inputs),
                consumed_pending_inputs: Some(consumed_pending_inputs.clone()),
                pending_inputs_accepting: None,
                run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
                model_id: None,
                judge_client: None,
                force_automode: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,

                subagent_ctx: None,
                subagent_bypass: false,
                persister: None,
                call_tag: Default::default(),
            },
            Arc::new(move |event| {
                if matches!(event.payload, EventPayload::TurnFinished { .. })
                    && !injected_once_for_sink.swap(true, Ordering::SeqCst)
                {
                    pending_for_sink
                        .lock()
                        .unwrap()
                        .push(common::runtime::PendingUserInput {
                            content: "ToolStep 后的引导".to_string(),
                            attachments: Vec::new(),
                        });
                }
            }),
        )
        .await
        .expect("run should continue with pending input from the turn boundary");

        assert_eq!(result.text, "已按引导继续");
        assert_eq!(client.calls.load(Ordering::SeqCst), 2);
        let consumed = consumed_pending_inputs.lock().unwrap();
        assert_eq!(consumed.len(), 1);
        assert_eq!(consumed[0].content, "ToolStep 后的引导");
        assert!(matches!(
            transcript.entries.as_slice(),
            [
                TranscriptEntry::User(_),
                TranscriptEntry::Assistant(_),
                TranscriptEntry::ToolResults(_),
                TranscriptEntry::User(user),
                TranscriptEntry::Assistant(second)
            ] if user.text == "ToolStep 后的引导"
                && second.text == "已按引导继续"
        ));
    }

    #[tokio::test]
    async fn max_tool_iterations_limits_loop() {
        struct AlwaysToolCallClient {
            calls: AtomicUsize,
        }

        #[async_trait]
        impl ModelClient for AlwaysToolCallClient {
            fn provider_id(&self) -> &str {
                "test"
            }

            fn supports_streaming_tools(&self) -> bool {
                true
            }

            async fn complete(
                &self,
                _req: ModelRequest,
                _cancel: CancelFlag,
            ) -> Result<ModelResponse, ModelError> {
                unreachable!("test uses streaming")
            }

            async fn stream(
                &self,
                _req: ModelRequest,
                _cancel: CancelFlag,
                on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
            ) -> Result<ModelResponse, ModelError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                on_event(ModelStreamEvent::TextDelta {
                    text: "tool call".to_string(),
                });
                Ok(ModelResponse::ToolCalls {
                    text: "tool call".to_string(),
                    reasoning: String::new(),
                    calls: vec![model_gateway::types::ToolCall {
                        id: "call_always".to_string(),
                        name: "missing_tool".to_string(),
                        input: serde_json::json!({}),
                    }],
                    attachments: Vec::new(),
                    usage: Usage::default(),
                    reasoning_signature: String::new(),
                })
            }
        }

        let mut transcript = Transcript::new(None);
        transcript.push_user("反复调用工具".to_string(), Vec::new());

        let client = AlwaysToolCallClient {
            calls: AtomicUsize::new(0),
        };
        let state = Arc::new(RunState::new(RunId::new()));
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path().to_path_buf(), Vec::new());

        let err = run_loop(
            LoopParams {
                client: &client,
                registry: Arc::new(ToolRegistry::new(Vec::new())),
                hitl: Arc::new(HitlGate::default()),
                hooks: Arc::new(HookManager::empty()),
                transcript: &mut transcript,
                enabled_tools: &[],
                compaction_policy: &CompactionPolicy::default(),
                workspace,
                stream: true,
                cancel: Arc::new(AtomicBool::new(false)),
                state,
                agent: AgentRef::new("test"),
                parent: None,
                model_io_dump: None,
                pending_inputs: None,
                consumed_pending_inputs: None,
                pending_inputs_accepting: None,
                run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
                model_id: None,
                judge_client: None,
                force_automode: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: Some(2),
                system_rules: None,

                subagent_ctx: None,
                subagent_bypass: false,
                persister: None,
                call_tag: Default::default(),
            },
            Arc::new(|_| {}),
        )
        .await
        .expect_err("should fail when max_tool_iterations exceeded");

        assert!(
            matches!(&err, ModelError::Other(msg) if msg.contains("已达到最大工具调用轮数 2")),
            "error should mention max iterations, got: {err:?}"
        );
        // iteration check happens after model call: iterations 0,1,2 → 3 calls
        assert_eq!(client.calls.load(Ordering::SeqCst), 3);
    }

    /// 主 client 每个 turn 都自然结束（end_turn）；judge 先判 NotYet 再判 Achieved。
    /// 验证 goal 闭环：NotYet 注入 `<goal-feedback>` 续跑 + emit GoalProgress；
    /// Achieved 清目标 + emit GoalAchieved + 正常出 turn。
    #[tokio::test]
    async fn goal_notyet_then_achieved_drives_resume_and_clear() {
        // 主 client：每轮都 Done(end_turn)，让 loop 落进 Stop 分支跑 goal 裁决。
        struct DoneEachTurnClient {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl ModelClient for DoneEachTurnClient {
            fn provider_id(&self) -> &str {
                "test"
            }
            fn supports_streaming_tools(&self) -> bool {
                true
            }
            async fn complete(
                &self,
                _req: ModelRequest,
                _cancel: CancelFlag,
            ) -> Result<ModelResponse, ModelError> {
                unreachable!("test uses streaming")
            }
            async fn stream(
                &self,
                _req: ModelRequest,
                _cancel: CancelFlag,
                on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
            ) -> Result<ModelResponse, ModelError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let text = format!("第 {n} 轮收尾");
                on_event(ModelStreamEvent::TextDelta { text: text.clone() });
                Ok(ModelResponse::Done {
                    finish: model_gateway::types::FinishReason::Stop,
                    text,
                    reasoning: String::new(),
                    attachments: Vec::new(),
                    usage: Usage::default(),
                    reasoning_signature: String::new(),
                })
            }
        }

        // judge client：第 1 次 NotYet，第 2 次 Achieved。走 complete（goal judge 用 complete）。
        // calls 用 Arc 共享，run 结束后可在外部断言裁决被调了几次。
        struct JudgeClient {
            calls: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl ModelClient for JudgeClient {
            fn provider_id(&self) -> &str {
                "judge"
            }
            async fn complete(
                &self,
                _req: ModelRequest,
                _cancel: CancelFlag,
            ) -> Result<ModelResponse, ModelError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let json = if n == 0 {
                    r#"{"ok": false, "reason": "还差一个测试没过"}"#
                } else {
                    r#"{"ok": true, "reason": "全部测试通过"}"#
                };
                Ok(ModelResponse::Done {
                    finish: model_gateway::types::FinishReason::Stop,
                    text: json.to_string(),
                    reasoning: String::new(),
                    attachments: Vec::new(),
                    usage: Usage::default(),
                    reasoning_signature: String::new(),
                })
            }
            async fn stream(
                &self,
                _req: ModelRequest,
                _cancel: CancelFlag,
                _on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
            ) -> Result<ModelResponse, ModelError> {
                unreachable!("goal judge uses complete")
            }
        }

        let data_dir = tempfile::tempdir().unwrap();
        let session = crate::storage::sessions::create(
            data_dir.path(),
            "test".into(),
            "test-model".into(),
            None,
            None,
        )
        .unwrap();
        let session_id = session.id.clone();
        crate::storage::sessions::set_active_goal(
            data_dir.path(),
            &session_id,
            Some(crate::storage::sessions::ActiveGoal {
                condition: "所有测试通过".into(),
                created_at: 1,
                iterations: 0,
                last_reason: None,
                pending_set_marker: false,
            }),
        )
        .unwrap();

        let mut transcript = Transcript::new(None);
        transcript.push_user("把测试修绿".to_string(), Vec::new());

        let client = DoneEachTurnClient {
            calls: AtomicUsize::new(0),
        };
        let judge_calls = Arc::new(AtomicUsize::new(0));
        let judge: Arc<dyn ModelClient> = Arc::new(JudgeClient {
            calls: Arc::clone(&judge_calls),
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_sink = Arc::clone(&events);
        let state = Arc::new(RunState::new(RunId::new()));
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(tmp.path().to_path_buf(), Vec::new());

        let result = run_loop(
            LoopParams {
                client: &client,
                registry: Arc::new(ToolRegistry::new(Vec::new())),
                hitl: Arc::new(HitlGate::default()),
                hooks: Arc::new(HookManager::empty()),
                transcript: &mut transcript,
                enabled_tools: &[],
                compaction_policy: &CompactionPolicy::default(),
                workspace,
                stream: true,
                cancel: Arc::new(AtomicBool::new(false)),
                state,
                agent: AgentRef::new("test"),
                parent: None,
                model_io_dump: None,
                pending_inputs: None,
                consumed_pending_inputs: None,
                pending_inputs_accepting: None,
                run_mode: Arc::new(std::sync::Mutex::new(crate::run_mode::RunMode::Default)),
                model_id: Some("test-model".into()),
                judge_client: Some(judge),
                force_automode: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                data_dir: Some(data_dir.path().to_path_buf()),
                session_id: Some(session_id.clone()),
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,

                subagent_ctx: None,
                subagent_bypass: false,
                persister: None,
                call_tag: Default::default(),
            },
            Arc::new(move |event| {
                events_for_sink.lock().unwrap().push(event.payload);
            }),
        )
        .await;

        assert!(result.is_ok(), "goal achieved 应正常出 turn: {result:?}");
        // 主 client 跑两轮：turn1 → NotYet 续跑 → turn2 → Achieved 收尾。
        assert_eq!(client.calls.load(Ordering::SeqCst), 2);
        // judge 恰好裁决两次：turn1 末判 NotYet、turn2 末判 Achieved。
        assert_eq!(
            judge_calls.load(Ordering::SeqCst),
            2,
            "judge 应被调用 2 次（每轮 turn 末各一次）"
        );

        let result = result.unwrap();
        // 最终返回的是第 2 轮（n=1）收尾文本。
        assert_eq!(
            result.text, "第 1 轮收尾",
            "应返回第 2 轮的收尾文本，而非首轮"
        );

        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EventPayload::GoalProgress { iteration: 1, .. })),
            "应 emit GoalProgress(iteration=1)"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EventPayload::GoalAchieved { .. })),
            "应 emit GoalAchieved"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, EventPayload::GoalImpossible { .. })),
            "不应 emit GoalImpossible"
        );

        // NotYet 只注入一条 <goal-feedback> user message（恰好一次，不多不少）。
        let feedback_count = transcript
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    TranscriptEntry::User(u) if u.text.contains("<goal-feedback>")
                        && u.text.contains("还差一个测试没过")
                )
            })
            .count();
        assert_eq!(
            feedback_count, 1,
            "transcript 应恰好含一条注入的 goal-feedback 续跑提示"
        );

        // Achieved 后目标被清空（落盘可见）。
        let reloaded = crate::storage::sessions::load(data_dir.path(), &session_id).unwrap();
        assert_eq!(reloaded.active_goal, None, "Achieved 后 active_goal 应清空");

        // 每次裁决都落了一条 GoalOutcome marker（progress 一条 + achieved 一条），
        // 供 UI 渲染成彩色竖线结果块、随会话持久化、重启可重建。
        let goal_markers: Vec<&str> = reloaded
            .messages
            .iter()
            .filter_map(|m| match &m.meta {
                Some(crate::storage::sessions::MessageMeta::GoalOutcome { kind, .. }) => {
                    Some(kind.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            goal_markers,
            vec!["progress", "achieved"],
            "应按序落 progress 与 achieved 两条 GoalOutcome marker"
        );
    }
}
