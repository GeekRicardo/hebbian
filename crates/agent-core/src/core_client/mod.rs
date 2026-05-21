//! CoreClient 共享层（架构 §7）。
//!
//! 两种实现：
//! - [`LocalCoreClient`]：in-process 转发到 storage / model_gateway / permissions 模块。
//!   Desktop 用它，零序列化。
//! - [`HttpCoreClient`]：远端版（占位，未实施）。
//!
//! 双通路（架构 §3）：
//! - 对话流：`submit(Op)` / `subscribe(RunId)` 走 [`Harness`](crate::Harness) 的 actor。
//!   本期 `subscribe` 仅占位返回 `Unsupported`——Desktop 自己拿 `RunHandle` 消费事件。
//! - 同步 API：providers / sessions / project settings / permissions / prompts / skills /
//!   tool manifest。每个方法对应一个 storage / model_gateway 函数，CoreClient 仅做转发。
//!
//! 不重复定义类型：直接借用 `agent_core::storage::*` / `agent_core::permissions::*` /
//! `model_gateway::config::*` / `protocol::*`。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::permissions::{PermissionStore, RuleEffect};
use crate::storage::{
    permissions as permissions_store, projects as projects_store, prompts as prompts_store,
    sessions as sessions_store, settings as settings_store,
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

    // === 同步 API：Workspace / Projects ===

    fn list_projects(&self) -> Result<Vec<projects_store::WorkspaceProject>, CoreError>;
    fn save_project(
        &self,
        input: projects_store::WorkspaceProjectInput,
    ) -> Result<projects_store::WorkspaceProject, CoreError>;
    fn delete_project(&self, project_id: &str) -> Result<(), CoreError>;

    // === 同步 API：项目设置 ===

    fn get_settings(&self) -> settings_store::Settings;
    fn save_settings(&self, settings: settings_store::Settings) -> Result<(), CoreError>;

    // === 同步 API：权限规则（架构 §4.6 / §6.1.2）===
    // Claude Code 风格的字符串 pattern：`<Tool>(<arg>)` 或 `<Tool>`（任意调用）

    /// 列出某层（global / project / session）的 allow 或 deny pattern 列表。
    fn list_permissions(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&std::path::Path>,
        effect: RuleEffect,
    ) -> Vec<String>;
    /// 增加一条 allow / deny pattern。pattern 必须合法（`<Tool>` 或 `<Tool>(<arg>)`）。
    fn add_permission(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&std::path::Path>,
        effect: RuleEffect,
        pattern: String,
    ) -> Result<(), CoreError>;
    /// 删除一条 pattern。返回是否真删了。
    fn remove_permission(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&std::path::Path>,
        effect: RuleEffect,
        pattern: &str,
    ) -> Result<bool, CoreError>;
    /// 清空某 scope 下所有 allow / deny + paths。
    fn clear_permissions(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&std::path::Path>,
    ) -> Result<(), CoreError>;
    /// 列出 paths 白名单（permissions.json 中的 paths 段）。
    /// scope = Project + workdir 给定 → 该项目；Global → 全局。其他组合返回空。
    fn list_permission_paths(
        &self,
        scope: PermissionScope,
        workdir: Option<&std::path::Path>,
    ) -> Vec<std::path::PathBuf>;
    /// 增加一条 paths 白名单条目（架构 §6.1.2）。
    fn add_permission_path(
        &self,
        scope: PermissionScope,
        workdir: Option<&std::path::Path>,
        path: std::path::PathBuf,
    ) -> Result<(), CoreError>;
    /// 删除一条 paths 白名单条目。
    fn remove_permission_path(
        &self,
        scope: PermissionScope,
        workdir: Option<&std::path::Path>,
        path: &std::path::Path,
    ) -> Result<bool, CoreError>;

    // === 同步 API：Prompt（用户 persona）===

    fn list_prompts(&self) -> Result<prompts_store::PromptsFile, CoreError>;
    fn upsert_prompt(
        &self,
        prompt: prompts_store::Prompt,
    ) -> Result<prompts_store::Prompt, CoreError>;
    fn delete_prompt(&self, id: &str) -> Result<(), CoreError>;
    fn set_default_prompt(
        &self,
        id: Option<String>,
    ) -> Result<prompts_store::PromptsFile, CoreError>;

    // === 同步 API：Skills ===

    fn list_skills(&self, workdir: &Path) -> Vec<crate::tools::skill::Skill>;
    /// 列出 `~/.claude/skills/` 下的 skill 名（可导入候选）。
    fn list_claude_skills(&self) -> Vec<String>;
    /// 从 `~/.claude/skills/` 导入到 hebbian 全局或项目层（架构 §6.1.3）。
    fn import_claude_skills(
        &self,
        scope: crate::storage::skills::ImportScope,
        workdir: Option<&Path>,
        names: Option<&[String]>,
        overwrite: bool,
    ) -> Result<Vec<crate::storage::skills::ImportedSkill>, CoreError>;
    /// 递归扫描 `src_dir` 找所有 SKILL.md（用于"先扫描再选导入"UX）。
    fn scan_skill_dir(
        &self,
        src_dir: &Path,
    ) -> Result<Vec<crate::storage::skills::ScannedSkill>, CoreError>;
    /// 浅 clone git 仓库到临时目录后扫描，结束清理。
    fn scan_skill_github(
        &self,
        repo_url: &str,
        subpath: Option<&str>,
    ) -> Result<Vec<crate::storage::skills::ScannedSkill>, CoreError>;
    /// 从任意本地目录导入 skill；`selected_paths` = 相对 `src_dir` 的 relative_path 列表，
    /// None 表示导入所有扫描结果。
    fn import_skills_from_dir(
        &self,
        scope: crate::storage::skills::ImportScope,
        workdir: Option<&Path>,
        src_dir: &Path,
        selected_paths: Option<&[String]>,
        overwrite: bool,
    ) -> Result<Vec<crate::storage::skills::ImportedSkill>, CoreError>;
    /// 从 git 仓库 URL 下载 skill：浅 clone 到临时目录后拷贝，结束清理。
    fn import_skills_from_github(
        &self,
        scope: crate::storage::skills::ImportScope,
        workdir: Option<&Path>,
        repo_url: &str,
        subpath: Option<&str>,
        selected_paths: Option<&[String]>,
        overwrite: bool,
    ) -> Result<Vec<crate::storage::skills::ImportedSkill>, CoreError>;
    /// 启用/禁用一个 skill。禁用的 skill 不会暴露给模型（agent 看不到）。
    /// 状态持久化到 `~/.hebbian/disabled_skills.json`。
    fn set_skill_enabled(&self, name: &str, enabled: bool) -> Result<(), CoreError>;
    /// 删除一个 skill：按 source 定位目录，不动 ProjectCode（那是用户项目代码，不该被 surface 删）。
    fn delete_skill(
        &self,
        source: crate::tools::skill::SkillSource,
        workdir: Option<&Path>,
        name: &str,
    ) -> Result<bool, CoreError>;

    // === 同步 API：工具菜单（UI 用）===

    fn list_tools(&self) -> Vec<ToolInfo>;

    // === 数据目录访问 ===

    /// 暴露 `~/.hebbian/` 路径——Desktop 仍需要它构造 Workspace / model_io_dump 等。
    fn data_dir(&self) -> &Path;
}

/// In-process 实现：所有同步 API 转发到 storage / model_gateway 函数；
/// `submit` 转发到 `Harness::submit`。
///
/// 持有 [`Harness`] 和 [`PermissionStore`] 让上层在一份对象上能同时拿到
/// 「跑 run」和「列规则 / 清规则」两类能力。Desktop 当前在 `send_message` 时单独
/// 构造 Harness 走 chat 模块，CoreClient 仅承担同步 API 转发——这种场景下
/// 构造时传 `None`。远期若 Desktop 改为复用全局 Harness，则传 `Some`。
pub struct LocalCoreClient {
    data_dir: PathBuf,
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
            .ok_or(CoreError::Unsupported("submit: 未挂 Harness"))?;
        let submission = Submission::new(op);
        let id = submission.id.clone();
        harness
            .submit(submission)
            .map_err(|_| CoreError::HarnessClosed)?;
        Ok(id)
    }

    fn subscribe(&self, _run_id: &protocol::RunId) -> Result<(), CoreError> {
        // 本期不做 broadcast：Desktop 直接消费 `RunHandle`。架构 §13 留尾巴。
        Err(CoreError::Unsupported(
            "subscribe: Desktop 直接消费 RunHandle，跨进程 broadcast 待实施",
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
        sessions_store::search(&self.data_dir, query, case_sensitive, regex)
            .map_err(CoreError::from)
    }

    fn list_projects(&self) -> Result<Vec<projects_store::WorkspaceProject>, CoreError> {
        projects_store::list(&self.data_dir).map_err(CoreError::from)
    }

    fn save_project(
        &self,
        input: projects_store::WorkspaceProjectInput,
    ) -> Result<projects_store::WorkspaceProject, CoreError> {
        projects_store::save(&self.data_dir, input).map_err(CoreError::from)
    }

    fn delete_project(&self, project_id: &str) -> Result<(), CoreError> {
        projects_store::delete(&self.data_dir, project_id).map_err(CoreError::from)
    }

    fn get_settings(&self) -> settings_store::Settings {
        settings_store::load(&self.data_dir)
    }

    fn save_settings(&self, settings: settings_store::Settings) -> Result<(), CoreError> {
        settings_store::save(&self.data_dir, &settings).map_err(CoreError::from)
    }

    fn list_permissions(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&std::path::Path>,
        effect: RuleEffect,
    ) -> Vec<String> {
        let pick = |f: permissions_store::PermissionsFile| match effect {
            RuleEffect::Allow => f.allow,
            RuleEffect::Deny => f.deny,
        };
        match (&self.permission_store, scope) {
            (None, PermissionScope::Global) => permissions_store::load_global(&self.data_dir)
                .map(pick)
                .unwrap_or_default(),
            (None, PermissionScope::Project) => match workdir {
                Some(wd) => permissions_store::load_project(&self.data_dir, wd)
                    .map(pick)
                    .unwrap_or_default(),
                None => Vec::new(),
            },
            (None, _) => Vec::new(),
            (Some(store), _) => store.list(scope, session_id, workdir, effect),
        }
    }

    fn add_permission(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&std::path::Path>,
        effect: RuleEffect,
        pattern: String,
    ) -> Result<(), CoreError> {
        match &self.permission_store {
            Some(store) => store
                .add(scope, session_id, workdir, effect, pattern)
                .map_err(CoreError::from),
            None => Err(CoreError::from(common::AppError::msg(
                "PermissionStore 未启用：当前进程无法持久化权限规则",
            ))),
        }
    }

    fn remove_permission(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&std::path::Path>,
        effect: RuleEffect,
        pattern: &str,
    ) -> Result<bool, CoreError> {
        match &self.permission_store {
            Some(store) => store
                .remove(scope, session_id, workdir, effect, pattern)
                .map_err(CoreError::from),
            None => Ok(false),
        }
    }

    fn clear_permissions(
        &self,
        scope: PermissionScope,
        session_id: Option<&str>,
        workdir: Option<&std::path::Path>,
    ) -> Result<(), CoreError> {
        match &self.permission_store {
            Some(store) => store
                .clear(scope, session_id, workdir)
                .map_err(CoreError::from),
            None => Ok(()),
        }
    }

    fn list_permission_paths(
        &self,
        scope: PermissionScope,
        workdir: Option<&std::path::Path>,
    ) -> Vec<std::path::PathBuf> {
        match (&self.permission_store, scope) {
            (None, PermissionScope::Global) => permissions_store::load_global(&self.data_dir)
                .map(|f| f.paths)
                .unwrap_or_default(),
            (None, PermissionScope::Project) => match workdir {
                Some(wd) => permissions_store::load_project(&self.data_dir, wd)
                    .map(|f| f.paths)
                    .unwrap_or_default(),
                None => Vec::new(),
            },
            (None, _) => Vec::new(),
            (Some(store), _) => store.list_paths(scope, workdir),
        }
    }

    fn add_permission_path(
        &self,
        scope: PermissionScope,
        workdir: Option<&std::path::Path>,
        path: std::path::PathBuf,
    ) -> Result<(), CoreError> {
        match &self.permission_store {
            Some(store) => store
                .add_path(scope, workdir, path)
                .map_err(CoreError::from),
            None => {
                // 兜底直接写盘
                match scope {
                    PermissionScope::Global => {
                        let mut file = permissions_store::load_global(&self.data_dir)?;
                        if !file.paths.contains(&path) {
                            file.paths.push(path);
                            permissions_store::save_global(&self.data_dir, &file)?;
                        }
                        Ok(())
                    }
                    PermissionScope::Project => {
                        let wd = workdir.ok_or_else(|| {
                            common::AppError::msg("Project scope 加 paths 需要 workdir")
                        })?;
                        let mut file = permissions_store::load_project(&self.data_dir, wd)?;
                        if !file.paths.contains(&path) {
                            file.paths.push(path);
                            permissions_store::save_project(&self.data_dir, wd, &file)?;
                        }
                        Ok(())
                    }
                    _ => Err(CoreError::from(common::AppError::msg(
                        "add_permission_path 仅支持 Global / Project",
                    ))),
                }
            }
        }
    }

    fn remove_permission_path(
        &self,
        scope: PermissionScope,
        workdir: Option<&std::path::Path>,
        path: &std::path::Path,
    ) -> Result<bool, CoreError> {
        let (mut file, save_fn): (
            permissions_store::PermissionsFile,
            Box<dyn FnOnce(&permissions_store::PermissionsFile) -> common::AppResult<()>>,
        ) = match scope {
            PermissionScope::Global => {
                let data_dir = self.data_dir.clone();
                (
                    permissions_store::load_global(&self.data_dir)?,
                    Box::new(move |f| permissions_store::save_global(&data_dir, f)),
                )
            }
            PermissionScope::Project => {
                let wd = workdir
                    .ok_or_else(|| common::AppError::msg("Project scope 删 paths 需要 workdir"))?
                    .to_path_buf();
                let data_dir = self.data_dir.clone();
                (
                    permissions_store::load_project(&self.data_dir, &wd)?,
                    Box::new(move |f| permissions_store::save_project(&data_dir, &wd, f)),
                )
            }
            _ => {
                return Err(CoreError::from(common::AppError::msg(
                    "remove_permission_path 仅支持 Global / Project",
                )))
            }
        };
        let before = file.paths.len();
        file.paths.retain(|p| p != path);
        let removed = file.paths.len() != before;
        if removed {
            save_fn(&file)?;
            // PermissionStore 缓存通过 mtime 热加载会感知到
        }
        Ok(removed)
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
        let dirs = crate::tools::skill::default_skill_dirs(&self.data_dir, workdir);
        let mut skills = crate::tools::skill::load_skills(&dirs);
        crate::storage::skills::apply_disabled(&self.data_dir, &mut skills);
        skills
    }

    fn list_claude_skills(&self) -> Vec<String> {
        crate::storage::skills::list_claude_skills()
    }

    fn import_claude_skills(
        &self,
        scope: crate::storage::skills::ImportScope,
        workdir: Option<&Path>,
        names: Option<&[String]>,
        overwrite: bool,
    ) -> Result<Vec<crate::storage::skills::ImportedSkill>, CoreError> {
        crate::storage::skills::import_from_claude(
            &self.data_dir,
            scope,
            workdir,
            names,
            overwrite,
        )
        .map_err(CoreError::from)
    }

    fn scan_skill_dir(
        &self,
        src_dir: &Path,
    ) -> Result<Vec<crate::storage::skills::ScannedSkill>, CoreError> {
        crate::storage::skills::scan_skill_dir(src_dir).map_err(CoreError::from)
    }

    fn scan_skill_github(
        &self,
        repo_url: &str,
        subpath: Option<&str>,
    ) -> Result<Vec<crate::storage::skills::ScannedSkill>, CoreError> {
        crate::storage::skills::scan_skill_github(repo_url, subpath).map_err(CoreError::from)
    }

    fn import_skills_from_dir(
        &self,
        scope: crate::storage::skills::ImportScope,
        workdir: Option<&Path>,
        src_dir: &Path,
        selected_paths: Option<&[String]>,
        overwrite: bool,
    ) -> Result<Vec<crate::storage::skills::ImportedSkill>, CoreError> {
        crate::storage::skills::import_from_dir(
            &self.data_dir,
            scope,
            workdir,
            src_dir,
            selected_paths,
            overwrite,
        )
        .map_err(CoreError::from)
    }

    fn import_skills_from_github(
        &self,
        scope: crate::storage::skills::ImportScope,
        workdir: Option<&Path>,
        repo_url: &str,
        subpath: Option<&str>,
        selected_paths: Option<&[String]>,
        overwrite: bool,
    ) -> Result<Vec<crate::storage::skills::ImportedSkill>, CoreError> {
        crate::storage::skills::import_from_github(
            &self.data_dir,
            scope,
            workdir,
            repo_url,
            subpath,
            selected_paths,
            overwrite,
        )
        .map_err(CoreError::from)
    }

    fn set_skill_enabled(&self, name: &str, enabled: bool) -> Result<(), CoreError> {
        crate::storage::skills::set_skill_enabled(&self.data_dir, name, enabled)
            .map_err(CoreError::from)
    }

    fn delete_skill(
        &self,
        source: crate::tools::skill::SkillSource,
        workdir: Option<&Path>,
        name: &str,
    ) -> Result<bool, CoreError> {
        use crate::tools::skill::SkillSource;
        let dir = match source {
            SkillSource::Global => self.data_dir.join("skills").join(name),
            SkillSource::Project => {
                let wd = workdir.ok_or_else(|| {
                    CoreError::from(common::AppError::msg("Project skill 删除需要 workdir"))
                })?;
                crate::storage::projects::project_dir(&self.data_dir, wd)
                    .join("skills")
                    .join(name)
            }
            SkillSource::ProjectCode => {
                return Err(CoreError::from(common::AppError::msg(
                    "ProjectCode skill 位于用户项目代码内，请直接修改源文件，hebbian 不代为删除",
                )));
            }
        };
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| CoreError::from(common::AppError::from(e)))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn list_tools(&self) -> Vec<ToolInfo> {
        tools::tool_manifest()
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}
