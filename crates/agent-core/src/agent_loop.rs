use std::sync::Arc;
use std::time::Instant;

use futures_util::future::join_all;
use protocol::{
    AgentRef, ApprovalDecision, ErrorReport, Event, EventPayload, PermissionKind, RiskLevel, RunId,
    StopReason,
};
use tracing::{debug, info, warn};

use crate::{
    context::{
        compaction::{compact_structural, needs_compaction},
        transcript::Transcript,
    },
    definition::CompactionPolicy,
    hooks::{HookManager, HookPoint},
    run_state::RunState,
    tools::{
        hosted_tool_definitions,
        permissions::{PermissionDecision, PermissionGate},
        registry::ToolRegistry,
    },
};
use model_gateway::{
    client::ModelClient,
    types::{
        AssistantOutput, ModelError, ModelRequest, ModelResponse, ModelStreamEvent, ToolResult,
    },
};
use platform::{runtime as cancellation, CancelFlag};

const MAX_TOOL_ITERATIONS: u32 = 10;
const MAX_TOOL_RESULT_INLINE: usize = 6_000;

/// 运行 agent loop 的入参集合
pub struct LoopParams<'a> {
    pub client: &'a dyn ModelClient,
    pub registry: Arc<ToolRegistry>,
    pub gate: Arc<PermissionGate>,
    pub hooks: Arc<HookManager>,
    pub transcript: &'a mut Transcript,
    pub enabled_tools: &'a [String],
    pub compaction_policy: &'a CompactionPolicy,
    pub stream: bool,
    pub cancel: CancelFlag,
    pub state: Arc<RunState>,
    pub agent: AgentRef,
    pub parent: Option<RunId>,
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

#[tracing::instrument(
    level = "info",
    skip_all,
    fields(run_id = %params.state.run_id, agent = %params.agent)
)]
pub async fn run_loop(
    params: LoopParams<'_>,
    on_event: EventSink,
) -> Result<AssistantOutput, ModelError> {
    let LoopParams {
        client,
        registry,
        gate,
        hooks,
        transcript,
        enabled_tools,
        compaction_policy,
        stream,
        cancel,
        state,
        agent,
        parent,
    } = params;

    let emit = |payload: EventPayload| on_event(state.event(payload));

    info!("run started");

    emit(EventPayload::RunStarted {
        agent: agent.clone(),
        parent,
    });
    hooks.trigger(&HookPoint::BeforeRun).await;

    let run_start = Instant::now();
    let mut iteration: u32 = 0;
    let mut tool_call_dispatch_offset = 0usize;
    let mut output_attachments = Vec::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    let result: Result<AssistantOutput, ModelError> = loop {
        if cancellation::is_cancelled(&cancel) {
            debug!("run cancelled");
            gate.cancel_all_pending();
            break Err(ModelError::Cancelled);
        }

        if needs_compaction(
            transcript.system.as_deref(),
            &transcript.entries,
            compaction_policy,
        ) {
            let compaction_result = compact_structural(
                transcript.system.as_deref(),
                transcript.entries.clone(),
                compaction_policy,
            );
            info!(
                before_tokens = compaction_result.before_tokens,
                after_tokens = compaction_result.after_tokens,
                "context compacted"
            );
            transcript.entries = compaction_result.entries;
            emit(EventPayload::ContextCompacted {
                before_tokens: compaction_result.before_tokens,
                after_tokens: compaction_result.after_tokens,
            });
            hooks.trigger(&HookPoint::OnContextCompaction).await;
        }

        let turn_index = state.next_turn();
        let turn_id = protocol::TurnId::new();
        emit(EventPayload::TurnStarted {
            turn_id: turn_id.clone(),
            turn: turn_index,
        });
        hooks
            .trigger(&HookPoint::BeforeTurn { turn: turn_index })
            .await;

        let tool_defs = if enabled_tools.is_empty() {
            Vec::new()
        } else {
            let mut defs = registry.definitions(enabled_tools);
            defs.extend(hosted_tool_definitions(enabled_tools));
            defs
        };

        let req = ModelRequest {
            model: String::new(),
            system: transcript.system.clone(),
            entries: transcript.entries.clone(),
            tools: tool_defs,
            max_tokens: 8192,
        };

        debug!(iteration, "calling model");
        hooks
            .trigger(&HookPoint::BeforeModelCall { turn: turn_index })
            .await;
        let call_start = Instant::now();

        let stream_tool_call_offset = tool_call_dispatch_offset;
        let on_event_for_stream = on_event.clone();
        let state_for_stream = state.clone();
        let response = if stream && (enabled_tools.is_empty() || client.supports_streaming_tools())
        {
            client
                .stream(req, cancel.clone(), &move |stream_event: ModelStreamEvent| {
                    let payload = match stream_event {
                        ModelStreamEvent::TextDelta { text } => EventPayload::TextDelta { text },
                        ModelStreamEvent::ToolCallDelta(delta) => EventPayload::ToolCallDelta {
                            index: stream_tool_call_offset + delta.index,
                            id: delta.id,
                            name: delta.name,
                            arguments_delta: delta.arguments_delta,
                        },
                    };
                    on_event_for_stream(state_for_stream.event(payload));
                })
                .await?
        } else {
            client.complete(req, cancel.clone()).await?
        };

        let call_duration_ms = call_start.elapsed().as_millis() as u64;
        hooks
            .trigger(&HookPoint::AfterModelCall { turn: turn_index })
            .await;

        match response {
            ModelResponse::Done {
                text,
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

                emit(EventPayload::TextDone {
                    full_text: text.clone(),
                });
                emit(EventPayload::TurnFinished {
                    turn_id,
                    turn: turn_index,
                    stop_reason: StopReason::EndTurn,
                });
                hooks
                    .trigger(&HookPoint::AfterTurn { turn: turn_index })
                    .await;

                transcript.push_assistant(text.clone(), Vec::new());
                let mut all_attachments = output_attachments;
                all_attachments.extend(attachments);
                break Ok(AssistantOutput {
                    text,
                    attachments: all_attachments,
                });
            }
            ModelResponse::ToolCalls {
                text,
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

                if !text.is_empty() {
                    emit(EventPayload::TextDelta { text: text.clone() });
                }
                output_attachments.extend(attachments);
                transcript.push_assistant(text, calls.clone());

                if iteration >= MAX_TOOL_ITERATIONS {
                    let msg = format!("已达到最大工具调用轮数 {MAX_TOOL_ITERATIONS}");
                    warn!(max_iterations = MAX_TOOL_ITERATIONS, "max iterations");
                    emit(EventPayload::TurnFinished {
                        turn_id: turn_id.clone(),
                        turn: turn_index,
                        stop_reason: StopReason::MaxIterations,
                    });
                    hooks
                        .trigger(&HookPoint::AfterTurn { turn: turn_index })
                        .await;
                    break Err(ModelError::Other(msg));
                }
                iteration += 1;

                // —— 派发工具：每个 call 一个 future，futures 并发执行 ——
                let mut tool_tasks = Vec::new();
                for (call_index, call) in calls.iter().enumerate() {
                    let dispatch_index = tool_call_dispatch_offset + call_index;
                    if cancellation::is_cancelled(&cancel) {
                        gate.cancel_all_pending();
                        break;
                    }

                    let decision = gate.check(&call.name, &call.input);

                    if let PermissionDecision::NeedsApproval { request_id, .. } = &decision {
                        emit(EventPayload::PermissionRequested {
                            request_id: request_id.clone(),
                            kind: PermissionKind::ToolCall {
                                tool_name: call.name.clone(),
                                input: call.input.clone(),
                            },
                            summary: format!("工具 {} 请求执行", call.name),
                            risk: RiskLevel::Medium,
                        });
                    }

                    let call = call.clone();
                    let tool = registry.find(&call.name);
                    let on_event_local = on_event.clone();
                    let state_local = state.clone();
                    let cancel_local = cancel.clone();

                    tool_tasks.push(async move {
                        let approved: Result<(), String> = match decision {
                            PermissionDecision::Approved => Ok(()),
                            PermissionDecision::Denied { reason } => Err(reason),
                            PermissionDecision::NeedsApproval { request_id, waiter } => {
                                match waiter.await {
                                    Ok(decision) => {
                                        on_event_local(state_local.event(
                                            EventPayload::PermissionResolved {
                                                request_id,
                                                decision: decision.clone(),
                                            },
                                        ));
                                        match decision {
                                            ApprovalDecision::AllowOnce
                                            | ApprovalDecision::AllowAndRemember { .. } => Ok(()),
                                            ApprovalDecision::Deny => Err("用户拒绝".into()),
                                            ApprovalDecision::DenyWithFeedback { feedback } => {
                                                Err(feedback)
                                            }
                                        }
                                    }
                                    Err(_) => Err("审批通道已关闭".into()),
                                }
                            }
                        };

                        if cancellation::is_cancelled(&cancel_local) {
                            return Err::<(usize, ToolResult), ModelError>(ModelError::Cancelled);
                        }

                        if let Err(reason) = approved {
                            warn!(tool = %call.name, %reason, "tool denied");
                            let denied_content = format!("工具调用被拒绝: {reason}");
                            on_event_local(state_local.event(EventPayload::ToolCallStarted {
                                index: dispatch_index,
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                input: call.input.clone(),
                            }));
                            on_event_local(state_local.event(EventPayload::ToolCallFinished {
                                index: dispatch_index,
                                call_id: call.id.clone(),
                                result: denied_content.clone(),
                                duration_ms: 0,
                                truncated: false,
                            }));
                            return Ok((
                                call_index,
                                ToolResult {
                                    call_id: call.id.clone(),
                                    name: call.name.clone(),
                                    content: denied_content,
                                },
                            ));
                        }

                        on_event_local(state_local.event(EventPayload::ToolCallStarted {
                            index: dispatch_index,
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            input: call.input.clone(),
                        }));

                        let tool_start = Instant::now();
                        let output = match tool {
                            Some(tool) => {
                                tool.execute(call.input.clone()).await.unwrap_or_else(|e| {
                                    warn!(tool = %call.name, error = %e, "tool exec error");
                                    format!("工具执行错误: {e}")
                                })
                            }
                            None => {
                                warn!(tool = %call.name, "tool not in registry");
                                format!("未找到工具: {}", call.name)
                            }
                        };
                        let duration_ms = tool_start.elapsed().as_millis() as u64;

                        let truncated = output.len() > MAX_TOOL_RESULT_INLINE;
                        let content = if truncated {
                            format!("{}…[已截断]", &output[..MAX_TOOL_RESULT_INLINE])
                        } else {
                            output
                        };

                        on_event_local(state_local.event(EventPayload::ToolCallFinished {
                            index: dispatch_index,
                            call_id: call.id.clone(),
                            result: content.clone(),
                            duration_ms,
                            truncated,
                        }));

                        Ok::<(usize, ToolResult), ModelError>((
                            call_index,
                            ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content,
                            },
                        ))
                    });
                }

                let mut results: Vec<(usize, ToolResult)> = Vec::new();
                for outcome in join_all(tool_tasks).await {
                    match outcome {
                        Ok(pair) => results.push(pair),
                        Err(e) => {
                            emit(EventPayload::TurnFinished {
                                turn_id: turn_id.clone(),
                                turn: turn_index,
                                stop_reason: StopReason::Cancelled,
                            });
                            return Err(e);
                        }
                    }
                }

                results.sort_by_key(|(index, _)| *index);
                transcript.push_tool_results(
                    results
                        .into_iter()
                        .map(|(_, result)| result)
                        .collect::<Vec<_>>(),
                );
                tool_call_dispatch_offset += calls.len();

                emit(EventPayload::TurnFinished {
                    turn_id,
                    turn: turn_index,
                    stop_reason: StopReason::EndTurn,
                });
                hooks
                    .trigger(&HookPoint::AfterTurn { turn: turn_index })
                    .await;
            }
        }
    };

    let duration_ms = run_start.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => {
            emit(EventPayload::RunFinished {
                total_input_tokens,
                total_output_tokens,
                duration_ms,
            });
        }
        Err(ModelError::Cancelled) => {
            emit(EventPayload::RunCancelled);
        }
        Err(e) => {
            emit(EventPayload::RunFailed {
                error: ErrorReport::other(e.to_string()),
            });
        }
    }
    hooks.trigger(&HookPoint::AfterRun).await;

    result
}
