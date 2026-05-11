//! 工具结果落盘（架构 §4.7 / §6.1）。
//!
//! 大输出与被压缩的小输出走同一套机制，路径形如
//! `~/.hebbian/sessions/<sid>/tool_results/<call_id>.txt`。本模块仅暴露最小 API；
//! 实际触发点在 context engine（Step 9 落地）。

use std::path::{Path, PathBuf};

use common::AppResult;

use super::lock;

pub fn dir_for_session(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("sessions")
        .join(session_id)
        .join("tool_results")
}

pub fn save_tool_result(
    data_dir: &Path,
    session_id: &str,
    call_id: &str,
    content: &str,
) -> AppResult<PathBuf> {
    let dir = dir_for_session(data_dir, session_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{call_id}.txt"));
    lock::write_atomic(&path, content.as_bytes())?;
    Ok(path)
}
