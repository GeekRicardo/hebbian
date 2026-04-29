//! Session：一次会话的运行时容器。
//!
//! 管理 transcript / workspace / definition / client / enabled_tools，
//! 提供 [`Session::run`] 直接起一次 run。Surface 不再需要自己组 [`RunParams`]。
//!
//! [`RunParams`]: crate::harness::RunParams

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use platform::{attachments::MessageAttachment, CancelFlag};
use protocol::AgentRef;

use crate::{
    context::transcript::Transcript,
    definition::AgentDefinition,
    harness::{Harness, RunHandle, RunParams},
    recorder::Recorder,
    tools::hitl::HitlGate,
    workspace::Workspace,
};
use model_gateway::client::ModelClient;
use model_gateway::types::ToolCall;

/// 创建 [`Session`] 所需的配置。
pub struct SessionConfig {
    pub definition: AgentDefinition,
    pub workspace: Arc<Workspace>,
    pub client: Arc<dyn ModelClient>,
    pub enabled_tools: Vec<String>,
    /// 起始 transcript。新会话传 `Transcript::new(system_prompt)`；
    /// 加载历史会话传 `Transcript::from_session(...)`。
    pub initial_transcript: Transcript,
    /// 可选事件落盘。给定后每次 run 的事件流自动追加进 jsonl。
    pub recorder: Option<Recorder>,
}

/// 一次会话。持有 transcript、workspace、agent definition、provider client、可选 recorder。
pub struct Session {
    harness: Arc<Harness>,
    client: Arc<dyn ModelClient>,
    transcript: Transcript,
    workspace: Arc<Workspace>,
    definition: AgentDefinition,
    enabled_tools: Vec<String>,
    recorder: Option<Recorder>,
}

impl Session {
    pub fn new(harness: Arc<Harness>, config: SessionConfig) -> Self {
        Self {
            harness,
            client: config.client,
            transcript: config.initial_transcript,
            workspace: config.workspace,
            definition: config.definition,
            enabled_tools: config.enabled_tools,
            recorder: config.recorder,
        }
    }

    pub fn recorder(&self) -> Option<&Recorder> {
        self.recorder.as_ref()
    }

    pub fn append_user(&mut self, text: String, attachments: Vec<MessageAttachment>) {
        self.transcript.push_user(text, attachments);
    }

    /// run 结束 Done 后调用，把最终 assistant 文本与 tool_calls 落入 transcript。
    pub fn commit_assistant(&mut self, text: String, tool_calls: Vec<ToolCall>) {
        self.transcript.push_assistant(text, tool_calls);
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn transcript_mut(&mut self) -> &mut Transcript {
        &mut self.transcript
    }

    pub fn definition(&self) -> &AgentDefinition {
        &self.definition
    }

    pub fn workspace(&self) -> &Arc<Workspace> {
        &self.workspace
    }

    pub fn enabled_tools(&self) -> &[String] {
        &self.enabled_tools
    }

    pub fn set_enabled_tools(&mut self, tools: Vec<String>) {
        self.enabled_tools = tools;
    }

    /// 用默认参数启动 run（fresh hitl + 内部新建 cancel flag）。
    pub fn run(&self) -> RunHandle {
        self.run_with(Arc::new(AtomicBool::new(false)))
    }

    /// 用调用方提供的 cancel 启动 run（接入外部取消机制）。
    pub fn run_with(&self, cancel: CancelFlag) -> RunHandle {
        let hitl = Arc::new(HitlGate::new(self.definition.permission_policy.clone()));
        self.harness.spawn_run(
            self.client.clone(),
            RunParams {
                agent: AgentRef::new(&self.definition.id),
                hitl,
                transcript: self.transcript.clone(),
                enabled_tools: self.enabled_tools.clone(),
                compaction_policy: self.definition.compaction_policy.clone(),
                workspace: self.workspace.clone(),
                stream: true,
                cancel,
                parent: None,
                recorder: self.recorder.clone(),
            },
        )
    }
}
