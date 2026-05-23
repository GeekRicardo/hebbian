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
    /// 列出全部 skill collection（架构 §6.1.3）。仅 Global 来源——同时给 list_skills 路径
    /// 用来 join skill→collection_id。
    fn list_skill_collections(&self) -> Vec<crate::storage::skill_collections::SkillCollection>;
    /// 删除一个 collection；同时把该 collection 里的 skill 目录从
    /// `~/.hebbian/skills/<name>/` 物理删除。返回被删除的 skill 名列表。
    fn delete_skill_collection(&self, id: &str) -> Result<Vec<String>, CoreError>;

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
        // 走带 partial 恢复的路径：surface 加载会话历史时，先把上次中断残留的 partial
        // 折叠成 Assistant + Interrupted marker 落进 jsonl，再返回最终视图。
        sessions_store::load_with_partial_recovery(&self.data_dir, session_id)
            .map_err(CoreError::from)
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
        // 架构 §6.1.3：给 Global skill 附上所属 collection id。两类来源：
        //   1. sidecar 记录（一次 import 多个 skill 时显式 id=uuid）
        //   2. 虚拟集合（用户手放 / 无 sidecar 的孤儿 skill，id=`local:<name>`）
        // 二者互斥——sidecar 命中优先，未命中走虚拟。
        // 这样 SkillsPane UI 没有"未分组"段，每个 skill 都属于某个集合（要么真实
        // 来源、要么 self-collection）。
        let collections = crate::storage::skill_collections::load(&self.data_dir).collections;
        let mut index: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::with_capacity(collections.len() * 4);
        for c in &collections {
            for skill_name in &c.skills {
                index.insert(skill_name.as_str(), c.id.as_str());
            }
        }
        for s in skills.iter_mut() {
            if matches!(s.source, crate::tools::skill::SkillSource::Global) {
                s.collection_id = match index.get(s.name.as_str()) {
                    Some(id) => Some((*id).to_string()),
                    None => Some(crate::storage::skill_collections::synthetic_local_id(&s.name)),
                };
            }
        }
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

    fn list_skill_collections(&self) -> Vec<crate::storage::skill_collections::SkillCollection> {
        use crate::storage::skill_collections::{
            synthetic_local_id, CollectionSource, SkillCollection,
        };
        // sidecar 显式记录 + 为每个孤儿 Global skill 合成一条虚拟 collection。
        // 虚拟 collection 不落盘——只在运行时给 UI 用，每个 skill 自成一组
        // （label = skill 目录名 / 1 个 skill）。
        let mut out = crate::storage::skill_collections::load(&self.data_dir).collections;
        let covered: std::collections::HashSet<String> = out
            .iter()
            .flat_map(|c| c.skills.iter().cloned())
            .collect();

        let skills_root = self.data_dir.join("skills");
        let Ok(entries) = std::fs::read_dir(&skills_root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join("SKILL.md").exists() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            if covered.contains(&name) {
                continue;
            }
            out.push(SkillCollection {
                id: synthetic_local_id(&name),
                label: name.clone(),
                source: CollectionSource::Local { path: path.clone() },
                // 用目录 mtime 当 imported_at，给前端排序用——拿不到就给空串
                imported_at: std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
                    .unwrap_or_default(),
                skills: vec![name],
            });
        }
        out
    }

    fn delete_skill_collection(&self, id: &str) -> Result<Vec<String>, CoreError> {
        // 虚拟 collection 不在 sidecar 里，直接按 id 后缀解析 skill 名删目录。
        if let Some(skill_name) =
            crate::storage::skill_collections::skill_name_from_local_id(id)
        {
            let dir = self.data_dir.join("skills").join(skill_name);
            if dir.exists() {
                std::fs::remove_dir_all(&dir)
                    .map_err(|e| CoreError::from(common::AppError::from(e)))?;
                return Ok(vec![skill_name.to_string()]);
            }
            return Ok(Vec::new());
        }

        let removed = crate::storage::skill_collections::remove(&self.data_dir, id)
            .map_err(CoreError::from)?;
        let Some(c) = removed else {
            return Ok(Vec::new());
        };
        // 把 collection 里的 skill 目录从 `~/.hebbian/skills/<name>/` 物理删除。
        // 个别 skill 目录可能因为用户手动改名 / 已删除而不存在——graceful skip，
        // 整体不报错；返回值里只包含**实际删除成功**的 skill 名。
        let mut deleted = Vec::new();
        for name in &c.skills {
            let dir = self.data_dir.join("skills").join(name);
            if dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    tracing::warn!(
                        error = %e,
                        skill = %name,
                        "卸载 collection 时删除 skill 目录失败，跳过"
                    );
                    continue;
                }
                deleted.push(name.clone());
            }
        }
        Ok(deleted)
    }

    fn list_tools(&self) -> Vec<ToolInfo> {
        tools::tool_manifest()
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::skill_collections::{
        self, CollectionSource, SkillCollection, SkillCollectionsFile,
    };

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("hebbian-core-client-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_skill(skills_root: &Path, name: &str, body: &str) {
        let d = skills_root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), body).unwrap();
    }

    fn make_client(data_dir: PathBuf) -> LocalCoreClient {
        LocalCoreClient::new(None, data_dir, None)
    }

    /// 回归 2026-05-23：用户在 `~/.hebbian/skills/karpathy/SKILL.md` 手放的
    /// skill 没经过 import，sidecar 没记录——`list_skill_collections` 应当
    /// 为它合成一条 Local collection，前端 UI 才能把它独立分组而不归到"未分组"。
    #[test]
    fn list_skill_collections_synthesizes_local_for_orphan_skills() {
        let data_dir = tmp("synth-local");
        let skills_root = data_dir.join("skills");
        write_skill(&skills_root, "karpathy", "# karpathy");
        write_skill(&skills_root, "hallmark", "# hallmark");

        let client = make_client(data_dir.clone());
        let mut collections = client.list_skill_collections();
        collections.sort_by(|a, b| a.label.cmp(&b.label));

        assert_eq!(collections.len(), 2);
        assert_eq!(collections[0].label, "hallmark");
        assert_eq!(collections[0].id, "local:hallmark");
        assert!(matches!(collections[0].source, CollectionSource::Local { .. }));
        assert_eq!(collections[0].skills, vec!["hallmark".to_string()]);

        assert_eq!(collections[1].label, "karpathy");
        assert_eq!(collections[1].id, "local:karpathy");
    }

    /// sidecar collection 与孤儿 skill 同时存在时，前者优先（其内成员不被
    /// 重复合成 Local），其他孤儿仍各自合成。
    #[test]
    fn list_skill_collections_mixes_sidecar_and_local() {
        let data_dir = tmp("mix");
        let skills_root = data_dir.join("skills");
        // sidecar 集合「superpowers」覆盖两个 skill
        write_skill(&skills_root, "brainstorming", "# b");
        write_skill(&skills_root, "writing-skills", "# w");
        // 一个孤儿 skill
        write_skill(&skills_root, "karpathy", "# k");

        skill_collections::save(
            &data_dir,
            &SkillCollectionsFile {
                collections: vec![SkillCollection {
                    id: "fixed-id".into(),
                    label: "superpowers".into(),
                    source: CollectionSource::Github {
                        repo_url: "https://github.com/obra/superpowers".into(),
                        subpath: None,
                    },
                    imported_at: "2026-05-23T00:00:00Z".into(),
                    skills: vec!["brainstorming".into(), "writing-skills".into()],
                }],
            },
        )
        .unwrap();

        let client = make_client(data_dir.clone());
        let collections = client.list_skill_collections();
        assert_eq!(collections.len(), 2);

        // sidecar 集合保持原样
        let sp = collections.iter().find(|c| c.label == "superpowers").unwrap();
        assert_eq!(sp.id, "fixed-id");
        assert_eq!(sp.skills.len(), 2);

        // karpathy 自成一组
        let kp = collections.iter().find(|c| c.label == "karpathy").unwrap();
        assert_eq!(kp.id, "local:karpathy");
        assert!(matches!(kp.source, CollectionSource::Local { .. }));

        // list_skills 给 sidecar 成员填正确 id，给 karpathy 填虚拟 id
        let skills = client.list_skills(&PathBuf::from("/tmp/nowhere"));
        let brain = skills.iter().find(|s| s.name == "brainstorming").unwrap();
        assert_eq!(brain.collection_id.as_deref(), Some("fixed-id"));
        let karp = skills.iter().find(|s| s.name == "karpathy").unwrap();
        assert_eq!(karp.collection_id.as_deref(), Some("local:karpathy"));
    }

    /// `delete_skill_collection` 接受虚拟 id：按 id 后缀解析 skill 名，删该单个目录。
    /// 行为等价于 `delete_skill(Global, name)`，但走 collection API 入口保持前端 UX 一致。
    #[test]
    fn delete_skill_collection_handles_synthetic_local_id() {
        let data_dir = tmp("delete-synth");
        let skills_root = data_dir.join("skills");
        write_skill(&skills_root, "karpathy", "# k");
        write_skill(&skills_root, "hallmark", "# h");

        let client = make_client(data_dir.clone());
        let deleted = client.delete_skill_collection("local:karpathy").unwrap();
        assert_eq!(deleted, vec!["karpathy".to_string()]);
        assert!(!skills_root.join("karpathy").exists());
        // 旁边那个不受影响
        assert!(skills_root.join("hallmark").exists());

        // 二次删除 / 目录不存在——返回空，不报错
        let again = client.delete_skill_collection("local:karpathy").unwrap();
        assert!(again.is_empty());
    }
}
