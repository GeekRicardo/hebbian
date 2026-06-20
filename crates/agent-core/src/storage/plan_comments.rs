//! Plan 评论流落盘（架构 §4.4.5）。
//!
//! 每个 plan（`plans/plan-<ts>.md`）配套一份 `plans/plan-<ts>.comments.jsonl`，
//! 一行一条 [`PlanCommentLine`]：
//!
//! - `Append(PlanComment)`：用户加了一条新评论
//! - `MarkConsumed { ids }`：批量把某些评论标记为"已注入下一轮 user message"
//!
//! [`list_comments`] 折叠规则：从空 vec 开始，按行序应用：
//! - `Append` 直接 push
//! - `MarkConsumed { ids }` 把命中 id 的条目 `consumed = true`
//!
//! 不做整文件重写——评论数量级小（单 plan 几条到几十条），增量行体积可忽略。

use std::path::{Path, PathBuf};

use chrono::Utc;
use common::AppResult;
use protocol::todo::PlanComment;
use serde::{Deserialize, Serialize};

use super::lock;
use super::plans;

/// jsonl 单行类型。新版本读到未知 variant 应跳过而非报错。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PlanCommentLine {
    Append(PlanComment),
    MarkConsumed { ids: Vec<String> },
}

fn comments_path(
    data_dir: &Path,
    workdir: Option<&Path>,
    session_id: &str,
    plan_id: &str,
) -> PathBuf {
    plans::dir_for_session(data_dir, workdir, session_id).join(format!("{plan_id}.comments.jsonl"))
}

/// 给 plan 加一条评论。`comment.id` 调用方负责生成（ulid 推荐）。
/// 函数会强制 `consumed=false`、补 `created_at_ms`。
pub fn append_comment(
    data_dir: &Path,
    workdir: Option<&Path>,
    session_id: &str,
    plan_id: &str,
    mut comment: PlanComment,
) -> AppResult<PlanComment> {
    comment.plan_id = plan_id.to_string();
    comment.consumed = false;
    if comment.created_at_ms == 0 {
        comment.created_at_ms = Utc::now().timestamp_millis();
    }
    let path = comments_path(data_dir, workdir, session_id, plan_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(&PlanCommentLine::Append(comment.clone()))?;
    lock::append_jsonl(&path, &line)?;
    Ok(comment)
}

/// 列出当前所有评论（含已消费的，按时间序）。
/// 文件不存在视为空。
pub fn list_comments(
    data_dir: &Path,
    workdir: Option<&Path>,
    session_id: &str,
    plan_id: &str,
) -> AppResult<Vec<PlanComment>> {
    let path = comments_path(data_dir, workdir, session_id, plan_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = lock::read_locked(&path)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut out: Vec<PlanComment> = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<PlanCommentLine>(trimmed) {
            Ok(PlanCommentLine::Append(c)) => out.push(c),
            Ok(PlanCommentLine::MarkConsumed { ids }) => {
                let set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
                for c in out.iter_mut() {
                    if set.contains(c.id.as_str()) {
                        c.consumed = true;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    lineno = lineno + 1,
                    "plan comment jsonl 行解析失败，跳过"
                );
            }
        }
    }
    Ok(out)
}

/// 批量把指定评论 id 标记为已消费（已注入下一轮 user message）。
pub fn mark_consumed(
    data_dir: &Path,
    workdir: Option<&Path>,
    session_id: &str,
    plan_id: &str,
    ids: Vec<String>,
) -> AppResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let path = comments_path(data_dir, workdir, session_id, plan_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(&PlanCommentLine::MarkConsumed { ids })?;
    lock::append_jsonl(&path, &line)?;
    Ok(())
}

/// 取出所有未消费的评论。返回顺序 = 写入序。
pub fn list_unconsumed(
    data_dir: &Path,
    workdir: Option<&Path>,
    session_id: &str,
    plan_id: &str,
) -> AppResult<Vec<PlanComment>> {
    Ok(list_comments(data_dir, workdir, session_id, plan_id)?
        .into_iter()
        .filter(|c| !c.consumed)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn comment(id: &str, body: &str) -> PlanComment {
        PlanComment {
            id: id.to_string(),
            plan_id: String::new(),
            anchor: "L1".to_string(),
            body: body.to_string(),
            created_at_ms: 0,
            consumed: false,
        }
    }

    #[test]
    fn append_list_consume_roundtrip() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let sid = "sid-1";
        let plan_id = "plan-20260525";

        let c1 = append_comment(data_dir, None, sid, plan_id, comment("c1", "first")).unwrap();
        let c2 = append_comment(data_dir, None, sid, plan_id, comment("c2", "second")).unwrap();
        assert_eq!(c1.plan_id, plan_id);
        assert!(c1.created_at_ms > 0);

        let all = list_comments(data_dir, None, sid, plan_id).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|c| !c.consumed));

        let unconsumed = list_unconsumed(data_dir, None, sid, plan_id).unwrap();
        assert_eq!(unconsumed.len(), 2);

        mark_consumed(data_dir, None, sid, plan_id, vec![c1.id.clone()]).unwrap();
        let after = list_comments(data_dir, None, sid, plan_id).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after[0].consumed);
        assert!(!after[1].consumed);
        assert_eq!(c2.id, after[1].id);

        let unconsumed = list_unconsumed(data_dir, None, sid, plan_id).unwrap();
        assert_eq!(unconsumed.len(), 1);
        assert_eq!(unconsumed[0].id, "c2");
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let out = list_comments(dir.path(), None, "sid", "plan-x").unwrap();
        assert!(out.is_empty());
    }
}
