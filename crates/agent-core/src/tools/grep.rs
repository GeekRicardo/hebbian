//! Grep 工具：基于 ripgrep 在 workspace 范围内搜索文件内容。
//!
//! - read-only：默认 auto-approve
//! - 默认搜 workdir；可指定 `path`，但必须落在 workspace 内
//! - 默认 `files_with_matches` 模式；`output_mode: "content"` 显示匹配行

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time;

use super::Tool;
use crate::workspace::Workspace;

const MAX_OUTPUT_BYTES: usize = 30_000;
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct GrepTool {
    workspace: Arc<Workspace>,
}

impl GrepTool {
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "用 ripgrep 在文件内容中搜索正则。默认搜对话的 workdir。\
         output_mode 默认 \"files_with_matches\"，可选 \"content\" 显示匹配行（带行号）\
         或 \"count\" 显示每个文件的命中次数。\
         glob 用于按文件名过滤；type 用于按语言过滤（如 rust/py/ts）。\
         path 必须在对话允许的目录范围内。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "正则模式（ripgrep 语法）"
                },
                "path": {
                    "type": "string",
                    "description": "搜索路径（绝对路径）。不传则用对话 workdir。"
                },
                "glob": {
                    "type": "string",
                    "description": "文件名 glob，比如 \"*.rs\" 或 \"*.{ts,tsx}\""
                },
                "type": {
                    "type": "string",
                    "description": "ripgrep 语言类型，比如 rust / py / ts / go / js"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["files_with_matches", "content", "count"],
                    "description": "默认 files_with_matches"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "忽略大小写。默认 false。"
                },
                "head_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "限制输出前 N 行。默认 200。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| AppError::msg("Grep: 缺少 pattern"))?;

        let path = match input["path"].as_str().filter(|s| !s.is_empty()) {
            // 越界检查在 agent_loop 统一做
            Some(p) => PathBuf::from(p),
            None => self.workspace.workdir().to_path_buf(),
        };

        let mode = input["output_mode"]
            .as_str()
            .unwrap_or("files_with_matches");
        let case_insensitive = input["case_insensitive"].as_bool().unwrap_or(false);
        let head_limit = input["head_limit"].as_u64().unwrap_or(200) as usize;

        let mut args: Vec<String> = vec!["--hidden".into(), "--max-columns=500".into()];
        for vcs in [".git", ".svn", ".hg", "node_modules", "target"] {
            args.push("--glob".into());
            args.push(format!("!{vcs}"));
        }
        if case_insensitive {
            args.push("-i".into());
        }
        match mode {
            "files_with_matches" => args.push("-l".into()),
            "count" => args.push("-c".into()),
            "content" => args.push("-n".into()),
            other => return Err(AppError::msg(format!("Grep: 无效的 output_mode {other}"))),
        }
        if let Some(g) = input["glob"].as_str().filter(|s| !s.is_empty()) {
            args.push("--glob".into());
            args.push(g.into());
        }
        if let Some(t) = input["type"].as_str().filter(|s| !s.is_empty()) {
            args.push("--type".into());
            args.push(t.into());
        }
        // 模式以 - 开头时用 -e 防止被当成 flag
        if pattern.starts_with('-') {
            args.push("-e".into());
            args.push(pattern.into());
        } else {
            args.push(pattern.into());
        }
        args.push(path.to_string_lossy().into_owned());

        let mut cmd = Command::new("rg");
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        let output = match time::timeout(TIMEOUT, cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Err(AppError::msg(
                        "Grep: 未找到 ripgrep（rg）。请安装：\
                         macOS `brew install ripgrep`，Linux `apt install ripgrep`",
                    ));
                }
                return Err(AppError::msg(format!("Grep: 启动失败 {e}")));
            }
            Err(_) => return Err(AppError::msg("Grep: 搜索超时（30s）")),
        };

        // rg 退出码：0 = 有匹配；1 = 无匹配；2 = 错误
        let code = output.status.code().unwrap_or(-1);
        if code == 1 {
            return Ok("(无匹配)".into());
        }
        if code != 0 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::msg(format!("Grep: rg 退出码 {code}\n{stderr}")));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        let total = lines.len();
        let kept: Vec<&str> = lines.into_iter().take(head_limit).collect();
        let mut result = kept.join("\n");
        if total > head_limit {
            result.push_str(&format!(
                "\n…（共 {total} 行，已截到前 {head_limit} 行；调高 head_limit 看更多）"
            ));
        }
        Ok(truncate_bytes(&result, MAX_OUTPUT_BYTES))
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
    use std::io::Write;

    fn workspace_at(path: &std::path::Path) -> Arc<Workspace> {
        Workspace::new(path, Vec::new())
    }

    #[tokio::test]
    async fn finds_matching_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello world").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "nothing here").unwrap();

        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool.execute(json!({"pattern": "hello"})).await.unwrap();
        assert!(out.contains("a.txt"));
        assert!(!out.contains("b.txt"));
    }

    #[tokio::test]
    async fn content_mode_includes_line_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(tmp.path().join("a.txt")).unwrap();
        writeln!(f, "first").unwrap();
        writeln!(f, "needle here").unwrap();
        writeln!(f, "third").unwrap();

        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool
            .execute(json!({
                "pattern": "needle",
                "output_mode": "content",
            }))
            .await
            .unwrap();
        assert!(out.contains("2:needle here"));
    }

    #[tokio::test]
    async fn no_matches_returns_friendly_message() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "abc").unwrap();
        let tool = GrepTool::new(workspace_at(tmp.path()));
        let out = tool.execute(json!({"pattern": "xyzunique"})).await.unwrap();
        assert!(out.contains("无匹配"));
    }
}
