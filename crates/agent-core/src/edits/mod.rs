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
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use fs2::FileExt;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use common::{AppError, AppResult};

use crate::workspace::Workspace;

pub mod hashline;
pub mod metadata;

use metadata::{load_metadata, save_metadata, worktree_dir, EditEntry};

/// 同进程内每个 real_path 对应一把 async Mutex，dispatch 时同 path 的多个 Edit
/// 在 async 层串行化，**不阻塞 tokio worker**。
/// 跨进程的互斥仍由后面的 fd-lock 负责（架构 §4.13.4）。
type PerPathLocks = AsyncMutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>;

/// fd-lock 单次 try_lock_exclusive 的重试间隔与总超时上限。
/// 超时后返回错误，调用方（dispatcher）`.ok()` 后等价于跳过快照——
/// 与「git 不可用 → enabled=false」是同质降级路径，不阻塞 Edit 本身。
const FD_LOCK_POLL_INTERVAL_MS: u64 = 50;
const FD_LOCK_TIMEOUT_SECS: u64 = 30;

/// 单次 snapshot 的结果。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub sha: String,
    pub file_bytes: u64,
}

/// 文件互斥锁 guard（架构 §4.13.4）。同时持有：
/// - in-process async Mutex guard：保证同进程同 path 串行化
/// - fd-lock 文件：保证跨进程互斥
/// Drop 时按字段声明顺序先释放 fd-lock 再释放 async guard。
pub struct FileLockGuard {
    fd_lock: Option<File>,
    _async_guard: OwnedMutexGuard<()>,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.fd_lock.take() {
            let _ = fs2::FileExt::unlock(&file);
        }
    }
}

pub struct EditsWorktree {
    worktree_dir: PathBuf,
    workspace_root: PathBuf,
    git_available: Mutex<Option<bool>>,
    per_path_locks: PerPathLocks,
}

impl EditsWorktree {
    pub fn new(data_dir: &Path, session_id: &str, workspace: &Workspace) -> Self {
        Self {
            worktree_dir: worktree_dir(data_dir, session_id),
            workspace_root: workspace.workdir().to_path_buf(),
            git_available: Mutex::new(None),
            per_path_locks: AsyncMutex::new(HashMap::new()),
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

    /// 按真实文件绝对路径获取排他锁（架构 §4.13.4）。
    /// 调用方应在 `snapshot_before` 之前获取此锁，`snapshot_after` 之后释放。
    ///
    /// 两层互斥：
    /// 1. **in-process**：先拿 per-path async Mutex，同进程同 path 在 async 层串行，
    ///    不会让 fd-lock 同步 syscall 阻塞 tokio worker。
    /// 2. **inter-process**：再 `spawn_blocking` 调 `try_lock_exclusive` + 50ms 轮询，
    ///    总超时 30s。fd-lock 文件路径：`<worktree>/.locks/<hash(real_path)>.lock`
    ///
    /// 30s 超时返回 `Err`——dispatcher 用 `.ok()` 折叠成 `None`，等价于「跳过快照
    /// 但 Edit 继续」，与 git 不可用时的降级路径同质。
    pub async fn lock_file(&self, real_path: &Path) -> AppResult<FileLockGuard> {
        let mutex = {
            let mut map = self.per_path_locks.lock().await;
            map.entry(real_path.to_path_buf())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let async_guard = mutex.lock_owned().await;

        let lock_path = self.lock_path_for(real_path);
        if let Some(parent) = lock_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AppError::msg(format!("创建 .locks 目录失败 {}: {e}", parent.display()))
            })?;
        }
        let real_path_disp = real_path.display().to_string();
        let fd_lock = tokio::task::spawn_blocking(move || -> AppResult<File> {
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|e| {
                    AppError::msg(format!("打开 lock 文件失败 {}: {e}", lock_path.display()))
                })?;
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(FD_LOCK_TIMEOUT_SECS);
            loop {
                match file.try_lock_exclusive() {
                    Ok(()) => return Ok(file),
                    Err(_) => {
                        if std::time::Instant::now() >= deadline {
                            return Err(AppError::msg(format!(
                                "{}s 内无法获取 {} 的跨进程文件锁",
                                FD_LOCK_TIMEOUT_SECS, real_path_disp
                            )));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(
                            FD_LOCK_POLL_INTERVAL_MS,
                        ));
                    }
                }
            }
        })
        .await
        .map_err(|e| AppError::msg(format!("spawn_blocking join: {e}")))??;

        Ok(FileLockGuard {
            fd_lock: Some(fd_lock),
            _async_guard: async_guard,
        })
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

    /// 回退单次 Edit。按 entry.action 分派：
    /// - `Create`：直接删除真实文件（before 状态本就是「不存在」）
    /// - `Modify` / `Overwrite`：生成 after→before 反向 patch，apply 到当前真实文件
    ///
    /// 反向 patch 路径：
    /// 1. `git diff after before -- <mirrored>`（**保留 stdout 原样**，patch 末尾换行不能丢，否则 `git apply` 报 `corrupt patch`）
    /// 2. 拷贝真实文件 → 镜像位置（apply 的目标 = 当前状态）
    /// 3. `git apply --check` 探测冲突；无冲突则 `git apply`
    /// 4. 拷贝镜像 → 真实文件
    ///
    /// 冲突时拒绝，不动真实文件。
    pub async fn revert(&self, entry: &EditEntry) -> AppResult<()> {
        if !self.enabled().await {
            return Err(AppError::msg("git 不可用，回退功能已禁用"));
        }
        let real_path = Path::new(&entry.real_path);

        // create 的 before 状态 = 文件不存在；patch 路径走不通（before_sha 为空 ref）。
        // 语义上回退 = 删除文件即可。
        if matches!(entry.action, protocol::EditAction::Create) {
            match tokio::fs::remove_file(real_path).await {
                Ok(()) => return Ok(()),
                // 用户已经手动删了——视作回退已达成的等价状态
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => {
                    return Err(AppError::msg(format!(
                        "删除 create 文件失败 {}: {e}",
                        real_path.display()
                    )))
                }
            }
        }

        // Modify / Overwrite：走反向 patch
        if entry.before_sha.is_empty() {
            return Err(AppError::msg(
                "非 create 类型但 before_sha 为空，metadata 损坏",
            ));
        }

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
        // rev-parse 返回 `<sha>\n`；存进 EditEntry 前要 trim，否则字符串 sha 带换行
        // 会污染后续 git 命令拼接。
        let raw = run_git(&self.worktree_dir, &["rev-parse", "HEAD"]).await?;
        Ok(raw.trim().to_string())
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

/// 跑一条 git 命令，返回 **原样** stdout（不 trim）。
///
/// 不能 trim 的原因：`git diff` / `git show` 的输出末尾的 `\n` 是 patch / 文件
/// 内容的一部分，吃掉后 `git apply` 会报 `corrupt patch`；调用方需要 sha 之类
/// 短串时自己 `.trim()`。
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
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
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

    // ── 端到端：snapshot → revert ────────────────────────────────────────
    //
    // 这一组测试固化一个曾经长期 broken 的属性：worktree 的反向 patch 回退
    // 必须真的能把文件改回去。回归点是 `run_git` 之前对 stdout 调用 `.trim()`，
    // 吃掉了 `git diff` 输出末尾的 `\n`，导致 patch 永远 `corrupt`。

    fn fake_entry(
        real: &Path,
        before_sha: String,
        after_sha: String,
        action: protocol::EditAction,
    ) -> metadata::EditEntry {
        metadata::EditEntry {
            snapshot_id: "s".into(),
            call_id: "c".into(),
            tool: "Edit".into(),
            real_path: real.to_string_lossy().to_string(),
            action,
            before_sha,
            after_sha,
            before_bytes: 0,
            after_bytes: 0,
            ts_ms: 0,
            reverted: false,
            reverted_at_ms: None,
        }
    }

    /// 同步检查 git 可用；不可用时打印跳过。所有依赖 git 的测试都走这一关。
    async fn require_git_or_skip(wt: &EditsWorktree) -> bool {
        if !wt.enabled().await {
            eprintln!("跳过：当前环境没有 git CLI");
            return false;
        }
        true
    }

    #[tokio::test]
    async fn snapshot_then_revert_restores_modify() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let real = ws.path().join("foo.txt");
        tokio::fs::write(&real, b"line-1\n").await.unwrap();
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);
        if !require_git_or_skip(&wt).await {
            return;
        }

        let before = wt
            .snapshot_before("c1", &real)
            .await
            .unwrap()
            .expect("before snapshot");
        tokio::fs::write(&real, b"line-1\nline-2\n").await.unwrap();
        let after = wt
            .snapshot_after("c1", &real)
            .await
            .unwrap()
            .expect("after snapshot");

        let entry = fake_entry(&real, before.sha, after.sha, protocol::EditAction::Modify);
        wt.revert(&entry)
            .await
            .expect("revert 应当成功（trim 不再破坏 patch）");

        let got = tokio::fs::read_to_string(&real).await.unwrap();
        assert_eq!(got, "line-1\n", "文件没被回退到 before 状态");
    }

    #[tokio::test]
    async fn revert_create_deletes_file() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let real = ws.path().join("new.txt");
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);
        if !require_git_or_skip(&wt).await {
            return;
        }

        // before：文件不存在，snapshot_before 会因 mirror_file 失败而 Err，
        // dispatch 路径用 unwrap_or(None) 吞掉——这里复现该兼容。
        let before_sha = wt
            .snapshot_before("c2", &real)
            .await
            .ok()
            .flatten()
            .map(|s| s.sha)
            .unwrap_or_default();
        tokio::fs::write(&real, b"hello\n").await.unwrap();
        let after = wt
            .snapshot_after("c2", &real)
            .await
            .unwrap()
            .expect("after snapshot");

        let entry = fake_entry(&real, before_sha, after.sha, protocol::EditAction::Create);
        wt.revert(&entry)
            .await
            .expect("create 类型 revert 应直接删文件");

        assert!(!real.exists(), "create 回退后真实文件应被删除");
    }

    #[tokio::test]
    async fn revert_rejects_when_user_changed_same_line() {
        // 模型 Edit 后用户绕开 Edit 又动了同一段文本——git apply --check 应当冲突，
        // 不动真实文件。这是「保留用户改动」的硬保证。
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let real = ws.path().join("foo.txt");
        tokio::fs::write(&real, b"alpha\nbeta\ngamma\n")
            .await
            .unwrap();
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);
        if !require_git_or_skip(&wt).await {
            return;
        }

        let before = wt.snapshot_before("c3", &real).await.unwrap().unwrap();
        tokio::fs::write(&real, b"alpha\nBETA-EDIT\ngamma\n")
            .await
            .unwrap();
        let after = wt.snapshot_after("c3", &real).await.unwrap().unwrap();

        // 用户在 Edit 改过的那一行再动一下
        tokio::fs::write(&real, b"alpha\nBETA-USER\ngamma\n")
            .await
            .unwrap();

        let entry = fake_entry(&real, before.sha, after.sha, protocol::EditAction::Modify);
        let err = wt.revert(&entry).await.unwrap_err();
        assert!(err.to_string().contains("冲突"), "应当报冲突，实际: {err}");

        let got = tokio::fs::read_to_string(&real).await.unwrap();
        assert_eq!(got, "alpha\nBETA-USER\ngamma\n", "冲突时不能动用户文件");
    }

    #[tokio::test]
    async fn list_entries_works_on_fresh_instance() {
        // 模拟「desktop 重启」：写一份 metadata，然后用全新的 EditsWorktree
        // 实例（sessionStreams 一定不存在）拿 list_entries——后端必须能读到。
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);

        let dir = metadata::worktree_dir(dd.path(), "sid");
        std::fs::create_dir_all(&dir).unwrap();
        let meta = metadata::EditsMetadata {
            version: 1,
            entries: vec![fake_entry(
                Path::new("/tmp/x"),
                String::new(),
                "abcd".into(),
                protocol::EditAction::Create,
            )],
        };
        metadata::save_metadata(&dir, &meta).unwrap();

        let entries = wt.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
    }

    /// 回归：5 个 future 并发拿同 path 的 lock_file 必须在 5s 内全部 ok。
    /// 修复前 `file.lock_exclusive()` 是同步阻塞 syscall 直接吃 tokio worker，
    /// `join_all` 5 个同 path Edit 会让进程死锁；现在两层互斥（in-process async
    /// Mutex + spawn_blocking 包 fd-lock），N 并发应当顺次串行通过。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lock_file_concurrent_same_path_does_not_deadlock() {
        use futures_util::future::join_all;
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let real = ws.path().join("hot.txt");
        tokio::fs::write(&real, b"x").await.unwrap();
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = Arc::new(EditsWorktree::new(dd.path(), "sid", &workspace));

        let mut tasks = Vec::new();
        for _ in 0..5 {
            let wt = wt.clone();
            let real = real.clone();
            tasks.push(async move {
                let _guard = wt.lock_file(&real).await.expect("lock_file");
                // 模拟临界区 IO（用 tokio::time::sleep 不阻塞 worker）
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            });
        }
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(5), join_all(tasks)).await;
        assert!(
            outcome.is_ok(),
            "5 个同 path 并发 lock_file 应在 5s 内全部完成，否则视为死锁回退"
        );
    }
}
