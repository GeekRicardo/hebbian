use crate::engine::EngineEvent;
use crate::error::{AppError, AppResult};
use crate::hitl::HitlState;
use agent_core::{
    Harness, Session as CoreSession, SessionConfig, TurnObserver, TurnOutcome,
    context::transcript::Transcript,
    definition::AgentDefinition,
    hooks::HookManager,
    tools::{hitl::HitlGate, skill::default_skill_dirs},
    types::{AgentEvent, AgentEventPayload},
    workspace::Workspace,
};
use async_trait::async_trait;
use model_gateway::{self, config::Provider};
use platform::{
    CancelFlag,
    attachments::MessageAttachment,
    config::settings as global_settings,
    storage::sessions::{
        self, Message, MessageMeta, MessagePart, MessageToolCall, Role, Session, TokenStats,
    },
};
use protocol::{
    ApprovalDecision, EventPayload, PermissionKind, PermissionRequestId, QuestionOption,
    UserAnswer,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Manager, ipc::Channel};

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

    // Workspace：session 字段优先；没设则用全局设置；都没设则 ~/
    let settings = global_settings::load(data_dir);
    let workdir = session
        .workdir
        .clone()
        .or_else(|| settings.conversation.workdir.clone())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let allowed_dirs = session
        .allowed_dirs
        .clone()
        .unwrap_or_else(|| settings.conversation.allowed_dirs.clone());
    let workspace = Workspace::new(workdir.clone(), allowed_dirs);

    let configured_skill_dirs = session
        .skill_dirs
        .clone()
        .unwrap_or_else(|| settings.conversation.skill_dirs.clone());
    let skill_dirs = if configured_skill_dirs.is_empty() {
        default_skill_dirs(&workdir)
    } else {
        configured_skill_dirs
    };

    let harness = Arc::new(Harness::new(
        agent_core::tools::default_tools(workspace.clone(), &skill_dirs),
        HookManager::empty(),
    ));
    let definition = AgentDefinition::default();

    let session_enabled_tools = session
        .enabled_tools
        .clone()
        .unwrap_or_else(|| settings.conversation.enabled_tools.clone());
    let effective_enabled_tools = if args.enabled_tools.is_empty() {
        session_enabled_tools
    } else {
        args.enabled_tools.clone()
    };

    let mut core_session = CoreSession::new(
        harness,
        SessionConfig {
            definition,
            workspace,
            client,
            enabled_tools: effective_enabled_tools,
            initial_transcript: Transcript::from_session(
                session.system_prompt.clone(),
                &prior_session.messages,
            ),
            recorder: None,
        },
    );
    core_session.append_user(args.user_content.clone(), args.attachments);

    let mut handle = core_session.run_with(args.cancel_flag.clone());
    let hitl = handle.hitl().clone();

    let mut observer = DesktopObserver::new(args.hitl.clone(), hitl.clone(), &emit_event);
    let summary = handle.drive(&mut observer).await;
    if let Some(state) = &args.hitl {
        state.forget(&hitl);
    }

    // 不论 Done / Cancelled / Failed 都把这一轮的 token 用量累加进 session.json，
    // 让前端 TokenStatsPanel 即使在中断/失败的情况下也能反映已扣费的部分。
    if let Some(usage) = summary.usage {
        accumulate_session_tokens(
            data_dir,
            &args.session_id,
            TokenStats {
                input_tokens: usage.input,
                output_tokens: usage.output,
                cache_read_tokens: usage.cache_read,
                cache_creation_tokens: usage.cache_creation,
                run_count: 1,
            },
        );
    }

    let DesktopObserver {
        mut parts,
        partial_output,
        tool_calls,
        output_attachments,
        ..
    } = observer;

    match summary.outcome {
        TurnOutcome::Done => {}
        TurnOutcome::Cancelled => {
            persist_interrupted_assistant_output(
                data_dir,
                &args.session_id,
                &partial_output,
                &parts.parts,
                &tool_calls,
            )?;
            return Err(AppError::msg("请求已中断"));
        }
        TurnOutcome::Failed(error) => {
            persist_failed_assistant_output(
                data_dir,
                &args.session_id,
                &partial_output,
                &parts.parts,
                &tool_calls,
                &error,
            )?;
            return Err(AppError::msg(error));
        }
    }

    let final_text = parts.last_text_snapshot();
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

/// Desktop 端 [`TurnObserver`] 实现：累积 assistant parts / tool_calls / partial_output，
/// 把每个事件翻译成 `EngineEvent` 推送给 React，并把 HITL pending 注册到全局桥接。
struct DesktopObserver<'a> {
    parts: AssistantPartsRecorder,
    partial_output: String,
    tool_calls: Vec<MessageToolCall>,
    output_attachments: Vec<MessageAttachment>,
    hitl_state: Option<Arc<HitlState>>,
    hitl: Arc<HitlGate>,
    emit: &'a (dyn Fn(EngineEvent) + Send + Sync),
}

impl<'a> DesktopObserver<'a> {
    fn new(
        hitl_state: Option<Arc<HitlState>>,
        hitl: Arc<HitlGate>,
        emit: &'a (dyn Fn(EngineEvent) + Send + Sync),
    ) -> Self {
        Self {
            parts: AssistantPartsRecorder::default(),
            partial_output: String::new(),
            tool_calls: Vec::new(),
            output_attachments: Vec::new(),
            hitl_state,
            hitl,
            emit,
        }
    }
}

#[async_trait]
impl<'a> TurnObserver for DesktopObserver<'a> {
    fn on_event(&mut self, event: &AgentEvent) {
        if let EventPayload::TextDelta { text } = &event.payload {
            self.partial_output.push_str(text);
        }
        if let EventPayload::TextDone { full_text } = &event.payload {
            // complete 路径只发 TextDone 不发 TextDelta；补一次 append 避免落盘空文本。
            if !full_text.is_empty() {
                if !self.parts.last_text_snapshot().ends_with(full_text.as_str()) {
                    self.parts.append_text(full_text);
                }
                if self.partial_output.is_empty() {
                    self.partial_output.push_str(full_text);
                }
            }
        }
        record_assistant_part_event(&mut self.parts, event);
        record_tool_event(&mut self.tool_calls, event);
        if let Some(ev) = agent_event_to_engine_event(event) {
            (self.emit)(ev);
        }
    }

    async fn on_permission_request(
        &mut self,
        request_id: &PermissionRequestId,
        _kind: &PermissionKind,
        _summary: &str,
    ) -> Option<ApprovalDecision> {
        if let Some(state) = &self.hitl_state {
            state.track(request_id.0.clone(), Arc::clone(&self.hitl));
        }
        None
    }

    async fn on_question(
        &mut self,
        request_id: &PermissionRequestId,
        _question: &str,
        _options: &[QuestionOption],
    ) -> Option<UserAnswer> {
        if let Some(state) = &self.hitl_state {
            state.track(request_id.0.clone(), Arc::clone(&self.hitl));
        }
        None
    }
}

fn persist_interrupted_assistant_output(
    data_dir: &std::path::Path,
    session_id: &str,
    partial_output: &str,
    parts: &[MessagePart],
    tool_calls: &[MessageToolCall],
) -> AppResult<Session> {
    let mut session = sessions::load(data_dir, session_id)?;
    if let Some(message) = assistant_message_from_partial(partial_output, parts, tool_calls) {
        session.messages.push(message);
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
    parts: &[MessagePart],
    tool_calls: &[MessageToolCall],
    error: &str,
) -> AppResult<Session> {
    let mut session = sessions::load(data_dir, session_id)?;
    session.messages.push(failed_assistant_message(
        partial_output,
        parts,
        tool_calls,
        error,
    ));
    sessions::save(data_dir, session)
}

fn assistant_message_from_partial(
    partial_output: &str,
    parts: &[MessagePart],
    tool_calls: &[MessageToolCall],
) -> Option<Message> {
    let assistant_parts = normalized_partial_parts(partial_output, parts);
    let assistant_content = text_from_parts(&assistant_parts)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| partial_output.to_string());

    if assistant_content.is_empty() && assistant_parts.is_empty() && tool_calls.is_empty() {
        return None;
    }

    Some(Message {
        id: sessions::new_id(),
        role: Role::Assistant,
        content: assistant_content,
        attachments: Vec::new(),
        tool_calls: tool_calls.to_vec(),
        parts: assistant_parts,
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
    })
}

fn failed_assistant_message(
    partial_output: &str,
    parts: &[MessagePart],
    tool_calls: &[MessageToolCall],
    error: &str,
) -> Message {
    let mut assistant_parts = normalized_partial_parts(partial_output, parts);
    let error_marker = format!("[请求失败：{}]", error.trim());
    if !error_marker.trim().is_empty() {
        if !assistant_parts.is_empty() {
            assistant_parts.push(MessagePart::Text {
                text: format!("\n\n{error_marker}"),
            });
        } else {
            assistant_parts.push(MessagePart::Text { text: error_marker });
        }
    }
    let assistant_content = text_from_parts(&assistant_parts)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| format_failed_assistant_content(partial_output, error));

    Message {
        id: sessions::new_id(),
        role: Role::Assistant,
        content: assistant_content,
        attachments: Vec::new(),
        tool_calls: tool_calls.to_vec(),
        parts: assistant_parts,
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
    }
}

fn normalized_partial_parts(partial_output: &str, parts: &[MessagePart]) -> Vec<MessagePart> {
    if !parts.is_empty() {
        return parts.to_vec();
    }
    if partial_output.is_empty() {
        Vec::new()
    } else {
        vec![MessagePart::Text {
            text: partial_output.to_string(),
        }]
    }
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

    fn append_reasoning(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.parts.last_mut() {
            Some(MessagePart::Reasoning { text: existing }) => existing.push_str(text),
            _ => self.parts.push(MessagePart::Reasoning {
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
        AgentEventPayload::Reasoning { text } => parts.append_reasoning(text),
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

/// 当前会话的上下文用量。前端用来渲染输入框旁的环形进度条。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextUsageDto {
    pub used_tokens: usize,
    pub budget_tokens: usize,
}

/// 把这一轮 run 的 token delta 累加进 session.json 的 token_stats 字段。
/// 失败不传染（拿不到 session 文件 / 序列化失败也不能影响主请求结果）。
fn accumulate_session_tokens(data_dir: &Path, session_id: &str, delta: TokenStats) {
    let Ok(mut session) = sessions::load(data_dir, session_id) else {
        return;
    };
    let mut stats = session.token_stats.unwrap_or_default();
    stats.accumulate(delta);
    session.token_stats = Some(stats);
    let _ = sessions::save(data_dir, session);
}

/// 计算指定 session 的上下文用量。直接复用 [`agent_core::context::budget`]
/// 估算器，与发起 run 时看到的口径一致。
pub fn context_usage(data_dir: &Path, session_id: &str) -> AppResult<ContextUsageDto> {
    let session = sessions::load(data_dir, session_id)?;
    let transcript = Transcript::from_session(session.system_prompt.clone(), &session.messages);
    let used = agent_core::context::budget::estimate_transcript_tokens(
        transcript.system.as_deref(),
        &transcript.entries,
    );
    let budget = AgentDefinition::default().compaction_policy.token_budget;
    Ok(ContextUsageDto {
        used_tokens: used,
        budget_tokens: budget,
    })
}

/// 主动压缩：调一次模型把当前 transcript 浓缩成摘要，然后在 session 里
/// 追加一条 [`Role::Marker`] + [`MessageMeta::CompactBoundary`] 标记，
/// 后续读取该 session 时 `Transcript::from_session` 会跳过标记之前的所有消息。
pub async fn compact_session(
    data_dir: &Path,
    session_id: &str,
    custom_instructions: Option<String>,
) -> AppResult<ContextUsageDto> {
    let session = sessions::load(data_dir, session_id)?;
    let provider = model_gateway::config::get(data_dir, &session.provider_id)?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| AppError::msg(format!("OAuth token 刷新失败: {e}")))?;
    let model = session.model.clone();
    let inner = model_gateway::build_client(provider)
        .map_err(|e| AppError::msg(format!("无法创建 ModelClient: {e}")))?;
    let client: Arc<dyn ModelClient> = Arc::new(ModelWithName::new(inner, model));

    let transcript = Transcript::from_session(session.system_prompt.clone(), &session.messages);
    let result = agent_core::context::compaction::compact_with_llm(
        client.as_ref(),
        transcript.system.as_deref(),
        transcript.entries,
        custom_instructions.as_deref(),
    )
    .await
    .map_err(|e| AppError::msg(format!("压缩失败: {e}")))?;

    let marker = Message {
        id: sessions::new_id(),
        role: Role::Marker,
        content: result.summary.clone(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: Some(MessageMeta::CompactBoundary {
            summary: result.summary.clone(),
            before_tokens: result.before_tokens,
            after_tokens: result.after_tokens,
        }),
    };
    sessions::append_message(data_dir, session_id, marker)?;

    Ok(ContextUsageDto {
        used_tokens: result.after_tokens,
        budget_tokens: AgentDefinition::default().compaction_policy.token_budget,
    })
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
                reasoning: String::new(),
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
        reasoning: None,
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
        Reasoning { text } => Some(EngineEvent::Reasoning { text: text.clone() }),
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
            let (kind_str, tool_name, tool_input, paths) = match kind {
                agent_core::types::PermissionKind::ToolCall { tool_name, input } => (
                    "tool_call",
                    tool_name.clone(),
                    input.clone(),
                    Vec::<String>::new(),
                ),
                agent_core::types::PermissionKind::PathAccess { tool_name, paths } => (
                    "path_access",
                    tool_name.clone(),
                    serde_json::Value::Null,
                    paths.clone(),
                ),
                agent_core::types::PermissionKind::Plan { .. } => {
                    ("plan", String::new(), serde_json::Value::Null, Vec::new())
                }
                agent_core::types::PermissionKind::ContinueLongRun { .. } => (
                    "continue_long_run",
                    String::new(),
                    serde_json::Value::Null,
                    Vec::new(),
                ),
            };
            Some(EngineEvent::PermissionRequested {
                request_id: request_id.0.clone(),
                kind: kind_str.into(),
                tool_name,
                input: tool_input,
                summary: summary.clone(),
                risk: format!("{risk:?}").to_lowercase(),
                paths,
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
                    enabled: true,
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
                        reasoning: String::new(),
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
                        reasoning: String::new(),
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
                        reasoning: String::new(),
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
                        reasoning: String::new(),
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
                        reasoning: String::new(),
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

        persist_interrupted_assistant_output(&data_dir, &session.id, "partial answer", &[], &[])
            .unwrap();

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
            &[],
            &[],
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
    fn persist_failed_output_preserves_structured_parts_and_tool_calls() {
        let data_dir = temp_data_dir();
        let session = sessions::create(
            &data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .unwrap();
        let parts = vec![
            MessagePart::Text {
                text: "准备执行".to_string(),
            },
            MessagePart::ToolCall {
                id: "call_bash".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "pwd"}),
                arguments: "{\"command\":\"pwd\"}".to_string(),
                result: Some("/tmp\n".to_string()),
                duration_ms: Some(12),
            },
        ];
        let calls = vec![MessageToolCall {
            id: "call_bash".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "pwd"}),
            result: Some("/tmp\n".to_string()),
            duration_ms: Some(12),
        }];

        persist_failed_assistant_output(
            &data_dir,
            &session.id,
            "准备执行",
            &parts,
            &calls,
            "provider refused follow-up",
        )
        .unwrap();

        let saved = sessions::load(&data_dir, &session.id).unwrap();
        assert_eq!(saved.messages.len(), 1);
        let message = &saved.messages[0];
        assert_eq!(message.role, Role::Assistant);
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].name, "Bash");
        assert_eq!(message.parts.len(), 3);
        assert!(matches!(
            &message.parts[1],
            MessagePart::ToolCall { name, result, .. }
                if name == "Bash" && result.as_deref() == Some("/tmp\n")
        ));
        assert!(message.content.contains("准备执行"));
        assert!(message.content.contains("provider refused follow-up"));

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
