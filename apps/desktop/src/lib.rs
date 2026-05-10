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
use tauri::{ipc::Channel, AppHandle, Manager, State, WindowEvent};

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
    reasoning: Option<platform::ReasoningConfig>,
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
        sessions::insert_reasoning_switch_marker(
            &dd,
            &id,
            prev_reasoning,
            s.reasoning.clone(),
        )?;
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
    let supports = platform::reasoning::anthropic_supports_thinking(&updated.model)
        || platform::reasoning::openai_supports_reasoning(&updated.model);
    if supports {
        // 首次切到支持推理的模型：默认 thinking on + extra effort（用户可在 UI 改）
        if updated.reasoning.is_none() {
            updated.reasoning = Some(platform::ReasoningConfig {
                enabled: Some(true),
                effort: Some(platform::ReasoningEffort::Extra),
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
    session_id: String,
    content: String,
    attachments: Vec<platform::attachments::MessageAttachment>,
    stream: bool,
    enabled_tools: Vec<String>,
    request_id: String,
    on_event: Channel<EngineEvent>,
) -> AppResult<Message> {
    let runtime = cancellation::register(request_id.clone());
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
    attachments: Vec<platform::attachments::MessageAttachment>,
) -> AppResult<platform::storage::sessions::Message> {
    use platform::storage::sessions::{self, Message, Role};
    let dd = data_dir(&app)?;
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
    sessions::append_message(&dd, &session_id, user_msg.clone())?;

    let injected = cancellation::inject_pending_input(
        &request_id,
        platform::runtime::PendingUserInput {
            content,
            attachments,
        },
    );
    if !injected {
        // run 已经结束（或还没注册）——session.json 里这条 user message 已经落盘，
        // 前端拿到它后会把它作为接下来一条 user 显示，等用户决定下一步。
        tracing::debug!(
            request_id,
            "inject_user_message: run not registered, message persisted only"
        );
    }
    Ok(user_msg)
}

#[tauri::command]
fn get_context_usage(app: AppHandle, session_id: String) -> AppResult<chat::ContextUsageDto> {
    chat::context_usage(&data_dir(&app)?, &session_id)
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
) -> AppResult<()> {
    let decision = match decision.as_str() {
        "allow_once" => protocol::ApprovalDecision::AllowOnce,
        "allow_and_remember" => protocol::ApprovalDecision::AllowAndRemember {
            scope: protocol::PermissionScope::Session,
            pattern,
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

#[tauri::command]
async fn generate_session_title(app: AppHandle, id: String) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    let s = sessions::load(&dd, &id)?;
    let has_user = s.messages.iter().any(|m| matches!(m.role, Role::User));
    if !has_user {
        return Ok(s);
    }

    // 决定用谁来生成标题：优先用 ProvidersFile 中标记为「标题生成模型」的 provider，
    // 否则回退到 session 自己的 provider/model。
    let providers_file = providers::load(&dd)?;
    let title_provider = providers_file.providers.into_iter().find(|p| {
        p.enabled
            && p.title_gen_enabled
            && p.title_gen_model.as_deref().is_some_and(|m| !m.is_empty())
    });

    let (provider, model) = match title_provider {
        Some(p) => {
            let model = p.title_gen_model.clone().unwrap_or_default();
            (p, model)
        }
        None => (providers::get(&dd, &s.provider_id)?, s.model.clone()),
    };

    let title = match try_generate_title(&dd, provider, &model, &s.messages).await {
        Some(t) => t,
        None => title_gen::fallback_from_first_user(&s.messages),
    };
    sessions::rename(&dd, &id, title)
}

async fn try_generate_title(
    dd: &std::path::Path,
    provider: model_gateway::config::Provider,
    model: &str,
    messages: &[platform::storage::sessions::Message],
) -> Option<String> {
    let provider = oauth::refresh::ensure_fresh_provider_token(dd, provider)
        .await
        .ok()?;
    title_gen::generate(&provider, model, messages).await.ok()
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
///
/// 三态语义靠两组字段表达，避开 `Option<Option<T>>` 在 IPC 反序列化时
/// 把 `null` 直接折叠成外层 `None` 的歧义：
/// - 设值：传 `xxx` 字段，例如 `workdir = "/foo"` / `allowed_dirs = ["/bar"]`
/// - 清空：传 `clearXxx = true`（前端 invoke 用 camelCase）
/// - 不动：两边都不传
///
/// `allowed_dirs` 的特殊语义：
/// - 对话还没发出过 user message → 直接覆盖 `s.allowed_dirs`（initial 集合可任意改）
/// - 对话已开始 → `s.allowed_dirs` 锁定，**禁止删除任何已存在的目录**；新增的目录
///   追加到 `pending_runtime_allowed_dirs`，下次 send_message 时通过
///   `<workspace-update>` 段告诉模型，**不会改 system prompt**，因此 prompt cache 不破。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn update_session_settings(
    app: AppHandle,
    id: String,
    workdir: Option<PathBuf>,
    clear_workdir: Option<bool>,
    allowed_dirs: Option<Vec<PathBuf>>,
    clear_allowed_dirs: Option<bool>,
    enabled_tools: Option<Vec<String>>,
    clear_enabled_tools: Option<bool>,
    skill_dirs: Option<Vec<PathBuf>>,
    clear_skill_dirs: Option<bool>,
) -> AppResult<Session> {
    let dd = data_dir(&app)?;
    let mut s = sessions::load(&dd, &id)?;
    if clear_workdir.unwrap_or(false) {
        s.workdir = None;
    } else if let Some(v) = workdir {
        s.workdir = Some(v);
    }
    if clear_allowed_dirs.unwrap_or(false) {
        apply_allowed_dirs_update(&mut s, None)?;
    } else if let Some(v) = allowed_dirs {
        apply_allowed_dirs_update(&mut s, Some(v))?;
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
    sessions::save(&dd, s)
}

/// `update_session_settings` 中 `allowed_dirs` 字段的处理逻辑，单独拆出来便于测试。
fn apply_allowed_dirs_update(
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
        session.allowed_dirs = new_value;
        session.runtime_allowed_dirs.clear();
        session.pending_runtime_allowed_dirs.clear();
        return Ok(());
    }

    // 对话已开始：锁定 initial。新值必须是当前所有已知目录的超集，新增项进 pending。
    let target: Vec<PathBuf> = new_value.unwrap_or_default();
    let initial: Vec<PathBuf> = session.allowed_dirs.clone().unwrap_or_default();
    let announced: Vec<PathBuf> = session.runtime_allowed_dirs.clone();
    let pending: Vec<PathBuf> = session.pending_runtime_allowed_dirs.clone();

    for known in initial.iter().chain(announced.iter()).chain(pending.iter()) {
        if !target.iter().any(|p| p == known) {
            return Err(AppError::msg(format!(
                "对话开始后不能移除已允许的目录：{}",
                known.display()
            )));
        }
    }

    for d in target {
        let existed = initial.iter().any(|p| p == &d)
            || announced.iter().any(|p| p == &d)
            || pending.iter().any(|p| p == &d);
        if !existed {
            session.pending_runtime_allowed_dirs.push(d);
        }
    }
    Ok(())
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
        // 路径越界审批不在工具/命令维度，pattern 永远 None
        protocol::ApprovalDecision::AllowAndRemember {
            scope: scope_enum,
            pattern: None,
        }
    };
    hitl.resolve_approval(&request_id, decision)
        .map_err(AppError::msg)
}

// ========== Path attach (粘贴/拖拽路径) ==========

/// 前端粘贴/拖拽路径时的探测结果。前端只调一次 RPC 就能拿到全部信息：
/// 是文件就直接返回 `MessageAttachment`，是目录就告诉前端把它加到 allowed_dirs。
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AttachPathResult {
    Dir { path: String, name: String },
    File { attachment: platform::attachments::MessageAttachment },
    Missing { path: String },
    Unsupported { path: String, reason: String },
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
            attachment: platform::attachments::MessageAttachment::Image {
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
    let content = std::fs::read_to_string(p)
        .map_err(|e| AppError::msg(format!("{name} 读取失败：{e}")))?;
    Ok(AttachPathResult::File {
        attachment: platform::attachments::MessageAttachment::TextFile {
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
            if let (Some(h), Some(l)) = (
                hex_val(bytes[i + 1]),
                hex_val(bytes[i + 2]),
            ) {
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
            "txt" | "md" | "markdown" | "json" | "jsonl" | "csv" | "ts" | "tsx" | "js"
            | "jsx" | "rs" | "py" | "go" | "java" | "c" | "cpp" | "h" | "hpp" | "css"
            | "html" | "htm" | "xml" | "yaml" | "yml" | "toml" | "sql"
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
    // 同步入口：observability::init 内部用独占 tokio runtime 跑 OTel 导出 task，
    // 与 Tauri 的 runtime 完全隔离。OTEL_EXPORTER_OTLP_ENDPOINT 未设时只装日志。
    let otel_guard = observability::init("hebbian-desktop", "agent_core=debug,warn");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(HitlState::default()))
        .manage(otel_guard)
        .setup(|app| {
            window_control::initialize(app.handle()).map_err(|err| {
                Box::<dyn std::error::Error>::from(std::io::Error::other(err.to_string()))
            })?;
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
            cancel_message,
            inject_user_message,
            get_context_usage,
            compact_session,
            approve_permission,
            answer_question,
            generate_session_title,
            list_tools,
            get_settings,
            save_settings,
            update_session_settings,
            attach_path,
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
