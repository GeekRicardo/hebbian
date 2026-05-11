//! `/compact` 时压缩前的 transcript 落盘为 markdown（架构 §4.7 / §6.1）。
//!
//! 路径形如 `~/.hebbian/sessions/<sid>/compactions/compact-<ts>.md`。
//! 当前模块只暴露写入函数；触发点由 context engine 在 Step 9 接入。

use std::path::{Path, PathBuf};

use common::AppResult;

use super::lock;

pub fn dir_for_session(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("sessions")
        .join(session_id)
        .join("compactions")
}

pub fn save_compaction(
    data_dir: &Path,
    session_id: &str,
    timestamp_label: &str,
    markdown: &str,
) -> AppResult<PathBuf> {
    let dir = dir_for_session(data_dir, session_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("compact-{timestamp_label}.md"));
    lock::write_atomic(&path, markdown.as_bytes())?;
    Ok(path)
}
