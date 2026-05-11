//! OAuth 凭据目录（架构 §6.1）。
//!
//! 实际 OAuth token 读写当前仍在 `model-gateway::auth`；本模块只提供路径
//! 工具与目录初始化，Step 11/12 后将统一接入 `lock::write_atomic` 形成与
//! 其它 storage 模块一致的并发保护。

use std::path::{Path, PathBuf};

pub fn oauth_dir(data_dir: &Path) -> PathBuf {
    let dir = data_dir.join("oauth");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn provider_token_path(data_dir: &Path, provider_id: &str) -> PathBuf {
    oauth_dir(data_dir).join(format!("{provider_id}.json"))
}
