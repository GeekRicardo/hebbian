//! 渠道消息与 hebbian agent_core 的桥接。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use agent_core::context::transcript::Transcript;
use agent_core::core_client::LocalCoreClient;
use agent_core::definition::AgentDefinition;
use agent_core::edits::EditsWorktree;
use agent_core::hooks::HookManager;
use agent_core::permissions::PermissionStore;
use agent_core::read_state::ReadStateTracker;
use agent_core::storage::{sessions, sessions_dir, settings as settings_store};
use agent_core::tools::{background, skill::default_skill_dirs};
use agent_core::workspace::Workspace;
use agent_core::{Harness, Session as CoreSession, SessionConfig, TurnObserver, TurnOutcome};
use anyhow::anyhow;
use async_trait::async_trait;
use channel_core::commands::{self, CommandResult};
use channel_core::contract::Channel;
use channel_core::message::{InboundMessage, OutboundMessage};
use channel_core::owner_state::OwnerState;
use chrono::Utc;
use common::runtime::PendingInputs;
use model_gateway::client::{DynModelClient, ModelClient};
use model_gateway::types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent};
use protocol::{
    ApprovalDecision, Event as AgentEvent, EventPayload, PermissionKind, PermissionRequestId,
    QuestionOption, UserAnswer,
};
use serde_json::Value;
use tokio::sync::oneshot;
use tracing::{error, info, warn};

struct PendingInteractions {
    approvals: HashMap<String, oneshot::Sender<ApprovalDecision>>,
    questions: HashMap<String, oneshot::Sender<UserAnswer>>,
}

impl PendingInteractions {
    fn new() -> Self {
        Self {
            approvals: HashMap::new(),
            questions: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct ChannelBridge {
    pub data_dir: PathBuf,
    pending: Arc<Mutex<PendingInteractions>>,
    active_run: Arc<AtomicBool>,
}

impl ChannelBridge {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            pending: Arc::new(Mutex::new(PendingInteractions::new())),
            active_run: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn run_loop(
        &self,
        channel: Arc<dyn Channel>,
        state: &mut OwnerState,
        account_id: &str,
    ) -> anyhow::Result<()> {
        info!(channel = channel.id(), account_id, "渠道网关启动");
        loop {
            let messages = match channel.poll().await {
                Ok(messages) => messages,
                Err(err) => {
                    warn!(error = %err, "渠道 poll 失败，5 秒后重试");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            for message in messages {
                if let Err(err) = self.handle_message(channel.clone(), state, account_id, message).await {
                    error!(error = %err, "处理渠道消息失败");
                }
            }
        }
    }

    async fn handle_message(
        &self,
        channel: Arc<dyn Channel>,
        state: &mut OwnerState,
        account_id: &str,
        message: InboundMessage,
    ) -> anyhow::Result<()> {
        if self.try_resolve_pending(&message).await? {
            return Ok(());
        }

        let permission_store = PermissionStore::open(&self.data_dir).ok().map(Arc::new);
        let core = LocalCoreClient::new(None, self.data_dir.clone(), permission_store);
        match commands::dispatch(
            &message.text,
            state,
            &core,
            &self.data_dir,
            channel.id(),
            account_id,
        ) {
            CommandResult::Reply(reply) => {
                channel
                    .send_text(&OutboundMessage {
                        to: message.from,
                        text: reply,
                        channel_context: message.channel_context,
                    })
                    .await?;
            }
            CommandResult::NotCommand => {
                if state.active_session_id.is_none() {
                    channel
                        .send_text(&OutboundMessage {
                            to: message.from,
                            text: "还没有活跃对话。用 /new 创建一个，或 /help 查看帮助。".into(),
                            channel_context: message.channel_context,
                        })
                        .await?;
                    return Ok(());
                }
                if self.active_run.swap(true, Ordering::SeqCst) {
                    channel
                        .send_text(&OutboundMessage {
                            to: message.from,
                            text: "上一轮还在运行中，请稍后再发。".into(),
                            channel_context: message.channel_context,
                        })
                        .await?;
                    return Ok(());
                }

                let bridge = self.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    let result = bridge.run_agent_turn(channel.clone(), state, message).await;
                    bridge.active_run.store(false, Ordering::SeqCst);
                    if let Err(err) = result {
                        error!(error = %err, "agent run 失败");
                    }
                });
            }
        }
        Ok(())
    }

    async fn try_resolve_pending(&self, message: &InboundMessage) -> anyhow::Result<bool> {
        let text = message.text.trim();
        let lower = text.to_ascii_lowercase();
        let mut pending = self.pending.lock().unwrap();

        if let Some((request_id, tx)) = pending.approvals.drain().next() {
            let decision = match lower.as_str() {
                "y" | "yes" | "允许" | "通过" => ApprovalDecision::AllowOnce,
                other if other.starts_with("deny ") || other.starts_with("拒绝 ") => {
                    ApprovalDecision::DenyWithFeedback {
                        feedback: text
                            .split_once(char::is_whitespace)
                            .map(|(_, rest)| rest.to_string())
                            .unwrap_or_else(|| "用户拒绝".to_string()),
                    }
                }
                _ => ApprovalDecision::Deny,
            };
            let _ = tx.send(decision);
            info!(request_id, "已从渠道文本解析审批回复");
            return Ok(true);
        }

        if let Some((request_id, tx)) = pending.questions.drain().next() {
            let answer = if lower == "cancel" || lower == "取消" {
                UserAnswer::Cancelled
            } else {
                UserAnswer::Custom {
                    text: text.to_string(),
                }
            };
            let _ = tx.send(answer);
            info!(request_id, "已从渠道文本解析问题回复");
            return Ok(true);
        }

        Ok(false)
    }

    async fn run_agent_turn(
        &self,
        channel: Arc<dyn Channel>,
        state: OwnerState,
        message: InboundMessage,
    ) -> anyhow::Result<()> {
        let session_id = state
            .active_session_id
            .as_ref()
            .ok_or_else(|| anyhow!("缺少 active_session_id"))?;
        let provider_id = state
            .provider_id
            .as_ref()
            .ok_or_else(|| anyhow!("缺少 provider_id，请用 /new --provider 指定"))?;
        let model = state
            .model
            .as_ref()
            .ok_or_else(|| anyhow!("缺少 model，请用 /new --model 指定"))?;

        let prior = sessions::load_with_partial_recovery(&self.data_dir, session_id)?;
        let user_msg = sessions::Message {
            id: sessions::new_id(),
            role: sessions::Role::User,
            content: message.text.clone(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: Utc::now().timestamp_millis(),
            meta: None,
            subagent_call_id: None,
        };
        sessions::append_message(&self.data_dir, session_id, user_msg)?;

        let providers_file = model_gateway::config::load(&self.data_dir)?;
        let provider = providers_file
            .providers
            .iter()
            .find(|provider| &provider.id == provider_id)
            .ok_or_else(|| anyhow!("provider {provider_id} 不存在"))?
            .clone();
        let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(
            &self.data_dir,
            provider,
        )
        .await?;
        let provider_kind = provider.kind;
        let vision = agent_core::vision_bridge::build_vision_client(&self.data_dir).await?;
        let inner = model_gateway::build_client(provider)?;
        let inner = agent_core::vision_bridge::wrap_with_vision_client(inner, vision);
        let client: Arc<dyn ModelClient> = Arc::new(NamedModelClient::new(
            inner,
            model.clone(),
            prior.reasoning.clone(),
        ));

        let settings = settings_store::load(&self.data_dir);
        let workdir = prior
            .workdir
            .clone()
            .or_else(|| settings.conversation.workdir.clone())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let allowed_paths = prior
            .allowed_paths
            .clone()
            .unwrap_or_else(|| settings.conversation.allowed_paths.clone());
        let workspace = Workspace::with_runtime_state(
            workdir.clone(),
            allowed_paths,
            prior.runtime_allowed_paths.clone(),
            prior.pending_runtime_allowed_paths.clone(),
        );

        let skill_dirs = {
            let configured = prior
                .skill_dirs
                .clone()
                .unwrap_or_else(|| settings.conversation.skill_dirs.clone());
            if configured.is_empty() {
                default_skill_dirs(&self.data_dir, &workdir)
            } else {
                configured
                    .into_iter()
                    .map(|p| (agent_core::tools::skill::SkillSource::Global, p))
                    .collect()
            }
        };

        let phase = agent_core::wakeup::new_phase_channel();
        let shells = background::registry_for_session(session_id);
        agent_core::wakeup::WakeupScheduler::global()
            .register_session_shells(session_id.clone(), shells.clone());
        let hook_cfg = agent_core::hooks::load_hooks_config(&self.data_dir, Some(workspace.workdir()));
        let external_hooks = agent_core::hooks::ExternalHook::from_config(hook_cfg);
        let bg_log_dir = Some(sessions_dir::bg_dir(&self.data_dir, session_id));
        let read_state_tracker = Arc::new(ReadStateTracker::new());
        let edits_worktree = Arc::new(EditsWorktree::new(&self.data_dir, session_id, &workspace));

        let harness = Arc::new(Harness::new(
            agent_core::tools::default_tools_with_mcp(
                workspace.clone(),
                &skill_dirs,
                bg_log_dir,
                phase.clone(),
                shells,
                Some(self.data_dir.clone()),
                Some(session_id.clone()),
                Some(read_state_tracker),
                settings.general.shell.clone(),
                settings.general.edit_backend,
                agent_core::storage::mcp::load(&self.data_dir).with_cwd(workspace.workdir().to_path_buf()),
            )
            .await,
            HookManager::new(external_hooks),
        ));

        let permission_store = PermissionStore::open(&self.data_dir).ok().map(Arc::new);
        if let Some(store) = &permission_store {
            store.ensure_session_view(session_id);
        }
        let run_mode = prior.run_mode;
        let enabled_tools = {
            let tools = prior.enabled_tools.clone().unwrap_or_default();
            if tools.is_empty() {
                settings.conversation.enabled_tools.clone()
            } else {
                tools
            }
        };
        let global_rules = prior
            .global_rules
            .clone()
            .unwrap_or_else(|| settings.conversation.global_rules.clone());

        let mut core_session = CoreSession::new(
            harness,
            SessionConfig {
                definition: {
                    let mut definition = AgentDefinition::default();
                    let ctx_window = model_gateway::context_window::context_window_for(
                        provider_kind,
                        model,
                    );
                    definition.compaction_policy.token_budget = (ctx_window as f64 * 0.75) as usize;
                    definition
                },
                workspace: workspace.clone(),
                client,
                enabled_tools,
                initial_transcript: Transcript::from_session(prior.system_prompt.clone(), &prior.messages),
                recorder: None,
                model_io_dump: agent_core::model_io_dump::open_for_session_if_enabled(
                    &self.data_dir,
                    session_id,
                )
                .await,
                permission_store,
                session_id: Some(session_id.clone()),
                run_mode,
                model_id: Some(model.clone()),
                force_automode: false,
                data_dir: Some(self.data_dir.clone()),
                phase: Some(phase),
                global_rules,
                rules_files: prior.rules_files.clone(),
                edits_worktree: Some(edits_worktree),
            },
        );
        core_session.append_user(message.text.clone(), Vec::new());

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let pending_inputs: PendingInputs = Arc::new(Mutex::new(Vec::new()));
        let consumed_inputs = Arc::new(Mutex::new(Vec::new()));
        let mut handle = core_session.run_with_runtime_inputs(
            cancel_flag,
            Some(pending_inputs),
            Some(consumed_inputs.clone()),
            None,
        );
        let mut observer = ChannelObserver::new(
            self.pending.clone(),
            channel.clone(),
            message.from.clone(),
            message.channel_context.clone(),
        );
        let summary = handle.drive(&mut observer).await;

        consumed_inputs.lock().unwrap().clear();
        match summary.outcome {
            TurnOutcome::Done | TurnOutcome::Suspended => {
                observer.flush().await;
                if let Some(msg) = observer.build_message() {
                    sessions::append_message(&self.data_dir, session_id, msg)?;
                }
            }
            TurnOutcome::Cancelled => {
                observer.send("run 已取消").await;
            }
            TurnOutcome::Failed(err) => {
                observer.send(&format!("❌ {err}")).await;
            }
        }
        Ok(())
    }
}

struct ChannelObserver {
    pending: Arc<Mutex<PendingInteractions>>,
    channel: Arc<dyn Channel>,
    to: String,
    channel_context: serde_json::Value,
    buffer: String,
    full_text: String,
    parts: Vec<sessions::MessagePart>,
    tool_calls: Vec<sessions::MessageToolCall>,
    pending_tools: HashMap<String, (String, Value)>,
}

impl ChannelObserver {
    fn new(
        pending: Arc<Mutex<PendingInteractions>>,
        channel: Arc<dyn Channel>,
        to: String,
        channel_context: serde_json::Value,
    ) -> Self {
        Self {
            pending,
            channel,
            to,
            channel_context,
            buffer: String::new(),
            full_text: String::new(),
            parts: Vec::new(),
            tool_calls: Vec::new(),
            pending_tools: HashMap::new(),
        }
    }

    async fn send(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let _ = self
            .channel
            .send_text(&OutboundMessage {
                to: self.to.clone(),
                text: text.trim().to_string(),
                channel_context: self.channel_context.clone(),
            })
            .await;
    }

    async fn flush(&mut self) {
        if !self.buffer.trim().is_empty() {
            let chunk = std::mem::take(&mut self.buffer);
            self.send(&chunk).await;
        }
    }

    fn build_message(self) -> Option<sessions::Message> {
        if self.full_text.is_empty() && self.tool_calls.is_empty() {
            return None;
        }
        Some(sessions::Message {
            id: sessions::new_id(),
            role: sessions::Role::Assistant,
            content: self.full_text,
            attachments: Vec::new(),
            tool_calls: self.tool_calls,
            parts: self.parts,
            created_at: Utc::now().timestamp_millis(),
            meta: None,
            subagent_call_id: None,
        })
    }
}

#[async_trait]
impl TurnObserver for ChannelObserver {
    fn on_event(&mut self, event: &AgentEvent) {
        if event.subagent_call_id.is_some() {
            return;
        }
        match &event.payload {
            EventPayload::Reasoning { text } => {
                self.parts
                    .push(sessions::MessagePart::Reasoning { text: text.clone() });
            }
            EventPayload::TextDelta { text } => {
                self.buffer.push_str(text);
                for chunk in drain_ready_chunks(&mut self.buffer) {
                    let channel = self.channel.clone();
                    let to = self.to.clone();
                    let channel_context = self.channel_context.clone();
                    tokio::spawn(async move {
                        let _ = channel
                            .send_text(&OutboundMessage {
                                to,
                                text: chunk,
                                channel_context,
                            })
                            .await;
                    });
                }
            }
            EventPayload::TextDone { full_text } => {
                self.full_text = full_text.clone();
                self.parts
                    .retain(|p| !matches!(p, sessions::MessagePart::Text { .. }));
                self.parts.push(sessions::MessagePart::Text {
                    text: full_text.clone(),
                });
            }
            EventPayload::ToolCallStarted { call_id, name, input, .. } => {
                self.pending_tools
                    .insert(call_id.clone(), (name.clone(), input.clone()));
            }
            EventPayload::ToolCallFinished {
                call_id,
                result,
                duration_ms,
                ..
            } => {
                if let Some((name, input)) = self.pending_tools.remove(call_id) {
                    let tool_call = sessions::MessageToolCall {
                        id: call_id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                        result: Some(result.clone()),
                        duration_ms: Some(*duration_ms),
                    };
                    self.tool_calls.push(tool_call);
                    self.parts.push(sessions::MessagePart::ToolCall {
                        id: call_id.clone(),
                        name,
                        input,
                        arguments: String::new(),
                        result: Some(result.clone()),
                        duration_ms: Some(*duration_ms),
                    });
                }
            }
            _ => {}
        }
    }

    async fn on_permission_request(
        &mut self,
        request_id: &PermissionRequestId,
        _kind: &PermissionKind,
        summary: &str,
    ) -> Option<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .approvals
            .insert(request_id.as_str().to_string(), tx);
        self.send(&format!(
            "⚠️ 需要审批：{summary}\n回复 y/yes/允许 通过；回复 n/no/拒绝 拒绝；回复 deny <原因> 拒绝并反馈。"
        ))
        .await;
        rx.await.ok()
    }

    async fn on_question(
        &mut self,
        request_id: &PermissionRequestId,
        question: &str,
        options: &[QuestionOption],
        _multi: bool,
    ) -> Option<UserAnswer> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .questions
            .insert(request_id.as_str().to_string(), tx);
        let labels = options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        self.send(&format!("❓ {question}\n选项：{labels}\n也可以直接回复自定义文本。"))
            .await;
        rx.await.ok()
    }
}

fn drain_ready_chunks(buffer: &mut String) -> Vec<String> {
    let mut chunks = Vec::new();
    while let Some(split) = find_split_point(buffer) {
        let chunk = buffer.drain(..split).collect::<String>().trim().to_string();
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
    }
    chunks
}

fn find_split_point(buffer: &str) -> Option<usize> {
    if let Some(pos) = buffer.find("\n\n") {
        return Some(pos + 2);
    }
    if buffer.len() > 500 {
        let search = &buffer[..500];
        if let Some(pos) = search.rfind('\n') {
            return Some(pos + 1);
        }
        if let Some(pos) = search.rfind('。') {
            return Some(pos + '。'.len_utf8());
        }
        if let Some(pos) = search.rfind('.') {
            return Some(pos + 1);
        }
        return Some(500);
    }
    None
}

struct NamedModelClient {
    inner: DynModelClient,
    model: String,
    reasoning: Option<common::ReasoningConfig>,
}

impl NamedModelClient {
    fn new(
        inner: DynModelClient,
        model: String,
        reasoning: Option<common::ReasoningConfig>,
    ) -> Self {
        Self {
            inner,
            model,
            reasoning,
        }
    }

    fn patch(&self, mut request: ModelRequest) -> ModelRequest {
        request.model = self.model.clone();
        if request.reasoning.is_none() {
            request.reasoning = self.reasoning.clone();
        }
        request
    }
}

#[async_trait]
impl ModelClient for NamedModelClient {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn supports_streaming_tools(&self) -> bool {
        self.inner.supports_streaming_tools()
    }

    async fn complete(
        &self,
        request: ModelRequest,
        cancel: common::CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        self.inner.complete(self.patch(request), cancel).await
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: common::CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        self.inner.stream(self.patch(request), cancel, on_event).await
    }
}
