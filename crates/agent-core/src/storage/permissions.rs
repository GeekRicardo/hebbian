//! Global / Project 级权限规则持久化（架构 §4.6 / §6.1.2）。
//!
//! 文件结构（两层同形，Claude Code 风格）：
//!
//! ```json
//! {
//!   "allow": ["Bash(xargs)", "Edit(/Users/x/proj)", "WebFetch(github.com)"],
//!   "deny":  ["Bash(rm -rf /)"],
//!   "paths": ["/etc/hosts"]
//! }
//! ```
//!
//! - **allow / deny**：字符串 pattern 数组，语法 `<Tool>(<arg>)` 或 `<Tool>`（任意调用）；
//!   解析与匹配逻辑见 [`crate::permissions::Permission`]
//! - **paths**：扩展可访问路径白名单，与工具维度的 allow/deny 正交（架构 §6.1.2 §4.6.4）
//!
//! Scope 由**文件位置**隐含决定，不再写入字段：
//! - `~/.hebbian/permissions.json` → Global
//! - `~/.hebbian/projects/<encode(workdir)>/permissions.json` → Project
//!
//! Session 级规则仅在 PermissionStore 内存视图中，不落 jsonl（架构 §4.6.2）。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use common::AppResult;

use super::lock;
use super::projects;

const FILE_NAME: &str = "permissions.json";

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct PermissionsFile {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

pub fn global_path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

pub fn project_path(data_dir: &Path, workdir: &Path) -> PathBuf {
    projects::project_dir(data_dir, workdir).join(FILE_NAME)
}

pub fn project_path_by_id(data_dir: &Path, id: &str) -> PathBuf {
    projects::project_dir_by_id(data_dir, id).join(FILE_NAME)
}

pub fn global_mtime(data_dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(global_path(data_dir))
        .and_then(|m| m.modified())
        .ok()
}

pub fn project_mtime(data_dir: &Path, workdir: Option<&Path>) -> Option<SystemTime> {
    let workdir = workdir?;
    std::fs::metadata(project_path(data_dir, workdir))
        .and_then(|m| m.modified())
        .ok()
}

pub fn load_global(data_dir: &Path) -> AppResult<PermissionsFile> {
    load_at(&global_path(data_dir))
}

pub fn load_project(data_dir: &Path, workdir: &Path) -> AppResult<PermissionsFile> {
    load_at(&project_path(data_dir, workdir))
}

pub fn save_global(data_dir: &Path, file: &PermissionsFile) -> AppResult<()> {
    save_at(&global_path(data_dir), file)
}

pub fn save_project(data_dir: &Path, workdir: &Path, file: &PermissionsFile) -> AppResult<()> {
    let dir = projects::project_dir(data_dir, workdir);
    std::fs::create_dir_all(&dir)?;
    save_at(&dir.join(FILE_NAME), file)
}

fn load_at(path: &Path) -> AppResult<PermissionsFile> {
    if !path.exists() {
        return Ok(PermissionsFile::default());
    }
    let bytes = lock::read_locked(path)?;
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

fn save_at(path: &Path, file: &PermissionsFile) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(file)?;
    lock::write_atomic(path, &bytes)
}
