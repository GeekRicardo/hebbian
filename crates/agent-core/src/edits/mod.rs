//! Edits Worktree（架构 §4.13）。
//!
//! Session 私有的独立 git 仓库，给每次 Edit 调用拍快照，支持单次精确回退。
//!
//! 核心操作：
//! - [`EditsWorktree::snapshot_before`] / [`EditsWorktree::snapshot_after`]
//!   在 Edit 执行前后包夹调用，把真实文件镜像进 worktree 并 git commit
//! - [`EditsWorktree::revert`] 生成反向 patch 并 apply 到真实文件
//! - 无 git CLI → enabled=false，整套机制降级，不阻塞 Edit 本身

use std::collections::hash_map::DefaultHasher;
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fs2::FileExt;

use common::{AppError, AppResult};

use crate::workspace::Workspace;

pub mod metadata;

use metadata::{load_metadata, save_metadata, worktree_dir, EditEntry};

/// 单次 snapshot 的结果。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub sha: String,
    pub file_bytes: u64,
}

/// 文件互斥锁 guard（架构 §4.13.4）。按真实文件路径排他锁，确保
/// 同一文件的 snapshot + execute + snapshot_after 不被并发打断。
/// Drop 时自动释放 fs2 锁。
pub struct FileLockGuard {
    _file: File,
}

pub struct EditsWorktree {
    worktree_dir: PathBuf,
    workspace_root: PathBuf,
    git_available: Mutex<Option<bool>>,
}

impl EditsWorktree {
    pub fn new(data_dir: &Path, session_id: &str, workspace: &Workspace) -> Self {
        Self {
            worktree_dir: worktree_dir(data_dir, session_id),
            workspace_root: workspace.workdir().to_path_buf(),
            git_available: Mutex::new(None),
        }
    }

    /// edits-worktree 是否可用。首次调用检测 git，结果缓存。
    pub async fn enabled(&self) -> bool {
        {
            let cached = self.git_available.lock().unwrap();
            if let Some(v) = *cached {
                return v;
            }
        }
        let ok = check_git().await;
        *self.git_available.lock().unwrap() = Some(ok);
        ok
    }

    /// 按真实文件绝对路径获取排他 fd-lock（架构 §4.13.4）。
    /// 调用方应在 `snapshot_before` 之前获取此锁，`snapshot_after` 之后释放。
    /// lock 文件路径：`<worktree>/.locks/<hash(real_path)>.lock`
    pub fn lock_file(&self, real_path: &Path) -> AppResult<FileLockGuard> {
        let lock_path = self.lock_path_for(real_path);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        file.lock_exclusive()
            .map_err(|e| AppError::msg(format!("获取文件锁失败 {}: {e}", real_path.display())))?;
        Ok(FileLockGuard { _file: file })
    }

    /// Edit 执行前快照。git 不可用时返回 `Ok(None)`——调用方跳过，不阻塞 Edit。
    pub async fn snapshot_before(
        &self,
        call_id: &str,
        real_path: &Path,
    ) -> AppResult<Option<Snapshot>> {
        if !self.enabled().await {
            return Ok(None);
        }
        self.ensure_init().await?;
        self.mirror_file(real_path).await?;
        self.git_add(real_path).await?;
        let sha = self.git_commit(&format!("before:{call_id}")).await?;
        let file_bytes = self.file_size(real_path).await;
        Ok(Some(Snapshot { sha, file_bytes }))
    }

    /// Edit 执行后快照。
    pub async fn snapshot_after(
        &self,
        call_id: &str,
        real_path: &Path,
    ) -> AppResult<Option<Snapshot>> {
        if !self.enabled().await {
            return Ok(None);
        }
        self.ensure_init().await?;
        self.mirror_file(real_path).await?;
        self.git_add(real_path).await?;
        let sha = self.git_commit(&format!("after:{call_id}")).await?;
        let file_bytes = self.file_size(real_path).await;
        Ok(Some(Snapshot { sha, file_bytes }))
    }

    /// 回退单次 Edit：
    /// 1. 生成反向 patch（`git diff after before -- <mirrored>`）
    /// 2. 拷贝真实文件 → 镜像位置（备份当前状态）
    /// 3. `git apply --check` 探测冲突；无冲突则 `git apply`
    /// 4. 拷贝镜像 → 真实文件
    ///
    /// 冲突时拒绝，不动真实文件。
    pub async fn revert(&self, entry: &EditEntry) -> AppResult<()> {
        if !self.enabled().await {
            return Err(AppError::msg("git 不可用，回退功能已禁用"));
        }
        let real_path = Path::new(&entry.real_path);

        let patch = self
            .git_diff(&entry.after_sha, &entry.before_sha, real_path)
            .await?;

        if patch.trim().is_empty() {
            return Err(AppError::msg("回退 patch 为空"));
        }

        // 拷贝真实文件 → 镜像，作为 apply 的目标
        self.mirror_file(real_path).await?;

        // 写临时 patch 文件
        let patch_file = self.worktree_dir.join(".revert.patch");
        tokio::fs::write(&patch_file, &patch)
            .await
            .map_err(|e| AppError::msg(format!("写临时 patch 失败: {e}")))?;

        let result = self.git_apply(&patch_file).await;
        let _ = tokio::fs::remove_file(&patch_file).await;

        match result {
            Ok(()) => {
                // 拷贝镜像 → 真实文件
                let mirrored = self.mirrored_path(real_path);
                tokio::fs::copy(&mirrored, real_path)
                    .await
                    .map_err(|e| AppError::msg(format!("回退写入失败: {e}")))?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn list_entries(&self) -> AppResult<Vec<EditEntry>> {
        let meta = load_metadata(&self.worktree_dir)?;
        Ok(meta.entries)
    }

    pub fn append_entry(&self, entry: EditEntry) -> AppResult<()> {
        let mut meta = load_metadata(&self.worktree_dir)?;
        meta.entries.push(entry);
        save_metadata(&self.worktree_dir, &meta)
    }

    pub fn mark_reverted(&self, snapshot_id: &str) -> AppResult<()> {
        let mut meta = load_metadata(&self.worktree_dir)?;
        if let Some(entry) = metadata::find_entry_mut(&mut meta, snapshot_id) {
            entry.reverted = true;
            entry.reverted_at_ms = Some(chrono::Utc::now().timestamp_millis());
        }
        save_metadata(&self.worktree_dir, &meta)
    }

    /// 取某个 commit 上的文件镜像内容（`git show <sha>:<path>`）。
    pub async fn get_file_at_sha(&self, sha: &str, real_path: &Path) -> AppResult<String> {
        let rel = self.mirrored_path_relative(real_path);
        run_git(&self.worktree_dir, &["show", &format!("{sha}:{rel}")]).await
    }

    /// 取 entry 对应的 before / after 文本内容。
    pub async fn diff_text(&self, entry: &EditEntry) -> AppResult<(String, String)> {
        let real_path = Path::new(&entry.real_path);
        let before = self.get_file_at_sha(&entry.before_sha, real_path).await?;
        let after = self.get_file_at_sha(&entry.after_sha, real_path).await?;
        Ok((before, after))
    }
}

// ── 内部 helper ────────────────────────────────────────────────────────────

impl EditsWorktree {
    fn lock_path_for(&self, real_path: &Path) -> PathBuf {
        let hash = hash_path(real_path);
        self.worktree_dir
            .join(".locks")
            .join(format!("{hash}.lock"))
    }

    async fn ensure_init(&self) -> AppResult<()> {
        if self.worktree_dir.join(".git").exists() {
            return Ok(());
        }
        tokio::fs::create_dir_all(&self.worktree_dir)
            .await
            .map_err(|e| AppError::msg(format!("创建 edits-worktree 目录失败: {e}")))?;
        run_git(&self.worktree_dir, &["init"]).await?;
        run_git(
            &self.worktree_dir,
            &["config", "user.email", "edits-worktree@hebbian.local"],
        )
        .await?;
        run_git(
            &self.worktree_dir,
            &["config", "user.name", "Hebbian Edit Tracker"],
        )
        .await?;
        Ok(())
    }

    async fn mirror_file(&self, real_path: &Path) -> AppResult<()> {
        let mirrored = self.mirrored_path(real_path);
        if let Some(parent) = mirrored.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AppError::msg(format!(
                    "创建 worktree 子目录失败 {}: {e}",
                    parent.display()
                ))
            })?;
        }
        tokio::fs::copy(real_path, &mirrored).await.map_err(|e| {
            AppError::msg(format!(
                "镜像文件失败 {} → {}: {e}",
                real_path.display(),
                mirrored.display()
            ))
        })?;
        Ok(())
    }

    async fn git_add(&self, real_path: &Path) -> AppResult<()> {
        let rel = self.mirrored_path_relative(real_path);
        run_git(&self.worktree_dir, &["add", "--", &rel]).await?;
        Ok(())
    }

    async fn git_commit(&self, message: &str) -> AppResult<String> {
        run_git(
            &self.worktree_dir,
            &["commit", "--allow-empty", "-m", message],
        )
        .await?;
        run_git(&self.worktree_dir, &["rev-parse", "HEAD"]).await
    }

    async fn git_diff(&self, from_sha: &str, to_sha: &str, real_path: &Path) -> AppResult<String> {
        let rel = self.mirrored_path_relative(real_path);
        run_git(&self.worktree_dir, &["diff", from_sha, to_sha, "--", &rel]).await
    }

    async fn git_apply(&self, patch_file: &Path) -> AppResult<()> {
        let patch_file = patch_file.to_path_buf();
        let worktree_dir = self.worktree_dir.clone();

        let output = tokio::task::spawn_blocking(move || {
            // --check 先探测冲突
            let check = std::process::Command::new("git")
                .args(["apply", "--check"])
                .arg(&patch_file)
                .current_dir(&worktree_dir)
                .output()
                .map_err(|e| AppError::msg(format!("git apply --check: {e}")))?;

            if !check.status.success() {
                let stderr = String::from_utf8_lossy(&check.stderr);
                return Err(AppError::msg(format!("回退冲突：{stderr}")));
            }

            let apply = std::process::Command::new("git")
                .args(["apply"])
                .arg(&patch_file)
                .current_dir(&worktree_dir)
                .output()
                .map_err(|e| AppError::msg(format!("git apply: {e}")))?;

            if !apply.status.success() {
                let stderr = String::from_utf8_lossy(&apply.stderr);
                return Err(AppError::msg(format!("apply patch 失败: {stderr}")));
            }
            Ok(())
        })
        .await
        .map_err(|e| AppError::msg(format!("spawn_blocking join: {e}")))?;

        output
    }

    async fn file_size(&self, real_path: &Path) -> u64 {
        tokio::fs::metadata(real_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    }

    fn mirrored_path(&self, real_path: &Path) -> PathBuf {
        if let Ok(rel) = real_path.strip_prefix(&self.workspace_root) {
            self.worktree_dir.join(rel)
        } else {
            let hash = hash_path(real_path);
            let basename = real_path.file_name().unwrap_or_default();
            self.worktree_dir
                .join("_external")
                .join(hash)
                .join(basename)
        }
    }

    fn mirrored_path_relative(&self, real_path: &Path) -> String {
        let mirrored = self.mirrored_path(real_path);
        mirrored
            .strip_prefix(&self.worktree_dir)
            .unwrap_or(&mirrored)
            .to_string_lossy()
            .to_string()
    }
}

// ── 自由函数 ────────────────────────────────────────────────────────────────

async fn check_git() -> bool {
    tokio::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn run_git(dir: &Path, args: &[&str]) -> AppResult<String> {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let error_label = args.join(" ");

    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .output()
            .map_err(|e| AppError::msg(format!("git {} 失败: {e}", args.join(" "))))
    })
    .await
    .map_err(|e| AppError::msg(format!("spawn_blocking join: {e}")))?;

    let output = output?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::msg(format!(
            "git {error_label} 退出码非零: {stderr}"
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn hash_path(path: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored_path_inside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_root = tmp.path().join("proj");
        std::fs::create_dir_all(&ws_root).unwrap();
        let ws = Workspace::new(&ws_root, Vec::new());
        let wt = EditsWorktree::new(tmp.path(), "sid1", &ws);

        let real = ws_root.join("src/main.rs");
        let mirrored = wt.mirrored_path(&real);
        assert!(mirrored.to_string_lossy().contains("src/main.rs"));
    }

    #[test]
    fn mirrored_path_outside_workspace_uses_external_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_root = tmp.path().join("proj");
        std::fs::create_dir_all(&ws_root).unwrap();
        let ws = Workspace::new(&ws_root, Vec::new());
        let wt = EditsWorktree::new(tmp.path(), "sid1", &ws);

        let real = Path::new("/etc/hosts");
        let mirrored = wt.mirrored_path(real);
        assert!(mirrored.to_string_lossy().contains("_external"));
        assert!(mirrored.ends_with("hosts"));
    }
}
