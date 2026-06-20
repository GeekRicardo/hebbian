//! PlanMode 的 plan markdown 落盘（架构 §4.4.5 / §6.1）。
//!
//! plan 按 workdir 归属、会话子目录隔离：
//! - 有 workdir（code 栏项目）：`~/.hebbian/projects/<encode(workdir)>/plans/<sid>/plan-<ts>.md`
//! - 无 workdir（chat 栏对话）：`~/.hebbian/plans/<sid>/plan-<ts>.md`
//!
//! 同一项目下多个会话各占一个 `<sid>/` 子目录，list 直接读会话子目录、删会话整目录删。
//! 调用方负责传 `workdir`（来自 `Session.workdir`，是 plan 归属的唯一真源）。

use std::path::{Path, PathBuf};

use chrono::Utc;
use common::AppResult;

use super::lock;
use super::projects;

/// 解析某会话的 plan 目录。`workdir` 来自 `Session.workdir`：
/// `Some` → 项目级 `projects/<encode>/plans/<sid>/`；`None` → 全局 `plans/<sid>/`。
pub fn dir_for_session(data_dir: &Path, workdir: Option<&Path>, session_id: &str) -> PathBuf {
    let root = match workdir {
        Some(wd) => projects::project_dir(data_dir, wd),
        None => data_dir.to_path_buf(),
    };
    root.join("plans").join(session_id)
}

/// 新建一份 plan 并返回最终文件路径（PlanMode enter 时创建草稿）。
///
/// 时间戳格式 `yyyymmddHHmmss`，秒粒度足以避免大多数冲突；同秒多次落盘
/// 时间戳相同但写入会被 lock 串行化，最终覆盖前者。
pub fn save_plan(
    data_dir: &Path,
    workdir: Option<&Path>,
    session_id: &str,
    content: &str,
) -> AppResult<PathBuf> {
    let dir = dir_for_session(data_dir, workdir, session_id);
    std::fs::create_dir_all(&dir)?;
    let ts = Utc::now().format("%Y%m%d%H%M%S").to_string();
    let path = dir.join(format!("plan-{ts}.md"));
    lock::write_atomic(&path, content.as_bytes())?;
    Ok(path)
}

/// 覆盖写一份已存在的 plan 文件（PlanMode update 时反复打磨 plan）。
///
/// `plan_id` 是文件 stem（如 `plan-20260525143012`）。文件不存在时一并创建，
/// 让 update 在 enter 草稿缺失等边角场景下仍能成功落盘。
pub fn update_plan(
    data_dir: &Path,
    workdir: Option<&Path>,
    session_id: &str,
    plan_id: &str,
    content: &str,
) -> AppResult<PathBuf> {
    let dir = dir_for_session(data_dir, workdir, session_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{plan_id}.md"));
    lock::write_atomic(&path, content.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_plan_without_workdir_goes_global() {
        let tmp = tempfile::tempdir().unwrap();
        let path = save_plan(tmp.path(), None, "sid-abc", "# Plan\n- step 1").unwrap();
        assert!(path.exists());
        let parent = path.parent().unwrap();
        assert!(parent.ends_with("plans/sid-abc"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("# Plan"));
    }

    #[test]
    fn save_plan_with_workdir_goes_under_project() {
        let tmp = tempfile::tempdir().unwrap();
        let wd = Path::new("/Users/x/proj");
        let path = save_plan(tmp.path(), Some(wd), "sid-abc", "# Plan").unwrap();
        let enc = projects::encode_workdir(wd);
        let parent = path.parent().unwrap();
        assert!(parent.ends_with(format!("projects/{enc}/plans/sid-abc")));
    }

    #[test]
    fn update_plan_overwrites_same_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = save_plan(tmp.path(), None, "sid-1", "# v1").unwrap();
        let plan_id = p1.file_stem().unwrap().to_str().unwrap().to_string();
        let p2 = update_plan(tmp.path(), None, "sid-1", &plan_id, "# v2").unwrap();
        assert_eq!(p1, p2, "update 应覆盖同一文件而非新建");
        assert_eq!(std::fs::read_to_string(&p2).unwrap(), "# v2");
    }
}
