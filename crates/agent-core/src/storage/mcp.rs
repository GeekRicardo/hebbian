//! MCP server 配置（`mcp.json`）。
//!
//! 落盘到 `<data_dir>/mcp.json`，兼容 Claude 风格 `mcpServers` 与较新的
//! `servers` 键。读取失败时返回空配置，保存时走文件锁 + 原子写。

use std::path::{Path, PathBuf};

use common::AppResult;

pub use crate::mcp::config::McpConfig;

const FILE_NAME: &str = "mcp.json";

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

pub fn load(data_dir: &Path) -> McpConfig {
    let p = path(data_dir);
    if !p.exists() {
        return McpConfig::default();
    }
    let text = match crate::storage::lock::read_locked(&p) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => return McpConfig::default(),
    };
    McpConfig::parse_json(&text).unwrap_or_default()
}

pub fn save(data_dir: &Path, config: &McpConfig) -> AppResult<()> {
    std::fs::create_dir_all(data_dir)?;
    let text = serde_json::to_string_pretty(config)?;
    crate::storage::lock::write_atomic(&path(data_dir), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hebbian-mcp-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tmp("roundtrip");
        let cfg = McpConfig::parse_json(
            r#"{"mcpServers":{"fs":{"command":"npx","args":["-y","server"]}}}"#,
        )
        .unwrap();

        save(&dir, &cfg).unwrap();
        let loaded = load(&dir);

        assert_eq!(loaded.mcp_servers.len(), 1);
        assert_eq!(loaded.mcp_servers["fs"].command.as_deref(), Some("npx"));
    }

    #[test]
    fn invalid_file_loads_empty_config() {
        let dir = tmp("invalid");
        std::fs::write(dir.join(FILE_NAME), "{").unwrap();

        assert!(load(&dir).mcp_servers.is_empty());
    }
}
