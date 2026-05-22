pub mod chat;
mod engine;
mod error;
mod force_automode;
mod hitl;
mod window_control;

pub use engine::EngineEvent;
pub use error::{AppError, AppResult};
pub use force_automode::ForceAutomodeState;
pub use hitl::HitlState;

use std::sync::Arc;

use agent_core::core_client::{CoreClient, LocalCoreClient};
use agent_core::edits;
use agent_core::edits::metadata::EditEntry;
use agent_core::permissions::PermissionStore;
use agent_core::rules::{RuleFileInfo, RuleFileState};
use agent_core::storage::{
    projects::{WorkspaceProject, WorkspaceProjectInput},
    prompts::{Prompt, PromptsFile},
    sessions::{self as sessions, Message, MessageMeta, Role, SearchHit, Session, SessionMeta},
    settings::{self as settings_store, Settings},
};
use agent_core::tools::ToolInfo;
use agent_core::workspace::Workspace;
use common::runtime as cancellation;
use model_gateway::{
    auth as oauth,
    config::{self as providers, Provider, ProviderPreset, ProvidersFile},
    discovery::FetchedModel,
    health::ProviderModelTestResult,
};
use std::path::PathBuf;
use tauri::{ipc::Channel, AppHandle, Emitter, Manager, State, WindowEvent};

fn data_dir(_app: &AppHandle) -> AppResult<std::path::PathBuf> {
    // 架构 §6.1 / 决策 D10：Desktop 多窗口/多进程共享 ~/.hebbian/。
    // `default_data_dir` 在第一次调用时会检测 Tauri bundle 老路径并自动迁移
    // （Library/Application Support/dev.ricardo.hebbian → ~/.hebbian），打 info log。
    Ok(agent_core::storage::default_data_dir())
}

// ========== Providers ==========
//
// 架构 §7.1：surface 调 CoreClient 转发到 storage / model_gateway。
// Tauri command 仅做参数透传 + AppError 兜底。

fn core(app: &AppHandle) -> AppResult<Arc<LocalCoreClient>> {
    let st: tauri::State<'_, Arc<LocalCoreClient>> = app
        .try_state::<Arc<LocalCoreClient>>()
        .ok_or_else(|| AppError::msg("LocalCoreClient 未注册"))?;
    Ok(st.inner().clone())
}

fn map_core_err(e: agent_core::core_client::CoreError) -> AppError {
    match e {
        agent_core::core_client::CoreError::Storage(err) => err,
        other => AppError::msg(other.to_string()),
    }
}

#[tauri::command]
fn get_providers(app: AppHandle) -> AppResult<ProvidersFile> {
    core(&app)?.list_providers().map_err(map_core_err)
}

#[tauri::command]
fn save_providers(app: AppHandle, file: ProvidersFile) -> AppResult<()> {
    core(&app)?.save_providers(file).map_err(map_core_err)
}

#[tauri::command]
fn upsert_provider(app: AppHandle, provider: Provider) -> AppResult<Provider> {
    core(&app)?.save_provider(provider).map_err(map_core_err)
}

#[tauri::command]
fn list_provider_presets(app: AppHandle) -> AppResult<Vec<ProviderPreset>> {
    Ok(core(&app)?.list_provider_presets())
}

#[tauri::command]
async fn fetch_provider_models(app: AppHandle, provider: Provider) -> AppResult<Vec<FetchedModel>> {
    core(&app)?
        .fetch_provider_models(provider)
        .await
        .map_err(map_core_err)
}

#[tauri::command]
async fn test_provider_model(
    app: AppHandle,
    provider: Provider,
    model: String,
) -> AppResult<ProviderModelTestResult> {
    core(&app)?
        .test_provider(provider, model)
        .await
        .map_err(map_core_err)
}

// ========== Prompts ==========

#[tauri::command]
fn list_prompts(app: AppHandle) -> AppResult<PromptsFile> {
    core(&app)?.list_prompts().map_err(map_core_err)
}

#[tauri::command]
fn upsert_prompt(app: AppHandle, prompt: Prompt) -> AppResult<Prompt> {
    core(&app)?.upsert_prompt(prompt).map_err(map_core_err)
}

#[tauri::command]
fn delete_prompt(app: AppHandle, id: String) -> AppResult<()> {
    core(&app)?.delete_prompt(&id).map_err(map_core_err)
}

#[tauri::command]
fn set_default_prompt(app: AppHandle, id: Option<String>) -> AppResult<PromptsFile> {
    core(&app)?.set_default_prompt(id).map_err(map_core_err)
}

// ========== Sessions ==========

#[tauri::command]
fn list_sessions(app: AppHandle) -> AppResult<Vec<SessionMeta>> {
    core(&app)?.list_sessions().map_err(map_core_err)
}

#[tauri::command]
fn get_session(app: AppHandle, id: String) -> AppResult<Session> {
    core(&app)?.load_session(&id).map_err(map_core_err)
}

#[tauri::command]
fn create_session(
    app: AppHandle,
    provider_id: String,
    model: String,
    system_prompt: Option<String>,
    prompt_id: Option<String>,
    project_id: Option<String>,
    workdir: Option<PathBuf>,
    allowed_paths: Option<Vec<PathBuf>>,
) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    let session = if project_id.is_some()
        || workdir.is_some()
        || allowed_paths
            .as_ref()
            .is_some_and(|paths| !paths.is_empty())
    {
        sessions::create_with_workspace(
            &dd,
            provider_id,
            model,
            system_prompt,
            prompt_id,
            "desktop".to_string(),
            project_id,
            workdir,
            allowed_paths.unwrap_or_default(),
        )?
    } else {
        sessions::create(&dd, provider_id, model, system_prompt, prompt_id)?
    };
    // 架构 §4.9.1 / §10.8：新 session 同步生成目录结构 + meta.json，
    // 为流式 partial sidecar 与中断恢复预留落点（即使主体 jsonl 仍走老路径）。
    if let Err(e) = ensure_session_layout(&dd, &session) {
        tracing::warn!(error = %e, session_id = %session.id, "初始化 session 目录失败");
    }
    Ok(session)
}

/// 读 session 的 `model_io.jsonl`：返回 `Vec<DumpEntry-as-Value>`，每条对应一次模型调用。
/// 给前端 Model I/O 调试器排查"每次给模型发了什么、模型返了什么"——比 bubble
/// 上的 `preview_session_payload` 多一份"历史真实出参"维度，因为 preview 是
/// 基于当前 session 状态实时重建，而这里是后端真发出去过的。
#[tauri::command]
fn list_session_model_io(app: AppHandle, session_id: String) -> AppResult<Vec<serde_json::Value>> {
    let dd = data_dir(&app)?;
    agent_core::storage::model_io::read_session(&dd, &session_id)
        .map_err(|e| AppError::msg(format!("读 model_io.jsonl 失败：{e}")))
}

// ========== Workspace Projects ==========

#[tauri::command]
fn list_projects(app: AppHandle) -> AppResult<Vec<WorkspaceProject>> {
    core(&app)?.list_projects().map_err(map_core_err)
}

#[tauri::command]
fn save_project(app: AppHandle, input: WorkspaceProjectInput) -> AppResult<WorkspaceProject> {
    core(&app)?.save_project(input).map_err(map_core_err)
}

#[tauri::command]
fn delete_project(app: AppHandle, id: String) -> AppResult<()> {
    core(&app)?.delete_project(&id).map_err(map_core_err)
}

#[tauri::command]
fn import_vscode_project(
    app: AppHandle,
    path: PathBuf,
    name: Option<String>,
) -> AppResult<WorkspaceProject> {
    let content = std::fs::read_to_string(&path)?;
    agent_core::storage::projects::import_vscode_workspace(
        &data_dir(&app)?,
        &content,
        name,
        Some(&path),
    )
}

#[tauri::command]
fn import_project_file(app: AppHandle, path: PathBuf) -> AppResult<WorkspaceProject> {
    let content = std::fs::read_to_string(&path)?;
    let project: WorkspaceProject = serde_json::from_str(&content)?;
    let workdir = project.workdir().cloned().unwrap_or_default();
    let allowed_paths = project.allowed_paths();
    let project_id = project.id.clone();
    let name = project.name.clone();
    let source = project.source.clone();
    core(&app)?
        .save_project(WorkspaceProjectInput {
            id: Some(project_id),
            name,
            workdir,
            allowed_paths,
            source,
        })
        .map_err(map_core_err)
}

fn ensure_session_layout(data_dir: &std::path::Path, session: &Session) -> AppResult<()> {
    use agent_core::storage::sessions_dir;
    sessions_dir::ensure_session_dirs(data_dir, &session.id)?;
    sessions_dir::save_meta(
        data_dir,
        &sessions_dir::SessionDirMeta {
            session_id: session.id.clone(),
            created_at: session.created_at,
            agent: session.prompt_id.clone().unwrap_or_default(),
            workdir: session.workdir.clone(),
            provider: session.provider_id.clone(),
            model: session.model.clone(),
            last_interrupted_at: None,
        },
    )?;
    Ok(())
}

#[tauri::command]
fn rename_session(app: AppHandle, id: String, title: String) -> AppResult<Session> {
    core(&app)?.rename_session(&id, title).map_err(map_core_err)
}

#[tauri::command]
fn delete_session(app: AppHandle, id: String) -> AppResult<()> {
    core(&app)?.delete_session(&id).map_err(map_core_err)
}

#[tauri::command]
fn fork_session(
    app: AppHandle,
    session_id: String,
    up_to_message_id: String,
) -> AppResult<Session> {
    sessions::fork(&data_dir(&app)?, &session_id, &up_to_message_id)
}

#[tauri::command]
fn truncate_after(app: AppHandle, id: String, message_id: String) -> AppResult<Session> {
    sessions::truncate_after(&data_dir(&app)?, &id, &message_id)
}

#[tauri::command]
fn truncate_inclusive(app: AppHandle, id: String, message_id: String) -> AppResult<Session> {
    sessions::truncate_inclusive(&data_dir(&app)?, &id, &message_id)
}

#[tauri::command]
fn search_sessions(
    app: AppHandle,
    query: String,
    case_sensitive: Option<bool>,
    regex: Option<bool>,
) -> AppResult<Vec<SearchHit>> {
    core(&app)?
        .search_sessions(
            &query,
            case_sensitive.unwrap_or(false),
            regex.unwrap_or(false),
        )
        .map_err(map_core_err)
}

// ========== Edits Worktree（架构 §4.13）==========

#[derive(Debug, Clone, serde::Serialize)]
struct DiffPayload {
    before_text: String,
    after_text: String,
    before_sha: String,
    after_sha: String,
    file_path: String,
    action: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RevertResult {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct EditsWorktreeStatus {
    enabled: bool,
    entry_count: usize,
}

fn build_edits_worktree(
    data_dir: &std::path::Path,
    session_id: &str,
) -> AppResult<edits::EditsWorktree> {
    let session = sessions::load(data_dir, session_id)?;
    let settings = settings_store::load(data_dir);
    let workdir = session
        .workdir
        .clone()
        .or_else(|| settings.conversation.workdir.clone())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let initial_allowed_paths = session
        .allowed_paths
        .clone()
        .unwrap_or_else(|| settings.conversation.allowed_paths.clone());
    let workspace = Workspace::with_runtime_state(
        workdir,
        initial_allowed_paths,
        session.runtime_allowed_paths,
        session.pending_runtime_allowed_paths,
    );
    Ok(edits::EditsWorktree::new(data_dir, session_id, &workspace))
}

#[tauri::command]
fn list_edits(app: AppHandle, session_id: String) -> AppResult<Vec<EditEntry>> {
    let dd = data_dir(&app)?;
    let wd = edits::metadata::worktree_dir(&dd, &session_id);
    let meta = edits::metadata::load_metadata(&wd)?;
    Ok(meta.entries)
}

#[tauri::command]
async fn diff_edit(
    app: AppHandle,
    session_id: String,
    snapshot_id: String,
) -> AppResult<DiffPayload> {
    let dd = data_dir(&app)?;
    let worktree = build_edits_worktree(&dd, &session_id)?;
    if !worktree.enabled().await {
        return Err(AppError::msg("git 不可用，无法生成 diff"));
    }
    let entries = worktree.list_entries()?;
    let entry = entries
        .into_iter()
        .find(|e| e.snapshot_id == snapshot_id)
        .ok_or_else(|| AppError::msg("找不到该快照"))?;
    let (before_text, after_text) = worktree.diff_text(&entry).await?;
    Ok(DiffPayload {
        before_text,
        after_text,
        before_sha: entry.before_sha,
        after_sha: entry.after_sha,
        file_path: entry.real_path,
        action: format!("{:?}", entry.action).to_lowercase(),
    })
}

#[tauri::command]
async fn revert_edit(
    app: AppHandle,
    session_id: String,
    snapshot_id: String,
) -> AppResult<RevertResult> {
    let dd = data_dir(&app)?;
    let worktree = build_edits_worktree(&dd, &session_id)?;
    if !worktree.enabled().await {
        return Err(AppError::msg("git 不可用，回退功能已禁用"));
    }
    let entries = worktree.list_entries()?;
    let entry = entries
        .into_iter()
        .find(|e| e.snapshot_id == snapshot_id)
        .ok_or_else(|| AppError::msg("找不到该快照"))?;
    if entry.reverted {
        return Err(AppError::msg("该快照已回退过"));
    }
    match worktree.revert(&entry).await {
        Ok(()) => {
            worktree.mark_reverted(&snapshot_id)?;
            let payload = serde_json::json!({
                "session_id": session_id,
                "snapshot_id": snapshot_id,
                "file_path": entry.real_path,
            });
            app.emit("edit-reverted", payload).ok();
            Ok(RevertResult {
                success: true,
                error: None,
            })
        }
        Err(e) => Ok(RevertResult {
            success: false,
            error: Some(e.to_string()),
        }),
    }
}

#[tauri::command]
async fn edits_worktree_status(
    app: AppHandle,
    session_id: String,
) -> AppResult<EditsWorktreeStatus> {
    let dd = data_dir(&app)?;
    let worktree = build_edits_worktree(&dd, &session_id)?;
    let enabled = worktree.enabled().await;
    let entry_count = if enabled {
        worktree.list_entries().map(|e| e.len()).unwrap_or(0)
    } else {
        0
    };
    Ok(EditsWorktreeStatus {
        enabled,
        entry_count,
    })
}

#[tauri::command]
fn update_session_config(
    app: AppHandle,
    id: String,
    provider_id: Option<String>,
    model: Option<String>,
    system_prompt: Option<String>,
    prompt_id: Option<String>,
    stream: Option<bool>,
    reasoning: Option<common::ReasoningConfig>,
    // clear_reasoning：显式重置推理配置；当 reasoning=None 且这里传 Some(true) 时清空。
    clear_reasoning: Option<bool>,
) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    let mut s = sessions::load(&dd, &id)?;
    if let Some(pid) = provider_id {
        s.provider_id = pid;
    }
    if let Some(m) = model {
        s.model = m;
    }
    if let Some(sp) = system_prompt {
        s.system_prompt = if sp.is_empty() { None } else { Some(sp) };
    }
    if let Some(pid) = prompt_id {
        s.prompt_id = if pid.is_empty() { None } else { Some(pid) };
    }
    if let Some(st) = stream {
        s.stream = st;
    }
    let prev_reasoning = s.reasoning.clone();
    if let Some(r) = reasoning {
        s.reasoning = Some(r);
    } else if clear_reasoning.unwrap_or(false) {
        s.reasoning = None;
    }
    if prev_reasoning != s.reasoning {
        // 把 reasoning 切换当作模型切换的轻量版本——往对话流里插一条 marker，
        // 让 UI 渲染分割线，从这条之后的回复里都是新参数下产生的。
        sessions::insert_reasoning_switch_marker(&dd, &id, prev_reasoning, s.reasoning.clone())?;
        // 上面 marker 写入后再 reload 一次，避免覆盖 marker。
        let mut latest = sessions::load(&dd, &id)?;
        latest.system_prompt = s.system_prompt.clone();
        latest.prompt_id = s.prompt_id.clone();
        latest.stream = s.stream;
        latest.reasoning = s.reasoning.clone();
        latest.provider_id = s.provider_id.clone();
        latest.model = s.model.clone();
        return sessions::save(&dd, latest);
    }
    sessions::save(&dd, s)
}

#[tauri::command]
fn switch_provider_model(
    app: AppHandle,
    id: String,
    new_provider_id: String,
    new_model: String,
) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    let cur = sessions::load(&dd, &id)?;
    let cur_provider = providers::get(&dd, &cur.provider_id).ok();
    let new_provider = providers::get(&dd, &new_provider_id).ok();
    let from_provider = cur_provider
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| cur.provider_id.clone());
    let to_provider = new_provider
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| new_provider_id.clone());

    if cur.provider_id == new_provider_id && cur.model == new_model {
        return Ok(cur);
    }

    // 锁定模型系列：一旦会话开始有真实对话，就不允许 DeepSeek 与其他系列互切
    // （DeepSeek web 协议的 prompt / tool_call / thinking 编码与 OpenAI/Anthropic
    // 完全不同，跨系列重放历史会让模型脑补伪角色头）。新会话（还没产生任何
    // user/assistant 消息）不受限。
    let has_real_turn = cur
        .messages
        .iter()
        .any(|m| matches!(m.role, sessions::Role::User | sessions::Role::Assistant));
    if has_real_turn {
        if let (Some(c), Some(n)) = (cur_provider.as_ref(), new_provider.as_ref()) {
            let cur_is_ds = matches!(c.kind, providers::ProviderKind::Deepseek);
            let new_is_ds = matches!(n.kind, providers::ProviderKind::Deepseek);
            if cur_is_ds != new_is_ds {
                return Err(AppError::msg(
                    "本会话已锁定模型系列：DeepSeek 与其他模型之间不可互相切换，请新建会话。",
                ));
            }
        }
    }

    let meta = MessageMeta::Switch {
        from_provider,
        from_model: cur.model.clone(),
        to_provider,
        to_model: new_model.clone(),
    };
    sessions::insert_switch_marker(&dd, &id, meta)?;

    let mut updated = sessions::load(&dd, &id)?;
    updated.provider_id = new_provider_id;
    updated.model = new_model;
    let supports = common::reasoning::anthropic_supports_thinking(&updated.model)
        || common::reasoning::openai_supports_reasoning(&updated.model);
    if supports {
        // 首次切到支持推理的模型：默认 thinking on + extra effort（用户可在 UI 改）
        if updated.reasoning.is_none() {
            updated.reasoning = Some(common::ReasoningConfig {
                enabled: Some(true),
                effort: Some(common::ReasoningEffort::Extra),
                long_context: None,
            });
        }
    } else {
        // 切到不支持的模型：丢掉旧 reasoning，避免遗留 thinking 字段被 server 拒。
        updated.reasoning = None;
    }
    sessions::save(&dd, updated)
}

#[tauri::command]
async fn preview_session_payload(
    app: AppHandle,
    session_id: String,
    upto_message_id: Option<String>,
) -> AppResult<serde_json::Value> {
    let dd = data_dir(&app)?;
    chat::build_preview_payload(&dd, &session_id, upto_message_id.as_deref()).await
}

#[tauri::command]
async fn send_message(
    app: AppHandle,
    hitl: State<'_, Arc<HitlState>>,
    permission_store: State<'_, Option<Arc<PermissionStore>>>,
    force_automode: State<'_, Arc<ForceAutomodeState>>,
    session_id: String,
    content: String,
    attachments: Vec<common::attachments::MessageAttachment>,
    stream: bool,
    enabled_tools: Vec<String>,
    request_id: String,
    on_event: Channel<EngineEvent>,
) -> AppResult<Message> {
    let runtime = cancellation::register(request_id.clone());
    let force_automode_enabled = force_automode.is_enabled(&session_id);
    let result = chat::send_and_save(
        &app,
        chat::SendArgs {
            session_id,
            user_content: content,
            attachments,
            stream,
            enabled_tools,
            cancel_flag: runtime.cancel.clone(),
            pending_inputs: Some(runtime.pending_inputs.clone()),
            consumed_pending_inputs: Some(runtime.consumed_pending_inputs.clone()),
            hitl: Some(hitl.inner().clone()),
            permission_store: permission_store.inner().clone(),
            force_automode: force_automode_enabled,
        },
        on_event,
    )
    .await;
    cancellation::unregister(&request_id);
    result
}

#[tauri::command]
fn cancel_message(request_id: String) -> bool {
    cancellation::cancel(&request_id)
}

/// 「立即发送」入口：在 streaming 中把 user message 注入到当前 run 的 pending 队列，
/// 并把它持久化到 session.json + 返回给前端立刻渲染到 chat 区域。
///
/// agent_loop 在下一次 model.request 之前会把 pending 列表 drain 出来作为新的 user
/// message 加入 transcript（不打断当前 agent loop，下一个 iteration 立刻可见）。
#[tauri::command]
fn inject_user_message(
    app: AppHandle,
    session_id: String,
    request_id: String,
    content: String,
    attachments: Vec<common::attachments::MessageAttachment>,
) -> AppResult<agent_core::storage::sessions::Message> {
    use agent_core::storage::sessions::{self, Message, Role};
    let dd = data_dir(&app)?;
    let injected = cancellation::inject_pending_input(
        &request_id,
        common::runtime::PendingUserInput {
            content: content.clone(),
            attachments: attachments.clone(),
        },
    );
    if !injected {
        return Err(AppError::msg("当前 run 已结束，无法引导插队"));
    }

    let user_msg = Message {
        id: sessions::new_id(),
        role: Role::User,
        content: content.clone(),
        attachments: attachments.clone(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: None,
    };
    let _ = sessions::load(&dd, &session_id)?;
    Ok(user_msg)
}

#[tauri::command]
async fn get_context_usage(app: AppHandle, session_id: String) -> AppResult<chat::ContextUsageDto> {
    chat::context_usage(&data_dir(&app)?, &session_id).await
}

#[tauri::command]
async fn compact_session(
    app: AppHandle,
    session_id: String,
    custom_instructions: Option<String>,
) -> AppResult<chat::ContextUsageDto> {
    let dd = data_dir(&app)?;
    chat::compact_session(&dd, &session_id, custom_instructions).await
}

/// 用户在 UI 中点击审批按钮后调用。
///
/// `decision` 取值：`"allow_once"` / `"allow_and_remember"` / `"deny"` / `"deny_with_feedback"`
/// `feedback` 仅在 `deny_with_feedback` 时使用。
/// `pattern` 仅在 `allow_and_remember` 时有意义：传命令前缀（如 `"git status"` / `"git"`）
/// 启用命令级记忆；不传则做工具名级记忆（对 Bash 等会被 hitl 黑名单兜回 AllowOnce）。
#[tauri::command]
fn approve_permission(
    hitl: State<'_, Arc<HitlState>>,
    request_id: String,
    decision: String,
    feedback: Option<String>,
    pattern: Option<String>,
    scope: Option<String>,
    extra_patterns: Option<Vec<String>>,
) -> AppResult<()> {
    let allow_scope = match scope.as_deref().unwrap_or("session") {
        "session" => protocol::PermissionScope::Session,
        "project" => protocol::PermissionScope::Project,
        "global" => protocol::PermissionScope::Global,
        "once" => protocol::PermissionScope::Once,
        other => return Err(AppError::msg(format!("未知 scope: {other}"))),
    };
    let decision = match decision.as_str() {
        "allow_once" => protocol::ApprovalDecision::AllowOnce,
        "allow_and_remember" => protocol::ApprovalDecision::AllowAndRemember {
            scope: allow_scope,
            pattern,
            extra_patterns: extra_patterns.unwrap_or_default(),
        },
        "deny" => protocol::ApprovalDecision::Deny,
        "deny_with_feedback" => protocol::ApprovalDecision::DenyWithFeedback {
            feedback: feedback.unwrap_or_default(),
        },
        other => return Err(AppError::msg(format!("未知 decision: {other}"))),
    };
    hitl.resolve_approval(&request_id, decision)
        .map_err(AppError::msg)
}

/// 用户回应一次 agent 提问（ask 工具）。
///
/// `kind` 取值：`"selected"` / `"selected_multi"` / `"custom"` / `"cancelled"`
/// - `selected` → `text` 为单个 label
/// - `selected_multi` → `labels` 为勾选的 label 列表
/// - `custom` → `text` 为用户输入
/// - `cancelled` → 字段忽略
#[tauri::command]
fn answer_question(
    hitl: State<'_, Arc<HitlState>>,
    request_id: String,
    kind: String,
    text: Option<String>,
    labels: Option<Vec<String>>,
) -> AppResult<()> {
    let answer = match kind.as_str() {
        "selected" => protocol::UserAnswer::Selected {
            label: text.unwrap_or_default(),
        },
        "selected_multi" => protocol::UserAnswer::SelectedMulti {
            labels: labels.unwrap_or_default(),
        },
        "custom" => protocol::UserAnswer::Custom {
            text: text.unwrap_or_default(),
        },
        "cancelled" => protocol::UserAnswer::Cancelled,
        other => return Err(AppError::msg(format!("未知 kind: {other}"))),
    };
    hitl.answer_question(&request_id, answer)
        .map_err(AppError::msg)
}

// ========== `//` 命令系统：force-automode ==========
//
// 架构 §4.4.4 / §8：force_automode 是 RunMode=AutoMode 下的子开关。
// 用户在 ChatInput 输入 `//force-automode [on|off|toggle]`，前端解析后调下面两个 command。
// 状态仅存内存（架构 §8.2 决策：危险开关重启回归 false）。

#[tauri::command]
fn get_force_automode(state: State<'_, Arc<ForceAutomodeState>>, session_id: String) -> bool {
    state.is_enabled(&session_id)
}

#[tauri::command]
fn set_force_automode(
    state: State<'_, Arc<ForceAutomodeState>>,
    session_id: String,
    enabled: bool,
) -> bool {
    state.set(session_id, enabled);
    enabled
}

#[tauri::command]
fn get_run_mode(app: AppHandle, session_id: String) -> AppResult<String> {
    let dd = data_dir(&app)?;
    Ok(sessions::load(&dd, &session_id)?
        .run_mode
        .as_str()
        .to_string())
}

#[tauri::command]
fn set_run_mode(app: AppHandle, session_id: String, mode: String) -> AppResult<String> {
    let parsed = agent_core::run_mode::RunMode::parse(&mode)
        .ok_or_else(|| AppError::msg(format!("未知的 RunMode：{mode}")))?;
    let dd = data_dir(&app)?;
    sessions::set_run_mode(&dd, &session_id, parsed)?;
    Ok(parsed.as_str().to_string())
}

/// 「重新生成标题」入口（前端 sidebar 右键 / chat header 按钮触发）。
/// 自动生成已下沉到 [`agent_core::session_titler`]，由 Harness::spawn_run 在首轮
/// TurnFinished 后异步触发并通过 `EngineEvent::SessionTitleChanged` 推到前端。
/// 本 invoke 命令只是手动重生成入口——无视当前 title，强制走一次。
#[tauri::command]
async fn generate_session_title(app: AppHandle, id: String) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    agent_core::session_titler::regenerate_session_title(&dd, &id).await
}

#[tauri::command]
fn list_tools(app: AppHandle) -> AppResult<Vec<ToolInfo>> {
    Ok(core(&app)?.list_tools())
}

// ========== 权限规则与路径白名单（架构 §4.6 / §6.1.2）==========

fn parse_perm_scope(s: &str) -> AppResult<protocol::PermissionScope> {
    Ok(match s {
        "session" => protocol::PermissionScope::Session,
        "project" => protocol::PermissionScope::Project,
        "global" => protocol::PermissionScope::Global,
        "once" => protocol::PermissionScope::Once,
        other => return Err(AppError::msg(format!("未知 scope: {other}"))),
    })
}

fn parse_rule_effect(s: &str) -> AppResult<agent_core::permissions::RuleEffect> {
    Ok(match s {
        "deny" => agent_core::permissions::RuleEffect::Deny,
        _ => agent_core::permissions::RuleEffect::Allow,
    })
}

#[tauri::command]
fn list_permissions(
    app: AppHandle,
    scope: String,
    effect: String,
    session_id: Option<String>,
    workdir: Option<PathBuf>,
) -> AppResult<Vec<String>> {
    let scope = parse_perm_scope(&scope)?;
    let effect = parse_rule_effect(&effect)?;
    Ok(core(&app)?.list_permissions(scope, session_id.as_deref(), workdir.as_deref(), effect))
}

#[tauri::command]
fn add_permission(
    app: AppHandle,
    scope: String,
    effect: String,
    pattern: String,
    session_id: Option<String>,
    workdir: Option<PathBuf>,
) -> AppResult<()> {
    let scope = parse_perm_scope(&scope)?;
    let effect = parse_rule_effect(&effect)?;
    core(&app)?
        .add_permission(scope, session_id.as_deref(), workdir.as_deref(), effect, pattern)
        .map_err(map_core_err)
}

#[tauri::command]
fn remove_permission(
    app: AppHandle,
    scope: String,
    effect: String,
    pattern: String,
    session_id: Option<String>,
    workdir: Option<PathBuf>,
) -> AppResult<bool> {
    let scope = parse_perm_scope(&scope)?;
    let effect = parse_rule_effect(&effect)?;
    core(&app)?
        .remove_permission(scope, session_id.as_deref(), workdir.as_deref(), effect, &pattern)
        .map_err(map_core_err)
}

#[tauri::command]
fn clear_permissions(
    app: AppHandle,
    scope: String,
    session_id: Option<String>,
    workdir: Option<PathBuf>,
) -> AppResult<()> {
    let scope = parse_perm_scope(&scope)?;
    core(&app)?
        .clear_permissions(scope, session_id.as_deref(), workdir.as_deref())
        .map_err(map_core_err)
}

#[tauri::command]
fn list_permission_paths(
    app: AppHandle,
    scope: String,
    workdir: Option<PathBuf>,
) -> AppResult<Vec<PathBuf>> {
    let scope = parse_perm_scope(&scope)?;
    Ok(core(&app)?.list_permission_paths(scope, workdir.as_deref()))
}

#[tauri::command]
fn add_permission_path(
    app: AppHandle,
    scope: String,
    path: PathBuf,
    workdir: Option<PathBuf>,
) -> AppResult<()> {
    let scope = parse_perm_scope(&scope)?;
    core(&app)?
        .add_permission_path(scope, workdir.as_deref(), path)
        .map_err(map_core_err)
}

#[tauri::command]
fn remove_permission_path(
    app: AppHandle,
    scope: String,
    path: PathBuf,
    workdir: Option<PathBuf>,
) -> AppResult<bool> {
    let scope = parse_perm_scope(&scope)?;
    core(&app)?
        .remove_permission_path(scope, workdir.as_deref(), &path)
        .map_err(map_core_err)
}

// ========== Skills（架构 §6.1.3）==========

#[tauri::command]
fn list_skills(
    app: AppHandle,
    workdir: PathBuf,
) -> AppResult<Vec<agent_core::tools::skill::Skill>> {
    Ok(core(&app)?.list_skills(&workdir))
}

#[tauri::command]
fn list_claude_skills(app: AppHandle) -> AppResult<Vec<String>> {
    Ok(core(&app)?.list_claude_skills())
}

#[tauri::command]
fn import_claude_skills(
    app: AppHandle,
    scope: String,
    workdir: Option<PathBuf>,
    names: Option<Vec<String>>,
    overwrite: Option<bool>,
) -> AppResult<Vec<agent_core::storage::skills::ImportedSkill>> {
    let scope = match scope.as_str() {
        "project" => agent_core::storage::skills::ImportScope::Project,
        _ => agent_core::storage::skills::ImportScope::Global,
    };
    core(&app)?
        .import_claude_skills(
            scope,
            workdir.as_deref(),
            names.as_deref(),
            overwrite.unwrap_or(false),
        )
        .map_err(map_core_err)
}

#[tauri::command]
fn import_skills_from_dir(
    app: AppHandle,
    scope: String,
    src_dir: PathBuf,
    workdir: Option<PathBuf>,
    selected_paths: Option<Vec<String>>,
    overwrite: Option<bool>,
) -> AppResult<Vec<agent_core::storage::skills::ImportedSkill>> {
    let scope = match scope.as_str() {
        "project" => agent_core::storage::skills::ImportScope::Project,
        _ => agent_core::storage::skills::ImportScope::Global,
    };
    core(&app)?
        .import_skills_from_dir(
            scope,
            workdir.as_deref(),
            &src_dir,
            selected_paths.as_deref(),
            overwrite.unwrap_or(true),
        )
        .map_err(map_core_err)
}

#[tauri::command]
fn import_skills_from_github(
    app: AppHandle,
    scope: String,
    repo_url: String,
    subpath: Option<String>,
    workdir: Option<PathBuf>,
    selected_paths: Option<Vec<String>>,
    overwrite: Option<bool>,
) -> AppResult<Vec<agent_core::storage::skills::ImportedSkill>> {
    let scope = match scope.as_str() {
        "project" => agent_core::storage::skills::ImportScope::Project,
        _ => agent_core::storage::skills::ImportScope::Global,
    };
    core(&app)?
        .import_skills_from_github(
            scope,
            workdir.as_deref(),
            &repo_url,
            subpath.as_deref(),
            selected_paths.as_deref(),
            overwrite.unwrap_or(true),
        )
        .map_err(map_core_err)
}

#[tauri::command]
fn scan_skill_dir(
    app: AppHandle,
    src_dir: PathBuf,
) -> AppResult<Vec<agent_core::storage::skills::ScannedSkill>> {
    core(&app)?.scan_skill_dir(&src_dir).map_err(map_core_err)
}

#[tauri::command]
fn scan_skill_github(
    app: AppHandle,
    repo_url: String,
    subpath: Option<String>,
) -> AppResult<Vec<agent_core::storage::skills::ScannedSkill>> {
    core(&app)?
        .scan_skill_github(&repo_url, subpath.as_deref())
        .map_err(map_core_err)
}

/// 读取一个 SKILL.md 的原始内容。`path` 必须直接指向 SKILL.md 文件
/// （前端调用方应传 `list_skills` 返回的 `path` 字段）。
///
/// 限制：仅允许读 `SKILL.md` 命名的文件，避免被当成任意路径读取工具。
#[tauri::command]
fn read_skill_md(path: PathBuf) -> AppResult<String> {
    let is_skill = path
        .file_name()
        .map(|n| n == std::ffi::OsStr::new("SKILL.md"))
        .unwrap_or(false);
    if !is_skill {
        return Err(AppError::msg("仅允许读取 SKILL.md 文件"));
    }
    std::fs::read_to_string(&path)
        .map_err(|e| AppError::msg(format!("读取 {} 失败：{e}", path.display())))
}

#[tauri::command]
fn set_skill_enabled(app: AppHandle, name: String, enabled: bool) -> AppResult<()> {
    core(&app)?.set_skill_enabled(&name, enabled).map_err(map_core_err)
}

#[tauri::command]
fn delete_skill(
    app: AppHandle,
    source: String,
    name: String,
    workdir: Option<PathBuf>,
) -> AppResult<bool> {
    let source = match source.as_str() {
        "project" => agent_core::tools::skill::SkillSource::Project,
        "project_code" => agent_core::tools::skill::SkillSource::ProjectCode,
        _ => agent_core::tools::skill::SkillSource::Global,
    };
    core(&app)?
        .delete_skill(source, workdir.as_deref(), &name)
        .map_err(map_core_err)
}

#[derive(Debug, Clone, serde::Serialize)]
struct BackgroundTaskInfo {
    task_id: String,
    state: String,
    command: String,
    cwd: String,
    elapsed_secs: u64,
    log_path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct SessionBackgroundReport {
    /// 当前 session 注册的所有后台 shell（含 running / exited / killed）。
    shells: Vec<BackgroundTaskInfo>,
    /// 当前 session 还在等的 cron 唤醒。
    pending_crons: Vec<agent_core::wakeup::PendingCron>,
    /// 当前 session 是否有挂起态 checkpoint（架构 §4.12.6）。surface 用来决定
    /// 是否在 BackgroundTaskPanel 渲染「挂起中」徽标。
    has_suspended_checkpoint: bool,
}

/// 强杀本 session 注册表中的某个 bg shell（包装 BackgroundShells.kill）。
/// surface 用：BackgroundTaskPanel 上的「停止」按钮。
#[tauri::command]
async fn kill_background_task(session_id: String, task_id: String) -> AppResult<String> {
    let shells = agent_core::tools::background::registry_for_session(&session_id);
    match shells.kill(&task_id).await {
        Some(state) => Ok(state.label().to_string()),
        None => Err(AppError::msg(format!(
            "未找到 task_id={task_id}（可能已被清理）"
        ))),
    }
}

/// 架构 §4.12.9：BackgroundTaskPanel 调它轮询当前 session 的后台情况。
/// session-scoped——跨 session 的 bg shell 互不可见。
#[tauri::command]
fn list_background_tasks(app: AppHandle, session_id: String) -> AppResult<SessionBackgroundReport> {
    let dd = data_dir(&app)?;
    let shells_registry = agent_core::tools::background::registry_for_session(&session_id);
    // 只暴露真后台任务（is_background=true）：用户显式 run_in_background=true
    // 或前台超时转后台的。前台正常 exit 的命令已经被 BashTool 直接 unregister，
    // 这里就算注册表里万一还残留 is_background=false 的条目（罕见 race），surface 也不要展示。
    let shells: Vec<BackgroundTaskInfo> = shells_registry
        .list()
        .into_iter()
        .filter(|s| s.is_background())
        .map(|s| BackgroundTaskInfo {
            task_id: s.task_id.clone(),
            state: s.state().label().to_string(),
            command: s.command.clone(),
            cwd: s.cwd.clone(),
            elapsed_secs: s.started_at.elapsed().as_secs(),
            log_path: s.log_path().map(|p| p.display().to_string()),
        })
        .collect();
    let pending_crons =
        agent_core::wakeup::WakeupScheduler::global().list_pending_crons(&session_id);
    let has_suspended_checkpoint = agent_core::storage::run_checkpoint::load(&dd, &session_id)
        .ok()
        .flatten()
        .is_some();
    Ok(SessionBackgroundReport {
        shells,
        pending_crons,
        has_suspended_checkpoint,
    })
}

// ========== Settings ==========

#[tauri::command]
fn get_settings(app: AppHandle) -> AppResult<Settings> {
    Ok(core(&app)?.get_settings())
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> AppResult<()> {
    core(&app)?.save_settings(settings).map_err(map_core_err)
}

/// 更新对话级设置（workdir / allowed_paths / enabled_tools / skill_dirs）。
///
/// 三态语义靠两组字段表达，避开 `Option<Option<T>>` 在 IPC 反序列化时
/// 把 `null` 直接折叠成外层 `None` 的歧义：
/// - 设值：传 `xxx` 字段，例如 `workdir = "/foo"` / `allowed_paths = ["/bar"]`
/// - 清空：传 `clearXxx = true`（前端 invoke 用 camelCase）
/// - 不动：两边都不传
///
/// `allowed_paths` 的特殊语义：
/// - 对话还没发出过 user message → 直接覆盖 `s.allowed_paths`（initial 集合可任意改）
/// - 对话已开始 → `s.allowed_paths` 锁定，**禁止删除任何已存在的路径**；新增的路径
///   追加到 `pending_runtime_allowed_paths`，下次 send_message 时通过
///   `<workspace-update>` 段告诉模型，**不会改 system prompt**，因此 prompt cache 不破。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn update_session_settings(
    app: AppHandle,
    id: String,
    workdir: Option<PathBuf>,
    clear_workdir: Option<bool>,
    allowed_paths: Option<Vec<PathBuf>>,
    clear_allowed_paths: Option<bool>,
    enabled_tools: Option<Vec<String>>,
    clear_enabled_tools: Option<bool>,
    skill_dirs: Option<Vec<PathBuf>>,
    clear_skill_dirs: Option<bool>,
    global_rules: Option<Vec<PathBuf>>,
    clear_global_rules: Option<bool>,
    rules_files: Option<Vec<RuleFileState>>,
    clear_rules_files: Option<bool>,
) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    let mut s = sessions::load(&dd, &id)?;
    if clear_workdir.unwrap_or(false) {
        s.workdir = None;
    } else if let Some(v) = workdir {
        s.workdir = Some(v);
    }
    if clear_allowed_paths.unwrap_or(false) {
        apply_allowed_paths_update(&mut s, None)?;
    } else if let Some(v) = allowed_paths {
        apply_allowed_paths_update(&mut s, Some(v))?;
    }
    if clear_enabled_tools.unwrap_or(false) {
        s.enabled_tools = None;
    } else if let Some(v) = enabled_tools {
        s.enabled_tools = Some(v);
    }
    if clear_skill_dirs.unwrap_or(false) {
        s.skill_dirs = None;
    } else if let Some(v) = skill_dirs {
        s.skill_dirs = Some(v);
    }
    if clear_global_rules.unwrap_or(false) {
        s.global_rules = None;
    } else if let Some(v) = global_rules {
        s.global_rules = Some(v);
    }
    if clear_rules_files.unwrap_or(false) {
        s.rules_files = None;
    } else if let Some(v) = rules_files {
        s.rules_files = Some(v);
    }
    sessions::save(&dd, s)
}

/// `update_session_settings` 中 `allowed_paths` 字段的处理逻辑，单独拆出来便于测试。
fn apply_allowed_paths_update(
    session: &mut Session,
    new_value: Option<Vec<PathBuf>>,
) -> AppResult<()> {
    let conversation_started = session
        .messages
        .iter()
        .any(|m| matches!(m.role, Role::User));

    if !conversation_started {
        // 还没发过消息：自由覆盖 initial。新值同时也代表"用户期望本对话起始集"，
        // runtime / pending 应该是空（无消息时不可能产生）但稳妥起见也清一遍。
        session.allowed_paths = new_value;
        session.runtime_allowed_paths.clear();
        session.pending_runtime_allowed_paths.clear();
        return Ok(());
    }

    // 对话已开始：锁定 initial。新值必须是当前所有已知路径的超集，新增项进 pending。
    let target: Vec<PathBuf> = new_value.unwrap_or_default();
    let initial: Vec<PathBuf> = session.allowed_paths.clone().unwrap_or_default();
    let announced: Vec<PathBuf> = session.runtime_allowed_paths.clone();
    let pending: Vec<PathBuf> = session.pending_runtime_allowed_paths.clone();

    for known in initial.iter().chain(announced.iter()).chain(pending.iter()) {
        if !target.iter().any(|p| p == known) {
            return Err(AppError::msg(format!(
                "对话开始后不能移除已允许的路径：{}",
                known.display()
            )));
        }
    }

    for d in target {
        let existed = initial.iter().any(|p| p == &d)
            || announced.iter().any(|p| p == &d)
            || pending.iter().any(|p| p == &d);
        if !existed {
            session.pending_runtime_allowed_paths.push(d);
        }
    }
    Ok(())
}

/// 审批越界路径并落盘到 session（this-project）或全局 settings（all-project）。
/// 在 UI 用户点击 "this-project" / "all-project" 按钮时调用，
/// 内部会先把路径加进对应存储，再 resolve `request_id`（AllowOnce 语义即可生效本轮）。
#[tauri::command]
fn approve_path_access(
    app: AppHandle,
    hitl: State<'_, Arc<HitlState>>,
    request_id: String,
    paths: Vec<PathBuf>,
    scope: String,
    session_id: Option<String>,
) -> AppResult<()> {
    let dd = data_dir(&app)?;
    // scope 命名（前端/后端同步）：
    //   once          → 仅本次，不持久化
    //   this_session  → 仅当前对话（写 session.allowed_paths）
    //   this_project  → 当前 workdir 所有对话（PermissionStore Project scope FilePath 规则）
    //   global        → 任意对话（PermissionStore Global scope FilePath 规则）
    match scope.as_str() {
        "this_session" => {
            let session_id = session_id.clone().ok_or_else(|| {
                AppError::msg("approve_path_access: this_session 需要 session_id")
            })?;
            let mut s = sessions::load(&dd, &session_id)?;
            let mut existing = s.allowed_paths.unwrap_or_default();
            for p in &paths {
                if !existing.iter().any(|path| path == p) {
                    existing.push(p.clone());
                }
            }
            s.allowed_paths = Some(existing);
            sessions::save(&dd, s)?;
        }
        "global" => {
            let mut settings = settings_store::load(&dd);
            for p in &paths {
                if !settings
                    .conversation
                    .allowed_paths
                    .iter()
                    .any(|path| path == p)
                {
                    settings.conversation.allowed_paths.push(p.clone());
                }
            }
            settings_store::save(&dd, &settings)?;
        }
        "this_project" | "once" => {
            // this_project：仅落 PermissionStore Project FilePath 规则（在下面 hitl.resolve 后）
            // once：不持久化
        }
        other => return Err(AppError::msg(format!("未知 scope: {other}"))),
    }
    // resolve gate；workspace.add_allowed_path 已经由 agent_loop 在 AllowAndRemember 时执行
    let decision = match scope.as_str() {
        "once" => protocol::ApprovalDecision::AllowOnce,
        "this_session" => protocol::ApprovalDecision::AllowAndRemember {
            scope: protocol::PermissionScope::Session,
            pattern: None,
        extra_patterns: Vec::new(),
        },
        "this_project" => protocol::ApprovalDecision::AllowAndRemember {
            scope: protocol::PermissionScope::Project,
            pattern: None,
        extra_patterns: Vec::new(),
        },
        "global" => protocol::ApprovalDecision::AllowAndRemember {
            scope: protocol::PermissionScope::Global,
            pattern: None,
        extra_patterns: Vec::new(),
        },
        other => return Err(AppError::msg(format!("未知 scope: {other}"))),
    };
    hitl.resolve_approval(&request_id, decision)
        .map_err(AppError::msg)
}

/// 从 workdir + allowed_paths 发现所有规则文件（CLAUDE.md / AGENTS.md 等），
/// 返回轻量信息列表给前端渲染 Rules 开关列表。
#[tauri::command]
fn discover_rules_files(
    workdir: PathBuf,
    allowed_paths: Vec<PathBuf>,
) -> AppResult<Vec<RuleFileInfo>> {
    let files = agent_core::rules::discover(&workdir, &allowed_paths);
    Ok(files
        .into_iter()
        .map(|f| RuleFileInfo {
            path: f.path.display().to_string(),
            source: f.source,
        })
        .collect())
}

/// 发现"全部"规则文件：
/// - 全局：合并 `global_candidates`（来自 settings/session 配置）与 `default_global_rules()`，
///   过滤出真实存在的文件
/// - 项目：当 workdir 给定时调 `rules::discover` 扫祖先链 + allowed_paths
///
/// 用于 SessionSettingsDialog 的「全局/项目」两段统一列表。
#[tauri::command]
fn discover_all_rules(
    workdir: Option<PathBuf>,
    allowed_paths: Option<Vec<PathBuf>>,
    global_candidates: Option<Vec<PathBuf>>,
) -> AppResult<Vec<RuleFileInfo>> {
    use agent_core::rules::RuleSource;
    let mut out: Vec<RuleFileInfo> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut globals: Vec<PathBuf> = global_candidates.unwrap_or_default();
    for d in agent_core::rules::default_global_rules() {
        if !globals.contains(&d) {
            globals.push(d);
        }
    }
    for g in globals {
        if !g.exists() {
            continue;
        }
        let key = g.display().to_string();
        if seen.insert(key.clone()) {
            out.push(RuleFileInfo {
                path: key,
                source: RuleSource::Global,
            });
        }
    }

    if let Some(wd) = workdir {
        let ap = allowed_paths.unwrap_or_default();
        for f in agent_core::rules::discover(&wd, &ap) {
            let key = f.path.display().to_string();
            if seen.insert(key.clone()) {
                out.push(RuleFileInfo {
                    path: key,
                    source: f.source,
                });
            }
        }
    }
    Ok(out)
}

// ========== Path attach (粘贴/拖拽路径) ==========

/// 前端粘贴/拖拽路径时的探测结果。前端只调一次 RPC 就能拿到全部信息：
/// 是文件就直接返回 `MessageAttachment`，是目录就告诉前端把它加到 allowed_paths。
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AttachPathResult {
    Dir {
        path: String,
        name: String,
    },
    File {
        attachment: common::attachments::MessageAttachment,
    },
    Missing {
        path: String,
    },
    Unsupported {
        path: String,
        reason: String,
    },
}

/// 路径附件的兜底大小限制，与前端 ChatInput 中的 MAX_* 常量保持一致。
const MAX_TEXT_FILE_BYTES: u64 = 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 12 * 1024 * 1024;

#[tauri::command]
fn attach_path(path: String) -> AppResult<AttachPathResult> {
    use base64::Engine as _;
    let raw = path.trim();
    if raw.is_empty() {
        return Ok(AttachPathResult::Missing { path });
    }
    // 接受 file:// URI（macOS Finder / GTK 拖拽常见格式）
    let cleaned = raw
        .strip_prefix("file://")
        .map(|s| {
            // 把 %20 等百分号编码还原成原样路径
            percent_decode(s)
        })
        .unwrap_or_else(|| raw.to_string());
    let p = std::path::Path::new(&cleaned);
    let meta = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(_) => {
            return Ok(AttachPathResult::Missing { path });
        }
    };
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cleaned.clone());

    if meta.is_dir() {
        return Ok(AttachPathResult::Dir {
            path: p.to_string_lossy().into_owned(),
            name,
        });
    }

    let size = meta.len();
    let media_type = guess_media_type(p);
    let is_image = media_type.starts_with("image/");

    if is_image {
        if size > MAX_IMAGE_BYTES {
            return Ok(AttachPathResult::Unsupported {
                path,
                reason: format!("{name} 超过 12MB"),
            });
        }
        let bytes = std::fs::read(p)?;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        return Ok(AttachPathResult::File {
            attachment: common::attachments::MessageAttachment::Image {
                name,
                media_type,
                data,
            },
        });
    }

    if !looks_like_text(&media_type, p) {
        return Ok(AttachPathResult::Unsupported {
            path,
            reason: format!("{name} 不是支持的文本或图片文件"),
        });
    }
    if size > MAX_TEXT_FILE_BYTES {
        return Ok(AttachPathResult::Unsupported {
            path,
            reason: format!("{name} 超过 1MB"),
        });
    }
    let content =
        std::fs::read_to_string(p).map_err(|e| AppError::msg(format!("{name} 读取失败：{e}")))?;
    Ok(AttachPathResult::File {
        attachment: common::attachments::MessageAttachment::TextFile {
            name,
            media_type,
            content,
        },
    })
}

fn percent_decode(s: &str) -> String {
    // 简易 percent decode：只处理常见 %XX 的两位 hex；其余保持原样。
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn guess_media_type(p: &std::path::Path) -> String {
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => "image/png".into(),
        Some("jpg") | Some("jpeg") => "image/jpeg".into(),
        Some("gif") => "image/gif".into(),
        Some("webp") => "image/webp".into(),
        Some("bmp") => "image/bmp".into(),
        Some("svg") => "image/svg+xml".into(),
        Some("json") => "application/json".into(),
        Some("xml") => "application/xml".into(),
        Some("html") | Some("htm") => "text/html".into(),
        Some("css") => "text/css".into(),
        Some("csv") => "text/csv".into(),
        Some("md") | Some("markdown") => "text/markdown".into(),
        Some(_) => "text/plain".into(),
        None => "text/plain".into(),
    }
}

fn looks_like_text(media_type: &str, p: &std::path::Path) -> bool {
    if media_type.starts_with("text/") {
        return true;
    }
    if matches!(media_type, "application/json" | "application/xml") {
        return true;
    }
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    matches!(
        ext.as_deref(),
        Some(
            "txt"
                | "md"
                | "markdown"
                | "json"
                | "jsonl"
                | "csv"
                | "ts"
                | "tsx"
                | "js"
                | "jsx"
                | "rs"
                | "py"
                | "go"
                | "java"
                | "c"
                | "cpp"
                | "h"
                | "hpp"
                | "css"
                | "html"
                | "htm"
                | "xml"
                | "yaml"
                | "yml"
                | "toml"
                | "sql"
        )
    )
}

// ========== OAuth ==========

#[tauri::command]
async fn oauth_codex_start() -> AppResult<oauth::DeviceCodeInfo> {
    oauth::codex_start().await
}

#[tauri::command]
async fn oauth_codex_poll(device_code: String) -> AppResult<Option<oauth::CodexTokenInfo>> {
    oauth::codex_poll(&device_code).await
}

#[tauri::command]
async fn oauth_codex_refresh(refresh_token: String) -> AppResult<oauth::CodexTokenInfo> {
    oauth::codex_refresh(&refresh_token).await
}

#[tauri::command]
fn oauth_openai_start() -> AppResult<oauth::AuthUrlResult> {
    oauth::openai_oauth_start()
}

#[tauri::command]
async fn oauth_openai_exchange(
    session_id: String,
    code: String,
    state: Option<String>,
) -> AppResult<oauth::ImportedToken> {
    oauth::openai_oauth_exchange(&session_id, &code, state.as_deref()).await
}

#[tauri::command]
fn oauth_claude_start() -> AppResult<oauth::AuthUrlResult> {
    oauth::claude_oauth_start()
}

#[tauri::command]
async fn oauth_claude_exchange(
    session_id: String,
    code: String,
) -> AppResult<oauth::ImportedToken> {
    oauth::claude_oauth_exchange(&session_id, &code).await
}

#[tauri::command]
async fn oauth_claude_refresh(refresh_token: String) -> AppResult<oauth::ImportedToken> {
    oauth::claude_oauth_refresh(&refresh_token).await
}

#[tauri::command]
fn oauth_claude_code_import() -> AppResult<oauth::ImportedToken> {
    oauth::claude_code_import()
}

#[tauri::command]
fn oauth_gemini_start() -> AppResult<oauth::AuthUrlResult> {
    oauth::gemini_oauth_start()
}

#[tauri::command]
async fn oauth_gemini_exchange(
    session_id: String,
    code: String,
) -> AppResult<oauth::ImportedToken> {
    oauth::gemini_oauth_exchange(&session_id, &code).await
}

#[tauri::command]
async fn oauth_gemini_refresh(
    refresh_token: String,
    client_id: String,
    client_secret: String,
) -> AppResult<oauth::ImportedToken> {
    oauth::gemini_refresh(&refresh_token, &client_id, &client_secret).await
}

#[tauri::command]
async fn oauth_gemini_cli_import() -> AppResult<oauth::ImportedToken> {
    oauth::gemini_cli_import().await
}

#[tauri::command]
async fn deepseek_login(
    input: oauth::deepseek::DeepseekLoginInput,
) -> AppResult<oauth::deepseek::DeepseekLoginToken> {
    oauth::deepseek::deepseek_login(input).await
}

// ========== App startup ==========

/// 关窗时若仍有 pending HITL 或正在跑的 run：
/// - 把所有 pending 提问 / 审批按"取消"resolve（让 spawn_ask 醒来正常 emit
///   ToolCallFinished + UserQuestionAnswered，UI 持久化时能看到「取消」答案）
/// - 设置全部 run 的 cancel flag（阻止 agent_loop 进入下一轮 model.complete）
/// - 短暂等待（最多 2s）让 send_and_save 走完 persist_interrupted 把状态写盘
///
/// 用户硬关 / 强制退出还是会丢，这只是合作式的「正常关窗」路径。
fn handle_close_with_pending_hitl(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    let app = window.app_handle().clone();
    let hitl_state: Arc<HitlState> = match app.try_state::<Arc<HitlState>>() {
        Some(s) => s.inner().clone(),
        None => return,
    };
    let needs_wait = hitl_state.has_pending() || cancellation::has_active_runs();
    if !needs_wait {
        return;
    }
    api.prevent_close();
    hitl_state.cancel_all_pending();
    cancellation::cancel_all();
    let window = window.clone();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while cancellation::has_active_runs() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = window.close();
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 从 CWD 向上递归找 `.env` 并加载到进程环境。已有的 shell env 不会被覆盖
    // （shell > .env 优先级符合 12-factor 直觉）。dev 模式 CWD 在 apps/desktop，向上找命中
    // workspace 根的 .env；release 包从可执行文件所在目录向上找，需要时再加 from_path 兜底。
    let _ = dotenvy::dotenv();

    observability::init("agent_core=debug,model_gateway=info,warn");

    // 全局唯一 PermissionStore：从 ~/.hebbian/permissions.json 加载 Global 规则到内存，
    // 注入到每个 Session（架构 §4.6.2）。打开失败时打 warn，等同未挂 store——
    // AllowAndRemember(Global) 会兜底为 AllowOnce。
    let data_dir_for_core = agent_core::storage::default_data_dir();
    let permission_store = match PermissionStore::open(&data_dir_for_core) {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            tracing::warn!(error = %e, "PermissionStore 打开失败，全局权限规则将不可用");
            None
        }
    };
    // 架构 §7：LocalCoreClient 在 AppState 中作为同步 API 入口；Desktop 的
    // chat / send_message 仍走 chat 模块按需构造 Harness，因此 CoreClient
    // 不持 Harness。
    let core_client = Arc::new(LocalCoreClient::new(
        None,
        data_dir_for_core.clone(),
        permission_store.clone(),
    ));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(HitlState::default()))
        .manage(Arc::new(ForceAutomodeState::default()))
        .manage(permission_store)
        .manage(core_client)
        .setup(|app| {
            // macOS 在进程启动时会自动把 Regular 应用 activate 到前台，
            // dev 每次改代码重编译都会重启进程 → 抢走当前焦点。
            // 在进入 NSApplicationDidFinishLaunching 后立刻降级为 Accessory，
            // 系统的「自动激活」就失效；过几百毫秒再切回 Regular，
            // 此时不再触发 activate，但 dock 图标恢复正常。
            // 仅 debug 构建启用——release 启动是用户主动双击触发的，期望抢前台。
            #[cfg(all(target_os = "macos", debug_assertions))]
            {
                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    let _ = handle.set_activation_policy(tauri::ActivationPolicy::Regular);
                });
            }
            window_control::initialize(app.handle()).map_err(|err| {
                Box::<dyn std::error::Error>::from(std::io::Error::other(err.to_string()))
            })?;

            // 架构 §4.12.6：注册 WakeupScheduler 的 resume 回调。BgFinishHook /
            // CronTimer 触发时把 `<wakeup>` XML + session_id 通过 Tauri 事件
            // `wakeup-fired` 推给前端，前端 listener 自动把它当 user message 发出
            // → 后端 chat 命令检测到 checkpoint 走 resume_with 路径。
            let resume_handle = app.handle().clone();
            agent_core::wakeup::WakeupScheduler::global().set_resume_handler(Arc::new(
                move |event| {
                    let payload = serde_json::json!({
                        "session_id": event.session_id(),
                        "run_id": event.run_id(),
                        "wakeup_xml": agent_core::wakeup::wakeup_xml(&event),
                    });
                    if let Err(e) = resume_handle.emit("wakeup-fired", payload) {
                        tracing::warn!(error = %e, "failed to emit wakeup-fired tauri event");
                    }
                },
            ));

            Ok(())
        })
        .on_window_event(|window, event| {
            window_control::handle_window_event(window, event);
            if let WindowEvent::CloseRequested { api, .. } = event {
                handle_close_with_pending_hitl(window, api);
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_providers,
            save_providers,
            upsert_provider,
            list_provider_presets,
            fetch_provider_models,
            test_provider_model,
            list_prompts,
            upsert_prompt,
            delete_prompt,
            set_default_prompt,
            list_sessions,
            get_session,
            create_session,
            list_projects,
            save_project,
            delete_project,
            import_vscode_project,
            import_project_file,
            rename_session,
            delete_session,
            fork_session,
            truncate_after,
            truncate_inclusive,
            search_sessions,
            update_session_config,
            switch_provider_model,
            send_message,
            preview_session_payload,
            list_session_model_io,
            cancel_message,
            inject_user_message,
            get_context_usage,
            compact_session,
            approve_permission,
            answer_question,
            get_force_automode,
            set_force_automode,
            get_run_mode,
            set_run_mode,
            generate_session_title,
            list_tools,
            list_permissions,
            add_permission,
            remove_permission,
            clear_permissions,
            list_permission_paths,
            add_permission_path,
            remove_permission_path,
            list_skills,
            list_claude_skills,
            import_claude_skills,
            import_skills_from_dir,
            import_skills_from_github,
            scan_skill_dir,
            scan_skill_github,
            read_skill_md,
            set_skill_enabled,
            delete_skill,
            list_background_tasks,
            kill_background_task,
            get_settings,
            save_settings,
            update_session_settings,
            discover_rules_files,
            discover_all_rules,
            attach_path,
            approve_path_access,
            list_edits,
            diff_edit,
            revert_edit,
            edits_worktree_status,
            oauth_codex_start,
            oauth_codex_poll,
            oauth_codex_refresh,
            oauth_openai_start,
            oauth_openai_exchange,
            oauth_claude_start,
            oauth_claude_exchange,
            oauth_claude_refresh,
            oauth_claude_code_import,
            oauth_gemini_start,
            oauth_gemini_exchange,
            oauth_gemini_refresh,
            oauth_gemini_cli_import,
            deepseek_login,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
