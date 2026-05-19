//! 全局设置（`settings.json`）。每个对话可以用 Session 字段覆盖部分项。
//!
//! 落盘到 `<data_dir>/settings.json`。文件不存在或损坏时退回默认值。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use common::AppResult;

const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    /// 通用 / app 行为
    #[serde(default)]
    pub general: GeneralSettings,
    /// 默认对话设置（新对话继承这些值）
    #[serde(default)]
    pub conversation: ConversationDefaults,
    /// agent 配置：预设 prompt 列表（与现有 prompts 文件并存）
    #[serde(default)]
    pub agents: AgentDefaults,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GeneralSettings {
    /// 开机启动（macOS / Windows）
    #[serde(default)]
    pub launch_at_login: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationDefaults {
    /// 默认 workdir。`None` 表示用户主目录 `~/`。
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    /// 默认额外允许的路径。
    #[serde(default)]
    pub allowed_paths: Vec<PathBuf>,
    /// 默认启用的非内置工具（来自 tool_manifest）
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    /// 默认的 skill 目录列表。空 = 用 `default_skill_dirs(workdir)`。
    #[serde(default)]
    pub skill_dirs: Vec<PathBuf>,
    /// 默认启用的全局规则文件路径列表。默认仅 `~/.claude/CLAUDE.md`。
    #[serde(default = "default_global_rules")]
    pub global_rules: Vec<PathBuf>,
    /// edits-worktree 保留天数（架构 §4.13.12）。session 关闭后超过此天数的
    /// worktree 会被后台任务清理（metadata 保留但标灰）。默认 30 天。
    #[serde(default = "default_edits_worktree_ttl_days")]
    pub edits_worktree_ttl_days: u32,
}

fn default_global_rules() -> Vec<PathBuf> {
    crate::rules::default_global_rules()
}

fn default_edits_worktree_ttl_days() -> u32 {
    30
}

impl Default for ConversationDefaults {
    fn default() -> Self {
        Self {
            workdir: None,
            allowed_paths: Vec::new(),
            enabled_tools: Vec::new(),
            skill_dirs: Vec::new(),
            global_rules: crate::rules::default_global_rules(),
            edits_worktree_ttl_days: default_edits_worktree_ttl_days(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentDefaults {
    /// 默认选中的 prompt id（指向 prompts 模块管理的预设）
    #[serde(default)]
    pub default_prompt_id: Option<String>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

pub fn load(data_dir: &Path) -> Settings {
    let p = path(data_dir);
    if !p.exists() {
        return Settings::default();
    }
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

pub fn save(data_dir: &Path, settings: &Settings) -> AppResult<()> {
    std::fs::create_dir_all(data_dir)?;
    let text = serde_json::to_string_pretty(settings)?;
    std::fs::write(path(data_dir), text)?;
    Ok(())
}
