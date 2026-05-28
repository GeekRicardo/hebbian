use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

use observability::attr;
use protocol::{AgentRef, ErrorReport, Event, EventPayload, RunId, StopReason};
use tracing::{debug, field::Empty, info, Instrument};

use crate::{
    context::{
        compaction::{compact_structural, needs_compaction},
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
    types::{AssistantOutput, ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use protocol::ResumeCause;

/// Stop hook 在一个 Run 内最多注入多少次 reminder（架构 §4.8.3）。超过即放弃注入正常出 turn。
/// 防 cargo check 永远修不好把 loop 跑爆。
const MAX_STOP_INJECTIONS: u32 = 3;

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
    /// 运行模式（架构 §4.4.3）。默认 `AskBeforeEdits`。
    pub run_mode: crate::run_mode::RunMode,
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
            let compaction_result = compact_structural(
                transcript.system.as_deref(),
                transcript.entries.clone(),
                compaction_policy,
            );
            let before_tokens = compaction_result.before_tokens;
            let after_tokens = compaction_result.after_tokens;
            compaction_span.record(attr::COMPACTION_BEFORE_TOKENS, before_tokens);
            compaction_span.record(attr::COMPACTION_AFTER_TOKENS, after_tokens);
            info!(before_tokens, after_tokens, "context compacted");
            transcript.entries = compaction_result.entries;
            drop(_enter);
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
        if run_mode == crate::run_mode::RunMode::PlanMode {
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
        let response_result = if used_stream_path {
            client
                .stream(
                    req,
                    cancel.clone(),
                    &move |stream_event: ModelStreamEvent| {
                        let payload = match stream_event {
                            ModelStreamEvent::TextDelta { text } => {
                                EventPayload::TextDelta { text }
                            }
                            ModelStreamEvent::ReasoningDelta { text } => {
                                EventPayload::Reasoning { text }
                            }
                            ModelStreamEvent::ToolCallDelta(delta) => EventPayload::ToolCallDelta {
                                index: stream_tool_call_offset + delta.index,
                                id: delta.id,
                                name: delta.name,
                                arguments_delta: delta.arguments_delta,
                            },
                        };
                        on_event_for_stream(state_for_stream.event(payload));
                    },
                )
                .instrument(model_span.clone())
                .await
        } else {
            client
                .complete(req, cancel.clone())
                .instrument(model_span.clone())
                .await
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
            } => {
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
                    run_mode,
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
                                run_mode: format!("{:?}", run_mode),
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
                        text: "第一段".to_string(),
                        reasoning: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
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
                        text: "引导后的回答".to_string(),
                        reasoning: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
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
                        text: "已按引导继续".to_string(),
                        reasoning: String::new(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
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
                run_mode: crate::run_mode::RunMode::AskBeforeEdits,
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
                run_mode: crate::run_mode::RunMode::AskBeforeEdits,
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
                run_mode: crate::run_mode::RunMode::AskBeforeEdits,
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
                run_mode: crate::run_mode::RunMode::AskBeforeEdits,
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
