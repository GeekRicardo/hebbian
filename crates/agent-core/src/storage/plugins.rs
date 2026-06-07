//! 插件系统持久化（架构 §6.1.4）。
//!
//! 数据布局：
//! ```text
//! ~/.hebbian/plugins/
//! ├── registry.json          已安装插件清单
//! ├── marketplaces.json      已添加的 marketplace 列表
//! └── cache/<plugin-name>/   插件源文件缓存（shallow clone 结果）
//! ```
//!
//! 插件安装后，各组件按类型路由到 Hebbian 已有运行时路径：
//! - skills  → symlink 到 `~/.hebbian/skills/<name>/`
//! - agents  → copy 到 `~/.hebbian/subagents/<plugin>-<name>.md`
//! - hooks   → merge 进 `~/.hebbian/hooks.json`（带 plugin 前缀标记）
//! - MCP     → merge 进 `~/.hebbian/mcp.json`（server name 带 plugin 前缀）

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};

// ── 数据模型 ──────────────────────────────────────────────────────

/// `~/.hebbian/plugins/registry.json`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginRegistry {
    #[serde(default)]
    pub installed: Vec<InstalledPlugin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<String>,
    pub repo_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    pub installed_at: String,
    pub components: InstalledComponents,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstalledComponents {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub hooks_merged: bool,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
}

/// `~/.hebbian/plugins/marketplaces.json`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketplaceRegistry {
    #[serde(default)]
    pub marketplaces: Vec<MarketplaceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceEntry {
    pub name: String,
    pub source: MarketplaceSource,
    pub added_at: String,
    /// 如果 marketplace 实际只是一个单插件 repo（没有 marketplace.json），
    /// 就把 plugin manifest 信息缓存在这里。
    #[serde(default)]
    pub is_single_plugin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MarketplaceSource {
    Github { owner: String, repo: String },
    GitUrl { url: String },
}

impl MarketplaceSource {
    pub fn clone_url(&self) -> String {
        match self {
            Self::Github { owner, repo } => {
                format!("https://github.com/{owner}/{repo}.git")
            }
            Self::GitUrl { url } => url.clone(),
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Github { owner, repo } => format!("{owner}/{repo}"),
            Self::GitUrl { url } => url.clone(),
        }
    }
}

/// Claude Code marketplace.json 中的 plugin 条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_catalog_source")]
    pub source: Option<CatalogPluginSource>,
    #[serde(default)]
    pub homepage: Option<String>,
}

/// marketplace.json 里的 plugin source，实际有三种格式：
/// 1. 纯字符串 `"./path"` → 相对路径（Claude Code 最常见）
/// 2. 对象 `{"source":"url","url":"https://...","sha":"..."}` → 独立 git repo
/// 3. 对象 `{"source":"relative","path":"./..."}` → 同仓库子目录（罕见）
#[derive(Debug, Clone, Serialize)]
pub enum CatalogPluginSource {
    Relative { path: String },
    Url { url: String, sha: Option<String> },
}

fn deserialize_catalog_source<'de, D>(
    deserializer: D,
) -> Result<Option<CatalogPluginSource>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    let Some(v) = value else {
        return Ok(None);
    };
    match v {
        serde_json::Value::String(s) => {
            Ok(Some(CatalogPluginSource::Relative { path: s }))
        }
        serde_json::Value::Object(map) => {
            let tag = map
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("relative");
            match tag {
                "url" => {
                    let url = map
                        .get("url")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| de::Error::missing_field("url"))?
                        .to_string();
                    let sha = map.get("sha").and_then(|v| v.as_str()).map(String::from);
                    Ok(Some(CatalogPluginSource::Url { url, sha }))
                }
                _ => {
                    let path = map
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or(".")
                        .to_string();
                    Ok(Some(CatalogPluginSource::Relative { path }))
                }
            }
        }
        _ => Ok(None),
    }
}

/// `.claude-plugin/plugin.json` 的解析结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    // 组件路径（默认自动发现 skills/ agents/ hooks/ .mcp.json）
    #[serde(default)]
    pub skills: Option<serde_json::Value>,
    #[serde(default)]
    pub agents: Option<serde_json::Value>,
    #[serde(default)]
    pub hooks: Option<serde_json::Value>,
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: Option<serde_json::Value>,
}

/// marketplace.json 的顶层结构。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketplaceCatalog {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub plugins: Vec<CatalogEntry>,
}

/// 插件列表展示项（给前端用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginListItem {
    pub name: String,
    pub display_name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub marketplace: Option<String>,
    pub skills_count: usize,
    pub agents_count: usize,
    pub has_hooks: bool,
    pub mcp_servers_count: usize,
}

// ── 目录路径 ──────────────────────────────────────────────────────

fn plugins_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("plugins")
}

fn registry_path(data_dir: &Path) -> PathBuf {
    plugins_dir(data_dir).join("registry.json")
}

fn marketplaces_path(data_dir: &Path) -> PathBuf {
    plugins_dir(data_dir).join("marketplaces.json")
}

fn cache_dir(data_dir: &Path) -> PathBuf {
    plugins_dir(data_dir).join("cache")
}

pub fn plugin_cache_dir(data_dir: &Path, plugin_name: &str) -> PathBuf {
    cache_dir(data_dir).join(plugin_name)
}

fn marketplace_clone_dir(data_dir: &Path, name: &str) -> PathBuf {
    plugins_dir(data_dir).join("marketplace-clones").join(name)
}

// ── Registry IO ──────────────────────────────────────────────────

pub fn load_registry(data_dir: &Path) -> PluginRegistry {
    let p = registry_path(data_dir);
    if !p.exists() {
        return PluginRegistry::default();
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_registry(data_dir: &Path, reg: &PluginRegistry) -> AppResult<()> {
    let dir = plugins_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(reg)?;
    std::fs::write(registry_path(data_dir), json)?;
    Ok(())
}

pub fn load_marketplaces(data_dir: &Path) -> MarketplaceRegistry {
    let p = marketplaces_path(data_dir);
    if !p.exists() {
        return MarketplaceRegistry::default();
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_marketplaces(data_dir: &Path, reg: &MarketplaceRegistry) -> AppResult<()> {
    let dir = plugins_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(reg)?;
    std::fs::write(marketplaces_path(data_dir), json)?;
    Ok(())
}

// ── Git 操作 ─────────────────────────────────────────────────────

fn git_clone_shallow(url: &str, dest: &Path) -> AppResult<()> {
    std::fs::create_dir_all(dest)?;
    let output = std::process::Command::new("git")
        .args(["clone", "--depth=1", "--quiet", url])
        .arg(dest)
        .output()
        .map_err(|e| AppError::msg(format!("git 未找到或调用失败：{e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::msg(format!(
            "git clone 失败：{}",
            stderr.lines().next().unwrap_or("未知错误")
        )));
    }
    Ok(())
}

// ── Marketplace 操作 ─────────────────────────────────────────────

/// 解析 `owner/repo` 或完整 git URL。
fn parse_source(input: &str) -> AppResult<MarketplaceSource> {
    let trimmed = input.trim();
    if trimmed.contains("://") || trimmed.starts_with("git@") || trimmed.ends_with(".git") {
        return Ok(MarketplaceSource::GitUrl {
            url: trimmed.to_string(),
        });
    }
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        return Ok(MarketplaceSource::Github {
            owner: parts[0].to_string(),
            repo: parts[1].to_string(),
        });
    }
    Err(AppError::msg(format!(
        "无法解析来源：{trimmed}（期望 owner/repo 或 git URL）"
    )))
}

/// 添加 marketplace。如果 repo 包含 `.claude-plugin/marketplace.json` 则视为
/// marketplace；否则检查 `.claude-plugin/plugin.json`，视为单插件 marketplace。
pub fn marketplace_add(data_dir: &Path, input: &str) -> AppResult<MarketplaceEntry> {
    let source = parse_source(input)?;
    let clone_url = source.clone_url();

    // 检查是否已添加
    let mut reg = load_marketplaces(data_dir);
    let source_display = source.display();
    if reg.marketplaces.iter().any(|m| m.source.display() == source_display) {
        return Err(AppError::msg(format!(
            "已添加过该来源：{source_display}"
        )));
    }

    // clone 到临时目录探测类型
    let tmp = std::env::temp_dir().join(format!("hebbian-mkt-{}", uuid::Uuid::new_v4()));
    git_clone_shallow(&clone_url, &tmp)?;

    let marketplace_json = tmp.join(".claude-plugin").join("marketplace.json");
    let plugin_json = tmp.join(".claude-plugin").join("plugin.json");
    let is_marketplace = marketplace_json.exists();
    let is_single_plugin = !is_marketplace && plugin_json.exists();

    if !is_marketplace && !is_single_plugin {
        // 顶层有 SKILL.md？也算单插件
        let has_skill = tmp.join("SKILL.md").exists() || tmp.join("skills").exists();
        if !has_skill {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(AppError::msg(
                "仓库中既没有 .claude-plugin/marketplace.json 也没有 .claude-plugin/plugin.json",
            ));
        }
    }

    // 确定 marketplace name
    let name = if is_marketplace {
        // 从 marketplace.json 读 name
        std::fs::read_to_string(&marketplace_json)
            .ok()
            .and_then(|s| serde_json::from_str::<MarketplaceCatalog>(&s).ok())
            .and_then(|c| c.name)
            .unwrap_or_else(|| match &source {
                MarketplaceSource::Github { repo, .. } => repo.clone(),
                MarketplaceSource::GitUrl { url } => url
                    .rsplit('/')
                    .next()
                    .unwrap_or("unknown")
                    .trim_end_matches(".git")
                    .to_string(),
            })
    } else {
        // 单插件 repo：用 plugin name 或 repo name
        std::fs::read_to_string(&plugin_json)
            .ok()
            .and_then(|s| serde_json::from_str::<PluginManifest>(&s).ok())
            .map(|m| m.name)
            .unwrap_or_else(|| match &source {
                MarketplaceSource::Github { repo, .. } => repo.clone(),
                MarketplaceSource::GitUrl { url } => url
                    .rsplit('/')
                    .next()
                    .unwrap_or("unknown")
                    .trim_end_matches(".git")
                    .to_string(),
            })
    };

    // 持久化 clone 结果到 marketplace-clones/
    let clone_dest = marketplace_clone_dir(data_dir, &name);
    if clone_dest.exists() {
        let _ = std::fs::remove_dir_all(&clone_dest);
    }
    std::fs::create_dir_all(clone_dest.parent().unwrap_or(Path::new(".")))?;
    std::fs::rename(&tmp, &clone_dest).or_else(|_| {
        super::copy_dir_all(&tmp, &clone_dest)?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok::<_, std::io::Error>(())
    })?;

    let entry = MarketplaceEntry {
        name: name.clone(),
        source,
        added_at: chrono::Utc::now().to_rfc3339(),
        is_single_plugin,
    };
    reg.marketplaces.push(entry.clone());
    save_marketplaces(data_dir, &reg)?;
    Ok(entry)
}

/// 列出 marketplace 中的 plugin 目录。
pub fn marketplace_list_plugins(
    data_dir: &Path,
    marketplace_name: &str,
) -> AppResult<Vec<CatalogEntry>> {
    let reg = load_marketplaces(data_dir);
    let entry = reg
        .marketplaces
        .iter()
        .find(|m| m.name == marketplace_name)
        .ok_or_else(|| AppError::msg(format!("未找到 marketplace：{marketplace_name}")))?;

    let clone_dir = marketplace_clone_dir(data_dir, marketplace_name);
    if !clone_dir.exists() {
        return Err(AppError::msg(format!(
            "marketplace 缓存不存在，请重新添加：{marketplace_name}"
        )));
    }

    if entry.is_single_plugin {
        // 单插件 repo → 返回一个 entry
        let manifest = read_plugin_manifest(&clone_dir)?;
        return Ok(vec![CatalogEntry {
            name: manifest.name,
            description: manifest.description,
            source: None,
            homepage: manifest.homepage.or(manifest.repository),
        }]);
    }

    // 真正的 marketplace → 读 marketplace.json
    let mkt_path = clone_dir
        .join(".claude-plugin")
        .join("marketplace.json");
    let raw = std::fs::read_to_string(&mkt_path)
        .map_err(|e| AppError::msg(format!("读取 marketplace.json 失败：{e}")))?;
    let catalog: MarketplaceCatalog = serde_json::from_str(&raw)
        .map_err(|e| AppError::msg(format!("解析 marketplace.json 失败：{e}")))?;
    Ok(catalog.plugins)
}

/// 删除 marketplace。
pub fn marketplace_remove(data_dir: &Path, name: &str) -> AppResult<()> {
    let mut reg = load_marketplaces(data_dir);
    let before = reg.marketplaces.len();
    reg.marketplaces.retain(|m| m.name != name);
    if reg.marketplaces.len() == before {
        return Err(AppError::msg(format!("未找到 marketplace：{name}")));
    }
    save_marketplaces(data_dir, &reg)?;
    // 清理 clone 缓存
    let clone_dir = marketplace_clone_dir(data_dir, name);
    if clone_dir.exists() {
        let _ = std::fs::remove_dir_all(&clone_dir);
    }
    Ok(())
}

// ── Plugin manifest 解析 ─────────────────────────────────────────

fn read_plugin_manifest(plugin_dir: &Path) -> AppResult<PluginManifest> {
    let path = plugin_dir.join(".claude-plugin").join("plugin.json");
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| AppError::msg(format!("读取 plugin.json 失败：{e}")))?;
        let manifest: PluginManifest = serde_json::from_str(&raw)
            .map_err(|e| AppError::msg(format!("解析 plugin.json 失败：{e}")))?;
        return Ok(manifest);
    }
    // 没有 plugin.json：从目录名推导
    let name = plugin_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    Ok(PluginManifest {
        name,
        ..Default::default()
    })
}

// ── 组件提取 ─────────────────────────────────────────────────────

/// 把 `${CLAUDE_PLUGIN_ROOT}` 替换为实际缓存路径。
fn substitute_plugin_root(s: &str, plugin_root: &Path) -> String {
    s.replace("${CLAUDE_PLUGIN_ROOT}", &plugin_root.display().to_string())
}

fn substitute_in_value(v: &mut serde_json::Value, plugin_root: &Path) {
    match v {
        serde_json::Value::String(s) => {
            *s = substitute_plugin_root(s, plugin_root);
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                substitute_in_value(item, plugin_root);
            }
        }
        serde_json::Value::Object(obj) => {
            for (_, val) in obj.iter_mut() {
                substitute_in_value(val, plugin_root);
            }
        }
        _ => {}
    }
}

/// 提取 skills：对 plugin_dir/skills/ 下的每个含 SKILL.md 的子目录，
/// 创建 symlink 从 `~/.hebbian/skills/<name>` 指向 cache 中的目录。
fn extract_skills(plugin_dir: &Path, data_dir: &Path) -> AppResult<Vec<String>> {
    let skills_dir = plugin_dir.join("skills");
    let mut extracted = Vec::new();

    // 单 skill 插件：根目录有 SKILL.md 但没有 skills/ 目录
    if !skills_dir.exists() {
        let root_skill = plugin_dir.join("SKILL.md");
        if root_skill.exists() {
            let manifest = read_plugin_manifest(plugin_dir)?;
            let name = manifest.name.clone();
            let target = data_dir.join("skills").join(&name);
            std::fs::create_dir_all(data_dir.join("skills"))?;
            if target.exists() {
                // 移除旧的 symlink 或目录
                if target.is_symlink() {
                    std::fs::remove_file(&target)?;
                } else {
                    std::fs::remove_dir_all(&target)?;
                }
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(plugin_dir, &target)?;
            #[cfg(not(unix))]
            super::copy_dir_all(plugin_dir, &target)?;
            extracted.push(name);
        }
        return Ok(extracted);
    }

    std::fs::create_dir_all(data_dir.join("skills"))?;
    let entries = std::fs::read_dir(&skills_dir)
        .map_err(|e| AppError::msg(format!("读取 skills/ 目录失败：{e}")))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("SKILL.md").exists() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let target = data_dir.join("skills").join(&name);
        if target.exists() {
            if target.is_symlink() {
                std::fs::remove_file(&target)?;
            } else {
                std::fs::remove_dir_all(&target)?;
            }
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&path, &target)?;
        #[cfg(not(unix))]
        super::copy_dir_all(&path, &target)?;
        extracted.push(name);
    }
    Ok(extracted)
}

/// 提取 agents：对 plugin_dir/agents/*.md，copy 到
/// `~/.hebbian/subagents/<plugin-name>-<agent-name>.md`。
fn extract_agents(
    plugin_dir: &Path,
    data_dir: &Path,
    plugin_name: &str,
) -> AppResult<Vec<String>> {
    let agents_dir = plugin_dir.join("agents");
    if !agents_dir.exists() {
        return Ok(Vec::new());
    }
    let dest_dir = data_dir.join("subagents");
    std::fs::create_dir_all(&dest_dir)?;
    let mut extracted = Vec::new();
    let entries = std::fs::read_dir(&agents_dir)
        .map_err(|e| AppError::msg(format!("读取 agents/ 目录失败：{e}")))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let dest_name = format!("{plugin_name}-{stem}");
        let dest_path = dest_dir.join(format!("{dest_name}.md"));
        std::fs::copy(&path, &dest_path)?;
        extracted.push(dest_name);
    }
    Ok(extracted)
}

/// 提取 hooks：读 plugin 的 hooks/hooks.json，转换为 Hebbian 格式后
/// merge 进全局 `~/.hebbian/hooks.json`。
///
/// Claude Code hooks.json 格式与 Hebbian 不同：
/// ```json
/// { "hooks": { "PostToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "..." }] }] } }
/// ```
/// Hebbian 格式：
/// ```json
/// { "PostToolUse": [{ "matcher": { "tool": "Bash" }, "command": "..." }] }
/// ```
fn extract_hooks(
    plugin_dir: &Path,
    data_dir: &Path,
    plugin_name: &str,
) -> AppResult<bool> {
    let hooks_path = plugin_dir.join("hooks").join("hooks.json");
    if !hooks_path.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(&hooks_path)
        .map_err(|e| AppError::msg(format!("读取 hooks.json 失败：{e}")))?;
    let top: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| AppError::msg(format!("解析 hooks.json 失败：{e}")))?;

    // 解析出事件 → 规则列表的映射，兼容两种格式
    let plugin_hooks = parse_plugin_hooks(&top, plugin_dir)?;
    if plugin_hooks.is_empty() {
        return Ok(false);
    }

    let global_hooks_path = data_dir.join("hooks.json");
    let mut global: BTreeMap<String, Vec<serde_json::Value>> = if global_hooks_path.exists() {
        std::fs::read_to_string(&global_hooks_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        BTreeMap::new()
    };

    // 先移除该 plugin 的旧规则
    for rules in global.values_mut() {
        rules.retain(|r| {
            r.get("_plugin")
                .and_then(|v| v.as_str())
                .map(|s| s != plugin_name)
                .unwrap_or(true)
        });
    }

    for (event, mut rules) in plugin_hooks {
        for rule in &mut rules {
            if let serde_json::Value::Object(obj) = rule {
                obj.insert(
                    "_plugin".to_string(),
                    serde_json::Value::String(plugin_name.to_string()),
                );
            }
        }
        global.entry(event).or_default().extend(rules);
    }

    // 清理空事件
    global.retain(|_, v| !v.is_empty());

    std::fs::write(
        &global_hooks_path,
        serde_json::to_string_pretty(&global)?,
    )?;
    Ok(true)
}

/// 把 Claude Code 或 Hebbian 格式的 hooks.json 统一转成 Hebbian 格式。
///
/// Claude Code 格式：
/// ```json
/// { "hooks": { "Event": [{ "matcher": "ToolName", "hooks": [{ "type": "command", "command": "..." }] }] } }
/// ```
/// Hebbian 格式：
/// ```json
/// { "Event": [{ "matcher": { "tool": "ToolName" }, "command": "..." }] }
/// ```
fn parse_plugin_hooks(
    top: &serde_json::Value,
    plugin_dir: &Path,
) -> AppResult<BTreeMap<String, Vec<serde_json::Value>>> {
    // 检测格式：如果顶层有 "hooks" key 且值是 object → Claude Code 格式
    let events_obj = if let Some(inner) = top.get("hooks").and_then(|v| v.as_object()) {
        inner.clone()
    } else if let Some(obj) = top.as_object() {
        // Hebbian 原生格式：顶层直接是 event → rules
        let mut result: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        for (event, rules_val) in obj {
            if let Some(rules) = rules_val.as_array() {
                let mut converted = Vec::new();
                for rule in rules {
                    let mut r = rule.clone();
                    substitute_in_value(&mut r, plugin_dir);
                    converted.push(r);
                }
                result.insert(event.clone(), converted);
            }
        }
        return Ok(result);
    } else {
        return Ok(BTreeMap::new());
    };

    // Claude Code 格式：转换
    let mut result: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for (event, entries_val) in &events_obj {
        let Some(entries) = entries_val.as_array() else {
            continue;
        };
        let mut hebbian_rules = Vec::new();
        for entry in entries {
            // 每个 entry 形如 { "matcher": "Bash", "hooks": [{ "type": "command", "command": "..." }] }
            let matcher_str = entry
                .get("matcher")
                .and_then(|v| v.as_str())
                .unwrap_or("*");
            let sub_hooks = entry
                .get("hooks")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            for hook in sub_hooks {
                // 提取 command
                let command = hook
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if command.is_empty() {
                    continue;
                }
                let command = substitute_plugin_root(&command, plugin_dir);

                let mut rule = serde_json::json!({
                    "command": command,
                });
                // matcher 转成 Hebbian 的 { "tool": "..." } 格式
                if matcher_str != "*" {
                    rule["matcher"] = serde_json::json!({ "tool": matcher_str });
                }
                hebbian_rules.push(rule);
            }
        }
        if !hebbian_rules.is_empty() {
            result.insert(event.clone(), hebbian_rules);
        }
    }
    Ok(result)
}

/// 提取 MCP：读 plugin 的 .mcp.json，用 `<plugin>-` 前缀 merge 进全局 mcp.json。
fn extract_mcp(
    plugin_dir: &Path,
    data_dir: &Path,
    plugin_name: &str,
) -> AppResult<Vec<String>> {
    let mcp_path = plugin_dir.join(".mcp.json");
    if !mcp_path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&mcp_path)
        .map_err(|e| AppError::msg(format!("读取 .mcp.json 失败：{e}")))?;
    let mut plugin_mcp: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| AppError::msg(format!("解析 .mcp.json 失败：{e}")))?;
    substitute_in_value(&mut plugin_mcp, plugin_dir);

    // 提取 mcpServers 或 servers 字段
    let servers_obj = plugin_mcp
        .get("mcpServers")
        .or_else(|| plugin_mcp.get("mcp_servers"))
        .or_else(|| plugin_mcp.get("servers"))
        .cloned();
    let servers: BTreeMap<String, serde_json::Value> = match servers_obj {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => return Ok(Vec::new()),
    };
    if servers.is_empty() {
        return Ok(Vec::new());
    }

    // 加载全局 mcp 配置
    let mut global_config = crate::storage::mcp::load(data_dir);
    let mut added = Vec::new();

    for (server_name, server_config) in servers {
        let namespaced = format!("{plugin_name}-{server_name}");
        let cfg: crate::mcp::config::McpServerConfig =
            serde_json::from_value(server_config).unwrap_or_default();
        global_config.mcp_servers.insert(namespaced.clone(), cfg);
        added.push(namespaced);
    }

    crate::storage::mcp::save(data_dir, &global_config)?;
    Ok(added)
}

/// 移除 plugin 的 hooks 规则。
fn remove_hooks(data_dir: &Path, plugin_name: &str) {
    let path = data_dir.join("hooks.json");
    if !path.exists() {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut global) = serde_json::from_str::<BTreeMap<String, Vec<serde_json::Value>>>(&raw)
    else {
        return;
    };
    for rules in global.values_mut() {
        rules.retain(|r| {
            r.get("_plugin")
                .and_then(|v| v.as_str())
                .map(|s| s != plugin_name)
                .unwrap_or(true)
        });
    }
    global.retain(|_, v| !v.is_empty());
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&global).unwrap_or_default());
}

/// 移除 plugin 的 MCP servers。
fn remove_mcp_servers(data_dir: &Path, servers: &[String]) {
    if servers.is_empty() {
        return;
    }
    let mut config = crate::storage::mcp::load(data_dir);
    for name in servers {
        config.mcp_servers.remove(name);
    }
    let _ = crate::storage::mcp::save(data_dir, &config);
}

// ── 插件安装/卸载 ────────────────────────────────────────────────

/// 安装一个插件。
///
/// 查找逻辑：
/// 1. 如果 `marketplace` 参数给了，只在该 marketplace 里找
/// 2. 否则遍历所有 marketplace 找第一个匹配的
/// 3. 找到后 clone 到 cache → 提取组件 → 写 registry
pub fn plugin_install(
    data_dir: &Path,
    name: &str,
    marketplace: Option<&str>,
) -> AppResult<InstalledPlugin> {
    // 检查是否已安装
    let reg = load_registry(data_dir);
    if reg.installed.iter().any(|p| p.name == name) {
        return Err(AppError::msg(format!("插件已安装：{name}")));
    }

    // 查找 plugin 来源
    let mkt_reg = load_marketplaces(data_dir);
    let found = find_plugin_source(data_dir, name, marketplace, &mkt_reg)?;

    // 准备 plugin cache 目录
    let cache = plugin_cache_dir(data_dir, name);
    if cache.exists() {
        std::fs::remove_dir_all(&cache)?;
    }

    let (repo_url, subpath) = match &found {
        FoundPluginSource::Remote { repo_url } => {
            git_clone_shallow(repo_url, &cache)?;
            (repo_url.clone(), None)
        }
        FoundPluginSource::LocalSubdir {
            marketplace_clone,
            subpath,
        } => {
            // 从 marketplace clone 拷贝子目录到 cache，避免重复网络请求
            let normalized = subpath.trim_start_matches("./").trim_start_matches('/');
            let src = if normalized.is_empty() || normalized == "." {
                marketplace_clone.clone()
            } else {
                marketplace_clone.join(normalized)
            };
            if !src.exists() {
                return Err(AppError::msg(format!(
                    "marketplace clone 里找不到子目录：{}",
                    src.display()
                )));
            }
            super::copy_dir_all(&src, &cache)?;
            ("(local)".to_string(), Some(subpath.clone()))
        }
    };

    let plugin_root = cache.clone();

    // 解析 manifest
    let manifest = read_plugin_manifest(&plugin_root)?;

    // 提取组件
    let skills = extract_skills(&plugin_root, data_dir)?;
    let agents = extract_agents(&plugin_root, data_dir, name)?;
    let hooks_merged = extract_hooks(&plugin_root, data_dir, name)?;
    let mcp_servers = extract_mcp(&plugin_root, data_dir, name)?;

    // 记录 skill collection
    if !skills.is_empty() {
        let label = manifest
            .display_name
            .as_deref()
            .unwrap_or(&manifest.name)
            .to_string();
        let source = crate::storage::skill_collections::CollectionSource::Plugin {
            plugin_name: name.to_string(),
        };
        let _ = crate::storage::skill_collections::record_import(
            data_dir,
            label,
            source,
            skills.clone(),
        );
    }

    let installed = InstalledPlugin {
        name: manifest.name.clone(),
        display_name: manifest.display_name,
        version: manifest.version,
        description: manifest.description,
        marketplace: marketplace.map(String::from),
        repo_url,
        subpath,
        installed_at: chrono::Utc::now().to_rfc3339(),
        components: InstalledComponents {
            skills,
            agents,
            hooks_merged,
            mcp_servers,
        },
    };

    let mut reg = load_registry(data_dir);
    reg.installed.push(installed.clone());
    save_registry(data_dir, &reg)?;
    Ok(installed)
}

/// 插件来源查找结果。
enum FoundPluginSource {
    /// 独立 git repo，需要 clone
    Remote { repo_url: String },
    /// marketplace clone 里的子目录，直接拷贝
    LocalSubdir { marketplace_clone: PathBuf, subpath: String },
}

/// 在 marketplace 中查找 plugin 的 repo URL + subpath。
fn find_plugin_source(
    data_dir: &Path,
    name: &str,
    marketplace_filter: Option<&str>,
    mkt_reg: &MarketplaceRegistry,
) -> AppResult<FoundPluginSource> {
    let candidates: Vec<&MarketplaceEntry> = match marketplace_filter {
        Some(mkt) => mkt_reg
            .marketplaces
            .iter()
            .filter(|m| m.name == mkt)
            .collect(),
        None => mkt_reg.marketplaces.iter().collect(),
    };

    if candidates.is_empty() {
        return Err(AppError::msg(if marketplace_filter.is_some() {
            format!("未找到 marketplace：{}", marketplace_filter.unwrap())
        } else {
            "没有已添加的 marketplace，请先 //plugin marketplace add <owner/repo>".to_string()
        }));
    }

    for mkt in candidates {
        // 单插件 marketplace：name 直接匹配
        if mkt.is_single_plugin {
            let clone_dir = marketplace_clone_dir(data_dir, &mkt.name);
            if let Ok(manifest) = read_plugin_manifest(&clone_dir) {
                if manifest.name == name {
                    return Ok(FoundPluginSource::LocalSubdir {
                        marketplace_clone: clone_dir,
                        subpath: ".".to_string(),
                    });
                }
            }
            continue;
        }

        // 真正 marketplace：查 catalog
        let clone_dir = marketplace_clone_dir(data_dir, &mkt.name);
        let mkt_path = clone_dir.join(".claude-plugin").join("marketplace.json");
        let Ok(raw) = std::fs::read_to_string(&mkt_path) else {
            continue;
        };
        let Ok(catalog) = serde_json::from_str::<MarketplaceCatalog>(&raw) else {
            continue;
        };

        if let Some(entry) = catalog.plugins.iter().find(|p| p.name == name) {
            match &entry.source {
                Some(CatalogPluginSource::Url { url, .. }) => {
                    return Ok(FoundPluginSource::Remote {
                        repo_url: url.clone(),
                    });
                }
                Some(CatalogPluginSource::Relative { path }) => {
                    // 同仓库子目录 → 直接用 marketplace clone
                    return Ok(FoundPluginSource::LocalSubdir {
                        marketplace_clone: clone_dir,
                        subpath: path.clone(),
                    });
                }
                None => {
                    // 没有显式 source：假设同仓库、目录名 = plugin name
                    return Ok(FoundPluginSource::LocalSubdir {
                        marketplace_clone: clone_dir,
                        subpath: format!("plugins/{name}"),
                    });
                }
            }
        }
    }

    Err(AppError::msg(format!(
        "在所有 marketplace 中都没找到插件：{name}"
    )))
}

/// 卸载插件：清理所有组件 + registry 记录。
pub fn plugin_uninstall(data_dir: &Path, name: &str) -> AppResult<()> {
    let mut reg = load_registry(data_dir);
    let idx = reg
        .installed
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| AppError::msg(format!("插件未安装：{name}")))?;
    let plugin = reg.installed.remove(idx);

    // 清理 skills（symlinks）
    for skill_name in &plugin.components.skills {
        let path = data_dir.join("skills").join(skill_name);
        if path.is_symlink() {
            let _ = std::fs::remove_file(&path);
        } else if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }

    // 清理 skill collection
    if !plugin.components.skills.is_empty() {
        let _ = crate::storage::skill_collections::remove_by_plugin(data_dir, name);
    }

    // 清理 agents
    for agent_name in &plugin.components.agents {
        let path = data_dir.join("subagents").join(format!("{agent_name}.md"));
        let _ = std::fs::remove_file(&path);
    }

    // 清理 hooks
    if plugin.components.hooks_merged {
        remove_hooks(data_dir, name);
    }

    // 清理 MCP servers
    remove_mcp_servers(data_dir, &plugin.components.mcp_servers);

    // 清理 cache
    let cache = plugin_cache_dir(data_dir, name);
    if cache.exists() {
        let _ = std::fs::remove_dir_all(&cache);
    }

    save_registry(data_dir, &reg)?;
    Ok(())
}

/// 列出已安装的插件（UI 友好格式）。
pub fn plugin_list(data_dir: &Path) -> Vec<PluginListItem> {
    let reg = load_registry(data_dir);
    reg.installed
        .into_iter()
        .map(|p| PluginListItem {
            name: p.name,
            display_name: p.display_name,
            version: p.version,
            description: p.description,
            marketplace: p.marketplace,
            skills_count: p.components.skills.len(),
            agents_count: p.components.agents.len(),
            has_hooks: p.components.hooks_merged,
            mcp_servers_count: p.components.mcp_servers.len(),
        })
        .collect()
}

/// 列出已添加的 marketplace。
pub fn marketplace_list(data_dir: &Path) -> Vec<(String, String)> {
    let reg = load_marketplaces(data_dir);
    reg.marketplaces
        .into_iter()
        .map(|m| (m.name, m.source.display()))
        .collect()
}
