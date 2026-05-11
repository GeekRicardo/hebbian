//! Session：一次会话的运行时容器。
//!
//! 管理 transcript / workspace / definition / client / enabled_tools，
//! 提供 [`Session::run`] 直接起一次 run。Surface 不再需要自己组 [`RunParams`]。
//!
//! [`RunParams`]: crate::harness::RunParams

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use common::{attachments::MessageAttachment, CancelFlag};
use protocol::AgentRef;

use crate::{
    context::{
        budget,
        compaction::{compact_with_llm, CompactionResult},
        transcript::Transcript,
    },
    definition::AgentDefinition,
    harness::{Harness, RunHandle, RunParams},
    hooks::{HookManager, HookPoint},
    model_io_dump::ModelIoDump,
    permissions::PermissionStore,
    recorder::Recorder,
    run_mode::RunMode,
    system_prompt::{prepend_environment, EnvironmentSnapshot},
    tools::hitl::HitlGate,
    workspace::Workspace,
};
use model_gateway::client::ModelClient;
use model_gateway::types::{ModelError, ToolCall, TranscriptEntry};

/// 当前会话的上下文用量快照。surface 端用来渲染输入框旁的环形进度条。
#[derive(Debug, Clone, Copy)]
pub struct ContextUsage {
    pub used_tokens: usize,
    pub budget_tokens: usize,
}

impl ContextUsage {
    /// 0.0 ~ 1.0+ 的占用比例（>1 说明已超预算）。
    pub fn ratio(&self) -> f32 {
        if self.budget_tokens == 0 {
            0.0
        } else {
            self.used_tokens as f32 / self.budget_tokens as f32
        }
    }
}

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
    /// 可选模型 IO dump：每次 model 请求完整 request / response 写入 jsonl。
    /// 由环境变量 [`crate::model_io_dump::ENV_VAR`] 触发，由 surface 决定路径。
    pub model_io_dump: Option<ModelIoDump>,
    /// 持久化权限规则的共享 store。挂上后 HitlGate 在 AllowAndRemember 时直接写盘
    /// （架构 §4.6）。surface 可在启动时 [`PermissionStore::open`] 一次共享给所有
    /// session。
    pub permission_store: Option<Arc<PermissionStore>>,
    /// session_id（架构 §4.9.3 格式：`{yyyymmddHHmm}-{shortUuid}`）。用于
    /// PermissionStore 按 session 索引内存规则，以及未来 Recorder 定位
    /// `~/.hebbian/sessions/<id>/session.jsonl`。
    pub session_id: Option<String>,
    /// 运行模式（架构 §4.4.3）：默认 `AskBeforeEdits`。
    /// `AutoMode` 时派发器在 destructive 工具调用前调一次 LLM judge（限定 claude-opus-4-7）。
    pub run_mode: RunMode,
    /// 当前会话使用的模型 id（如 `"claude-opus-4-7"`）。AutoMode judge 用它做模型限定。
    /// `None` 时 AutoMode 自动降级为 Ask。
    pub model_id: Option<String>,
    /// 数据目录路径。给定后 microcompact 把被压缩的原始 tool result 落盘到
    /// `<data_dir>/sessions/<sid>/tool_results/<call_id>.txt`（架构 §4.7 / Step 9）。
    pub data_dir: Option<PathBuf>,
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
    model_io_dump: Option<ModelIoDump>,
    permission_store: Option<Arc<PermissionStore>>,
    session_id: Option<String>,
    run_mode: RunMode,
    model_id: Option<String>,
    data_dir: Option<PathBuf>,
    /// 来自 Harness 的共享 HookManager；Session 在 new / append_user / close
    /// 三个生命周期点 spawn 异步触发对应的外部 hook（架构 §4.8.1）。
    hooks: Arc<HookManager>,
}

impl Session {
    pub fn new(harness: Arc<Harness>, config: SessionConfig) -> Self {
        let hooks = harness.hooks();
        let session = Self {
            harness,
            client: config.client,
            transcript: config.initial_transcript,
            workspace: config.workspace,
            definition: config.definition,
            enabled_tools: config.enabled_tools,
            recorder: config.recorder,
            model_io_dump: config.model_io_dump,
            permission_store: config.permission_store,
            session_id: config.session_id,
            run_mode: config.run_mode,
            model_id: config.model_id,
            data_dir: config.data_dir,
            hooks,
        };
        // SessionStart hook（架构 §4.8.1）：fire-and-forget，hook 失败不影响主流程。
        if !session.hooks.is_empty() {
            if let Some(sid) = session.session_id.clone() {
                let workdir = session.workspace.workdir().display().to_string();
                let hooks = session.hooks.clone();
                tokio::spawn(async move {
                    let _ = hooks
                        .trigger(&HookPoint::SessionStart {
                            session_id: sid,
                            workdir,
                        })
                        .await;
                });
            }
        }
        session
    }

    /// 关闭会话：fire-and-forget 触发 `SessionEnd` 外部 hook（架构 §4.8.1）。
    /// surface 在退出 / 切换 session 前调一次；当前实现仅 emit hook，
    /// 不做其它清理（recorder / model_io_dump 由各自 Drop / flush 收尾）。
    pub async fn close(&self) {
        if self.hooks.is_empty() {
            return;
        }
        let Some(sid) = self.session_id.clone() else {
            return;
        };
        let _ = self
            .hooks
            .trigger(&HookPoint::SessionEnd { session_id: sid })
            .await;
    }

    /// 当前运行模式。
    pub fn run_mode(&self) -> RunMode {
        self.run_mode
    }

    /// 切换运行模式（架构 §10.2）。本期暂不 emit RunModeChanged 事件，留 Step 8 后续扩展。
    pub fn set_run_mode(&mut self, mode: RunMode) {
        self.run_mode = mode;
    }

    /// 当前会话使用的模型 id，AutoMode 判官限定模型时需要。
    pub fn model_id(&self) -> Option<&str> {
        self.model_id.as_deref()
    }

    pub fn client_arc(&self) -> Arc<dyn ModelClient> {
        self.client.clone()
    }

    pub fn recorder(&self) -> Option<&Recorder> {
        self.recorder.as_ref()
    }

    /// 追加一条 user 消息到 transcript。
    ///
    /// 头部按需注入两类块（不影响 system 段，prompt cache 不破）：
    /// - **首条 user message**：`<environment>` 快照（cwd / allowed_dirs / platform / date）。
    ///   transcript 里若已经有 user 消息（含恢复出来的历史）则跳过——只在真正全新的对话开头注入。
    /// - **任何 user message**：若 workspace 有 runtime_pending 的允许目录，drain 后包成
    ///   `<workspace-update>` 紧接 environment 之后注入。
    pub fn append_user(&mut self, text: String, attachments: Vec<MessageAttachment>) {
        let needs_environment = !self
            .transcript
            .entries
            .iter()
            .any(|e| matches!(e, TranscriptEntry::User(_)));
        let pending = self.workspace.take_pending_announcement();
        let mut final_text = prepend_workspace_update(text, &pending);
        if needs_environment {
            let snapshot = EnvironmentSnapshot::from_workspace(&self.workspace)
                .with_run_mode(self.run_mode);
            final_text = prepend_environment(final_text, &snapshot);
        }
        // UserPromptSubmit hook（架构 §4.8.1）：fire-and-forget，把最终 user text 发给外部 hook。
        // 当前实现不消费 hook 返回，完整 Modify patch 协议留增量。
        if !self.hooks.is_empty() {
            if let Some(sid) = self.session_id.clone() {
                let hooks = self.hooks.clone();
                let text_for_hook = final_text.clone();
                tokio::spawn(async move {
                    let _ = hooks
                        .trigger(&HookPoint::UserPromptSubmit {
                            session_id: sid,
                            text: text_for_hook,
                        })
                        .await;
                });
            }
        }
        self.transcript.push_user(final_text, attachments);
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

    /// 计算当前 transcript 的上下文占用，用 [`AgentDefinition::compaction_policy.token_budget`]
    /// 作为分母。surface 端用来渲染输入框旁的环形进度条。
    pub fn context_usage(&self) -> ContextUsage {
        let used = budget::estimate_transcript_tokens(
            self.transcript.system.as_deref(),
            &self.transcript.entries,
        );
        ContextUsage {
            used_tokens: used,
            budget_tokens: self.definition.compaction_policy.token_budget,
        }
    }

    /// 主动压缩：调一次模型把当前 transcript 浓缩成一份摘要，
    /// 用 `[前情概要 + assistant 确认]` 替换原 entries。
    /// 失败时不改动 transcript，原样返回错误。
    pub async fn compact(
        &mut self,
        custom_instructions: Option<&str>,
    ) -> Result<CompactionResult, ModelError> {
        let system = self.transcript.system.clone();
        let entries = self.transcript.entries.clone();
        let result =
            compact_with_llm(self.client.as_ref(), system.as_deref(), entries, custom_instructions)
                .await?;
        self.transcript.entries = result.entries.clone();
        Ok(result)
    }

    /// 用调用方提供的 cancel 启动 run（接入外部取消机制）。
    pub fn run_with(&self, cancel: CancelFlag) -> RunHandle {
        self.run_with_pending(cancel, None)
    }

    /// 与 [`Self::run_with`] 一致，但额外接入 surface 的运行时输入队列。
    /// surface 在 streaming 中往 `pending_inputs` 推 user message，agent_loop 在下一次
    /// model.request 之前 drain 出来加入 transcript（实现「立即发送」语义）。
    pub fn run_with_pending(
        &self,
        cancel: CancelFlag,
        pending_inputs: Option<common::runtime::PendingInputs>,
    ) -> RunHandle {
        let mut gate = HitlGate::new(self.definition.permission_policy.clone());
        if let (Some(store), Some(sid)) = (&self.permission_store, &self.session_id) {
            gate = gate.with_store(store.clone(), sid.clone());
        }
        let hitl = Arc::new(gate);
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
                model_io_dump: self.model_io_dump.clone(),
                pending_inputs,
                run_mode: self.run_mode,
                model_id: self.model_id.clone(),
                data_dir: self.data_dir.clone(),
                session_id: self.session_id.clone(),
            },
        )
    }
}

/// 把"对话开始后追加的允许目录"包成 `<workspace-update>` 前置到 user content。
/// `pending` 为空时原样返回 `text`，避免无谓改写消息内容。
fn prepend_workspace_update(text: String, pending: &[PathBuf]) -> String {
    if pending.is_empty() {
        return text;
    }
    let mut s = String::from("<workspace-update>\n");
    s.push_str("以下目录已被加入本次对话的允许访问范围（运行时追加）：\n");
    for p in pending {
        s.push_str(&format!("  - {}\n", p.display()));
    }
    s.push_str("</workspace-update>\n\n");
    s.push_str(&text);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepend_workspace_update_noop_when_empty() {
        let out = prepend_workspace_update("hi".into(), &[]);
        assert_eq!(out, "hi");
    }

    #[test]
    fn prepend_workspace_update_lists_each_path() {
        let pending = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let out = prepend_workspace_update("hi".into(), &pending);
        assert!(out.starts_with("<workspace-update>"));
        assert!(out.contains("- /a"));
        assert!(out.contains("- /b"));
        assert!(out.ends_with("hi"));
    }
}
