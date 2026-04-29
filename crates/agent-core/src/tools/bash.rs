//! Bash 工具：在用户机器上跑 shell 命令。
//!
//! - destructive：默认走 PermissionGate（`PermissionPolicy::always_ask` 含 "Bash"）
//! - cwd 必须在 workspace 范围内，越界直接拒绝
//! - 输出 stdout + stderr 合并，超长会截断

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use platform::{AppError, AppResult};
use protocol::RiskLevel;
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time;

use super::{Tool, ToolClass};
use crate::workspace::Workspace;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_OUTPUT_BYTES: usize = 30_000;

pub struct BashTool {
    workspace: Arc<Workspace>,
}

impl BashTool {
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "在工作目录下执行 shell 命令（通过 `bash -lc`）。\
         合并 stdout/stderr 返回，过长会截断。\
         需要审批，因为命令可能修改或删除文件。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的命令"
                },
                "cwd": {
                    "type": "string",
                    "description": "工作目录（绝对路径）。不传则用对话的 workdir。\
                                    必须在对话允许的目录范围内。"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_SECS,
                    "description": "超时秒数，默认 60，最大 600"
                },
                "description": {
                    "type": "string",
                    "description": "5-10 字描述这条命令在做什么，便于审批 UI 展示"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| AppError::msg("Bash: 缺少 command"))?;
        if command.trim().is_empty() {
            return Err(AppError::msg("Bash: command 不能为空"));
        }
        // 越界检查在 agent_loop 已统一做（PathAccess 审批），这里直接用
        let cwd = self.workspace.resolve_cwd(input["cwd"].as_str());
        let timeout = input["timeout_secs"]
            .as_u64()
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .current_dir(&cwd);

        let output = match time::timeout(Duration::from_secs(timeout), cmd.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(AppError::msg(format!("Bash: 启动失败 {e}"))),
            Err(_) => {
                return Err(AppError::msg(format!("Bash: 命令超时（{timeout}s）")));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let status = output.status;

        let mut text = String::new();
        if !stdout.is_empty() {
            text.push_str(stdout.as_ref());
        }
        if !stderr.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("[stderr]\n");
            text.push_str(stderr.as_ref());
        }
        if !status.success() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            match status.code() {
                Some(code) => text.push_str(&format!("[exit {code}]")),
                None => text.push_str("[terminated by signal]"),
            }
        }

        if text.is_empty() {
            text.push_str("(无输出)");
        }
        Ok(truncate_bytes(&text, MAX_OUTPUT_BYTES))
    }

    fn affected_paths(&self, input: &Value) -> Vec<PathBuf> {
        vec![self.workspace.resolve_cwd(input["cwd"].as_str())]
    }

    fn classify(&self, _input: &Value) -> ToolClass {
        ToolClass::Destructive { risk: RiskLevel::High }
    }
}

fn truncate_bytes(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…[已截断，共 {} 字节]", &s[..end], s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_at(path: &std::path::Path) -> Arc<Workspace> {
        Workspace::new(path, Vec::new())
    }

    #[tokio::test]
    async fn echo_works() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = BashTool::new(workspace_at(tmp.path()));
        let out = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn nonzero_exit_includes_code() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = BashTool::new(workspace_at(tmp.path()));
        let out = tool.execute(json!({"command": "exit 7"})).await.unwrap();
        assert!(out.contains("[exit 7]"));
    }

    #[tokio::test]
    async fn affected_paths_returns_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = BashTool::new(workspace_at(tmp.path()));
        let paths = tool.affected_paths(&json!({"command": "ls"}));
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], tmp.path());

        let paths = tool.affected_paths(&json!({"command": "ls", "cwd": "/etc"}));
        assert_eq!(paths[0], std::path::Path::new("/etc"));
    }

    #[tokio::test]
    async fn timeout_is_enforced() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = BashTool::new(workspace_at(tmp.path()));
        let res = tool
            .execute(json!({"command": "sleep 5", "timeout_secs": 1}))
            .await;
        assert!(res.is_err());
    }
}
