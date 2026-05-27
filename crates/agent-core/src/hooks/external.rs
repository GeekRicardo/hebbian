//! 外部 hook：通过子进程 + stdin/stdout JSON 协议接入用户脚本（架构 §4.8.2 / §4.8.4）。
//!
//! 协议（与 CodeIsland 兼容；Stop 语义与 Claude Code / Codex 对齐）：
//! - hook 命令从 stdin 读一行 JSON（含 event / context）
//! - hook 命令往 stdout 写一行 JSON 响应 `{ outcome: continue|modify|block|inject, patch?, reason?, reminder? }`
//! - 超时视为 Continue（默认 5s，Stop 类 verify 一般配置 30-60）
//! - Stop 点位的 shell 风格降级：脚本不输出 JSON 但 exit != 0 + 有 stdout → 自动构造 inject + stdout 作为 reminder
//!
//! 配置文件 `~/.hebbian/hooks.json` 形如：
//! ```json
//! {
//!   "PreToolUse": [
//!     { "matcher": { "tool": "Bash" }, "command": "python ~/.hebbian/hooks/bash-guard.py" }
//!   ],
//!   "Stop": [
//!     { "command": "cargo check --workspace 2>&1 | tail -50", "mode": "sync", "timeout_secs": 60 }
//!   ]
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};

use super::{Hook, HookOutcome, HookPatch, HookPoint};

const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Hook 执行模式（架构 §4.8.2，对齐 Codex `HookExecutionMode`）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookExecMode {
    /// 阻塞 await stdout，最长 `timeout_secs`。Stop 后置 verify 必须用这个。
    #[default]
    Sync,
    /// Fire-and-forget：spawn 子进程不读 stdout 不等结果，立即返回 Continue。
    /// 适合审计 / 通知 / 上报类，不影响主流程。
    Async,
}

/// 外部 hook 的匹配规则：按工具名 / 点位过滤是否调用。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookMatcher {
    /// 工具名匹配（仅 Pre/PostToolUse / PermissionRequest 有效）。`"*"` = 全部。
    #[serde(default)]
    pub tool: Option<String>,
}

impl HookMatcher {
    fn matches(&self, point: &HookPoint) -> bool {
        let Some(ref pattern) = self.tool else {
            return true;
        };
        if pattern == "*" {
            return true;
        }
        let tool_name = match point {
            HookPoint::PreToolUse { tool_name, .. }
            | HookPoint::PostToolUse { tool_name, .. }
            | HookPoint::PostToolUseFailure { tool_name, .. }
            | HookPoint::PermissionRequest { tool_name, .. } => tool_name.as_str(),
            _ => return true, // 非 tool 类点位忽略 tool matcher
        };
        tool_name.eq_ignore_ascii_case(pattern)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRule {
    #[serde(default)]
    pub matcher: HookMatcher,
    pub command: String,
    /// 执行模式（默认 Sync）。架构 §4.8.2。
    #[serde(default)]
    pub mode: HookExecMode,
    /// 单条 hook 的超时（秒）。缺省 = [`DEFAULT_TIMEOUT_SECS`]。
    /// Stop 类后置 verify 一般配 30-60。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// `~/.hebbian/hooks.json` 解析结果：点位 → 规则列表。
pub type HookConfig = HashMap<String, Vec<HookRule>>;

/// 加载 hook 配置：全局层 + 项目层追加合并（架构 §4.8.2）。
///
/// - 全局：`<data_dir>/hooks.json` —— 所有 session 共享
/// - 项目：`<data_dir>/projects/<encode(workdir)>/hooks.json` —— 仅当 `workdir`
///   传入且对应项目目录存在时加载，与决策 §6.1（项目配置目录化）对齐
///
/// 合并语义：**逐点位追加**（与 PermissionRule 同样的"层叠 + 全部生效"模型）。
/// 同一点位下，全局规则先跑、项目规则后跑——HookManager 第一个非 Continue 胜出，
/// 所以"项目想优先决策"在前缀里挂；"项目想做补充检查"挂在后面。
///
/// 任一层缺失或解析失败都仅 warn，不报错（不阻塞 session 启动）。
pub fn load_hooks_config(data_dir: &Path, workdir: Option<&Path>) -> HookConfig {
    let mut merged = load_hooks_file(&data_dir.join("hooks.json"));
    if let Some(wd) = workdir {
        let project_path = data_dir
            .join("projects")
            .join(crate::storage::projects::encode_workdir(wd))
            .join("hooks.json");
        let project_cfg = load_hooks_file(&project_path);
        for (event, rules) in project_cfg {
            merged.entry(event).or_default().extend(rules);
        }
    }
    merged
}

fn load_hooks_file(path: &Path) -> HookConfig {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return HookConfig::new(),
    };
    match serde_json::from_str(&raw) {
        Ok(cfg) => cfg,
        Err(e) => {
            warn!(error = %e, path = %path.display(), "hooks.json parse failed, ignoring");
            HookConfig::new()
        }
    }
}

/// 外部 hook 实例：单个配置规则，按点位类型分发。
pub struct ExternalHook {
    name: String,
    rules: Vec<HookRule>,
}

impl ExternalHook {
    pub fn from_config(config: HookConfig) -> Vec<Box<dyn Hook>> {
        let mut hooks: Vec<Box<dyn Hook>> = Vec::new();
        for (event, rules) in config {
            hooks.push(Box::new(ExternalHook { name: event, rules }));
        }
        hooks
    }

    async fn run_one(&self, point: &HookPoint, rule: &HookRule) -> Option<HookOutcome> {
        if !rule.matcher.matches(point) {
            return None;
        }
        let payload = serde_json::json!({
            "event": point.event_name(),
            "context": describe_point(point),
        });
        let payload_line = format!("{payload}\n");

        // 解析 command 用 shell 风格切分
        let mut parts = rule.command.split_whitespace();
        let program = parts.next()?;
        let args: Vec<String> = parts.map(str::to_string).collect();

        // 把当前点位携带的 workdir 设为子进程 cwd——让 `cargo check` / `pnpm tsc`
        // 这类相对路径命令在用户项目根目录跑（架构 §4.8.2）。
        let cwd = point_workdir(point);

        // Async 模式：fire-and-forget。spawn 后立刻返回 Continue，子进程在后台跑完即被
        // tokio 回收；stdout / 退出码不读取（即便挂 inject 也不会注入，符合 §4.8.2 语义）。
        if rule.mode == HookExecMode::Async {
            let program = program.to_string();
            let cmd_line = rule.command.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move {
                let mut cmd = Command::new(&program);
                cmd.args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if let Some(ref dir) = cwd {
                    cmd.current_dir(dir);
                }
                match cmd.spawn() {
                    Ok(mut child) => {
                        if let Some(mut stdin) = child.stdin.take() {
                            let _ = stdin.write_all(payload_line.as_bytes()).await;
                            drop(stdin);
                        }
                        let _ = child.wait().await;
                    }
                    Err(e) => warn!(error = %e, command = %cmd_line, "async hook spawn failed"),
                }
            });
            return None;
        }

        let timeout_secs = rule.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let mut cmd = Command::new(program);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, command = %rule.command, "hook spawn failed");
                return None;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload_line.as_bytes()).await;
            drop(stdin);
        }
        let mut stdout_buf = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_end(&mut stdout_buf).await;
        }
        let mut stderr_buf = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_end(&mut stderr_buf).await;
        }
        let wait = timeout(Duration::from_secs(timeout_secs), child.wait()).await;
        let status_success = match wait {
            Ok(Ok(status)) => status.success(),
            Ok(Err(e)) => {
                warn!(error = %e, command = %rule.command, "hook wait failed");
                return None;
            }
            Err(_) => {
                warn!(timeout_s = timeout_secs, command = %rule.command, "hook timed out");
                let _ = child.kill().await;
                return None;
            }
        };

        // 优先尝试 JSON 响应解析（结构化协议）。
        let raw = String::from_utf8_lossy(&stdout_buf);
        let first_json_line = raw
            .lines()
            .find(|l| l.trim().starts_with('{'))
            .map(str::trim);
        if let Some(line) = first_json_line {
            if let Ok(resp) = serde_json::from_str::<serde_json::Value>(line) {
                return parse_json_outcome(&resp, point);
            }
        }

        // Shell 风格降级：仅 Stop 点位，exit != 0 + 有 stdout/stderr → inject。
        // 兼容直接挂 `cargo check 2>&1 | tail -50` 这种"哑脚本"，不强迫用户写 JSON。
        if !status_success && matches!(point, HookPoint::Stop { .. }) {
            let combined = if !stdout_buf.is_empty() {
                String::from_utf8_lossy(&stdout_buf).into_owned()
            } else {
                String::from_utf8_lossy(&stderr_buf).into_owned()
            };
            let trimmed = combined.trim();
            if !trimmed.is_empty() {
                return Some(HookOutcome::InjectFollowup(trimmed.to_string()));
            }
        }
        if !status_success {
            warn!(command = %rule.command, "hook exited non-zero with no parseable response");
        }
        None
    }
}

fn parse_json_outcome(resp: &serde_json::Value, point: &HookPoint) -> Option<HookOutcome> {
    let outcome = resp
        .get("outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("continue");
    match outcome {
        "block" => {
            let reason = resp
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("hook blocked")
                .to_string();
            Some(HookOutcome::Block(reason))
        }
        "modify" => {
            // 完整 patch 协议（架构 §4.8.2）：从 resp.patch 解析 input / result /
            // system_prefix 三个可选字段，dispatcher 按点位拿对应字段。
            let patch_value = resp
                .get("patch")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let patch = HookPatch {
                input: patch_value.get("input").cloned().filter(|v| !v.is_null()),
                result: patch_value
                    .get("result")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                system_prefix: patch_value
                    .get("system_prefix")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            };
            if patch.input.is_none() && patch.result.is_none() && patch.system_prefix.is_none() {
                debug!(event = %point.event_name(), "hook modify with empty patch — treated as continue");
                None
            } else {
                Some(HookOutcome::Modify(patch))
            }
        }
        "inject" => {
            // InjectFollowup 仅 Stop 点位由 agent_loop 消费（架构 §4.8.3）。
            // 其他点位若返回 inject 会被 HookManager::trigger 当作非 Continue 早出，
            // 但消费方不消费 → 行为上等同 Continue，不会有副作用，仅记一条 debug。
            if !matches!(point, HookPoint::Stop { .. }) {
                debug!(
                    event = %point.event_name(),
                    "hook returned inject on non-Stop point — will be ignored by agent_loop",
                );
            }
            let reminder = resp
                .get("reminder")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if reminder.is_empty() {
                None
            } else {
                Some(HookOutcome::InjectFollowup(reminder))
            }
        }
        _ => None, // continue
    }
}

#[async_trait]
impl Hook for ExternalHook {
    fn name(&self) -> &str {
        &self.name
    }

    async fn invoke(&self, point: &HookPoint) -> HookOutcome {
        // 仅在点位名匹配本 hook 的 event 时调
        if self.name != point.event_name() {
            return HookOutcome::Continue;
        }
        for rule in &self.rules {
            if let Some(outcome) = self.run_one(point, rule).await {
                if !matches!(outcome, HookOutcome::Continue) {
                    return outcome;
                }
            }
        }
        HookOutcome::Continue
    }
}

/// 当前点位若携带 workdir，返回它——子进程 spawn 时设为 cwd。
/// 仅 Stop 点位有 workdir 字段（架构 §4.8.2）；其它点位继承 daemon 启动目录。
fn point_workdir(point: &HookPoint) -> Option<std::path::PathBuf> {
    match point {
        HookPoint::Stop {
            workdir: Some(w), ..
        } => Some(std::path::PathBuf::from(w)),
        _ => None,
    }
}

fn describe_point(point: &HookPoint) -> serde_json::Value {
    use serde_json::json;
    match point {
        HookPoint::SessionStart {
            session_id,
            workdir,
        } => json!({
            "session_id": session_id,
            "workdir": workdir,
        }),
        HookPoint::SessionEnd { session_id } => json!({ "session_id": session_id }),
        HookPoint::UserPromptSubmit { session_id, text } => json!({
            "session_id": session_id,
            "text": text,
        }),
        HookPoint::PreToolUse {
            session_id,
            tool_name,
            input,
        } => json!({
            "session_id": session_id,
            "tool": tool_name,
            "input": input,
        }),
        HookPoint::PostToolUse {
            session_id,
            tool_name,
            result,
        } => json!({
            "session_id": session_id,
            "tool": tool_name,
            "result": result,
        }),
        HookPoint::PostToolUseFailure {
            session_id,
            tool_name,
            error,
        } => json!({
            "session_id": session_id,
            "tool": tool_name,
            "error": error,
        }),
        HookPoint::PermissionRequest {
            session_id,
            tool_name,
            input,
        } => json!({
            "session_id": session_id,
            "tool": tool_name,
            "input": input,
        }),
        HookPoint::PreCompact {
            session_id,
            strategy,
        } => json!({
            "session_id": session_id,
            "strategy": strategy,
        }),
        HookPoint::PostCompact {
            session_id,
            before_tokens,
            after_tokens,
        } => json!({
            "session_id": session_id,
            "before_tokens": before_tokens,
            "after_tokens": after_tokens,
        }),
        HookPoint::Notification {
            session_id,
            level,
            message,
        } => json!({
            "session_id": session_id,
            "level": level,
            "message": message,
        }),
        HookPoint::Stop {
            session_id,
            reason,
            workdir,
        } => json!({
            "session_id": session_id,
            "reason": reason,
            "workdir": workdir,
        }),
        // 内置 4 点（非外部 hook 关心，但为完整性提供）
        HookPoint::BeforeModelCall { turn } => json!({ "turn": turn }),
        HookPoint::OnPermissionCheck { tool_name, input } => json!({
            "tool": tool_name,
            "input": input,
        }),
        HookPoint::OnToolResult { tool_name, content } => json!({
            "tool": tool_name,
            "content_len": content.len(),
        }),
        HookPoint::OnCompaction {
            before_tokens,
            after_tokens,
        } => json!({
            "before_tokens": before_tokens,
            "after_tokens": after_tokens,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stop_point() -> HookPoint {
        HookPoint::Stop {
            session_id: "sid".into(),
            reason: "end_turn".into(),
            workdir: None,
        }
    }

    #[test]
    fn inject_outcome_parses_reminder_on_stop_point() {
        let resp = json!({ "outcome": "inject", "reminder": "cargo check failed: E0308" });
        match parse_json_outcome(&resp, &stop_point()) {
            Some(HookOutcome::InjectFollowup(s)) => assert_eq!(s, "cargo check failed: E0308"),
            other => panic!("expected InjectFollowup, got {other:?}"),
        }
    }

    #[test]
    fn inject_outcome_with_empty_reminder_falls_through_to_continue() {
        let resp = json!({ "outcome": "inject", "reminder": "   " });
        assert!(parse_json_outcome(&resp, &stop_point()).is_none());
    }

    #[test]
    fn block_outcome_propagates_reason() {
        let resp = json!({ "outcome": "block", "reason": "policy violation" });
        match parse_json_outcome(&resp, &stop_point()) {
            Some(HookOutcome::Block(s)) => assert_eq!(s, "policy violation"),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn unknown_outcome_treated_as_continue() {
        let resp = json!({ "outcome": "ignore-me" });
        assert!(parse_json_outcome(&resp, &stop_point()).is_none());
    }

    #[test]
    fn hook_rule_defaults_to_sync_mode_and_no_timeout_override() {
        let raw = r#"{ "command": "cargo check" }"#;
        let rule: HookRule = serde_json::from_str(raw).expect("parse");
        assert_eq!(rule.mode, HookExecMode::Sync);
        assert!(rule.timeout_secs.is_none());
        assert_eq!(rule.command, "cargo check");
    }

    #[test]
    fn hook_rule_async_mode_parses() {
        let raw = r#"{ "command": "audit.py", "mode": "async", "timeout_secs": 30 }"#;
        let rule: HookRule = serde_json::from_str(raw).expect("parse");
        assert_eq!(rule.mode, HookExecMode::Async);
        assert_eq!(rule.timeout_secs, Some(30));
    }

    /// 全局 + 项目层 hooks.json 追加合并（架构 §4.8.2 / §6.1）：
    /// 同一点位下两层规则都加载，全局先、项目后。
    #[test]
    fn load_hooks_config_merges_global_and_project_layers() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let data_dir = dir.path();

        std::fs::write(
            data_dir.join("hooks.json"),
            r#"{
                "Stop": [{ "command": "/g/verify-rust.sh", "timeout_secs": 60 }],
                "PreToolUse": [{ "command": "/g/audit.sh", "mode": "async" }]
            }"#,
        )
        .unwrap();

        let workdir = data_dir.join("ws");
        std::fs::create_dir_all(&workdir).unwrap();
        let project_dir = data_dir
            .join("projects")
            .join(crate::storage::projects::encode_workdir(&workdir));
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("hooks.json"),
            r#"{
                "Stop": [{ "command": "/p/run-e2e.sh", "timeout_secs": 120 }]
            }"#,
        )
        .unwrap();

        let cfg = load_hooks_config(data_dir, Some(&workdir));
        let stop = cfg.get("Stop").expect("Stop merged");
        assert_eq!(stop.len(), 2, "global + project Stop hooks both kept");
        assert_eq!(stop[0].command, "/g/verify-rust.sh");
        assert_eq!(stop[1].command, "/p/run-e2e.sh");
        let pre = cfg.get("PreToolUse").expect("PreToolUse kept");
        assert_eq!(
            pre.len(),
            1,
            "project layer not interfering with other events"
        );
    }

    /// workdir = None 时仅读全局层（兼容旧 surface 调用 / 测试场景）。
    #[test]
    fn load_hooks_config_without_workdir_only_reads_global() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let data_dir = dir.path();
        std::fs::write(
            data_dir.join("hooks.json"),
            r#"{ "Stop": [{ "command": "/g/x.sh" }] }"#,
        )
        .unwrap();
        // 即使项目目录存在也不读
        let project_dir = data_dir.join("projects").join("-ws");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("hooks.json"),
            r#"{ "Stop": [{ "command": "/p/x.sh" }] }"#,
        )
        .unwrap();

        let cfg = load_hooks_config(data_dir, None);
        assert_eq!(cfg.get("Stop").unwrap().len(), 1);
        assert_eq!(cfg.get("Stop").unwrap()[0].command, "/g/x.sh");
    }

    /// 端到端：真 spawn `sh -c` 输出 JSON inject，HookManager 在 Stop 点位拿到
    /// InjectFollowup。证明子进程协议 + 解析 + 派发链路完整可用。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_hook_inject_outcome_propagates_via_hook_manager() {
        // printf 走 PATH；JSON 不含空格 → split_whitespace 切出来仍是合法 JSON 一行。
        let cfg: HookConfig = serde_json::from_value(json!({
            "Stop": [{
                "command": "printf {\"outcome\":\"inject\",\"reminder\":\"cargo_check_failed\"}",
                "mode": "sync",
                "timeout_secs": 5
            }]
        }))
        .expect("parse cfg");
        let hooks = ExternalHook::from_config(cfg);
        let mgr = crate::hooks::HookManager::new(hooks);
        let outcome = mgr
            .trigger(&HookPoint::Stop {
                session_id: "sid".into(),
                reason: "end_turn".into(),
                workdir: None,
            })
            .await;
        match outcome {
            HookOutcome::InjectFollowup(s) => assert_eq!(s, "cargo_check_failed"),
            other => panic!("expected InjectFollowup, got {other:?}"),
        }
    }

    /// HookPoint::Stop 携带 workdir 时，子进程 cwd 设为它。
    /// 用 `pwd` 脚本验证：spawn 后 stdout 必须等于传入的 workdir。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_hook_sets_cwd_from_workdir() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let script = dir.path().join("dump-pwd.sh");
        std::fs::write(&script, "#!/bin/sh\npwd\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&script).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script, perm).unwrap();
        }
        let cfg: HookConfig = serde_json::from_value(json!({
            "Stop": [{
                "command": script.display().to_string(),
                "mode": "sync",
                "timeout_secs": 5
            }]
        }))
        .expect("parse cfg");
        let hooks = ExternalHook::from_config(cfg);
        let mgr = crate::hooks::HookManager::new(hooks);
        // 用 canonicalize 解 macOS 的 /tmp -> /private/tmp symlink。
        let expected = std::fs::canonicalize(dir.path()).expect("canonicalize");
        let outcome = mgr
            .trigger(&HookPoint::Stop {
                session_id: "sid".into(),
                reason: "end_turn".into(),
                workdir: Some(expected.display().to_string()),
            })
            .await;
        match outcome {
            HookOutcome::InjectFollowup(s) => {
                let got = std::fs::canonicalize(s.trim()).expect("canonicalize stdout");
                assert_eq!(got, expected, "cwd should equal workdir");
            }
            other => panic!("expected InjectFollowup with pwd output, got {other:?}"),
        }
    }

    /// Shell 风格降级：脚本不输出 JSON、exit != 0 + 有 stdout → 自动注入 stdout 作为 reminder。
    /// 模拟用户挂 `cargo check 2>&1 | tail -50` 这种"哑脚本"的形态。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_hook_shell_degraded_inject_on_nonzero_exit() {
        // split_whitespace 把 `sh -c '...'` 拆碎，所以走临时脚本文件 + chmod +x。
        let dir = tempfile::tempdir().expect("tmpdir");
        let script = dir.path().join("verify.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'cargo check: E0308 type mismatch'\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&script).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script, perm).unwrap();
        }
        let cfg: HookConfig = serde_json::from_value(json!({
            "Stop": [{
                "command": script.display().to_string(),
                "mode": "sync",
                "timeout_secs": 5
            }]
        }))
        .expect("parse cfg");
        let hooks = ExternalHook::from_config(cfg);
        let mgr = crate::hooks::HookManager::new(hooks);
        let outcome = mgr
            .trigger(&HookPoint::Stop {
                session_id: "sid".into(),
                reason: "end_turn".into(),
                workdir: None,
            })
            .await;
        match outcome {
            HookOutcome::InjectFollowup(s) => {
                assert!(s.contains("E0308"), "reminder should contain stdout: {s}")
            }
            other => panic!("expected InjectFollowup (shell-degraded), got {other:?}"),
        }
    }
}
