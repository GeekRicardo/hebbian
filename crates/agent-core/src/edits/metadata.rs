//! Edit 元数据持久化（架构 §4.13.5）。
//!
//! `.hebbian-edits.json` 追加式写入；回退不删条目，仅置 `reverted: true`。

use std::path::{Path, PathBuf};

use common::{AppError, AppResult};
use protocol::EditAction;
use serde::{Deserialize, Serialize};

use crate::storage;

/// 单次 Edit 的元数据条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditEntry {
    pub snapshot_id: String,
    pub call_id: String,
    pub tool: String,
    pub real_path: String,
    pub action: EditAction,
    pub before_sha: String,
    pub after_sha: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub ts_ms: i64,
    #[serde(default)]
    pub reverted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_at_ms: Option<i64>,
}

/// `.hebbian-edits.json` 的顶层结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditsMetadata {
    pub version: u32,
    pub entries: Vec<EditEntry>,
}

impl Default for EditsMetadata {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
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
    serde_json::from_str(&s)
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

/// 按 snapshot_id 查找条目。
pub fn find_entry<'a>(meta: &'a EditsMetadata, snapshot_id: &str) -> Option<&'a EditEntry> {
    meta.entries.iter().find(|e| e.snapshot_id == snapshot_id)
}

/// 按 snapshot_id 查找可变条目。
pub fn find_entry_mut<'a>(
    meta: &'a mut EditsMetadata,
    snapshot_id: &str,
) -> Option<&'a mut EditEntry> {
    meta.entries
        .iter_mut()
        .find(|e| e.snapshot_id == snapshot_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_default() {
        let meta = EditsMetadata::default();
        assert_eq!(meta.version, 1);
        assert!(meta.entries.is_empty());
    }

    #[test]
    fn find_entry_by_snapshot_id() {
        let mut meta = EditsMetadata::default();
        meta.entries.push(EditEntry {
            snapshot_id: "s1".into(),
            call_id: "c1".into(),
            tool: "Edit".into(),
            real_path: "/tmp/a.txt".into(),
            action: EditAction::Modify,
            before_sha: "abc".into(),
            after_sha: "def".into(),
            before_bytes: 100,
            after_bytes: 200,
            ts_ms: 1000,
            reverted: false,
            reverted_at_ms: None,
        });
        assert!(find_entry(&meta, "s1").is_some());
        assert!(find_entry(&meta, "nope").is_none());
    }
}
