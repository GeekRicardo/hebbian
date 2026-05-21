//! 读 `<sid>/model_io.jsonl`：把每行 [`DumpEntry`] 反序列化成 surface 友好的列表。
//!
//! 写盘走 [`crate::model_io_dump`]（actor 模式异步落盘）；读这边只走只读 IO，
//! 给前端 Model I/O 调试器、heb CLI `model-io` 子命令提供数据源。
//!
//! 容错策略：jsonl 中坏行（解析失败）只记 warn 并跳过，不让一个孤行毁了整次读取——
//! dump 是 best-effort 写入，可能因进程被 SIGKILL 留半截行；调试器要照常打开。

use std::path::Path;

use serde_json::Value;

use crate::model_io_dump::default_path;

/// 读 session 的所有 model_io 条目。
/// 文件不存在返回空 vec（默认开启后大部分新 session 都会有，但旧 session
/// 或刚创建未跑过 turn 的 session 没有）。
///
/// 每条 entry 是 `DumpEntry` 序列化后的 JSON 对象。给前端用 `Vec<Value>` 已经够——
/// 不强行把它套回 Rust struct 让 surface 自己挑字段渲染（避免在 storage 层
/// 维护一个跟 `DumpEntry` 重复的 DTO）。
pub fn read_session(data_dir: &Path, session_id: &str) -> std::io::Result<Vec<Value>> {
    let path = default_path(data_dir, session_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => out.push(v),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    line_idx = idx,
                    "model_io jsonl 行解析失败，跳过"
                );
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_returns_empty_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = read_session(tmp.path(), "missing-sid").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn read_parses_well_formed_lines_and_skips_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        let path = default_path(tmp.path(), "sid1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = format!(
            "{}\n{}\nnot json here\n{}\n",
            serde_json::to_string(&json!({"turn": 1, "model": "m1"})).unwrap(),
            serde_json::to_string(&json!({"turn": 2, "model": "m1"})).unwrap(),
            serde_json::to_string(&json!({"turn": 3, "model": "m1"})).unwrap(),
        );
        std::fs::write(&path, body).unwrap();
        let entries = read_session(tmp.path(), "sid1").unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["turn"], 1);
        assert_eq!(entries[2]["turn"], 3);
    }
}
