//! axum router + WebSocket 处理 + invoke 命令派发。
//!
//! Routes:
//!   GET  /healthz         — 健康检查
//!   GET  /ws              — WebSocket 升级
//!   GET  /*               — 前端静态文件（dist 目录）
//!
//! 命令派发：每条 `Invoke` WS 消息 → 查 cmd 名字 → 调对应 handler → 回 InvokeResponse。
//! 事件流：handler 通过 `runtime.emit_engine_event(...)` 广播到所有订阅本 session 的 WS。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use agent_core::{
    core_client::{CoreClient, LocalCoreClient},
    permissions::PermissionStore,
    storage::{
        projects as projects_store, prompts as prompts_store, sessions as sessions_store,
        sessions_dir, settings as settings_store,
    },
};
use anyhow::{anyhow, Result};
use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::{get, get_service},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use protocol::{ApprovalDecision, PermissionScope, UserAnswer};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, RwLock};
use tower_http::services::ServeDir;
use tracing::{info, warn};

use crate::protocol::{WsClientMessage, WsServerMessage};
use crate::session::{run_turn, SessionRuntime};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const EVENT_CHANNEL_CAPACITY: usize = 1024;

// ─── 共享 server 状态 ─────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ServerState {
    pub data_dir: PathBuf,
    /// session_id → SessionRuntime（多 session 共享同一进程）
    pub sessions: Arc<RwLock<HashMap<String, Arc<SessionRuntime>>>>,
    pub permission_store: Option<Arc<PermissionStore>>,
    /// 复用 desktop 同一个业务 facade。CoreClient trait 暴露的 25+ 方法
    /// 直接可用——无需 hebweb 自己再 wrap 一遍 storage / model_gateway API。
    pub core: Arc<LocalCoreClient>,
}

impl ServerState {
    pub fn new(data_dir: PathBuf) -> Self {
        let permission_store = PermissionStore::open(&data_dir).ok().map(Arc::new);
        // hebweb 内不挂 Harness——每个 SessionRuntime 自己跑 agent_loop。
        // facade 仅承担同步 API 转发（list/save/get providers / sessions / prompts / ...）。
        let core = Arc::new(LocalCoreClient::new(
            None,
            data_dir.clone(),
            permission_store.clone(),
        ));
        Self {
            data_dir,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            permission_store,
            core,
        }
    }

    /// 取已 attach 的 SessionRuntime；若不存在则按 session.json 自动 attach 一个。
    /// attach 失败（session 不存在 / 缺 provider）返回 Err。
    pub async fn ensure_runtime(&self, session_id: &str) -> Result<Arc<SessionRuntime>> {
        if let Some(rt) = self.sessions.read().await.get(session_id).cloned() {
            return Ok(rt);
        }
        // 加锁后再检查一次，避免并发重复 attach
        let mut guard = self.sessions.write().await;
        if let Some(rt) = guard.get(session_id).cloned() {
            return Ok(rt);
        }

        let session = sessions_store::load(&self.data_dir, session_id)
            .map_err(|e| anyhow!("session {session_id} 不存在：{e}"))?;
        sessions_dir::ensure_session_dirs(&self.data_dir, session_id)?;

        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
        let (event_tx, _) = broadcast::channel::<WsServerMessage>(EVENT_CHANNEL_CAPACITY);
        let run_mode = session.run_mode;

        let runtime = Arc::new(SessionRuntime {
            session_id: session_id.to_string(),
            data_dir: self.data_dir.clone(),
            provider_id: session.provider_id.clone(),
            model: session.model.clone(),
            reasoning: None,
            pending_approvals: Mutex::new(HashMap::new()),
            pending_questions: Mutex::new(HashMap::new()),
            active_run: AtomicBool::new(false),
            cancel_flag: Mutex::new(None),
            pending_inputs: Mutex::new(None),
            run_mode: Mutex::new(run_mode),
            force_automode: AtomicBool::new(false),
            input_tx,
            event_tx,
            permission_store: self.permission_store.clone(),
        });

        // 启动 turn 主循环：依次处理 input_rx 的每条输入
        let rt_for_loop = runtime.clone();
        tokio::spawn(async move {
            while let Some(text) = input_rx.recv().await {
                if let Err(e) = run_turn(rt_for_loop.clone(), text).await {
                    rt_for_loop.emit_engine_event(crate::events::EngineEvent::Error {
                        message: e.to_string(),
                    });
                }
            }
        });

        guard.insert(session_id.to_string(), runtime.clone());
        Ok(runtime)
    }
}

// ─── axum router ──────────────────────────────────────────────────────────

/// 启动 server。`static_dir` 是 dist/ 目录；不存在时只提供 healthz + ws。
pub fn build_router(state: ServerState, static_dir: Option<PathBuf>) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_upgrade));

    if let Some(dir) = static_dir {
        if dir.exists() {
            // fallback_service：未匹配的路径都走静态文件
            router = router.fallback_service(get_service(ServeDir::new(dir.clone())));
            info!(?dir, "serving static frontend");
        } else {
            warn!(?dir, "static frontend dir 不存在，跳过");
        }
    }

    router.with_state(state)
}

async fn healthz(State(state): State<ServerState>) -> impl IntoResponse {
    let active: Vec<String> = state.sessions.read().await.keys().cloned().collect();
    Json(json!({
        "ok": true,
        "version": SERVER_VERSION,
        "data_dir": state.data_dir.display().to_string(),
        "active_sessions": active,
    }))
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

// ─── 单 WS 连接 ────────────────────────────────────────────────────────────

async fn handle_ws(socket: WebSocket, state: ServerState) {
    let (mut sender, mut receiver) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<WsServerMessage>();

    // 发送任务：把 WsServerMessage 序列化后写到 ws
    let send_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let Ok(text) = serde_json::to_string(&msg) else { continue };
            if sender.send(WsMessage::Text(text)).await.is_err() {
                break;
            }
        }
    });

    // 握手
    let _ = out_tx.send(WsServerMessage::Hello { server_version: SERVER_VERSION });

    // 当前订阅的 session（同一连接同时只订阅一个；切换时 abort 旧 task）
    let mut event_task: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(Ok(msg)) = receiver.next().await {
        let text = match msg {
            WsMessage::Text(t) => t,
            WsMessage::Close(_) => break,
            _ => continue,
        };
        let parsed: WsClientMessage = match serde_json::from_str(&text) {
            Ok(p) => p,
            Err(e) => {
                let _ = out_tx.send(WsServerMessage::err(
                    String::new(),
                    format!("invalid message: {e}"),
                ));
                continue;
            }
        };

        match parsed {
            WsClientMessage::Subscribe { session_id } => {
                // 终止旧订阅
                if let Some(t) = event_task.take() {
                    t.abort();
                }
                match state.ensure_runtime(&session_id).await {
                    Ok(runtime) => {
                        let mut rx = runtime.event_tx.subscribe();
                        let tx = out_tx.clone();
                        let sid = session_id.clone();
                        event_task = Some(tokio::spawn(async move {
                            while let Ok(ev) = rx.recv().await {
                                if tx.send(ev).is_err() {
                                    break;
                                }
                            }
                            tracing::debug!(session_id = %sid, "event subscription ended");
                        }));
                        let _ = out_tx.send(WsServerMessage::Subscribed { session_id });
                    }
                    Err(e) => {
                        let _ = out_tx.send(WsServerMessage::err(
                            String::new(),
                            format!("subscribe failed: {e}"),
                        ));
                    }
                }
            }
            WsClientMessage::Unsubscribe => {
                if let Some(t) = event_task.take() {
                    t.abort();
                }
            }
            WsClientMessage::Invoke { id, cmd, args, session_id } => {
                let response = dispatch_invoke(&state, &cmd, args, session_id).await;
                let msg = match response {
                    Ok(data) => WsServerMessage::ok(id, data),
                    Err(e) => WsServerMessage::err(id, e),
                };
                let _ = out_tx.send(msg);
            }
        }
    }

    if let Some(t) = event_task {
        t.abort();
    }
    drop(out_tx);
    let _ = send_task.await;
}

// ─── invoke 派发 ───────────────────────────────────────────────────────────

async fn dispatch_invoke(
    state: &ServerState,
    cmd: &str,
    args: Value,
    session_id: Option<String>,
) -> Result<Option<Value>> {
    match cmd {
        // 核心交互
        "list_sessions" => cmd_list_sessions(state).await.map(Some),
        "get_session" => cmd_get_session(state, args).await.map(Some),
        "create_session" => cmd_create_session(state, args).await.map(Some),
        "send_message" => cmd_send_message(state, args, session_id).await.map(|_| None),
        "inject_user_message" => {
            cmd_inject_user_message(state, args, session_id).await.map(|_| None)
        }
        "approve_permission" => cmd_approve_permission(state, args, session_id).await.map(|_| None),
        "answer_question" => cmd_answer_question(state, args, session_id).await.map(|_| None),
        "cancel_message" => cmd_cancel_message(state, session_id).await.map(|_| None),
        // 前端 init 必需的只读命令（直接读 ~/.hebbian/ 文件）
        "get_providers" => cmd_get_providers(state).await.map(Some),
        "list_provider_presets" => cmd_list_provider_presets().await.map(Some),
        "list_prompts" => cmd_list_prompts(state).await.map(Some),
        "list_projects" => cmd_list_projects(state).await.map(Some),
        "get_settings" => cmd_get_settings(state).await.map(Some),
        // providers 写
        "save_providers" => cmd_save_providers(state, args).await.map(|_| None),
        "upsert_provider" => cmd_upsert_provider(state, args).await.map(Some),
        // prompts 写
        "upsert_prompt" => cmd_upsert_prompt(state, args).await.map(Some),
        "delete_prompt" => cmd_delete_prompt(state, args).await.map(|_| None),
        "set_default_prompt" => cmd_set_default_prompt(state, args).await.map(Some),
        // sessions 写
        "rename_session" => cmd_rename_session(state, args).await.map(Some),
        "delete_session" => cmd_delete_session(state, args).await.map(|_| None),
        "fork_session" => cmd_fork_session(state, args).await.map(Some),
        "truncate_after" => cmd_truncate_after(state, args).await.map(Some),
        "truncate_inclusive" => cmd_truncate_inclusive(state, args).await.map(Some),
        "search_sessions" => cmd_search_sessions(state, args).await.map(Some),
        "update_session_config" => cmd_update_session_config(state, args).await.map(Some),
        // settings 写
        "save_settings" => cmd_save_settings(state, args).await.map(|_| None),
        // projects 写
        "save_project" => cmd_save_project(state, args).await.map(Some),
        "delete_project" => cmd_delete_project(state, args).await.map(|_| None),
        // mode
        "get_run_mode" => cmd_get_run_mode(state, session_id).await.map(Some),
        "set_run_mode" => cmd_set_run_mode(state, args, session_id).await.map(|_| None),
        "get_force_automode" => cmd_get_force_automode(state, session_id).await.map(Some),
        "set_force_automode" => cmd_set_force_automode(state, args, session_id).await.map(|_| None),
        // ─── 走 LocalCoreClient facade（与 desktop 共享同一份业务逻辑）
        "get_provider" => cmd_core_get_provider(state, args).await.map(Some),
        "fetch_provider_models" => cmd_core_fetch_provider_models(state, args).await.map(Some),
        "test_provider_model" => cmd_core_test_provider_model(state, args).await.map(Some),
        "list_tools" => cmd_core_list_tools(state).await.map(Some),
        "list_permission_rules" => cmd_core_list_permission_rules(state, args).await.map(Some),
        "remove_permission_rule" => {
            cmd_core_remove_permission_rule(state, args).await.map(Some)
        }
        "clear_permission_rules" => {
            cmd_core_clear_permission_rules(state, args).await.map(|_| None)
        }
        // 其余 desktop Tauri command 在 v1 浏览器 surface 暂不实现
        // OAuth 系列、edits diff/revert、fetch/test provider models、context_usage / compact
        // 都依赖额外组件或外部 HTTP 调用，本期 hebweb 留 not_implemented
        other => Err(anyhow!(
            "command `{other}` not implemented in hebweb v1 (use Desktop for now)"
        )),
    }
}

// ─── 命令实现 ──────────────────────────────────────────────────────────────

async fn cmd_list_sessions(state: &ServerState) -> Result<Value> {
    let metas = sessions_store::list(&state.data_dir).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(metas)?)
}

async fn cmd_get_session(state: &ServerState, args: Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let session = sessions_store::load(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(session)?)
}

async fn cmd_create_session(state: &ServerState, args: Value) -> Result<Value> {
    let provider_id = args
        .get("providerId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `providerId`"))?
        .to_string();
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `model`"))?
        .to_string();
    let system_prompt = args
        .get("systemPrompt")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let prompt_id = args
        .get("promptId")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let session = sessions_store::create_with_source(
        &state.data_dir,
        provider_id,
        model,
        system_prompt,
        prompt_id,
        "hebweb".to_string(),
    )
    .map_err(|e| anyhow!("{e}"))?;
    sessions_dir::ensure_session_dirs(&state.data_dir, &session.id)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(session)?)
}

fn need_session(session_id: Option<String>) -> Result<String> {
    session_id.ok_or_else(|| anyhow!("missing `session_id`"))
}

/// 接受 `content` 或 `text` 任一字段，让 hebweb 同时兼容 desktop 前端（用 content）
/// 与 heb CLI / 简化的脚本客户端（多半用 text）。
fn pick_text(args: &Value) -> Result<String> {
    for k in ["content", "text"] {
        if let Some(s) = args.get(k).and_then(|v| v.as_str()) {
            return Ok(s.to_string());
        }
    }
    Err(anyhow!("missing `content` (or `text`)"))
}

async fn cmd_send_message(
    state: &ServerState,
    args: Value,
    session_id: Option<String>,
) -> Result<()> {
    let sid = need_session(session_id)?;
    let text = pick_text(&args)?;

    let runtime = state.ensure_runtime(&sid).await?;
    if runtime.is_active() {
        // 有 active run 时，新输入注入当前 turn 的 pending_inputs（fire-and-forget）
        if !runtime.inject(text) {
            return Err(anyhow!("inject failed: no active pending_inputs"));
        }
        return Ok(());
    }
    // 直接 await run_turn，让前端 invoke 等到整个 turn（含 HITL 审批）完成才 resolve。
    // 跟 Tauri send_message 行为一致：前端拿到 invoke resolve 时 turn 已结束，
    // 此刻清理 sessionStreams 槽是安全的；否则 ws 推来的 permission_requested
    // 会因槽已被清而被丢弃，popup 渲染不出来。
    run_turn(runtime, text).await
}

async fn cmd_inject_user_message(
    state: &ServerState,
    args: Value,
    session_id: Option<String>,
) -> Result<()> {
    let sid = need_session(session_id)?;
    let text = pick_text(&args)?;
    let runtime = state.ensure_runtime(&sid).await?;
    if !runtime.inject(text) {
        return Err(anyhow!("no active run, nothing to inject"));
    }
    Ok(())
}

async fn cmd_approve_permission(
    state: &ServerState,
    args: Value,
    session_id: Option<String>,
) -> Result<()> {
    let sid = need_session(session_id)?;
    let request_id = args
        .get("requestId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `requestId`"))?
        .to_string();
    let decision_str = args
        .get("decision")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `decision`"))?;
    // 与 desktop ApprovalDecision 一致：allow_once / allow_and_remember / deny / deny_with_feedback
    // 同时兼容 heb CLI 的简短写法 allow / deny + scope=once|session|project|global
    let scope_str = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("session");
    let pattern = args.get("pattern").and_then(|v| v.as_str()).map(str::to_string);
    let feedback = args.get("feedback").and_then(|v| v.as_str()).map(str::to_string);
    let extra_patterns: Vec<String> = args
        .get("extraPatterns")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    let scope = match scope_str {
        "project" => PermissionScope::Project,
        "global" => PermissionScope::Global,
        _ => PermissionScope::Session,
    };

    let decision = match decision_str {
        "allow_once" => ApprovalDecision::AllowOnce,
        "allow_and_remember" => ApprovalDecision::AllowAndRemember {
            scope,
            pattern,
            extra_patterns,
        },
        "deny" => ApprovalDecision::Deny,
        "deny_with_feedback" => ApprovalDecision::DenyWithFeedback {
            feedback: feedback
                .ok_or_else(|| anyhow!("deny_with_feedback requires `feedback`"))?,
        },
        // 简短形态（heb CLI 风格）
        "allow" if scope_str == "once" => ApprovalDecision::AllowOnce,
        "allow" => ApprovalDecision::AllowAndRemember {
            scope,
            pattern,
            extra_patterns,
        },
        _ => return Err(anyhow!("invalid decision: {decision_str}")),
    };

    let runtime = state.ensure_runtime(&sid).await?;
    let tx = runtime.pending_approvals.lock().unwrap().remove(&request_id);
    match tx {
        Some(tx) => {
            let _ = tx.send(decision);
            Ok(())
        }
        None => Err(anyhow!("unknown request_id: {request_id}")),
    }
}

async fn cmd_answer_question(
    state: &ServerState,
    args: Value,
    session_id: Option<String>,
) -> Result<()> {
    let sid = need_session(session_id)?;
    let request_id = args
        .get("requestId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `requestId`"))?
        .to_string();
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("selected");
    // 兼容前端两种命名：value（heb CLI 风格）/ text（desktop tauri.ts 风格）
    let value = args
        .get("value")
        .or_else(|| args.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let labels: Vec<String> = args
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    let answer = match kind {
        "cancelled" => UserAnswer::Cancelled,
        "custom" => UserAnswer::Custom { text: value },
        "selected_multi" => UserAnswer::SelectedMulti { labels },
        _ => UserAnswer::Selected { label: value },
    };

    let runtime = state.ensure_runtime(&sid).await?;
    let tx = runtime.pending_questions.lock().unwrap().remove(&request_id);
    match tx {
        Some(tx) => {
            let _ = tx.send(answer);
            Ok(())
        }
        None => Err(anyhow!("unknown request_id: {request_id}")),
    }
}

async fn cmd_cancel_message(state: &ServerState, session_id: Option<String>) -> Result<()> {
    let sid = need_session(session_id)?;
    let runtime = state.ensure_runtime(&sid).await?;
    runtime.stop();
    Ok(())
}

// ─── 只读命令（前端 init 必需）──────────────────────────────────────────────

async fn cmd_get_providers(state: &ServerState) -> Result<Value> {
    let file = model_gateway::config::load(&state.data_dir).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(file)?)
}

async fn cmd_list_provider_presets() -> Result<Value> {
    let presets = model_gateway::config::list_presets();
    Ok(serde_json::to_value(presets)?)
}

async fn cmd_list_prompts(state: &ServerState) -> Result<Value> {
    let file = prompts_store::load(&state.data_dir).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(file)?)
}

async fn cmd_list_projects(state: &ServerState) -> Result<Value> {
    let projects = projects_store::list(&state.data_dir).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(projects)?)
}

async fn cmd_get_settings(state: &ServerState) -> Result<Value> {
    let s = settings_store::load(&state.data_dir);
    Ok(serde_json::to_value(s)?)
}

// ─── 写命令 ─────────────────────────────────────────────────────────────────

async fn cmd_save_providers(state: &ServerState, args: Value) -> Result<()> {
    let file: model_gateway::config::ProvidersFile =
        serde_json::from_value(args.get("file").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("missing/invalid `file`: {e}"))?;
    model_gateway::config::save(&state.data_dir, &file).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_upsert_provider(state: &ServerState, args: Value) -> Result<Value> {
    let provider: model_gateway::config::Provider =
        serde_json::from_value(args.get("provider").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("missing/invalid `provider`: {e}"))?;
    let saved = model_gateway::config::upsert(&state.data_dir, provider)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(saved)?)
}

async fn cmd_upsert_prompt(state: &ServerState, args: Value) -> Result<Value> {
    let prompt: agent_core::storage::prompts::Prompt =
        serde_json::from_value(args.get("prompt").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("missing/invalid `prompt`: {e}"))?;
    let saved = prompts_store::upsert(&state.data_dir, prompt).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(saved)?)
}

async fn cmd_delete_prompt(state: &ServerState, args: Value) -> Result<()> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing `id`"))?;
    prompts_store::delete(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_set_default_prompt(state: &ServerState, args: Value) -> Result<Value> {
    let id = args.get("id").and_then(|v| v.as_str()).map(str::to_string);
    let file = prompts_store::set_default(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(file)?)
}

async fn cmd_rename_session(state: &ServerState, args: Value) -> Result<Value> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing `id`"))?;
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `title`"))?
        .to_string();
    let s = sessions_store::rename(&state.data_dir, id, title).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(s)?)
}

async fn cmd_delete_session(state: &ServerState, args: Value) -> Result<()> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing `id`"))?;
    // 同时从内存里移除 SessionRuntime
    state.sessions.write().await.remove(id);
    sessions_store::delete(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_fork_session(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let up_to = args
        .get("upToMessageId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `upToMessageId`"))?;
    let s = sessions_store::fork(&state.data_dir, sid, up_to).map_err(|e| anyhow!("{e}"))?;
    sessions_dir::ensure_session_dirs(&state.data_dir, &s.id).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(s)?)
}

async fn cmd_truncate_after(state: &ServerState, args: Value) -> Result<Value> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing `id`"))?;
    let mid = args
        .get("messageId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `messageId`"))?;
    let s = sessions_store::truncate_after(&state.data_dir, id, mid)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(s)?)
}

async fn cmd_truncate_inclusive(state: &ServerState, args: Value) -> Result<Value> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing `id`"))?;
    let mid = args
        .get("messageId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `messageId`"))?;
    let s = sessions_store::truncate_inclusive(&state.data_dir, id, mid)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(s)?)
}

async fn cmd_search_sessions(state: &ServerState, args: Value) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `query`"))?;
    let case_sensitive = args.get("caseSensitive").and_then(|v| v.as_bool()).unwrap_or(false);
    let regex = args.get("regex").and_then(|v| v.as_bool()).unwrap_or(false);
    let hits = sessions_store::search(&state.data_dir, query, case_sensitive, regex)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(hits)?)
}

async fn cmd_update_session_config(state: &ServerState, args: Value) -> Result<Value> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing `id`"))?;
    let mut s = sessions_store::load(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    if let Some(p) = args.get("providerId").and_then(|v| v.as_str()) {
        s.provider_id = p.to_string();
    }
    if let Some(m) = args.get("model").and_then(|v| v.as_str()) {
        s.model = m.to_string();
    }
    if let Some(sp) = args.get("systemPrompt").and_then(|v| v.as_str()) {
        s.system_prompt = if sp.is_empty() { None } else { Some(sp.to_string()) };
    }
    if let Some(pid) = args.get("promptId").and_then(|v| v.as_str()) {
        s.prompt_id = if pid.is_empty() { None } else { Some(pid.to_string()) };
    }
    if let Some(stream) = args.get("stream").and_then(|v| v.as_bool()) {
        s.stream = stream;
    }
    let reasoning_val = args.get("reasoning").cloned();
    let clear_reasoning = args.get("clearReasoning").and_then(|v| v.as_bool()).unwrap_or(false);
    if let Some(v) = reasoning_val {
        if !v.is_null() {
            let r: common::ReasoningConfig =
                serde_json::from_value(v).map_err(|e| anyhow!("invalid `reasoning`: {e}"))?;
            s.reasoning = Some(r);
        } else if clear_reasoning {
            s.reasoning = None;
        }
    } else if clear_reasoning {
        s.reasoning = None;
    }
    let saved = sessions_store::save(&state.data_dir, s).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(saved)?)
}

async fn cmd_save_settings(state: &ServerState, args: Value) -> Result<()> {
    let settings: agent_core::storage::settings::Settings =
        serde_json::from_value(args.get("settings").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("missing/invalid `settings`: {e}"))?;
    settings_store::save(&state.data_dir, &settings).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_save_project(state: &ServerState, args: Value) -> Result<Value> {
    let input: agent_core::storage::projects::WorkspaceProjectInput =
        serde_json::from_value(args.get("input").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("missing/invalid `input`: {e}"))?;
    let p = projects_store::save(&state.data_dir, input).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(p)?)
}

async fn cmd_delete_project(state: &ServerState, args: Value) -> Result<()> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing `id`"))?;
    projects_store::delete(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_get_run_mode(state: &ServerState, session_id: Option<String>) -> Result<Value> {
    let sid = need_session(session_id)?;
    let runtime = state.ensure_runtime(&sid).await?;
    let mode = *runtime.run_mode.lock().unwrap();
    Ok(Value::String(mode.as_str().to_string()))
}

async fn cmd_set_run_mode(
    state: &ServerState,
    args: Value,
    session_id: Option<String>,
) -> Result<()> {
    let sid = need_session(session_id)?;
    let mode_str = args
        .get("mode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `mode`"))?;
    let mode = agent_core::run_mode::RunMode::parse(mode_str)
        .ok_or_else(|| anyhow!("invalid mode: {mode_str}"))?;
    let runtime = state.ensure_runtime(&sid).await?;
    *runtime.run_mode.lock().unwrap() = mode;
    // 同步持久化到 session.json
    sessions_store::set_run_mode(&state.data_dir, &sid, mode).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_get_force_automode(
    state: &ServerState,
    session_id: Option<String>,
) -> Result<Value> {
    let sid = need_session(session_id)?;
    let runtime = state.ensure_runtime(&sid).await?;
    Ok(Value::Bool(runtime.force_automode.load(Ordering::SeqCst)))
}

async fn cmd_set_force_automode(
    state: &ServerState,
    args: Value,
    session_id: Option<String>,
) -> Result<()> {
    let sid = need_session(session_id)?;
    // desktop 前端字段是 `enabled`，同时兼容 `value`
    let enabled = args
        .get("enabled")
        .or_else(|| args.get("value"))
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow!("missing `enabled`"))?;
    let runtime = state.ensure_runtime(&sid).await?;
    runtime.force_automode.store(enabled, Ordering::SeqCst);
    Ok(())
}

// ─── 走 LocalCoreClient facade 的命令 ──────────────────────────────────────
//
// 这些命令 desktop 也是一行 `core(&app)?.xxx(...)` 调用——本块让 hebweb 与 desktop
// 共享同一份业务实现，未来 agent_core 给 CoreClient trait 加新方法时 hebweb 自动获得。

fn map_core_err(e: agent_core::core_client::CoreError) -> anyhow::Error {
    anyhow!("{e}")
}

async fn cmd_core_get_provider(state: &ServerState, args: Value) -> Result<Value> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing `id`"))?;
    let p = state.core.get_provider(id).map_err(map_core_err)?;
    Ok(serde_json::to_value(p)?)
}

async fn cmd_core_fetch_provider_models(state: &ServerState, args: Value) -> Result<Value> {
    let provider: model_gateway::config::Provider =
        serde_json::from_value(args.get("provider").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("missing/invalid `provider`: {e}"))?;
    let models = state.core.fetch_provider_models(provider).await.map_err(map_core_err)?;
    Ok(serde_json::to_value(models)?)
}

async fn cmd_core_test_provider_model(state: &ServerState, args: Value) -> Result<Value> {
    let provider: model_gateway::config::Provider =
        serde_json::from_value(args.get("provider").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("missing/invalid `provider`: {e}"))?;
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `model`"))?
        .to_string();
    let result = state.core.test_provider(provider, model).await.map_err(map_core_err)?;
    Ok(serde_json::to_value(result)?)
}

async fn cmd_core_list_tools(state: &ServerState) -> Result<Value> {
    Ok(serde_json::to_value(state.core.list_tools())?)
}

async fn cmd_core_list_permission_rules(state: &ServerState, args: Value) -> Result<Value> {
    let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("session");
    let scope = match scope_str {
        "project" => PermissionScope::Project,
        "global" => PermissionScope::Global,
        _ => PermissionScope::Session,
    };
    let session_id = args.get("sessionId").and_then(|v| v.as_str());
    let rules = state.core.list_permission_rules(scope, session_id);
    Ok(serde_json::to_value(rules)?)
}

async fn cmd_core_remove_permission_rule(state: &ServerState, args: Value) -> Result<Value> {
    let rule_id = args
        .get("ruleId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `ruleId`"))?;
    let session_id = args.get("sessionId").and_then(|v| v.as_str());
    let removed = state
        .core
        .remove_permission_rule(session_id, rule_id)
        .map_err(map_core_err)?;
    Ok(Value::Bool(removed))
}

async fn cmd_core_clear_permission_rules(state: &ServerState, args: Value) -> Result<()> {
    let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("session");
    let scope = match scope_str {
        "project" => PermissionScope::Project,
        "global" => PermissionScope::Global,
        _ => PermissionScope::Session,
    };
    let session_id = args.get("sessionId").and_then(|v| v.as_str());
    state.core.clear_permission_rules(scope, session_id).map_err(map_core_err)?;
    Ok(())
}

// silence Ordering import-unused warning (Ordering used through SessionRuntime methods)
#[allow(dead_code)]
fn _force_ordering_import(_: Ordering) {}
