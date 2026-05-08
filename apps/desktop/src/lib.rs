pub mod chat;
mod engine;
mod error;
mod hitl;
mod title_gen;
mod window_control;

pub use engine::EngineEvent;
pub use error::{AppError, AppResult};
pub use hitl::HitlState;

use std::sync::Arc;

use agent_core::tools::{self as tools, ToolInfo};
use model_gateway::{
    auth as oauth,
    config::{self as providers, Provider, ProviderPreset, ProvidersFile},
    discovery::{self as model_fetch, FetchedModel},
    health::{self as provider_health, ProviderModelTestResult},
};
use platform::{
    config::{
        prompts::{self as prompts, Prompt, PromptsFile},
        settings::{self as settings_store, Settings},
    },
    runtime as cancellation,
    storage::sessions::{
        self as sessions, Message, MessageMeta, Role, SearchHit, Session, SessionMeta,
    },
};
use std::path::PathBuf;
use tauri::{ipc::Channel, AppHandle, Manager, State};

fn data_dir(app: &AppHandle) -> AppResult<std::path::PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::msg(e.to_string()))?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ========== Providers ==========

#[tauri::command]
fn get_providers(app: AppHandle) -> AppResult<ProvidersFile> {
    providers::load(&data_dir(&app)?)
}

#[tauri::command]
fn save_providers(app: AppHandle, file: ProvidersFile) -> AppResult<()> {
    providers::save(&data_dir(&app)?, &file)
}

#[tauri::command]
fn upsert_provider(app: AppHandle, provider: Provider) -> AppResult<Provider> {
    providers::upsert(&data_dir(&app)?, provider)
}

#[tauri::command]
fn list_provider_presets() -> Vec<ProviderPreset> {
    providers::list_presets()
}

#[tauri::command]
async fn fetch_provider_models(provider: Provider) -> AppResult<Vec<FetchedModel>> {
    model_fetch::fetch(&provider).await
}

#[tauri::command]
async fn test_provider_model(
    provider: Provider,
    model: String,
) -> AppResult<ProviderModelTestResult> {
    provider_health::test_provider_model(provider, model)
        .await
        .map_err(|e| AppError::msg(e.to_string()))
}

// ========== Prompts ==========

#[tauri::command]
fn list_prompts(app: AppHandle) -> AppResult<PromptsFile> {
    prompts::load(&data_dir(&app)?)
}

#[tauri::command]
fn upsert_prompt(app: AppHandle, prompt: Prompt) -> AppResult<Prompt> {
    prompts::upsert(&data_dir(&app)?, prompt)
}

#[tauri::command]
fn delete_prompt(app: AppHandle, id: String) -> AppResult<()> {
    prompts::delete(&data_dir(&app)?, &id)
}

#[tauri::command]
fn set_default_prompt(app: AppHandle, id: Option<String>) -> AppResult<PromptsFile> {
    prompts::set_default(&data_dir(&app)?, id)
}

// ========== Sessions ==========

#[tauri::command]
fn list_sessions(app: AppHandle) -> AppResult<Vec<SessionMeta>> {
    sessions::list(&data_dir(&app)?)
}

#[tauri::command]
fn get_session(app: AppHandle, id: String) -> AppResult<Session> {
    sessions::load(&data_dir(&app)?, &id)
}

#[tauri::command]
fn create_session(
    app: AppHandle,
    provider_id: String,
    model: String,
    system_prompt: Option<String>,
    prompt_id: Option<String>,
) -> AppResult<Session> {
    sessions::create(
        &data_dir(&app)?,
        provider_id,
        model,
        system_prompt,
        prompt_id,
    )
}

#[tauri::command]
fn rename_session(app: AppHandle, id: String, title: String) -> AppResult<Session> {
    sessions::rename(&data_dir(&app)?, &id, title)
}

#[tauri::command]
fn delete_session(app: AppHandle, id: String) -> AppResult<()> {
    sessions::delete(&data_dir(&app)?, &id)
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
    sessions::search(
        &data_dir(&app)?,
        &query,
        case_sensitive.unwrap_or(false),
        regex.unwrap_or(false),
    )
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
    let from_provider = providers::get(&dd, &cur.provider_id)
        .map(|p| p.name)
        .unwrap_or_else(|_| cur.provider_id.clone());
    let to_provider = providers::get(&dd, &new_provider_id)
        .map(|p| p.name)
        .unwrap_or_else(|_| new_provider_id.clone());

    if cur.provider_id == new_provider_id && cur.model == new_model {
        return Ok(cur);
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
    sessions::save(&dd, updated)
}

#[tauri::command]
async fn send_message(
    app: AppHandle,
    hitl: State<'_, Arc<HitlState>>,
    session_id: String,
    content: String,
    attachments: Vec<platform::attachments::MessageAttachment>,
    stream: bool,
    enabled_tools: Vec<String>,
    request_id: String,
    on_event: Channel<EngineEvent>,
) -> AppResult<Message> {
    let cancel_flag = cancellation::register(request_id.clone());
    let result = chat::send_and_save(
        &app,
        chat::SendArgs {
            session_id,
            user_content: content,
            attachments,
            stream,
            enabled_tools,
            cancel_flag,
            hitl: Some(hitl.inner().clone()),
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

/// 用户在 UI 中点击审批按钮后调用。
///
/// `decision` 取值：`"allow_once"` / `"allow_and_remember"` / `"deny"` / `"deny_with_feedback"`
/// `feedback` 仅在 `deny_with_feedback` 时使用。
#[tauri::command]
fn approve_permission(
    hitl: State<'_, Arc<HitlState>>,
    request_id: String,
    decision: String,
    feedback: Option<String>,
) -> AppResult<()> {
    let decision = match decision.as_str() {
        "allow_once" => protocol::ApprovalDecision::AllowOnce,
        "allow_and_remember" => protocol::ApprovalDecision::AllowAndRemember {
            scope: protocol::PermissionScope::Session,
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
/// `kind` 取值：`"selected"` / `"custom"` / `"cancelled"`
/// `text` 在 `selected` 时是 label，在 `custom` 时是用户输入，在 `cancelled` 时忽略。
#[tauri::command]
fn answer_question(
    hitl: State<'_, Arc<HitlState>>,
    request_id: String,
    kind: String,
    text: Option<String>,
) -> AppResult<()> {
    let answer = match kind.as_str() {
        "selected" => protocol::UserAnswer::Selected {
            label: text.unwrap_or_default(),
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

#[tauri::command]
async fn generate_session_title(app: AppHandle, id: String) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    let s = sessions::load(&dd, &id)?;
    let has_user = s.messages.iter().any(|m| matches!(m.role, Role::User));
    if !has_user {
        return Ok(s);
    }
    let provider = providers::get(&dd, &s.provider_id)?;
    let provider = oauth::refresh::ensure_fresh_provider_token(&dd, provider)
        .await
        .map_err(|e| AppError::msg(format!("OAuth token 刷新失败: {e}")))?;
    let title = title_gen::generate(&provider, &s.model, &s.messages).await?;
    sessions::rename(&dd, &id, title)
}

#[tauri::command]
fn list_tools() -> Vec<ToolInfo> {
    tools::tool_manifest()
}

// ========== Settings ==========

#[tauri::command]
fn get_settings(app: AppHandle) -> AppResult<Settings> {
    Ok(settings_store::load(&data_dir(&app)?))
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> AppResult<()> {
    settings_store::save(&data_dir(&app)?, &settings)
}

/// 更新对话级设置（workdir / allowed_dirs / enabled_tools / skill_dirs）。
/// 任一字段传 `null` = 清空（回退到全局默认）。
#[tauri::command]
fn update_session_settings(
    app: AppHandle,
    id: String,
    workdir: Option<Option<PathBuf>>,
    allowed_dirs: Option<Option<Vec<PathBuf>>>,
    enabled_tools: Option<Option<Vec<String>>>,
    skill_dirs: Option<Option<Vec<PathBuf>>>,
) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    let mut s = sessions::load(&dd, &id)?;
    if let Some(v) = workdir {
        s.workdir = v;
    }
    if let Some(v) = allowed_dirs {
        s.allowed_dirs = v;
    }
    if let Some(v) = enabled_tools {
        s.enabled_tools = v;
    }
    if let Some(v) = skill_dirs {
        s.skill_dirs = v;
    }
    sessions::save(&dd, s)
}

/// 审批越界路径并落盘到 session（this-project）或全局 settings（all-project）。
/// 在 UI 用户点击 "this-project" / "all-project" 按钮时调用，
/// 内部会先把目录加进对应存储，再 resolve `request_id`（AllowOnce 语义即可生效本轮）。
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
    match scope.as_str() {
        "this_project" => {
            let session_id = session_id.ok_or_else(|| {
                AppError::msg("approve_path_access: this_project 需要 session_id")
            })?;
            let mut s = sessions::load(&dd, &session_id)?;
            let mut existing = s.allowed_dirs.unwrap_or_default();
            for p in &paths {
                if !existing.iter().any(|d| d == p) {
                    existing.push(p.clone());
                }
            }
            s.allowed_dirs = Some(existing);
            sessions::save(&dd, s)?;
        }
        "all_project" => {
            let mut settings = settings_store::load(&dd);
            for p in &paths {
                if !settings.conversation.allowed_dirs.iter().any(|d| d == p) {
                    settings.conversation.allowed_dirs.push(p.clone());
                }
            }
            settings_store::save(&dd, &settings)?;
        }
        "once" => {
            // 不持久化，仅放行本次
        }
        other => return Err(AppError::msg(format!("未知 scope: {other}"))),
    }
    // resolve gate；workspace.add_allowed_dir 已经由 agent_loop 在 AllowAndRemember 时执行
    let scope_enum = match scope.as_str() {
        "all_project" => protocol::PermissionScope::Global,
        "this_project" => protocol::PermissionScope::Session,
        _ => protocol::PermissionScope::Run,
    };
    let decision = if scope == "once" {
        protocol::ApprovalDecision::AllowOnce
    } else {
        protocol::ApprovalDecision::AllowAndRemember { scope: scope_enum }
    };
    hitl.resolve_approval(&request_id, decision)
        .map_err(AppError::msg)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("agent_core=debug,warn")),
        )
        .with_target(true)
        .compact()
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(HitlState::default()))
        .setup(|app| {
            window_control::initialize(app.handle()).map_err(|err| {
                Box::<dyn std::error::Error>::from(std::io::Error::other(err.to_string()))
            })?;
            Ok(())
        })
        .on_window_event(|window, event| {
            window_control::handle_window_event(window, event);
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
            rename_session,
            delete_session,
            fork_session,
            truncate_after,
            truncate_inclusive,
            search_sessions,
            update_session_config,
            switch_provider_model,
            send_message,
            cancel_message,
            approve_permission,
            answer_question,
            generate_session_title,
            list_tools,
            get_settings,
            save_settings,
            update_session_settings,
            approve_path_access,
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
