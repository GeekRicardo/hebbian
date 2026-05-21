//! ~/.hebbian 统一持久化（架构 §6）。
//!
//! 本模块负责所有共享数据的落盘、读取与并发保护。子模块按数据类别拆分：
//!
//! - [`lock`]：文件锁 + 原子写原语（架构 §6.3）
//! - [`sessions`]：对话历史 jsonl 读写（rollout v1）
//! - [`sessions_dir`]：每段对话目录骨架 + partial sidecar + meta.json
//! - [`prompts`]：用户 persona 列表（prompts.json）
//! - [`settings`]：全局通用 settings.json
//! - [`permissions`]：Global 级审批规则（架构 §4.6 / §4.5.4）
//! - [`tool_results`]：工具结果 txt（含大输出落盘 + 压缩占位符）
//! - [`compactions`]：/compact 时压缩前 markdown
//! - [`oauth`]：provider OAuth 凭据（目录占位）
//!
//! `~/.hebbian/providers.json` 因被 `model-gateway` 单独读写、避免反向依赖
//! `agent-core`，其路径 helper 仍保留在 [`common::storage`]。

pub mod compactions;
pub mod lock;
pub mod oauth;
pub mod permissions;
pub mod plans;
pub mod projects;
pub mod prompts;
pub mod run_checkpoint;
pub mod sessions;
pub mod sessions_dir;
pub mod settings;
pub mod skills;
pub mod tool_results;

use std::path::{Path, PathBuf};

/// 默认数据目录：`~/.hebbian/`。
///
/// 架构 §6.1：Desktop 多窗口/多进程共享同一根目录，跨平台保持一致（决策 D10）。
/// 早期使用了 `~/Library/Application Support/dev.ricardo.hebbian/`（macOS）等
/// 平台原生路径，本函数若发现旧路径存在但新路径为空，会迁移到 `~/.hebbian/`。
///
/// 同时会触发 sessions 老布局（`sessions/<date>/<id>.jsonl` 或平铺
/// `sessions/<id>.jsonl`）到新布局（`sessions/<id>/session.jsonl`）的一次性迁移。
pub fn default_data_dir() -> PathBuf {
    let new = home_dir().join(".hebbian");
    if !new.exists() {
        if let Some(old) = legacy_data_dir() {
            if old.exists() && old != new {
                if let Err(e) = migrate_legacy_data_dir(&old, &new) {
                    tracing::warn!(error = %e, from = %old.display(), to = %new.display(), "迁移旧数据目录失败");
                } else {
                    tracing::info!(from = %old.display(), to = %new.display(), "已迁移旧数据目录到 ~/.hebbian/");
                }
            }
        }
    }
    let _ = std::fs::create_dir_all(&new);
    match sessions::migrate_legacy_layout_if_needed(&new) {
        Ok(0) => {}
        Ok(n) => tracing::info!(count = n, "已迁移 sessions 老布局到新目录化布局"),
        Err(e) => tracing::warn!(error = %e, "sessions 老布局迁移失败"),
    }
    new
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// 旧 Tauri bundle 路径：跨平台映射到 `dirs::data_dir()/dev.ricardo.hebbian`。
fn legacy_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("dev.ricardo.hebbian"))
}

/// 把整个旧目录 rename 到新位置；rename 失败（如跨卷）退回到 copy 后保留旧目录。
fn migrate_legacy_data_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // 跨卷或权限问题：递归 copy 后留底（不删旧目录）。
    copy_dir_all(from, to)?;
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
