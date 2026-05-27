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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettings {
    /// 开机启动（macOS / Windows）
    #[serde(default)]
    pub launch_at_login: bool,
    /// Grep 工具结果中显示搜索位置。
    #[serde(default = "default_show_grep_search_path")]
    pub show_grep_search_path: bool,
    /// 工具调度日志落盘开关。开启后每条 tool_start/done/permission 事件写入
    /// `~/.hebbian/logs/dispatch-YYYY-MM-DD.log`，按天 rotate，保留 30 天。
    #[serde(default)]
    pub log_enabled: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            show_grep_search_path: default_show_grep_search_path(),
            log_enabled: false,
        }
    }
}

fn default_show_grep_search_path() -> bool {
    true
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
    let mut settings: Settings = match std::fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => return Settings::default(),
    };
    // 老命名向新命名迁移（架构 §4.4.7：工具名 PascalCase）。
    // 一次性 normalize + 透明回写，避免老 settings.json 里的 snake_case 工具名
    // 在 UI 与运行时全部"看着是空"的 bug。
    let normalized = normalize_legacy_tool_names(&settings.conversation.enabled_tools);
    if normalized != settings.conversation.enabled_tools {
        settings.conversation.enabled_tools = normalized;
        if let Err(e) = save(data_dir, &settings) {
            tracing::warn!(error = %e, "normalize enabled_tools 回写失败，仅内存生效");
        }
    }
    // 把所有 path 字段里的 `~/` 展开成绝对路径。`std::fs` 不识别 tilde（那是 shell
    // 语法糖），如果用户在 settings.json 写了 `"~/.hebbian/skills"` 之类，下游
    // read_dir 会拿到字面 `~`，悄悄返回空——SkillTool 看起来"没加载到任何 skill"。
    // 这里 in-memory 展开但不回写文件，用户在 settings.json 里仍能看到 `~/` 表达。
    expand_home_in_settings(&mut settings);
    settings
}

/// 把所有 `~` / `~/...` 形式的 path 字段就地展开成绝对路径。
fn expand_home_in_settings(s: &mut Settings) {
    if let Some(ref mut wd) = s.conversation.workdir {
        *wd = expand_home(wd);
    }
    for p in &mut s.conversation.allowed_paths {
        *p = expand_home(p);
    }
    for p in &mut s.conversation.skill_dirs {
        *p = expand_home(p);
    }
    for p in &mut s.conversation.global_rules {
        *p = expand_home(p);
    }
}

/// `~` → `$HOME`、`~/foo` → `$HOME/foo`；其他原样返回。`$HOME` 拿不到时也原样返回
/// （比让 fs 操作拿一个垃圾值更友好——下游会自然报错）。
pub fn expand_home(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s != "~" && !s.starts_with("~/") {
        return p.to_path_buf();
    }
    let Some(home) = dirs::home_dir() else {
        return p.to_path_buf();
    };
    let rest = s.trim_start_matches('~').trim_start_matches('/');
    if rest.is_empty() {
        home
    } else {
        home.join(rest)
    }
}

/// 已知的工具名迁移映射（老 → 新）。新工具加入时这里无需改——只迁移已废弃的别名。
fn normalize_legacy_tool_names(names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|n| match n.as_str() {
            "web_search" => "WebSearch".to_string(),
            // 老版本的 web_fetch 与 WebFetch 现已统一为 "Fetch"
            "web_fetch" | "WebFetch" => "Fetch".to_string(),
            "image_generation" => model_gateway::types::IMAGE_GENERATION_TOOL_NAME.to_string(),
            _ => n.clone(),
        })
        .collect()
}

pub fn save(data_dir: &Path, settings: &Settings) -> AppResult<()> {
    std::fs::create_dir_all(data_dir)?;
    let text = serde_json::to_string_pretty(settings)?;
    std::fs::write(path(data_dir), text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("hebbian-settings-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn normalize_maps_legacy_snake_case_to_pascal() {
        let names = vec![
            "web_search".to_string(),
            "web_fetch".to_string(),
            "image_generation".to_string(),
            "Custom".to_string(),
        ];
        let out = normalize_legacy_tool_names(&names);
        assert_eq!(out[0], "WebSearch");
        assert_eq!(out[1], "Fetch");
        assert_eq!(
            out[2],
            model_gateway::types::IMAGE_GENERATION_TOOL_NAME.to_string()
        );
        assert_eq!(out[3], "Custom"); // 未知 → 透传
    }

    #[test]
    fn load_rewrites_legacy_tool_names_to_disk() {
        let dir = tmp("legacy-rewrite");
        std::fs::write(
            dir.join("settings.json"),
            r#"{"conversation":{"enabled_tools":["web_search","web_fetch"]}}"#,
        )
        .unwrap();
        let s = load(&dir);
        assert_eq!(
            s.conversation.enabled_tools,
            vec!["WebSearch".to_string(), "Fetch".to_string()]
        );
        // 已透明回写
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(text.contains("WebSearch"));
        assert!(text.contains("Fetch"));
        assert!(!text.contains("web_search"));
    }

    /// 回归 2026-05-23：用户在 `~/.hebbian/settings.json` 写
    /// `"skill_dirs":["~/.hebbian/skills"]`，下游 std::fs::read_dir 拿到字面 `~`
    /// 默默返回空，SkillTool 看起来"加载不到任何 skill"。load 必须把 `~/` 展开。
    #[test]
    fn load_expands_tilde_in_path_fields() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let dir = tmp("tilde-expand");
        std::fs::write(
            dir.join("settings.json"),
            r#"{
              "conversation": {
                "workdir": "~/work",
                "allowed_paths": ["~/code", "/abs/path"],
                "skill_dirs": ["~/.hebbian/skills", "~"],
                "global_rules": ["~/.claude/CLAUDE.md"]
              }
            }"#,
        )
        .unwrap();
        let s = load(&dir);
        assert_eq!(
            s.conversation.workdir.as_deref(),
            Some(home.join("work").as_path())
        );
        assert_eq!(
            s.conversation.allowed_paths,
            vec![home.join("code"), PathBuf::from("/abs/path")]
        );
        assert_eq!(
            s.conversation.skill_dirs,
            vec![home.join(".hebbian/skills"), home.clone()]
        );
        assert_eq!(
            s.conversation.global_rules,
            vec![home.join(".claude/CLAUDE.md")]
        );
    }

    #[test]
    fn expand_home_handles_edge_cases() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert_eq!(expand_home(&PathBuf::from("~")), home);
        assert_eq!(expand_home(&PathBuf::from("~/")), home);
        assert_eq!(expand_home(&PathBuf::from("~/foo")), home.join("foo"));
        // 仅前缀展开，中间出现的 `~` 不动（合法目录名场景）
        assert_eq!(
            expand_home(&PathBuf::from("/etc/~hostname")),
            PathBuf::from("/etc/~hostname")
        );
        // 已经是绝对路径——原样返回
        assert_eq!(expand_home(&PathBuf::from("/abs")), PathBuf::from("/abs"));
    }
}
