//! [`SubagentRunner`]：跑一次嵌套 agent_loop（NestedRun，架构 §4.4.11）。
//!
//! 当前范围（P3.1a）：`isolated` / `inherit` 模式 + 前台同步（父等子完成）+ 子 session
//! 落盘到 `sessions/<parent_sid>/subagents/<child_sid>/`。`run_in_background=true` 留到 P4。

use std::sync::Arc;

use common::CancelFlag;
use model_gateway::types::{ModelError, TranscriptEntry};
use protocol::{AgentRef, Event, RunId};

use crate::agent_loop::{self, EventSink, LoopParams};
use crate::context::transcript::Transcript;
use crate::run_state::RunState;
use crate::storage::subagents::{SubagentDefinition, DEFAULT_MAX_ITERATIONS};
use crate::tools::task::{TaskInput, TaskMode};
use crate::tools::{registry::ToolRegistry, Tool};
use crate::workspace::Workspace;

use super::ctx::SubagentCtx;

/// 跑一次 NestedRun。
pub struct SubagentRunner<'a> {
    pub ctx: &'a SubagentCtx,
    /// 父 ToolRegistry——按 subagent.tools 过滤后给子用。
    pub parent_registry: Arc<ToolRegistry>,
    pub parent_sink: EventSink,
    pub parent_workspace: Arc<Workspace>,
    pub parent_hitl: Arc<crate::tools::hitl::HitlGate>,
    pub parent_cancel: CancelFlag,
    pub parent_edits_worktree: Option<Arc<crate::edits::EditsWorktree>>,
    /// 父 Run id（子事件装饰器把 event.run_id 重写为此值，让 surface 按父 Run 路由）。
    pub parent_run_id: RunId,
    /// 父 model id；subagent 定义里没指定 `model` 时跟父。
    pub parent_model_id: Option<String>,
    /// 父 Task 工具调用的 call_id——所有子事件经装饰器填上此字段后转发到父 sink。
    pub parent_task_call_id: String,
    /// 父 Transcript 在「触发 turn 之前」的 entries 快照（架构 §4.4.11.3）。
    /// `inherit` 模式深拷贝其内容作为子 transcript 起点；`isolated` 模式忽略。
    /// `None` 表示当前会话未启用 subagent 或本轮无 Task 调用（理论上 spawn_task 不会走到，
    /// 但 inherit 模式落到 None 时降级为 isolated 起手，避免硬错把整组 parallel 拖崩）。
    pub parent_transcript_snapshot: Option<Arc<Vec<TranscriptEntry>>>,
}

impl<'a> SubagentRunner<'a> {
    /// 执行一次 Task 调用。返回子的终态文本（最后一条 assistant 输出）。
    pub async fn execute(&self, input: TaskInput) -> Result<String, ModelError> {
        // 1. 找到 subagent 定义
        let def = match self.ctx.find(&input.subagent_type) {
            Some(d) => d.clone(),
            None => {
                let available: Vec<&str> =
                    self.ctx.subagents.iter().map(|d| d.name.as_str()).collect();
                return Err(ModelError::Other(format!(
                    "未找到 subagent `{}`（可用：{}）",
                    input.subagent_type,
                    if available.is_empty() {
                        "<空>".to_string()
                    } else {
                        available.join(", ")
                    }
                )));
            }
        };

        // 2. 模式分流（P2 仅支持 isolated；其它分支留 TODO 错误防止静默走错）
        if input.run_in_background {
            return Err(ModelError::Other(
                "Task.run_in_background=true 尚未实施（P4 phase）".to_string(),
            ));
        }
        let initial_transcript = match input.mode {
            TaskMode::Isolated => build_isolated_transcript(&def, &input.prompt),
            TaskMode::Inherit => build_inherit_transcript(
                &def,
                &input.prompt,
                self.parent_transcript_snapshot
                    .as_deref()
                    .map(Vec::as_slice),
            ),
        };

        // 3. 构造子 ToolRegistry（按 subagent.tools 白名单过滤 + 永远剔除 Task 自身）
        let child_registry = self.build_child_registry(&def);

        // 4. 子 RunState（独立 RunId / 独立 seq）+ EventSink 装饰器
        let child_run_id = RunId::new();
        let child_state = Arc::new(RunState::new(child_run_id.clone()));
        let child_sink = self.wrap_sink_with_decorator();

        // 4.1 子 session id 与落盘目录（架构 §4.4.11.2）
        let child_session_id = prepare_child_session(
            self.ctx.parent_session_id.as_deref(),
            self.ctx.data_dir.as_deref(),
        );

        // 5. 跑 agent_loop
        let mut transcript = initial_transcript;
        let agent = AgentRef::new(format!("subagent:{}", def.name));
        let enabled_tools: Vec<String> = def
            .tools
            .clone()
            .unwrap_or_else(|| child_registry.tool_names());
        let compaction_policy = self.ctx.compaction_policy.clone();
        let model_id = def.model.clone().or_else(|| self.parent_model_id.clone());
        let max_iter = def.max_iterations.unwrap_or(DEFAULT_MAX_ITERATIONS);

        let params = LoopParams {
            client: self.ctx.client.as_ref(),
            registry: Arc::new(child_registry),
            hitl: self.parent_hitl.clone(),
            hooks: self.ctx.hooks.clone(),
            transcript: &mut transcript,
            enabled_tools: &enabled_tools,
            compaction_policy: &compaction_policy,
            workspace: self.parent_workspace.clone(),
            stream: self.ctx.stream,
            cancel: self.parent_cancel.clone(),
            state: child_state,
            agent,
            parent: Some(self.parent_run_id.clone()),
            model_io_dump: None,
            pending_inputs: None,
            consumed_pending_inputs: None,
            pending_inputs_accepting: None,
            // 子默认 EditAutomatically，避免再弹 RunMode 选择（架构 §4.4.11 决策）
            run_mode: crate::run_mode::RunMode::EditAutomatically,
            model_id,
            judge_client: Some(self.ctx.client.clone()),
            force_automode: false,
            data_dir: self.ctx.data_dir.clone(),
            session_id: child_session_id,
            phase: None,
            resume_from: None,
            edits_worktree: self.parent_edits_worktree.clone(),
            max_tool_iterations: Some(max_iter),
            system_rules: None,
            // 子不允许再调 Task（多层嵌套已被 child_registry 剔除 Task 工具阻断），
            // 但保险起见 subagent_ctx 也传 None——即便模型硬调 Task 也会拿到"不可用"错误。
            subagent_ctx: None,
        };

        let output = agent_loop::run_loop(params, child_sink).await?;
        Ok(output.text)
    }

    fn build_child_registry(&self, def: &SubagentDefinition) -> ToolRegistry {
        let allowed: Option<&[String]> = def.tools.as_deref();
        let mut child_tools: Vec<Arc<dyn Tool>> = Vec::new();
        for arc_tool in self.parent_registry.iter() {
            let name = arc_tool.name();
            if name == crate::tools::task::TASK_TOOL_NAME {
                continue; // 永远剔除 Task 防止多层嵌套
            }
            if let Some(allow) = allowed {
                if !allow.iter().any(|n| n == name) {
                    continue;
                }
            }
            child_tools.push(arc_tool.clone());
        }
        ToolRegistry::from_arcs(child_tools)
    }

    /// 包一层装饰器：子 sink 接收 child event → 重写 run_id 为父 run_id +
    /// 填 subagent_call_id = parent_task_call_id → 调父 sink。
    fn wrap_sink_with_decorator(&self) -> EventSink {
        let parent_sink = self.parent_sink.clone();
        let parent_run_id = self.parent_run_id.clone();
        let parent_task_call_id = self.parent_task_call_id.clone();
        Arc::new(move |event: Event| {
            let rewritten = Event {
                run_id: parent_run_id.clone(),
                seq: event.seq,
                at_ms: event.at_ms,
                subagent_call_id: Some(parent_task_call_id.clone()),
                payload: event.payload,
            };
            parent_sink(rewritten);
        })
    }
}

/// 计算并落盘子 session 的目录骨架（架构 §4.4.11.2）。
///
/// 返回 `Some(child_session_id)` 形如 `<parent_sid>/subagents/<child_sid>`，
/// session_dir 内部 `data_dir.join("sessions").join(id)` 会按 `/` 自然展开为嵌套目录；
/// list_sessions 只扫一级目录，天然忽略子 session，不污染会话列表。
///
/// 父 session_id 或 data_dir 缺失（CLI 单跑 / 单测）时返回 `None`，子不落盘。
/// 目录创建失败时降级为 `None` 让子 run 仍能跑——只是这次子不持久化。
fn prepare_child_session(
    parent_session_id: Option<&str>,
    data_dir: Option<&std::path::Path>,
) -> Option<String> {
    let (parent_sid, data_dir) = parent_session_id.zip(data_dir)?;
    let child_sid = crate::storage::sessions_dir::new_session_id();
    let composed = format!("{parent_sid}/subagents/{child_sid}");
    match crate::storage::sessions_dir::ensure_session_dirs(data_dir, &composed) {
        Ok(()) => Some(composed),
        Err(e) => {
            tracing::warn!(
                error = %e,
                composed = %composed,
                "ensure_session_dirs for subagent failed; child run will not persist"
            );
            None
        }
    }
}

/// isolated 模式（架构 §4.4.11.2）：system 段用 subagent 自定义 prompt，
/// 不组装父默认 6 段；transcript 只含一条 user(prompt)。
fn build_isolated_transcript(def: &SubagentDefinition, prompt: &str) -> Transcript {
    let mut transcript = Transcript::new(Some(def.system_prompt.clone()));
    transcript.push_user(prompt.to_string(), Vec::new());
    transcript
}

/// inherit 模式（架构 §4.4.11.3）：
/// - system = 子 subagent 自定义 prompt（**不**继承父 system；父子角色不同，套父
///   system 会串改子的人格定位）。"继承"语义只针对 transcript 历史（讨论上下文）。
/// - entries = 父 transcript 快照的深拷贝（截止「触发 turn」之前，不含触发本次 Task
///   的 assistant tool_call——避免子 transcript 出现无对应 ToolResult 的 self-reference
///   而让 provider body 转换失败）。
/// - 末尾追加 user(prompt)，让子知道当下要做的具体任务。
///
/// `snapshot=None` 时降级为 isolated 形态（仅 system + prompt）。实际 agent_loop 在
/// calls 含 Task 时一定会抓快照——None 仅作为防御性兜底。
fn build_inherit_transcript(
    def: &SubagentDefinition,
    prompt: &str,
    snapshot: Option<&[TranscriptEntry]>,
) -> Transcript {
    let mut transcript = Transcript::new(Some(def.system_prompt.clone()));
    if let Some(entries) = snapshot {
        transcript.entries = entries.to_vec();
    }
    transcript.push_user(prompt.to_string(), Vec::new());
    transcript
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_gateway::types::{AssistantEntry, ToolCall, ToolResult, UserEntry};
    use serde_json::json;

    fn def(name: &str) -> SubagentDefinition {
        SubagentDefinition {
            name: name.to_string(),
            description: "test".to_string(),
            tools: None,
            model: None,
            max_iterations: None,
            system_prompt: format!("You are {name}."),
            enabled: true,
        }
    }

    #[test]
    fn isolated_transcript_has_only_system_and_one_user() {
        let t = build_isolated_transcript(&def("reviewer"), "review the diff");
        assert_eq!(t.system.as_deref(), Some("You are reviewer."));
        assert_eq!(t.entries.len(), 1);
        match &t.entries[0] {
            TranscriptEntry::User(u) => assert_eq!(u.text, "review the diff"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn inherit_transcript_keeps_parent_entries_then_appends_user_prompt() {
        let parent_snapshot: Vec<TranscriptEntry> = vec![
            TranscriptEntry::User(UserEntry {
                text: "build me X".to_string(),
                attachments: Vec::new(),
            }),
            TranscriptEntry::Assistant(AssistantEntry {
                text: "plan: do A then B".to_string(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall {
                    id: "tc-1".to_string(),
                    name: "Read".to_string(),
                    input: json!({"file_path": "src/main.rs"}),
                }],
            }),
            TranscriptEntry::ToolResults(vec![ToolResult {
                call_id: "tc-1".to_string(),
                name: "Read".to_string(),
                content: "...file body...".to_string(),
                artifact: None,
            }]),
        ];

        let t = build_inherit_transcript(
            &def("doc-writer"),
            "now write the tests",
            Some(&parent_snapshot),
        );

        // system 是子自己的，不继承父
        assert_eq!(t.system.as_deref(), Some("You are doc-writer."));
        // 3 条历史 + 1 条新 user prompt
        assert_eq!(t.entries.len(), 4);
        match &t.entries[3] {
            TranscriptEntry::User(u) => assert_eq!(u.text, "now write the tests"),
            other => panic!("last entry should be user prompt, got {other:?}"),
        }
        // 父历史完整保留
        match &t.entries[1] {
            TranscriptEntry::Assistant(a) => {
                assert_eq!(a.text, "plan: do A then B");
                assert_eq!(a.tool_calls.len(), 1);
                assert_eq!(a.tool_calls[0].id, "tc-1");
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn inherit_with_none_snapshot_degrades_to_isolated_shape() {
        let t = build_inherit_transcript(&def("x"), "do thing", None);
        assert_eq!(t.system.as_deref(), Some("You are x."));
        assert_eq!(t.entries.len(), 1);
        match &t.entries[0] {
            TranscriptEntry::User(u) => assert_eq!(u.text, "do thing"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn prepare_child_session_creates_expected_nested_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let parent_sid = "202605281234-aaaaaaaa";

        let composed =
            prepare_child_session(Some(parent_sid), Some(data_dir)).expect("Some(child id)");
        // 形如 `<parent>/subagents/<child>`
        assert!(composed.starts_with(&format!("{parent_sid}/subagents/")));
        let child_sid = composed.rsplit('/').next().unwrap();
        assert!(!child_sid.is_empty());

        let child_root = data_dir
            .join("sessions")
            .join(parent_sid)
            .join("subagents")
            .join(child_sid);
        assert!(child_root.is_dir(), "child session root should exist");
        for sub in ["tool_results", "compactions", "plans", "partial", "bg"] {
            assert!(
                child_root.join(sub).is_dir(),
                "child {sub} subdir should exist"
            );
        }
    }

    #[test]
    fn prepare_child_session_returns_none_when_inputs_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(prepare_child_session(None, Some(tmp.path())).is_none());
        assert!(prepare_child_session(Some("parent"), None).is_none());
        assert!(prepare_child_session(None, None).is_none());
    }

    #[test]
    fn inherit_deep_copies_snapshot_no_aliasing() {
        let mut parent_snapshot = vec![TranscriptEntry::User(UserEntry {
            text: "original".to_string(),
            attachments: Vec::new(),
        })];
        let t = build_inherit_transcript(&def("x"), "next", Some(&parent_snapshot));

        // 修改父快照不应影响子 transcript（深拷贝语义）
        if let TranscriptEntry::User(u) = &mut parent_snapshot[0] {
            u.text = "mutated".to_string();
        }
        match &t.entries[0] {
            TranscriptEntry::User(u) => assert_eq!(u.text, "original"),
            other => panic!("expected User, got {other:?}"),
        }
    }
}
