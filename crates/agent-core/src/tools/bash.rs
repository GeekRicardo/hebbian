//! Bash 工具：在用户机器上跑 shell 命令。
//!
//! - 分类按命令本身决定（[`Self::classify`]）：
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
use platform::{AppError, AppResult};
use protocol::RiskLevel;
use serde_json::{json, Value};
use tokio::process::Command;

use super::background::{BackgroundShells, ReadOutput, ShellState, READ_CHUNK_BYTES};
use super::{safe_commands, shell_parse, Tool, ToolClass};
use crate::workspace::Workspace;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
const MAX_OUTPUT_BYTES: usize = 30_000;

pub struct BashTool {
    workspace: Arc<Workspace>,
    shells: BackgroundShells,
}

impl BashTool {
    pub fn new(workspace: Arc<Workspace>, shells: BackgroundShells) -> Self {
        Self { workspace, shells }
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
                                    必须在对话允许的目录范围内。"
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
        let shell = self
            .shells
            .register(command.to_string(), cwd_str, child);

        if background {
            return Ok(format!(
                "[bash] 已在后台启动：task_id={} cmd=`{}`\n用 BashOutput {{\"task_id\": \"{}\"}} 查询进度。",
                shell.task_id, command, shell.task_id
            ));
        }

        // 前台等待：要么进程退出，要么超时。等待期间不抽 buffer——
        // 后台 reader task 一直在抽，最后我们直接 read_incremental 取 buffer。
        let exited = tokio::time::timeout(Duration::from_secs(timeout), shell.wait_terminal())
            .await
            .is_ok();

        if !exited {
            // 超时：进程仍在跑，转后台。
            let snapshot = shell.read_incremental(READ_CHUNK_BYTES);
            let mut text = format!(
                "[bash] 命令在 {timeout}s 内未结束，已转后台：task_id={}\n",
                shell.task_id
            );
            text.push_str(&format!(
                "继续用 BashOutput {{\"task_id\": \"{}\"}} 查询，或 KillShell 终止。\n",
                shell.task_id
            ));
            if !snapshot.content.is_empty() {
                text.push_str("--- 已产出 ---\n");
                text.push_str(&snapshot.content);
            }
            return Ok(truncate_bytes(&text, MAX_OUTPUT_BYTES));
        }

        // 已退出：抽全部 tail buffer 拼输出 + 退出码。
        let snapshot = shell.read_incremental(usize::MAX);
        Ok(truncate_bytes(
            &format_finished(&snapshot, shell.task_id.as_str()),
            MAX_OUTPUT_BYTES,
        ))
    }

    fn affected_paths(&self, input: &Value) -> Vec<PathBuf> {
        vec![self.workspace.resolve_cwd(input["cwd"].as_str())]
    }

    /// 用于命令级记忆的指纹：把第一段命令规范化成 `"root sub ..."` 形式（剥引号、
    /// 单空格连接），让 `git status -uno` 和 `git status README` 共用 `"git status"`
    /// 前缀。复合命令（含 `&&` `|` 等）取首段；解析失败 → 退回原始 command。
    fn permission_fingerprint(&self, input: &Value) -> Option<String> {
        let raw = input["command"].as_str()?.trim();
        if raw.is_empty() {
            return None;
        }
        match shell_parse::parse(raw) {
            Ok(parsed) if !parsed.commands.is_empty() => {
                Some(parsed.commands[0].argv.join(" "))
            }
            _ => Some(raw.to_string()),
        }
    }

    /// 解析命令文本，全部子命令安全且无危险结构 → ReadOnly（直接放行），
    /// 否则 Destructive（走审批）。解析失败一律按不安全处理。
    fn classify(&self, input: &Value) -> ToolClass {
        let destructive = ToolClass::Destructive {
            risk: RiskLevel::High,
        };
        let Some(line) = input["command"].as_str() else {
            return destructive;
        };
        let Ok(parsed) = shell_parse::parse(line) else {
            return destructive;
        };
        if parsed.dangerous || parsed.commands.is_empty() {
            return destructive;
        }
        if parsed.commands.iter().all(safe_commands::is_safe) {
            ToolClass::ReadOnly
        } else {
            destructive
        }
    }
}

fn format_finished(snapshot: &ReadOutput, task_id: &str) -> String {
    let mut text = String::new();
    if !snapshot.content.is_empty() {
        text.push_str(&snapshot.content);
    }
    if snapshot.bytes_dropped > 0 {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!(
            "[警告] 因 buffer 上限丢失开头 {} 字节",
            snapshot.bytes_dropped
        ));
    }
    let suffix = match &snapshot.state {
        ShellState::Exited { code: Some(0) } => None,
        ShellState::Exited { code: Some(c) } => Some(format!("[exit {c}] task_id={task_id}")),
        ShellState::Exited { code: None } => {
            Some(format!("[terminated by signal] task_id={task_id}"))
        }
        ShellState::Killed => Some(format!("[killed] task_id={task_id}")),
        ShellState::Failed { error } => {
            Some(format!("[failed: {error}] task_id={task_id}"))
        }
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
        BashTool::new(workspace_at(path), BackgroundShells::new())
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
    async fn affected_paths_returns_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tool(tmp.path());
        let paths = t.affected_paths(&json!({"command": "ls"}));
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], tmp.path());

        let paths = t.affected_paths(&json!({"command": "ls", "cwd": "/etc"}));
        assert_eq!(paths[0], std::path::Path::new("/etc"));
    }

    #[tokio::test]
    async fn timeout_transitions_to_background() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BackgroundShells::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone());
        let out = t
            .execute(json!({"command": "sleep 5", "timeout_secs": 1}))
            .await
            .unwrap();
        assert!(out.contains("已转后台"));
        assert!(out.contains("task_id=bash_"));
        // 注册表里应该能找到这个 task
        let tasks = shells.list();
        assert_eq!(tasks.len(), 1);
        // kill 它，避免测试结束后还有 sleep 进程
        let id = tasks[0].task_id.clone();
        shells.kill(&id).await;
    }

    /// 端到端：Bash 超时 → 转后台 → BashOutput 增量查询 → KillShell 终止。
    #[tokio::test]
    async fn end_to_end_background_lifecycle() {
        use crate::tools::bash_output::BashOutputTool;
        use crate::tools::kill_shell::KillShellTool;

        let tmp = tempfile::tempdir().unwrap();
        let shells = BackgroundShells::new();
        let bash = BashTool::new(workspace_at(tmp.path()), shells.clone());
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
        let killed = kill
            .execute(json!({"task_id": task_id}))
            .await
            .unwrap();
        assert!(killed.contains("killed"));
    }

    #[tokio::test]
    async fn run_in_background_returns_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let shells = BackgroundShells::new();
        let t = BashTool::new(workspace_at(tmp.path()), shells.clone());
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

    fn class_of(line: &str) -> ToolClass {
        let tmp = tempfile::tempdir().unwrap();
        tool(tmp.path()).classify(&json!({"command": line}))
    }

    #[test]
    fn classify_ls_is_readonly() {
        assert!(matches!(class_of("ls -la"), ToolClass::ReadOnly));
    }

    #[test]
    fn classify_git_status_is_readonly() {
        assert!(matches!(class_of("git status -uno"), ToolClass::ReadOnly));
    }

    #[test]
    fn classify_pipe_of_safe_commands_is_readonly() {
        assert!(matches!(
            class_of("git log --oneline | head -5"),
            ToolClass::ReadOnly
        ));
    }

    #[test]
    fn classify_compound_with_unsafe_step_is_destructive() {
        assert!(matches!(
            class_of("cd foo && rm -rf bar"),
            ToolClass::Destructive { .. }
        ));
    }

    #[test]
    fn classify_redirection_is_destructive() {
        assert!(matches!(
            class_of("echo hi > /tmp/x"),
            ToolClass::Destructive { .. }
        ));
    }

    #[test]
    fn classify_command_substitution_is_destructive() {
        assert!(matches!(
            class_of("echo $(whoami)"),
            ToolClass::Destructive { .. }
        ));
    }

    #[test]
    fn classify_unknown_root_is_destructive() {
        assert!(matches!(
            class_of("./scripts/foo.sh"),
            ToolClass::Destructive { .. }
        ));
    }

    #[test]
    fn classify_malformed_input_is_destructive() {
        assert!(matches!(class_of("echo 'hi"), ToolClass::Destructive { .. }));
    }
}
