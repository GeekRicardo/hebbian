//! ReadState Tracker（架构 §4.4.10）。
//!
//! Edit 工具的前置条件追踪：当前 session 已 Read 过哪些文件、Read 时戳与
//! 内容 hash 是什么。Edit 据此拒绝两类错误：
//! - 未读盲改：模型没读过文件就直接 Edit
//! - 过期写入：Read 之后用户 / linter 已改过文件，Edit 会覆盖这些外部改动
//!
//! 每个 [`Session`](crate::Session) 一份；进程内、不落盘——重开 session 从零追踪。
//! [`ReadTool`](crate::tools::read::ReadTool) 与 [`EditTool`](crate::tools::edit::EditTool)
//! 通过 `Arc<ReadStateTracker>` 共享同一实例。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 单个文件的 Read 状态快照。
#[derive(Debug, Clone, Copy)]
pub struct ReadState {
    /// 读取时计算的内容 hash（DefaultHasher，进程内唯一）。
    pub content_hash: u64,
    /// 读取时文件的 mtime（Unix epoch ms）。
    pub mtime_ms: i64,
}

/// Edit 前置检查结果。
#[derive(Debug, PartialEq, Eq)]
pub enum EditPrecheck {
    /// 已读且未被外部改动。
    Fresh,
    /// 该文件从未被 Read。
    NotRead,
    /// Read 过但外部已修改（当前 mtime > 已读时戳）。
    Stale,
}

#[derive(Default)]
pub struct ReadStateTracker {
    states: Mutex<HashMap<PathBuf, ReadState>>,
}

impl ReadStateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次 Read 或 Edit 之后的最新状态。
    pub fn record(&self, path: &Path, content_hash: u64, mtime_ms: i64) {
        self.states.lock().unwrap().insert(
            path.to_path_buf(),
            ReadState {
                content_hash,
                mtime_ms,
            },
        );
    }

    /// Edit 前置检查。`current_mtime_ms` 是磁盘上**当下**的 mtime（调用方负责取）。
    pub fn precheck(&self, path: &Path, current_mtime_ms: i64) -> EditPrecheck {
        let states = self.states.lock().unwrap();
        match states.get(path) {
            None => EditPrecheck::NotRead,
            Some(state) if current_mtime_ms > state.mtime_ms => EditPrecheck::Stale,
            Some(_) => EditPrecheck::Fresh,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn not_read_by_default() {
        let t = ReadStateTracker::new();
        assert_eq!(t.precheck(Path::new("/x"), 0), EditPrecheck::NotRead);
    }

    #[test]
    fn fresh_after_record() {
        let t = ReadStateTracker::new();
        t.record(Path::new("/x"), 42, 1000);
        assert_eq!(t.precheck(Path::new("/x"), 1000), EditPrecheck::Fresh);
    }

    #[test]
    fn stale_when_external_mtime_newer() {
        let t = ReadStateTracker::new();
        t.record(Path::new("/x"), 42, 1000);
        assert_eq!(t.precheck(Path::new("/x"), 1500), EditPrecheck::Stale);
    }

    #[test]
    fn equal_mtime_is_fresh() {
        // Linter / formatter 写完通常 mtime 与读时戳相等或更新；用 > 严格判断，等于视为 Fresh
        let t = ReadStateTracker::new();
        t.record(Path::new("/x"), 42, 1000);
        assert_eq!(t.precheck(Path::new("/x"), 1000), EditPrecheck::Fresh);
    }
}
