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
    /// 运行模式（架构 §4.4.3）：默认 `Default`。
    /// `AutoMode` 时派发器在 destructive 工具调用前调一次 LLM judge（限定 claude-opus-4-7）。
    pub run_mode: RunMode,
    /// 当前会话使用的模型 id（如 `"claude-opus-4-7"`）。AutoMode judge 用它做模型限定。
    /// `None` 时 AutoMode 自动降级为 Ask。
    pub model_id: Option<String>,
    /// `force_automode` 子开关（架构 §4.4.4）。仅 [`RunMode::AutoMode`] 下生效：
    /// 判官返回 `Ask` 时折叠成 `Deny`，让"放手跑"模式不被打断。CLI 用
    /// `--force-automode` flag 启动 / REPL `/force-automode` 切换。
    pub force_automode: bool,
    /// 数据目录路径。给定后 microcompact 把被压缩的原始 tool result 落盘到
    /// `<data_dir>/sessions/<sid>/tool_results/<call_id>.txt`（架构 §4.7 / Step 9）。
    pub data_dir: Option<PathBuf>,
    /// 挂起请求通道（架构 §4.12.4）。`ScheduleWakeup` 写它，
    /// agent_loop 在 ToolStep 后读它决定是否进入 Suspended。必须与本 session 的
    /// `default_tools` 拿到的 phase channel 是同一份（否则模型挂起请求永远到不了
    /// agent_loop）。
    pub phase: Option<crate::wakeup::PhaseChannel>,
    /// 启用的全局规则文件路径列表。
    pub global_rules: Vec<PathBuf>,
    /// 项目规则文件开关状态。None = 自动发现（workdir 下的默认 on）。
    pub rules_files: Option<Vec<crate::rules::RuleFileState>>,
    /// Edit 工具快照仓库（架构 §4.13）。`None` 时跳过快照，不阻塞 Edit。
    pub edits_worktree: Option<Arc<crate::edits::EditsWorktree>>,
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
    force_automode: bool,
    data_dir: Option<PathBuf>,
    /// 挂起请求通道（架构 §4.12.4）。
    phase: Option<crate::wakeup::PhaseChannel>,
    /// 来自 Harness 的共享 HookManager；Session 在 new / append_user / close
    /// 三个生命周期点 spawn 异步触发对应的外部 hook（架构 §4.8.1）。
    hooks: Arc<HookManager>,
    global_rules: Vec<PathBuf>,
    rules_files: Option<Vec<crate::rules::RuleFileState>>,
    edits_worktree: Option<Arc<crate::edits::EditsWorktree>>,
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
            force_automode: config.force_automode,
            data_dir: config.data_dir,
            phase: config.phase,
            hooks,
            global_rules: config.global_rules,
            rules_files: config.rules_files,
            edits_worktree: config.edits_worktree,
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

    /// `force_automode` 子开关当前值（架构 §4.4.4）。
    pub fn force_automode(&self) -> bool {
        self.force_automode
    }

    /// 切换 `force_automode` 子开关。REPL `/force-automode` 命令通过 surface 调本方法。
    /// 与 RunMode 正交：仅 AutoMode 下生效，其它 mode 时被派发器忽略。
    pub fn set_force_automode(&mut self, on: bool) {
        self.force_automode = on;
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
    /// - **首条 user message**：`<environment>` 快照（cwd / allowed_paths / platform / date）。
    ///   transcript 里若已经有 user 消息（含恢复出来的历史）则跳过——只在真正全新的对话开头注入。
    /// - **任何 user message**：若 workspace 有 runtime_pending 的允许路径，drain 后包成
    ///   `<workspace-update>` 紧接 environment 之后注入。
    pub fn append_user(&mut self, text: String, attachments: Vec<MessageAttachment>) {
        let needs_environment = !self
            .transcript
            .entries
            .iter()
            .any(|e| matches!(e, TranscriptEntry::User(_)));
        let pending = self.workspace.take_pending_announcement();
        let mut final_text = prepend_workspace_update(text, &pending);

        // 架构 §4.12.7：本 session 当前 Running 状态的后台任务列表注入
        // `<background_tasks>` 块——每条 user message 都附带（不只是首条），
        // 因为 bg 任务列表随时间变化。
        let bg_summaries: Vec<crate::system_prompt::BackgroundTaskSummary> = self
            .session_id
            .as_deref()
            .map(|sid| {
                let shells = crate::tools::background::registry_for_session(sid);
                shells
                    .list()
                    .into_iter()
                    .filter(|s| {
                        // 双重过滤：is_background=true 排除前台命令的瞬时残留；
                        // ShellState::Running 排除已结束条目。两个一起才是"模型当下需要感知的活后台任务"。
                        s.is_background()
                            && matches!(s.state(), crate::tools::background::ShellState::Running)
                    })
                    .map(|s| crate::system_prompt::BackgroundTaskSummary {
                        task_id: s.task_id.clone(),
                        state: s.state().label().to_string(),
                        command: s.command.clone(),
                        elapsed_secs: s.started_at.elapsed().as_secs(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        if needs_environment {
            let extra_paths = self
                .permission_store
                .as_ref()
                .map(|s| s.effective_paths(Some(self.workspace.workdir())))
                .unwrap_or_default();

            // 架构 §4.14：首条 user message 注入记忆 L0 清单——global + 当前项目（若绑定）。
            // data_dir 缺失（CLI 单跑 / 单测）时跳过；scan 失败 warn 不阻塞主路径。
            let memory_index = match self.data_dir.as_deref() {
                Some(dd) => collect_memory_index(dd, self.workspace.workdir()),
                None => Vec::new(),
            };

            let snapshot = EnvironmentSnapshot::from_workspace(&self.workspace)
                .with_run_mode(self.run_mode)
                .with_background_tasks(bg_summaries.clone())
                .with_extra_paths(extra_paths)
                .with_memory_index(memory_index);
            final_text = prepend_environment(final_text, &snapshot);
        } else if !bg_summaries.is_empty() {
            // 非首条 user message：单独前置 `<background_tasks>` 块
            final_text = crate::system_prompt::prepend_background_tasks(final_text, &bg_summaries);
        }

        // 架构 §4.4.5：当前 active_plan 存在 unconsumed comments 时把它们包成
        // `<plan_comments>` 块前置，让 agent 在下一轮 ModelStep 看到用户对 plan
        // 的反馈，并把它们标记为 consumed（不会被注入第二次）。
        if let (Some(dd), Some(sid)) = (self.data_dir.as_deref(), self.session_id.as_deref()) {
            if let Ok(s) = crate::storage::sessions::load(dd, sid) {
                if let Some(plan_path) = s.active_plan.as_deref() {
                    let plan_id = std::path::Path::new(plan_path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    if !plan_id.is_empty() {
                        if let Ok(unconsumed) =
                            crate::storage::plan_comments::list_unconsumed(dd, sid, plan_id)
                        {
                            if !unconsumed.is_empty() {
                                final_text = crate::system_prompt::prepend_plan_comments(
                                    final_text,
                                    &unconsumed,
                                );
                                let ids: Vec<String> =
                                    unconsumed.iter().map(|c| c.id.clone()).collect();
                                if let Err(e) = crate::storage::plan_comments::mark_consumed(
                                    dd, sid, plan_id, ids,
                                ) {
                                    tracing::warn!(error = %e, "plan_comments::mark_consumed failed");
                                }
                            }
                        }
                    }
                }
            }
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
        let result = compact_with_llm(
            self.client.as_ref(),
            system.as_deref(),
            entries,
            custom_instructions,
        )
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
        self.run_with_runtime_inputs(cancel, pending_inputs, None, None)
    }

    fn resolve_system_rules(&self) -> Option<String> {
        let files = crate::rules::resolve_injection_files(
            &self.global_rules,
            self.rules_files.as_deref(),
            self.workspace.workdir(),
            self.workspace.initial_allowed_paths(),
        );
        let block = crate::rules::format_injection(&files);
        if block.is_empty() {
            None
        } else {
            Some(block)
        }
    }

    /// 构造一次 spawn_run 用的 [`crate::subagent::SubagentCtx`] 快照（架构 §4.4.11）。
    /// 没有可用 subagent（数据目录未设 / 列表为空）时返回 None——ToolDispatcher 拿到
    /// None 时 Task 工具走兜底错误，但实际上 default_tools 条件注入会让 Task 工具根本
    /// 没被注册，模型连选择项都看不到，所以"None + 模型硬调 Task"路径理论上不可达。
    fn build_subagent_ctx_snapshot(&self) -> Option<Arc<crate::subagent::SubagentCtx>> {
        let data_dir = self.data_dir.as_ref()?;
        let subagents: Vec<_> =
            crate::storage::subagents::load_for_workdir(data_dir, Some(self.workspace.workdir()))
                .into_iter()
                .filter(|d| d.enabled)
                .collect();
        if subagents.is_empty() {
            return None;
        }
        Some(Arc::new(crate::subagent::SubagentCtx {
            client: self.client.clone(),
            hooks: self.hooks.clone(),
            compaction_policy: self.definition.compaction_policy.clone(),
            data_dir: Some(data_dir.clone()),
            parent_session_id: self.session_id.clone(),
            stream: true,
            subagents: Arc::new(subagents),
        }))
    }

    /// Desktop surface 需要在 run 结束后把已消费的 PendingInputs 按正确顺序落盘。
    pub fn run_with_runtime_inputs(
        &self,
        cancel: CancelFlag,
        pending_inputs: Option<common::runtime::PendingInputs>,
        consumed_pending_inputs: Option<common::runtime::ConsumedPendingInputs>,
        pending_inputs_accepting: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> RunHandle {
        let mut gate = HitlGate::new(self.definition.permission_policy.clone());
        if let (Some(store), Some(sid)) = (&self.permission_store, &self.session_id) {
            gate = gate.with_store(
                store.clone(),
                sid.clone(),
                Some(self.workspace.workdir().to_path_buf()),
            );
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
                consumed_pending_inputs,
                pending_inputs_accepting,
                run_mode: self.run_mode,
                model_id: self.model_id.clone(),
                force_automode: self.force_automode,
                data_dir: self.data_dir.clone(),
                session_id: self.session_id.clone(),
                phase: self.phase.clone(),
                resume_from: None,
                edits_worktree: self.edits_worktree.clone(),
                max_tool_iterations: None,
                system_rules: self.resolve_system_rules(),
                subagent_ctx: self.build_subagent_ctx_snapshot(),
            },
        )
    }

    /// 从挂起态恢复 Run（架构 §4.12.6）：调用方先把 `<wakeup>` user message
    /// 追加到 transcript，再调本函数；agent_loop 入口会 emit `RunResumed { cause }`
    /// 并从 checkpoint 计数器起步。`phase` 参数 = 同一份挂起通道，让 resume 后
    /// 模型可以再次调 ScheduleWakeup 形成多次挂起。
    pub fn resume_with(
        &self,
        cancel: CancelFlag,
        pending_inputs: Option<common::runtime::PendingInputs>,
        phase: Option<crate::wakeup::PhaseChannel>,
        resume_from: crate::agent_loop::RunResumeState,
    ) -> RunHandle {
        self.resume_with_runtime_inputs(cancel, pending_inputs, None, None, phase, resume_from)
    }

    pub fn resume_with_runtime_inputs(
        &self,
        cancel: CancelFlag,
        pending_inputs: Option<common::runtime::PendingInputs>,
        consumed_pending_inputs: Option<common::runtime::ConsumedPendingInputs>,
        pending_inputs_accepting: Option<Arc<std::sync::atomic::AtomicBool>>,
        phase: Option<crate::wakeup::PhaseChannel>,
        resume_from: crate::agent_loop::RunResumeState,
    ) -> RunHandle {
        let mut gate = HitlGate::new(self.definition.permission_policy.clone());
        if let (Some(store), Some(sid)) = (&self.permission_store, &self.session_id) {
            gate = gate.with_store(
                store.clone(),
                sid.clone(),
                Some(self.workspace.workdir().to_path_buf()),
            );
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
                consumed_pending_inputs,
                pending_inputs_accepting,
                run_mode: self.run_mode,
                model_id: self.model_id.clone(),
                force_automode: self.force_automode,
                data_dir: self.data_dir.clone(),
                session_id: self.session_id.clone(),
                phase,
                resume_from: Some(resume_from),
                edits_worktree: self.edits_worktree.clone(),
                max_tool_iterations: None,
                system_rules: self.resolve_system_rules(),
                subagent_ctx: self.build_subagent_ctx_snapshot(),
            },
        )
    }
}

/// 把"对话开始后追加的允许路径"包成 `<workspace-update>` 前置到 user content。
/// `pending` 为空时原样返回 `text`，避免无谓改写消息内容。
fn prepend_workspace_update(text: String, pending: &[PathBuf]) -> String {
    if pending.is_empty() {
        return text;
    }
    let mut s = String::from("<workspace-update>\n");
    s.push_str("以下目录已被加入本次对话的可访问范围（运行时追加）：\n");
    for p in pending {
        s.push_str(&format!("  - {}\n", p.display()));
    }
    s.push_str("</workspace-update>\n\n");
    s.push_str(&text);
    s
}

/// 拼出首条 user message 的 `<memory-index>` 注入清单：global 在前，当前项目（若是
/// 真实项目目录而非 home / 根）在后。任何一边读失败仅 warn 不阻塞主路径——记忆系统
/// 是增强、不能拖垮对话。
///
/// 记忆系统关闭（`MemorySettings::active()` 为 false）时返回空——与后台抽取共用同一
/// 门控，保证「关闭后既不注入也不抽取」（架构 §4.14.6）。
fn collect_memory_index(
    data_dir: &std::path::Path,
    workdir: &std::path::Path,
) -> Vec<crate::storage::memory::MemoryL0> {
    use crate::storage::memory::{list_l0, mem_log, mem_warn, MemoryScope};

    if !crate::storage::settings::load(data_dir).memory.active() {
        return Vec::new();
    }

    let mut out = match list_l0(data_dir, None, MemoryScope::Global) {
        Ok(v) => v,
        Err(e) => {
            mem_warn!("Query", "列出全局记忆失败，跳过 memory-index 全局段：{e}");
            Vec::new()
        }
    };
    if let Some(project_wd) = crate::tools::memory_project_workdir(workdir) {
        match list_l0(data_dir, Some(&project_wd), MemoryScope::Project) {
            Ok(mut v) => out.append(&mut v),
            Err(e) => {
                mem_warn!("Query", "列出项目记忆失败，跳过 memory-index 项目段：{e}");
            }
        }
    }
    mem_log!("Inject", "memory-index：{} 条", out.len());
    out
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

    /// 架构 §4.14.6：记忆系统关闭时既不注入也不抽取。这里盯住注入侧——
    /// 即使磁盘上有记忆，`memory.active()` 为 false 时 `collect_memory_index`
    /// 必须返回空，与后台抽取门控对称。A/B：默认 settings（enabled=false）应空，
    /// 显式开启 + 配模型后应注入到那条记忆。
    #[test]
    fn collect_memory_index_gated_by_active() {
        use crate::storage::memory::{write, MemoryScope};
        use crate::storage::settings::{self, MemoryModelRef};

        let dd =
            std::env::temp_dir().join(format!("heb-sess-mem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dd).unwrap();
        write(&dd, None, MemoryScope::Global, "k", "c", "记得这件事", "正文").unwrap();

        // 关闭（默认 enabled=false）→ 不注入。
        let workdir = std::path::Path::new("/");
        assert!(
            collect_memory_index(&dd, workdir).is_empty(),
            "记忆关闭时不应注入 memory-index"
        );

        // 开启 + 配 fallback 模型 → 注入磁盘上的记忆。
        let mut s = settings::load(&dd);
        s.memory.enabled = true;
        s.memory.models = vec![MemoryModelRef {
            provider_id: "p".into(),
            model: "m".into(),
        }];
        settings::save(&dd, &s).unwrap();
        assert_eq!(
            collect_memory_index(&dd, workdir).len(),
            1,
            "记忆开启时应注入磁盘上的记忆"
        );
    }
}
