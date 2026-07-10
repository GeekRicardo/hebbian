//! Bash 工具：在用户机器上跑 shell 命令。
//!
//! - 分类逻辑挪到 [`crate::effects::analyze_effects`]（架构 §4.4.2 effects 解耦）：
//!   - 解析 shell line，全部子命令命中 [`safe_commands`] 白名单且无危险结构 → `ReadOnly`，自动放行
//!   - 否则 → `Destructive`，走 HITL 审批
//! - cwd 必须在 workspace 范围内，越界直接拒绝
//! - 输出 stdout + stderr 合并，超长会截断
//! - **超时不 kill**：超时（或 `run_in_background=true`）时把进程转后台，
//!   返回 `task_id`，由 `BashOutput` / `KillShell` 后续接管。详见
//!   [`super::background::BackgroundShells`]。
//!
//! [`safe_commands`]: super::safe_commands

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};
use tokio::process::Command;

use super::background::{BgTaskRegistry, ShellState, READ_CHUNK_BYTES};
use super::{Tool, ToolCtx};
use crate::workspace::Workspace;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_OUTPUT_BYTES: usize = 30_000;

pub struct BashTool {
    workspace: Arc<Workspace>,
    shells: BgTaskRegistry,
    /// 当前 session 的 bg 日志目录（架构 §4.12.3）。`None` 时 BgTaskRegistry
    /// 回落到 tail-only。CLI 单跑 / 单测一般传 None；desktop chat.rs 传
    /// `~/.hebbian/sessions/<sid>/bg`。
    bg_log_dir: Option<PathBuf>,
    shell: Option<String>,
}

impl BashTool {
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
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "在工作目录下执行 shell 命令（通过 `bash -lc`）。\
         合并 stdout/stderr 返回，过长会截断。\
         **超时不会 kill 进程**：到点后命令转后台运行，返回 `task_id`，\
         随后用 `BashOutput` 增量取输出、`KillShell` 主动终止。\
         也可以传 `run_in_background: true` 立即放后台。"
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
                    "description": "前台等待秒数，默认 60，最大 600。\
                                    到点未结束则进程转后台，返回 task_id。"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "true = 立即放后台，立刻返回 task_id 不等待。\
                                    适合 dev server / build / test watch 等长跑命令。"
                },
                "description": {
                    "type": "string",
                    "description": "5-10 字描述这条命令在做什么，便于审批 UI 展示"
                },
                "pty": {
                    "type": "boolean",
                    "description": "true = 使用 PTY（伪终端）执行。默认 true。\
                                    子进程以为自己连的是真实终端，docker pull / npm install \
                                    等命令会正常显示实时进度条。\
                                    **注意**：PTY 下超时会自动切管道模式转后台。\
                                    传 false 强制走管道模式（无实时进度，但 stdout/stderr 分离）。"
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

impl BashTool {
    /// 主执行路径。返回 `(聚合输出文本, 是否语义失败)`——命令退出码非 0 /
    /// 被信号杀死 / 启动后故障都算失败，surface 据此把这次调用标红。
    /// 转后台 / 用户中断不算失败：前者结果未知，后者是用户主动行为。
    async fn run(&self, ctx: ToolCtx, input: Value) -> AppResult<(String, bool)> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| AppError::msg("Bash: 缺少 command"))?;
        if command.trim().is_empty() {
            return Err(AppError::msg("Bash: command 不能为空"));
        }
        let cwd = self.workspace.resolve_cwd(input["cwd"].as_str());
        let timeout = input["timeout_secs"]
            .as_u64()
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);
        let background = input["run_in_background"].as_bool().unwrap_or(false);
        let use_pty = input["pty"].as_bool().unwrap_or(true);

        // PTY 路径：伪终端执行，独立于 BackgroundShell 体系。
        // 不支持转后台——PTY 子进程生命周期完全在函数内管理。
        if use_pty && !background {
            return self
                .run_pty(&ctx, command, &cwd.display().to_string(), timeout)
                .await;
        }

        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .current_dir(&cwd);
        // 创建独立进程组：kill 时杀整组而非仅 bash，避免子命令（find/docker/…）变孤儿 PPID=1。
        #[cfg(unix)]
        cmd.process_group(0);
        apply_noninteractive_env_pipe(&mut cmd);
        if let Some(path) = crate::shell_env::resolve_shell_path(self.shell.as_deref()).await {
            cmd.env("PATH", path);
        }

        let child = cmd
            .spawn()
            .map_err(|e| AppError::msg(format!("Bash: 启动失败 {e}")))?;

        let cwd_str = cwd.display().to_string();
        // 前台命令此刻 register 时 is_background=false 且不传 log_dir——
        // 跑完会被 unregister，避免短命前台命令在 surface 残留为"已结束的后台任务"；
        // 超时转后台时再 promote_to_background()。只有用户显式 run_in_background=true
        // 才一开始就 is_background=true + 开日志。
        let log_dir = if background {
            self.bg_log_dir.as_deref()
        } else {
            None
        };
        let shell = self
            .shells
            .register(command.to_string(), cwd_str, background, log_dir, child);

        if background {
            // 自动 arm 通知（架构 §4.12.5 修订 / 借鉴 CC 2.1 "completed will notify"）：
            // task 进入终态时 WakeupScheduler 投递 BgTaskFinished 事件，surface 据此
            // 把 task-notification 注入下一轮 user message。
            arm_auto_notification(&ctx, &shell.task_id);
            // 极简一行：task_id 是模型下一步唯一需要的信息。
            // BashOutput / KillShell 的用法 / 自动通知机制都在工具描述里讲一次就够，
            // 每条 tool_result 不重复；日志路径模型用不上（无 fs 读权限），surface
            // BackgroundTaskPanel 已展示，不污染 transcript。
            return Ok((format!("[{}] 已在后台启动", shell.task_id), false));
        }

        // 前台等待：要么进程退出，要么超时。等待期间持续抽 tail buffer 增量，
        // 通过 ctx.progress emit `ToolCallOutputDelta`——surface 端能在
        // ToolCallFinished 之前看到 stdout/stderr 实时刷出来。
        //
        // 关键约束：read_incremental 推进 cursor，再次调 read 不会拿到旧字节。
        // 所以我们本地 buffer 累加 forward 出去的内容，最后聚合返回。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
        let mut buffer = String::new();
        let mut user_cancelled = false;
        let mut promoted_by_user = false;
        let exited = loop {
            // 终态 / deadline / 用户取消 / 用户切后台都退出循环；中间每 ~200ms 抽一次增量。
            let tick =
                tokio::time::sleep_until(tokio::time::Instant::now() + Duration::from_millis(200));
            // 有 cancel flag 时监听取消信号；没有时用 pending() 永不触发。
            let cancel_flag = ctx.cancel.clone();
            let cancel_fut = async move {
                match cancel_flag {
                    Some(flag) => wait_for_cancel(flag).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                _ = shell.wait_terminal() => {
                    drain_into(&shell, &ctx, &mut buffer, usize::MAX);
                    break true;
                }
                _ = shell.wait_background() => {
                    drain_into(&shell, &ctx, &mut buffer, READ_CHUNK_BYTES);
                    promoted_by_user = true;
                    break false;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    drain_into(&shell, &ctx, &mut buffer, READ_CHUNK_BYTES);
                    break false;
                }
                _ = cancel_fut => {
                    drain_into(&shell, &ctx, &mut buffer, usize::MAX);
                    user_cancelled = true;
                    break false;
                }
                _ = tick => {
                    drain_into(&shell, &ctx, &mut buffer, READ_CHUNK_BYTES);
                }
            }
        };

        if user_cancelled {
            // 用户点了停止：立即 kill 子进程，返回已产出内容。
            self.shells.kill(&shell.task_id).await;
            self.shells.unregister(&shell.task_id);
            let mut text = buffer;
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("[已中断]");
            return Ok((truncate_bytes(&text, MAX_OUTPUT_BYTES), false));
        }

        if !exited {
            if !promoted_by_user {
                // 超时：进程仍在跑，转后台。
                shell.promote_to_background();
            }
            // 与显式 run_in_background=true 同款自动 arm 通知：超时/用户转后台后，task 终态
            // 也由 WakeupScheduler 主动通知，模型不需要 poll。
            arm_auto_notification(&ctx, &shell.task_id);
            // 转后台同样极简——已产出的 output 还要保留（模型需要它继续推理），
            // 但不再重复工具用法和通知机制说明。
            let mut text = if promoted_by_user {
                format!("[{}] 已转后台", shell.task_id)
            } else {
                format!("[{}] {timeout}s 内未结束，已转后台", shell.task_id)
            };
            if !buffer.is_empty() {
                text.push_str("\n--- 已产出 ---\n");
                text.push_str(&buffer);
            }
            return Ok((truncate_bytes(&text, MAX_OUTPUT_BYTES), false));
        }

        // 已退出：聚合 buffer + 终态后缀；从注册表摘掉前台条目。
        let final_state = shell.state();
        let is_error = !matches!(final_state, ShellState::Exited { code: Some(0) });
        let text = format_finished(&buffer, &final_state);
        self.shells.unregister(&shell.task_id);
        Ok((truncate_bytes(&text, MAX_OUTPUT_BYTES), is_error))
    }

    /// PTY（伪终端）执行路径：让子进程认为自己连的是真实终端，从而获得
    /// docker pull / npm install / apt 等命令的实时进度条输出。
    ///
    /// 与 `run` 的管道模式不同：
    /// - stdout/stderr 在 PTY 中自然合并（与真实终端行为一致）
    /// - 输出含 ANSI 转义码（前端 `ansiToHtml` 已处理）
    /// - 不支持转后台——超时直接返回已产出内容
    /// - 不经过 BackgroundShell 注册表，生命周期完全自包含
    async fn run_pty(
        &self,
        ctx: &ToolCtx,
        command: &str,
        cwd: &str,
        timeout_secs: u64,
    ) -> AppResult<(String, bool)> {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::Read;
        use tokio::sync::mpsc;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AppError::msg(format!("PTY: 打开失败 {e}")))?;

        let mut cmd_builder = CommandBuilder::new("bash");
        cmd_builder.arg("-lc");
        cmd_builder.arg(command);
        cmd_builder.cwd(cwd);
        // PTY 会让不少 CLI 认为可以进入交互模式；Bash 工具没有 stdin，
        // 所以这里统一关闭 pager / prompt，保留 TTY 输出能力但不等待人工按键。
        apply_noninteractive_env_pty(&mut cmd_builder);
        if let Some(path) = crate::shell_env::resolve_shell_path(self.shell.as_deref()).await {
            cmd_builder.env("PATH", path);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd_builder)
            .map_err(|e| AppError::msg(format!("PTY: 启动失败 {e}")))?;

        // 父进程关闭 slave 端
        drop(pair.slave);

        let master_reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AppError::msg(format!("PTY: reader 失败 {e}")))?;

        // 关闭 master 写端：子进程的 stdin 读不到输入 = EOF，
        // 避免 apt install / rm -i 等交互式 y/N 提示卡住永远等输入。
        drop(pair.master);

        // mpsc: spawn_blocking reader → async context
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(128);

        tokio::task::spawn_blocking(move || {
            let mut reader = master_reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF: 子进程退出，PTY master 关闭
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break; // 接收端已关闭
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        let mut buffer = String::new();

        // 主循环：读 PTY 输出 → emit chunk → 累积 buffer。
        // 三个出口都在 select 分支内 return：reader EOF（正常退出）、cancel、timeout。
        loop {
            let cancel_flag = ctx.cancel.clone();
            let cancel_fut = async {
                match cancel_flag {
                    Some(flag) => wait_for_cancel(flag).await,
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                biased;
                chunk = rx.recv() => {
                    match chunk {
                        Some(data) => {
                            let text = String::from_utf8_lossy(&data);
                            ctx.emit_chunk(text.to_string());
                            buffer.push_str(&text);
                        }
                        None => {
                            // PTY master EOF → 子进程已退出。
                            // child.wait() 是同步调用，必须 spawn_blocking 避免阻塞 tokio worker。
                            let status = tokio::task::spawn_blocking(move || child.wait())
                                .await
                                .map_err(|e| AppError::msg(format!("PTY: join 失败 {e}")))?
                                .map_err(|e| AppError::msg(format!("PTY: wait 失败 {e}")))?;
                            let is_error = !status.success();
                            let suffix = if is_error {
                                "[exit non-zero]".to_string()
                            } else {
                                String::new()
                            };
                            let text = if buffer.is_empty() {
                                if suffix.is_empty() { "(无输出)".to_string() } else { suffix }
                            } else {
                                if suffix.is_empty() { buffer } else { format!("{buffer}\n{suffix}") }
                            };
                            let cleaned = clean_ansi_progress(&text);
                            return Ok((truncate_bytes(&cleaned, MAX_OUTPUT_BYTES), is_error));
                        }
                    }
                }
                _ = cancel_fut => {
                    // 杀进程组：PTY 下 forkpty 已创独立 session/进程组，
                    // child.kill() 只杀 leader，子命令变孤儿。
                    pty_kill_process_group(&mut child);
                    rx.close();
                    while let Ok(data) = rx.try_recv() {
                        let text = String::from_utf8_lossy(&data);
                        buffer.push_str(&text);
                    }
                    if !buffer.is_empty() && !buffer.ends_with('\n') {
                        buffer.push('\n');
                    }
                    buffer.push_str("[已中断]");
                    let cleaned = clean_ansi_progress(&buffer);
                    return Ok((truncate_bytes(&cleaned, MAX_OUTPUT_BYTES), false));
                }
                _ = tokio::time::sleep_until(deadline) => {
                    // 超时不杀——注册为后台任务，PTY 输出继续灌 tail buffer，
                    // 模型后续用 BashOutput 读、KillShell 终止。
                    let pid = child.process_id().unwrap_or(0);

                    let shell = self.shells.register_pty_background(
                        command.to_string(),
                        cwd.to_string(),
                        pid,
                        self.bg_log_dir.as_deref(),
                        None, // 现有 BashTool PTY 路径不支持 stdin 写入
                    );
                    arm_auto_notification(ctx, &shell.task_id);

                    // 后台续跑：继续读 PTY master → 灌 shell tail buffer
                    let shell_bg = shell.clone();
                    tokio::spawn(async move {
                        while let Some(data) = rx.recv().await {
                            let text = String::from_utf8_lossy(&data);
                            shell_bg.append(None, text.as_bytes());
                        }
                        // PTY master EOF → 子进程已退出
                        let status = match tokio::task::spawn_blocking(move || child.wait())
                            .await
                        {
                            Ok(Ok(s)) => s,
                            Ok(Err(e)) => {
                                shell_bg.finish(super::background::ShellState::Failed {
                                    error: e.to_string(),
                                });
                                return;
                            }
                            Err(e) => {
                                shell_bg.finish(super::background::ShellState::Failed {
                                    error: e.to_string(),
                                });
                                return;
                            }
                        };
                        let code = if status.success() { Some(0) } else { None };
                        shell_bg.finish(super::background::ShellState::Exited { code });
                    });

                    let mut text = format!(
                        "[{}] {timeout_secs}s 内未结束，已转后台",
                        shell.task_id
                    );
                    if !buffer.is_empty() {
                        text.push_str("\n--- 已产出 ---\n");
                        text.push_str(&clean_ansi_progress(&buffer));
                    }
                    // 提示模型用 BashOutput + wait_ms 取增量，避免立刻 poll 到空
                    text.push_str("\n\n[BashOutput 增量读取，传 wait_ms 等新输出]");
                    return Ok((truncate_bytes(&text, MAX_OUTPUT_BYTES), false));
                }
            }
        }
    }
}

/// 给 Bash 子进程设置非交互环境：保留命令输出，但禁止 pager / prompt 等需要人工输入的行为。
fn apply_noninteractive_env_pipe(cmd: &mut Command) {
    cmd.env("CI", "1")
        .env("TERM", "dumb")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("LESS", "FRX")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
}

/// PTY 路径使用 portable-pty 的 CommandBuilder，环境变量需单独设置。
pub(crate) fn apply_noninteractive_env_pty(cmd: &mut portable_pty::CommandBuilder) {
    cmd.env("CI", "1");
    cmd.env("TERM", "dumb");
    cmd.env("PAGER", "cat");
    cmd.env("GIT_PAGER", "cat");
    cmd.env("LESS", "FRX");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GCM_INTERACTIVE", "never");
}

/// 杀 PTY 子进程所在进程组（而非仅杀 leader）。
/// PTY 下 forkpty 已创建独立 session，子进程是 session leader，
/// kill(-pid, SIGKILL) 把整组（含 bash 内部跑的 find/docker 等）一起终止。
#[cfg(unix)]
fn pty_kill_process_group(child: &mut Box<dyn portable_pty::Child + Send + Sync>) {
    if let Some(pid) = child.process_id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn pty_kill_process_group(child: &mut Box<dyn portable_pty::Child + Send + Sync>) {
    let _ = child.kill();
}

/// 清理 PTY 输出中的光标移动 ANSI 转义码并合并 `\r` 回行。
/// docker pull / npm install 等进度条用 `\r` 覆盖同行、`ESC[nA` 回跳前 n 行。
/// 不处理的话文件里会堆积大量重复行和乱码，模型 Read 不到可读内容。
pub(crate) fn clean_ansi_progress(text: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;
    static RE_CURSOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("\x1b\\[\\d*[ABCDEFGHJKSTfhl]").unwrap());
    static RE_DEC: LazyLock<Regex> = LazyLock::new(|| Regex::new("\x1b\\[\\?\\d+[hl]").unwrap());

    let cleaned = RE_CURSOR.replace_all(text, "");
    let cleaned = RE_DEC.replace_all(&cleaned, "");

    // CRLF → LF：PTY 输出常用 \r\n 换行，不能让 \r 被误当进度条覆盖
    let cleaned = cleaned.replace("\r\n", "\n");

    // 合并 \r 回行：每行只保留最后一个 \r 之后的内容
    let cleaned = cleaned.trim_end_matches('\n');
    let mut out = String::with_capacity(cleaned.len());
    for line in cleaned.split('\n') {
        if let Some(last) = line.rsplit('\r').next() {
            out.push_str(last);
        }
        out.push('\n');
    }
    // 去掉末尾多余的换行
    out.truncate(out.trim_end().len());
    out
}

/// 启动后台 task 后自动 arm 一个 WakeupScheduler 监听（架构 §4.12.5 修订）。
///
/// 没有 session_id / run_id（CLI 单跑 / 单测路径）时跳过——没有完整 RunState 串接，
/// arm 也唤不醒任何 ResumeHandler，徒增日志噪音。
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

/// 把 shell 当前未读的 tail buffer 抽出来：emit 给 surface 一份、累加到本地 buffer 一份。
/// 单次最多抽 `max_bytes`，超长在下一次循环再继续。
fn drain_into(
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

fn format_finished(buffer: &str, state: &ShellState) -> String {
    let mut text = String::new();
    if !buffer.is_empty() {
        text.push_str(buffer);
    }
    let suffix = match state {
        ShellState::Exited { code: Some(0) } => None,
        ShellState::Exited { code: Some(c) } => Some(format!("[exit {c}]")),
        ShellState::Exited { code: None } => Some("[terminated by signal]".to_string()),
        ShellState::Killed => Some("[killed]".to_string()),
        ShellState::Failed { error } => Some(format!("[failed: {error}]")),
        ShellState::Running => None,
    };
    if let Some(s) = suffix {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&s);
    }
    if text.is_empty() {
        text.push_str("(无输出)");
    }
    text
}

/// 异步等待 cancel flag 置位（50ms 轮询）。
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

    fn tool(path: &std::path::Path) -> BashTool {
        BashTool::new(workspace_at(path), BgTaskRegistry::new(), None, None)
    }

    #[tokio::test]
    async fn echo_works() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tool(tmp.path())
            .execute(json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn user_shell_path_is_loaded_before_running_bash() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let marker_bin = bin_dir.join("hebbian-shell-marker");
        std::fs::write(&marker_bin, "#!/bin/sh\necho shell-path-ok\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&marker_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let profile = tmp.path().join("profile.sh");
        std::fs::write(
            &profile,
            format!(
                "#!/bin/sh\nexport PATH=\"{}:$PATH\"\nshift\nexec /bin/sh -c \"$@\"\n",
                bin_dir.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let shell = profile.display().to_string();
        let bash = BashTool::new(
            workspace_at(tmp.path()),
            BgTaskRegistry::new(),
            None,
            Some(shell),
        );

        let out = bash
            .execute(json!({"command": "hebbian-shell-marker"}))
            .await
            .unwrap();
        assert!(out.contains("shell-path-ok"), "{out}");
    }

    #[tokio::test]
    async fn nonzero_exit_includes_code() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tool(tmp.path())
            .execute(json!({"command": "exit 7", "pty": false}))
            .await
            .unwrap();
        assert!(out.contains("[exit 7]"));
    }

    /// 非零退出码必须通过 execute_rich 自报 is_error=true（成功命令保持 false），
    /// 否则前端状态点不会标红。
    #[tokio::test]
    async fn execute_rich_reports_is_error_on_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tool(tmp.path());
        let fail = t
            .execute_rich(ToolCtx::noop(), json!({"command": "exit 7"}))
            .await
            .unwrap();
        assert!(fail.is_error, "exit 7 应自报失败");
        let ok = t
            .execute_rich(ToolCtx::noop(), json!({"command": "true"}))
            .await
            .unwrap();
        assert!(!ok.is_error, "exit 0 不该标失败");
    }

    #[tokio::test]
    async fn timeout_transitions_to_background() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let out = t
            .execute(json!({"command": "sleep 5", "timeout_secs": 1}))
            .await
            .unwrap();
        assert!(out.contains("已转后台"));
        assert!(out.contains("[bash_"));
        // 注册表里应该能找到这个 task，且 is_background 已被 promote 翻为 true
        let tasks = shells.list();
        assert_eq!(tasks.len(), 1);
        assert!(
            tasks[0].is_background(),
            "超时转后台后必须 is_background=true"
        );
        // kill 它，避免测试结束后还有 sleep 进程
        let id = tasks[0].task_id.clone();
        shells.kill(&id).await;
    }

    /// 前台命令正常退出后，BashTool 应该把自己从 BackgroundShells 摘掉——
    /// 否则 surface 会把 "ls" 也展示为 "已结束的后台任务"。
    #[tokio::test]
    async fn foreground_exit_unregisters_from_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let out = t.execute(json!({"command": "echo hello"})).await.unwrap();
        assert!(out.contains("hello"));
        // 注册表必须为空：前台 ls 不该残留为"已结束的后台任务"
        assert!(
            shells.list().is_empty(),
            "前台命令跑完后 BackgroundShells 应清空，实际：{:?}",
            shells.list().iter().map(|s| &s.task_id).collect::<Vec<_>>()
        );
    }

    /// 显式 run_in_background=true 时一开始就 is_background=true 且留在注册表里。
    #[tokio::test]
    async fn explicit_background_keeps_in_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let _ = t
            .execute(json!({"command": "sleep 30", "run_in_background": true}))
            .await
            .unwrap();
        let tasks = shells.list();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].is_background());
        shells.kill(&tasks[0].task_id).await;
    }

    /// 端到端：Bash 超时 → 转后台 → BashOutput 增量查询 → KillShell 终止。
    #[tokio::test]
    async fn end_to_end_background_lifecycle() {
        use crate::tools::bash_output::BashOutputTool;
        use crate::tools::kill_shell::KillShellTool;

        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let bash = BashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let bash_out = BashOutputTool::new(shells.clone());
        let kill = KillShellTool::new(shells.clone());

        // 长跑命令（5s 内不会结束）→ 1s 后转后台
        let out = bash
            .execute(json!({
                "command": "for i in 1 2 3 4 5; do echo line$i; sleep 0.6; done",
                "timeout_secs": 1,
            }))
            .await
            .unwrap();
        assert!(out.contains("已转后台"));
        let task_id = shells.list()[0].task_id.clone();

        // BashOutput 等到至少一行
        let read = bash_out
            .execute(json!({"task_id": task_id, "wait_ms": 1000}))
            .await
            .unwrap();
        assert!(read.contains("line"));
        assert!(read.contains("running") || read.contains("exited"));

        // KillShell
        let killed = kill.execute(json!({"task_id": task_id})).await.unwrap();
        assert!(killed.contains("killed"));
    }

    #[tokio::test]
    async fn external_promote_returns_background_result() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);

        let run = tokio::spawn({
            let t = t;
            async move {
                t.execute(json!({
                    "command": "sleep 30",
                    "timeout_secs": 30,
                    "pty": false,
                }))
                .await
                .unwrap()
            }
        });

        let task_id = loop {
            if let Some(shell) = shells.list().into_iter().find(|s| !s.is_background()) {
                break shell.task_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        let shell = shells.get(&task_id).unwrap();
        shell.promote_to_background();
        let out = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap();
        assert!(out.contains("已转后台"), "{out}");
        assert!(shells.get(&task_id).unwrap().is_background());
        shells.kill(&task_id).await;
    }

    #[tokio::test]
    async fn run_in_background_returns_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let started = std::time::Instant::now();
        let out = t
            .execute(json!({"command": "sleep 30", "run_in_background": true}))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(out.contains("已在后台启动"));
        let id = shells.list()[0].task_id.clone();
        shells.kill(&id).await;
    }

    // 经过简化后 classify / affected_paths / permission_fingerprint 都搬到了
    // `crate::effects` 模块；具体单测见 `crate::effects::tests`。

    /// 流式输出（架构 §4.4.1）：长跑命令在 ToolCallFinished 到达之前，
    /// progress 通道应该收到至少两段 chunk——而不是憋到结尾一次性吐出来。
    #[tokio::test]
    async fn streaming_emits_chunks_before_finish() {
        use crate::tools::ToolProgress;
        use std::sync::Mutex;

        struct CaptureProgress {
            chunks: Mutex<Vec<String>>,
        }
        impl ToolProgress for CaptureProgress {
            fn emit(&self, chunk: String) {
                self.chunks.lock().unwrap().push(chunk);
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let bash = BashTool::new(workspace_at(tmp.path()), shells.clone(), None, None);
        let progress = Arc::new(CaptureProgress {
            chunks: Mutex::new(Vec::new()),
        });
        let ctx = crate::tools::ToolCtx {
            call_id: "test_call".into(),
            progress: Some(progress.clone()),
            session_id: None,
            run_id: None,
            cancel: None,
        };

        // 三行隔 300ms 输出——足够 forwarder 抽到 ≥2 段 chunk。
        let out = bash
            .execute_streaming(
                ctx,
                json!({
                    "command": "for i in 1 2 3; do echo line$i; sleep 0.3; done",
                    "timeout_secs": 5,
                }),
            )
            .await
            .unwrap();

        let chunks = progress.chunks.lock().unwrap();
        assert!(
            chunks.len() >= 2,
            "至少应收到 2 段 chunk（实时输出），实际：{:?}",
            *chunks
        );
        let joined: String = chunks.iter().cloned().collect();
        assert!(joined.contains("line1") && joined.contains("line3"));
        assert!(out.contains("line1") && out.contains("line3"));
    }

    /// 命令瞬时完成时也走 progress 路径（不应 panic / 丢内容）。chunk 数 0~1 皆可。
    #[tokio::test]
    async fn streaming_short_command_still_returns_result() {
        use crate::tools::ToolProgress;
        use std::sync::Mutex;

        struct CaptureProgress(Mutex<Vec<String>>);
        impl ToolProgress for CaptureProgress {
            fn emit(&self, c: String) {
                self.0.lock().unwrap().push(c);
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let shells = BgTaskRegistry::new();
        let bash = BashTool::new(workspace_at(tmp.path()), shells, None, None);
        let progress = Arc::new(CaptureProgress(Mutex::new(Vec::new())));
        let ctx = crate::tools::ToolCtx {
            call_id: "test_call".into(),
            progress: Some(progress.clone()),
            session_id: None,
            run_id: None,
            cancel: None,
        };

        let out = bash
            .execute_streaming(ctx, json!({"command": "echo quick"}))
            .await
            .unwrap();
        assert!(out.contains("quick"));
    }

    #[tokio::test]
    async fn pty_echo_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let bash = tool(tmp.path());
        let out = bash
            .execute(json!({"command": "echo hello-from-pty", "pty": true}))
            .await
            .unwrap();
        // PTY 输出可能包含 ANSI 转义码（bash 的 PS1 等），只检查核心内容
        assert!(out.contains("hello-from-pty"), "output: {out}");
    }

    #[tokio::test]
    async fn pty_multiline_output() {
        let tmp = tempfile::tempdir().unwrap();
        let bash = tool(tmp.path());
        let out = bash
            .execute(json!({
                "command": "echo line1; echo line2; echo line3",
                "pty": true
            }))
            .await
            .unwrap();
        assert!(out.contains("line1"), "output: {out}");
        assert!(out.contains("line2"), "output: {out}");
        assert!(out.contains("line3"), "output: {out}");
    }

    #[tokio::test]
    async fn pty_exit_code_nonzero() {
        let tmp = tempfile::tempdir().unwrap();
        let bash = tool(tmp.path());
        let out = bash
            .execute_rich(ToolCtx::noop(), json!({"command": "exit 42", "pty": true}))
            .await
            .unwrap();
        assert!(out.is_error, "exit 42 should be error");
        // PTY 下 exit 不一定产生输出，但 is_error 必须为 true
    }

    /// 验证 PTY 实时进度：`\r` 回行的进度条在管道模式下被缓冲，
    /// PTY 下应逐 chunk 到达（因为子进程认为自己是终端，每 flush 就推送）。
    #[tokio::test]
    async fn pty_realtime_progress_chunks() {
        use crate::tools::ToolProgress;
        use std::sync::Mutex;

        struct CaptureProgress(pub Mutex<Vec<String>>);
        #[async_trait]
        impl ToolProgress for CaptureProgress {
            fn emit(&self, chunk: String) {
                self.0.lock().unwrap().push(chunk);
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        // 写一个带 \r 回行的进度脚本
        let script = tmp.path().join("progress.py");
        std::fs::write(
            &script,
            "import sys, time\nfor i in range(10):\n sys.stdout.write(f'\\r[{i}/10]')\n sys.stdout.flush()\n time.sleep(0.05)\nsys.stdout.write('\\nDONE\\n')\n",
        )
        .unwrap();

        let bash = tool(tmp.path());
        let progress = Arc::new(CaptureProgress(Mutex::new(Vec::new())));
        let ctx = crate::tools::ToolCtx {
            call_id: "test_pty_progress".into(),
            progress: Some(progress.clone()),
            session_id: None,
            run_id: None,
            cancel: None,
        };

        let out = bash
            .execute_streaming(ctx, json!({"command": format!("python3 {}", script.display()), "pty": true, "timeout_secs": 10}))
            .await
            .unwrap();

        let chunks: Vec<String> = progress.0.lock().unwrap().clone();
        eprintln!("=== PTY progress chunks ({}) ===", chunks.len());
        for (i, c) in chunks.iter().enumerate() {
            eprintln!("  chunk[{i}]: {:?}", c);
        }
        // PTY 下应收到多个 chunk（不是等到结束才一次性吐）
        assert!(
            chunks.len() >= 2,
            "expected >=2 chunks, got {} chunks. Output: {out}",
            chunks.len()
        );
        assert!(out.contains("DONE"), "output should contain DONE: {out}");
    }

    #[tokio::test]
    async fn pty_git_diff_does_not_enter_pager() {
        let tmp = tempfile::tempdir().unwrap();
        let setup = std::process::Command::new("bash")
            .arg("-lc")
            .arg("git init >/dev/null && git config user.email test@example.com && git config user.name test && seq 1 200 > file.txt && git add file.txt && git commit -m init >/dev/null && seq 1 200 | sed 's/$/ changed/' > file.txt")
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(setup.success());

        let bash = tool(tmp.path());
        let started = std::time::Instant::now();
        let out = bash
            .execute(json!({
                "command": "git diff --stat && git diff --name-status --cached && git diff --stat --cached",
                "pty": true,
                "timeout_secs": 3
            }))
            .await
            .unwrap();

        assert!(started.elapsed() < Duration::from_secs(3), "output: {out}");
        assert!(!out.contains("Press RETURN to continue"), "output: {out}");
        assert!(!out.contains("已转后台"), "output: {out}");
        assert!(out.contains("file.txt"), "output: {out}");
    }

    #[test]
    fn clean_ansi_progress_strips_cursor_and_merges_cr() {
        // 模拟 docker pull 的典型输出：
        // line1 → ESC[1A(回上行) ESC[2K(清行) \r(回行首) line1-updated → line2 \r merged
        // 光标移动 ESC[1A 的"回上行覆盖"语义在纯文本处理中无法精确模拟（需要终端仿真器），
        // 因此旧行和新行会共存——但至少去掉了乱码 ANSI 转义序列、合并了 \r 回行。
        let raw = "line1\n\x1b[1A\x1b[2K\rline1-updated\nline2\rmerged\n";
        let cleaned = super::clean_ansi_progress(raw);
        assert_eq!(cleaned, "line1\nline1-updated\nmerged");
    }
}
