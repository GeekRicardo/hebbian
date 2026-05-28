//! EditHashlineTool — Hashline 后端的 Edit 工具。
//!
//! JSON 入参只有 `patch: string`，接受 hashline 格式的 patch 文本。
//! 解析、hash 校验、行号越界检查均在 edits::hashline 层完成；
//! 本工具只负责：读追踪检查 → 读文件 → apply → 落盘 → 更新追踪。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};
use tokio::fs;

use super::Tool;
use crate::edits::hashline::{
    apply::{apply_section, ApplyError},
    format::hash3,
    parser::parse_patch,
};
use crate::read_state::{EditPrecheck, ReadStateTracker};
use crate::workspace::Workspace;

pub struct EditHashlineTool {
    tracker: Option<Arc<ReadStateTracker>>,
}

impl EditHashlineTool {
    pub fn new(_workspace: Arc<Workspace>, tracker: Option<Arc<ReadStateTracker>>) -> Self {
        Self { tracker }
    }
}

#[async_trait]
impl Tool for EditHashlineTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        include_str!("../edits/hashline/prompt.md")
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["patch"],
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Hashline patch 文本。完整语法见工具说明。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let patch_text = input["patch"]
            .as_str()
            .ok_or_else(|| AppError::msg("Edit: 缺少 patch 字段"))?;

        let patch = parse_patch(patch_text)
            .map_err(|e| AppError::msg(format!("Edit: patch 解析失败 — {e}")))?;

        if patch.sections.is_empty() {
            return Err(AppError::msg("Edit: patch 为空，没有任何文件 section"));
        }

        let mut report = Vec::with_capacity(patch.sections.len());

        for section in &patch.sections {
            let file_path = PathBuf::from(&section.path);

            let exists = fs::try_exists(&file_path).await.unwrap_or(false);
            if !exists {
                return Err(AppError::msg(format!(
                    "Edit: 文件不存在 {} — hashline 后端不支持创建新文件，请用 string-replace 后端的 old_string=\"\" 语法",
                    section.path
                )));
            }

            let meta = fs::metadata(&file_path)
                .await
                .map_err(|e| AppError::msg(format!("Edit: 读取元数据失败 {}: {e}", section.path)))?;

            let current_mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);

            // ReadStateTracker 前置检查：未读 / stale 都拒绝
            if let Some(tracker) = self.tracker.as_ref() {
                match tracker.precheck(&file_path, current_mtime_ms) {
                    EditPrecheck::NotRead => {
                        return Err(AppError::msg(format!(
                            "Edit: 文件尚未读取——先用 Read 工具读取 {} 后再编辑",
                            section.path
                        )));
                    }
                    EditPrecheck::Stale => {
                        return Err(AppError::msg(format!(
                            "Edit: 文件 {} 在读取后被外部修改，请重新 Read 后再编辑",
                            section.path
                        )));
                    }
                    EditPrecheck::Fresh => {}
                }
            }

            let original = fs::read_to_string(&file_path)
                .await
                .map_err(|e| AppError::msg(format!("Edit: 读取失败 {}: {e}", section.path)))?;

            // apply_section 内部校验 hash（stale hash 在这里被拒绝）
            let new_content = apply_section(section, &original).map_err(|e| match e {
                ApplyError::StaleHash { .. } => AppError::msg(format!(
                    "Edit: {e} — 请重新 Read {} 后再 Edit",
                    section.path
                )),
                ApplyError::OutOfRange(_) => AppError::msg(format!(
                    "Edit: 行号越界 ({e}) — 请对照最新 Read 输出修正行号"
                )),
                ApplyError::Parse(p) => AppError::msg(format!("Edit: patch 解析失败 — {p}")),
            })?;

            fs::write(&file_path, &new_content)
                .await
                .map_err(|e| AppError::msg(format!("Edit: 写入失败 {}: {e}", section.path)))?;

            // 写后更新 tracker
            if let Some(tracker) = self.tracker.as_ref() {
                let mtime_ms = current_disk_mtime(&file_path).await;
                tracker.record(&file_path, hash_bytes(new_content.as_bytes()), mtime_ms);
            }

            report.push(format!(
                "applied {} ({} hunk{}) → new hash {}",
                section.path,
                section.hunks.len(),
                if section.hunks.len() == 1 { "" } else { "s" },
                hash3(&new_content),
            ));
        }

        Ok(report.join("\n"))
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

async fn current_disk_mtime(path: &std::path::Path) -> i64 {
    fs::metadata(path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edits::hashline::format::hash3;
    use crate::read_state::ReadStateTracker;

    fn make_tool() -> (Arc<ReadStateTracker>, tempfile::TempDir, EditHashlineTool) {
        let tmp = tempfile::tempdir().unwrap();
        let tracker = Arc::new(ReadStateTracker::new());
        let ws = Workspace::new(tmp.path(), Vec::new());
        let tool = EditHashlineTool::new(ws, Some(tracker.clone()));
        (tracker, tmp, tool)
    }

    async fn mark_read(tracker: &ReadStateTracker, path: &std::path::Path) {
        let mtime = current_disk_mtime(path).await;
        tracker.record(path, 0, mtime);
    }

    #[tokio::test]
    async fn applies_simple_replacement() {
        let (tracker, tmp, tool) = make_tool();
        let p = tmp.path().join("foo.txt");
        let original = "alpha\nbeta\ngamma\n";
        std::fs::write(&p, original).unwrap();
        mark_read(&tracker, &p).await;

        let patch = format!("¶{}#{}\n2 2\n+BETA\n", p.to_string_lossy(), hash3(original));
        let res = tool.execute(json!({ "patch": patch })).await.unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "alpha\nBETA\ngamma\n");
        assert!(res.contains("applied"));
    }

    #[tokio::test]
    async fn rejects_unread_file() {
        let (_tracker, tmp, tool) = make_tool();
        let p = tmp.path().join("foo.txt");
        std::fs::write(&p, "x\n").unwrap();
        let patch = format!("¶{}#{}\n1 1\n+y\n", p.to_string_lossy(), hash3("x\n"));
        let err = tool.execute(json!({ "patch": patch })).await.unwrap_err();
        assert!(
            err.to_string().contains("尚未读取"),
            "未先 Read 必须报错: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_stale_hash() {
        let (tracker, tmp, tool) = make_tool();
        let p = tmp.path().join("foo.txt");
        std::fs::write(&p, "current\n").unwrap();
        mark_read(&tracker, &p).await;

        let patch = format!("¶{}#000\n1 1\n+y\n", p.to_string_lossy());
        let err = tool.execute(json!({ "patch": patch })).await.unwrap_err();
        let s = err.to_string().to_lowercase();
        assert!(s.contains("stale") || s.contains("hash"), "stale hash 必须报错: {err}");
    }

    #[tokio::test]
    async fn multi_file_patch() {
        let (tracker, tmp, tool) = make_tool();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        std::fs::write(&a, "A1\n").unwrap();
        std::fs::write(&b, "B1\n").unwrap();
        mark_read(&tracker, &a).await;
        mark_read(&tracker, &b).await;

        let patch = format!(
            "¶{}#{}\n1 1\n+A2\n¶{}#{}\n1 1\n+B2\n",
            a.to_string_lossy(),
            hash3("A1\n"),
            b.to_string_lossy(),
            hash3("B1\n"),
        );
        tool.execute(json!({ "patch": patch })).await.unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "A2\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "B2\n");
    }

    #[tokio::test]
    async fn rejects_missing_patch_field() {
        let (_tracker, _tmp, tool) = make_tool();
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("patch"));
    }
}
