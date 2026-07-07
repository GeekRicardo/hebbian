//! 渠道消息与 hebbian agent_core 的桥接。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use crate::commands::{self, CommandResult};
use crate::contract::Channel;
use crate::message::{InboundMessage, OutboundMessage};
use crate::owner_state::OwnerState;
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

/// 把机主在渠道里的回复落地为 HITL 决定。
///
/// 渠道转发的审批/问题可能来自两类发起方：① 渠道自己跑的 agent run（本地 oneshot）；
/// ② 其他 surface（如 Desktop 主对话）在机主离开电脑时转发过来的请求。后者的落地点
/// 在 surface 侧（如 Desktop 的 `HitlState`），channel-core 不依赖 surface，故用 trait 注入。
pub trait RemoteHitlResolver: Send + Sync {
    fn resolve_approval(&self, request_id: &str, decision: ApprovalDecision);
    fn answer_question(&self, request_id: &str, answer: UserAnswer);
}

/// 审批待办的落地出口。
enum ApprovalSink {
    /// 渠道自身 run 的 HITL：直接唤醒 await 中的 observer。
    Local(oneshot::Sender<ApprovalDecision>),
    /// 外部 surface 转发来的审批：回落给注入的 resolver。
    Remote(Arc<dyn RemoteHitlResolver>),
}

/// 问题待办：携带选项上下文，机主回数字时映射为对应 label。
struct PendingQuestion {
    options: Vec<QuestionOption>,
    multi: bool,
    sink: QuestionSink,
}

enum QuestionSink {
    Local(oneshot::Sender<UserAnswer>),
    Remote(Arc<dyn RemoteHitlResolver>),
}

#[derive(Clone)]
struct OwnerTarget {
    to: String,
    channel_context: Value,
}

struct PendingInteractions {
    approvals: HashMap<String, ApprovalSink>,
    questions: HashMap<String, PendingQuestion>,
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
    /// 当前 agent run 的取消标志，供渠道 `/cancel` 命令打断。
    current_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    /// run_loop 启动后持有的渠道句柄，供外部转发审批/问题时发消息。
    channel: Arc<Mutex<Option<Arc<dyn Channel>>>>,
    /// 最近一次收到机主消息的回复目标（to + context_token）。
    /// iLink 协议要求机主先发过消息才有 context_token，故转发前必有此值。
    last_owner_target: Arc<Mutex<Option<OwnerTarget>>>,
}

impl ChannelBridge {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            pending: Arc::new(Mutex::new(PendingInteractions::new())),
            active_run: Arc::new(AtomicBool::new(false)),
            current_cancel: Arc::new(Mutex::new(None)),
            channel: Arc::new(Mutex::new(None)),
            last_owner_target: Arc::new(Mutex::new(None)),
        }
    }

    /// 外部 surface 转发一条审批到机主渠道。已组装好的 `text` 直接发出，
    /// 机主回 y/n/deny 经 poll loop 解析后回落给 `resolver`。
    /// 渠道未就绪或机主从未发过消息（无回复目标）时返回 false，调用方应回退本地通知。
    pub fn forward_approval(
        &self,
        request_id: &str,
        text: &str,
        resolver: Arc<dyn RemoteHitlResolver>,
    ) -> bool {
        let Some((channel, target)) = self.forward_target() else {
            return false;
        };
        self.pending
            .lock()
            .unwrap()
            .approvals
            .insert(request_id.to_string(), ApprovalSink::Remote(resolver));
        self.spawn_send(channel, target, text.to_string());
        true
    }

    /// 外部 surface 转发一条提问到机主渠道。`options` 用于把机主回的数字映射回 label。
    pub fn forward_question(
        &self,
        request_id: &str,
        text: &str,
        options: Vec<QuestionOption>,
        multi: bool,
        resolver: Arc<dyn RemoteHitlResolver>,
    ) -> bool {
        let Some((channel, target)) = self.forward_target() else {
            return false;
        };
        self.pending.lock().unwrap().questions.insert(
            request_id.to_string(),
            PendingQuestion {
                options,
                multi,
                sink: QuestionSink::Remote(resolver),
            },
        );
        self.spawn_send(channel, target, text.to_string());
        true
    }

    /// 撤销一条尚未被机主回复的转发待办（如审批已在 Desktop 端处理）。
    pub fn cancel_forwarded(&self, request_id: &str) {
        let mut pending = self.pending.lock().unwrap();
        pending.approvals.remove(request_id);
        pending.questions.remove(request_id);
    }

    /// 渠道是否就绪可转发（已登录运行 + 机主发过消息有回复目标）。
    /// 调用方据此决定是否落「已转发」痕迹——避免渠道离线却落假痕迹。
    pub fn can_forward(&self) -> bool {
        self.channel.lock().unwrap().is_some() && self.last_owner_target.lock().unwrap().is_some()
    }

    /// 主动向指定用户发送消息（不依赖 `last_owner_target`）。
    /// 渠道未就绪时返回 false。
    ///
    /// `channel_context` 为空时，由具体 Channel 实现自行兜底
    /// （如 WeChatChannel 会从 context_store 查之前缓存过的 context_token）。
    pub fn send_to_user(&self, to: &str, text: &str) -> bool {
        let channel = match self.channel.lock().unwrap().clone() {
            Some(c) => c,
            None => return false,
        };
        let to = to.to_string();
        let text = text.to_string();
        tokio::spawn(async move {
            let _ = channel
                .send_text(&OutboundMessage {
                    to,
                    text,
                    channel_context: Value::Null,
                })
                .await;
        });
        true
    }

    fn forward_target(&self) -> Option<(Arc<dyn Channel>, OwnerTarget)> {
        let channel = self.channel.lock().unwrap().clone()?;
        let target = self.last_owner_target.lock().unwrap().clone()?;
        Some((channel, target))
    }

    fn spawn_send(&self, channel: Arc<dyn Channel>, target: OwnerTarget, text: String) {
        tokio::spawn(async move {
            let _ = channel
                .send_text(&OutboundMessage {
                    to: target.to,
                    text,
                    channel_context: target.channel_context,
                })
                .await;
        });
    }

    pub async fn run_loop(
        &self,
        channel: Arc<dyn Channel>,
        state: &mut OwnerState,
        account_id: &str,
    ) -> anyhow::Result<()> {
        info!(channel = channel.id(), account_id, "渠道网关启动");
        *self.channel.lock().unwrap() = Some(channel.clone());
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
                if let Err(err) = self
                    .handle_message(channel.clone(), state, account_id, message)
                    .await
                {
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
        // 记录机主回复目标：转发外部审批/问题时据此发到机主微信。
        *self.last_owner_target.lock().unwrap() = Some(OwnerTarget {
            to: message.from.clone(),
            channel_context: message.channel_context.clone(),
        });

        if self.try_resolve_pending(&message).await? {
            return Ok(());
        }

        // /cancel 是运行时控制命令，需打断当前 run 的 cancel_flag，不走纯查询的 commands::dispatch。
        if message.text.trim().eq_ignore_ascii_case("/cancel") {
            let cancelled = self
                .current_cancel
                .lock()
                .unwrap()
                .as_ref()
                .map(|flag| {
                    flag.store(true, Ordering::SeqCst);
                    true
                })
                .unwrap_or(false);
            let reply = if cancelled {
                "🛑 已发送停止信号，当前对话即将中断。"
            } else {
                "当前没有正在运行的对话。"
            };
            channel
                .send_text(&OutboundMessage {
                    to: message.from,
                    text: reply.into(),
                    channel_context: message.channel_context,
                })
                .await?;
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

        // 取出一条待办（审批优先）。锁内只取 sink，发送/落地在锁外做。
        enum Resolved {
            Approval(String, ApprovalSink),
            Question(String, PendingQuestion),
        }
        let resolved = {
            let mut pending = self.pending.lock().unwrap();
            if let Some(request_id) = pending.approvals.keys().next().cloned() {
                let sink = pending.approvals.remove(&request_id).unwrap();
                Some(Resolved::Approval(request_id, sink))
            } else if let Some(request_id) = pending.questions.keys().next().cloned() {
                let question = pending.questions.remove(&request_id).unwrap();
                Some(Resolved::Question(request_id, question))
            } else {
                None
            }
        };

        match resolved {
            Some(Resolved::Approval(request_id, sink)) => {
                let decision = parse_approval(text, &lower);
                match sink {
                    ApprovalSink::Local(tx) => {
                        let _ = tx.send(decision);
                    }
                    ApprovalSink::Remote(resolver) => {
                        resolver.resolve_approval(&request_id, decision);
                    }
                }
                info!(request_id, "已从渠道文本解析审批回复");
                Ok(true)
            }
            Some(Resolved::Question(request_id, question)) => {
                let answer = parse_answer(text, &lower, &question.options, question.multi);
                match question.sink {
                    QuestionSink::Local(tx) => {
                        let _ = tx.send(answer);
                    }
                    QuestionSink::Remote(resolver) => {
                        resolver.answer_question(&request_id, answer);
                    }
                }
                info!(request_id, "已从渠道文本解析问题回复");
                Ok(true)
            }
            None => Ok(false),
        }
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
            run_duration_ms: None,
        };
        sessions::append_message(&self.data_dir, session_id, user_msg)?;

        let providers_file = model_gateway::config::load(&self.data_dir)?;
        let provider = providers_file
            .providers
            .iter()
            .find(|provider| &provider.id == provider_id)
            .ok_or_else(|| anyhow!("provider {provider_id} 不存在"))?
            .clone();
        let provider =
            model_gateway::auth::refresh::ensure_fresh_provider_token(&self.data_dir, provider)
                .await?;
        let ctx_window =
            model_gateway::context_window::effective_context_window_for(&provider, model);
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
        let hook_cfg =
            agent_core::hooks::load_hooks_config(&self.data_dir, Some(workspace.workdir()));
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
                agent_core::storage::mcp::load(&self.data_dir)
                    .with_cwd(workspace.workdir().to_path_buf()),
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
                    definition.compaction_policy.token_budget = (ctx_window as f64 * 0.75) as usize;
                    definition
                },
                workspace: workspace.clone(),
                client,
                enabled_tools,
                initial_transcript: Transcript::from_session(
                    prior.system_prompt.clone(),
                    &prior.messages,
                ),
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
                // surface 主对话：tag=Main（前端不额外标记，§4.11）。
                call_tag: model_gateway::types::ModelCallTag::Main,
                data_dir: Some(self.data_dir.clone()),
                phase: Some(phase),
                global_rules,
                rules_files: prior.rules_files.clone(),
                edits_worktree: Some(edits_worktree),
                derived_sink: None,
            },
        );
        core_session.append_user(message.text.clone(), Vec::new());

        let cancel_flag = Arc::new(AtomicBool::new(false));
        *self.current_cancel.lock().unwrap() = Some(cancel_flag.clone());
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
        *self.current_cancel.lock().unwrap() = None;
        match summary.outcome {
            TurnOutcome::Done | TurnOutcome::Suspended => {
                observer.flush().await;
                if let Some(mut msg) = observer.build_message() {
                    msg.run_duration_ms = summary.duration_ms;
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
            run_duration_ms: None,
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
                self.parts.push(sessions::MessagePart::Reasoning {
                    text: text.clone(),
                    duration_ms: None,
                });
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
            EventPayload::ToolCallStarted {
                call_id,
                name,
                input,
                ..
            } => {
                self.pending_tools
                    .insert(call_id.clone(), (name.clone(), input.clone()));
            }
            EventPayload::ToolCallFinished {
                call_id,
                result,
                duration_ms,
                is_error,
                ..
            } => {
                if let Some((name, input)) = self.pending_tools.remove(call_id) {
                    let tool_call = sessions::MessageToolCall {
                        id: call_id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                        result: Some(result.clone()),
                        duration_ms: Some(*duration_ms),
                        is_error: *is_error,
                        nested: Vec::new(),
                    };
                    self.tool_calls.push(tool_call);
                    self.parts.push(sessions::MessagePart::ToolCall {
                        id: call_id.clone(),
                        name,
                        input,
                        arguments: String::new(),
                        result: Some(result.clone()),
                        duration_ms: Some(*duration_ms),
                        is_error: *is_error,
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
            .insert(request_id.as_str().to_string(), ApprovalSink::Local(tx));
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
        multi: bool,
        questions: &[protocol::AskQuestion],
    ) -> Option<UserAnswer> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().questions.insert(
            request_id.as_str().to_string(),
            PendingQuestion {
                options: options.to_vec(),
                multi,
                sink: QuestionSink::Local(tx),
            },
        );
        if !questions.is_empty() {
            let body = questions
                .iter()
                .map(|q| {
                    let labels = q
                        .options
                        .iter()
                        .map(|option| option.label.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ");
                    format!("- {}：{}", q.title, labels)
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.send(&format!(
                "❓ 请回答下面几个问题：\n{body}\n也可以直接回复自定义文本。"
            ))
            .await;
        } else {
            let labels = options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            self.send(&format!(
                "❓ {question}\n选项：{labels}\n也可以直接回复自定义文本。"
            ))
            .await;
        }
        rx.await.ok()
    }
}

/// 把机主在渠道里的回复解析为审批决定。
///
/// `y/yes/允许/通过/1` → 允许一次；`deny <原因>` / `拒绝 <原因>` → 拒绝并反馈；其余 → 拒绝。
fn parse_approval(text: &str, lower: &str) -> ApprovalDecision {
    match lower {
        "y" | "yes" | "允许" | "通过" | "1" => ApprovalDecision::AllowOnce,
        other if other.starts_with("deny ") || other.starts_with("拒绝 ") => {
            ApprovalDecision::DenyWithFeedback {
                feedback: text
                    .split_once(char::is_whitespace)
                    .map(|(_, rest)| rest.trim().to_string())
                    .filter(|rest| !rest.is_empty())
                    .unwrap_or_else(|| "用户拒绝".to_string()),
            }
        }
        _ => ApprovalDecision::Deny,
    }
}

/// 把机主在渠道里的回复解析为问题答案。
///
/// 选项解析（需求「ask 的选项走解析」）：机主回数字时映射为对应选项 label——
/// 单选回 `2`（1-based）取第 2 个选项；多选回 `1,3` 或 `1 3` 取多个。回 `取消/cancel`
/// 取消整轮；其余文本作为自由输入 [`UserAnswer::Custom`]。
fn parse_answer(text: &str, lower: &str, options: &[QuestionOption], multi: bool) -> UserAnswer {
    if lower == "cancel" || lower == "取消" {
        return UserAnswer::Cancelled;
    }

    if !options.is_empty() {
        let indices: Vec<usize> = text
            .split(|c: char| c == ',' || c == '，' || c.is_whitespace())
            .filter(|token| !token.is_empty())
            .filter_map(|token| token.parse::<usize>().ok())
            .filter(|n| *n >= 1 && *n <= options.len())
            .map(|n| n - 1)
            .collect();

        if !indices.is_empty() {
            if multi {
                let mut labels = Vec::new();
                for index in indices {
                    let label = options[index].label.clone();
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
                return UserAnswer::SelectedMulti { labels };
            }
            return UserAnswer::Selected {
                label: options[indices[0]].label.clone(),
            };
        }

        // 机主直接回了选项原文（而非编号）也认。
        if let Some(option) = options.iter().find(|option| option.label == text.trim()) {
            return UserAnswer::Selected {
                label: option.label.clone(),
            };
        }
    }

    UserAnswer::Custom {
        text: text.to_string(),
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
        self.inner
            .stream(self.patch(request), cancel, on_event)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(labels: &[&str]) -> Vec<QuestionOption> {
        labels
            .iter()
            .map(|label| QuestionOption {
                label: label.to_string(),
                description: String::new(),
            })
            .collect()
    }

    fn answer(text: &str, options: &[QuestionOption], multi: bool) -> UserAnswer {
        parse_answer(text, &text.to_ascii_lowercase(), options, multi)
    }

    #[test]
    fn single_choice_by_number_maps_to_label() {
        let options = opts(&["系统空闲", "窗口隐藏", "锁屏"]);
        assert!(matches!(
            answer("2", &options, false),
            UserAnswer::Selected { label } if label == "窗口隐藏"
        ));
    }

    #[test]
    fn multi_choice_by_numbers_dedup_and_order() {
        let options = opts(&["切换对话", "删除对话", "压缩"]);
        match answer("1，3 1", &options, true) {
            UserAnswer::SelectedMulti { labels } => {
                assert_eq!(labels, vec!["切换对话".to_string(), "压缩".to_string()]);
            }
            other => panic!("期望 SelectedMulti，得到 {other:?}"),
        }
    }

    #[test]
    fn out_of_range_number_falls_back_to_custom() {
        let options = opts(&["A", "B"]);
        assert!(matches!(
            answer("9", &options, false),
            UserAnswer::Custom { text } if text == "9"
        ));
    }

    #[test]
    fn label_text_matches_selected() {
        let options = opts(&["锁屏", "息屏"]);
        assert!(matches!(
            answer("息屏", &options, false),
            UserAnswer::Selected { label } if label == "息屏"
        ));
    }

    #[test]
    fn cancel_keyword_cancels() {
        let options = opts(&["A", "B"]);
        assert!(matches!(
            answer("取消", &options, false),
            UserAnswer::Cancelled
        ));
    }

    #[test]
    fn approval_keywords() {
        assert!(matches!(
            parse_approval("1", "1"),
            ApprovalDecision::AllowOnce
        ));
        assert!(matches!(
            parse_approval("允许", "允许"),
            ApprovalDecision::AllowOnce
        ));
        assert!(matches!(
            parse_approval("deny 太危险", "deny 太危险"),
            ApprovalDecision::DenyWithFeedback { feedback } if feedback == "太危险"
        ));
        assert!(matches!(parse_approval("n", "n"), ApprovalDecision::Deny));
    }
}
