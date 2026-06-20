//! PlanMode：agent 自主进出计划模式的工具（架构 §4.4.5）。
//!
//! 一个工具三个 action：
//! - `enter`：从当前模式切到 PlanMode，开一份 plan 草稿。仅在**非 PlanMode** 暴露。
//! - `update`：把 `plan_markdown` 覆盖写进当前 plan，反复打磨。仅在 **PlanMode** 暴露。
//! - `submit`：定稿提交，走 HITL 审批；批准后切回进入前的模式。仅在 **PlanMode** 暴露。
//!
//! 真正的行为（落盘 / 切模式 / emit 事件 / 发起 HITL 审批 / 拼未消费 plan_comments）
//! 由 [`crate::dispatch::ToolDispatcher`] 的 short-circuit 分支处理——它持有
//! `data_dir + session_id + workspace + hitl + sink` 上下文，而 [`Tool`] trait 不带这些。
//!
//! 本文件只剩 Tool trait 实现（schema 给模型看）+ input 解析；`execute` 走兜底返回错误，
//! 正常路径不会被 dispatcher 调到。注入时 agent_loop 按当前模式定制 description/schema
//! （见 [`enter_description`] / [`active_description`]）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use common::{AppError, AppResult};

use crate::tools::Tool;

pub const PLAN_MODE_TOOL_NAME: &str = "PlanMode";

/// 三种 action。serde 用 snake_case 与 schema 枚举值对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    Enter,
    Update,
    Submit,
}

#[derive(Debug, Deserialize)]
pub struct PlanInput {
    pub action: PlanAction,
    /// `update` / `submit` 必填，`enter` 可空（草稿初始可为空骨架）。
    #[serde(default)]
    pub plan_markdown: String,
    /// 一句话摘要，UI 列表 / 审批弹窗标题用。可空。
    #[serde(default)]
    pub summary: String,
}

pub struct PlanModeTool;

/// 非 PlanMode 时给模型看的描述（只能 enter）。
pub fn enter_description() -> &'static str {
    "Enter Plan Mode to investigate read-only and draft an implementation plan before \
     making any changes. Call with `action: \"enter\"` when a task is non-trivial, \
     ambiguous, or risky and you want to research and propose a plan for the user to \
     approve first. After entering, editing files and running commands are disabled; \
     use `action: \"update\"` to write/refine the plan and `action: \"submit\"` to \
     submit it for approval."
}

/// PlanMode 时给模型看的描述（只能 update / submit）。
pub fn active_description() -> &'static str {
    "Manage the current plan while in Plan Mode. \
     `action: \"update\"` overwrites the working plan with `plan_markdown` so you can \
     refine it across turns (visible to the user live). \
     `action: \"submit\"` submits the plan for user approval; if `plan_markdown` is \
     given it is saved first. On approval, run mode switches back to the mode you were \
     in before entering Plan Mode and you may proceed with implementation. On rejection, \
     use the feedback to revise and submit again. \
     `plan_markdown` should be structured (Scope, Steps, Affected files, Risks); \
     `summary` is an optional one-line headline."
}

/// 非 PlanMode 注入用 schema：action 固定 enter，plan_markdown 可空。
pub fn enter_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["action"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["enter"],
                "description": "Enter Plan Mode."
            },
            "plan_markdown": {
                "type": "string",
                "description": "Optional initial plan skeleton in markdown."
            },
            "summary": {
                "type": "string",
                "description": "Optional one-line summary."
            }
        }
    })
}

/// PlanMode 注入用 schema：action update/submit，plan_markdown 必填。
pub fn active_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["action", "plan_markdown"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["update", "submit"],
                "description": "`update` to refine the plan, `submit` to submit for approval."
            },
            "plan_markdown": {
                "type": "string",
                "description": "Markdown plan. Include sections: Scope, Steps, Affected files, Risks."
            },
            "summary": {
                "type": "string",
                "description": "Optional one-line summary for the approval popup title."
            }
        }
    })
}

#[async_trait]
impl Tool for PlanModeTool {
    fn name(&self) -> &str {
        PLAN_MODE_TOOL_NAME
    }

    fn description(&self) -> &str {
        // registry 默认描述；实际注入时 agent_loop 按模式替换为 enter/active 版本。
        active_description()
    }

    fn parameters_schema(&self) -> Value {
        active_schema()
    }

    /// 兜底实现：当 dispatcher 没走 short-circuit（理论上不该发生）时返回错误。
    /// 真正的逻辑见 [`crate::dispatch::ToolDispatcher::spawn_plan_mode`]。
    async fn execute(&self, _input: Value) -> AppResult<String> {
        Err(AppError::msg(
            "PlanMode 必须由 dispatcher short-circuit 处理；走到 Tool::execute 说明 dispatch 路径有 bug",
        ))
    }
}

/// 解析模型传入的 input。供 dispatcher short-circuit 复用。
pub fn parse_input(input: Value) -> AppResult<PlanInput> {
    serde_json::from_value::<PlanInput>(input)
        .map_err(|e| AppError::msg(format!("invalid PlanMode input: {e}")))
}

/// 从 plan 文件绝对路径生成 plan_id（取 file stem，如 `plan-20260525143012`）。
pub fn plan_id_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "plan-unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_enter_allows_empty_markdown() {
        let p = parse_input(json!({"action": "enter"})).unwrap();
        assert_eq!(p.action, PlanAction::Enter);
        assert!(p.plan_markdown.is_empty());
    }

    #[test]
    fn parse_update_and_submit() {
        let u = parse_input(json!({"action": "update", "plan_markdown": "# x"})).unwrap();
        assert_eq!(u.action, PlanAction::Update);
        assert_eq!(u.plan_markdown, "# x");
        let s = parse_input(json!({"action": "submit", "plan_markdown": "# y", "summary": "z"}))
            .unwrap();
        assert_eq!(s.action, PlanAction::Submit);
        assert_eq!(s.summary, "z");
    }

    #[test]
    fn parse_rejects_unknown_action() {
        assert!(parse_input(json!({"action": "bogus"})).is_err());
    }
}
