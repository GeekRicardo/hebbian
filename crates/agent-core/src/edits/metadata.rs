//! Edit 元数据持久化（架构 §4.13.5）。
//!
//! `.hebbian-edits.json` 追加式写入；回退不删条目，仅置 `reverted: true`。

use std::path::{Path, PathBuf};

use common::{AppError, AppResult};
use protocol::EditAction;
use serde::{Deserialize, Serialize};

use crate::storage;

/// 一个 Run 内某个文件的净变化。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnFileChange {
    pub real_path: String,
    pub action: EditAction,
    pub before_sha: String,
    pub after_sha: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
}

impl From<TurnFileChange> for protocol::TurnFileChange {
    fn from(value: TurnFileChange) -> Self {
        Self {
            real_path: value.real_path,
            action: value.action,
            before_sha: value.before_sha,
            after_sha: value.after_sha,
            before_bytes: value.before_bytes,
            after_bytes: value.after_bytes,
        }
    }
}

/// 一个 Run（整个 agent_loop，含插队）的文件净变化元数据条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEditEntry {
    pub run_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub files: Vec<TurnFileChange>,
    #[serde(default)]
    pub reverted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_at_ms: Option<i64>,
}

/// `.hebbian-edits.json` 的顶层结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditsMetadata {
    pub version: u32,
    #[serde(default)]
    pub runs: Vec<RunEditEntry>,
}

impl Default for EditsMetadata {
    fn default() -> Self {
        Self {
            version: 3,
            runs: Vec::new(),
        }
    }
}

/// edits-worktree 根目录（含 `.git/` 和 `.hebbian-edits.json`）。
pub fn worktree_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("sessions")
        .join(session_id)
        .join("edits-worktree")
}

/// metadata 文件路径。
pub fn metadata_path(worktree_dir: &Path) -> PathBuf {
    worktree_dir.join(".hebbian-edits.json")
}

/// 加载 metadata；文件不存在返回默认空。
pub fn load_metadata(worktree_dir: &Path) -> AppResult<EditsMetadata> {
    let path = metadata_path(worktree_dir);
    if !path.exists() {
        return Ok(EditsMetadata::default());
    }
    let data = storage::lock::read_locked(&path)?;
    let s = String::from_utf8_lossy(&data);
    if s.trim().is_empty() {
        return Ok(EditsMetadata::default());
    }
    let value: serde_json::Value = serde_json::from_str(&s)
        .map_err(|e| AppError::msg(format!("解析 .hebbian-edits.json 失败: {e}")))?;
    // 版本守卫：v3 之前是 per-Edit(v1) / per-Turn(v2) 旧格式，结构不兼容，整体丢弃
    // （不迁移；旧会话失去历史 Edit 记录但不影响 session transcript）。
    if value.get("version").and_then(|v| v.as_u64()) != Some(3) {
        return Ok(EditsMetadata::default());
    }
    serde_json::from_value(value)
        .map_err(|e| AppError::msg(format!("解析 .hebbian-edits.json 失败: {e}")))
}

/// 保存 metadata（整文件原子写，加排他锁）。
pub fn save_metadata(worktree_dir: &Path, meta: &EditsMetadata) -> AppResult<()> {
    let path = metadata_path(worktree_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| AppError::msg(format!("序列化 metadata: {e}")))?;
    storage::lock::write_atomic(&path, json.as_bytes())
}

/// 按 run_id 查找条目。
pub fn find_run<'a>(meta: &'a EditsMetadata, run_id: &str) -> Option<&'a RunEditEntry> {
    meta.runs.iter().find(|e| e.run_id == run_id)
}

/// 按 run_id 查找可变条目。
pub fn find_run_mut<'a>(
    meta: &'a mut EditsMetadata,
    run_id: &str,
) -> Option<&'a mut RunEditEntry> {
    meta.runs.iter_mut().find(|e| e.run_id == run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_default() {
        let meta = EditsMetadata::default();
        assert_eq!(meta.version, 3);
        assert!(meta.runs.is_empty());
    }

    #[test]
    fn find_run_by_run_id() {
        let mut meta = EditsMetadata::default();
        meta.runs.push(RunEditEntry {
            run_id: "r1".into(),
            started_at_ms: 1000,
            finished_at_ms: 2000,
            files: vec![TurnFileChange {
                real_path: "/tmp/a.txt".into(),
                action: EditAction::Modify,
                before_sha: "abc".into(),
                after_sha: "def".into(),
                before_bytes: 100,
                after_bytes: 200,
            }],
            reverted: false,
            reverted_at_ms: None,
        });
        assert!(find_run(&meta, "r1").is_some());
        assert!(find_run(&meta, "nope").is_none());
    }
}
