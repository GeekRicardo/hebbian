//! BashInput 工具：向 InteractiveBash 启动的交互式会话发送 stdin 输入。
//!
//! - 与 [`super::bash_output::BashOutputTool`] 对偶：一个读、一个写
//! - 仅对 InteractiveBash 启动的会话有效；普通 Bash 后台任务无 PTY writer，调用报错
//! - `press_enter` 默认 true：自动在输入末尾追加 `\n`，模拟用户按回车
//! - 特殊字符：`\x04`（Ctrl-D / EOF）、`\x03`（Ctrl-C）等按原样写入

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};

use crate::tools::bash::clean_ansi_progress;

use super::background::{BgTaskRegistry, ShellState, READ_CHUNK_BYTES};
use super::Tool;

const DEFAULT_WAIT_MS: u64 = 30_000;
const MAX_WAIT_MS: u64 = 30_000;
const DEFAULT_IDLE_MS: u64 = 5_000;
const MAX_IDLE_MS: u64 = 30_000;

pub struct BashInputTool {
    shells: BgTaskRegistry,
}

impl BashInputTool {
    pub fn new(shells: BgTaskRegistry) -> Self {
        Self { shells }
    }
}

#[async_trait]
impl Tool for BashInputTool {
    fn name(&self) -> &str {
        "BashInput"
    }

    fn description(&self) -> &str {
        "向 InteractiveBash 启动的交互式会话发送 stdin 输入（按 task_id）。\
         用于响应 y/N 确认、密码输入等交互式 prompt。\
         返回本次输入之后产生的新输出，不拼接之前已经产生的输出。\n\
         `press_enter` 默认 true：自动在末尾追加换行符（模拟按回车）。\
         发送 Ctrl-D（EOF）时设 `press_enter: false` 并传 `\\x04`。\
         `wait_ms` 默认 30000，最多 30000：连续有输出时最多等待的总时间。\
         `idle_ms` 默认 5000，最多 30000：连续这么久没有新输出就暂时返回；进程仍可能在运行。\
         仅对 InteractiveBash 的 task 有效，普通 Bash 后台任务会报错。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task_id", "input"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "InteractiveBash 返回的 task_id（形如 bash_001）"
                },
                "input": {
                    "type": "string",
                    "description": "要发送的输入内容。特殊字符：\\x04=Ctrl-D(EOF)、\\x03=Ctrl-C"
                },
                "press_enter": {
                    "type": "boolean",
                    "default": true,
                    "description": "true=自动在末尾追加换行符（默认）。false=原样发送，不追加。"
                },
                "wait_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_WAIT_MS,
                    "default": DEFAULT_WAIT_MS,
                    "description": "发送输入后等待新输出 / 状态变化的总毫秒数，最大 30000。默认 30000。"
                },
                "idle_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_IDLE_MS,
                    "default": DEFAULT_IDLE_MS,
                    "description": "连续多少毫秒没有新输出就暂时返回。默认 5000，最大 30000。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let task_id = input["task_id"]
            .as_str()
            .ok_or_else(|| AppError::msg("BashInput: 缺少 task_id"))?;
        let text = input["input"]
            .as_str()
            .ok_or_else(|| AppError::msg("BashInput: 缺少 input"))?;
        let press_enter = input["press_enter"].as_bool().unwrap_or(true);
        let wait_ms = input["wait_ms"]
            .as_u64()
            .unwrap_or(DEFAULT_WAIT_MS)
            .min(MAX_WAIT_MS);
        let idle_ms = input["idle_ms"]
            .as_u64()
            .unwrap_or(DEFAULT_IDLE_MS)
            .min(MAX_IDLE_MS);

        if task_id.starts_with("subagent-") {
            return Err(AppError::msg(
                "BashInput 只能向 InteractiveBash 会话发送输入，不支持后台 subagent 任务",
            ));
        }

        let shell = self
            .shells
            .get(task_id)
            .ok_or_else(|| AppError::msg(format!("BashInput: 未找到 task_id={task_id}")))?;

        if !shell.has_pty_writer() {
            return Err(AppError::msg(
                "该任务不支持输入（非 InteractiveBash 会话）。只有 InteractiveBash 启动的会话才能发送输入。",
            ));
        }

        let mut data = text.as_bytes().to_vec();
        if press_enter && !data.ends_with(b"\n") {
            data.push(b'\n');
        }

        let cursor_before_input = shell.mark_read_to_end();
        shell.write_input(&data)?;
        shell
            .wait_for_quiet_after(cursor_before_input, wait_ms, idle_ms)
            .await;

        let snapshot = shell.read_incremental(READ_CHUNK_BYTES);
        let status = match &snapshot.state {
            ShellState::Running => "running".to_string(),
            ShellState::Exited { code: Some(c) } => format!("exit {c}"),
            ShellState::Exited { code: None } => "terminated".to_string(),
            ShellState::Killed => "killed".to_string(),
            ShellState::Failed { error } => format!("failed: {error}"),
        };

        let preview = if text.len() > 50 {
            format!("{}…", &text[..50])
        } else {
            text.to_string()
        };
        let mut out = format!("[{} {}] 已发送输入: {preview:?}\n", task_id, status);
        if matches!(snapshot.state, ShellState::Running) {
            out.push_str(&format!(
                "[仍在运行] 已因 {}ms 无新输出暂时返回；可继续用 BashInput 发送输入，或用 BashOutput 读取后续输出。\n",
                idle_ms
            ));
        }
        if snapshot.bytes_dropped > 0 {
            out.push_str(&format!(
                "[warn] buffer 上限丢失开头 {} 字节\n",
                snapshot.bytes_dropped
            ));
        }
        if snapshot.content.is_empty() {
            if !snapshot.state.is_terminal() {
                out.push_str("(暂无新输出)\n");
            }
        } else {
            out.push_str(&clean_ansi_progress(&snapshot.content));
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_task_id_errors() {
        let shells = BgTaskRegistry::new();
        let tool = BashInputTool::new(shells);
        let res = tool.execute(json!({"input": "hello"})).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn missing_input_errors() {
        let shells = BgTaskRegistry::new();
        let tool = BashInputTool::new(shells);
        let res = tool.execute(json!({"task_id": "bash_001"})).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn unknown_task_id_errors() {
        let shells = BgTaskRegistry::new();
        let tool = BashInputTool::new(shells);
        let res = tool
            .execute(json!({"task_id": "bash_999", "input": "hello"}))
            .await;
        assert!(res.is_err());
    }
}
