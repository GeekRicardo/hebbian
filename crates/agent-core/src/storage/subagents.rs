//! Subagent 配置存储（架构 §4.4.11 / §6.1）。
//!
//! - 全局定义：`~/.hebbian/subagents/<name>.md`（YAML frontmatter + system prompt body）
//! - 全局启用状态：`~/.hebbian/subagents/settings.json`（`{ "enabled": { "<name>": bool } }`）
//! - 项目启用 override：`~/.hebbian/projects/<enc>/settings.json` 的 `subagents` key
//!
//! 合并语义：项目级 `enabled.<name>` 有显式值覆盖全局；项目未设跟全局；两层都未设 = 默认启用。
//!
//! 模块只负责存储——SubagentRunner（agent-core 内的 NestedRun 执行体）通过 `load_for_workdir`
//! 拿到合并后的 `Vec<SubagentDefinition>` 后判 `enabled` 字段过滤再用。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};

use super::projects;

const SUBAGENTS_DIRNAME: &str = "subagents";
const SETTINGS_FILENAME: &str = "settings.json";

/// 默认 subagent 最大工具调用次数（架构 §4.4.11.4）。subagent 定义里没填 `max_iterations`
/// 时用这个；调用方按需读 `definition.max_iterations.unwrap_or(DEFAULT_MAX_ITERATIONS)`。
pub const DEFAULT_MAX_ITERATIONS: u32 = 50;

/// Subagent 来源层级（架构 §4.4.11.4）。前端据此区分「内置」与「自定义」与「临时」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubagentSource {
    /// 代码内嵌（[`super::subagents_builtin`]）。
    Builtin,
    /// 用户磁盘 `~/.hebbian/subagents/<name>.md`。
    #[default]
    Global,
    /// 运行时 `CreateSubagent` 工具创建的会话级临时定义，进程内内存，不落盘。
    Session,
}

/// Subagent 权限维度（架构 §4.4.11.4），对齐 CC 的 subagent `permissionMode`——
/// 控制子 NestedRun 的工具调用如何审批。frontmatter `permission` 字段；缺省 `Inherit`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SubagentPermission {
    /// 子用**父 Run 的 RunMode** 跑审批（父 AutoMode→judge；父 Default→子会写工具弹审批）。
    #[default]
    Inherit,
    /// 子强制 `Default` 语义（界内编辑 + 只读自主免审；会写 Bash / 越界编辑仍审批），不随父 Plan / Auto 浮动。
    AcceptEdits,
    /// 子在 `tools` 白名单内全放行、不弹审批，仅危险红线（`rm -rf` / 覆盖重定向）拦截。
    Bypass,
}

/// 单个 subagent 的完整定义（架构 §4.4.11.4）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentDefinition {
    /// 文件名（不带 `.md`）= subagent 唯一 id，也是 Task 工具参数 `subagent_type` 的值。
    pub name: String,
    /// 单行描述。Task 工具的 schema description 里会平铺所有可用 subagent 的 description，
    /// 让模型基于描述选用。
    pub description: String,
    /// 受限工具白名单（PascalCase）。`None` = 继承父的全工具集（除 Task 自身）；
    /// `Some(empty)` = 没有工具可用（子 agent 只能"答字"）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// 模型 id。`None` = 跟父 Run 用同模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 单次 Task 调用的最大 ToolStep 次数。`None` = 用 [`DEFAULT_MAX_ITERATIONS`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// system prompt 正文（frontmatter 之后的全部内容，已去前导空行）。
    pub system_prompt: String,
    /// 当前 workdir 下是否启用。由 [`load_for_workdir`] 合并两层 settings.json 后填。
    /// 直接读 `~/.hebbian/subagents/<name>.md` 不经合并时默认 `true`。
    pub enabled: bool,
    /// 来源层级（架构 §4.4.11.4）：`Builtin` = 代码内嵌；`Global` = 用户磁盘 .md。
    /// 前端据此区分「内置」（只读 + 可禁用 + 复制为自定义）与「自定义」（可编辑/删除）。
    #[serde(default)]
    pub source: SubagentSource,
    /// 权限维度（架构 §4.4.11.4，对齐 CC `permissionMode`）。`None` = `Inherit`（跟父 RunMode）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<SubagentPermission>,
}

/// settings.json 全局形态：`{ "enabled": { ... } }`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalSettings {
    #[serde(default)]
    pub enabled: BTreeMap<String, bool>,
}

/// 项目 settings.json 中 `subagents` key 的形态：与 [`GlobalSettings`] 等价
/// （仅嵌套层级不同——项目 settings.json 外包一层 `{ "subagents": {...} }`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectSubagentsSection {
    #[serde(default)]
    pub enabled: BTreeMap<String, bool>,
}

/// 项目级 settings.json 的最小形态：只关心 `subagents` 字段；其余 key 透传保留。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectSettings {
    #[serde(default)]
    pub subagents: ProjectSubagentsSection,
    /// 其它项目级 settings（hooks_enabled / model_overrides 等）字段透传保留，避免本模块
    /// 覆写无关键。
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

pub fn global_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(SUBAGENTS_DIRNAME)
}

pub fn global_settings_path(data_dir: &Path) -> PathBuf {
    global_dir(data_dir).join(SETTINGS_FILENAME)
}

pub fn project_settings_path(data_dir: &Path, workdir: &Path) -> PathBuf {
    projects::project_dir(data_dir, workdir).join(SETTINGS_FILENAME)
}

/// 读全局 settings.json；文件不存在或解析失败时返回默认（全部启用）。
pub fn load_global_settings(data_dir: &Path) -> GlobalSettings {
    let p = global_settings_path(data_dir);
    if !p.exists() {
        return GlobalSettings::default();
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_global_settings(data_dir: &Path, settings: &GlobalSettings) -> AppResult<()> {
    let dir = global_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(global_settings_path(data_dir), json)?;
    Ok(())
}

/// 读项目 settings.json；文件不存在 / 解析失败时返回默认（subagents 段为空）。
pub fn load_project_settings(data_dir: &Path, workdir: &Path) -> ProjectSettings {
    let p = project_settings_path(data_dir, workdir);
    if !p.exists() {
        return ProjectSettings::default();
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_project_settings(
    data_dir: &Path,
    workdir: &Path,
    settings: &ProjectSettings,
) -> AppResult<()> {
    let dir = projects::project_dir(data_dir, workdir);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(project_settings_path(data_dir, workdir), json)?;
    Ok(())
}

/// 写单个 subagent 在某 scope 下的启用状态。`scope = None` → 全局；`Some(workdir)` → 项目级。
pub enum EnableScope<'a> {
    Global,
    Project(&'a Path),
}

pub fn set_enabled(
    data_dir: &Path,
    scope: EnableScope<'_>,
    name: &str,
    enabled: bool,
) -> AppResult<()> {
    match scope {
        EnableScope::Global => {
            let mut s = load_global_settings(data_dir);
            s.enabled.insert(name.to_string(), enabled);
            save_global_settings(data_dir, &s)
        }
        EnableScope::Project(workdir) => {
            let mut s = load_project_settings(data_dir, workdir);
            s.subagents.enabled.insert(name.to_string(), enabled);
            save_project_settings(data_dir, workdir, &s)
        }
    }
}

/// 删除单个 subagent 的启用状态项（恢复到"默认启用"）。
pub fn clear_enabled(data_dir: &Path, scope: EnableScope<'_>, name: &str) -> AppResult<()> {
    match scope {
        EnableScope::Global => {
            let mut s = load_global_settings(data_dir);
            s.enabled.remove(name);
            save_global_settings(data_dir, &s)
        }
        EnableScope::Project(workdir) => {
            let mut s = load_project_settings(data_dir, workdir);
            s.subagents.enabled.remove(name);
            save_project_settings(data_dir, workdir, &s)
        }
    }
}

/// 加载全部全局 subagent 定义。**不**应用启用状态合并——直接读盘。
/// 调用方需要带 workdir 的启用合并请用 [`load_for_workdir`]。
pub fn load_global_definitions(data_dir: &Path) -> Vec<SubagentDefinition> {
    let dir = global_dir(data_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<SubagentDefinition> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        match parse_definition(&name, &content) {
            Ok(def) => out.push(def),
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "subagent 定义解析失败，跳过")
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 读单个 subagent 定义。文件不存在或解析失败返回 Err。
pub fn get_definition(data_dir: &Path, name: &str) -> AppResult<SubagentDefinition> {
    let p = global_dir(data_dir).join(format!("{name}.md"));
    if !p.exists() {
        return Err(AppError::msg(format!("subagent `{name}` 不存在")));
    }
    let content = std::fs::read_to_string(&p)
        .map_err(|e| AppError::msg(format!("读取 subagent `{name}` 失败：{e}")))?;
    parse_definition(name, &content)
}

/// 写一个 subagent 定义到全局目录。`content` 应已含 frontmatter + body。
pub fn save_definition(data_dir: &Path, name: &str, content: &str) -> AppResult<()> {
    // 先解析一遍校验，避免落入解析不出来的脏数据
    parse_definition(name, content)?;
    let dir = global_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{name}.md")), content)?;
    Ok(())
}

/// 删除 subagent 定义文件 + 两层 settings.json 的启用项（避免残留 stale key）。
pub fn delete_definition(data_dir: &Path, name: &str, workdir: Option<&Path>) -> AppResult<()> {
    let p = global_dir(data_dir).join(format!("{name}.md"));
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    // 清全局 enabled key
    if let Err(e) = clear_enabled(data_dir, EnableScope::Global, name) {
        tracing::warn!(error = %e, "清除全局 subagent enabled 项失败");
    }
    // 清当前 workdir 项目级 enabled key（其它 workdir 的项目级残留留给后续自然清理）
    if let Some(wd) = workdir {
        if let Err(e) = clear_enabled(data_dir, EnableScope::Project(wd), name) {
            tracing::warn!(error = %e, "清除项目级 subagent enabled 项失败");
        }
    }
    Ok(())
}

/// 加载并合并两层 enabled 状态。返回的 `enabled` 字段已带正确语义：
///
/// - 项目级 `enabled.<name>` 有值 → 用项目值
/// - 项目级无值，全局 `enabled.<name>` 有值 → 用全局值
/// - 两层都无值 → 默认 `true`（缺省启用）
///
/// `workdir = None` 时只查全局。
pub fn load_for_workdir(data_dir: &Path, workdir: Option<&Path>) -> Vec<SubagentDefinition> {
    let mut defs = merge_builtin_with_disk(data_dir);
    let global_settings = load_global_settings(data_dir);
    let project_settings = workdir.map(|wd| load_project_settings(data_dir, wd));
    for def in defs.iter_mut() {
        def.enabled = resolve_enabled(
            &def.name,
            &global_settings,
            project_settings.as_ref().map(|s| &s.subagents),
        );
    }
    defs
}

/// builtin 垫底 + 磁盘同名覆盖（架构 §4.4.11.4 来源层级）：内置项被磁盘同名定义整体顶替。
fn merge_builtin_with_disk(data_dir: &Path) -> Vec<SubagentDefinition> {
    let disk = load_global_definitions(data_dir);
    let mut out: Vec<SubagentDefinition> = super::subagents_builtin::builtin_subagents()
        .into_iter()
        .filter(|b| !disk.iter().any(|d| d.name == b.name))
        .collect();
    out.extend(disk);
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn resolve_enabled(
    name: &str,
    global: &GlobalSettings,
    project: Option<&ProjectSubagentsSection>,
) -> bool {
    if let Some(proj) = project {
        if let Some(v) = proj.enabled.get(name) {
            return *v;
        }
    }
    if let Some(v) = global.enabled.get(name) {
        return *v;
    }
    true
}

/// 解析 `<name>.md` 内容：YAML frontmatter（极简 key:value 行）+ body。
fn parse_definition(name: &str, content: &str) -> AppResult<SubagentDefinition> {
    let (fm, body) = split_frontmatter(content);
    let mut description = String::new();
    let mut tools: Option<Vec<String>> = None;
    let mut model: Option<String> = None;
    let mut max_iterations: Option<u32> = None;
    let mut permission: Option<SubagentPermission> = None;
    if let Some(fm) = fm {
        for line in fm.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, val)) = trimmed.split_once(':') else {
                continue;
            };
            let val = val.trim();
            match key.trim() {
                "name" => {
                    // frontmatter name 字段允许存在但仅作展示；规范 id 始终用文件名
                }
                "description" => description = strip_quotes(val).to_string(),
                "tools" => tools = Some(parse_string_list(val)),
                "model" => {
                    let v = strip_quotes(val);
                    if !v.is_empty() {
                        model = Some(v.to_string());
                    }
                }
                "max_iterations" => {
                    if let Ok(n) = strip_quotes(val).parse::<u32>() {
                        max_iterations = Some(n);
                    }
                }
                "permission" => {
                    permission = match strip_quotes(val).to_ascii_lowercase().as_str() {
                        "inherit" => Some(SubagentPermission::Inherit),
                        "acceptedits" | "accept_edits" => Some(SubagentPermission::AcceptEdits),
                        "bypass" | "bypasspermissions" => Some(SubagentPermission::Bypass),
                        _ => None, // 未知值忽略，按缺省 Inherit 处理
                    };
                }
                _ => {}
            }
        }
    }
    let system_prompt = body.trim_start().to_string();
    if description.is_empty() {
        return Err(AppError::msg(format!(
            "subagent `{name}` 缺少 frontmatter `description` 字段"
        )));
    }
    if system_prompt.is_empty() {
        return Err(AppError::msg(format!(
            "subagent `{name}` 没有 system prompt 正文（frontmatter 之后为空）"
        )));
    }
    Ok(SubagentDefinition {
        name: name.to_string(),
        description,
        tools,
        model,
        max_iterations,
        system_prompt,
        enabled: true,
        source: SubagentSource::Global,
        permission,
    })
}

/// 返回 `(frontmatter, body)`。无 frontmatter 时 `frontmatter = None`，整篇都是 body。
fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    if !content.starts_with("---") {
        return (None, content);
    }
    // 去掉首个 `---` 行
    let rest = match content.strip_prefix("---") {
        Some(r) => r.trim_start_matches('\n'),
        None => return (None, content),
    };
    // 找下一个 `---` 单独成行
    if let Some(end) = find_frontmatter_end(rest) {
        let fm = &rest[..end];
        let after = &rest[end..];
        // 跳过结束 `---` 行
        let body = after
            .trim_start_matches("---")
            .trim_start_matches('\n')
            .trim_start_matches('\r');
        (Some(fm), body)
    } else {
        (None, content)
    }
}

fn find_frontmatter_end(s: &str) -> Option<usize> {
    let mut start = 0;
    for line in s.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some(start);
        }
        start += line.len();
    }
    None
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    let s = s.strip_prefix('\'').unwrap_or(s);
    s.strip_suffix('\'').unwrap_or(s)
}

/// 解析 `[A, B, C]` / `A, B, C` 两种风格的字符串列表。
fn parse_string_list(s: &str) -> Vec<String> {
    let inner = s.trim();
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let inner = inner.strip_suffix(']').unwrap_or(inner);
    inner
        .split(',')
        .map(|item| strip_quotes(item.trim()).to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

use std::sync::{Arc, RwLock};

// ── session-scoped 临时 subagent 路由表 ───────────────────────────────
// 复刻 BgTaskRegistry 的 session 路由模式（架构 §4.12.2 修订）：
// 同一 session_id 跨 chat() / spawn_run 调用拿到同一份 Arc<RwLock<Vec<...>>>；
// 不同 session 完全隔离。进程重启即丢失——符合「临时」语义。

/// 进程内 `session_id → session 级临时 subagent 列表` 路由表。
static SESSION_SUBAGENTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<RwLock<Vec<SubagentDefinition>>>>>,
> = std::sync::OnceLock::new();

fn session_subagents_map(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<RwLock<Vec<SubagentDefinition>>>>>
{
    SESSION_SUBAGENTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 按 session_id 取（或首次创建）该 session 的临时 subagent 列表句柄。
/// 同一 session 多次调用返回同一份 Arc；不同 session 互不可见。
pub fn session_subagents_for(session_id: &str) -> Arc<RwLock<Vec<SubagentDefinition>>> {
    let mut map = session_subagents_map().lock().expect("session subagents mutex");
    map.entry(session_id.to_string())
        .or_insert_with(|| Arc::new(RwLock::new(Vec::new())))
        .clone()
}

/// session 关闭 / 删除时从路由表摘除，释放内存。
pub fn discard_session_subagents(session_id: &str) {
    if let Ok(mut map) = session_subagents_map().lock() {
        map.remove(session_id);
    }
}

/// 读取并克隆某 session 的全部临时 subagent 定义（供 `build_subagent_ctx_snapshot` 合并用）。
/// session_id 不存在或列表为空时返回空 Vec。
pub fn take_session_subagents(session_id: &str) -> Vec<SubagentDefinition> {
    let map = session_subagents_map().lock().expect("session subagents mutex");
    match map.get(session_id) {
        Some(lock) => lock.read().expect("session subagents rwlock").clone(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_data_dir(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hebbian-subagent-test-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parses_minimal_definition() {
        let content = "---\ndescription: Reviews code\n---\nYou are a reviewer.\n";
        let def = parse_definition("code-reviewer", content).unwrap();
        assert_eq!(def.name, "code-reviewer");
        assert_eq!(def.description, "Reviews code");
        assert_eq!(def.system_prompt, "You are a reviewer.\n");
        assert!(def.tools.is_none());
        assert!(def.model.is_none());
        assert!(def.max_iterations.is_none());
        assert!(def.enabled);
    }

    #[test]
    fn parses_full_definition() {
        let content = "---\ndescription: \"A demo\"\ntools: [Read, Grep, Bash]\nmodel: claude-opus-4-7\nmax_iterations: 30\n---\nBe thorough.\n";
        let def = parse_definition("demo", content).unwrap();
        let tools = def.tools.unwrap();
        assert_eq!(tools, vec!["Read", "Grep", "Bash"]);
        assert_eq!(def.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(def.max_iterations, Some(30));
        assert_eq!(def.system_prompt, "Be thorough.\n");
    }

    #[test]
    fn missing_description_fails() {
        let content = "---\n---\nbody";
        assert!(parse_definition("x", content).is_err());
    }

    #[test]
    fn missing_body_fails() {
        let content = "---\ndescription: x\n---\n";
        assert!(parse_definition("x", content).is_err());
    }

    #[test]
    fn save_then_load_definition_roundtrip() {
        let dd = tmp_data_dir("roundtrip");
        let content = "---\ndescription: Test reviewer\n---\nReview things.\n";
        save_definition(&dd, "reviewer", content).unwrap();
        let def = get_definition(&dd, "reviewer").unwrap();
        assert_eq!(def.description, "Test reviewer");
        assert_eq!(def.system_prompt, "Review things.\n");
    }

    #[test]
    fn load_global_definitions_sorts_by_name() {
        let dd = tmp_data_dir("sort");
        let dir = global_dir(&dd);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["zebra", "alpha", "mango"] {
            std::fs::write(
                dir.join(format!("{name}.md")),
                format!("---\ndescription: {name} agent\n---\nYou are {name}.\n"),
            )
            .unwrap();
        }
        let defs = load_global_definitions(&dd);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn enabled_defaults_to_true_when_neither_layer_sets_it() {
        let dd = tmp_data_dir("default-enabled");
        save_definition(&dd, "a", "---\ndescription: A\n---\nbody").unwrap();
        let defs = load_for_workdir(&dd, None);
        // builtin 垫底后结果含内置项，按 name 找回自定义 "a"，验证两层都未设时缺省启用。
        let a = defs
            .iter()
            .find(|d| d.name == "a")
            .expect("自定义 a 应在合并结果里");
        assert!(a.enabled);
    }

    #[test]
    fn global_enabled_disable_takes_effect_when_no_project_override() {
        let dd = tmp_data_dir("global-disable");
        save_definition(&dd, "a", "---\ndescription: A\n---\nbody").unwrap();
        set_enabled(&dd, EnableScope::Global, "a", false).unwrap();
        let defs = load_for_workdir(&dd, None);
        assert!(!defs[0].enabled);
    }

    #[test]
    fn project_override_wins_over_global() {
        let dd = tmp_data_dir("project-override");
        save_definition(&dd, "a", "---\ndescription: A\n---\nbody").unwrap();
        // 全局禁用
        set_enabled(&dd, EnableScope::Global, "a", false).unwrap();
        // 项目级显式启用
        let wd = PathBuf::from("/Users/x/proj");
        set_enabled(&dd, EnableScope::Project(&wd), "a", true).unwrap();
        let defs = load_for_workdir(&dd, Some(&wd));
        assert!(defs[0].enabled);
    }

    #[test]
    fn project_unset_falls_back_to_global() {
        let dd = tmp_data_dir("project-fallback");
        save_definition(&dd, "a", "---\ndescription: A\n---\nbody").unwrap();
        set_enabled(&dd, EnableScope::Global, "a", false).unwrap();
        // 项目级有 settings 但没动 a 的 key
        let wd = PathBuf::from("/Users/x/proj");
        set_enabled(&dd, EnableScope::Project(&wd), "other", true).unwrap();
        let defs = load_for_workdir(&dd, Some(&wd));
        assert!(!defs[0].enabled);
    }

    #[test]
    fn project_settings_preserves_other_keys_on_write() {
        let dd = tmp_data_dir("preserve-other");
        let wd = PathBuf::from("/Users/x/proj");
        // 用户手放了一个未来的 settings 字段
        let raw = "{\"subagents\":{\"enabled\":{}},\"hooks_enabled\":false}";
        let dir = projects::project_dir(&dd, &wd);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SETTINGS_FILENAME), raw).unwrap();
        // 走 set_enabled，预期 hooks_enabled 不丢
        set_enabled(&dd, EnableScope::Project(&wd), "a", true).unwrap();
        let written = std::fs::read_to_string(dir.join(SETTINGS_FILENAME)).unwrap();
        assert!(written.contains("hooks_enabled"));
        assert!(written.contains("\"a\": true"));
    }

    #[test]
    fn delete_definition_removes_file_and_clears_enabled_keys() {
        let dd = tmp_data_dir("delete");
        save_definition(&dd, "a", "---\ndescription: A\n---\nbody").unwrap();
        set_enabled(&dd, EnableScope::Global, "a", false).unwrap();
        let wd = PathBuf::from("/Users/x/proj");
        set_enabled(&dd, EnableScope::Project(&wd), "a", true).unwrap();

        delete_definition(&dd, "a", Some(&wd)).unwrap();

        // 文件被删
        assert!(!global_dir(&dd).join("a.md").exists());
        // settings 中的 key 被清
        let g = load_global_settings(&dd);
        assert!(!g.enabled.contains_key("a"));
        let p = load_project_settings(&dd, &wd);
        assert!(!p.subagents.enabled.contains_key("a"));
    }

    #[test]
    fn builtin_appears_when_no_disk_definitions() {
        let dd = tmp_data_dir("builtin-default");
        let defs = load_for_workdir(&dd, None);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"explore"), "内置 explore 应默认出现");
        assert!(names.contains(&"general-purpose"));
        assert!(defs.iter().all(|d| d.enabled), "内置缺省启用");
    }

    #[test]
    fn disk_definition_overrides_builtin_with_same_name() {
        let dd = tmp_data_dir("override-builtin");
        save_definition(
            &dd,
            "explore",
            "---\ndescription: my explore\n---\nCustom explore.",
        )
        .unwrap();
        let defs = load_for_workdir(&dd, None);
        let explore: Vec<_> = defs.iter().filter(|d| d.name == "explore").collect();
        assert_eq!(explore.len(), 1, "同名只保留磁盘版（覆盖内嵌）");
        assert_eq!(explore[0].description, "my explore");
        assert_eq!(explore[0].system_prompt, "Custom explore.");
    }

    // ── session-scoped 路由表 ──

    fn make_session_def(name: &str) -> SubagentDefinition {
        SubagentDefinition {
            name: name.to_string(),
            description: format!("{name} desc"),
            tools: None,
            model: None,
            max_iterations: None,
            system_prompt: format!("You are {name}."),
            enabled: true,
            source: SubagentSource::Session,
            permission: None,
        }
    }

    #[test]
    fn session_subagents_for_same_id_returns_same_arc() {
        let sid = format!("test-same-arc-{}", uuid::Uuid::new_v4());
        let a = session_subagents_for(&sid);
        let b = session_subagents_for(&sid);
        assert!(Arc::ptr_eq(&a, &b), "同 session_id 必须返回同一 Arc");
        discard_session_subagents(&sid);
    }

    #[test]
    fn session_subagents_for_different_ids_are_isolated() {
        let sid_a = format!("test-iso-a-{}", uuid::Uuid::new_v4());
        let sid_b = format!("test-iso-b-{}", uuid::Uuid::new_v4());
        let a = session_subagents_for(&sid_a);
        a.write().unwrap().push(make_session_def("alpha"));
        let b = session_subagents_for(&sid_b);
        assert!(b.read().unwrap().is_empty(), "不同 session 互不可见");
        discard_session_subagents(&sid_a);
        discard_session_subagents(&sid_b);
    }

    #[test]
    fn take_session_subagents_returns_clone() {
        let sid = format!("test-take-{}", uuid::Uuid::new_v4());
        let lock = session_subagents_for(&sid);
        lock.write().unwrap().push(make_session_def("beta"));
        let taken = take_session_subagents(&sid);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].name, "beta");
        assert_eq!(taken[0].source, SubagentSource::Session);
        // 原 Vec 不被清空（take 是 read+clone 语义）
        assert_eq!(lock.read().unwrap().len(), 1);
        discard_session_subagents(&sid);
    }

    #[test]
    fn take_session_subagents_unknown_id_returns_empty() {
        let sid = format!("test-unknown-{}", uuid::Uuid::new_v4());
        assert!(take_session_subagents(&sid).is_empty());
    }

    #[test]
    fn discard_clears_session_subagents() {
        let sid = format!("test-discard-{}", uuid::Uuid::new_v4());
        let a = session_subagents_for(&sid);
        a.write().unwrap().push(make_session_def("gamma"));
        discard_session_subagents(&sid);
        let b = session_subagents_for(&sid);
        assert!(
            !Arc::ptr_eq(&a, &b),
            "discard 后再取应得新 Arc（旧的已被摘除）"
        );
        assert!(b.read().unwrap().is_empty());
        discard_session_subagents(&sid);
    }
}
