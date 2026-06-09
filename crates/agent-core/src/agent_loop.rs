use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

use observability::attr;
use protocol::{AgentRef, ErrorReport, Event, EventPayload, LogLevel, RunId, StopReason};
use tracing::{debug, field::Empty, info, Instrument};

use crate::{
    context::{
        compaction::{build_compaction_request, compact_request_with_llm, needs_compaction},
        microcompact::{microcompact, MicrocompactPolicy},
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

/// 单次 ModelStep 非正常退出后的自动重试上限（架构 §4.3）。指数退避，每次 emit toast。
/// 与 model-gateway 的 `retry_request`（包初始 HTTP 发送的快速瞬时重试）正交：这一层
/// 是「整轮模型调用」的用户可见重试，覆盖 SSE 流内 error / 上游 overloaded 等场景。
const MAX_MODEL_RETRIES: u32 = 5;

/// 第 `attempt`（从 1 起）次重试前的退避时长：1s / 2s / 4s / 8s / 16s 封顶。
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
    /// `force_automode` 子开关（架构 §4.4.4）。仅 [`crate::run_mode::RunMode::AutoMode`]
    /// 下生效：判官返回 `Ask` 时折叠成 `Deny`，让"放手跑"模式不被打断。
    /// 由 CLI flag `--force-automode` 或 REPL `/force-automode` 切换。
    pub force_automode: bool,
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
}

/// 把 [`compose_system_prompt`] 重新导出为旧名字，方便其它 crate 沿用。
/// 内部已经不再混入 workspace XML——环境信息走第一条 user message 的 `<environment>` 块。
pub use crate::system_prompt::compose_system_prompt as build_system_prompt;

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

fn drain_pending_inputs(
    pending_inputs: Option<&PendingInputs>,
    consumed_pending_inputs: Option<&ConsumedPendingInputs>,
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
        transcript.push_user(input.content, input.attachments);
    }
    drained_len
}

fn set_pending_inputs_accepting(flag: Option<&Arc<AtomicBool>>, accepting: bool) {
    if let Some(flag) = flag {
        flag.store(accepting, Ordering::SeqCst);
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
    } = params;

    let emit = |payload: EventPayload| on_event(state.event(payload));
    let run_span = tracing::Span::current();

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
        info!(?rs.cause, "run resumed");
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
        info!("run started");
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

    let run_start = Instant::now();
    let mut output_attachments = Vec::new();
    // Stop hook 已经在本 Run 注入了几次（架构 §4.8.3 防死循环），上限
    // `MAX_STOP_INJECTIONS` 后即使脚本继续 inject 也忽略，turn 正常出。
    let mut stop_hook_injections: u32 = 0;
    // 最后一个 ModelStep 的归一结束原因（架构 §4.11.4）。run 正常收尾后据此判断
    // 是否要在 surface 弹 toast + 写 pending_continue（架构 §4.3）。
    let mut last_finish = FinishReason::Stop;

    let result: Result<AssistantOutput, ModelError> = loop {
        if cancellation::is_cancelled(&cancel) {
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
            transcript,
        );

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
            let compaction_outcome =
                compact_request_with_llm(client, compact_req, compact_before_tokens).await;
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
        // PlanMode 工具过滤（架构 §4.4.3 / §4.4.5）：删除会改外界的工具，强制 agent 走
        // 只读探索路径；同时注入 ExitPlanMode 工具让 agent 主动结束规划。
        let current_run_mode = *run_mode.lock().unwrap();
        if current_run_mode == crate::run_mode::RunMode::PlanMode {
            let mutating = ["Bash", "PowerShell", "Edit"];
            tool_defs.retain(|t| !mutating.contains(&t.name.as_str()));
            let extra = registry.definitions(&["ExitPlanMode".to_string()]);
            tool_defs.extend(extra);
        } else {
            // 其他模式不暴露 ExitPlanMode，避免误调用
            tool_defs.retain(|t| t.name != "ExitPlanMode");
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
            model: String::new(),
            system: Some(combined_system),
            entries: transcript.entries.clone(),
            tools: tool_defs,
            max_tokens: 8192,
            reasoning: None,
        };

        debug!(iteration, "calling model");
        hooks
            .trigger(&HookPoint::BeforeModelCall { turn: turn_index })
            .await;
        let call_start = Instant::now();

        // 启用 dump 时先 clone 一份 ModelRequest（含完整 transcript），
        // 调用结束后落盘。未启用时 zero-cost。
        let dump_request = model_io_dump.as_ref().map(|_| req.clone());

        let stream_tool_call_offset = tool_call_dispatch_offset;
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
                ModelStreamEvent::ToolCallDelta(delta) => EventPayload::ToolCallDelta {
                    index: stream_tool_call_offset + delta.index,
                    id: delta.id,
                    name: delta.name,
                    arguments_delta: delta.arguments_delta,
                },
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
                kind: "main".to_string(),
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
                last_finish = finish;
                info!(
                    duration_ms = call_duration_ms,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    text_len = text.len(),
                    "model done"
                );
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
                total_cache_read_tokens += usage.cache_read_tokens;
                total_cache_creation_tokens += usage.cache_creation_tokens;

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

                transcript.push_assistant_with_reasoning(text.clone(), reasoning, Vec::new());
                let mut all_attachments = output_attachments;
                all_attachments.extend(attachments);
                set_pending_inputs_accepting(pending_inputs_accepting.as_ref(), false);
                if drain_pending_inputs(
                    pending_inputs.as_ref(),
                    consumed_pending_inputs.as_ref(),
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
                break Ok(AssistantOutput {
                    text,
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
                    "model requested tool calls"
                );
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

                // inherit 模式（架构 §4.4.11.3）需要"父当前 transcript 副本"。
                // 在 push 触发 turn 之前抓快照——子看到的形态截止上一 turn 结束，不含
                // 触发它的 assistant tool_call（避免子 transcript 出现无对应 ToolResult 的 self-reference）。
                // 同 ToolStep 内的 parallel Task 共享同一份 Arc，看到一致形态。
                let parent_transcript_snapshot = if calls
                    .iter()
                    .any(|c| c.name == crate::tools::task::TASK_TOOL_NAME)
                {
                    Some(Arc::new(transcript.entries.clone()))
                } else {
                    None
                };

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
                    force_automode,
                    hooks: hooks.clone(),
                    session_id_for_hooks: session_id.clone(),
                    data_dir_for_artifacts: data_dir.clone(),
                    permission_store: hitl.permission_store().cloned(),
                    edits_worktree: edits_worktree.clone(),
                    subagent_ctx: subagent_ctx.clone(),
                    parent_transcript_snapshot,
                    model_io_dump: model_io_dump.clone(),
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
                drain_pending_inputs(
                    pending_inputs.as_ref(),
                    consumed_pending_inputs.as_ref(),
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
    match &result {
        Ok(_) => {
            run_span.record("hebbian.run.outcome", attr::run_outcome::DONE);
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
            emit(EventPayload::RunCancelled);
        }
        Err(ModelError::Suspended) => {
            // 挂起态：本 task 退出，但 Run 仍 Active。不发 RunFinished / RunCancelled——
            // RunSuspended 已在 break 前 emit；resume_run 时由 Harness 复活同一个 Run。
            run_span.record("hebbian.run.outcome", "suspended");
        }
        Err(e) => {
            run_span.record("hebbian.run.outcome", attr::run_outcome::FAILED);
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
    use model_gateway::types::{
        ModelRequest, ModelResponse, ModelStreamEvent, TranscriptEntry, Usage,
    };
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
                run_mode: Arc::new(std::sync::Mutex::new(
                    crate::run_mode::RunMode::AskBeforeEdits,
                )),
                model_id: None,
                judge_client: None,
                force_automode: false,
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,

                subagent_ctx: None,
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
                run_mode: Arc::new(std::sync::Mutex::new(
                    crate::run_mode::RunMode::AskBeforeEdits,
                )),
                model_id: None,
                judge_client: None,
                force_automode: false,
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,

                subagent_ctx: None,
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
                run_mode: Arc::new(std::sync::Mutex::new(
                    crate::run_mode::RunMode::AskBeforeEdits,
                )),
                model_id: None,
                judge_client: None,
                force_automode: false,
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: None,
                system_rules: None,

                subagent_ctx: None,
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
                run_mode: Arc::new(std::sync::Mutex::new(
                    crate::run_mode::RunMode::AskBeforeEdits,
                )),
                model_id: None,
                judge_client: None,
                force_automode: false,
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
                edits_worktree: None,
                max_tool_iterations: Some(2),
                system_rules: None,

                subagent_ctx: None,
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
}
