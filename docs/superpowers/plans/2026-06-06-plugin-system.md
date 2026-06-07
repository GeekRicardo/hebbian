# Plugin System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 Hebbian 加入兼容 Claude Code 的插件系统，支持 marketplace 添加、插件安装/卸载，插件组件（skills / agents / hooks / MCP）自动路由到已有运行时。

**Architecture:** 插件系统是分发层——clone 插件 repo 到本地 cache，然后把各组件 symlink/copy/merge 到 Hebbian 已有的运行时路径（skills/ / subagents/ / hooks.json / mcp.json）。不修改 agent_core 主循环或 protocol。

**Tech Stack:** Rust (agent-core storage + core_client) / TypeScript (desktop frontend slashCommands)

---

## File Structure

### New Files
- `crates/agent-core/src/storage/plugins.rs` — 插件系统数据模型 + 全部 IO 操作
- (无新前端组件文件——命令走已有 `//` 命令系统)

### Modified Files
- `crates/agent-core/src/storage/mod.rs` — 加 `pub mod plugins;`
- `crates/agent-core/src/core_client/mod.rs` — CoreClient trait 加 plugin 方法 + LocalCoreClient 实现
- `apps/desktop/src/lib.rs` — Tauri commands
- `apps/desktop/frontend/src/desktop/bridge/tauri.ts` — API bindings
- `apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts` — `//plugin` 命令族
- `docs/架构.md` — 新增 §6.1.4 插件系统
- `docs/changelog.md` — 追加记录

---

### Task 1: storage/plugins.rs — 数据模型 + 基础 IO

**Files:**
- Create: `crates/agent-core/src/storage/plugins.rs`
- Modify: `crates/agent-core/src/storage/mod.rs`

**数据结构:**

```rust
// ~/.hebbian/plugins/registry.json
struct PluginRegistry {
    installed: Vec<InstalledPlugin>,
}

struct InstalledPlugin {
    name: String,              // plugin.json name
    display_name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    marketplace: Option<String>, // 来自哪个 marketplace
    repo_url: String,          // git clone URL
    subpath: Option<String>,   // mono-repo 内子路径
    installed_at: String,      // ISO timestamp
    // 安装时提取的组件清单（卸载时用来清理）
    components: InstalledComponents,
}

struct InstalledComponents {
    skills: Vec<String>,       // skill 目录名列表
    agents: Vec<String>,       // <plugin>-<agent-name>.md 列表
    hooks_merged: bool,        // 是否 merge 了 hooks
    mcp_servers: Vec<String>,  // merge 进全局 mcp 的 server 名列表
}

// ~/.hebbian/plugins/marketplaces.json
struct MarketplaceRegistry {
    marketplaces: Vec<MarketplaceEntry>,
}

struct MarketplaceEntry {
    name: String,              // 显示名
    source: MarketplaceSource,
    added_at: String,
}

enum MarketplaceSource {
    Github { owner: String, repo: String },
    GitUrl { url: String },
}

// marketplace.json 的 plugin 条目（从 repo 读取，不落盘）
struct MarketplaceCatalogEntry {
    name: String,
    description: Option<String>,
    source: PluginSourceEntry,
    homepage: Option<String>,
}

enum PluginSourceEntry {
    // 相对路径（同仓库内的子目录）
    Relative { path: String },
    // 独立 git 仓库
    Url { url: String, sha: Option<String> },
}
```

**核心函数:**
- `load_registry(data_dir) -> PluginRegistry`
- `save_registry(data_dir, &PluginRegistry)`
- `load_marketplaces(data_dir) -> MarketplaceRegistry`
- `save_marketplaces(data_dir, &MarketplaceRegistry)`
- `plugin_cache_dir(data_dir, plugin_name) -> PathBuf`
- `marketplace_add(data_dir, source: &str) -> Result<MarketplaceEntry>` — clone + 检测类型
- `marketplace_list_plugins(data_dir, marketplace_name) -> Result<Vec<MarketplaceCatalogEntry>>`
- `plugin_install(data_dir, name, marketplace?) -> Result<InstalledPlugin>` — clone + 解析 + 提取
- `plugin_uninstall(data_dir, name) -> Result<()>` — 清理所有组件
- `plugin_list(data_dir) -> Vec<InstalledPlugin>`

---

### Task 2: 插件安装核心逻辑 — clone + parse plugin.json + 组件提取

**Files:**
- Modify: `crates/agent-core/src/storage/plugins.rs`

**关键实现:**

1. `clone_plugin_repo(repo_url, subpath) -> TempDir` — git clone --depth=1
2. `parse_plugin_json(dir) -> PluginManifest` — 读 .claude-plugin/plugin.json
3. `extract_skills(plugin_dir, data_dir, plugin_name)` — 对 skills/ 下每个子目录 symlink 到 `~/.hebbian/skills/<name>`
4. `extract_agents(plugin_dir, data_dir, plugin_name)` — 对 agents/*.md copy 到 `~/.hebbian/subagents/<plugin>-<name>.md`
5. `extract_hooks(plugin_dir, data_dir, plugin_name)` — 读 hooks/hooks.json，用 plugin 前缀写入全局 hooks.json（不覆盖用户规则，追加段）
6. `extract_mcp(plugin_dir, data_dir, plugin_name)` — 读 .mcp.json，namespace 前缀 merge 进全局 mcp.json
7. 变量替换：`${CLAUDE_PLUGIN_ROOT}` → cache 目录绝对路径

---

### Task 3: CoreClient trait + LocalCoreClient 实现

**Files:**
- Modify: `crates/agent-core/src/core_client/mod.rs`

新增方法:
```rust
// === 同步 API：Plugins（§6.1.4）===
fn plugin_marketplace_add(&self, source: &str) -> Result<String, CoreError>;
fn plugin_marketplace_list(&self) -> Vec<(String, String)>; // (name, source_desc)
fn plugin_marketplace_remove(&self, name: &str) -> Result<(), CoreError>;
fn plugin_install(&self, name: &str, marketplace: Option<&str>) -> Result<String, CoreError>;
fn plugin_uninstall(&self, name: &str) -> Result<(), CoreError>;
fn plugin_list(&self) -> Vec<(String, Option<String>, Option<String>)>; // (name, version, desc)
```

---

### Task 4: Desktop Tauri commands

**Files:**
- Modify: `apps/desktop/src/lib.rs`

新增 6 个 `#[tauri::command]`:
- `plugin_marketplace_add(app, source: String) -> AppResult<String>`
- `plugin_marketplace_list(app) -> AppResult<Vec<(String, String)>>`
- `plugin_marketplace_remove(app, name: String) -> AppResult<()>`
- `plugin_install(app, name: String, marketplace: Option<String>) -> AppResult<String>`
- `plugin_uninstall(app, name: String) -> AppResult<()>`
- `plugin_list(app) -> AppResult<Vec<PluginListItem>>`

---

### Task 5: 前端 API + slashCommands

**Files:**
- Modify: `apps/desktop/frontend/src/desktop/bridge/tauri.ts`
- Modify: `apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts`

tauri.ts 加 API bindings。

slashCommands.ts 新增 `//plugin` 命令族：
- 注册为内置命令
- handler 解析子命令 (marketplace add/list/remove, install, uninstall, list)
- 结果通过 toast 展示

---

### Task 6: 文档 + changelog

**Files:**
- Modify: `docs/架构.md` — §6.1.4
- Modify: `docs/changelog.md`
