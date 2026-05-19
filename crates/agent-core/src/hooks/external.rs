//! 外部 hook：通过子进程 + stdin/stdout JSON 协议接入用户脚本（架构 §4.8.2 / §4.8.3）。
//!
//! 协议（与 CodeIsland 兼容）：
//! - hook 命令从 stdin 读一行 JSON（含 event / session_id / 上下文字段）
//! - hook 命令往 stdout 写一行 JSON 响应 `{ outcome: continue|modify|block, patch?, reason? }`
//! - 超时（默认 5s）视为 Continue
//!
//! 配置文件 `~/.hebbian/hooks.json` 形如：
//! ```json
//! {
//!   "PreToolUse": [
//!     { "matcher": { "tool": "Bash" }, "command": "python ~/.hebbian/hooks/bash-guard.py" },
//!     { "matcher": { "tool": "*" }, "command": "python ~/.hebbian/hooks/audit.py" }
//!   ],
//!   "SessionStart": [...]
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
}

/// `~/.hebbian/hooks.json` 解析结果：点位 → 规则列表。
pub type HookConfig = HashMap<String, Vec<HookRule>>;

/// 加载 `~/.hebbian/hooks.json`，缺失或解析失败时返回空 config（不报错）。
pub fn load_hooks_config(data_dir: &Path) -> HookConfig {
    let path = data_dir.join("hooks.json");
    let raw = match std::fs::read_to_string(&path) {
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
        let args: Vec<&str> = parts.collect();

        let mut child = match Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
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
        let wait = timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS), child.wait()).await;
        match wait {
            Ok(Ok(status)) if status.success() => {}
            Ok(Ok(status)) => {
                warn!(?status, command = %rule.command, "hook exited non-zero");
                return None;
            }
            Ok(Err(e)) => {
                warn!(error = %e, command = %rule.command, "hook wait failed");
                return None;
            }
            Err(_) => {
                warn!(timeout_s = DEFAULT_TIMEOUT_SECS, command = %rule.command, "hook timed out");
                let _ = child.kill().await;
                return None;
            }
        }

        let raw = String::from_utf8_lossy(&stdout_buf);
        let first = raw.lines().find(|l| !l.trim().is_empty())?.trim();
        let resp: serde_json::Value = match serde_json::from_str(first) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, output = %first, "hook response parse failed");
                return None;
            }
        };
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
                // 完整 patch 协议（架构 §4.8.2 / §4.8.4）：从 resp.patch 解析 input /
                // result / system_prefix 三个可选字段，dispatcher 按点位拿对应字段。
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
                if patch.input.is_none() && patch.result.is_none() && patch.system_prefix.is_none()
                {
                    debug!(event = %point.event_name(), "hook modify with empty patch — treated as continue");
                    None
                } else {
                    Some(HookOutcome::Modify(patch))
                }
            }
            _ => None, // continue
        }
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
        HookPoint::Stop { session_id, reason } => json!({
            "session_id": session_id,
            "reason": reason,
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
