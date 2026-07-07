//! 交互式 Bash 工具：启动一个保留 stdin 写入能力的 PTY 会话。
//!
//! 与 [`super::bash::BashTool`] 的区别：
//! - Bash 工具关闭 stdin（管道模式 `Stdio::null()` / PTY 模式 drop master），
//!   避免交互式提示卡住——适合绝大多数非交互命令
//! - InteractiveBash **保留** PTY master 写端，让 agent 后续通过
//!   [`super::bash_input::BashInputTool`] 向进程发送输入（y/N 确认、密码等）
//!
//! 典型场景：`apt install` / `ssh-keygen` / `mysql` 交互式 prompt 等。
//! 这些命令在没有 stdin 的环境下会卡死或跳过确认，InteractiveBash 让 agent
//! 能像人一样「读 prompt → 输入回答 → 读后续输出」。
//!
//! 生命周期：
//! 1. 启动 → 等待首批输出或超时（默认 60s，上限 60s）
//! 2. 超时 / 首批输出到达 → 转后台，返回 `task_id`
//! 3. 模型用 `BashOutput` 读增量输出、`BashInput` 发输入、`KillShell` 终止

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};

use super::background::BgTaskRegistry;
use super::{Tool, ToolCtx};
use crate::tools::bash::{apply_noninteractive_env_pty, clean_ansi_progress};
use crate::workspace::Workspace;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 60;
const MAX_OUTPUT_BYTES: usize = 30_000;

pub struct InteractiveBashTool {
    workspace: Arc<Workspace>,
    shells: BgTaskRegistry,
    bg_log_dir: Option<PathBuf>,
    shell: Option<String>,
}

impl InteractiveBashTool {
    pub fn new(
        workspace: Arc<Workspace>,
        shells: BgTaskRegistry,
        bg_log_dir: Option<PathBuf>,
        shell: Option<String>,
    ) -> Self {
        Self {
            workspace,
            shells,
            bg_log_dir,
            shell,
        }
    }
}

#[async_trait]
impl Tool for InteractiveBashTool {
    fn name(&self) -> &str {
        "InteractiveBash"
    }

    fn description(&self) -> &str {
        "启动一个**交互式** PTY 会话——保留 stdin 写入能力，让 agent 能响应 \
         y/N 确认、密码输入等交互式 prompt。\n\
         **仅**在命令明确需要交互式输入时使用（如 apt install / ssh-keygen / mysql）。\
         正常命令用 Bash 工具——它更高效且不会因交互提示卡住。\n\
         启动后等待首批输出或超时（默认 60s），随后转后台返回 task_id。\
         用 BashOutput 读取实时输出、BashInput 发送输入、KillShell 终止会话。"
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
                                    必须在对话允许的路径范围内。"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_SECS,
                    "description": "等待首批输出的秒数，默认 60，最大 60。\
                                    到点后转后台返回 task_id。"
                },
                "description": {
                    "type": "string",
                    "description": "5-10 字描述这条命令在做什么，便于审批 UI 展示"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        self.execute_streaming(ToolCtx::noop(), input).await
    }

    async fn execute_streaming(&self, ctx: ToolCtx, input: Value) -> AppResult<String> {
        Ok(self.run(ctx, input).await?.0)
    }

    async fn execute_rich(&self, ctx: ToolCtx, input: Value) -> AppResult<super::ToolOutput> {
        let (text, is_error) = self.run(ctx, input).await?;
        Ok(super::ToolOutput {
            text,
            attachments: Vec::new(),
            is_error,
        })
    }
}

impl InteractiveBashTool {
    async fn run(&self, ctx: ToolCtx, input: Value) -> AppResult<(String, bool)> {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::Read;

        let command = input["command"]
            .as_str()
            .ok_or_else(|| AppError::msg("InteractiveBash: 缺少 command"))?;
        if command.trim().is_empty() {
            return Err(AppError::msg("InteractiveBash: command 不能为空"));
        }
        let cwd = self.workspace.resolve_cwd(input["cwd"].as_str());
        let timeout = input["timeout_secs"]
            .as_u64()
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::msg(format!("InteractiveBash: PTY 打开失败 {e}")))?;

        let mut cmd_builder = CommandBuilder::new("bash");
        cmd_builder.arg("-lc");
        cmd_builder.arg(command);
        cmd_builder.cwd(&cwd);
        // 非交互环境：关 pager / git prompt 等，但保留 stdin 交互能力
        apply_noninteractive_env_pty(&mut cmd_builder);
        if let Some(path) = crate::shell_env::resolve_shell_path(self.shell.as_deref()).await {
            cmd_builder.env("PATH", path);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd_builder)
            .map_err(|e| AppError::msg(format!("InteractiveBash: 启动失败 {e}")))?;

        // 关闭 slave：子进程端已 fork，父端不再需要
        drop(pair.slave);

        let master_reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AppError::msg(format!("InteractiveBash: reader 失败 {e}")))?;

        // take_writer：保留 master 写端——这是与 BashTool run_pty 的关键区别。
        // BashTool drop(master) 关闭写端让 stdin EOF；InteractiveBash 保留写端
        // 让 agent 后续能通过 BashInput 发送输入。
        let master_writer = pair
            .master
            .take_writer()
            .map_err(|e| AppError::msg(format!("InteractiveBash: writer 失败 {e}")))?;

        let pid = child.process_id().unwrap_or(0);

        // 注册到 BgTaskRegistry，传 pty_writer 让 BashInput 能写入
        let shell = self.shells.register_pty_background(
            command.to_string(),
            cwd.display().to_string(),
            pid,
            self.bg_log_dir.as_deref(),
            Some(master_writer),
        );

        // arm 自动通知：进程终态时 WakeupScheduler 投递 BgTaskFinished
        arm_auto_notification(&ctx, &shell.task_id);

        // 后台 reader task：持续读 PTY 输出 → 灌入 shell tail buffer
        let shell_for_reader = shell.clone();
        tokio::task::spawn_blocking(move || {
            let mut reader = master_reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF: 子进程退出
                    Ok(n) => {
                        shell_for_reader.append_raw(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        });

        // 后台 waiter task：等子进程退出 → shell.finish()
        let shell_for_waiter = shell.clone();
        tokio::task::spawn_blocking(move || {
            let status = child.wait();
            let state = match status {
                Ok(s) => {
                    let code = if s.success() { Some(0) } else { None };
                    super::background::ShellState::Exited { code }
                }
                Err(e) => super::background::ShellState::Failed {
                    error: e.to_string(),
                },
            };
            shell_for_waiter.finish(state);
        });

        // 主流程：等待 timeout 或读到首批输出（以先到者为准），返回 task_id + 初始输出
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
        let mut buffer = String::new();

        loop {
            // cancel 支持
            let cancel_flag = ctx.cancel.clone();
            let cancel_fut = async {
                match cancel_flag {
                    Some(flag) => wait_for_cancel(flag).await,
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                biased;
                _ = shell.wait_terminal() => {
                    // 进程在超时前就退出了。等一小拍让 reader 线程把 PTY
                    // 残留输出灌入 tail buffer——wait_terminal 由 waiter 线程
                    // 触发，但 reader 线程可能还差一拍才读到 EOF。
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    drain_output(&shell, &ctx, &mut buffer, usize::MAX);
                    let final_state = shell.state();
                    let is_error = !matches!(final_state, super::background::ShellState::Exited { code: Some(0) });
                    let text = format_early_exit(&shell.task_id, &buffer, &final_state);
                    self.shells.unregister(&shell.task_id);
                    return Ok((truncate_bytes(&text, MAX_OUTPUT_BYTES), is_error));
                }
                _ = tokio::time::sleep_until(deadline) => {
                    // 超时：进程仍在跑，转后台
                    drain_output(&shell, &ctx, &mut buffer, super::background::READ_CHUNK_BYTES);
                    let mut text = format!("[{}] {timeout}s 内未结束，已转后台", shell.task_id);
                    if !buffer.is_empty() {
                        text.push_str("\n--- 已产出 ---\n");
                        text.push_str(&clean_ansi_progress(&buffer));
                    }
                    text.push_str("\n\n[BashOutput 增量读取输出，BashInput 发送输入，KillShell 终止]");
                    return Ok((truncate_bytes(&text, MAX_OUTPUT_BYTES), false));
                }
                _ = cancel_fut => {
                    // 用户取消
                    self.shells.kill(&shell.task_id).await;
                    drain_output(&shell, &ctx, &mut buffer, usize::MAX);
                    let mut text = format!("[{}] ", shell.task_id);
                    if !buffer.is_empty() {
                        text.push_str(&clean_ansi_progress(&buffer));
                        if !text.ends_with('\n') {
                            text.push('\n');
                        }
                    }
                    text.push_str("[已中断]");
                    return Ok((truncate_bytes(&text, MAX_OUTPUT_BYTES), false));
                }
                _ = tokio::time::sleep(Duration::from_millis(150)) => {
                    // 定期抽增量输出
                    drain_output(&shell, &ctx, &mut buffer, super::background::READ_CHUNK_BYTES);
                    // 读到首批输出就转后台——让模型尽快看到 prompt 并决定发什么输入
                    if !buffer.is_empty() {
                        let mut text = format!("[{}] 已捕获输出，转后台", shell.task_id);
                        text.push_str("\n--- 已产出 ---\n");
                        text.push_str(&clean_ansi_progress(&buffer));
                        text.push_str("\n\n[BashOutput 增量读取输出，BashInput 发送输入，KillShell 终止]");
                        return Ok((truncate_bytes(&text, MAX_OUTPUT_BYTES), false));
                    }
                }
            }
        }
    }
}

/// 启动后台 task 后自动 arm 一个 WakeupScheduler 监听（架构 §4.12.5）。
fn arm_auto_notification(ctx: &ToolCtx, task_id: &str) {
    let (Some(sid), Some(rid)) = (ctx.session_id.as_deref(), ctx.run_id.as_deref()) else {
        return;
    };
    let call_id = if ctx.call_id.is_empty() {
        None
    } else {
        Some(ctx.call_id.clone())
    };
    crate::wakeup::WakeupScheduler::global().arm_bg_task(
        sid.to_string(),
        rid.to_string(),
        task_id.to_string(),
        call_id,
    );
}

fn drain_output(
    shell: &super::background::BackgroundShell,
    ctx: &ToolCtx,
    buffer: &mut String,
    max_bytes: usize,
) {
    let snap = shell.read_incremental(max_bytes);
    if !snap.content.is_empty() {
        ctx.emit_chunk(snap.content.clone());
        buffer.push_str(&snap.content);
    }
}

fn format_early_exit(task_id: &str, buffer: &str, state: &super::background::ShellState) -> String {
    let suffix = match state {
        super::background::ShellState::Exited { code: Some(0) } => None,
        super::background::ShellState::Exited { code: Some(c) } => Some(format!("[exit {c}]")),
        super::background::ShellState::Exited { code: None } => {
            Some("[terminated by signal]".to_string())
        }
        super::background::ShellState::Killed => Some("[killed]".to_string()),
        super::background::ShellState::Failed { error } => Some(format!("[failed: {error}]")),
        super::background::ShellState::Running => None,
    };
    let mut text = format!("[{task_id}] ");
    if !buffer.is_empty() {
        text.push_str(&clean_ansi_progress(buffer));
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    if let Some(s) = suffix {
        text.push_str(&s);
    }
    text
}

async fn wait_for_cancel(cancel: common::CancelFlag) {
    use std::sync::atomic::Ordering;
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
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

    fn tool(path: &std::path::Path) -> InteractiveBashTool {
        InteractiveBashTool::new(workspace_at(path), BgTaskRegistry::new(), None, None)
    }

    #[tokio::test]
    async fn echo_works() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tool(tmp.path())
            .execute(json!({"command": "echo hello-interactive"}))
            .await
            .unwrap();
        assert!(out.contains("hello-interactive"), "output: {out}");
    }

    #[tokio::test]
    async fn returns_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tool(tmp.path())
            .execute(json!({"command": "echo hi"}))
            .await
            .unwrap();
        assert!(out.contains("[bash_"), "output: {out}");
    }

    /// 交互式场景：cat 等待 stdin 输入，BashInput 写入后直接返回本次输入产生的回显。
    #[tokio::test]
    async fn interactive_cat_echo() {
        use crate::tools::bash_input::BashInputTool;
        use crate::tools::bash_output::BashOutputTool;

        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let bash = InteractiveBashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let bash_input = BashInputTool::new(shells.clone());
        let bash_output = BashOutputTool::new(shells.clone());

        // cat 从 stdin 读取并回显——会阻塞等待输入
        let out = bash
            .execute(json!({"command": "cat", "timeout_secs": 2}))
            .await
            .unwrap();
        assert!(out.contains("[bash_"), "output: {out}");

        // 拿到 task_id
        let task_id = shells.list()[0].task_id.clone();

        // 向 cat 发送输入；BashInput 自己返回这次输入之后的新输出。
        let input_result = bash_input
            .execute(json!({"task_id": task_id, "input": "hello-from-input", "idle_ms": 100}))
            .await
            .unwrap();
        assert!(
            input_result.contains("已发送"),
            "input_result: {input_result}"
        );
        assert!(
            input_result.contains("hello-from-input"),
            "input_result: {input_result}"
        );
        assert!(
            input_result.contains("仍在运行"),
            "input_result: {input_result}"
        );

        // BashInput 已经推进 read_cursor，后续 BashOutput 不应重复返回同一段回显。
        let output = bash_output
            .execute(json!({"task_id": task_id, "wait_ms": 50}))
            .await
            .unwrap();
        assert!(
            !output.contains("hello-from-input"),
            "output should not duplicate BashInput result: {output}"
        );

        // 发 EOF（Ctrl-D）让 cat 退出
        let _ = bash_input
            .execute(
                json!({"task_id": task_id, "input": "\x04", "press_enter": false, "idle_ms": 100}),
            )
            .await;

        // 清理
        let _ = shells.kill(&task_id).await;
    }

    #[tokio::test]
    async fn bash_input_returns_only_output_since_this_input() {
        use crate::tools::bash_input::BashInputTool;

        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let bash = InteractiveBashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let bash_input = BashInputTool::new(shells.clone());

        let out = bash
            .execute(json!({
                "command": "stty -echo; while IFS= read -r line; do echo got:$line; done",
                "timeout_secs": 1
            }))
            .await
            .unwrap();
        assert!(out.contains("[bash_"), "output: {out}");
        let task_id = shells.list()[0].task_id.clone();

        let first = bash_input
            .execute(json!({"task_id": task_id, "input": "first", "idle_ms": 100}))
            .await
            .unwrap();
        assert!(first.contains("got:first"), "first: {first}");

        let second = bash_input
            .execute(json!({"task_id": task_id, "input": "second", "idle_ms": 100}))
            .await
            .unwrap();
        assert!(second.contains("got:second"), "second: {second}");
        assert!(
            !second.contains("got:first"),
            "second BashInput result should not include previous output: {second}"
        );

        let _ = bash_input
            .execute(
                json!({"task_id": task_id, "input": "\x04", "press_enter": false, "idle_ms": 100}),
            )
            .await;
        let _ = shells.kill(&task_id).await;
    }

    #[tokio::test]
    async fn bash_output_reads_incrementally_for_interactive_bash() {
        use crate::tools::bash_input::BashInputTool;
        use crate::tools::bash_output::BashOutputTool;

        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let bash = InteractiveBashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let bash_input = BashInputTool::new(shells.clone());
        let bash_output = BashOutputTool::new(shells.clone());

        let out = bash
            .execute(json!({
                "command": "stty -echo; while IFS= read -r line; do echo out:$line; done",
                "timeout_secs": 1
            }))
            .await
            .unwrap();
        assert!(out.contains("[bash_"), "output: {out}");
        let task_id = shells.list()[0].task_id.clone();

        // 用 wait_ms=0 只写入不读取，让 BashOutput 成为唯一消费者。
        let _ = bash_input
            .execute(json!({"task_id": task_id, "input": "one", "wait_ms": 0}))
            .await
            .unwrap();
        let first = bash_output
            .execute(json!({"task_id": task_id, "wait_ms": 1000}))
            .await
            .unwrap();
        assert!(first.contains("out:one"), "first: {first}");

        let second = bash_output
            .execute(json!({"task_id": task_id, "wait_ms": 50}))
            .await
            .unwrap();
        assert!(
            !second.contains("out:one"),
            "second BashOutput result should not repeat previous output: {second}"
        );

        let _ = bash_input
            .execute(
                json!({"task_id": task_id, "input": "\x04", "press_enter": false, "idle_ms": 100}),
            )
            .await;
        let _ = shells.kill(&task_id).await;
    }

    #[tokio::test]
    async fn interactive_bash_collapses_carriage_return_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tool(tmp.path())
            .execute(json!({"command": "printf 'step1\\rstep2\\rfinal\\n'"}))
            .await
            .unwrap();
        assert!(out.contains("final"), "output: {out:?}");
        assert!(
            !out.contains("step1"),
            "output should collapse CR frames: {out:?}"
        );
        assert!(
            !out.contains("step2"),
            "output should collapse CR frames: {out:?}"
        );
    }

    #[tokio::test]
    async fn bash_input_collapses_carriage_return_progress() {
        use crate::tools::bash_input::BashInputTool;

        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let bash = InteractiveBashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let bash_input = BashInputTool::new(shells.clone());

        let out = bash
            .execute(json!({
                "command": "stty -echo; while IFS= read -r line; do printf 'old\\rnew:%s\\n' \"$line\"; done",
                "timeout_secs": 1
            }))
            .await
            .unwrap();
        assert!(out.contains("[bash_"), "output: {out}");
        let task_id = shells.list()[0].task_id.clone();

        let result = bash_input
            .execute(json!({"task_id": task_id, "input": "value", "idle_ms": 100}))
            .await
            .unwrap();
        assert!(result.contains("new:value"), "result: {result:?}");
        assert!(
            !result.contains("old"),
            "result should collapse CR frames: {result:?}"
        );

        let _ = bash_input
            .execute(
                json!({"task_id": task_id, "input": "\x04", "press_enter": false, "idle_ms": 100}),
            )
            .await;
        let _ = shells.kill(&task_id).await;
    }

    #[tokio::test]
    async fn timeout_transitions_to_background() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let bash = InteractiveBashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        // cat 无输入会一直等
        let out = bash
            .execute(json!({"command": "cat", "timeout_secs": 1}))
            .await
            .unwrap();
        assert!(out.contains("已转后台"), "output: {out}");
        assert!(out.contains("[bash_"), "output: {out}");

        let task_id = shells.list()[0].task_id.clone();
        let _ = shells.kill(&task_id).await;
    }
}
