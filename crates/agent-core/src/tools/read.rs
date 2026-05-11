//! Read 工具：读取文件内容。
//!
//! - read-only：默认 auto-approve
//! - 行号前缀（cat -n 风格）便于后续编辑引用
//! - 支持 offset / limit 分页读大文件
//! - file_path 必须在 workspace 范围内

use std::path::PathBuf;
use std::sync::Arc;
// PathBuf 仍由 ReadTool::execute 内部使用，保留导入。

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};
use tokio::fs;

use super::Tool;
use crate::workspace::Workspace;

const DEFAULT_LIMIT: usize = 2_000;
const MAX_LINE_LENGTH: usize = 2_000;
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

pub struct ReadTool;

impl ReadTool {
    pub fn new(_workspace: Arc<Workspace>) -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "读取本地文件内容（绝对路径）。返回带行号前缀的文本（cat -n 风格）。\
         默认从第 1 行起最多读 2000 行；用 offset/limit 翻页读大文件。\
         超长行会截断到 2000 字符；文件过大（>5MB）会拒绝。\
         路径必须在对话允许的目录范围内。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path"],
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "文件的绝对路径"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "起始行号（1-based）。默认 1。"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "读取的行数。默认 2000。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let file_path_str = input["file_path"]
            .as_str()
            .ok_or_else(|| AppError::msg("Read: 缺少 file_path"))?;
        let file_path = PathBuf::from(file_path_str);
        // 越界检查在 agent_loop 统一做
        let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = input["limit"].as_u64().unwrap_or(DEFAULT_LIMIT as u64) as usize;

        let meta = fs::metadata(&file_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AppError::msg(format!("Read: 文件不存在 {file_path_str}"))
            } else {
                AppError::msg(format!("Read: stat 失败 {file_path_str}: {e}"))
            }
        })?;
        if meta.is_dir() {
            return Err(AppError::msg(format!(
                "Read: {file_path_str} 是目录，不是文件"
            )));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(AppError::msg(format!(
                "Read: 文件过大（{} 字节，>{}MB）。请用 offset/limit 或 Grep。",
                meta.len(),
                MAX_FILE_BYTES / 1024 / 1024
            )));
        }

        let content = fs::read(&file_path)
            .await
            .map_err(|e| AppError::msg(format!("Read: 读取失败 {file_path_str}: {e}")))?;
        let text = String::from_utf8_lossy(&content);

        let mut out = String::new();
        let mut total_lines = 0usize;
        for (idx, line) in text.lines().enumerate() {
            total_lines = idx + 1;
            if total_lines < offset || total_lines >= offset + limit {
                continue;
            }
            let trimmed = if line.len() > MAX_LINE_LENGTH {
                format!("{}…[行已截断]", &line[..MAX_LINE_LENGTH])
            } else {
                line.to_string()
            };
            out.push_str(&format!("{:>5}\t{}\n", total_lines, trimmed));
        }

        if out.is_empty() {
            return Ok(format!(
                "(文件共 {total_lines} 行；offset={offset} limit={limit} 范围内无内容)"
            ));
        }
        Ok(out)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn workspace_at(path: &std::path::Path) -> Arc<Workspace> {
        Workspace::new(path, Vec::new())
    }

    #[tokio::test]
    async fn read_returns_numbered_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "first\nsecond\n").unwrap();
        let tool = ReadTool::new(workspace_at(tmp.path()));

        let out = tool
            .execute(json!({"file_path": file.to_string_lossy()}))
            .await
            .unwrap();
        assert!(out.contains("    1\tfirst"));
        assert!(out.contains("    2\tsecond"));
    }

    #[tokio::test]
    async fn read_respects_offset_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        let mut f = std::fs::File::create(&file).unwrap();
        for i in 1..=10 {
            writeln!(f, "line{i}").unwrap();
        }
        let tool = ReadTool::new(workspace_at(tmp.path()));

        let out = tool
            .execute(json!({
                "file_path": file.to_string_lossy(),
                "offset": 5,
                "limit": 2,
            }))
            .await
            .unwrap();
        assert!(out.contains("    5\tline5"));
        assert!(out.contains("    6\tline6"));
        assert!(!out.contains("line4"));
        assert!(!out.contains("line7"));
    }

}
