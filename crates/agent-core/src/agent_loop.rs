use std::sync::Arc;
use std::time::Instant;

use protocol::{AgentRef, ErrorReport, Event, EventPayload, RunId, StopReason};
use tracing::{debug, info};

use crate::{
    context::{
        compaction::{compact_structural, needs_compaction},
        microcompact::{microcompact, MicrocompactPolicy},
        transcript::Transcript,
    },
    definition::CompactionPolicy,
    dispatch::ToolDispatcher,
    hooks::{HookManager, HookPoint},
    run_state::RunState,
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
use platform::{runtime as cancellation, CancelFlag};

const MAX_TOOL_ITERATIONS: u32 = 100;

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
}

/// 把 user system prompt + workspace XML 拼成最终 system 字段。
/// 每轮重拼，所以运行时 `add_allowed_dir` 后下一轮立刻反映。
fn build_system_prompt(user_system: Option<&str>, workspace: &Workspace) -> String {
    let mut s = String::new();
    if let Some(u) = user_system {
        if !u.is_empty() {
            s.push_str(u);
            s.push_str("\n\n");
        }
    }
    s.push_str(&workspace.to_system_xml());
    s
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
    } = params;

    let emit = |payload: EventPayload| on_event(state.event(payload));

    info!("run started");

    emit(EventPayload::RunStarted {
        agent: agent.clone(),
        parent,
    });

    let run_start = Instant::now();
    let mut iteration: u32 = 0;
    let mut tool_call_dispatch_offset = 0usize;
    let mut output_attachments = Vec::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_cache_read_tokens: u64 = 0;
    let mut total_cache_creation_tokens: u64 = 0;

    let result: Result<AssistantOutput, ModelError> = loop {
        if cancellation::is_cancelled(&cancel) {
            debug!("run cancelled");
            hitl.cancel_all_pending();
            break Err(ModelError::Cancelled);
        }

        // Microcompact：每轮模型请求前先把超阈值的老 tool_result 影子化为占位符。
        // 不消耗模型调用，只改 transcript entries，幂等。
        let mc_report = microcompact(&mut transcript.entries, &MicrocompactPolicy::default());
        if mc_report.shadowed_count > 0 {
            tracing::info!(
                shadowed = mc_report.shadowed_count,
                kept = mc_report.kept_count,
                total = mc_report.total_compactable,
                "microcompact shadowed old tool results"
            );
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
            let before_tokens = compaction_result.before_tokens;
            let after_tokens = compaction_result.after_tokens;
            transcript.entries = compaction_result.entries;
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
        let has_tools = !tool_defs.is_empty();

        // system prompt：用户提供的 + workspace XML，每轮重拼以反映运行时新增的 allowed_dirs
        let combined_system = build_system_prompt(transcript.system.as_deref(), &workspace);

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

        let stream_tool_call_offset = tool_call_dispatch_offset;
        let on_event_for_stream = on_event.clone();
        let state_for_stream = state.clone();
        // 走 stream 的条件：调用方要求流式 + (本轮无工具 || provider 支持流式工具调用)。
        // anthropic / gemini 默认不支持流式工具调用，含工具时只能用 complete 路径。
        let used_stream_path = stream && (!has_tools || client.supports_streaming_tools());
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
                .await
        } else {
            client.complete(req, cancel.clone()).await
        };

        let call_duration_ms = call_start.elapsed().as_millis() as u64;

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

                emit(EventPayload::TextDone {
                    full_text: text.clone(),
                });
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
                if !used_stream_path && !text.is_empty() {
                    emit(EventPayload::TextDelta { text: text.clone() });
                }
                output_attachments.extend(attachments);
                transcript.push_assistant_with_reasoning(text, reasoning, calls.clone());

                if iteration >= MAX_TOOL_ITERATIONS {
                    let msg = format!("已达到最大工具调用轮数 {MAX_TOOL_ITERATIONS}");
                    tracing::warn!(max_iterations = MAX_TOOL_ITERATIONS, "max iterations");
                    emit(EventPayload::TurnFinished {
                        turn_id: turn_id.clone(),
                        turn: turn_index,
                        stop_reason: StopReason::MaxIterations,
                    });
                    break Err(ModelError::Other(msg));
                }
                iteration += 1;

                let dispatcher = ToolDispatcher {
                    registry: registry.clone(),
                    hitl: hitl.clone(),
                    workspace: workspace.clone(),
                    state: state.clone(),
                    sink: on_event.clone(),
                    cancel: cancel.clone(),
                };

                let results = match dispatcher.run_calls(&calls, tool_call_dispatch_offset).await {
                    Ok(results) => results,
                    Err(e) => {
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

                emit(EventPayload::TurnFinished {
                    turn_id,
                    turn: turn_index,
                    stop_reason: StopReason::EndTurn,
                });
            }
        }
    };

    let duration_ms = run_start.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => {
            emit(EventPayload::RunFinished {
                total_input_tokens,
                total_output_tokens,
                total_cache_read_tokens,
                total_cache_creation_tokens,
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
