//! Core RPC：dispatch 唯一 command 入口（架构 §7.1 / §7.8.6 步骤2）。
//!
//! 把 agent-core 的全部业务能力（[`CoreClient`] 61 个同步方法 + 对话主链路）收进一个
//! 强类型 [`CoreRequest`] enum，由唯一的 [`dispatch`] 大 match 处理——对标 codex
//! `app-server` 的 `message_processor`。三 surface（desktop in-process / heb unix-socket /
//! hebweb ws）都把请求表达成 `CoreRequest` 交给同一 dispatch，agent-core 业务逻辑只写一遍。
//!
//! - **transport 解耦**：`CoreRequest` / [`CoreResponse`] 全程 serde，可走 in-process 免
//!   序列化直传，也可走 unix-socket / ws 的 JSON-RPC 信封（步骤③）。
//! - **对话主链路**：`StartRun` / `Submit` / `Subscribe` 经 [`agent_core::session_hub`] 的
//!   「单写者 + 多观察者」broadcast 落地（§7.8.5），步骤③合并 hebcore 进程后跨进程共享。
//! - **事件推送** = `CoreNotification` = [`protocol::WireEvent`]（§3.1.1），不另起一份。

use std::path::PathBuf;

use agent_core::core_client::{CoreClient, CoreError, SubagentScope};
use agent_core::permissions::RuleEffect;
use agent_core::storage::skills::ImportScope;
use agent_core::tools::skill::SkillSource;
use protocol::{Op, PermissionScope, RunId};
use serde::{Deserialize, Serialize};

/// 事件推送 = WireEvent（架构 §3.1.1）。surface 订阅一个 run 后逐条收。
pub type CoreNotification = protocol::WireEvent;

/// 唯一 command 入口的请求全集（架构 §7.1）。每个 variant 对应 [`CoreClient`] 的一个方法
/// （借用参数改 owned 以便跨进程序列化）+ 对话主链路。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum CoreRequest {
    // ── 对话主链路（架构 §7.8.5）──────────────────────────────────────────
    /// 投递一个控制 Op（Approve / AnswerQuestion / Interrupt / SwitchRunMode）。
    Submit(Op),
    /// 订阅一个 run 的事件流（多观察者）。
    Subscribe(RunId),

    // ── 供应商 ──────────────────────────────────────────────────────────
    ListProviders,
    GetProvider { id: String },
    SaveProvider { provider: model_gateway::config::Provider },
    SaveProviders { file: model_gateway::config::ProvidersFile },
    ListProviderPresets,
    TestProvider { provider: model_gateway::config::Provider, model: String },
    FetchProviderModels { provider: model_gateway::config::Provider },

    // ── 对话历史 ────────────────────────────────────────────────────────
    ListSessions,
    LoadSession { session_id: String },
    DeleteSession { session_id: String },
    RenameSession { session_id: String, title: String },
    SearchSessions { query: String, case_sensitive: bool, regex: bool },

    // ── Projects ────────────────────────────────────────────────────────
    ListProjects,
    SaveProject { input: agent_core::storage::projects::WorkspaceProjectInput },
    DeleteProject { project_id: String },

    // ── 项目设置 ────────────────────────────────────────────────────────
    GetSettings,
    SaveSettings { settings: agent_core::storage::settings::Settings },

    // ── 权限规则（架构 §4.6 / §6.1.2）──────────────────────────────────
    ListPermissions { scope: PermissionScope, session_id: Option<String>, workdir: Option<PathBuf>, effect: RuleEffect },
    AddPermission { scope: PermissionScope, session_id: Option<String>, workdir: Option<PathBuf>, effect: RuleEffect, pattern: String },
    RemovePermission { scope: PermissionScope, session_id: Option<String>, workdir: Option<PathBuf>, effect: RuleEffect, pattern: String },
    ClearPermissions { scope: PermissionScope, session_id: Option<String>, workdir: Option<PathBuf> },
    ListPermissionPaths { scope: PermissionScope, workdir: Option<PathBuf> },
    AddPermissionPath { scope: PermissionScope, workdir: Option<PathBuf>, path: PathBuf },
    RemovePermissionPath { scope: PermissionScope, workdir: Option<PathBuf>, path: PathBuf },

    // ── Prompt（用户 persona）──────────────────────────────────────────
    ListPrompts,
    UpsertPrompt { prompt: agent_core::storage::prompts::Prompt },
    DeletePrompt { id: String },
    SetDefaultPrompt { id: Option<String> },

    // ── Skills ──────────────────────────────────────────────────────────
    ListSkills { workdir: PathBuf },
    ListClaudeSkills,
    ImportClaudeSkills { scope: ImportScope, workdir: Option<PathBuf>, names: Option<Vec<String>>, overwrite: bool },
    ScanSkillDir { src_dir: PathBuf },
    ScanSkillGithub { repo_url: String, subpath: Option<String> },
    ImportSkillsFromDir { scope: ImportScope, workdir: Option<PathBuf>, src_dir: PathBuf, selected_paths: Option<Vec<String>>, overwrite: bool },
    ImportSkillsFromGithub { scope: ImportScope, workdir: Option<PathBuf>, repo_url: String, subpath: Option<String>, selected_paths: Option<Vec<String>>, overwrite: bool },
    SetSkillEnabled { name: String, enabled: bool },
    DeleteSkill { source: SkillSource, workdir: Option<PathBuf>, name: String },
    ListSkillCollections,
    DeleteSkillCollection { id: String },

    // ── Subagents（架构 §4.4.11.5）─────────────────────────────────────
    ListSubagents { workdir: Option<PathBuf> },
    GetSubagent { name: String },
    SaveSubagent { name: String, content: String },
    DeleteSubagent { name: String, workdir: Option<PathBuf> },
    SetSubagentEnabled { name: String, scope: SubagentScope, enabled: bool },
    LoadSubagentRun { parent_session_id: String, child_session_id: String },

    // ── 工具菜单 ────────────────────────────────────────────────────────
    ListTools,

    // ── MCP ─────────────────────────────────────────────────────────────
    GetMcpConfig,
    SaveMcpConfig { config: agent_core::mcp::config::McpConfig },
    DiscoverMcpTools,

    // ── Plugins（§6.1.4）───────────────────────────────────────────────
    PluginMarketplaceAdd { source: String },
    PluginMarketplaceList,
    PluginMarketplaceListPlugins { name: String },
    PluginMarketplaceRemove { name: String },
    PluginInstall { name: String, marketplace: Option<String> },
    PluginUninstall { name: String },
    PluginList,

    // ── Hooks（§4.8）───────────────────────────────────────────────────
    GetHooksRaw,
    SaveHooksRaw { raw: String },
}

/// 唯一 command 出口的响应全集。每个 variant 对应 [`CoreRequest`] 的一个方法返回类型。
/// 无返回值的方法 → [`CoreResponse::Unit`]；dispatch 内部错误 → [`CoreResponse::Error`]。
///
/// 只 `Serialize`：dispatch 在 core 侧产出并序列化发出；客户端（跨进程时）按需把 JSON
/// 解析成自己的强类型或 `Value`。部分返回类型（如含 `&'static` 预设的 `ProviderPreset`）
/// 本就不可反序列化，故响应方向不强求 `Deserialize` / `Clone`。
#[derive(Debug, Serialize)]
#[serde(tag = "ok_type", content = "data", rename_all = "snake_case")]
pub enum CoreResponse {
    /// 无返回值方法的成功响应（`Result<(), _>` / 控制 Op）。
    Unit,
    /// dispatch 执行出错（CoreError 文本化）。
    Error(String),

    // ── 对话主链路 ──────────────────────────────────────────────────────
    Submit(protocol::SubmissionId),

    // ── 供应商 ──────────────────────────────────────────────────────────
    ListProviders(model_gateway::config::ProvidersFile),
    GetProvider(model_gateway::config::Provider),
    SaveProvider(model_gateway::config::Provider),
    ListProviderPresets(Vec<model_gateway::config::ProviderPreset>),
    TestProvider(model_gateway::health::ProviderModelTestResult),
    FetchProviderModels(Vec<model_gateway::discovery::FetchedModel>),

    // ── 对话历史 ────────────────────────────────────────────────────────
    ListSessions(Vec<agent_core::storage::sessions::SessionMeta>),
    LoadSession(agent_core::storage::sessions::Session),
    RenameSession(agent_core::storage::sessions::Session),
    SearchSessions(Vec<agent_core::storage::sessions::SearchHit>),

    // ── Projects ────────────────────────────────────────────────────────
    ListProjects(Vec<agent_core::storage::projects::WorkspaceProject>),
    SaveProject(agent_core::storage::projects::WorkspaceProject),

    // ── 项目设置 ────────────────────────────────────────────────────────
    GetSettings(agent_core::storage::settings::Settings),

    // ── 权限规则 ────────────────────────────────────────────────────────
    ListPermissions(Vec<String>),
    RemovePermission(bool),
    ListPermissionPaths(Vec<PathBuf>),
    RemovePermissionPath(bool),

    // ── Prompt ──────────────────────────────────────────────────────────
    ListPrompts(agent_core::storage::prompts::PromptsFile),
    UpsertPrompt(agent_core::storage::prompts::Prompt),
    SetDefaultPrompt(agent_core::storage::prompts::PromptsFile),

    // ── Skills ──────────────────────────────────────────────────────────
    ListSkills(Vec<agent_core::tools::skill::Skill>),
    ListClaudeSkills(Vec<String>),
    ImportedSkills(Vec<agent_core::storage::skills::ImportedSkill>),
    ScannedSkills(Vec<agent_core::storage::skills::ScannedSkill>),
    DeleteSkill(bool),
    ListSkillCollections(Vec<agent_core::storage::skill_collections::SkillCollection>),
    DeleteSkillCollection(Vec<String>),

    // ── Subagents ───────────────────────────────────────────────────────
    ListSubagents(Vec<agent_core::storage::subagents::SubagentDefinition>),
    GetSubagent(agent_core::storage::subagents::SubagentDefinition),
    LoadSubagentRun(agent_core::storage::sessions::Session),

    // ── 工具菜单 ────────────────────────────────────────────────────────
    ListTools(Vec<agent_core::tools::ToolInfo>),

    // ── MCP ─────────────────────────────────────────────────────────────
    GetMcpConfig(agent_core::mcp::config::McpConfig),
    DiscoverMcpTools(Vec<agent_core::tools::McpToolReport>),

    // ── Plugins ─────────────────────────────────────────────────────────
    PluginMarketplaceAdd(String),
    PluginMarketplaceList(Vec<(String, String)>),
    PluginMarketplaceListPlugins(Vec<agent_core::storage::plugins::CatalogEntry>),
    PluginInstall(agent_core::storage::plugins::PluginListItem),
    PluginList(Vec<agent_core::storage::plugins::PluginListItem>),

    // ── Hooks ───────────────────────────────────────────────────────────
    GetHooksRaw(String),
}

impl CoreResponse {
    /// `Result<T, CoreError>` → 成功走 `ok(value)`，失败统一成 [`CoreResponse::Error`]。
    fn from_result<T>(r: Result<T, CoreError>, ok: impl FnOnce(T) -> CoreResponse) -> CoreResponse {
        match r {
            Ok(v) => ok(v),
            Err(e) => CoreResponse::Error(e.to_string()),
        }
    }

    /// `Result<(), CoreError>` → 成功 [`Unit`]，失败 [`Error`]。
    fn from_unit(r: Result<(), CoreError>) -> CoreResponse {
        match r {
            Ok(()) => CoreResponse::Unit,
            Err(e) => CoreResponse::Error(e.to_string()),
        }
    }

    /// surface 把强类型响应转成前端要的 JSON：成功 variant 提取内层 data 序列化成
    /// [`serde_json::Value`]（[`Unit`] → `null`），[`Error`] 转成 `Err(String)`。
    /// in-process surface（hebweb / desktop）拆 dispatch 结果时用这一个出口，无需 match 全部
    /// variant——序列化标签 `ok_type` 已经无歧义标识了类型。
    ///
    /// [`Unit`]: CoreResponse::Unit
    /// [`Error`]: CoreResponse::Error
    pub fn into_json(self) -> Result<serde_json::Value, String> {
        // serde 把每个 variant 序列化成 `{"ok_type": "...", "data": <inner>}`；
        // Unit 无 data 字段、Error 的 data 是错误文本。统一提取 data（Unit → null）。
        if let CoreResponse::Error(msg) = &self {
            return Err(msg.clone());
        }
        let mut v = serde_json::to_value(&self).map_err(|e| e.to_string())?;
        match v.get_mut("data") {
            Some(data) => Ok(data.take()),
            None => Ok(serde_json::Value::Null),
        }
    }
}

/// 唯一 command 入口（架构 §7.1）：把一个 [`CoreRequest`] 派发到 [`CoreClient`] 的对应能力，
/// 包成强类型 [`CoreResponse`]。同步 API 纯转发；对话主链路（Submit/Subscribe）走 hub。
///
/// `async` 是因为少数能力（test_provider / fetch_provider_models / discover_mcp_tools）异步。
pub async fn dispatch(req: CoreRequest, core: &dyn CoreClient) -> CoreResponse {
    use CoreRequest as Q;
    use CoreResponse as R;

    match req {
        // ── 对话主链路 ──────────────────────────────────────────────────
        Q::Submit(op) => R::from_result(core.submit(op), R::Submit),
        Q::Subscribe(run_id) => R::from_unit(core.subscribe(&run_id)),

        // ── 供应商 ──────────────────────────────────────────────────────
        Q::ListProviders => R::from_result(core.list_providers(), R::ListProviders),
        Q::GetProvider { id } => R::from_result(core.get_provider(&id), R::GetProvider),
        Q::SaveProvider { provider } => R::from_result(core.save_provider(provider), R::SaveProvider),
        Q::SaveProviders { file } => R::from_unit(core.save_providers(file)),
        Q::ListProviderPresets => R::ListProviderPresets(core.list_provider_presets()),
        Q::TestProvider { provider, model } => {
            R::from_result(core.test_provider(provider, model).await, R::TestProvider)
        }
        Q::FetchProviderModels { provider } => {
            R::from_result(core.fetch_provider_models(provider).await, R::FetchProviderModels)
        }

        // ── 对话历史 ────────────────────────────────────────────────────
        Q::ListSessions => R::from_result(core.list_sessions(), R::ListSessions),
        Q::LoadSession { session_id } => R::from_result(core.load_session(&session_id), R::LoadSession),
        Q::DeleteSession { session_id } => R::from_unit(core.delete_session(&session_id)),
        Q::RenameSession { session_id, title } => {
            R::from_result(core.rename_session(&session_id, title), R::RenameSession)
        }
        Q::SearchSessions { query, case_sensitive, regex } => {
            R::from_result(core.search_sessions(&query, case_sensitive, regex), R::SearchSessions)
        }

        // ── Projects ────────────────────────────────────────────────────
        Q::ListProjects => R::from_result(core.list_projects(), R::ListProjects),
        Q::SaveProject { input } => R::from_result(core.save_project(input), R::SaveProject),
        Q::DeleteProject { project_id } => R::from_unit(core.delete_project(&project_id)),

        // ── 项目设置 ────────────────────────────────────────────────────
        Q::GetSettings => R::GetSettings(core.get_settings()),
        Q::SaveSettings { settings } => R::from_unit(core.save_settings(settings)),

        // ── 权限规则 ────────────────────────────────────────────────────
        Q::ListPermissions { scope, session_id, workdir, effect } => R::ListPermissions(
            core.list_permissions(scope, session_id.as_deref(), workdir.as_deref(), effect),
        ),
        Q::AddPermission { scope, session_id, workdir, effect, pattern } => R::from_unit(
            core.add_permission(scope, session_id.as_deref(), workdir.as_deref(), effect, pattern),
        ),
        Q::RemovePermission { scope, session_id, workdir, effect, pattern } => R::from_result(
            core.remove_permission(scope, session_id.as_deref(), workdir.as_deref(), effect, &pattern),
            R::RemovePermission,
        ),
        Q::ClearPermissions { scope, session_id, workdir } => R::from_unit(
            core.clear_permissions(scope, session_id.as_deref(), workdir.as_deref()),
        ),
        Q::ListPermissionPaths { scope, workdir } => {
            R::ListPermissionPaths(core.list_permission_paths(scope, workdir.as_deref()))
        }
        Q::AddPermissionPath { scope, workdir, path } => {
            R::from_unit(core.add_permission_path(scope, workdir.as_deref(), path))
        }
        Q::RemovePermissionPath { scope, workdir, path } => R::from_result(
            core.remove_permission_path(scope, workdir.as_deref(), &path),
            R::RemovePermissionPath,
        ),

        // ── Prompt ──────────────────────────────────────────────────────
        Q::ListPrompts => R::from_result(core.list_prompts(), R::ListPrompts),
        Q::UpsertPrompt { prompt } => R::from_result(core.upsert_prompt(prompt), R::UpsertPrompt),
        Q::DeletePrompt { id } => R::from_unit(core.delete_prompt(&id)),
        Q::SetDefaultPrompt { id } => R::from_result(core.set_default_prompt(id), R::SetDefaultPrompt),

        // ── Skills ──────────────────────────────────────────────────────
        Q::ListSkills { workdir } => R::ListSkills(core.list_skills(&workdir)),
        Q::ListClaudeSkills => R::ListClaudeSkills(core.list_claude_skills()),
        Q::ImportClaudeSkills { scope, workdir, names, overwrite } => R::from_result(
            core.import_claude_skills(scope, workdir.as_deref(), names.as_deref(), overwrite),
            R::ImportedSkills,
        ),
        Q::ScanSkillDir { src_dir } => R::from_result(core.scan_skill_dir(&src_dir), R::ScannedSkills),
        Q::ScanSkillGithub { repo_url, subpath } => {
            R::from_result(core.scan_skill_github(&repo_url, subpath.as_deref()), R::ScannedSkills)
        }
        Q::ImportSkillsFromDir { scope, workdir, src_dir, selected_paths, overwrite } => R::from_result(
            core.import_skills_from_dir(scope, workdir.as_deref(), &src_dir, selected_paths.as_deref(), overwrite),
            R::ImportedSkills,
        ),
        Q::ImportSkillsFromGithub { scope, workdir, repo_url, subpath, selected_paths, overwrite } => {
            R::from_result(
                core.import_skills_from_github(scope, workdir.as_deref(), &repo_url, subpath.as_deref(), selected_paths.as_deref(), overwrite),
                R::ImportedSkills,
            )
        }
        Q::SetSkillEnabled { name, enabled } => R::from_unit(core.set_skill_enabled(&name, enabled)),
        Q::DeleteSkill { source, workdir, name } => {
            R::from_result(core.delete_skill(source, workdir.as_deref(), &name), R::DeleteSkill)
        }
        Q::ListSkillCollections => R::ListSkillCollections(core.list_skill_collections()),
        Q::DeleteSkillCollection { id } => {
            R::from_result(core.delete_skill_collection(&id), R::DeleteSkillCollection)
        }

        // ── Subagents ───────────────────────────────────────────────────
        Q::ListSubagents { workdir } => R::ListSubagents(core.list_subagents(workdir.as_deref())),
        Q::GetSubagent { name } => R::from_result(core.get_subagent(&name), R::GetSubagent),
        Q::SaveSubagent { name, content } => R::from_unit(core.save_subagent(&name, &content)),
        Q::DeleteSubagent { name, workdir } => {
            R::from_unit(core.delete_subagent(&name, workdir.as_deref()))
        }
        Q::SetSubagentEnabled { name, scope, enabled } => {
            R::from_unit(core.set_subagent_enabled(&name, scope, enabled))
        }
        Q::LoadSubagentRun { parent_session_id, child_session_id } => R::from_result(
            core.load_subagent_run(&parent_session_id, &child_session_id),
            R::LoadSubagentRun,
        ),

        // ── 工具菜单 ────────────────────────────────────────────────────
        Q::ListTools => R::ListTools(core.list_tools()),

        // ── MCP ─────────────────────────────────────────────────────────
        Q::GetMcpConfig => R::GetMcpConfig(core.get_mcp_config()),
        Q::SaveMcpConfig { config } => R::from_unit(core.save_mcp_config(config)),
        Q::DiscoverMcpTools => R::DiscoverMcpTools(core.discover_mcp_tools().await),

        // ── Plugins ─────────────────────────────────────────────────────
        Q::PluginMarketplaceAdd { source } => {
            R::from_result(core.plugin_marketplace_add(&source), R::PluginMarketplaceAdd)
        }
        Q::PluginMarketplaceList => R::PluginMarketplaceList(core.plugin_marketplace_list()),
        Q::PluginMarketplaceListPlugins { name } => R::from_result(
            core.plugin_marketplace_list_plugins(&name),
            R::PluginMarketplaceListPlugins,
        ),
        Q::PluginMarketplaceRemove { name } => R::from_unit(core.plugin_marketplace_remove(&name)),
        Q::PluginInstall { name, marketplace } => {
            R::from_result(core.plugin_install(&name, marketplace.as_deref()), R::PluginInstall)
        }
        Q::PluginUninstall { name } => R::from_unit(core.plugin_uninstall(&name)),
        Q::PluginList => R::PluginList(core.plugin_list()),

        // ── Hooks ───────────────────────────────────────────────────────
        Q::GetHooksRaw => R::GetHooksRaw(core.get_hooks_raw()),
        Q::SaveHooksRaw { raw } => R::from_unit(core.save_hooks_raw(&raw)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CoreRequest 的 serde round-trip：跨进程 transport 要把请求序列化成 JSON 再解析回来，
    /// 必须无损。抽几个有代表性的 variant（无参 / 单参 / 多参 / Option / 枚举参数）。
    #[test]
    fn core_request_json_round_trip() {
        let cases = vec![
            CoreRequest::ListProviders,
            CoreRequest::GetProvider { id: "p1".into() },
            CoreRequest::LoadSession { session_id: "s1".into() },
            CoreRequest::SearchSessions {
                query: "hi".into(),
                case_sensitive: true,
                regex: false,
            },
            CoreRequest::ListPermissions {
                scope: PermissionScope::Project,
                session_id: Some("s1".into()),
                workdir: Some(PathBuf::from("/tmp/x")),
                effect: RuleEffect::Allow,
            },
            CoreRequest::SetSkillEnabled {
                name: "foo".into(),
                enabled: true,
            },
        ];
        for req in cases {
            let json = serde_json::to_string(&req).expect("序列化");
            let back: CoreRequest = serde_json::from_str(&json).expect("反序列化");
            // 再序列化一次比对字符串，证明 round-trip 稳定。
            let json2 = serde_json::to_string(&back).expect("再序列化");
            assert_eq!(json, json2, "round-trip 不稳定: {json}");
        }
    }

    /// CoreRequest 用 JSON-RPC 友好的 method/params 信封。验证 tag 形态符合预期。
    #[test]
    fn core_request_uses_method_params_envelope() {
        let req = CoreRequest::GetProvider { id: "abc".into() };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["method"], "get_provider");
        assert_eq!(v["params"]["id"], "abc");
    }

    /// CoreResponse 用 ok_type/data 信封。
    #[test]
    fn core_response_uses_ok_type_envelope() {
        let resp = CoreResponse::GetHooksRaw("{}".into());
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["ok_type"], "get_hooks_raw");
        assert_eq!(v["data"], "{}");
    }
}
