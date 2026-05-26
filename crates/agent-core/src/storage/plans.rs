//! PlanMode 退出时把 plan markdown 落盘（架构 §4.4.5 / §6.1）。
//!
//! 路径形如 `~/.hebbian/sessions/<sid>/plans/plan-<yyyymmddHHmmss>.md`。
//! 调用方（[`crate::dispatch::ToolDispatcher`] 的 ExitPlanMode short-circuit
//! 分支）负责传入 `data_dir + session_id`。

use std::path::{Path, PathBuf};

use chrono::Utc;
use common::AppResult;

use super::lock;

pub fn dir_for_session(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join("sessions").join(session_id).join("plans")
}

/// 写入一份 plan 并返回最终文件路径。
///
/// 时间戳格式 `yyyymmddHHmmss`，秒粒度足以避免大多数冲突；同秒多次落盘
/// 时间戳相同但写入会被 lock 串行化，最终覆盖前者（PlanMode 一次 turn
/// 内重复 ExitPlanMode 的概率极低）。
pub fn save_plan(data_dir: &Path, session_id: &str, content: &str) -> AppResult<PathBuf> {
    let dir = dir_for_session(data_dir, session_id);
    std::fs::create_dir_all(&dir)?;
    let ts = Utc::now().format("%Y%m%d%H%M%S").to_string();
    let path = dir.join(format!("plan-{ts}.md"));
    lock::write_atomic(&path, content.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_plan_creates_dir_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = save_plan(tmp.path(), "sid-abc", "# Plan\n- step 1").unwrap();
        assert!(path.exists());
        let parent = path.parent().unwrap();
        assert!(parent.ends_with("sessions/sid-abc/plans"));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("# Plan"));
    }
}
