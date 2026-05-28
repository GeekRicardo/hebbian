//! BashOutput 工具：按 task_id 增量取后台 shell 的输出。
//!
//! - 只读，不需要审批（`EffectClass::ReadOnly`）。
//! - 每次返回 *自上次以来* 未读的输出（按字节游标推进）。已结束的进程会附带
//!   退出码 / 状态。
//! - 可传 `wait_ms` 让工具阻塞最多 N 毫秒等新输出，避免空轮询。

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};

use super::background::{BgTaskRegistry, ShellState, READ_CHUNK_BYTES};
use super::Tool;

const MAX_WAIT_MS: u64 = 30_000;

pub struct BashOutputTool {
    shells: BgTaskRegistry,
}

impl BashOutputTool {
    pub fn new(shells: BgTaskRegistry) -> Self {
        Self { shells }
    }
}

#[async_trait]
impl Tool for BashOutputTool {
    fn name(&self) -> &str {
        "BashOutput"
    }

    fn description(&self) -> &str {
        "查询 Bash 后台进程的最新输出（按 task_id）。\
         自上次查询以来的增量输出 + 当前状态（running / exited / killed）。\
         可传 wait_ms 阻塞等待新输出，避免空轮询。\
         不带任何 task_id 调用会列出当前所有后台 shell。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Bash 工具返回的 task_id（形如 bash_001）。不传则列出所有后台 shell。"
                },
                "wait_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_WAIT_MS,
                    "description": "阻塞等待新输出 / 状态变化的毫秒数，最大 30000。默认 0（不等待）。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let Some(task_id) = input["task_id"].as_str() else {
            return Ok(render_listing(&self.shells.list()));
        };
        if task_id.starts_with("subagent-") {
            return Err(AppError::msg(
                "BashOutput 只能查询 Bash 后台进程，后台 subagent 任务完成后会自动发送通知",
            ));
        }
        let shell = self
            .shells
            .get(task_id)
            .ok_or_else(|| AppError::msg(format!("BashOutput: 未找到 task_id={task_id}")))?;

        let wait_ms = input["wait_ms"].as_u64().unwrap_or(0).min(MAX_WAIT_MS);
        if wait_ms > 0 && !shell.state().is_terminal() {
            shell.wait_for_change(wait_ms).await;
        }

        let snapshot = shell.read_incremental(READ_CHUNK_BYTES);
        // 状态标签：running / exited 都合并到一行 task_id 头里，
        // 模型同时管多个后台任务时一眼能区分这是哪个 task 的输出 + 当前态。
        let status = match &snapshot.state {
            ShellState::Running => "running".to_string(),
            ShellState::Exited { code: Some(c) } => format!("exit {c}"),
            ShellState::Exited { code: None } => "terminated".to_string(),
            ShellState::Killed => "killed".to_string(),
            ShellState::Failed { error } => format!("failed: {error}"),
        };
        let mut text = format!("[{} {}]\n", shell.task_id, status);
        if snapshot.bytes_dropped > 0 {
            text.push_str(&format!(
                "[warn] buffer 上限丢失开头 {} 字节\n",
                snapshot.bytes_dropped
            ));
        }
        if !snapshot.content.is_empty() {
            text.push_str(&snapshot.content);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        } else if !snapshot.state.is_terminal() {
            text.push_str("(无新输出)\n");
        }
        Ok(text)
    }
}

fn render_listing(shells: &[std::sync::Arc<super::background::BackgroundShell>]) -> String {
    if shells.is_empty() {
        return "(无后台 shell)".into();
    }
    let mut text = String::from("后台 shell 列表（最近在前）：\n");
    for s in shells {
        let elapsed = s.started_at.elapsed().as_secs();
        text.push_str(&format!(
            "- {} [{}] {}s `{}`",
            s.task_id,
            s.state().label(),
            elapsed,
            s.command,
        ));
        if let Some(p) = s.log_path() {
            text.push_str(&format!(" log={}", p.display()));
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::process::Command;

    fn spawn(cmd: &str) -> tokio::process::Child {
        Command::new("bash")
            .arg("-lc")
            .arg(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn lists_when_no_task_id() {
        let shells = BgTaskRegistry::new();
        shells.register("true".into(), "/".into(), false, None, spawn("true"));
        let tool = BashOutputTool::new(shells);
        let out = tool.execute(json!({})).await.unwrap();
        assert!(out.contains("bash_"));
    }

    #[tokio::test]
    async fn reads_incremental_with_wait() {
        let shells = BgTaskRegistry::new();
        let s = shells.register(
            "echo a; sleep 0.05; echo b".into(),
            "/".into(),
            false,
            None,
            spawn("echo a; sleep 0.05; echo b"),
        );
        let tool = BashOutputTool::new(shells);
        let out = tool
            .execute(json!({"task_id": s.task_id, "wait_ms": 500}))
            .await
            .unwrap();
        assert!(out.contains("a") || out.contains("b"));
    }

    #[tokio::test]
    async fn unknown_task_id_errors() {
        let shells = BgTaskRegistry::new();
        let tool = BashOutputTool::new(shells);
        let res = tool.execute(json!({"task_id": "bash_999"})).await;
        assert!(res.is_err());
    }
}
