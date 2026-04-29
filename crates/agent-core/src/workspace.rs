//! Workspace：单次对话的文件访问边界。
//!
//! 每个对话有独立的 workspace（默认 `~/`），用户可以在对话设置里追加
//! `allowed_dirs`。Bash / Read / Write / Grep 的所有文件路径都必须落在
//! `workdir ∪ allowed_dirs` 任一目录的子树内；越界路径由 agent_loop 走
//! `PermissionGate` 走审批流程，用户可选择「允许一次」或「允许并加入 allowed_dirs」。
//!
//! 内部用 `RwLock` 包 allowed_dirs，所以审批通过后可以**运行时**追加目录，
//! 同一 run 后续工具调用立即生效。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Debug)]
pub struct Workspace {
    workdir: PathBuf,
    allowed_dirs: RwLock<Vec<PathBuf>>,
}

impl Workspace {
    /// 默认 workspace：`~/`，无额外允许目录
    pub fn home_default() -> Arc<Self> {
        Arc::new(Self {
            workdir: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            allowed_dirs: RwLock::new(Vec::new()),
        })
    }

    pub fn new(workdir: impl Into<PathBuf>, allowed_dirs: Vec<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            workdir: workdir.into(),
            allowed_dirs: RwLock::new(allowed_dirs),
        })
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn allowed_dirs_snapshot(&self) -> Vec<PathBuf> {
        self.allowed_dirs.read().unwrap().clone()
    }

    /// 运行时扩展允许目录（用户审批 AllowAndRemember 时调用）
    pub fn add_allowed_dir(&self, path: impl Into<PathBuf>) {
        let path = path.into();
        let mut dirs = self.allowed_dirs.write().unwrap();
        if !dirs.iter().any(|d| d == &path) {
            dirs.push(path);
        }
    }

    /// 判断路径是否在允许范围内。先 canonicalize 再做前缀匹配，
    /// 防止 `..` 绕过；canonicalize 失败时退回到原始路径（处理"打算写入
    /// 但还未创建"的场景，由父目录守住边界）。
    pub fn allows(&self, path: &Path) -> bool {
        let canon = canonicalize_lossy(path);
        if canon.starts_with(canonicalize_lossy(&self.workdir)) {
            return true;
        }
        let dirs = self.allowed_dirs.read().unwrap();
        dirs.iter()
            .any(|root| canon.starts_with(canonicalize_lossy(root)))
    }

    /// 给 Bash 用：解析 cwd 字段。未指定 → workdir。
    pub fn resolve_cwd(&self, cwd: Option<&str>) -> PathBuf {
        match cwd {
            None | Some("") => self.workdir.clone(),
            Some(p) => PathBuf::from(p),
        }
    }

    /// 注入到 system prompt 的 XML 片段
    pub fn to_system_xml(&self) -> String {
        let mut s = String::from("<workspace>\n");
        s.push_str(&format!(
            "  <workdir>{}</workdir>\n",
            self.workdir.display()
        ));
        let dirs = self.allowed_dirs.read().unwrap();
        for d in dirs.iter() {
            s.push_str(&format!("  <allowed_dir>{}</allowed_dir>\n", d.display()));
        }
        s.push_str(
            "  <note>读写、Bash 工具默认只能在以上目录中操作。\
             越界访问会触发用户审批；如需长期允许，请在对话设置里追加 allowed_dirs。</note>\n",
        );
        s.push_str("</workspace>");
        s
    }

    /// UI/错误提示用的人类可读描述
    pub fn describe(&self) -> String {
        let mut s = format!("workdir: {}", self.workdir.display());
        let dirs = self.allowed_dirs.read().unwrap();
        if !dirs.is_empty() {
            s.push_str("\nallowed_dirs:");
            for d in dirs.iter() {
                s.push_str(&format!("\n  - {}", d.display()));
            }
        }
        s
    }
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
    fn system_xml_includes_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path(), vec![extra.path().to_path_buf()]);
        let xml = ws.to_system_xml();
        assert!(xml.contains("<workspace>"));
        assert!(xml.contains(&tmp.path().to_string_lossy().to_string()));
        assert!(xml.contains(&extra.path().to_string_lossy().to_string()));
    }
}
