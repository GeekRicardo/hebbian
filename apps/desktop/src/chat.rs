use crate::engine::EngineEvent;
use crate::error::{AppError, AppResult};
use crate::hitl::HitlState;
use agent_core::{
    context::transcript::Transcript,
    definition::AgentDefinition,
    harness::RunParams,
    hooks::HookManager,
    tools::{permissions::PermissionGate, question::QuestionGate},
    types::{AgentEvent, AgentEventPayload},
    Harness,
};
use model_gateway::{self, config::Provider};
use platform::{
    attachments::MessageAttachment,
    storage::sessions::{self, Message, MessageMeta, MessagePart, MessageToolCall, Role, Session},
    CancelFlag,
};
use protocol::{AgentRef, EventPayload};
use std::{collections::HashMap, path::Path, sync::Arc};
use tauri::{ipc::Channel, AppHandle, Manager};

enum RunOutcome {
    Done,
    Cancelled,
    Failed(String),
}

pub struct SendArgs {
    pub session_id: String,
    pub user_content: String,
    pub attachments: Vec<MessageAttachment>,
    pub stream: bool,
    pub enabled_tools: Vec<String>,
    pub cancel_flag: CancelFlag,
    /// app 级 HITL 桥接。用 Tauri 的 `app.state::<Arc<HitlState>>()`
    /// 取出来塞进来。测试场景传 `None`。
    pub hitl: Option<Arc<HitlState>>,
}

fn data_dir(app: &AppHandle) -> AppResult<std::path::PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| AppError::msg(e.to_string()))
}

pub async fn send_and_save(
    app: &AppHandle,
    args: SendArgs,
    on_event: Channel<EngineEvent>,
) -> AppResult<Message> {
    let dd = data_dir(app)?;
    send_and_save_in_data_dir(&dd, args, move |event| {
        let _ = on_event.send(event);
    })
    .await
}

pub async fn send_and_save_in_data_dir(
    data_dir: &Path,
    args: SendArgs,
    emit_event: impl Fn(EngineEvent) + Send + Sync + 'static,
) -> AppResult<Message> {
    send_and_save_in_data_dir_with_client_factory(data_dir, args, emit_event, |provider, model| {
        let client = model_gateway::build_client(provider)
            .map_err(|e| AppError::msg(format!("无法创建 ModelClient: {e}")))?;
        Ok(Arc::new(ModelWithName::new(client, model)) as Arc<dyn ModelClient>)
    })
    .await
}

pub async fn send_and_save_in_data_dir_with_client_factory(
    data_dir: &Path,
    args: SendArgs,
    emit_event: impl Fn(EngineEvent) + Send + Sync + 'static,
    build_client: impl Fn(Provider, String) -> AppResult<Arc<dyn ModelClient>> + Send + Sync,
) -> AppResult<Message> {
    let prior_session = sessions::load(data_dir, &args.session_id)?;
    let user_msg = Message {
        id: sessions::new_id(),
        role: Role::User,
        content: args.user_content.clone(),
        attachments: args.attachments.clone(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
    };
    let session = sessions::append_message(data_dir, &args.session_id, user_msg)?;

    let provider = model_gateway::config::get(data_dir, &session.provider_id)?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| AppError::msg(format!("OAuth token 刷新失败: {e}")))?;
    let model = session.model.clone();

    let client = build_client(provider, model)?;

    let harness = Harness::new(agent_core::tools::default_tools(), HookManager::empty());
    let definition = AgentDefinition::default();
    let gate = Arc::new(PermissionGate::new(definition.permission_policy.clone()));
    let question_gate = Arc::new(QuestionGate::new());

    // 必须在 spawn_run 之前 subscribe，否则错过 RunStarted 等早期事件
    let mut events_rx = harness.subscribe();

    // 组装 transcript：历史 + 当前 user message
    let mut transcript =
        Transcript::from_session(session.system_prompt.clone(), &prior_session.messages);
    transcript.push_user(args.user_content.clone(), args.attachments);

    let run_id = harness.spawn_run(
        client,
        RunParams {
            agent: AgentRef::new(&definition.id),
            gate: gate.clone(),
            question_gate: question_gate.clone(),
            transcript,
            enabled_tools: args.enabled_tools.clone(),
            compaction_policy: definition.compaction_policy.clone(),
            stream: args.stream,
            cancel: args.cancel_flag.clone(),
            parent: None,
        },
    );

    // 同步累积状态。事件经 broadcast 串行送达，无需 Mutex。
    let mut partial_output = String::new();
    let mut tool_calls: Vec<MessageToolCall> = Vec::new();
    let mut parts = AssistantPartsRecorder::default();
    let mut output_attachments: Vec<MessageAttachment> = Vec::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    let cleanup_hitl = || {
        if let Some(hitl) = &args.hitl {
            hitl.unregister_approval_gate(&gate);
            hitl.unregister_question_gate(&question_gate);
        }
    };

    let outcome: RunOutcome = loop {
        let event = match events_rx.recv().await {
            Ok(e) => e,
            Err(_) => {
                cleanup_hitl();
                return Err(AppError::msg("事件流意外关闭"));
            }
        };
        if event.run_id != run_id {
            continue;
        }

        if let EventPayload::TextDelta { text } = &event.payload {
            partial_output.push_str(text);
        }
        if let Some(hitl) = &args.hitl {
            match &event.payload {
                EventPayload::PermissionRequested { request_id, .. } => {
                    hitl.register_approval(request_id.0.clone(), Arc::clone(&gate));
                }
                EventPayload::UserQuestionRequested { request_id, .. } => {
                    hitl.register_question(request_id.0.clone(), Arc::clone(&question_gate));
                }
                _ => {}
            }
        }
        record_assistant_part_event(&mut parts, &event);
        record_tool_event(&mut tool_calls, &event);
        if let Some(ev) = agent_event_to_engine_event(&event) {
            emit_event(ev);
        }

        match event.payload {
            EventPayload::RunFinished {
                total_input_tokens: i,
                total_output_tokens: o,
                ..
            } => {
                total_input_tokens = i;
                total_output_tokens = o;
                break RunOutcome::Done;
            }
            EventPayload::RunFailed { error } => {
                break RunOutcome::Failed(error.message);
            }
            EventPayload::RunCancelled => {
                break RunOutcome::Cancelled;
            }
            EventPayload::TextDone { full_text } => {
                // 收尾用：保留最后一段 assistant 文本作为 fallback content
                output_attachments.clear();
                let _ = full_text;
            }
            _ => {}
        }
    };

    cleanup_hitl();

    let _ = total_input_tokens; // 暂不记录到 session
    let _ = total_output_tokens;

    let final_text = match outcome {
        RunOutcome::Done => parts.last_text_snapshot(),
        RunOutcome::Cancelled => {
            persist_interrupted_assistant_output(data_dir, &args.session_id, &partial_output)?;
            return Err(AppError::msg("请求已中断"));
        }
        RunOutcome::Failed(error) => {
            persist_failed_assistant_output(data_dir, &args.session_id, &partial_output, &error)?;
            return Err(AppError::msg(error));
        }
    };

    parts.append_final_text_if_missing(&final_text);
    let assistant_parts = parts.parts.clone();
    let assistant_content = text_from_parts(&assistant_parts)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| final_text.clone());

    let assistant_msg = Message {
        id: sessions::new_id(),
        role: Role::Assistant,
        content: assistant_content,
        attachments: output_attachments,
        tool_calls,
        parts: assistant_parts,
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
    };
    sessions::append_message(data_dir, &args.session_id, assistant_msg.clone())?;

    Ok(assistant_msg)
}

fn persist_interrupted_assistant_output(
    data_dir: &std::path::Path,
    session_id: &str,
    partial_output: &str,
) -> AppResult<Session> {
    let mut session = sessions::load(data_dir, session_id)?;
    if !partial_output.is_empty() {
        session.messages.push(Message {
            id: sessions::new_id(),
            role: Role::Assistant,
            content: partial_output.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: chrono::Utc::now().timestamp_millis(),
            meta: None,
        });
    }
    session.messages.push(Message {
        id: sessions::new_id(),
        role: Role::Marker,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: Some(MessageMeta::Interrupted),
    });
    sessions::save(data_dir, session)
}

fn persist_failed_assistant_output(
    data_dir: &std::path::Path,
    session_id: &str,
    partial_output: &str,
    error: &str,
) -> AppResult<Session> {
    let mut session = sessions::load(data_dir, session_id)?;
    session.messages.push(Message {
        id: sessions::new_id(),
        role: Role::Assistant,
        content: format_failed_assistant_content(partial_output, error),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
    });
    sessions::save(data_dir, session)
}

fn format_failed_assistant_content(partial_output: &str, error: &str) -> String {
    let partial = partial_output.trim();
    let error = error.trim();
    if partial.is_empty() {
        format!("请求失败：{error}")
    } else {
        format!("{partial}\n\n[请求失败：{error}]")
    }
}

#[derive(Default)]
struct AssistantPartsRecorder {
    parts: Vec<MessagePart>,
    by_index: HashMap<usize, usize>,
    by_id: HashMap<String, usize>,
}

impl AssistantPartsRecorder {
    fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        match self.parts.last_mut() {
            Some(MessagePart::Text { text: existing }) => existing.push_str(text),
            _ => self.parts.push(MessagePart::Text {
                text: text.to_string(),
            }),
        }
    }

    fn append_final_text_if_missing(&mut self, final_text: &str) {
        if final_text.is_empty() {
            return;
        }

        let current = text_from_parts(&self.parts).unwrap_or_default();
        if !current.ends_with(final_text) {
            self.append_text(final_text);
        }
    }

    /// 当前已累积文本（仅 Text part 的拼接）
    fn last_text_snapshot(&self) -> String {
        text_from_parts(&self.parts).unwrap_or_default()
    }

    fn apply_tool_delta(
        &mut self,
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        arguments_delta: Option<&str>,
    ) {
        let pos = self.tool_position(index, id, name);
        let MessagePart::ToolCall {
            id: existing_id,
            name: existing_name,
            arguments,
            ..
        } = &mut self.parts[pos]
        else {
            return;
        };

        if let Some(next_id) = id.filter(|value| !value.trim().is_empty()) {
            *existing_id = next_id.to_string();
            self.by_id.insert(next_id.to_string(), pos);
        }
        if let Some(next_name) = name.filter(|value| !value.trim().is_empty()) {
            *existing_name = next_name.to_string();
        }
        if let Some(delta) = arguments_delta.filter(|value| !value.is_empty()) {
            arguments.push_str(delta);
        }
    }

    fn start_tool(&mut self, index: usize, call_id: &str, name: &str, input: serde_json::Value) {
        let pos = self.tool_position(index, Some(call_id), Some(name));
        if let MessagePart::ToolCall {
            id,
            name: existing_name,
            input: existing_input,
            arguments,
            ..
        } = &mut self.parts[pos]
        {
            *id = call_id.to_string();
            *existing_name = name.to_string();
            *existing_input = input.clone();
            if arguments.is_empty() {
                *arguments = input.to_string();
            }
            self.by_id.insert(call_id.to_string(), pos);
        }
    }

    fn finish_tool(&mut self, index: usize, call_id: &str, result: &str, duration_ms: u64) {
        let pos = self.tool_position(index, Some(call_id), None);
        if let MessagePart::ToolCall {
            id,
            result: existing_result,
            duration_ms: existing_duration_ms,
            ..
        } = &mut self.parts[pos]
        {
            *id = call_id.to_string();
            *existing_result = Some(result.to_string());
            *existing_duration_ms = Some(duration_ms);
            self.by_id.insert(call_id.to_string(), pos);
        }
    }

    fn tool_position(&mut self, index: usize, id: Option<&str>, name: Option<&str>) -> usize {
        if let Some(pos) = id
            .filter(|value| !value.trim().is_empty())
            .and_then(|value| self.by_id.get(value).copied())
        {
            self.by_index.entry(index).or_insert(pos);
            return pos;
        }

        if let Some(pos) = self.by_index.get(&index).copied() {
            return pos;
        }

        let pos = self.parts.len();
        let clean_id = id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default()
            .to_string();
        self.parts.push(MessagePart::ToolCall {
            id: clean_id.clone(),
            name: name
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_default()
                .to_string(),
            input: serde_json::json!({}),
            arguments: String::new(),
            result: None,
            duration_ms: None,
        });
        self.by_index.insert(index, pos);
        if !clean_id.is_empty() {
            self.by_id.insert(clean_id, pos);
        }
        pos
    }
}

fn record_assistant_part_event(parts: &mut AssistantPartsRecorder, event: &AgentEvent) {
    match &event.payload {
        AgentEventPayload::TextDelta { text } => parts.append_text(text),
        AgentEventPayload::ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } => parts.apply_tool_delta(
            *index,
            id.as_deref(),
            name.as_deref(),
            arguments_delta.as_deref(),
        ),
        AgentEventPayload::ToolCallStarted {
            index,
            call_id,
            name,
            input,
        } => parts.start_tool(*index, call_id, name, input.clone()),
        AgentEventPayload::ToolCallFinished {
            index,
            call_id,
            result,
            duration_ms,
            ..
        } => parts.finish_tool(*index, call_id, result, *duration_ms),
        _ => {}
    }
}

fn text_from_parts(parts: &[MessagePart]) -> Option<String> {
    let mut out = String::new();
    for part in parts {
        if let MessagePart::Text { text } = part {
            out.push_str(text);
        }
    }
    Some(out)
}

fn record_tool_event(tool_calls: &mut Vec<MessageToolCall>, event: &AgentEvent) {
    match &event.payload {
        AgentEventPayload::ToolCallStarted {
            index,
            call_id,
            name,
            input,
        } => {
            upsert_tool_call(tool_calls, *index, call_id, name, input.clone());
        }
        AgentEventPayload::ToolCallFinished {
            index,
            call_id,
            result,
            duration_ms,
            ..
        } => {
            if tool_calls.len() <= *index {
                tool_calls.resize_with(*index + 1, empty_tool_call);
            }
            let call = &mut tool_calls[*index];
            call.id = call_id.clone();
            call.result = Some(result.clone());
            call.duration_ms = Some(*duration_ms);
        }
        _ => {}
    }
}

fn upsert_tool_call(
    calls: &mut Vec<MessageToolCall>,
    index: usize,
    call_id: &str,
    name: &str,
    input: serde_json::Value,
) {
    if calls.len() <= index {
        calls.resize_with(index + 1, empty_tool_call);
    }

    let call = &mut calls[index];
    call.id = call_id.to_string();
    call.name = name.to_string();
    call.input = input;
}

fn empty_tool_call() -> MessageToolCall {
    MessageToolCall {
        id: String::new(),
        name: String::new(),
        input: serde_json::json!({}),
        result: None,
        duration_ms: None,
    }
}

pub async fn send_once(
    provider: &Provider,
    model: &str,
    system: Option<&str>,
    messages: &[Message],
) -> AppResult<String> {
    use model_gateway::types::{
        AssistantEntry, ModelRequest, ModelResponse, TranscriptEntry, UserEntry,
    };

    let client = model_gateway::build_client(provider.clone())
        .map_err(|e| AppError::msg(format!("无法创建 ModelClient: {e}")))?;
    let entries = messages
        .iter()
        .filter_map(|m| match m.role {
            Role::User => Some(TranscriptEntry::User(UserEntry {
                text: m.content.clone(),
                attachments: m.attachments.clone(),
            })),
            Role::Assistant => Some(TranscriptEntry::Assistant(AssistantEntry {
                text: m.content.clone(),
                tool_calls: Vec::new(),
            })),
            _ => None,
        })
        .collect();
    let req = ModelRequest {
        model: model.to_string(),
        system: system.map(str::to_string),
        entries,
        tools: Vec::new(),
        max_tokens: 4096,
    };
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    match client
        .complete(req, cancel)
        .await
        .map_err(|e| AppError::msg(e.to_string()))?
    {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => Ok(text),
    }
}

fn agent_event_to_engine_event(event: &AgentEvent) -> Option<EngineEvent> {
    use agent_core::types::AgentEventPayload::*;
    match &event.payload {
        TextDelta { text } => Some(EngineEvent::TextDelta { text: text.clone() }),
        TextDone { full_text } => Some(EngineEvent::TextDone {
            full_text: full_text.clone(),
        }),
        ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } => Some(EngineEvent::ToolCallDelta {
            index: *index,
            id: id.clone(),
            name: name.clone(),
            arguments_delta: arguments_delta.clone(),
        }),
        ToolCallStarted {
            index,
            call_id,
            name,
            input,
        } => Some(EngineEvent::ToolStart {
            index: *index,
            id: call_id.clone(),
            name: name.clone(),
            input: input.clone(),
        }),
        ToolCallFinished {
            index,
            call_id,
            result,
            duration_ms,
            ..
        } => Some(EngineEvent::ToolDone {
            index: *index,
            id: call_id.clone(),
            result: result.clone(),
            duration_ms: *duration_ms,
        }),
        RunFailed { error } => Some(EngineEvent::Error {
            message: error.message.clone(),
        }),
        PermissionRequested {
            request_id,
            kind,
            summary,
            risk,
        } => {
            let (tool_name, tool_input) = match kind {
                agent_core::types::PermissionKind::ToolCall { tool_name, input } => {
                    (tool_name.clone(), input.clone())
                }
                _ => (String::new(), serde_json::Value::Null),
            };
            Some(EngineEvent::PermissionRequested {
                request_id: request_id.0.clone(),
                tool_name,
                input: tool_input,
                summary: summary.clone(),
                risk: format!("{risk:?}").to_lowercase(),
            })
        }
        PermissionResolved {
            request_id,
            decision,
        } => Some(EngineEvent::PermissionResolved {
            request_id: request_id.0.clone(),
            decision: match decision {
                agent_core::types::ApprovalDecision::AllowOnce => "allow_once".into(),
                agent_core::types::ApprovalDecision::AllowAndRemember { .. } => {
                    "allow_and_remember".into()
                }
                agent_core::types::ApprovalDecision::Deny => "deny".into(),
                agent_core::types::ApprovalDecision::DenyWithFeedback { .. } => {
                    "deny_with_feedback".into()
                }
            },
        }),
        UserQuestionRequested {
            request_id,
            question,
            options,
        } => Some(EngineEvent::UserQuestionRequested {
            request_id: request_id.0.clone(),
            question: question.clone(),
            options: options
                .iter()
                .map(|o| crate::engine::QuestionOptionDto {
                    label: o.label.clone(),
                    description: o.description.clone(),
                })
                .collect(),
        }),
        UserQuestionAnswered { request_id, answer } => {
            let (kind, text) = match answer {
                protocol::UserAnswer::Selected { label } => ("selected", label.clone()),
                protocol::UserAnswer::Custom { text } => ("custom", text.clone()),
                protocol::UserAnswer::Cancelled => ("cancelled", String::new()),
            };
            Some(EngineEvent::UserQuestionAnswered {
                request_id: request_id.0.clone(),
                kind: kind.to_string(),
                text,
            })
        }
        _ => None,
    }
}

use async_trait::async_trait;
use model_gateway::{
    client::{DynModelClient, ModelClient},
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};

struct ModelWithName {
    inner: DynModelClient,
    model: String,
}

impl ModelWithName {
    fn new(inner: DynModelClient, model: String) -> Self {
        Self { inner, model }
    }

    fn patch_model(&self, mut req: ModelRequest) -> ModelRequest {
        req.model = self.model.clone();
        req
    }
}

#[async_trait]
impl ModelClient for ModelWithName {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }
    fn supports_streaming_tools(&self) -> bool {
        self.inner.supports_streaming_tools()
    }
    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        self.inner.complete(self.patch_model(req), cancel).await
    }
    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        self.inner
            .stream(self.patch_model(req), cancel, on_event)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_gateway::{
        config::{AuthMode, ProviderKind, ProvidersFile},
        types::{ToolCall, ToolCallStreamDelta, Usage},
    };
    use platform::storage::sessions::MessageMeta;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_data_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hebbian-interrupted-output-test-{}",
            sessions::new_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn save_test_provider(data_dir: &std::path::Path) {
        model_gateway::config::save(
            data_dir,
            &ProvidersFile {
                providers: vec![Provider {
                    id: "openai".to_string(),
                    name: "OpenAI".to_string(),
                    kind: ProviderKind::Openai,
                    auth_mode: AuthMode::ApiKey,
                    base_url: "https://example.test/v1".to_string(),
                    api_key: "test".to_string(),
                    refresh_token: None,
                    token_expires_at: None,
                    account_id: None,
                    extra_headers: BTreeMap::new(),
                    models: vec!["gpt-test".to_string()],
                    default_model: Some("gpt-test".to_string()),
                }],
                default_provider_id: Some("openai".to_string()),
            },
        )
        .unwrap();
    }

    struct OrderedPartsClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for OrderedPartsClient {
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
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "先说".to_string(),
                    });
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 0,
                        id: Some("call_a".to_string()),
                        name: Some("missing_a".to_string()),
                        arguments_delta: Some("{\"a\":1}".to_string()),
                    }));
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 1,
                        id: Some("call_b".to_string()),
                        name: Some("missing_b".to_string()),
                        arguments_delta: Some("{\"b\":2}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        calls: vec![
                            ToolCall {
                                id: "call_a".to_string(),
                                name: "missing_a".to_string(),
                                input: serde_json::json!({"a": 1}),
                            },
                            ToolCall {
                                id: "call_b".to_string(),
                                name: "missing_b".to_string(),
                                input: serde_json::json!({"b": 2}),
                            },
                        ],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "后说".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        text: "后说".to_string(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    struct RepeatedLocalIndexClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelClient for RepeatedLocalIndexClient {
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
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第一段".to_string(),
                    });
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 0,
                        id: Some("call_first".to_string()),
                        name: Some("missing_first".to_string()),
                        arguments_delta: Some("{\"first\":true}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        calls: vec![ToolCall {
                            id: "call_first".to_string(),
                            name: "missing_first".to_string(),
                            input: serde_json::json!({"first": true}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                1 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "第二段".to_string(),
                    });
                    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
                        index: 0,
                        id: Some("call_second".to_string()),
                        name: Some("missing_second".to_string()),
                        arguments_delta: Some("{\"second\":true}".to_string()),
                    }));
                    Ok(ModelResponse::ToolCalls {
                        text: String::new(),
                        calls: vec![ToolCall {
                            id: "call_second".to_string(),
                            name: "missing_second".to_string(),
                            input: serde_json::json!({"second": true}),
                        }],
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                2 => {
                    on_event(ModelStreamEvent::TextDelta {
                        text: "结束".to_string(),
                    });
                    Ok(ModelResponse::Done {
                        text: "结束".to_string(),
                        attachments: Vec::new(),
                        usage: Usage::default(),
                    })
                }
                _ => unreachable!("unexpected extra model call"),
            }
        }
    }

    #[test]
    fn persist_interrupted_output_appends_partial_assistant_then_marker() {
        let data_dir = temp_data_dir();
        let session = sessions::create(
            &data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .unwrap();

        persist_interrupted_assistant_output(&data_dir, &session.id, "partial answer").unwrap();

        let saved = sessions::load(&data_dir, &session.id).unwrap();
        assert_eq!(saved.messages.len(), 2);
        assert_eq!(saved.messages[0].role, Role::Assistant);
        assert_eq!(saved.messages[0].content, "partial answer");
        assert_eq!(saved.messages[1].role, Role::Marker);
        assert!(matches!(
            saved.messages[1].meta,
            Some(MessageMeta::Interrupted)
        ));

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn persist_failed_output_appends_assistant_error_message() {
        let data_dir = temp_data_dir();
        let session = sessions::create(
            &data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .unwrap();

        persist_failed_assistant_output(
            &data_dir,
            &session.id,
            "partial answer",
            "HTTP 400: missing name",
        )
        .unwrap();

        let saved = sessions::load(&data_dir, &session.id).unwrap();
        assert_eq!(saved.messages.len(), 1);
        assert_eq!(saved.messages[0].role, Role::Assistant);
        assert!(saved.messages[0].content.contains("partial answer"));
        assert!(saved.messages[0].content.contains("HTTP 400: missing name"));

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn saves_assistant_parts_in_stream_arrival_order() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    session_id: session.id,
                    user_content: "run tools".to_string(),
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: vec!["missing_a".to_string(), "missing_b".to_string()],
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    hitl: None,
                },
                |_| {},
                |_provider, _model| {
                    Ok(Arc::new(OrderedPartsClient {
                        calls: AtomicUsize::new(0),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "先说后说");
            assert_eq!(assistant.parts.len(), 4);
            assert!(matches!(
                &assistant.parts[0],
                platform::storage::sessions::MessagePart::Text { text } if text == "先说"
            ));
            assert!(matches!(
                &assistant.parts[1],
                platform::storage::sessions::MessagePart::ToolCall { name, .. } if name == "missing_a"
            ));
            assert!(matches!(
                &assistant.parts[2],
                platform::storage::sessions::MessagePart::ToolCall { name, .. } if name == "missing_b"
            ));
            assert!(matches!(
                &assistant.parts[3],
                platform::storage::sessions::MessagePart::Text { text } if text == "后说"
            ));

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }

    #[test]
    fn saves_repeated_local_tool_indexes_as_distinct_dispatches() {
        tauri::async_runtime::block_on(async {
            let data_dir = temp_data_dir();
            save_test_provider(&data_dir);
            let session = sessions::create(
                &data_dir,
                "openai".to_string(),
                "gpt-test".to_string(),
                None,
                None,
            )
            .unwrap();

            let assistant = send_and_save_in_data_dir_with_client_factory(
                &data_dir,
                SendArgs {
                    session_id: session.id,
                    user_content: "run tools".to_string(),
                    attachments: Vec::new(),
                    stream: true,
                    enabled_tools: vec!["missing_first".to_string(), "missing_second".to_string()],
                    cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    hitl: None,
                },
                |_| {},
                |_provider, _model| {
                    Ok(Arc::new(RepeatedLocalIndexClient {
                        calls: AtomicUsize::new(0),
                    }) as Arc<dyn ModelClient>)
                },
            )
            .await
            .unwrap();

            assert_eq!(assistant.content, "第一段第二段结束");
            let tool_names: Vec<&str> = assistant
                .parts
                .iter()
                .filter_map(|part| match part {
                    platform::storage::sessions::MessagePart::ToolCall { name, .. } => {
                        Some(name.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(tool_names, vec!["missing_first", "missing_second"]);

            std::fs::remove_dir_all(data_dir).unwrap();
        });
    }
}
