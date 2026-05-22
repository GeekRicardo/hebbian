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

use super::background::{BackgroundShells, ShellState, READ_CHUNK_BYTES};
use super::{Tool, ToolCtx};
use crate::workspace::Workspace;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_OUTPUT_BYTES: usize = 30_000;

pub struct BashTool {
    workspace: Arc<Workspace>,
    shells: BackgroundShells,
    /// 当前 session 的 bg 日志目录（架构 §4.12.3）。`None` 时 BackgroundShells
    /// 回落到 tail-only。CLI 单跑 / 单测一般传 None；desktop chat.rs 传
    /// `~/.hebbian/sessions/<sid>/bg`。
    bg_log_dir: Option<PathBuf>,
}

impl BashTool {
    pub fn new(
        workspace: Arc<Workspace>,
        shells: BackgroundShells,
        bg_log_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            workspace,
            shells,
            bg_log_dir,
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
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        self.execute_streaming(ToolCtx::noop(), input).await
    }

    async fn execute_streaming(&self, ctx: ToolCtx, input: Value) -> AppResult<String> {
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

        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .current_dir(&cwd);

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
            // 把 task-notification 注入下一轮 user message——不要求模型显式调 WaitForTask。
            arm_auto_notification(&ctx, &shell.task_id);
            let mut text = format!(
                "[bash] 已在后台启动：task_id={} cmd=`{}`\n",
                shell.task_id, command
            );
            if let Some(p) = shell.log_path() {
                text.push_str(&format!("完整输出落盘到：{}\n", p.display()));
            }
            text.push_str(&format!(
                "用 BashOutput {{\"task_id\": \"{}\"}} 查询进度；完成时会自动通知你，无需 poll。",
                shell.task_id
            ));
            return Ok(text);
        }

        // 前台等待：要么进程退出，要么超时。等待期间持续抽 tail buffer 增量，
        // 通过 ctx.progress emit `ToolCallOutputDelta`——surface 端能在
        // ToolCallFinished 之前看到 stdout/stderr 实时刷出来。
        //
        // 关键约束：read_incremental 推进 cursor，再次调 read 不会拿到旧字节。
        // 所以我们本地 buffer 累加 forward 出去的内容，最后聚合返回。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
        let mut buffer = String::new();
        let exited = loop {
            // 终态或 deadline 到点都退出循环；中间每 ~200ms 抽一次增量。
            let tick = tokio::time::sleep_until(
                tokio::time::Instant::now() + Duration::from_millis(200),
            );
            tokio::select! {
                biased;
                _ = shell.wait_terminal() => {
                    drain_into(&shell, &ctx, &mut buffer, usize::MAX);
                    break true;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    drain_into(&shell, &ctx, &mut buffer, READ_CHUNK_BYTES);
                    break false;
                }
                _ = tick => {
                    drain_into(&shell, &ctx, &mut buffer, READ_CHUNK_BYTES);
                }
            }
        };

        if !exited {
            // 超时：进程仍在跑，转后台。
            shell.promote_to_background();
            // 与显式 run_in_background=true 同款自动 arm 通知：超时转后台后，task 终态
            // 也由 WakeupScheduler 主动通知，模型不需要 poll。
            arm_auto_notification(&ctx, &shell.task_id);
            let mut text = format!(
                "[bash] 命令在 {timeout}s 内未结束，已转后台：task_id={}\n",
                shell.task_id
            );
            text.push_str(&format!(
                "继续用 BashOutput {{\"task_id\": \"{}\"}} 查询，或 KillShell 终止；完成时会自动通知。\n",
                shell.task_id
            ));
            if !buffer.is_empty() {
                text.push_str("--- 已产出 ---\n");
                text.push_str(&buffer);
            }
            return Ok(truncate_bytes(&text, MAX_OUTPUT_BYTES));
        }

        // 已退出：聚合 buffer + 终态后缀；从注册表摘掉前台条目。
        let final_state = shell.state();
        let text = format_finished(&buffer, &final_state);
        self.shells.unregister(&shell.task_id);
        Ok(truncate_bytes(&text, MAX_OUTPUT_BYTES))
    }
}

/// 启动后台 task 后自动 arm 一个 WakeupScheduler 监听（架构 §4.12.5 修订）。
/// task 终态时投递 BgTaskFinished 事件，surface 据此把 task-notification 注入下一轮
/// user message——CC 2.1 同款"completed 自动通知"。携带 `tool_use_id` 让通知能反查
/// 到触发它的 tool_call。
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
        BashTool::new(workspace_at(path), BackgroundShells::new(), None)
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
    async fn nonzero_exit_includes_code() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tool(tmp.path())
            .execute(json!({"command": "exit 7"}))
            .await
            .unwrap();
        assert!(out.contains("[exit 7]"));
    }

    #[tokio::test]
    async fn timeout_transitions_to_background() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BackgroundShells::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None);
        let out = t
            .execute(json!({"command": "sleep 5", "timeout_secs": 1}))
            .await
            .unwrap();
        assert!(out.contains("已转后台"));
        assert!(out.contains("task_id=bash_"));
        // 注册表里应该能找到这个 task，且 is_background 已被 promote 翻为 true
        let tasks = shells.list();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].is_background(), "超时转后台后必须 is_background=true");
        // kill 它，避免测试结束后还有 sleep 进程
        let id = tasks[0].task_id.clone();
        shells.kill(&id).await;
    }

    /// 前台命令正常退出后，BashTool 应该把自己从 BackgroundShells 摘掉——
    /// 否则 surface 会把 "ls" 也展示为 "已结束的后台任务"。
    #[tokio::test]
    async fn foreground_exit_unregisters_from_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BackgroundShells::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None);
        let out = t
            .execute(json!({"command": "echo hello"}))
            .await
            .unwrap();
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
        let shells = BackgroundShells::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None);
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
        let shells = BackgroundShells::new();
        let bash = BashTool::new(workspace_at(tmp.path()), shells.clone(), None);
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
    async fn run_in_background_returns_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BackgroundShells::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone(), None);
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
        let shells = BackgroundShells::new();
        let bash = BashTool::new(workspace_at(tmp.path()), shells.clone(), None);
        let progress = Arc::new(CaptureProgress {
            chunks: Mutex::new(Vec::new()),
        });
        let ctx = crate::tools::ToolCtx {
            call_id: "test_call".into(),
            progress: Some(progress.clone()),
            session_id: None,
            run_id: None,
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
        let shells = BackgroundShells::new();
        let bash = BashTool::new(workspace_at(tmp.path()), shells, None);
        let progress = Arc::new(CaptureProgress(Mutex::new(Vec::new())));
        let ctx = crate::tools::ToolCtx {
            call_id: "test_call".into(),
            progress: Some(progress.clone()),
            session_id: None,
            run_id: None,
        };

        let out = bash
            .execute_streaming(ctx, json!({"command": "echo quick"}))
            .await
            .unwrap();
        assert!(out.contains("quick"));
    }
}
