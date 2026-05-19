//! ExitPlanMode：PlanMode 下唯一退出途径（架构 §4.4.5）。
//!
//! agent 在 PlanMode 调研完成后调用此工具，将 plan markdown 作为输入提交。
//! 本期：
//! - 工具结果返回 plan markdown + 退模式提示
//! - 落盘到 `<data_dir>/sessions/<sid>/plans/plan-<ts>.md`，路径通过
//!   `HEBBIAN_CURRENT_DATA_DIR` / `HEBBIAN_CURRENT_SESSION_ID` 环境变量
//!   传入（hack）。Step 4 CoreClient 重构时改为构造时注入。
//! - `PlanReady` 事件 emit 留增量（需 dispatcher 接入事件 sink）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use common::AppResult;

use crate::storage::plans;
use crate::tools::Tool;

pub const ENV_DATA_DIR: &str = "HEBBIAN_CURRENT_DATA_DIR";
pub const ENV_SESSION_ID: &str = "HEBBIAN_CURRENT_SESSION_ID";

#[derive(Debug, Deserialize)]
struct ExitPlanInput {
    plan_markdown: String,
}

pub struct ExitPlanModeTool;

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "ExitPlanMode"
    }

    fn description(&self) -> &str {
        "Exit PlanMode by recording the final plan as markdown. \
         Call this only after you have completed read-only investigation \
         and have a concrete plan ready for user approval. Input field \
         `plan_markdown` should contain a structured plan: scope, steps, \
         affected files, and risks."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "plan_markdown": {
                    "type": "string",
                    "description": "Markdown formatted plan. Should include sections: 目标/Scope, 步骤/Steps, 影响文件/Affected files, 风险/Risks."
                }
            },
            "required": ["plan_markdown"]
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let parsed: ExitPlanInput = serde_json::from_value(input).map_err(|e| {
            common::error::AppError::msg(format!("invalid ExitPlanMode input: {e}"))
        })?;

        // 落盘 plan markdown：data_dir + session_id 都给定时写入；否则保持
        // 旧行为只返回提示。env var 由 CLI / Desktop 在创建 Session 时设置。
        let plan_path = match (
            std::env::var(ENV_DATA_DIR).ok(),
            std::env::var(ENV_SESSION_ID).ok(),
        ) {
            (Some(dd), Some(sid)) if !dd.is_empty() && !sid.is_empty() => {
                match plans::save_plan(std::path::Path::new(&dd), &sid, &parsed.plan_markdown) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::warn!(error = %e, "ExitPlanMode: 写 plan 文件失败");
                        None
                    }
                }
            }
            _ => None,
        };

        let mut out = String::from("[Plan recorded — awaiting user mode switch]\n\n");
        out.push_str(&parsed.plan_markdown);
        if let Some(p) = plan_path {
            out.push_str(&format!("\n\nPlan saved at: {}", p.display()));
        }
        Ok(out)
    }
}
