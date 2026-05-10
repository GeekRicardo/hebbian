//! Workspace：单次对话的文件访问边界。
//!
//! 每个对话有独立的 workspace（默认 `~/`）。允许目录分三层：
//!
//! - **initial**：对话起始时锁定的快照。**只有这一层会进首条 user message 的
//!   `<environment>` 块**（由 [`crate::system_prompt::EnvironmentSnapshot`] 渲染）。
//! - **runtime_announced**：对话开始之后追加的目录，且已经通过上一条 user message
//!   的 `<workspace-update>` 通知过模型。仅用于 `allows()` 判定。
//! - **runtime_pending**：刚追加、还没通知模型的目录。下一次 [`take_pending_announcement`]
//!   会把它们 drain 出来供上层注入到下条 user message，然后移入 announced。
//!
//! workspace 数据**完全不进 system 段**。system 段由 [`crate::system_prompt`] 单独管理，
//! 跨会话保持字节稳定，方便 prompt cache 命中。
//!
//! [`take_pending_announcement`]: Workspace::take_pending_announcement

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Debug)]
pub struct Workspace {
    workdir: PathBuf,
    /// 对话起始时确定，整个生命周期不变。进 system XML。
    initial_allowed_dirs: Vec<PathBuf>,
    /// 已通知模型的运行时追加目录。仅 `allows()` 使用。
    runtime_announced: RwLock<Vec<PathBuf>>,
    /// 还没通知模型的运行时追加目录。下次 user message 注入。
    runtime_pending: Mutex<Vec<PathBuf>>,
}

impl Workspace {
    /// 默认 workspace：`~/`，无额外允许目录。
    pub fn home_default() -> Arc<Self> {
        Self::with_runtime_state(
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// 全新会话使用：只给 initial。
    pub fn new(workdir: impl Into<PathBuf>, initial_allowed_dirs: Vec<PathBuf>) -> Arc<Self> {
        Self::with_runtime_state(workdir, initial_allowed_dirs, Vec::new(), Vec::new())
    }

    /// 从持久化恢复：把上次落盘的 announced + pending 一并塞回来。
    /// 三个集合之间相互去重——pending 中已存在于 initial / announced 的项会被剔除，
    /// announced 中已存在于 initial 的项也会被剔除，避免重复通知 / 重复 allows() 检查。
    pub fn with_runtime_state(
        workdir: impl Into<PathBuf>,
        initial_allowed_dirs: Vec<PathBuf>,
        runtime_announced: Vec<PathBuf>,
        runtime_pending: Vec<PathBuf>,
    ) -> Arc<Self> {
        let initial = dedup(initial_allowed_dirs);
        let announced: Vec<PathBuf> = dedup(runtime_announced)
            .into_iter()
            .filter(|p| !initial.contains(p))
            .collect();
        let pending: Vec<PathBuf> = dedup(runtime_pending)
            .into_iter()
            .filter(|p| !initial.contains(p) && !announced.contains(p))
            .collect();
        Arc::new(Self {
            workdir: workdir.into(),
            initial_allowed_dirs: initial,
            runtime_announced: RwLock::new(announced),
            runtime_pending: Mutex::new(pending),
        })
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn initial_allowed_dirs(&self) -> &[PathBuf] {
        &self.initial_allowed_dirs
    }

    pub fn runtime_announced_snapshot(&self) -> Vec<PathBuf> {
        self.runtime_announced.read().unwrap().clone()
    }

    pub fn runtime_pending_snapshot(&self) -> Vec<PathBuf> {
        self.runtime_pending.lock().unwrap().clone()
    }

    /// initial + announced + pending 合并去重，UI / 描述用。
    pub fn allowed_dirs_snapshot(&self) -> Vec<PathBuf> {
        let mut out = self.initial_allowed_dirs.clone();
        for p in self.runtime_announced.read().unwrap().iter() {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        for p in self.runtime_pending.lock().unwrap().iter() {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        out
    }

    /// 运行时扩展允许目录：已存在则跳过，否则进 pending（下次 user message 通知模型）。
    pub fn add_allowed_dir(&self, path: impl Into<PathBuf>) {
        let path = path.into();
        if self.initial_allowed_dirs.iter().any(|d| d == &path) {
            return;
        }
        if self.runtime_announced.read().unwrap().iter().any(|d| d == &path) {
            return;
        }
        let mut pending = self.runtime_pending.lock().unwrap();
        if !pending.iter().any(|d| d == &path) {
            pending.push(path);
        }
    }

    /// 把 pending 全部移入 announced，返回被移走的列表（顺序保留）。
    /// 上层在 `Session::append_user` 之前调用，用于在下条 user message 头部
    /// 注入 `<workspace-update>` 通知模型。
    pub fn take_pending_announcement(&self) -> Vec<PathBuf> {
        let mut pending = self.runtime_pending.lock().unwrap();
        if pending.is_empty() {
            return Vec::new();
        }
        let drained: Vec<PathBuf> = pending.drain(..).collect();
        let mut announced = self.runtime_announced.write().unwrap();
        for p in &drained {
            if !announced.iter().any(|d| d == p) {
                announced.push(p.clone());
            }
        }
        drained
    }

    /// 路径是否在允许范围内：先 canonicalize 再做前缀匹配，防止 `..` 绕过。
    /// canonicalize 失败时退回到 `canonicalize_lossy`（处理"打算写入但还未创建"的场景）。
    pub fn allows(&self, path: &Path) -> bool {
        let canon = canonicalize_lossy(path);
        if canon.starts_with(canonicalize_lossy(&self.workdir)) {
            return true;
        }
        for root in &self.initial_allowed_dirs {
            if canon.starts_with(canonicalize_lossy(root)) {
                return true;
            }
        }
        for root in self.runtime_announced.read().unwrap().iter() {
            if canon.starts_with(canonicalize_lossy(root)) {
                return true;
            }
        }
        for root in self.runtime_pending.lock().unwrap().iter() {
            if canon.starts_with(canonicalize_lossy(root)) {
                return true;
            }
        }
        false
    }

    /// 给 Bash 用：解析 cwd 字段。未指定 → workdir。
    pub fn resolve_cwd(&self, cwd: Option<&str>) -> PathBuf {
        match cwd {
            None | Some("") => self.workdir.clone(),
            Some(p) => PathBuf::from(p),
        }
    }

    /// UI/错误提示用的人类可读描述。
    pub fn describe(&self) -> String {
        let mut s = format!("workdir: {}", self.workdir.display());
        let all = self.allowed_dirs_snapshot();
        if !all.is_empty() {
            s.push_str("\nallowed_dirs:");
            for d in &all {
                s.push_str(&format!("\n  - {}", d.display()));
            }
        }
        s
    }
}

fn dedup(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::with_capacity(v.len());
    v.retain(|p| {
        if seen.iter().any(|s| s == p) {
            false
        } else {
            seen.push(p.clone());
            true
        }
    });
    v
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    // 路径或祖先不存在时（比如 Write 之前的目标文件），向上找第一个存在的祖先做 canonicalize，
    // 然后把剩余的相对部分原样拼回来——这样不存在的尾部不影响越界判定。
    let mut suffix: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(p) = std::fs::canonicalize(cur) {
            let mut out = p;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                suffix.push(name);
                cur = parent;
            }
            _ => break,
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_paths_inside_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path(), Vec::new());

        assert!(ws.allows(&tmp.path().join("a.txt")));
        assert!(ws.allows(&tmp.path().join("nested/dir/b.txt")));
    }

    #[test]
    fn rejects_paths_outside_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path(), Vec::new());

        assert!(!ws.allows(Path::new("/etc/passwd")));
    }

    #[test]
    fn add_allowed_dir_takes_effect_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path(), Vec::new());

        assert!(!ws.allows(&extra.path().join("x")));
        ws.add_allowed_dir(extra.path());
        assert!(ws.allows(&extra.path().join("x")));
    }

    #[test]
    fn add_allowed_dir_lands_in_pending_until_announced() {
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path(), Vec::new());

        ws.add_allowed_dir(extra.path());
        assert!(ws.runtime_announced_snapshot().is_empty());
        assert_eq!(ws.runtime_pending_snapshot().len(), 1);

        let drained = ws.take_pending_announcement();
        assert_eq!(drained.len(), 1);
        assert_eq!(ws.runtime_announced_snapshot().len(), 1);
        assert!(ws.runtime_pending_snapshot().is_empty());

        // 再 take 不会重复
        assert!(ws.take_pending_announcement().is_empty());
    }

    #[test]
    fn add_allowed_dir_skips_existing_in_initial_or_announced() {
        let tmp = tempfile::tempdir().unwrap();
        let already = tempfile::tempdir().unwrap();
        let ws = Workspace::with_runtime_state(
            tmp.path(),
            vec![already.path().to_path_buf()],
            Vec::new(),
            Vec::new(),
        );
        ws.add_allowed_dir(already.path());
        assert!(ws.runtime_pending_snapshot().is_empty());
    }

}
