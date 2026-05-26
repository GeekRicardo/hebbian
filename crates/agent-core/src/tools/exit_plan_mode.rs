//! ExitPlanMode：PlanMode 下唯一退出途径（架构 §4.4.5）。
//!
//! agent 在 PlanMode 调研完成后调用此工具，把最终 plan markdown 作为输入提交。
//! 真正的行为（落盘 / emit `PlanReady` / 发起 HITL 审批 / 切回 pre_plan_mode /
//! 拼未消费的 plan_comments）由 [`crate::dispatch::ToolDispatcher`] 的
//! short-circuit 分支处理——它持有 `data_dir + session_id + hitl + sink` 上下文，
//! 而 [`Tool`] trait 不带这些。
//!
//! 本文件只剩 Tool trait 实现（schema 给模型看），`execute` 走兜底返回汇总字符串，
//! 实际不会被 dispatcher 调到。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use common::{AppError, AppResult};

use crate::tools::Tool;

pub const EXIT_PLAN_MODE_TOOL_NAME: &str = "ExitPlanMode";

#[derive(Debug, Deserialize)]
pub struct ExitPlanInput {
    pub plan_markdown: String,
    /// 一句话摘要，UI 列表 / 通知用。可空。
    #[serde(default)]
    pub summary: String,
}

pub struct ExitPlanModeTool;

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        EXIT_PLAN_MODE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Exit PlanMode by submitting the final plan as markdown for user approval. \
         Call this only after read-only investigation is complete and you have a \
         concrete plan. The user will be shown the plan and asked to approve, edit, \
         or reject (with feedback). On approval, run mode switches back to the \
         pre-PlanMode mode and you may proceed with implementation. On rejection, \
         use the feedback to revise and call ExitPlanMode again. \
         Required field `plan_markdown` should be structured (Scope, Steps, \
         Affected files, Risks). Optional `summary` is a one-line headline."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["plan_markdown"],
            "properties": {
                "plan_markdown": {
                    "type": "string",
                    "description": "Markdown formatted plan. Include sections: Scope, Steps, Affected files, Risks."
                },
                "summary": {
                    "type": "string",
                    "description": "Optional one-line summary for the approval popup title."
                }
            }
        })
    }

    /// 兜底实现：当 dispatcher 没走 short-circuit（理论上不该发生）时返回错误。
    /// 真正的逻辑见 [`crate::dispatch::ToolDispatcher::spawn_exit_plan_mode`]。
    async fn execute(&self, _input: Value) -> AppResult<String> {
        Err(AppError::msg(
            "ExitPlanMode 必须由 dispatcher short-circuit 处理；走到 Tool::execute 说明 dispatch 路径有 bug",
        ))
    }
}

/// 解析模型传入的 input。供 dispatcher short-circuit 复用。
pub fn parse_input(input: Value) -> AppResult<ExitPlanInput> {
    serde_json::from_value::<ExitPlanInput>(input)
        .map_err(|e| AppError::msg(format!("invalid ExitPlanMode input: {e}")))
}

/// 从 plan 文件绝对路径生成 plan_id（取 file stem，如 `plan-20260525143012`）。
pub fn plan_id_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "plan-unknown".to_string())
}
