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
    /// 长期记忆系统配置（架构 §4.14）。
    #[serde(default)]
    pub memory: MemorySettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettings {
    /// 开机启动（macOS / Windows）
    #[serde(default)]
    pub launch_at_login: bool,
    /// 用户界面语言偏好。当前用于控制 AutoMode 判官原因的输出语言。
    #[serde(default)]
    pub language: AppLanguage,
    /// Grep 工具结果中显示搜索位置。
    #[serde(default = "default_show_grep_search_path")]
    pub show_grep_search_path: bool,
    /// 工具执行前用于初始化 PATH 的 shell。空表示使用系统默认 shell。
    #[serde(default = "default_shell")]
    pub shell: Option<String>,
    /// 工具调度日志落盘开关。开启后每条 tool_start/done/permission 事件写入
    /// `~/.hebbian/logs/dispatch-YYYY-MM-DD.log`，按天 rotate，保留 30 天。
    #[serde(default)]
    pub log_enabled: bool,
    /// Edit 工具后端选择。`StringReplace`（默认）= 现有 old_string/new_string 精确替换；
    /// `Hashline` = oh-my-pi 风格的 ¶path#HASH + 行号 patch 格式（实验性）。
    /// Read 与 Edit 强耦合，两者一起切换。
    #[serde(default)]
    pub edit_backend: EditBackend,
    /// Run 非正常结束后，ContinueBar 上点 continue 的恢复方式（架构 §7.3）。
    #[serde(default)]
    pub continue_strategy: ContinueStrategy,
    /// 聊天正文 / 工具卡片里的超链接点击去向（架构 §8.5）。`System`（默认）= 系统默认
    /// 浏览器；`Builtin` = 内置浏览器 tab。纯 UI 偏好，后端只存储，由 surface 据此分流。
    #[serde(default)]
    pub link_open_target: LinkOpenTarget,
    /// 启动时自动打开浏览器 DevTools（F12）。手动改配置文件开启，默认关闭。
    #[serde(default)]
    pub open_devtools: bool,
    /// 离开电脑多少分钟后，把桌面对话里待审批/待回答的 HITL 转发到已连接的渠道（如微信）。
    /// 0 = 关闭转发。默认 5 分钟。
    #[serde(default = "default_channel_idle_forward_minutes")]
    pub channel_idle_forward_minutes: u32,
}

fn default_channel_idle_forward_minutes() -> u32 {
    5
}

/// 应用语言偏好。当前只影响 AutoMode 判官原因；不做整套 UI i18n。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AppLanguage {
    #[default]
    ZhCn,
    En,
}

impl AppLanguage {
    pub fn judge_reason_instruction(self) -> &'static str {
        match self {
            Self::ZhCn => "Write DENY and ASK reasons in Simplified Chinese.",
            Self::En => "Write DENY and ASK reasons in English.",
        }
    }
}

/// 点「继续」的恢复方式（架构 §7.3）。这是一个 UI 行为偏好——后端只存储，
/// 由 surface 据此决定点击行为；不影响 agent_loop 本身。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContinueStrategy {
    /// 默认：主动发一条 user「继续」消息再跑。末尾天然是 user message，
    /// 不依赖任何"重建时偷偷补 user"的兜底——是唯一能根除 assistant prefill
    /// 400（"conversation must end with a user message"）的策略。
    #[default]
    SendContinue,
    /// 用当前 transcript 原样再起一次 agent_loop，不追加任何显式消息。
    /// 失败请求→天然重发；截断→模型接着写。注意：末尾若是 assistant，
    /// 靠 transcript 重建层补 user 兜底，不如 SendContinue 稳。
    ResumeLoop,
    /// 不自动跑，只把光标聚焦输入框，让用户改 prompt 再发。
    Manual,
}

/// 超链接点击去向（架构 §8.5）。纯 UI 偏好——后端只存储，由 surface 据此分流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LinkOpenTarget {
    /// 默认：交给系统默认浏览器打开。
    #[default]
    System,
    /// 在内置浏览器 tab 里打开。
    Builtin,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            language: AppLanguage::default(),
            show_grep_search_path: default_show_grep_search_path(),
            shell: default_shell(),
            log_enabled: false,
            edit_backend: EditBackend::default(),
            continue_strategy: ContinueStrategy::default(),
            link_open_target: LinkOpenTarget::default(),
            open_devtools: false,
            channel_idle_forward_minutes: default_channel_idle_forward_minutes(),
        }
    }
}

/// Edit 工具后端。Read 工具会跟随同一选项切换格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EditBackend {
    /// 现有实现：old_string / new_string 精确字符串替换。
    #[default]
    StringReplace,
    /// Hashline 实验后端：¶path#HASH 文件头 + 1-based 行号 patch。
    Hashline,
}

fn default_show_grep_search_path() -> bool {
    true
}

pub fn default_shell() -> Option<String> {
    std::env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
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

/// 长期记忆系统设置（架构 §4.14）。后台抽取按 `models` 顺序 fallback——
/// 每个模型最多重试 5 次，全链耗尽 → 整轮失败（游标不前进，下次补抽）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemorySettings {
    /// 总开关。关闭时既不注入 <memory-index> 也不跑后台抽取（手动 ReadMemory /
    /// WriteMemory 不受影响——是工具能力，不是后台行为）。
    #[serde(default)]
    pub enabled: bool,
    /// 抽取模型 fallback 链；空 = 没配，等同 `enabled=false` 的抽取行为。
    #[serde(default)]
    pub models: Vec<MemoryModelRef>,
    /// 空闲触发深睡的分钟数（架构 §3.1）。一个 Run 跑完后空闲超过它就整理记忆。
    /// `0` = 关闭 idle 深睡（仍可手动 / 显式触发）。默认 10min——本项目真实间隔分布
    /// 70.9% < 5min 是连续工作，10min 能精准避开、只抓真正的停顿。
    #[serde(default = "default_idle_consolidate_minutes")]
    pub idle_consolidate_minutes: u32,
    /// 联想注入模式（架构 §4.14 / 批5）。控制 `<memory-index>` 怎么选记忆注入。
    #[serde(default)]
    pub recall_mode: RecallMode,
}

/// 联想注入模式（架构 §4.14 / 批5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RecallMode {
    /// 门控器：每轮分词查倒排表，命中 + 话题漂移才激活扩散注入（经济权衡，默认）。
    #[default]
    Auto,
    /// 关闭联想：退回现状——仅首条 user message 注入全量 L0 清单。
    Off,
    /// 每轮强制激活，不走漂移缓存（长对话 / 强上下文依赖；最费）。
    Always,
}

fn default_idle_consolidate_minutes() -> u32 {
    10
}

impl MemorySettings {
    /// 记忆系统是否生效：注入 `<memory-index>` 与后台抽取共用这一判定，
    /// 避免两侧门控漂移（架构 §4.14.6：`enabled=false` 或 `models` 空 → 既不注入也不抽取）。
    pub fn active(&self) -> bool {
        self.enabled && !self.models.is_empty()
    }
}

/// 一个 fallback 链节点：复用现有 provider，绑定具体 model id。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryModelRef {
    pub provider_id: String,
    pub model: String,
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
    fn edit_backend_defaults_to_string_replace() {
        let settings = Settings::default();
        assert_eq!(settings.general.edit_backend, EditBackend::StringReplace);
    }

    #[test]
    fn edit_backend_round_trip_json() {
        let json = r#"{"general":{"edit_backend":"hashline"}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.general.edit_backend, EditBackend::Hashline);
        let out = serde_json::to_string(&s).unwrap();
        assert!(
            out.contains(r#""edit_backend":"hashline""#),
            "serialize must use kebab-case: {}",
            out,
        );
    }

    /// 旧 settings.json 没有 edit_backend 字段时必须自动用默认值，
    /// 不能因为新加字段就让老用户的设置炸掉。
    #[test]
    fn edit_backend_missing_uses_default() {
        let json = r#"{"general":{"launch_at_login":true}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.general.edit_backend, EditBackend::StringReplace);
        assert!(s.general.launch_at_login);
    }

    /// 续跑策略默认 = SendContinue：发一条真实「继续」user message，末尾天然是 user，
    /// 是唯一能根除 assistant prefill 400 的策略。没存过该字段的老用户也用这个默认。
    #[test]
    fn continue_strategy_defaults_to_send_continue() {
        assert_eq!(ContinueStrategy::default(), ContinueStrategy::SendContinue);
        let json = r#"{"general":{"launch_at_login":false}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.general.continue_strategy, ContinueStrategy::SendContinue);
    }

    /// 存过 resume_loop 的用户保留原选择——默认值变更不覆盖已有配置。
    #[test]
    fn continue_strategy_respects_persisted_resume_loop() {
        let json = r#"{"general":{"continue_strategy":"resume_loop"}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.general.continue_strategy, ContinueStrategy::ResumeLoop);
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
