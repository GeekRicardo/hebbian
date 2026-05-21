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

use crate::bridge::{BridgeClient, BridgeRegistry, ProxyResult};
use crate::protocol::{BridgeInbound, BridgeOutbound, WsClientMessage, WsServerMessage};
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
    /// Tauri 前端 invoke proxy 注册表。bridge 在时，client 的 invoke 优先走 bridge
    /// （等价于 desktop 完整命令集，含 OAuth / EditsWorktree 等）；不在时 fallback
    /// 到 hebweb 自己的 LocalCoreClient 实现（35 个已镜像命令）。
    pub bridges: BridgeRegistry,
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
            bridges: BridgeRegistry::new(),
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
        .route("/ws", get(ws_upgrade))
        .route("/ws/bridge", get(bridge_upgrade));

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
    let bridges = state.bridges.count().await;
    Json(json!({
        "ok": true,
        "version": SERVER_VERSION,
        "data_dir": state.data_dir.display().to_string(),
        "active_sessions": active,
        "bridges": bridges,
    }))
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn bridge_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_bridge(socket, state))
}

// ─── bridge 端 WS 处理 ─────────────────────────────────────────────────────

async fn handle_bridge(socket: WebSocket, state: ServerState) {
    let (mut sender, mut receiver) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<BridgeOutbound>();

    // 发送任务：BridgeOutbound → ws text
    let send_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let Ok(text) = serde_json::to_string(&msg) else { continue };
            if sender.send(WsMessage::Text(text)).await.is_err() {
                break;
            }
        }
    });

    // 等首条 register
    let label = match receiver.next().await {
        Some(Ok(WsMessage::Text(t))) => match serde_json::from_str::<BridgeInbound>(&t) {
            Ok(BridgeInbound::Register { client_label }) => client_label,
            _ => {
                info!("bridge: 首条不是 register，关闭");
                return;
            }
        },
        _ => return,
    };

    let bridge = Arc::new(BridgeClient::new(label.clone(), out_tx.clone()));
    state.bridges.register(bridge.clone()).await;
    info!(label = %label, "bridge registered");
    let _ = out_tx.send(BridgeOutbound::Welcome { server_version: SERVER_VERSION });

    // 消费 bridge 上来的 ProxyResponse，唤醒对应 pending oneshot
    let pending = bridge.pending();
    while let Some(Ok(msg)) = receiver.next().await {
        let text = match msg {
            WsMessage::Text(t) => t,
            WsMessage::Close(_) => break,
            _ => continue,
        };
        let Ok(parsed) = serde_json::from_str::<BridgeInbound>(&text) else {
            warn!("bridge: 收到非法消息 {text}");
            continue;
        };
        match parsed {
            BridgeInbound::ProxyResponse { req_id, ok, data, error } => {
                if let Some(tx) = pending.lock().await.remove(&req_id) {
                    let _ = tx.send(ProxyResult { ok, data, error });
                } else {
                    warn!(req_id = %req_id, "bridge: ProxyResponse 没有对应 pending");
                }
            }
            BridgeInbound::ChannelEvent { req_id: _, session_id, payload } => {
                // 流式事件转发：找到 / 创建对应 SessionRuntime，把 payload 当作
                // 已序列化的 engine-event 广播出去。所有订阅该 session 的 ws 都会收到。
                match state.ensure_runtime(&session_id).await {
                    Ok(runtime) => runtime.broadcast(WsServerMessage::Event {
                        session_id: session_id.clone(),
                        name: "engine-event".to_string(),
                        payload,
                    }),
                    Err(e) => warn!(session_id = %session_id, error = %e, "bridge ChannelEvent: ensure_runtime 失败"),
                }
            }
            BridgeInbound::Register { .. } => {} // 注册阶段已处理，后续 ignore
        }
    }

    info!(label = %label, "bridge disconnected");
    state.bridges.unregister(&label).await;
    drop(out_tx);
    let _ = send_task.await;
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
    // Step 2：bridge 在场就把**所有** invoke 都走 bridge——desktop 那边有完整的
    // SessionContext + HitlState + chat 管线，能跑 send_message / approve / answer 等
    // 流式对话命令。channel 事件通过 BridgeInbound::ChannelEvent 路径回流到 hebweb，
    // 按 session_id broadcast 给所有订阅该 session 的 ws。
    //
    // bridge 不在场时 fallback 到 hebweb 自己镜像的 35 个命令 + 本地 SessionRuntime。
    if let Some(bridge) = state.bridges.pick().await {
        // 完全透传 args 给 desktop——前端 tauri.ts 已经按 desktop Tauri command
        // 签名传了对应字段；这里如果擅自补 sessionId 会污染那些不需要 session 的命令
        // （例如 list_background_tasks），desktop 端会报 "invalid args".
        let _ = session_id;
        match bridge.proxy_invoke(cmd.to_string(), args.clone()).await {
            Ok(res) if res.ok => return Ok(res.data),
            Ok(res) => return Err(anyhow!("{}", res.error.unwrap_or_default())),
            Err(e) => {
                warn!(cmd = %cmd, error = %e, "bridge proxy 失败，fallback 到 hebweb 本地实现");
            }
        }
    }

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
        "list_permissions" => cmd_core_list_permissions(state, args).await.map(Some),
        "add_permission" => cmd_core_add_permission(state, args).await.map(|_| None),
        "remove_permission" => cmd_core_remove_permission(state, args).await.map(Some),
        "clear_permissions" => cmd_core_clear_permissions(state, args).await.map(|_| None),
        "list_permission_paths" => cmd_core_list_permission_paths(state, args).await.map(Some),
        "add_permission_path" => cmd_core_add_permission_path(state, args).await.map(|_| None),
        "remove_permission_path" => {
            cmd_core_remove_permission_path(state, args).await.map(Some)
        }
        "list_skills" => cmd_core_list_skills(state, args).await.map(Some),
        "list_claude_skills" => cmd_core_list_claude_skills(state).await.map(Some),
        "import_claude_skills" => cmd_core_import_claude_skills(state, args).await.map(Some),
        "import_skills_from_dir" => {
            cmd_core_import_skills_from_dir(state, args).await.map(Some)
        }
        "import_skills_from_github" => {
            cmd_core_import_skills_from_github(state, args).await.map(Some)
        }
        "scan_skill_dir" => cmd_core_scan_skill_dir(state, args).await.map(Some),
        "scan_skill_github" => cmd_core_scan_skill_github(state, args).await.map(Some),
        "set_skill_enabled" => cmd_core_set_skill_enabled(state, args).await.map(|_| None),
        "delete_skill" => cmd_core_delete_skill(state, args).await.map(Some),
        // ─── Round 1 standalone helpers（复刻 desktop chat / title_gen，不依赖 bridge）
        "compact_session" => cmd_compact_session(state, args).await.map(Some),
        "get_context_usage" => cmd_get_context_usage(state, args).await.map(Some),
        "generate_session_title" => cmd_generate_session_title(state, args).await.map(Some),
        "discover_rules_files" => cmd_discover_rules_files(args).await.map(Some),
        "list_background_tasks" => cmd_list_background_tasks_local(args).await.map(Some),
        "kill_background_task" => cmd_kill_background_task_local(args).await.map(Some),
        "update_session_settings" => {
            cmd_update_session_settings(state, args).await.map(Some)
        }
        // 其余 desktop Tauri command 在 v1 浏览器 surface 暂不实现
        // OAuth 系列、edits diff/revert、preview_payload 等需要 desktop bridge
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

// 注：原 Step 1 的 `is_local_runtime_command` 隔离名单已删除——Step 2 让 bridge 在场时
// 全部命令都走 bridge，包括 send_message（channel 事件回流由 BridgeInbound::ChannelEvent
// 路由）。bridge 不在场时所有命令仍 fallback 到 hebweb 本地实现。

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

fn parse_scope(s: &str) -> PermissionScope {
    match s {
        "project" => PermissionScope::Project,
        "global" => PermissionScope::Global,
        "once" => PermissionScope::Once,
        _ => PermissionScope::Session,
    }
}

fn parse_effect(s: &str) -> agent_core::permissions::RuleEffect {
    match s {
        "deny" => agent_core::permissions::RuleEffect::Deny,
        _ => agent_core::permissions::RuleEffect::Allow,
    }
}

async fn cmd_core_list_permissions(state: &ServerState, args: Value) -> Result<Value> {
    let scope = parse_scope(args.get("scope").and_then(|v| v.as_str()).unwrap_or("global"));
    let effect = parse_effect(args.get("effect").and_then(|v| v.as_str()).unwrap_or("allow"));
    let session_id = args.get("sessionId").and_then(|v| v.as_str());
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let list = state
        .core
        .list_permissions(scope, session_id, workdir.as_deref(), effect);
    Ok(serde_json::to_value(list)?)
}

async fn cmd_core_add_permission(state: &ServerState, args: Value) -> Result<()> {
    let scope = parse_scope(args.get("scope").and_then(|v| v.as_str()).unwrap_or("global"));
    let effect = parse_effect(args.get("effect").and_then(|v| v.as_str()).unwrap_or("allow"));
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `pattern`"))?
        .to_string();
    let session_id = args.get("sessionId").and_then(|v| v.as_str());
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    state
        .core
        .add_permission(scope, session_id, workdir.as_deref(), effect, pattern)
        .map_err(map_core_err)?;
    Ok(())
}

async fn cmd_core_remove_permission(state: &ServerState, args: Value) -> Result<Value> {
    let scope = parse_scope(args.get("scope").and_then(|v| v.as_str()).unwrap_or("global"));
    let effect = parse_effect(args.get("effect").and_then(|v| v.as_str()).unwrap_or("allow"));
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `pattern`"))?;
    let session_id = args.get("sessionId").and_then(|v| v.as_str());
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let removed = state
        .core
        .remove_permission(scope, session_id, workdir.as_deref(), effect, pattern)
        .map_err(map_core_err)?;
    Ok(Value::Bool(removed))
}

async fn cmd_core_clear_permissions(state: &ServerState, args: Value) -> Result<()> {
    let scope = parse_scope(args.get("scope").and_then(|v| v.as_str()).unwrap_or("global"));
    let session_id = args.get("sessionId").and_then(|v| v.as_str());
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    state
        .core
        .clear_permissions(scope, session_id, workdir.as_deref())
        .map_err(map_core_err)?;
    Ok(())
}

async fn cmd_core_list_permission_paths(state: &ServerState, args: Value) -> Result<Value> {
    let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("global");
    let scope = match scope_str {
        "project" => PermissionScope::Project,
        "session" => PermissionScope::Session,
        _ => PermissionScope::Global,
    };
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let paths = state.core.list_permission_paths(scope, workdir.as_deref());
    Ok(serde_json::to_value(paths)?)
}

async fn cmd_core_add_permission_path(state: &ServerState, args: Value) -> Result<()> {
    let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("global");
    let scope = match scope_str {
        "project" => PermissionScope::Project,
        _ => PermissionScope::Global,
    };
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `path`"))?;
    state
        .core
        .add_permission_path(scope, workdir.as_deref(), std::path::PathBuf::from(path))
        .map_err(map_core_err)?;
    Ok(())
}

async fn cmd_core_remove_permission_path(state: &ServerState, args: Value) -> Result<Value> {
    let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("global");
    let scope = match scope_str {
        "project" => PermissionScope::Project,
        _ => PermissionScope::Global,
    };
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `path`"))?;
    let removed = state
        .core
        .remove_permission_path(scope, workdir.as_deref(), std::path::Path::new(path))
        .map_err(map_core_err)?;
    Ok(Value::Bool(removed))
}

async fn cmd_core_list_skills(state: &ServerState, args: Value) -> Result<Value> {
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.data_dir.clone());
    let skills = state.core.list_skills(&workdir);
    Ok(serde_json::to_value(skills)?)
}

async fn cmd_core_list_claude_skills(state: &ServerState) -> Result<Value> {
    Ok(serde_json::to_value(state.core.list_claude_skills())?)
}

async fn cmd_core_import_claude_skills(state: &ServerState, args: Value) -> Result<Value> {
    use agent_core::storage::skills::ImportScope;
    let scope = match args.get("scope").and_then(|v| v.as_str()).unwrap_or("global") {
        "project" => ImportScope::Project,
        _ => ImportScope::Global,
    };
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let names: Option<Vec<String>> = args
        .get("names")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());
    let overwrite = args.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);
    let imported = state
        .core
        .import_claude_skills(scope, workdir.as_deref(), names.as_deref(), overwrite)
        .map_err(map_core_err)?;
    Ok(serde_json::to_value(imported)?)
}

async fn cmd_core_import_skills_from_dir(state: &ServerState, args: Value) -> Result<Value> {
    use agent_core::storage::skills::ImportScope;
    let scope = match args.get("scope").and_then(|v| v.as_str()).unwrap_or("global") {
        "project" => ImportScope::Project,
        _ => ImportScope::Global,
    };
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let src_dir = args
        .get("srcDir")
        .or_else(|| args.get("src_dir"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `srcDir`"))?;
    let overwrite = args.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(true);
    let selected: Option<Vec<String>> = args
        .get("selectedPaths")
        .or_else(|| args.get("selected_paths"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());
    let imported = state
        .core
        .import_skills_from_dir(
            scope,
            workdir.as_deref(),
            std::path::Path::new(src_dir),
            selected.as_deref(),
            overwrite,
        )
        .map_err(map_core_err)?;
    Ok(serde_json::to_value(imported)?)
}

async fn cmd_core_import_skills_from_github(state: &ServerState, args: Value) -> Result<Value> {
    use agent_core::storage::skills::ImportScope;
    let scope = match args.get("scope").and_then(|v| v.as_str()).unwrap_or("global") {
        "project" => ImportScope::Project,
        _ => ImportScope::Global,
    };
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let repo_url = args
        .get("repoUrl")
        .or_else(|| args.get("repo_url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `repoUrl`"))?;
    let subpath = args
        .get("subpath")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let overwrite = args.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(true);
    let selected: Option<Vec<String>> = args
        .get("selectedPaths")
        .or_else(|| args.get("selected_paths"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());
    let imported = state
        .core
        .import_skills_from_github(
            scope,
            workdir.as_deref(),
            repo_url,
            subpath,
            selected.as_deref(),
            overwrite,
        )
        .map_err(map_core_err)?;
    Ok(serde_json::to_value(imported)?)
}

async fn cmd_core_scan_skill_dir(state: &ServerState, args: Value) -> Result<Value> {
    let src_dir = args
        .get("srcDir")
        .or_else(|| args.get("src_dir"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `srcDir`"))?;
    let scanned = state
        .core
        .scan_skill_dir(std::path::Path::new(src_dir))
        .map_err(map_core_err)?;
    Ok(serde_json::to_value(scanned)?)
}

async fn cmd_core_scan_skill_github(state: &ServerState, args: Value) -> Result<Value> {
    let repo_url = args
        .get("repoUrl")
        .or_else(|| args.get("repo_url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `repoUrl`"))?;
    let subpath = args
        .get("subpath")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let scanned = state
        .core
        .scan_skill_github(repo_url, subpath)
        .map_err(map_core_err)?;
    Ok(serde_json::to_value(scanned)?)
}

async fn cmd_core_set_skill_enabled(state: &ServerState, args: Value) -> Result<()> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `name`"))?;
    let enabled = args
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow!("missing `enabled`"))?;
    state
        .core
        .set_skill_enabled(name, enabled)
        .map_err(map_core_err)?;
    Ok(())
}

async fn cmd_core_delete_skill(state: &ServerState, args: Value) -> Result<Value> {
    use agent_core::tools::skill::SkillSource;
    let source = match args.get("source").and_then(|v| v.as_str()).unwrap_or("global") {
        "project" => SkillSource::Project,
        "project_code" => SkillSource::ProjectCode,
        _ => SkillSource::Global,
    };
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `name`"))?;
    let removed = state
        .core
        .delete_skill(source, workdir.as_deref(), name)
        .map_err(map_core_err)?;
    Ok(Value::Bool(removed))
}

// silence Ordering import-unused warning (Ordering used through SessionRuntime methods)
#[allow(dead_code)]
fn _force_ordering_import(_: Ordering) {}

// ─── Round 1 standalone command handlers ───────────────────────────────────
// 让 hebweb 不需要 desktop bridge 也能镜像这些命令——复刻自 desktop lib.rs + chat.rs。
// bridge 在场时这些分支不会进（dispatch 优先 bridge）；不在场时它们让 hebweb 独立完整。

async fn cmd_get_context_usage(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let usage = crate::chat_helpers::context_usage(&state.data_dir, sid).await?;
    Ok(serde_json::to_value(usage)?)
}

async fn cmd_compact_session(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let custom = args
        .get("customInstructions")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let usage = crate::chat_helpers::compact_session(&state.data_dir, sid, custom).await?;
    Ok(serde_json::to_value(usage)?)
}

async fn cmd_generate_session_title(state: &ServerState, args: Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let mut session = sessions_store::load(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    let has_user = session
        .messages
        .iter()
        .any(|m| matches!(m.role, agent_core::storage::sessions::Role::User));
    if !has_user {
        return Ok(serde_json::to_value(session)?);
    }

    // 优先用 ProvidersFile 中标记为「标题生成 model」的 provider，否则回退到 session 自己的
    let providers_file = model_gateway::config::load(&state.data_dir).map_err(|e| anyhow!("{e}"))?;
    let title_provider = providers_file.providers.into_iter().find(|p| {
        p.enabled
            && p.title_gen_enabled
            && p.title_gen_model.as_deref().is_some_and(|m| !m.is_empty())
    });
    let (provider, model) = match title_provider {
        Some(p) => {
            let m = p.title_gen_model.clone().unwrap_or_default();
            (p, m)
        }
        None => (
            model_gateway::config::get(&state.data_dir, &session.provider_id)
                .map_err(|e| anyhow!("{e}"))?,
            session.model.clone(),
        ),
    };
    let title = crate::chat_helpers::try_generate_title(
        &state.data_dir,
        provider,
        &model,
        &session.messages,
    )
    .await
    .unwrap_or_else(|| crate::chat_helpers::fallback_from_first_user(&session.messages));
    session = sessions_store::rename(&state.data_dir, id, title).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(session)?)
}

async fn cmd_discover_rules_files(args: Value) -> Result<Value> {
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing `workdir`"))?;
    let allowed: Vec<PathBuf> = args
        .get("allowedPaths")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect())
        .unwrap_or_default();
    let files = agent_core::rules::discover(&workdir, &allowed);
    // 与 desktop RuleFileInfo 同结构：{ path, source }
    let dto: Vec<Value> = files
        .into_iter()
        .map(|f| {
            json!({
                "path": f.path.display().to_string(),
                "source": f.source,
            })
        })
        .collect();
    Ok(Value::Array(dto))
}

async fn cmd_list_background_tasks_local(args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let shells_registry = agent_core::tools::background::registry_for_session(sid);
    // 只暴露真后台任务（语义同 desktop 端 list_background_tasks）：
    // 前台命令跑完会被 BashTool 直接 unregister，is_background=false 的瞬时残留 surface 不展示。
    let shells: Vec<Value> = shells_registry
        .list()
        .into_iter()
        .filter(|s| s.is_background())
        .map(|s| {
            json!({
                "task_id": s.task_id,
                "state": s.state().label().to_string(),
                "command": s.command,
                "cwd": s.cwd,
                "elapsed_secs": s.started_at.elapsed().as_secs(),
                "log_path": s.log_path().map(|p| p.display().to_string()),
            })
        })
        .collect();
    let pending_crons = agent_core::wakeup::WakeupScheduler::global().list_pending_crons(sid);
    Ok(json!({
        "shells": shells,
        "pending_crons": pending_crons,
        "has_suspended_checkpoint": false,  // run_checkpoint 在 hebweb 未启用，恒 false
    }))
}

async fn cmd_kill_background_task_local(args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let task_id = args
        .get("taskId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `taskId`"))?;
    let shells = agent_core::tools::background::registry_for_session(sid);
    match shells.kill(task_id).await {
        Some(state) => Ok(Value::String(state.label().to_string())),
        None => Err(anyhow!("未找到 task_id={task_id}（可能已被清理）")),
    }
}

async fn cmd_update_session_settings(state: &ServerState, args: Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let mut s = sessions_store::load(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;

    let take_path = |key: &str| -> Option<PathBuf> {
        args.get(key).and_then(|v| v.as_str()).map(PathBuf::from)
    };
    let take_paths = |key: &str| -> Option<Vec<PathBuf>> {
        args.get(key).and_then(|v| v.as_array()).map(|arr| {
            arr.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect()
        })
    };
    let take_strs = |key: &str| -> Option<Vec<String>> {
        args.get(key).and_then(|v| v.as_array()).map(|arr| {
            arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        })
    };
    let take_bool = |key: &str| -> bool {
        args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
    };

    if take_bool("clearWorkdir") {
        s.workdir = None;
    } else if let Some(v) = take_path("workdir") {
        s.workdir = Some(v);
    }
    if take_bool("clearAllowedPaths") {
        s.allowed_paths = None;
    } else if let Some(v) = take_paths("allowedPaths") {
        s.allowed_paths = Some(v);
    }
    if take_bool("clearEnabledTools") {
        s.enabled_tools = None;
    } else if let Some(v) = take_strs("enabledTools") {
        s.enabled_tools = Some(v);
    }
    if take_bool("clearSkillDirs") {
        s.skill_dirs = None;
    } else if let Some(v) = take_paths("skillDirs") {
        s.skill_dirs = Some(v);
    }
    if take_bool("clearGlobalRules") {
        s.global_rules = None;
    } else if let Some(v) = take_paths("globalRules") {
        s.global_rules = Some(v);
    }
    if take_bool("clearRulesFiles") {
        s.rules_files = None;
    } else if let Some(v) = args.get("rulesFiles").cloned() {
        if !v.is_null() {
            let parsed: Vec<agent_core::rules::RuleFileState> =
                serde_json::from_value(v).map_err(|e| anyhow!("invalid `rulesFiles`: {e}"))?;
            s.rules_files = Some(parsed);
        }
    }
    let saved = sessions_store::save(&state.data_dir, s).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(saved)?)
}
