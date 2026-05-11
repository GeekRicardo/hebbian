//! CoreClient 共享层（架构 §7）。
//!
//! 两种实现：
//! - [`LocalCoreClient`]：in-process 转发到 storage / model_gateway / permissions 模块。
//!   Desktop / CLI 都用它，零序列化。
//! - [`HttpCoreClient`]：远端版（占位，未实施）。
//!
//! 双通路（架构 §3）：
//! - 对话流：`submit(Op)` / `subscribe(RunId)` 走 [`Harness`](crate::Harness) 的 actor。
//!   本期 `subscribe` 仅占位返回 `Unsupported`——每个 surface 自己拿 `RunHandle` 消费事件。
//! - 同步 API：providers / sessions / project settings / permissions / prompts / skills /
//!   surface settings。每个方法对应一个 storage / model_gateway 函数，CoreClient 仅做转发。
//!
//! 不重复定义类型：直接借用 `agent_core::storage::*` / `agent_core::permissions::*` /
//! `model_gateway::config::*` / `protocol::*`。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::permissions::{PermissionRule, PermissionStore};
use crate::storage::{
    permissions as permissions_store, prompts as prompts_store, sessions as sessions_store,
    settings as settings_store, surface_settings,
};
use crate::tools::{self as tools, ToolInfo};
use crate::Harness;
use model_gateway::config::{self as providers, Provider, ProvidersFile};
use model_gateway::health::ProviderModelTestResult;
use protocol::{Op, PermissionScope, Submission, SubmissionId};

pub mod http;

pub use http::HttpCoreClient;

/// CoreClient 错误。绝大多数路径转发 storage 错误（已是 `AppError`）；
/// `Unsupported` 用于占位方法。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Storage(#[from] common::AppError),
    #[error("Harness 已关闭或不可用")]
    HarnessClosed,
    #[error("model_gateway: {0}")]
    Gateway(String),
    #[error("尚未实现：{0}")]
    Unsupported(&'static str),
}

/// Surface 共享层 trait（架构 §7.1）。
///
/// 对话流 + 同步 API。所有方法对应架构 §3.2 中的某一项；没有 § 对应项的方法不放进来。
#[async_trait]
pub trait CoreClient: Send + Sync {
    // === 对话流（双向流式）===

    /// 投递一个 `Op`。当前 actor 仅处理控制类 `Op`（`Approve` / `AnswerQuestion` /
    /// `Interrupt` / `SwitchRunMode`），`StartRun` 等仍由 surface 自行调 `Harness::spawn_run`。
    fn submit(&self, op: Op) -> Result<SubmissionId, CoreError>;

    /// 订阅一个 run 的事件流。本期占位（surface 直接消费 `RunHandle` 即可）；
    /// 等到 multi-surface 同时观察同一 run 时再做 broadcast。
    fn subscribe(&self, run_id: &protocol::RunId) -> Result<(), CoreError>;

    // === 同步 API：供应商 ===

    fn list_providers(&self) -> Result<ProvidersFile, CoreError>;
    fn get_provider(&self, id: &str) -> Result<Provider, CoreError>;
    fn save_provider(&self, provider: Provider) -> Result<Provider, CoreError>;
    fn save_providers(&self, file: ProvidersFile) -> Result<(), CoreError>;
    fn list_provider_presets(&self) -> Vec<providers::ProviderPreset>;
    async fn test_provider(
        &self,
        provider: Provider,
        model: String,
    ) -> Result<ProviderModelTestResult, CoreError>;
    async fn fetch_provider_models(
        &self,
        provider: Provider,
    ) -> Result<Vec<model_gateway::discovery::FetchedModel>, CoreError>;

    // === 同步 API：对话历史 ===

    fn list_sessions(&self) -> Result<Vec<sessions_store::SessionMeta>, CoreError>;
    fn load_session(&self, session_id: &str) -> Result<sessions_store::Session, CoreError>;
    fn delete_session(&self, session_id: &str) -> Result<(), CoreError>;
    fn rename_session(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<sessions_store::Session, CoreError>;
    fn search_sessions(
        &self,
        query: &str,
        case_sensitive: bool,
        regex: bool,
    ) -> Result<Vec<sessions_store::SearchHit>, CoreError>;

    // === 同步 API：项目设置 ===

    fn get_settings(&self) -> settings_store::Settings;
    fn save_settings(&self, settings: settings_store::Settings) -> Result<(), CoreError>;

    // === 同步 API：权限规则（架构 §4.6）===

    fn list_permission_rules(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
    ) -> Vec<PermissionRule>;
    fn remove_permission_rule(
        &self,
        session_id: Option<&str>,
        rule_id: &str,
    ) -> Result<bool, CoreError>;
    fn clear_permission_rules(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
    ) -> Result<(), CoreError>;

    // === 同步 API：Prompt（用户 persona）===

    fn list_prompts(&self) -> Result<prompts_store::PromptsFile, CoreError>;
    fn upsert_prompt(&self, prompt: prompts_store::Prompt) -> Result<prompts_store::Prompt, CoreError>;
    fn delete_prompt(&self, id: &str) -> Result<(), CoreError>;
    fn set_default_prompt(&self, id: Option<String>) -> Result<prompts_store::PromptsFile, CoreError>;

    // === 同步 API：Skills ===

    fn list_skills(&self, workdir: &Path) -> Vec<crate::tools::skill::Skill>;

    // === 同步 API：工具菜单（UI 用）===

    fn list_tools(&self) -> Vec<ToolInfo>;

    // === 同步 API：Surface 设置（架构 §7.3）===

    fn get_surface_settings(
        &self,
        surface: surface_settings::Surface,
    ) -> Result<serde_json::Value, CoreError>;
    fn save_surface_settings(
        &self,
        surface: surface_settings::Surface,
        value: serde_json::Value,
    ) -> Result<(), CoreError>;

    // === 数据目录访问 ===

    /// 暴露 `~/.hebbian/` 路径——CLI / Desktop 仍需要它构造 Workspace / model_io_dump 等。
    fn data_dir(&self) -> &Path;
}

/// In-process 实现：所有同步 API 转发到 storage / model_gateway 函数；
/// `submit` 转发到 `Harness::submit`。
///
/// 持有 [`Harness`] 和 [`PermissionStore`] 是为了让 CLI / Desktop 在一份对象上能拿到
/// 「跑 run」和「列规则 / 清规则」两类能力。
pub struct LocalCoreClient {
    data_dir: PathBuf,
    /// 可选：CLI 等长生命周期 surface 持有全局 Harness；Desktop 在每次
    /// `send_message` 时单独构造 Harness 走 chat 模块，CoreClient 仅做同步 API
    /// 转发——这种场景下传 `None`。
    harness: Option<Arc<Harness>>,
    permission_store: Option<Arc<PermissionStore>>,
}

impl LocalCoreClient {
    pub fn new(
        harness: Option<Arc<Harness>>,
        data_dir: PathBuf,
        permission_store: Option<Arc<PermissionStore>>,
    ) -> Self {
        Self {
            data_dir,
            harness,
            permission_store,
        }
    }

    pub fn harness(&self) -> Option<&Arc<Harness>> {
        self.harness.as_ref()
    }

    pub fn permission_store(&self) -> Option<&Arc<PermissionStore>> {
        self.permission_store.as_ref()
    }
}

#[async_trait]
impl CoreClient for LocalCoreClient {
    fn submit(&self, op: Op) -> Result<SubmissionId, CoreError> {
        let harness = self
            .harness
            .as_ref()
            .ok_or(CoreError::Unsupported("submit: 该 surface 未挂 Harness"))?;
        let submission = Submission::new(op);
        let id = submission.id.clone();
        harness
            .submit(submission)
            .map_err(|_| CoreError::HarnessClosed)?;
        Ok(id)
    }

    fn subscribe(&self, _run_id: &protocol::RunId) -> Result<(), CoreError> {
        // 本期不做 broadcast：surface 直接消费 `RunHandle`。架构 §13 留尾巴。
        Err(CoreError::Unsupported(
            "subscribe: surface 直接消费 RunHandle，跨进程 broadcast 待实施",
        ))
    }

    fn list_providers(&self) -> Result<ProvidersFile, CoreError> {
        providers::load(&self.data_dir).map_err(CoreError::from)
    }

    fn get_provider(&self, id: &str) -> Result<Provider, CoreError> {
        providers::get(&self.data_dir, id).map_err(CoreError::from)
    }

    fn save_provider(&self, provider: Provider) -> Result<Provider, CoreError> {
        providers::upsert(&self.data_dir, provider).map_err(CoreError::from)
    }

    fn save_providers(&self, file: ProvidersFile) -> Result<(), CoreError> {
        providers::save(&self.data_dir, &file).map_err(CoreError::from)
    }

    fn list_provider_presets(&self) -> Vec<providers::ProviderPreset> {
        providers::list_presets()
    }

    async fn test_provider(
        &self,
        provider: Provider,
        model: String,
    ) -> Result<ProviderModelTestResult, CoreError> {
        model_gateway::health::test_provider_model(provider, model)
            .await
            .map_err(|e| CoreError::Gateway(e.to_string()))
    }

    async fn fetch_provider_models(
        &self,
        provider: Provider,
    ) -> Result<Vec<model_gateway::discovery::FetchedModel>, CoreError> {
        model_gateway::discovery::fetch(&provider)
            .await
            .map_err(CoreError::from)
    }

    fn list_sessions(&self) -> Result<Vec<sessions_store::SessionMeta>, CoreError> {
        sessions_store::list(&self.data_dir).map_err(CoreError::from)
    }

    fn load_session(&self, session_id: &str) -> Result<sessions_store::Session, CoreError> {
        sessions_store::load(&self.data_dir, session_id).map_err(CoreError::from)
    }

    fn delete_session(&self, session_id: &str) -> Result<(), CoreError> {
        sessions_store::delete(&self.data_dir, session_id).map_err(CoreError::from)
    }

    fn rename_session(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<sessions_store::Session, CoreError> {
        sessions_store::rename(&self.data_dir, session_id, title).map_err(CoreError::from)
    }

    fn search_sessions(
        &self,
        query: &str,
        case_sensitive: bool,
        regex: bool,
    ) -> Result<Vec<sessions_store::SearchHit>, CoreError> {
        sessions_store::search(&self.data_dir, query, case_sensitive, regex).map_err(CoreError::from)
    }

    fn get_settings(&self) -> settings_store::Settings {
        settings_store::load(&self.data_dir)
    }

    fn save_settings(&self, settings: settings_store::Settings) -> Result<(), CoreError> {
        settings_store::save(&self.data_dir, &settings).map_err(CoreError::from)
    }

    fn list_permission_rules(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
    ) -> Vec<PermissionRule> {
        match (&self.permission_store, scope) {
            // PermissionStore 未挂时，对 Global 兜底从磁盘直读，避免 UI 列空。
            (None, PermissionScope::Global) => permissions_store::load(&self.data_dir)
                .map(|f| f.rules)
                .unwrap_or_default(),
            (None, _) => Vec::new(),
            (Some(store), _) => store.list(scope, session_id),
        }
    }

    fn remove_permission_rule(
        &self,
        session_id: Option<&str>,
        rule_id: &str,
    ) -> Result<bool, CoreError> {
        match &self.permission_store {
            Some(store) => store.remove(session_id, rule_id).map_err(CoreError::from),
            None => Ok(false),
        }
    }

    fn clear_permission_rules(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
    ) -> Result<(), CoreError> {
        match &self.permission_store {
            Some(store) => store.clear(scope, session_id).map_err(CoreError::from),
            None => Ok(()),
        }
    }

    fn list_prompts(&self) -> Result<prompts_store::PromptsFile, CoreError> {
        prompts_store::load(&self.data_dir).map_err(CoreError::from)
    }

    fn upsert_prompt(
        &self,
        prompt: prompts_store::Prompt,
    ) -> Result<prompts_store::Prompt, CoreError> {
        prompts_store::upsert(&self.data_dir, prompt).map_err(CoreError::from)
    }

    fn delete_prompt(&self, id: &str) -> Result<(), CoreError> {
        prompts_store::delete(&self.data_dir, id).map_err(CoreError::from)
    }

    fn set_default_prompt(
        &self,
        id: Option<String>,
    ) -> Result<prompts_store::PromptsFile, CoreError> {
        prompts_store::set_default(&self.data_dir, id).map_err(CoreError::from)
    }

    fn list_skills(&self, workdir: &Path) -> Vec<crate::tools::skill::Skill> {
        let dirs = crate::tools::skill::default_skill_dirs(workdir);
        crate::tools::skill::load_skills(&dirs)
    }

    fn list_tools(&self) -> Vec<ToolInfo> {
        tools::tool_manifest()
    }

    fn get_surface_settings(
        &self,
        surface: surface_settings::Surface,
    ) -> Result<serde_json::Value, CoreError> {
        surface_settings::get_surface_settings(&self.data_dir, surface).map_err(CoreError::from)
    }

    fn save_surface_settings(
        &self,
        surface: surface_settings::Surface,
        value: serde_json::Value,
    ) -> Result<(), CoreError> {
        surface_settings::save_surface_settings(&self.data_dir, surface, &value)
            .map_err(CoreError::from)
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}
