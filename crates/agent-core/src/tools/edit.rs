//! Edit 工具（架构 §4.4.6 / §4.4.10）。
//!
//! 精准替换文件中的一段文本；统一承担创建 / 全覆盖 / 局部修改三种语义：
//! - `old_string == ""` → 创建新文件（new_string 作为完整内容）
//! - `old_string == 完整旧内容` → 全量覆盖
//! - 其余 → 精确字符串替换（默认要求唯一匹配，`replace_all=true` 时全量替换）
//!
//! 前置条件追踪由 [`ReadStateTracker`] 承担：editing 已有文件前必须先 Read，
//! 且 Read 之后磁盘 mtime 没被外部改动。具体校验流程见架构 §4.4.10 表格
//! （errorCode 与 Claude Code 2.1.144 同号 0-12）。
//!
//! 注意：edits-worktree 快照（§4.13）由 dispatcher 在 execute 前后包夹完成，
//! 不在本工具里——工具只负责"是否允许改 + 怎么改"。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};
use tokio::fs;

use super::Tool;
use crate::read_state::{EditPrecheck, ReadStateTracker};
use crate::workspace::Workspace;

const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB，与 Claude Code 2.1.144 一致

pub struct EditTool {
    tracker: Option<Arc<ReadStateTracker>>,
}

impl EditTool {
    pub fn new(_workspace: Arc<Workspace>, tracker: Option<Arc<ReadStateTracker>>) -> Self {
        Self { tracker }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "对文件做精准字符串替换。\
         old_string 必须在文件中精确存在（含原始缩进；CRLF/LF 自动归一），\
         默认要求在文件内唯一——非唯一时请加更多上下文，或设 replace_all=true 替换全部出现。\
         old_string=\"\" 时视为创建新文件（new_string 作为完整内容；目标路径必须不存在或为空）。\
         编辑已有文件前必须先用 Read 读取该文件（避免盲改和过期写入）。\
         需要审批；路径必须在对话允许的路径范围内。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path", "old_string", "new_string"],
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "old_string": {
                    "type": "string",
                    "description": "要被替换的原文，含原始缩进。\"\" 时视为创建新文件。"
                },
                "new_string": {
                    "type": "string",
                    "description": "替换后的新文本。"
                },
                "replace_all": {
                    "type": "boolean",
                    "default": false,
                    "description": "true 时替换文件中所有出现；false 时要求 old_string 在文件中唯一。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let file_path_str = input["file_path"]
            .as_str()
            .ok_or_else(|| AppError::msg("Edit: 缺少 file_path"))?;
        let old_string = input["old_string"]
            .as_str()
            .ok_or_else(|| AppError::msg("Edit: 缺少 old_string"))?;
        let new_string = input["new_string"]
            .as_str()
            .ok_or_else(|| AppError::msg("Edit: 缺少 new_string"))?;
        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // 路径越界由 dispatcher 兜底（架构 §4.4.2），这里不重复

        // ── Step 3: old_string == new_string（架构 §4.4.10 errorCode 1）
        if old_string == new_string {
            return Err(AppError::msg(
                "Edit: 没有变化——old_string 与 new_string 完全一致",
            ));
        }

        let file_path = PathBuf::from(file_path_str);

        // ── Step 7: .ipynb（errorCode 5）
        if file_path.extension().and_then(|e| e.to_str()) == Some("ipynb") {
            return Err(AppError::msg(
                "Edit: Jupyter Notebook 暂不支持，请改用 Read+Bash 的 jq/jupyter 命令操作单元格",
            ));
        }

        // ── old_string == "" 分支：创建新文件
        if old_string.is_empty() {
            return self
                .execute_create(&file_path, file_path_str, new_string)
                .await;
        }

        // ── 修改路径
        let exists = fs::try_exists(&file_path).await.unwrap_or(false);
        if !exists {
            return Err(AppError::msg(format!(
                "Edit: 文件不存在 {file_path_str}。要创建新文件请用 old_string=\"\""
            )));
        }

        let meta = fs::metadata(&file_path)
            .await
            .map_err(|e| AppError::msg(format!("Edit: 读取元数据失败 {file_path_str}: {e}")))?;

        // ── Step 5: 1GB 上限（errorCode 10）
        if meta.len() > MAX_FILE_BYTES {
            return Err(AppError::msg(format!(
                "Edit: 文件过大 ({} 字节)，上限 1GB",
                meta.len()
            )));
        }
        // ── Step 6: 不是目录 / 特殊类型（errorCode 11）
        if meta.is_dir() {
            return Err(AppError::msg(format!(
                "Edit: {file_path_str} 是目录，不是文件"
            )));
        }

        let current_mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        // ── Step 8 + 9: ReadStateTracker 强约束（errorCode 6 / 7）
        if let Some(tracker) = self.tracker.as_ref() {
            match tracker.precheck(&file_path, current_mtime_ms) {
                EditPrecheck::NotRead => {
                    return Err(AppError::msg(format!(
                        "Edit: 文件尚未读取——先用 Read 工具读取 {file_path_str} 后再编辑"
                    )));
                }
                EditPrecheck::Stale => {
                    return Err(AppError::msg(format!(
                        "Edit: 文件 {file_path_str} 在读取后被外部修改（用户 / linter / 其他工具），请重新 Read 后再编辑"
                    )));
                }
                EditPrecheck::Fresh => {}
            }
        }

        let content = fs::read_to_string(&file_path)
            .await
            .map_err(|e| AppError::msg(format!("Edit: 读取失败 {file_path_str}: {e}")))?;

        // ── Step 10: 查找 + CRLF 归一化 + Unicode escape 容错（errorCode 8）
        let normalized_content = content.replace("\r\n", "\n");
        let normalized_old = old_string.replace("\r\n", "\n");

        let (actual_old, match_count) = match find_matches(&normalized_content, &normalized_old) {
            Some(c) => (normalized_old.clone(), c),
            None => match swap_unicode_escapes(&normalized_old) {
                Some(swapped) => match find_matches(&normalized_content, &swapped) {
                    Some(c) => (swapped, c),
                    None => {
                        return Err(AppError::msg(
                            "Edit: 文件中找不到 old_string（已尝试 \\uXXXX 转义/反转义两种形式都不匹配）。请重新 Read 并复制确切的上下文。",
                        ));
                    }
                },
                None => {
                    return Err(AppError::msg(
                        "Edit: 文件中找不到 old_string。请重新 Read 并复制确切的上下文。",
                    ));
                }
            },
        };

        // ── Step 11: 唯一性（errorCode 9）
        if match_count > 1 && !replace_all {
            return Err(AppError::msg(format!(
                "Edit: 匹配到 {match_count} 处，但 replace_all=false。请加更多上下文以唯一定位，或设 replace_all=true。"
            )));
        }

        // ── 执行替换
        let new_content = if replace_all {
            normalized_content.replace(&actual_old, new_string)
        } else {
            normalized_content.replacen(&actual_old, new_string, 1)
        };

        let original_bytes = content.len();
        let new_bytes = new_content.len();

        fs::write(&file_path, &new_content)
            .await
            .map_err(|e| AppError::msg(format!("Edit: 写入失败 {file_path_str}: {e}")))?;

        // 写后立刻更新 tracker：再次写盘的 mtime 必须 = 当前 mtime
        if let Some(tracker) = self.tracker.as_ref() {
            let mtime_ms = current_disk_mtime(&file_path).await;
            tracker.record(&file_path, hash_bytes(new_content.as_bytes()), mtime_ms);
        }

        if replace_all && match_count > 1 {
            Ok(format!(
                "已修改文件 {file_path_str}（替换 {match_count} 处，{original_bytes} → {new_bytes} 字节）"
            ))
        } else {
            Ok(format!(
                "已修改文件 {file_path_str}（{original_bytes} → {new_bytes} 字节）"
            ))
        }
    }
}

impl EditTool {
    /// `old_string == ""` 分支：创建新文件（errorCode 3）。
    async fn execute_create(
        &self,
        file_path: &Path,
        file_path_str: &str,
        new_string: &str,
    ) -> AppResult<String> {
        let exists = fs::try_exists(file_path).await.unwrap_or(false);
        if exists {
            let existing = fs::read_to_string(file_path)
                .await
                .unwrap_or_else(|_| String::new());
            if !existing.trim().is_empty() {
                return Err(AppError::msg(format!(
                    "Edit: 无法创建——文件已存在且非空 {file_path_str}。如要全量覆盖，请先 Read 再 Edit（old_string=完整旧内容）"
                )));
            }
        }

        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::msg(format!("Edit: 创建父目录失败 {e}")))?;
        }

        fs::write(file_path, new_string)
            .await
            .map_err(|e| AppError::msg(format!("Edit: 写入失败 {file_path_str}: {e}")))?;

        if let Some(tracker) = self.tracker.as_ref() {
            let mtime_ms = current_disk_mtime(file_path).await;
            tracker.record(file_path, hash_bytes(new_string.as_bytes()), mtime_ms);
        }

        Ok(format!(
            "已创建文件 {file_path_str}（{} 字节，{} 行）",
            new_string.len(),
            new_string.lines().count()
        ))
    }
}

/// 返回 `needle` 在 `haystack` 中的出现次数。0 次返回 None，便于上层走容错分支。
fn find_matches(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let count = haystack.matches(needle).count();
    if count == 0 {
        None
    } else {
        Some(count)
    }
}

/// 把 `\uXXXX` 字面形式转换为真实 Unicode 字符。
/// 返回 `Some(swapped)` 当且仅当至少完成一次有效转换，否则 `None`。
fn swap_unicode_escapes(s: &str) -> Option<String> {
    if !s.contains("\\u") {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut changed = false;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 5 < bytes.len() && bytes[i + 1] == b'u' {
            let hex = &s[i + 2..i + 6];
            if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                if let Ok(code) = u32::from_str_radix(hex, 16) {
                    if let Some(ch) = char::from_u32(code) {
                        out.push(ch);
                        i += 6;
                        changed = true;
                        continue;
                    }
                }
            }
        }
        // 取一个 UTF-8 字符
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    if changed {
        Some(out)
    } else {
        None
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

async fn current_disk_mtime(path: &Path) -> i64 {
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
    use tokio::time::{sleep, Duration};

    fn workspace_at(path: &std::path::Path) -> Arc<Workspace> {
        Workspace::new(path, Vec::new())
    }

    fn tool_with_tracker() -> (Arc<ReadStateTracker>, tempfile::TempDir, EditTool) {
        let tmp = tempfile::tempdir().unwrap();
        let tracker = Arc::new(ReadStateTracker::new());
        let tool = EditTool::new(workspace_at(tmp.path()), Some(tracker.clone()));
        (tracker, tmp, tool)
    }

    #[tokio::test]
    async fn create_new_file_when_old_string_empty() {
        let (_tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("nested/dir/out.txt");
        let result = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "",
                "new_string": "hello",
            }))
            .await
            .unwrap();
        assert!(result.contains("创建"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    }

    #[tokio::test]
    async fn create_rejects_existing_nonempty_file() {
        let (_tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "old content").unwrap();

        let err = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "",
                "new_string": "new",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("文件已存在且非空"));
    }

    #[tokio::test]
    async fn modify_rejects_unread_file() {
        let (_tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "abc").unwrap();

        let err = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "abc",
                "new_string": "xyz",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("尚未读取"));
    }

    #[tokio::test]
    async fn modify_rejects_stale_file() {
        let (tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "abc").unwrap();

        // 模拟"已读"但时戳早于当前 mtime
        tracker.record(&target, 0, 0);

        let err = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "abc",
                "new_string": "xyz",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("外部修改"));
    }

    #[tokio::test]
    async fn modify_succeeds_when_fresh_and_unique() {
        let (tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "hello world").unwrap();

        // 注册"已读"且 mtime ≥ 当前 mtime
        let mtime = current_disk_mtime(&target).await;
        tracker.record(&target, 0, mtime);

        let result = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "world",
                "new_string": "rust",
            }))
            .await
            .unwrap();
        assert!(result.contains("已修改"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello rust");
    }

    #[tokio::test]
    async fn rejects_non_unique_old_string_without_replace_all() {
        let (tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "ab ab ab").unwrap();
        tracker.record(&target, 0, current_disk_mtime(&target).await);

        let err = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "ab",
                "new_string": "cd",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("匹配到 3 处"));
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let (tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "ab ab ab").unwrap();
        tracker.record(&target, 0, current_disk_mtime(&target).await);

        let result = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "ab",
                "new_string": "cd",
                "replace_all": true,
            }))
            .await
            .unwrap();
        assert!(result.contains("替换 3 处"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "cd cd cd");
    }

    #[tokio::test]
    async fn crlf_lf_normalized_match() {
        let (tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "line1\r\nline2\r\nline3").unwrap();
        tracker.record(&target, 0, current_disk_mtime(&target).await);

        // old_string 用 LF 也应该能匹配 CRLF 文件
        let result = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "line1\nline2",
                "new_string": "L1\nL2",
            }))
            .await
            .unwrap();
        assert!(result.contains("已修改"));
    }

    #[tokio::test]
    async fn unicode_escape_swap_matches() {
        let (tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "前缀: \u{4e2d}\u{6587}后缀").unwrap();
        tracker.record(&target, 0, current_disk_mtime(&target).await);

        // old_string 用 \uXXXX 字面形式，文件里是真实字符
        let result = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "\\u4e2d\\u6587",
                "new_string": "中文OK",
            }))
            .await;
        // 至少不能因"找不到"而失败；如果实现得对，应当替换成功
        let result = result.unwrap();
        assert!(result.contains("已修改"));
    }

    #[tokio::test]
    async fn rejects_old_equals_new() {
        let (_tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "abc").unwrap();
        let err = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "abc",
                "new_string": "abc",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("没有变化"));
    }

    #[tokio::test]
    async fn rejects_ipynb_files() {
        let (_tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.ipynb");
        // 不需要真的创建文件——.ipynb 检查在 path 解析阶段
        let err = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "x",
                "new_string": "y",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Jupyter Notebook"));
    }

    #[tokio::test]
    async fn rejects_modify_missing_file() {
        let (_tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("nope.txt");
        let err = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "x",
                "new_string": "y",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("文件不存在"));
    }

    #[tokio::test]
    async fn precheck_detects_external_modification_between_read_and_edit() {
        // 完整端到端：模拟 Read 之后用户改了文件，Edit 必须拒绝
        let (tracker, tmp, tool) = tool_with_tracker();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "v1").unwrap();
        let initial_mtime = current_disk_mtime(&target).await;
        tracker.record(&target, 0, initial_mtime);

        // 等 fs 时钟跨毫秒，再外部写一次
        sleep(Duration::from_millis(20)).await;
        std::fs::write(&target, "v1-modified-externally").unwrap();

        let err = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "old_string": "v1",
                "new_string": "v2",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("外部修改"));
    }
}
