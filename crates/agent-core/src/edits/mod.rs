//! Edits Worktree（架构 §4.13）。
//!
//! Session 私有的独立 git 仓库，按 turn 边界给 Edit 修改拍快照，支持整轮回退。
//!
//! 核心操作：
//! - [`EditsWorktree::begin_turn`] 在 TurnStarted 后登记当前 turn
//! - [`EditsWorktree::ensure_turn_before`] 在本轮首次触达某文件时拍 before
//! - [`EditsWorktree::commit_turn`] 在 TurnFinished 前拍 after 并写 turn 级 metadata
//! - [`EditsWorktree::revert_turn`] 生成反向 patch 并 apply 到真实文件
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

use metadata::{load_metadata, save_metadata, worktree_dir, RunEditEntry, TurnFileChange};

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

/// 本 Run 内某文件首次触达时的 before 快照。
/// `before_existed=false` 表示触达前文件不存在（本 Run 内新建）。
#[derive(Debug, Clone)]
struct BeforeSnapshot {
    real_path: PathBuf,
    before_existed: bool,
    sha: String,
    file_bytes: u64,
}

#[derive(Debug, Clone)]
struct ActiveRun {
    run_id: String,
    started_at_ms: i64,
    files: HashMap<PathBuf, BeforeSnapshot>,
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
    active_run: AsyncMutex<Option<ActiveRun>>,
}

impl EditsWorktree {
    pub fn new(data_dir: &Path, session_id: &str, workspace: &Workspace) -> Self {
        Self {
            worktree_dir: worktree_dir(data_dir, session_id),
            workspace_root: workspace.workdir().to_path_buf(),
            git_available: Mutex::new(None),
            per_path_locks: AsyncMutex::new(HashMap::new()),
            active_run: AsyncMutex::new(None),
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

    /// RunStarted 后登记当前 Run（零 IO）。重复 begin 覆盖（resume 走同一 run_id）。
    pub async fn begin_run(&self, run_id: &str) {
        let mut active = self.active_run.lock().await;
        if active.as_ref().is_some_and(|r| r.run_id == run_id) {
            return;
        }
        *active = Some(ActiveRun {
            run_id: run_id.to_string(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            files: HashMap::new(),
        });
    }

    /// 本 Run 首次触达某文件时拍 before；同文件后续触达跳过。
    /// 在工具执行**前**调用（删除类命令执行后文件就没了，必须先拍）。
    pub async fn ensure_run_before(&self, run_id: &str, real_path: &Path) -> AppResult<()> {
        if !self.enabled().await {
            return Ok(());
        }
        {
            let active = self.active_run.lock().await;
            if active
                .as_ref()
                .filter(|r| r.run_id == run_id)
                .and_then(|r| r.files.get(real_path))
                .is_some()
            {
                return Ok(());
            }
        }

        self.ensure_init().await?;
        // 触达前文件存在与否决定 before 状态：存在则镜像 + commit 拿 sha；不存在则空 sha。
        let before_existed = tokio::fs::try_exists(real_path).await.unwrap_or(false);
        let snapshot = if before_existed {
            self.snapshot_file(&format!("before:{run_id}"), real_path)
                .await?
        } else {
            Snapshot {
                sha: String::new(),
                file_bytes: 0,
            }
        };

        let mut active = self.active_run.lock().await;
        let run = active
            .as_mut()
            .filter(|r| r.run_id == run_id)
            .ok_or_else(|| AppError::msg("edits-worktree 当前没有匹配的 active run"))?;
        run.files
            .entry(real_path.to_path_buf())
            .or_insert(BeforeSnapshot {
                real_path: real_path.to_path_buf(),
                before_existed,
                sha: snapshot.sha,
                file_bytes: snapshot.file_bytes,
            });
        Ok(())
    }

    /// Run 结束（RunFinished / Cancelled / Failed）后拍 after 并写 metadata。
    /// 遍历本 Run 触达文件，逐个对比；全部无净变化 → 不落 metadata、返回 None（空 Run 不记录）。
    pub async fn finalize_run(&self, run_id: &str) -> AppResult<Option<RunEditEntry>> {
        let run = {
            let mut active = self.active_run.lock().await;
            match active.take() {
                Some(r) if r.run_id == run_id => r,
                other => {
                    *active = other;
                    return Ok(None);
                }
            }
        };
        if !self.enabled().await || run.files.is_empty() {
            return Ok(None);
        }

        self.ensure_init().await?;
        let mut files = Vec::new();
        for before in run.files.into_values() {
            let after_exists = tokio::fs::try_exists(&before.real_path)
                .await
                .unwrap_or(false);

            // 按 before/after 存在性 + 内容推断 action，决定是否记录为净变化。
            let (action, after_sha, after_bytes) = if after_exists {
                let after = self
                    .snapshot_file(&format!("after:{run_id}"), &before.real_path)
                    .await?;
                if !before.before_existed {
                    (protocol::EditAction::Create, after.sha, after.file_bytes)
                } else {
                    // commit sha 含时间戳每次都变，必须按内容 diff 判断是否真有净变化。
                    let patch = self
                        .git_diff(&before.sha, &after.sha, &before.real_path)
                        .await
                        .unwrap_or_default();
                    if patch.trim().is_empty() {
                        continue; // 无净变化，丢弃
                    }
                    (protocol::EditAction::Modify, after.sha, after.file_bytes)
                }
            } else if before.before_existed {
                (protocol::EditAction::Delete, String::new(), 0)
            } else {
                continue; // 触达前后都不存在（如建了又删）——无净变化
            };

            files.push(TurnFileChange {
                real_path: before.real_path.to_string_lossy().to_string(),
                action,
                before_sha: before.sha,
                after_sha,
                before_bytes: before.file_bytes,
                after_bytes,
            });
        }

        if files.is_empty() {
            return Ok(None);
        }

        let entry = RunEditEntry {
            run_id: run.run_id,
            started_at_ms: run.started_at_ms,
            finished_at_ms: chrono::Utc::now().timestamp_millis(),
            files,
            reverted: false,
            reverted_at_ms: None,
        };
        self.append_run(entry.clone())?;
        Ok(Some(entry))
    }

    /// 回退一整个 Run。逐文件按 action 分派：
    /// - `Create`：删除本 Run 新建的文件
    /// - `Delete`：从 before 镜像重建被删文件
    /// - `Modify` / `Overwrite`：after→before 反向 patch apply 到当前真实文件
    pub async fn revert_run(&self, entry: &RunEditEntry) -> AppResult<()> {
        if !self.enabled().await {
            return Err(AppError::msg("git 不可用，回退功能已禁用"));
        }

        for file in &entry.files {
            let real_path = Path::new(&file.real_path);
            let _lock = self.lock_file(real_path).await?;

            match file.action {
                protocol::EditAction::Create => match tokio::fs::remove_file(real_path).await {
                    Ok(()) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        return Err(AppError::msg(format!(
                            "删除 create 文件失败 {}: {e}",
                            real_path.display()
                        )))
                    }
                },
                protocol::EditAction::Delete => {
                    // 从 before 镜像内容重建被删文件
                    if file.before_sha.is_empty() {
                        return Err(AppError::msg("delete 条目缺 before_sha，无法重建"));
                    }
                    let content = self.get_file_at_sha(&file.before_sha, real_path).await?;
                    if let Some(parent) = real_path.parent() {
                        tokio::fs::create_dir_all(parent).await.map_err(|e| {
                            AppError::msg(format!("重建文件父目录失败: {e}"))
                        })?;
                    }
                    tokio::fs::write(real_path, content)
                        .await
                        .map_err(|e| AppError::msg(format!("重建被删文件失败: {e}")))?;
                    continue;
                }
                _ => {}
            }

            if file.before_sha.is_empty() {
                return Err(AppError::msg(
                    "非 create/delete 类型但 before_sha 为空，metadata 损坏",
                ));
            }

            let patch = self
                .git_diff(&file.after_sha, &file.before_sha, real_path)
                .await?;
            if patch.trim().is_empty() {
                continue;
            }
            self.mirror_file(real_path).await?;
            let patch_file = self
                .worktree_dir
                .join(format!(".revert-{}.patch", entry.run_id));
            tokio::fs::write(&patch_file, &patch)
                .await
                .map_err(|e| AppError::msg(format!("写临时 patch 失败: {e}")))?;
            let result = self.git_apply(&patch_file).await;
            let _ = tokio::fs::remove_file(&patch_file).await;
            result?;
            let mirrored = self.mirrored_path(real_path);
            tokio::fs::copy(&mirrored, real_path)
                .await
                .map_err(|e| AppError::msg(format!("回退写入失败: {e}")))?;
        }
        Ok(())
    }

    pub fn list_runs(&self) -> AppResult<Vec<RunEditEntry>> {
        let meta = load_metadata(&self.worktree_dir)?;
        Ok(meta.runs)
    }

    pub fn append_run(&self, entry: RunEditEntry) -> AppResult<()> {
        let mut meta = load_metadata(&self.worktree_dir)?;
        meta.runs.push(entry);
        save_metadata(&self.worktree_dir, &meta)
    }

    pub fn mark_run_reverted(&self, run_id: &str) -> AppResult<()> {
        let mut meta = load_metadata(&self.worktree_dir)?;
        if let Some(entry) = metadata::find_run_mut(&mut meta, run_id) {
            entry.reverted = true;
            entry.reverted_at_ms = Some(chrono::Utc::now().timestamp_millis());
        }
        save_metadata(&self.worktree_dir, &meta)
    }

    /// 取某个 commit 上的文件镜像内容（`git show <sha>:<path>`）。
    pub async fn get_file_at_sha(&self, sha: &str, real_path: &Path) -> AppResult<String> {
        if sha.is_empty() {
            return Ok(String::new());
        }
        let rel = self.mirrored_path_relative(real_path);
        run_git(&self.worktree_dir, &["show", &format!("{sha}:{rel}")]).await
    }

    /// 取 Run 内某个文件对应的 before / after 文本内容。
    pub async fn diff_text(&self, file: &TurnFileChange) -> AppResult<(String, String)> {
        let real_path = Path::new(&file.real_path);
        let before = self.get_file_at_sha(&file.before_sha, real_path).await?;
        let after = self.get_file_at_sha(&file.after_sha, real_path).await?;
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
        // rev-parse 返回 `<sha>\n`；存进 metadata 前要 trim，否则字符串 sha 带换行
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

    async fn snapshot_file(&self, message: &str, real_path: &Path) -> AppResult<Snapshot> {
        self.mirror_file(real_path).await?;
        self.git_add(real_path).await?;
        let sha = self.git_commit(message).await?;
        let file_bytes = self.file_size(real_path).await;
        Ok(Snapshot { sha, file_bytes })
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

    // ── 端到端：Run snapshot → revert ──────────────────────────────────────
    //
    // 这一组测试固化一个曾经长期 broken 的属性：worktree 的反向 patch 回退
    // 必须真的能把文件改回去。回归点是 `run_git` 之前对 stdout 调用 `.trim()`，
    // 吃掉了 `git diff` 输出末尾的 `\n`，导致 patch 永远 `corrupt`。

    /// 同步检查 git 可用；不可用时打印跳过。所有依赖 git 的测试都走这一关。
    async fn require_git_or_skip(wt: &EditsWorktree) -> bool {
        if !wt.enabled().await {
            eprintln!("跳过：当前环境没有 git CLI");
            return false;
        }
        true
    }

    #[tokio::test]
    async fn run_revert_restores_modify_after_multiple_edits() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let real = ws.path().join("foo.txt");
        tokio::fs::write(&real, b"line-1\n").await.unwrap();
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);
        if !require_git_or_skip(&wt).await {
            return;
        }

        wt.begin_run("r1").await;
        wt.ensure_run_before("r1", &real).await.unwrap();
        tokio::fs::write(&real, b"line-1\nline-2\n").await.unwrap();
        wt.ensure_run_before("r1", &real).await.unwrap();
        tokio::fs::write(&real, b"line-1\nline-2\nline-3\n")
            .await
            .unwrap();
        let entry = wt.finalize_run("r1").await.unwrap().expect("run entry");

        assert_eq!(
            entry.files.len(),
            1,
            "同一文件本 Run 多次修改应折叠为一个文件变化"
        );
        assert!(matches!(entry.files[0].action, protocol::EditAction::Modify));
        wt.revert_run(&entry)
            .await
            .expect("run revert 应当成功（trim 不再破坏 patch）");

        let got = tokio::fs::read_to_string(&real).await.unwrap();
        assert_eq!(got, "line-1\n", "文件没被回退到 Run before 状态");
    }

    #[tokio::test]
    async fn run_revert_create_deletes_file() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let real = ws.path().join("new.txt");
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);
        if !require_git_or_skip(&wt).await {
            return;
        }

        wt.begin_run("r2").await;
        wt.ensure_run_before("r2", &real).await.unwrap(); // 触达前不存在
        tokio::fs::write(&real, b"hello\n").await.unwrap();
        let entry = wt.finalize_run("r2").await.unwrap().expect("run entry");

        assert!(matches!(entry.files[0].action, protocol::EditAction::Create));
        wt.revert_run(&entry)
            .await
            .expect("create 类型 run revert 应直接删文件");
        assert!(!real.exists(), "create 回退后真实文件应被删除");
    }

    #[tokio::test]
    async fn run_revert_rebuilds_deleted_file() {
        // rm 删除文件：finalize 标 Delete，revert 从 before 镜像重建。
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let real = ws.path().join("gone.txt");
        tokio::fs::write(&real, b"keep me\n").await.unwrap();
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);
        if !require_git_or_skip(&wt).await {
            return;
        }

        wt.begin_run("r3").await;
        wt.ensure_run_before("r3", &real).await.unwrap(); // 删除前拍 before
        tokio::fs::remove_file(&real).await.unwrap(); // 模拟 rm
        let entry = wt.finalize_run("r3").await.unwrap().expect("run entry");

        assert!(matches!(entry.files[0].action, protocol::EditAction::Delete));
        wt.revert_run(&entry).await.expect("delete revert 应重建文件");
        let got = tokio::fs::read_to_string(&real).await.unwrap();
        assert_eq!(got, "keep me\n", "被删文件应从 before 镜像重建");
    }

    #[tokio::test]
    async fn run_with_no_net_change_records_nothing() {
        // 文件被触达但内容没变（before==after）→ 空 Run，不落 metadata。
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let real = ws.path().join("foo.txt");
        tokio::fs::write(&real, b"same\n").await.unwrap();
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);
        if !require_git_or_skip(&wt).await {
            return;
        }

        wt.begin_run("r4").await;
        wt.ensure_run_before("r4", &real).await.unwrap();
        // 内容不变
        let entry = wt.finalize_run("r4").await.unwrap();
        assert!(entry.is_none(), "无净变化的 Run 不应记录");
        assert!(wt.list_runs().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_revert_rejects_when_user_changed_same_line() {
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

        wt.begin_run("r5").await;
        wt.ensure_run_before("r5", &real).await.unwrap();
        tokio::fs::write(&real, b"alpha\nBETA-EDIT\ngamma\n")
            .await
            .unwrap();
        let entry = wt.finalize_run("r5").await.unwrap().expect("run entry");

        tokio::fs::write(&real, b"alpha\nBETA-USER\ngamma\n")
            .await
            .unwrap();

        let err = wt.revert_run(&entry).await.unwrap_err();
        assert!(err.to_string().contains("冲突"), "应当报冲突，实际: {err}");

        let got = tokio::fs::read_to_string(&real).await.unwrap();
        assert_eq!(got, "alpha\nBETA-USER\ngamma\n", "冲突时不能动用户文件");
    }

    #[tokio::test]
    async fn list_runs_works_on_fresh_instance() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(ws.path(), Vec::new());
        let wt = EditsWorktree::new(dd.path(), "sid", &workspace);

        let dir = metadata::worktree_dir(dd.path(), "sid");
        std::fs::create_dir_all(&dir).unwrap();
        let meta = metadata::EditsMetadata {
            version: 3,
            runs: vec![metadata::RunEditEntry {
                run_id: "r".into(),
                started_at_ms: 0,
                finished_at_ms: 1,
                files: vec![metadata::TurnFileChange {
                    real_path: "/tmp/x".into(),
                    action: protocol::EditAction::Create,
                    before_sha: String::new(),
                    after_sha: "abcd".into(),
                    before_bytes: 0,
                    after_bytes: 4,
                }],
                reverted: false,
                reverted_at_ms: None,
            }],
        };
        metadata::save_metadata(&dir, &meta).unwrap();

        let runs = wt.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
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
