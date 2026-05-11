//! Global 级权限规则持久化（架构 §4.6.1 / §4.6.3 / §6.1）。
//!
//! 落盘文件：`~/.hebbian/permissions.json`，整体替换写入（走 [`lock::write_atomic`]）。
//! Session 级规则不在本模块——它们写到对应 session 的 `session.jsonl`，由
//! `permissions::store` 模块组装内存视图。
//!
//! 文件格式（架构 §4.5.4 示例）：
//!
//! ```json
//! {
//!   "rules": [
//!     { "id": "...", "scope": "Global", "toolName": "Bash",
//!       "matcher": { "type": "Bash", "commandPrefix": "git" },
//!       "decision": "Allow", "createdAt": 0, "createdBy": "user" }
//!   ]
//! }
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use common::AppResult;

use super::lock;
use crate::permissions::PermissionRule;

const FILE_NAME: &str = "permissions.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PermissionsFile {
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

pub fn load(data_dir: &Path) -> AppResult<PermissionsFile> {
    let p = path(data_dir);
    if !p.exists() {
        return Ok(PermissionsFile::default());
    }
    let bytes = lock::read_locked(&p)?;
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

pub fn save(data_dir: &Path, file: &PermissionsFile) -> AppResult<()> {
    let p = path(data_dir);
    let bytes = serde_json::to_vec_pretty(file)?;
    lock::write_atomic(&p, &bytes)
}
