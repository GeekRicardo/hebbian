//! Surface 各自的设置（架构 §7.3）。
//!
//! 全局通用项仍在 `~/.hebbian/settings.json`（由 [`common::config::settings`] 管）；
//! 影响"怎么显示和默认行为"的项落到 surface 各自文件：
//!
//! - `~/.hebbian/desktop-settings.json`
//! - `~/.hebbian/cli-settings.json`
//!
//! 字段宽松：用 `serde_json::Value` 承载，让 Desktop / CLI 各自决定具体形状，
//! 避免本模块跟前端 schema 绑死。读时若文件不存在或损坏，返回空对象。

use std::path::{Path, PathBuf};

use serde_json::Value;

use common::AppResult;

use super::lock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Desktop,
    Cli,
}

impl Surface {
    fn file_name(self) -> &'static str {
        match self {
            Surface::Desktop => "desktop-settings.json",
            Surface::Cli => "cli-settings.json",
        }
    }
}

fn path(data_dir: &Path, surface: Surface) -> PathBuf {
    data_dir.join(surface.file_name())
}

/// 读 surface 设置。文件不存在 / 解析失败 → `Value::Null`，调用方按需 fallback。
pub fn get_surface_settings(data_dir: &Path, surface: Surface) -> AppResult<Value> {
    let p = path(data_dir, surface);
    if !p.exists() {
        return Ok(Value::Null);
    }
    let bytes = lock::read_locked(&p)?;
    Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// 写 surface 设置（原子覆盖 + 文件锁）。
pub fn save_surface_settings(data_dir: &Path, surface: Surface, value: &Value) -> AppResult<()> {
    let p = path(data_dir, surface);
    let bytes = serde_json::to_vec_pretty(value)?;
    lock::write_atomic(&p, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hebbian-ss-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_desktop_cli_separate() {
        let dir = tmp("rt");
        save_surface_settings(&dir, Surface::Desktop, &serde_json::json!({"theme":"dark"})).unwrap();
        save_surface_settings(&dir, Surface::Cli, &serde_json::json!({"tui":{"sidebar":false}})).unwrap();
        let d = get_surface_settings(&dir, Surface::Desktop).unwrap();
        let c = get_surface_settings(&dir, Surface::Cli).unwrap();
        assert_eq!(d["theme"], "dark");
        assert_eq!(c["tui"]["sidebar"], false);
    }
}
