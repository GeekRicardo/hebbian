//! ReadHashlineTool — Hashline 后端的 Read 工具。
//!
//! 与 ReadTool 行为完全一致，唯一区别是输出格式：
//! - ReadTool 输出 cat -n 风格（`    5\tline`）
//! - 本工具输出 hashline 风格（`¶path#HASH\n1:line\n2:line\n`）
//!
//! 超长行截断、分页、大文件拒绝、ReadStateTracker 更新均与 ReadTool 保持一致。

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};
use tokio::fs;

use super::Tool;
use crate::edits::hashline::format::hash3;
use crate::read_state::ReadStateTracker;

const DEFAULT_LIMIT: usize = 2_000;
const MAX_LINE_LENGTH: usize = 2_000;
const MAX_OUTPUT_BYTES: usize = 100_000;
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

pub struct ReadHashlineTool {
    data_dir: Option<PathBuf>,
    session_id: Option<String>,
    tracker: Option<Arc<ReadStateTracker>>,
}

impl ReadHashlineTool {
    pub fn new(
        data_dir: Option<PathBuf>,
        session_id: Option<String>,
        tracker: Option<Arc<ReadStateTracker>>,
    ) -> Self {
        Self {
            data_dir,
            session_id,
            tracker,
        }
    }

    fn record_read(&self, path: &Path, raw_bytes: &[u8], mtime_ms: i64) {
        let Some(tracker) = self.tracker.as_ref() else {
            return;
        };
        let mut hasher = DefaultHasher::new();
        raw_bytes.hash(&mut hasher);
        tracker.record(path, hasher.finish(), mtime_ms);
    }

    fn save_line_remainder(
        &self,
        file_path: &str,
        line_no: usize,
        remainder: &str,
    ) -> Option<String> {
        let (data_dir, session_id) = match (self.data_dir.as_ref(), self.session_id.as_deref()) {
            (Some(dd), Some(sid)) => (dd, sid),
            _ => return None,
        };

        let mut hasher = DefaultHasher::new();
        file_path.hash(&mut hasher);
        let file_hash = hasher.finish();

        let dir = data_dir
            .join("sessions")
            .join(session_id)
            .join("line_trunc");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(?dir, error = %e, "ReadHashline: 创建 line_trunc 目录失败");
            return None;
        }

        let path = dir.join(format!("{:016x}_L{}.txt", file_hash, line_no));
        let mut wrapped =
            String::with_capacity(remainder.len() + remainder.len() / MAX_LINE_LENGTH);
        let mut chars = remainder.chars();
        loop {
            let chunk: String = chars.by_ref().take(MAX_LINE_LENGTH).collect();
            if chunk.is_empty() {
                break;
            }
            wrapped.push_str(&chunk);
            wrapped.push('\n');
        }
        match std::fs::write(&path, &wrapped) {
            Ok(()) => Some(path.display().to_string()),
            Err(e) => {
                tracing::warn!(?path, error = %e, "ReadHashline: 落盘超长行剩余失败");
                None
            }
        }
    }
}

#[async_trait]
impl Tool for ReadHashlineTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "读取本地文件内容（绝对路径）。返回 hashline 格式：\
         第一行 `¶<path>#<HASH>`，正文 `N:line`（1-based 行号）。\
         HASH 是当前内容的 3-hex 指纹，Edit 工具会校验。\
         默认从第 1 行起最多读 2000 行；用 offset/limit 翻页读大文件。\
         超长行（>2000 字符）截断，剩余部分落盘；整体输出超 ~6KB 时截断，请用 offset/limit 翻页。\
         路径必须在对话允许的路径范围内。"
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

        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.record_read(&file_path, &content, mtime_ms);

        let text = String::from_utf8_lossy(&content);
        let content_hash = hash3(&text);

        // hashline 头：¶<path>#<HASH>
        // offset/limit 分页时，头部仍包含完整文件的 hash（让模型总能凭此 Edit）
        let header = format!("¶{}#{}\n", file_path_str, content_hash);
        let mut out = header;

        let mut total_lines = 0usize;
        let mut displayed_lines = 0usize;

        for (idx, line) in text.lines().enumerate() {
            total_lines = idx + 1;
            if total_lines < offset || total_lines >= offset + limit {
                continue;
            }

            let formatted = if line.len() > MAX_LINE_LENGTH {
                let visible = &line[..MAX_LINE_LENGTH];
                let remainder = &line[MAX_LINE_LENGTH..];
                let remainder_len = remainder.len();
                match self.save_line_remainder(file_path_str, total_lines, remainder) {
                    Some(saved_path) => format!(
                        "{}…[截断，剩余 {} 字符已落盘 {saved_path}]",
                        visible, remainder_len
                    ),
                    None => format!("{}…[截断，剩余 {} 字符]", visible, remainder_len),
                }
            } else {
                line.to_string()
            };

            // hashline 格式：N:line（而非 cat -n 的     N\tline）
            let line_str = format!("{}:{}\n", total_lines, formatted);

            if out.len() + line_str.len() > MAX_OUTPUT_BYTES && displayed_lines > 0 {
                let remaining_lines = text.lines().count().saturating_sub(total_lines);
                write!(
                    out,
                    "\n[输出截断：已显示 {displayed_lines} 行。\
                     后续约 {remaining_lines} 行未显示。\
                     请用 offset/limit 翻页读取（当前 offset={offset} limit={limit}）。]"
                )
                .ok();
                return Ok(out);
            }

            out.push_str(&line_str);
            displayed_lines += 1;
        }

        if displayed_lines == 0 {
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
    use crate::edits::hashline::format::hash3;

    fn make_tool() -> ReadHashlineTool {
        ReadHashlineTool::new(None, None, None)
    }

    #[tokio::test]
    async fn outputs_hashline_header_and_numbered_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("foo.txt");
        std::fs::write(&file, "alpha\nbeta\n").unwrap();
        let out = make_tool()
            .execute(json!({"file_path": file.to_string_lossy()}))
            .await
            .unwrap();
        assert!(out.starts_with('¶'), "must start with ¶: {out}");
        assert!(out.contains("\n1:alpha\n"));
        assert!(out.contains("\n2:beta\n"));
    }

    #[tokio::test]
    async fn hash_matches_format_hash3() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("foo.txt");
        let content = "line\n";
        std::fs::write(&file, content).unwrap();
        let out = make_tool()
            .execute(json!({"file_path": file.to_string_lossy()}))
            .await
            .unwrap();
        let expected = hash3(content);
        assert!(
            out.contains(&format!("#{expected}\n")),
            "header must contain #{expected}: {out}"
        );
    }

    #[tokio::test]
    async fn respects_offset_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        let content: String = (1..=5).map(|i| format!("line{i}\n")).collect();
        std::fs::write(&file, &content).unwrap();
        let out = make_tool()
            .execute(json!({"file_path": file.to_string_lossy(), "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert!(out.contains("\n2:line2\n"));
        assert!(out.contains("\n3:line3\n"));
        assert!(!out.contains(":line1"));
        assert!(!out.contains(":line4"));
    }

    #[tokio::test]
    async fn original_read_tool_still_works() {
        // 确认 ReadTool (cat-n) 没有被影响
        use crate::tools::read::ReadTool;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("b.txt");
        std::fs::write(&file, "x\ny\n").unwrap();
        let tool = ReadTool::new(None, None, None);
        let out = tool
            .execute(json!({"file_path": file.to_string_lossy()}))
            .await
            .unwrap();
        assert!(out.contains("    1\tx"));
        assert!(out.contains("    2\ty"));
        assert!(!out.contains('¶'));
    }
}
