use std::sync::Arc;
use std::time::Instant;

use observability::{attr, metrics};
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
    hooks::{HookManager, HookPoint},
    model_io_dump::{self, DumpEntry, ModelIoDump},
    run_state::RunState,
    system_prompt::compose_system_prompt,
    tools::{
        ask_only_definitions, hitl::HitlGate, hosted_tool_definitions, registry::ToolRegistry,
        BUILTIN_TOOL_NAMES,
    },
    workspace::Workspace,
};
use model_gateway::{
    client::ModelClient,
    types::{AssistantOutput, ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use common::{
    runtime::{self as cancellation, PendingInputs},
    CancelFlag,
};
use protocol::ResumeCause;

const MAX_TOOL_ITERATIONS: u32 = 100;

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
    /// 运行模式（架构 §4.4.3）。默认 `AskBeforeEdits`。
    pub run_mode: crate::run_mode::RunMode,
    /// 当前模型 id（AutoMode judge 用作模型限定）。
    pub model_id: Option<String>,
    /// AutoMode judge 用的 client。通常 = 主 client，便于复用 OAuth/重试链。
    /// `None` 时 AutoMode 直接降级为 Ask。
    pub judge_client: Option<Arc<dyn ModelClient>>,
    /// 数据目录路径，用于把 microcompact 压缩的原文落 txt（架构 §4.7 / Step 9）。
    pub data_dir: Option<std::path::PathBuf>,
    /// 会话 id（格式 `{yyyymmddHHmm}-{shortUuid}`）。与 `data_dir` 拼成
    /// `<data_dir>/sessions/<sid>/tool_results/<call_id>.txt`。
    pub session_id: Option<String>,
    /// 工具与 agent_loop 之间共享的"挂起请求"槽（架构 §4.12.4）。WaitForTask /
    /// ScheduleWakeup 调用时把 `RunPhase` 写进来；agent_loop 在每次 ToolStep 完成后
    /// 取出处理：emit RunSuspended → 落 RunCheckpoint → 注册到 WakeupScheduler → return。
    pub phase: Option<crate::wakeup::PhaseChannel>,
    /// 从挂起态恢复时由 Harness 注入：agent_loop 据此恢复计数器并 emit
    /// `RunResumed { cause }` 而不是 `RunStarted`（架构 §4.12.6）。`None` 表示
    /// 普通新起 Run。
    pub resume_from: Option<RunResumeState>,
}

/// 把 [`compose_system_prompt`] 重新导出为旧名字，方便其它 crate 沿用。
/// 内部已经不再混入 workspace XML——环境信息走第一条 user message 的 `<environment>` 块。
pub use crate::system_prompt::compose_system_prompt as build_system_prompt;

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

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
        run_mode,
        model_id,
        judge_client,
        data_dir,
        session_id,
        phase,
        resume_from,
    } = params;

    let emit = |payload: EventPayload| on_event(state.event(payload));

    // 入口：resume_from 给定时 emit `RunResumed`（架构 §4.12.6），否则 `RunStarted`。
    // 计数器从 checkpoint 起步，保证 MAX_TOOL_ITERATIONS 累积、Step index 单调。
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

    let result: Result<AssistantOutput, ModelError> = loop {
        if cancellation::is_cancelled(&cancel) {
            debug!("run cancelled");
            hitl.cancel_all_pending();
            // Stop hook（架构 §4.8.1）：fire-and-forget，把用户取消事件转给外部 hook。
            if !hooks.is_empty() {
                let sid = session_id.clone().unwrap_or_default();
                let hooks_for_stop = hooks.clone();
                tokio::spawn(async move {
                    let _ = hooks_for_stop
                        .trigger(&HookPoint::Stop {
                            session_id: sid,
                            reason: "user_cancelled".to_string(),
                        })
                        .await;
                });
            }
            break Err(ModelError::Cancelled);
        }

        // 「立即发送」语义：surface 在 streaming 中往 pending_inputs 推过的 user message，
        // 在下一次 model.request 之前 drain 出来加入 transcript——让模型在当前 agent loop
        // 的下一个 iteration 立刻看到这些消息，而不是等整个 turn 跑完再开新 turn。
        if let Some(slot) = pending_inputs.as_ref() {
            let drained: Vec<_> = std::mem::take(&mut *slot.lock().unwrap());
            for input in drained {
                transcript.push_user(input.content, input.attachments);
            }
        }

        // Microcompact：每轮模型请求前先把超阈值的老 tool_result 压缩为占位符。
        // 不消耗模型调用，只改 transcript entries，幂等。
        let mc_report = microcompact(&mut transcript.entries, &MicrocompactPolicy::default());
        // 把被压缩的原文落 txt（架构 §4.7 / Step 9）：data_dir + session_id 都给定时
        // 才落，否则只是 in-memory 占位符。占位符里写了 call_id，LLM 可用 Read
        // `<data_dir>/sessions/<sid>/tool_results/<call_id>.txt` 按需检索原始内容。
        if !mc_report.shadowed_artifacts.is_empty() {
            if let (Some(dd), Some(sid)) = (data_dir.as_ref(), session_id.as_deref()) {
                for (call_id, content) in &mc_report.shadowed_artifacts {
                    if let Err(e) = crate::storage::tool_results::save_tool_result(
                        dd, sid, call_id, content,
                    ) {
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
            info!(
                before_tokens,
                after_tokens,
                "context compacted"
            );
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
            "turn",
            hebbian.turn.index = turn_index,
            hebbian.turn.id = %turn_id,
            hebbian.turn.stop_reason = Empty,
            hebbian.turn.tool_calls = Empty,
        );
        let turn_started = Instant::now();
        emit(EventPayload::TurnStarted {
            turn_id: turn_id.clone(),
            turn: turn_index,
        });

        // 内置工具每轮都自动注入：ask + Bash/Read/Write/Grep/Skill。
        // 用户可选工具按 enabled_tools 过滤。
        let mut tool_defs = ask_only_definitions();
        let mut all_filter: Vec<String> =
            BUILTIN_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
        all_filter.extend(enabled_tools.iter().cloned());
        tool_defs.extend(registry.definitions(&all_filter));
        if !enabled_tools.is_empty() {
            tool_defs.extend(hosted_tool_definitions(enabled_tools));
        }
        // PlanMode 工具过滤（架构 §4.4.3 / §4.4.5）：删除会改外界的工具，强制 agent 走
        // 只读探索路径；同时注入 ExitPlanMode 工具让 agent 主动结束规划。
        if run_mode == crate::run_mode::RunMode::PlanMode {
            let mutating = ["Bash", "PowerShell", "Edit", "Write"];
            tool_defs.retain(|t| !mutating.contains(&t.name.as_str()));
            let extra = registry.definitions(&["ExitPlanMode".to_string()]);
            tool_defs.extend(extra);
        } else {
            // 其他模式不暴露 ExitPlanMode，避免误调用
            tool_defs.retain(|t| t.name != "ExitPlanMode");
        }
        let has_tools = !tool_defs.is_empty();

        // system prompt = BASE 常量 + 用户 persona。环境信息（cwd / allowed_dirs / runtime
        // 追加）走 user message 的 `<environment>` / `<workspace-update>` 块——保 prompt cache。
        let combined_system = compose_system_prompt(transcript.system.as_deref());

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
                .instrument(turn_span.clone())
                .await
        } else {
            client
                .complete(req, cancel.clone())
                .instrument(turn_span.clone())
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
            Err(e) => break Err(e),
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
                metrics::record_turn_duration(
                    turn_index,
                    "end_turn",
                    turn_started.elapsed().as_millis() as f64,
                );
                emit(EventPayload::TurnFinished {
                    turn_id,
                    turn: turn_index,
                    stop_reason: StopReason::EndTurn,
                });

                transcript.push_assistant_with_reasoning(text.clone(), reasoning, Vec::new());
                let mut all_attachments = output_attachments;
                all_attachments.extend(attachments);
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
                transcript.push_assistant_with_reasoning(text, reasoning, calls.clone());

                if iteration >= MAX_TOOL_ITERATIONS {
                    let msg = format!("已达到最大工具调用轮数 {MAX_TOOL_ITERATIONS}");
                    tracing::warn!(max_iterations = MAX_TOOL_ITERATIONS, "max iterations");
                    turn_span.record(attr::STOP_REASON, "max_iterations");
                    metrics::record_turn_duration(
                        turn_index,
                        "max_iterations",
                        turn_started.elapsed().as_millis() as f64,
                    );
                    emit(EventPayload::TurnFinished {
                        turn_id: turn_id.clone(),
                        turn: turn_index,
                        stop_reason: StopReason::MaxIterations,
                    });
                    break Err(ModelError::Other(msg));
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
                    hooks: hooks.clone(),
                    session_id_for_hooks: session_id.clone(),
                    data_dir_for_artifacts: data_dir.clone(),
                };

                tool_step_index += 1;
                emit(EventPayload::StepStarted {
                    step_kind: protocol::StepKind::Tool,
                    step_index: tool_step_index,
                });
                let results = match dispatcher
                    .run_calls(&calls, tool_call_dispatch_offset)
                    .instrument(turn_span.clone())
                    .await
                {
                    Ok(results) => results,
                    Err(e) => {
                        turn_span.record(attr::STOP_REASON, "cancelled");
                        metrics::record_turn_duration(
                            turn_index,
                            "cancelled",
                            turn_started.elapsed().as_millis() as f64,
                        );
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

                // 架构 §4.12.5：ToolStep 跑完后检查 phase channel。模型本 ToolStep
                // 调过 WaitForTask / ScheduleWakeup 时，phase 已被工具写入；这里：
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
                            RunPhase::AwaitingCron { fire_at_ms, .. } => (
                                protocol::SuspendReason::Cron,
                                Some(*fire_at_ms),
                                Vec::new(),
                            ),
                        };
                        emit(EventPayload::RunSuspended {
                            reason: reason_evt,
                            resumes_at_ms,
                            waiting_for_task_ids: waiting_for_task_ids.clone(),
                        });
                        if let (Some(dd), Some(sid)) =
                            (data_dir.as_ref(), session_id.as_deref())
                        {
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
                                scheduler.arm_bg_task(sid_for_arm, run_id_for_arm, task_id);
                            }
                            RunPhase::AwaitingCron {
                                fire_at_ms, reason, ..
                            } => {
                                scheduler.arm_cron(
                                    sid_for_arm,
                                    run_id_for_arm,
                                    fire_at_ms,
                                    reason,
                                );
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
                metrics::record_turn_duration(
                    turn_index,
                    "end_turn",
                    turn_started.elapsed().as_millis() as f64,
                );
                emit(EventPayload::TurnFinished {
                    turn_id,
                    turn: turn_index,
                    stop_reason: StopReason::EndTurn,
                });
            }
        }
    };

    let duration_ms = run_start.elapsed().as_millis() as u64;
    let run_span = tracing::Span::current();
    run_span.record("hebbian.run.iterations", iteration);
    let agent_id = agent.0.as_str();
    match &result {
        Ok(_) => {
            run_span.record("hebbian.run.outcome", attr::run_outcome::DONE);
            metrics::record_run_outcome(attr::run_outcome::DONE, agent_id, duration_ms as f64);
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
            metrics::record_run_outcome(attr::run_outcome::CANCELLED, agent_id, duration_ms as f64);
            emit(EventPayload::RunCancelled);
        }
        Err(ModelError::Suspended) => {
            // 挂起态：本 task 退出，但 Run 仍 Active。不发 RunFinished / RunCancelled——
            // RunSuspended 已在 break 前 emit；resume_run 时由 Harness 复活同一个 Run。
            run_span.record("hebbian.run.outcome", "suspended");
        }
        Err(e) => {
            run_span.record("hebbian.run.outcome", attr::run_outcome::FAILED);
            metrics::record_run_outcome(attr::run_outcome::FAILED, agent_id, duration_ms as f64);
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
    use model_gateway::types::{ModelRequest, ModelResponse, ModelStreamEvent};
    use std::sync::{atomic::AtomicBool, Mutex};

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
                run_mode: crate::run_mode::RunMode::AskBeforeEdits,
                model_id: None,
                judge_client: None,
                data_dir: None,
                session_id: None,
                phase: None,
                resume_from: None,
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
}
