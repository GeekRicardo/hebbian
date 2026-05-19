//! Read 工具：读取文件内容。
//!
//! - read-only：默认 auto-approve
//! - 行号前缀（cat -n 风格）便于后续编辑引用
//! - 支持 offset / limit 分页读大文件
//! - 超长行截断到 ~2000 字符，剩余部分落盘到会话目录（按 ~2000 字符换行）
//! - 整体输出超 ~6KB 时截断，提示 agent 用 offset/limit 翻页（不落盘整份文件）
//! - file_path 必须在 workspace 范围内

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
use crate::read_state::ReadStateTracker;

const DEFAULT_LIMIT: usize = 2_000;
const MAX_LINE_LENGTH: usize = 2_000;
const MAX_OUTPUT_BYTES: usize = 6_000;
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

pub struct ReadTool {
    data_dir: Option<PathBuf>,
    session_id: Option<String>,
    /// session 级 Read 状态追踪表；Edit 工具据此校验"已读 + 未 stale"。
    /// CLI / 单测无 session 场景传 `None`，仅跳过追踪，工具行为不受影响。
    tracker: Option<Arc<ReadStateTracker>>,
}

impl ReadTool {
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

    /// 把超长行剩余部分落盘到 `<data_dir>/sessions/<sid>/line_trunc/<hash>_L<line>.txt`
    /// 并按 MAX_LINE_LENGTH 换行。
    fn save_line_remainder(
        &self,
        file_path: &str,
        line_no: usize,
        remainder: &str,
    ) -> Option<String> {
        let (data_dir, session_id) = match (self.data_dir.as_ref(), self.session_id.as_deref())
        {
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
            tracing::warn!(?dir, error = %e, "Read: 创建 line_trunc 目录失败");
            return None;
        }

        let path = dir.join(format!("{:016x}_L{}.txt", file_hash, line_no));

        // 按 MAX_LINE_LENGTH 换行
        let mut wrapped = String::with_capacity(remainder.len() + remainder.len() / MAX_LINE_LENGTH);
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
                tracing::warn!(?path, error = %e, "Read: 落盘超长行剩余失败");
                None
            }
        }
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
        let limit = input["limit"]
            .as_u64()
            .unwrap_or(DEFAULT_LIMIT as u64) as usize;

        // 文件大小硬上限：避免 agent 误读巨型二进制
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
        // 写 ReadStateTracker（架构 §4.4.10）：Edit 工具据此校验已读 + 未 stale。
        // 取磁盘 mtime 而非 now()——避免与 Edit 的"current mtime > 已读时戳"比对错乱。
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.record_read(&file_path, &content, mtime_ms);

        let text = String::from_utf8_lossy(&content);

        let mut out = String::new();
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
                    None => format!(
                        "{}…[截断，剩余 {} 字符]",
                        visible, remainder_len
                    ),
                }
            } else {
                line.to_string()
            };

            let line_str = format!("{:>5}\t{}\n", total_lines, formatted);

            // 整体输出超 6KB 截断 —— 不落盘，让 agent 自己调 offset/limit 翻页
            if out.len() + line_str.len() > MAX_OUTPUT_BYTES && !out.is_empty() {
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

    #[tokio::test]
    async fn read_returns_numbered_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "first\nsecond\n").unwrap();
        let tool = ReadTool::new(None, None, None);

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
        let tool = ReadTool::new(None, None, None);

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

    #[tokio::test]
    async fn long_line_truncated_with_remainder_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("hebbian");
        let sid = "test-session";
        let file = tmp.path().join("long.txt");
        let long = "a".repeat(3_000);
        std::fs::write(&file, format!("short\n{long}\nlast\n")).unwrap();

        let tool = ReadTool::new(Some(data_dir.clone()), Some(sid.into()), None);

        let out = tool
            .execute(json!({"file_path": file.to_string_lossy()}))
            .await
            .unwrap();

        // 短行完整
        assert!(out.contains("    1\tshort"));
        // 长行截断
        assert!(out.contains("…[截断，剩余 1000 字符已落盘"));
        assert!(!out.contains(&"a".repeat(3_000)));
        // 截断后行完整
        assert!(out.contains("    3\tlast"));

        // 落盘文件存在且按 2000 字符换行
        let trunc_dir = data_dir.join("sessions").join(sid).join("line_trunc");
        let saved: Vec<_> = std::fs::read_dir(&trunc_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(saved.len(), 1);
        let saved_content = std::fs::read_to_string(saved[0].path()).unwrap();
        assert_eq!(saved_content, format!("{}\n", "a".repeat(1_000)));
    }

    #[tokio::test]
    async fn output_capped_with_offset_limit_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("big.txt");
        let mut f = std::fs::File::create(&file).unwrap();
        for i in 1..=500 {
            writeln!(f, "line {i:04} {}", "x".repeat(40)).unwrap();
        }
        let tool = ReadTool::new(None, None, None);

        let out = tool
            .execute(json!({"file_path": file.to_string_lossy()}))
            .await
            .unwrap();

        // 输出应该被截断，有 offset/limit 提示
        assert!(out.contains("[输出截断"));
        assert!(out.contains("offset/limit"));
        // 不应该有已落盘的整份文件指针
        assert!(!out.contains("已落盘到"));
        // 至少显示了第一行
        assert!(out.contains("line 0001"));
    }
}
