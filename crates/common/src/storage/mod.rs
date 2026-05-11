//! 通用 JSON 读写 helper + 仅供 model-gateway 使用的 `providers.json` 路径。
//!
//! 业务持久化（sessions / prompts / settings / permissions / oauth / 文件锁 /
//! sessions 目录化 / surface settings 等）按架构 §6.2 全部归属
//! [`agent_core::storage`]，本模块只保留最小公共原语，避免 `model-gateway`
//! 反向依赖 `agent-core`。

use crate::AppResult;
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

/// `~/.hebbian/providers.json` 路径。`model-gateway` 读写供应商列表用。
pub fn providers_path(data_dir: &Path) -> PathBuf {
    data_dir.join("providers.json")
}

pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> AppResult<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn read_json_required<T: DeserializeOwned>(path: &Path) -> AppResult<T> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
