//! Git 源代码管理（SCM）——纯 surface 能力（架构 §7.3）。
//!
//! 给 Desktop / hebweb 的「源代码管理」栏提供项目工作区相对 git 的改动视图与基础写操作。
//! 与 §4.13 edits-worktree 是**两套互不相干**的东西：
//! - edits-worktree：session 私有影子仓，按 AI 的 Run 聚合快照、可整 Run 回退
//! - git_scm：用户项目本身的 git 状态（用户手改 + AI 改混在一起，相对 HEAD/index）
//!
//! 安全边界（写操作不可逆，调用方必须遵守）：
//! - discard / commit 直接动用户真实仓库，不在 edits-worktree 回退保护内
//! - 所有写操作只作用于**单个文件**（commit 除外，提交已暂存内容），路径必须落在 root 下
//! - 绝不碰 remote（不 push）、不做 rebase / merge / 改 .git 配置 / 删分支

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{AppError, AppResult};

/// 一个文件的 git 状态。`x` / `y` 为 porcelain 的 index 态 / worktree 态字符
/// （'M' 'A' 'D' 'R' 'C' 'U' '?' ' '）。`staged` = index 侧有改动，`unstaged` = worktree 侧有改动。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GitFileStatus {
    /// 相对 root 的路径（porcelain 原样，含重命名时的新路径）。
    pub path: String,
    /// 绝对路径（root.join(path)）。
    pub abs_path: String,
    /// index 态字符（X）。
    pub x: String,
    /// worktree 态字符（Y）。
    pub y: String,
    /// index 侧有改动（在暂存区）。
    pub staged: bool,
    /// worktree 侧有改动（未暂存 / 未跟踪）。
    pub unstaged: bool,
    /// 是否未跟踪文件（X 与 Y 均为 '?'）。
    pub untracked: bool,
}

/// 一个项目（git 仓库根）的状态。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GitProjectStatus {
    /// 仓库根绝对路径。
    pub root: String,
    /// 根目录名（UI 显示用）。
    pub name: String,
    /// 当前分支名（detached 时为短 sha）。
    pub branch: String,
    pub files: Vec<GitFileStatus>,
}

/// 跑一条 git 命令，返回原样 stdout（不 trim——diff / show 末尾换行是内容的一部分）。
fn run_git(dir: &Path, args: &[&str]) -> AppResult<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| AppError::msg(format!("git {} 失败: {e}", args.join(" "))))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::msg(format!(
            "git {} 退出码非零: {stderr}",
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// 该目录是否在某个 git 工作树内（且其顶层即 dir）。非仓库返回 false，不报错。
pub fn is_git_repo(dir: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 校验 path 落在 root 下（拒绝越界写）。返回规范化后的绝对路径。
fn ensure_within(root: &Path, rel_path: &str) -> AppResult<PathBuf> {
    let abs = root.join(rel_path);
    // 不要求文件存在（discard 未跟踪文件删除后就不存在了），只做前缀与 `..` 防护。
    if rel_path.contains("..") {
        return Err(AppError::msg("路径含 .. ，拒绝越界操作"));
    }
    if !abs.starts_with(root) {
        return Err(AppError::msg("路径越出项目根，拒绝操作"));
    }
    Ok(abs)
}

/// 当前分支名；detached HEAD 时返回短 sha；取不到返回空串。
fn current_branch(dir: &Path) -> String {
    if let Ok(out) = run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        let name = out.trim();
        if name == "HEAD" {
            // detached：取短 sha
            return run_git(dir, &["rev-parse", "--short", "HEAD"])
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
        }
        return name.to_string();
    }
    String::new()
}

/// 列一个仓库的工作区状态（含未跟踪）。porcelain v1 -z NUL 分隔，避免文件名空格 / 引号歧义。
pub fn status(root: &Path) -> AppResult<GitProjectStatus> {
    let raw = run_git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let files = parse_porcelain_z(root, &raw);
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    Ok(GitProjectStatus {
        root: root.to_string_lossy().into_owned(),
        name,
        branch: current_branch(root),
        files,
    })
}

/// 解析 `git status --porcelain=v1 -z` 的 NUL 分隔输出。
///
/// 每条记录格式：`XY <path>`（rename/copy 是 `XY <new>\0<old>`，新路径在前，多吃一个字段）。
fn parse_porcelain_z(root: &Path, raw: &str) -> Vec<GitFileStatus> {
    let mut files = Vec::new();
    let mut fields = raw.split('\u{0}');
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        // 前两字符是 XY，第三字符是空格，其后是路径。
        let bytes = entry.as_bytes();
        if bytes.len() < 3 {
            continue;
        }
        let x = entry[0..1].to_string();
        let y = entry[1..2].to_string();
        let path = entry[3..].to_string();
        // rename / copy：紧跟一个 NUL 字段是旧路径，消费掉但不展示（UI 只看新路径）。
        if x == "R" || x == "C" {
            let _old = fields.next();
        }
        let untracked = x == "?" && y == "?";
        let staged = !untracked && x != " " && x != "?";
        let unstaged = untracked || (y != " ");
        let abs_path = root.join(&path).to_string_lossy().into_owned();
        files.push(GitFileStatus {
            path,
            abs_path,
            x,
            y,
            staged,
            unstaged,
            untracked,
        });
    }
    // dir-first 无意义（都是文件），按路径排序稳定展示。
    files.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
    files
}

/// 取某文件的 diff 两侧文本。
///
/// - `staged=true`：HEAD 版本 vs index 版本（暂存了什么）
/// - `staged=false`：index（无则 HEAD）版本 vs 工作区当前读盘（还没暂存的改动）
///
/// 取不到某侧（新增文件无 HEAD 版本、删除文件无工作区版本）时该侧为空串。
pub fn diff_file(root: &Path, rel_path: &str, staged: bool) -> AppResult<(String, String)> {
    let _ = ensure_within(root, rel_path)?;
    if staged {
        let before = show_or_empty(root, "HEAD", rel_path);
        let after = show_or_empty(root, "", rel_path); // ":<path>" = index
        Ok((before, after))
    } else {
        // 未暂存：基线取 index（暂存区当前），index 取不到再退到 HEAD。
        let before = {
            let idx = show_or_empty(root, "", rel_path);
            if !idx.is_empty() {
                idx
            } else {
                show_or_empty(root, "HEAD", rel_path)
            }
        };
        let abs = root.join(rel_path);
        let after = std::fs::read_to_string(&abs).unwrap_or_default();
        Ok((before, after))
    }
}

/// `git show <rev>:<path>`；rev 为空串表示 index（`:<path>`）。任何失败（文件在该版本不存在）返回空串。
fn show_or_empty(root: &Path, rev: &str, rel_path: &str) -> String {
    let spec = format!("{rev}:{rel_path}");
    run_git(root, &["show", &spec]).unwrap_or_default()
}

/// 暂存单个文件（`git add -- <path>`）。
pub fn stage(root: &Path, rel_path: &str) -> AppResult<()> {
    let _ = ensure_within(root, rel_path)?;
    run_git(root, &["add", "--", rel_path])?;
    Ok(())
}

/// 取消暂存单个文件（`git reset -q HEAD -- <path>`）。可逆。
pub fn unstage(root: &Path, rel_path: &str) -> AppResult<()> {
    let _ = ensure_within(root, rel_path)?;
    run_git(root, &["reset", "-q", "HEAD", "--", rel_path])?;
    Ok(())
}

/// 丢弃单个文件的工作区改动（不可逆）。
/// - tracked：`git checkout -- <path>` 还原到 index/HEAD
/// - untracked：删除该文件（限 root 内）
pub fn discard(root: &Path, rel_path: &str, untracked: bool) -> AppResult<()> {
    let abs = ensure_within(root, rel_path)?;
    if untracked {
        if abs.is_file() {
            std::fs::remove_file(&abs)
                .map_err(|e| AppError::msg(format!("删除未跟踪文件失败: {e}")))?;
        }
        return Ok(());
    }
    run_git(root, &["checkout", "--", rel_path])?;
    Ok(())
}

/// 提交已暂存内容（`git commit -m <message>`，不带 -a / 不自动 add）。返回新 commit 短 sha。
pub fn commit(root: &Path, message: &str) -> AppResult<String> {
    if message.trim().is_empty() {
        return Err(AppError::msg("提交信息不能为空"));
    }
    run_git(root, &["commit", "-m", message])?;
    Ok(run_git(root, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t.com"]);
        git(dir, &["config", "user.name", "t"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn status_classifies_modified_staged_untracked() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_repo(root);
        std::fs::write(root.join("a.txt"), "v1\n").unwrap();
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "-q", "-m", "init"]);

        // 改 a.txt（未暂存）、新增未跟踪 b.txt、暂存改动的 c.txt
        std::fs::write(root.join("a.txt"), "v2\n").unwrap();
        std::fs::write(root.join("b.txt"), "new\n").unwrap();
        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        git(root, &["add", "c.txt"]);

        let st = status(root).unwrap();
        let by = |p: &str| st.files.iter().find(|f| f.path == p).cloned().unwrap();

        let a = by("a.txt");
        assert!(a.unstaged && !a.staged && !a.untracked);
        let b = by("b.txt");
        assert!(b.untracked && b.unstaged);
        let c = by("c.txt");
        assert!(c.staged);
    }

    #[test]
    fn diff_file_unstaged_reads_worktree() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_repo(root);
        std::fs::write(root.join("a.txt"), "v1\n").unwrap();
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "-q", "-m", "init"]);
        std::fs::write(root.join("a.txt"), "v2\n").unwrap();

        let (before, after) = diff_file(root, "a.txt", false).unwrap();
        assert_eq!(before, "v1\n");
        assert_eq!(after, "v2\n");
    }

    #[test]
    fn stage_then_unstage_roundtrip() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_repo(root);
        std::fs::write(root.join("a.txt"), "v1\n").unwrap();
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "-q", "-m", "init"]);
        std::fs::write(root.join("a.txt"), "v2\n").unwrap();

        stage(root, "a.txt").unwrap();
        assert!(status(root).unwrap().files.iter().any(|f| f.path == "a.txt" && f.staged));
        unstage(root, "a.txt").unwrap();
        assert!(status(root).unwrap().files.iter().any(|f| f.path == "a.txt" && !f.staged && f.unstaged));
    }

    #[test]
    fn discard_restores_tracked_and_removes_untracked() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_repo(root);
        std::fs::write(root.join("a.txt"), "v1\n").unwrap();
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "-q", "-m", "init"]);

        std::fs::write(root.join("a.txt"), "v2\n").unwrap();
        discard(root, "a.txt", false).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "v1\n");

        std::fs::write(root.join("b.txt"), "x\n").unwrap();
        discard(root, "b.txt", true).unwrap();
        assert!(!root.join("b.txt").exists());
    }

    #[test]
    fn ensure_within_rejects_escape() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_within(tmp.path(), "../evil").is_err());
    }
}