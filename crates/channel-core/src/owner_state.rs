//! 机主状态：当前活跃 session / provider / model / project。
//!
//! 连接微信的就是机主本人，拥有整个 hebbian 的全权限；这里不是多用户映射。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OwnerState {
    pub active_session_id: Option<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub project_id: Option<String>,
}

impl OwnerState {
    /// 持久化路径：`~/.hebbian/channels/<channel>/<account_id>/state.json`。
    pub fn path(data_dir: &Path, channel: &str, account_id: &str) -> PathBuf {
        data_dir
            .join("channels")
            .join(channel)
            .join(account_id)
            .join("state.json")
    }

    pub fn load(data_dir: &Path, channel: &str, account_id: &str) -> Self {
        let path = Self::path(data_dir, channel, account_id);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path, channel: &str, account_id: &str) -> anyhow::Result<()> {
        let path = Self::path(data_dir, channel, account_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
