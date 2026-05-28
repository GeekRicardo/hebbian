//! KillShell 工具：主动终止后台 shell 进程。
//!
//! 标记为 `EffectClass::ReadOnly`——只对自己 spawn 的后台进程发 SIGKILL，
//! 不在 workspace 内做任何文件改动。审批价值不大，反而会打断 agent 自愈。

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};

use super::background::BgTaskRegistry;
use super::Tool;

pub struct KillShellTool {
    shells: BgTaskRegistry,
}

impl KillShellTool {
    pub fn new(shells: BgTaskRegistry) -> Self {
        Self { shells }
    }
}

#[async_trait]
impl Tool for KillShellTool {
    fn name(&self) -> &str {
        "KillShell"
    }

    fn description(&self) -> &str {
        "终止之前由 Bash 转入后台的进程（SIGKILL）。\
         按 task_id（如 bash_001）定位；进程已终态则直接返回当前状态。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Bash 工具返回的后台进程 ID。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let task_id = input["task_id"]
            .as_str()
            .ok_or_else(|| AppError::msg("KillShell: 缺少 task_id"))?;
        if task_id.starts_with("subagent-") {
            return Err(AppError::msg(
                "KillShell 只能终止 Bash 后台进程，不能终止后台 subagent 任务",
            ));
        }
        let state = self
            .shells
            .kill(task_id)
            .await
            .ok_or_else(|| AppError::msg(format!("KillShell: 未找到 task_id={task_id}")))?;
        Ok(format!("[task_id={task_id} status={}]", state.label()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::process::Command;

    #[tokio::test]
    async fn kill_running_process() {
        let shells = BgTaskRegistry::new();
        let child = Command::new("bash")
            .arg("-lc")
            .arg("sleep 30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let s = shells.register("sleep 30".into(), "/".into(), false, None, child);
        let tool = KillShellTool::new(shells);
        let out = tool.execute(json!({"task_id": s.task_id})).await.unwrap();
        assert!(out.contains("killed"));
    }

    #[tokio::test]
    async fn unknown_task_id_errors() {
        let tool = KillShellTool::new(BgTaskRegistry::new());
        let res = tool.execute(json!({"task_id": "bash_xx"})).await;
        assert!(res.is_err());
    }
}
