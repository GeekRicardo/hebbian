//! Task：subagent 调度工具（架构 §4.4.11）。
//!
//! 模型调 `Task(subagent_type, prompt, mode, run_in_background)` 委托一个子任务给
//! 自定义 system prompt + 受限工具子集的 subagent，跑一次嵌套 agent_loop（NestedRun）。
//!
//! 真正的 NestedRun 执行体由 [`crate::dispatch::ToolDispatcher`] 的 short-circuit 分支
//! 处理（持有 client / hitl / workspace / sink 等父 run 上下文），Tool trait 本身只
//! 描述 schema。P1 阶段 dispatcher short-circuit 尚未接入，工具实际不会被模型选中（条件
//! 注入：[`default_tools`] 仅在加载到至少一个启用的 subagent 定义时才注入 Task），
//! 所以 `execute` 走兜底返回 P1 占位错误是合理的：dispatcher 接好之前，工具就不该被调到。

use std::sync::Arc;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::storage::subagents::SubagentDefinition;
use crate::tools::Tool;

pub const TASK_TOOL_NAME: &str = "Task";

/// `mode` 参数（架构 §4.4.11.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    /// 子 Session 起手 transcript 仅一条 user message（prompt）。
    Isolated,
    /// 子 Session 起手 transcript = 父当前 messages 的深拷贝 + 追加 prompt。
    Inherit,
}

impl Default for TaskMode {
    fn default() -> Self {
        TaskMode::Isolated
    }
}

#[derive(Debug, Deserialize)]
pub struct TaskInput {
    /// 哪个 subagent（与 `~/.hebbian/subagents/<name>.md` 文件名一致）。
    pub subagent_type: String,
    /// 任务描述。isolated 模式下子只看到这一条；inherit 模式下追加在父 transcript 副本之后。
    pub prompt: String,
    #[serde(default)]
    pub mode: TaskMode,
    /// UI 卡片标题用的短标签。可空。
    #[serde(default)]
    pub description: Option<String>,
    /// 后台模式（架构 §4.4.11.7）。true 时立即返回 task_id，NestedRun 在 BgTaskRegistry
    /// 后台跑，完成时通过 WakeupScheduler 通知父模型。
    #[serde(default)]
    pub run_in_background: bool,
}

pub struct TaskTool {
    /// 当前会话可用的 subagent 列表（已应用启用合并，仅含 enabled=true 的项）。
    /// description 拼接里平铺出 subagent_type + description 给模型选用。
    subagents: Arc<Vec<SubagentDefinition>>,
    description: String,
}

impl TaskTool {
    pub fn new(subagents: Vec<SubagentDefinition>) -> Self {
        let description = render_description(&subagents);
        Self {
            subagents: Arc::new(subagents),
            description,
        }
    }

    /// 拿引用给 dispatcher short-circuit 查找定义。
    pub fn subagents(&self) -> &[SubagentDefinition] {
        &self.subagents
    }
}

fn render_description(subagents: &[SubagentDefinition]) -> String {
    let mut s = String::from(
        "Dispatch a sub-task to a specialized subagent that has its own system prompt and a restricted tool subset. \
         Use this when you want to delegate a self-contained piece of work (code review, focused research, writing tests, etc.).\n\n\
         `mode` choices:\n\
         - `isolated` (default): the subagent only sees the `prompt` argument; the parent conversation is hidden. \
         Use for self-contained tasks where context is captured fully in `prompt`.\n\
         - `inherit`: the subagent starts from a snapshot of the parent conversation plus the `prompt`. \
         Use when the sub-task continues the current discussion (e.g. \"write tests for the implementation we just designed\").\n\n\
         `run_in_background=true` returns immediately with a task_id; the system will send a BgTaskFinished \
         wakeup notification when it completes. Use this when you want to do other work in parallel, \
         or finish the current turn and wait for the notification.\n\n",
    );
    if subagents.is_empty() {
        s.push_str("No subagents are currently available; this tool should not be invoked.");
    } else {
        s.push_str("Available subagents (use as `subagent_type`):\n");
        for def in subagents {
            s.push_str(&format!("- `{}`: {}\n", def.name, def.description));
        }
    }
    s
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        TASK_TOOL_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["subagent_type", "prompt"],
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "description": "Which subagent to invoke (see the list in this tool's description)."
                },
                "prompt": {
                    "type": "string",
                    "description": "The task description sent to the subagent."
                },
                "mode": {
                    "type": "string",
                    "enum": ["isolated", "inherit"],
                    "default": "isolated",
                    "description": "isolated = subagent only sees `prompt`. inherit = subagent inherits a snapshot of the parent conversation plus `prompt`."
                },
                "description": {
                    "type": "string",
                    "description": "Short label of the sub-task for UI display."
                },
                "run_in_background": {
                    "type": "boolean",
                    "default": false,
                    "description": "If true, return immediately with a task_id; the subagent runs in the background and the system sends a BgTaskFinished wakeup when it completes."
                }
            }
        })
    }

    /// 兜底实现：dispatcher short-circuit 接入前不应被调到（default_tools 条件注入兜底）。
    /// 走到这里说明 dispatch 路径未把 Task 路由到 SubagentRunner，是 bug。
    async fn execute(&self, _input: Value) -> AppResult<String> {
        Err(AppError::msg(
            "Task 必须由 dispatcher short-circuit 路由到 SubagentRunner；走到 Tool::execute 说明 dispatch 路径未接入 NestedRun",
        ))
    }
}

/// 解析模型传入的 input。供 dispatcher short-circuit 复用。
pub fn parse_input(input: Value) -> AppResult<TaskInput> {
    serde_json::from_value::<TaskInput>(input)
        .map_err(|e| AppError::msg(format!("invalid Task input: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_def(name: &str, desc: &str) -> SubagentDefinition {
        SubagentDefinition {
            name: name.to_string(),
            description: desc.to_string(),
            tools: None,
            model: None,
            max_iterations: None,
            system_prompt: format!("You are {name}."),
            enabled: true,
            source: crate::storage::subagents::SubagentSource::Global,
            permission: None,
        }
    }

    #[test]
    fn description_lists_available_subagents() {
        let tool = TaskTool::new(vec![
            make_def("code-reviewer", "Reviews code for bugs."),
            make_def("doc-researcher", "Searches docs."),
        ]);
        let d = tool.description();
        assert!(d.contains("`code-reviewer`: Reviews code for bugs."));
        assert!(d.contains("`doc-researcher`: Searches docs."));
        assert!(d.contains("isolated"));
        assert!(d.contains("inherit"));
        assert!(d.contains("run_in_background"));
    }

    #[test]
    fn description_with_empty_subagents_says_unavailable() {
        let tool = TaskTool::new(Vec::new());
        let d = tool.description();
        assert!(d.contains("No subagents are currently available"));
    }

    #[test]
    fn schema_requires_subagent_type_and_prompt() {
        let tool = TaskTool::new(Vec::new());
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(req.contains(&"subagent_type"));
        assert!(req.contains(&"prompt"));
    }

    #[test]
    fn parse_input_defaults_mode_to_isolated() {
        let raw = json!({
            "subagent_type": "code-reviewer",
            "prompt": "review the diff"
        });
        let input = parse_input(raw).unwrap();
        assert_eq!(input.subagent_type, "code-reviewer");
        assert_eq!(input.mode, TaskMode::Isolated);
        assert!(!input.run_in_background);
    }

    #[test]
    fn parse_input_accepts_inherit_mode_and_background_flag() {
        let raw = json!({
            "subagent_type": "doc",
            "prompt": "continue what we discussed",
            "mode": "inherit",
            "run_in_background": true
        });
        let input = parse_input(raw).unwrap();
        assert_eq!(input.mode, TaskMode::Inherit);
        assert!(input.run_in_background);
    }

    #[tokio::test]
    async fn execute_returns_error_until_dispatcher_short_circuit_is_wired() {
        let tool = TaskTool::new(vec![make_def("x", "x")]);
        let res = tool
            .execute(json!({"subagent_type":"x","prompt":"y"}))
            .await;
        assert!(res.is_err());
        let msg = res.unwrap_err().to_string();
        assert!(msg.contains("dispatcher short-circuit"));
    }
}
