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
use std::sync::{atomic::Ordering, Arc};

use agent_core::{
    core_client::{CoreClient, LocalCoreClient},
    edits::{self, EditsWorktree},
    permissions::PermissionStore,
    storage::{
        projects as projects_store, prompts as prompts_store, sessions as sessions_store,
        sessions_dir, settings as settings_store,
    },
    workspace::Workspace,
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
use tokio::sync::{mpsc, RwLock};
use tower_http::services::ServeDir;
use tracing::{info, warn};

use crate::protocol::{WsClientMessage, WsServerMessage};
use surface_session::{run_turn, SessionRuntime};

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
    /// 旁支对话内存状态（与 desktop 共用 agent_core::branch 的 BranchEngine）。
    pub branches: Arc<agent_core::branch::BranchState>,
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
            branches: Arc::new(agent_core::branch::BranchState::new()),
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
        let run_mode = session.run_mode;

        let state_rt = agent_core::session_hub::SessionRuntimeState::new(
            session_id,
            EVENT_CHANNEL_CAPACITY,
            run_mode,
        );
        let runtime = Arc::new(SessionRuntime {
            session_id: session_id.to_string(),
            data_dir: self.data_dir.clone(),
            provider_id: session.provider_id.clone(),
            model: session.model.clone(),
            reasoning: None,
            input_tx,
            permission_store: self.permission_store.clone(),
            state: state_rt,
        });

        // 启动 turn 主循环：依次处理 input_rx 的每条输入
        let rt_for_loop = runtime.clone();
        tokio::spawn(async move {
            while let Some(text) = input_rx.recv().await {
                if let Err(e) = run_turn(rt_for_loop.clone(), text).await {
                    rt_for_loop.emit_engine_event(protocol::WireEvent::Error {
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

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<ServerState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

// ─── 单 WS 连接 ────────────────────────────────────────────────────────────

async fn handle_ws(socket: WebSocket, state: ServerState) {
    let (mut sender, mut receiver) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<WsServerMessage>();

    // 发送任务：把 WsServerMessage 序列化后写到 ws
    let send_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let Ok(text) = serde_json::to_string(&msg) else {
                continue;
            };
            if sender.send(WsMessage::Text(text)).await.is_err() {
                break;
            }
        }
    });

    // 握手
    let _ = out_tx.send(WsServerMessage::Hello {
        server_version: SERVER_VERSION,
    });

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
                        let mut rx = runtime.state.subscribe();
                        let tx = out_tx.clone();
                        let sid = session_id.clone();
                        event_task = Some(tokio::spawn(async move {
                            // broadcast 通道走通用 WireEvent（§7.8.5）；WS 层在这里包成
                            // 浏览器协议 WsServerMessage::Event（engine-event）。
                            while let Ok(ev) = rx.recv().await {
                                let payload = match serde_json::to_value(&ev) {
                                    Ok(v) => v,
                                    Err(_) => continue,
                                };
                                let msg = WsServerMessage::Event {
                                    session_id: sid.clone(),
                                    name: "engine-event".to_string(),
                                    payload,
                                };
                                if tx.send(msg).is_err() {
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
            WsClientMessage::Invoke {
                id,
                cmd,
                args,
                session_id,
            } => {
                let state = state.clone();
                let tx = out_tx.clone();
                tokio::spawn(async move {
                    let response = dispatch_invoke(&state, &cmd, args, session_id).await;
                    let msg = match response {
                        Ok(data) => WsServerMessage::ok(id, data),
                        Err(e) => WsServerMessage::err(id, e),
                    };
                    let _ = tx.send(msg);
                });
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
    let _ = session_id;
    match cmd {
        // 核心交互
        "list_sessions" => cmd_list_sessions(state).await.map(Some),
        "get_session" => cmd_get_session(state, args).await.map(Some),
        "create_session" => cmd_create_session(state, args).await.map(Some),
        "send_message" => cmd_send_message(state, args, session_id)
            .await
            .map(|_| None),
        "inject_user_message" => cmd_inject_user_message(state, args, session_id)
            .await
            .map(|_| None),
        "approve_permission" => cmd_approve_permission(state, args, session_id)
            .await
            .map(|_| None),
        "answer_question" => cmd_answer_question(state, args, session_id)
            .await
            .map(|_| None),
        "cancel_message" => cmd_cancel_message(state, session_id).await.map(|_| None),
        // 旁支对话（与 desktop 共用 agent_core::branch；事件经绑定主对话的 WS 推送）
        "branch_create" => cmd_branch_create(state, args).await.map(Some),
        "branch_send" => cmd_branch_send(state, args).await.map(Some),
        "branch_discard" => {
            cmd_branch_discard(state, args);
            Ok(None)
        }
        "branch_cancel" => {
            cmd_branch_cancel(state, args);
            Ok(None)
        }
        // parity：subagent / mcp / hooks / skill_collection / plugin（全委托 state.core）
        "list_subagents" => cmd_list_subagents(state, args).await.map(Some),
        "get_subagent" => cmd_get_subagent(state, args).await.map(Some),
        "save_subagent" => cmd_save_subagent(state, args).await.map(|_| None),
        "delete_subagent" => cmd_delete_subagent(state, args).await.map(|_| None),
        "set_subagent_enabled" => cmd_set_subagent_enabled(state, args).await.map(|_| None),
        "load_subagent_run" => cmd_load_subagent_run(state, args).await.map(Some),
        "get_mcp_config" => cmd_get_mcp_config(state).await.map(Some),
        "save_mcp_config" => cmd_save_mcp_config(state, args).await.map(|_| None),
        "discover_mcp_tools" => cmd_discover_mcp_tools(state).await.map(Some),
        "get_hooks_raw" => cmd_get_hooks_raw(state).await.map(Some),
        "save_hooks_raw" => cmd_save_hooks_raw(state, args).await.map(|_| None),
        "list_skill_collections" => cmd_list_skill_collections(state).await.map(Some),
        "delete_skill_collection" => cmd_delete_skill_collection(state, args).await.map(Some),
        "plugin_marketplace_add" => cmd_plugin_marketplace_add(state, args).await.map(Some),
        "plugin_marketplace_list" => cmd_plugin_marketplace_list(state).await.map(Some),
        "plugin_marketplace_list_plugins" => {
            cmd_plugin_marketplace_list_plugins(state, args).await.map(Some)
        }
        "plugin_marketplace_remove" => cmd_plugin_marketplace_remove(state, args).await.map(|_| None),
        "plugin_install" => cmd_plugin_install(state, args).await.map(Some),
        "plugin_uninstall" => cmd_plugin_uninstall(state, args).await.map(|_| None),
        "plugin_list" => cmd_plugin_list(state).await.map(Some),
        // parity：goal / todos / plan / model_io / import / 杂项（直接读 agent_core::storage）
        "list_todos" => cmd_list_todos(state, args).await.map(Some),
        "get_active_goal" => cmd_get_active_goal(state, args).await.map(Some),
        "set_active_goal" => cmd_set_active_goal(state, args).await.map(|_| None),
        "clear_active_goal" => cmd_clear_active_goal(state, args).await.map(|_| None),
        "undo_compaction" => cmd_undo_compaction(state, args).await.map(Some),
        "list_session_plans" => cmd_list_session_plans(state, args).await.map(Some),
        "read_plan_markdown" => cmd_read_plan_markdown(state, args).await.map(Some),
        "update_plan_markdown" => cmd_update_plan_markdown(state, args).await.map(|_| None),
        "list_plan_comments" => cmd_list_plan_comments(state, args).await.map(Some),
        "add_plan_comment" => cmd_add_plan_comment(state, args).await.map(Some),
        "read_skill_md" => cmd_read_skill_md(state, args).await.map(Some),
        "import_project_file" => cmd_import_project_file(state, args).await.map(Some),
        "switch_provider_model" => cmd_switch_provider_model(state, args).await.map(Some),
        "fetch_provider_usage" => cmd_fetch_provider_usage(state, args).await.map(Some),
        "export_session_to_claude" => cmd_export_session_to_claude(state, args).await.map(Some),
        "discover_all_rules" => cmd_discover_all_rules(state, args).await.map(Some),
        // parity：路径访问审批 / 粘贴拖拽 / payload 预览（与 desktop 同实现，委托 agent_core）
        "approve_path_access" => cmd_approve_path_access(state, args, session_id)
            .await
            .map(|_| None),
        "attach_path" => cmd_attach_path(args).await.map(Some),
        "drop_paths" => cmd_drop_paths(args).await.map(Some),
        "preview_session_payload" => cmd_preview_session_payload(state, args).await.map(Some),
        // parity：OAuth / deepseek 登录 / 日志（纯 model_gateway::auth + fs，浏览器 surface 天然支持）
        "oauth_codex_start" => cmd_oauth_codex_start().await.map(Some),
        "oauth_codex_poll" => cmd_oauth_codex_poll(args).await.map(Some),
        "oauth_codex_refresh" => cmd_oauth_codex_refresh(args).await.map(Some),
        "oauth_openai_start" => cmd_oauth_openai_start().await.map(Some),
        "oauth_openai_exchange" => cmd_oauth_openai_exchange(args).await.map(Some),
        "oauth_claude_start" => cmd_oauth_claude_start().await.map(Some),
        "oauth_claude_exchange" => cmd_oauth_claude_exchange(args).await.map(Some),
        "oauth_claude_refresh" => cmd_oauth_claude_refresh(args).await.map(Some),
        "oauth_claude_code_import" => cmd_oauth_claude_code_import().await.map(Some),
        "oauth_gemini_start" => cmd_oauth_gemini_start().await.map(Some),
        "oauth_gemini_exchange" => cmd_oauth_gemini_exchange(args).await.map(Some),
        "oauth_gemini_refresh" => cmd_oauth_gemini_refresh(args).await.map(Some),
        "oauth_gemini_cli_import" => cmd_oauth_gemini_cli_import().await.map(Some),
        "deepseek_login" => cmd_deepseek_login(args).await.map(Some),
        "read_log_file" => cmd_read_log_file().await.map(Some),
        "list_claude_sessions" => cmd_list_claude_sessions(state).await.map(Some),
        "import_claude_session" => cmd_import_claude_session(state, args).await.map(Some),
        "import_vscode_project" => cmd_import_vscode_project(state, args).await.map(Some),
        "refresh_models_catalog" => cmd_refresh_models_catalog(state).await.map(Some),
        // 前端 init 必需的只读命令（直接读 ~/.hebbian/ 文件）
        "get_providers" => cmd_get_providers(state).await.map(Some),
        "list_provider_presets" => cmd_list_provider_presets().await.map(Some),
        "list_prompts" => cmd_list_prompts(state).await.map(Some),
        "list_projects" => cmd_list_projects(state).await.map(Some),
        "get_settings" => cmd_get_settings(state).await.map(Some),
        "list_memories" => cmd_list_memories(state, args).await.map(Some),
        "read_memory" => cmd_read_memory(state, args).await.map(Some),
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
        "set_run_mode" => cmd_set_run_mode(state, args, session_id)
            .await
            .map(|_| None),
        "get_force_automode" => cmd_get_force_automode(state, session_id).await.map(Some),
        "set_force_automode" => cmd_set_force_automode(state, args, session_id)
            .await
            .map(|_| None),
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
        "add_permission_path" => cmd_core_add_permission_path(state, args)
            .await
            .map(|_| None),
        "remove_permission_path" => cmd_core_remove_permission_path(state, args).await.map(Some),
        "list_skills" => cmd_core_list_skills(state, args).await.map(Some),
        "list_claude_skills" => cmd_core_list_claude_skills(state).await.map(Some),
        "import_claude_skills" => cmd_core_import_claude_skills(state, args).await.map(Some),
        "import_skills_from_dir" => cmd_core_import_skills_from_dir(state, args).await.map(Some),
        "import_skills_from_github" => cmd_core_import_skills_from_github(state, args)
            .await
            .map(Some),
        "scan_skill_dir" => cmd_core_scan_skill_dir(state, args).await.map(Some),
        "scan_skill_github" => cmd_core_scan_skill_github(state, args).await.map(Some),
        "set_skill_enabled" => cmd_core_set_skill_enabled(state, args).await.map(|_| None),
        "delete_skill" => cmd_core_delete_skill(state, args).await.map(Some),
        // ─── 复刻 desktop chat / title_gen 的 standalone helpers
        "compact_session" => cmd_compact_session(state, args).await.map(Some),
        "get_context_usage" => cmd_get_context_usage(state, args).await.map(Some),
        "get_models_catalog" => cmd_get_models_catalog(state).await.map(Some),
        "generate_session_title" => cmd_generate_session_title(state, args).await.map(Some),
        "discover_rules_files" => cmd_discover_rules_files(args).await.map(Some),
        "list_background_tasks" => cmd_list_background_tasks_local(args).await.map(Some),
        "kill_background_task" => cmd_kill_background_task_local(args).await.map(Some),
        "read_background_task_output" => cmd_read_background_task_output_local(args).await.map(Some),
        "update_session_settings" => cmd_update_session_settings(state, args).await.map(Some),
        "list_session_model_io" => cmd_list_session_model_io(state, args).await.map(Some),
        "get_session_model_io_entry" => cmd_get_session_model_io_entry(state, args).await.map(Some),
        // Edits Worktree（架构 §4.13）
        "list_edits" => cmd_list_edits(state, args).await.map(Some),
        "diff_edit" => cmd_diff_edit(state, args).await.map(Some),
        "read_text_file" => cmd_read_text_file(args).await.map(Some),
        "read_dir" => cmd_read_dir(args).await.map(Some),
        "write_text_file" => cmd_write_text_file(args).await.map(|_| None),
        "revert_edit" => cmd_revert_edit(state, args).await.map(Some),
        "edits_worktree_status" => cmd_edits_worktree_status(state, args).await.map(Some),
        // 其余 desktop Tauri command（OAuth 14 / preview_payload / file dialog / ...）
        // 在 hebweb 浏览器 surface 尚未镜像；需要时按 Round 1 模式照搬 desktop 实现
        other => Err(anyhow!(
            "command `{other}` not implemented in hebweb (mirror from desktop lib.rs when needed)"
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
    sessions_dir::ensure_session_dirs(&state.data_dir, &session.id).map_err(|e| anyhow!("{e}"))?;
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

// ─── 旁支对话（branch）────────────────────────────────────────────────────────
//
// 业务逻辑全在 agent_core::branch::BranchEngine（与 desktop 同一份）。hebweb 这层只做：
// 解析 args、用 ServerState 的 data_dir + 共享 BranchState 组装 engine、把引擎产出的
// WireEvent 经绑定主对话的 WS 广播推给订阅者（前端按 session_id 收 engine-event）。

fn branch_engine(state: &ServerState) -> agent_core::branch::BranchEngine {
    agent_core::branch::BranchEngine::with_state(state.data_dir.clone(), state.branches.clone())
}

async fn cmd_branch_create(state: &ServerState, args: Value) -> Result<Value> {
    let session_id = args
        .get("sessionId")
        .or_else(|| args.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?
        .to_string();
    let up_to = args
        .get("upToMessageId")
        .or_else(|| args.get("up_to_message_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let info = branch_engine(state)
        .create(session_id, up_to)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(info)?)
}

fn cmd_branch_discard(state: &ServerState, args: Value) {
    if let Some(branch_id) = args.get("branchId").or_else(|| args.get("branch_id")).and_then(|v| v.as_str()) {
        state.branches.discard(branch_id);
    }
}

fn cmd_branch_cancel(state: &ServerState, args: Value) {
    if let Some(branch_id) = args.get("branchId").or_else(|| args.get("branch_id")).and_then(|v| v.as_str()) {
        state.branches.cancel(branch_id);
    }
}

async fn cmd_branch_send(state: &ServerState, args: Value) -> Result<Value> {
    let branch_id = args
        .get("branchId")
        .or_else(|| args.get("branch_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `branchId`"))?
        .to_string();
    let content = pick_text(&args)?;
    let attachments = args
        .get("attachments")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let provider_id = args
        .get("providerId")
        .or_else(|| args.get("provider_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let model = args.get("model").and_then(|v| v.as_str()).map(str::to_string);

    // 事件出口：把 WireEvent 经绑定主对话的 WS 广播推给前端（与主对话 engine-event 同通道）。
    // branch_id 形如 "branch-<sid>"——但事件要落到绑定的主对话 session 上，故先建 create 时
    // 已记住绑定关系；这里取 send 返回前需要的 bound session 由引擎内部持有，事件路由用
    // bound_session_id。前端 subscribe 的是主对话 session_id，故这里需要拿到它。
    let bound_session_id = state
        .branches
        .bound_session_of(&branch_id)
        .ok_or_else(|| anyhow!("这条旁支对话已经关掉了"))?;
    let runtime = state.ensure_runtime(&bound_session_id).await?;
    let emit = move |wire: protocol::WireEvent| {
        runtime.emit_engine_event(wire);
    };

    let assistant = branch_engine(state)
        .send(branch_id, content, attachments, provider_id, model, emit)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(assistant)?)
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
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let feedback = args
        .get("feedback")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let extra_patterns: Vec<String> = args
        .get("extraPatterns")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
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
            feedback: feedback.ok_or_else(|| anyhow!("deny_with_feedback requires `feedback`"))?,
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
    let decision_label = match &decision {
        ApprovalDecision::AllowOnce => "allow_once",
        ApprovalDecision::AllowAndRemember { .. } => "allow_and_remember",
        ApprovalDecision::Deny => "deny",
        ApprovalDecision::DenyWithFeedback { .. } => "deny_with_feedback",
    };
    let (resolved_scope, resolved_pattern, resolved_extra_patterns) = match &decision {
        ApprovalDecision::AllowAndRemember {
            scope,
            pattern,
            extra_patterns,
        } => {
            let scope = match scope {
                PermissionScope::Once => "once",
                PermissionScope::Session => "session",
                PermissionScope::Project => "project",
                PermissionScope::Global => "global",
            };
            (
                scope,
                pattern.as_deref().unwrap_or(""),
                extra_patterns.join(","),
            )
        }
        _ => ("", "", String::new()),
    };
    info!(
        session_id = %sid,
        request_id = %request_id,
        decision = decision_label,
        scope = resolved_scope,
        pattern = resolved_pattern,
        extra_patterns = %resolved_extra_patterns,
        "permission.approval: web backend received approval"
    );

    let runtime = state.ensure_runtime(&sid).await?;
    let tx = runtime
        .state
        .pending_approvals
        .lock()
        .unwrap()
        .remove(&request_id);
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
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let items = args
        .get("items")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|item| {
                    let title = item
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let kind = item
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("selected");
                    let text = item
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let labels = item
                        .get("labels")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let answer = match kind {
                        "custom" => protocol::SingleAnswer::Custom { text },
                        "selected_multi" => protocol::SingleAnswer::SelectedMulti { labels },
                        "cancelled" => protocol::SingleAnswer::Cancelled,
                        _ => protocol::SingleAnswer::Selected { label: text },
                    };
                    protocol::MultiQuestionAnswer { title, answer }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let answer = match kind {
        "cancelled" => UserAnswer::Cancelled,
        "custom" => UserAnswer::Custom { text: value },
        "selected_multi" => UserAnswer::SelectedMulti { labels },
        "multi" => UserAnswer::Multi { items },
        _ => UserAnswer::Selected { label: value },
    };

    let runtime = state.ensure_runtime(&sid).await?;
    let tx = runtime
        .state
        .pending_questions
        .lock()
        .unwrap()
        .remove(&request_id);
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

async fn cmd_get_models_catalog(state: &ServerState) -> Result<Value> {
    let cache = agent_core::storage::models_catalog::read_catalog(&state.data_dir);
    Ok(serde_json::to_value(cache)?)
}

async fn cmd_get_settings(state: &ServerState) -> Result<Value> {
    let s = settings_store::load(&state.data_dir);
    Ok(serde_json::to_value(s)?)
}

/// 记忆查看（架构 §4.14）：列 L0 清单（全局 + 可选项目）。与 desktop list_memories 对称。
async fn cmd_list_memories(state: &ServerState, args: Value) -> Result<Value> {
    use agent_core::storage::memory::{list_l0, MemoryScope};
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let mut out = list_l0(&state.data_dir, None, MemoryScope::Global)?;
    if let Some(proj) = workdir
        .as_deref()
        .and_then(agent_core::tools::memory_project_workdir)
    {
        out.extend(list_l0(&state.data_dir, Some(&proj), MemoryScope::Project)?);
    }
    Ok(serde_json::to_value(out)?)
}

/// 记忆查看：读一条全文。与 desktop read_memory 对称。
async fn cmd_read_memory(state: &ServerState, args: Value) -> Result<Value> {
    use agent_core::storage::memory::{read, MemoryLevel};
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let wd = if id.starts_with("proj/") {
        workdir
            .as_deref()
            .and_then(agent_core::tools::memory_project_workdir)
    } else {
        None
    };
    let body = read(&state.data_dir, wd.as_deref(), id, MemoryLevel::Full)?;
    Ok(Value::String(body))
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
    let saved =
        model_gateway::config::upsert(&state.data_dir, provider).map_err(|e| anyhow!("{e}"))?;
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
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    prompts_store::delete(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_set_default_prompt(state: &ServerState, args: Value) -> Result<Value> {
    let id = args.get("id").and_then(|v| v.as_str()).map(str::to_string);
    let file = prompts_store::set_default(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(file)?)
}

async fn cmd_rename_session(state: &ServerState, args: Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `title`"))?
        .to_string();
    let s = sessions_store::rename(&state.data_dir, id, title).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(s)?)
}

async fn cmd_delete_session(state: &ServerState, args: Value) -> Result<()> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
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
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let mid = args
        .get("messageId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `messageId`"))?;
    let s = sessions_store::truncate_after(&state.data_dir, id, mid).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(s)?)
}

async fn cmd_truncate_inclusive(state: &ServerState, args: Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let mid = args
        .get("messageId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `messageId`"))?;
    let s =
        sessions_store::truncate_inclusive(&state.data_dir, id, mid).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(s)?)
}

async fn cmd_search_sessions(state: &ServerState, args: Value) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `query`"))?;
    let case_sensitive = args
        .get("caseSensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let regex = args.get("regex").and_then(|v| v.as_bool()).unwrap_or(false);
    let hits = sessions_store::search(&state.data_dir, query, case_sensitive, regex)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(hits)?)
}

async fn cmd_update_session_config(state: &ServerState, args: Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let mut s = sessions_store::load(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    if let Some(p) = args.get("providerId").and_then(|v| v.as_str()) {
        s.provider_id = p.to_string();
    }
    if let Some(m) = args.get("model").and_then(|v| v.as_str()) {
        s.model = m.to_string();
    }
    if let Some(sp) = args.get("systemPrompt").and_then(|v| v.as_str()) {
        s.system_prompt = if sp.is_empty() {
            None
        } else {
            Some(sp.to_string())
        };
    }
    if let Some(pid) = args.get("promptId").and_then(|v| v.as_str()) {
        s.prompt_id = if pid.is_empty() {
            None
        } else {
            Some(pid.to_string())
        };
    }
    if let Some(stream) = args.get("stream").and_then(|v| v.as_bool()) {
        s.stream = stream;
    }
    let reasoning_val = args.get("reasoning").cloned();
    let clear_reasoning = args
        .get("clearReasoning")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
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
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    projects_store::delete(&state.data_dir, id).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_get_run_mode(state: &ServerState, session_id: Option<String>) -> Result<Value> {
    let sid = need_session(session_id)?;
    let runtime = state.ensure_runtime(&sid).await?;
    let mode = runtime.state.run_mode();
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
    runtime.state.set_run_mode(mode);
    // 同步持久化到 session.json
    sessions_store::set_run_mode(&state.data_dir, &sid, mode).map_err(|e| anyhow!("{e}"))?;
    // 同时更新运行中的 agent_loop
    agent_core::run_mode::LiveRunModeRegistry::global().set(&sid, mode);
    Ok(())
}

async fn cmd_get_force_automode(state: &ServerState, session_id: Option<String>) -> Result<Value> {
    let sid = need_session(session_id)?;
    let runtime = state.ensure_runtime(&sid).await?;
    Ok(Value::Bool(
        runtime.state.force_automode.load(Ordering::SeqCst),
    ))
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
    runtime.state.force_automode.store(enabled, Ordering::SeqCst);
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
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let p = state.core.get_provider(id).map_err(map_core_err)?;
    Ok(serde_json::to_value(p)?)
}

async fn cmd_core_fetch_provider_models(state: &ServerState, args: Value) -> Result<Value> {
    let provider: model_gateway::config::Provider =
        serde_json::from_value(args.get("provider").cloned().unwrap_or(Value::Null))
            .map_err(|e| anyhow!("missing/invalid `provider`: {e}"))?;
    let models = state
        .core
        .fetch_provider_models(provider)
        .await
        .map_err(map_core_err)?;
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
    let result = state
        .core
        .test_provider(provider, model)
        .await
        .map_err(map_core_err)?;
    Ok(serde_json::to_value(result)?)
}

async fn cmd_core_list_tools(state: &ServerState) -> Result<Value> {
    // 走 dispatch 唯一入口（架构 §7.1）：surface 把请求表达成 CoreRequest 交给
    // 同一个 dispatch，core 业务只走一条路径。其余同步命令在客户端化（步骤④⑤⑥连
    // hebcore）时统一切换；此处先验证 dispatch + LocalCoreClient facade 端到端通。
    core_rpc::dispatch(core_rpc::CoreRequest::ListTools, &*state.core)
        .await
        .into_json()
        .map_err(|e| anyhow!("{e}"))
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
    let scope = parse_scope(
        args.get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global"),
    );
    let effect = parse_effect(
        args.get("effect")
            .and_then(|v| v.as_str())
            .unwrap_or("allow"),
    );
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
    let scope = parse_scope(
        args.get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global"),
    );
    let effect = parse_effect(
        args.get("effect")
            .and_then(|v| v.as_str())
            .unwrap_or("allow"),
    );
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
    let scope = parse_scope(
        args.get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global"),
    );
    let effect = parse_effect(
        args.get("effect")
            .and_then(|v| v.as_str())
            .unwrap_or("allow"),
    );
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
    let scope = parse_scope(
        args.get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global"),
    );
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
    let scope_str = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global");
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
    let scope_str = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global");
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
    let scope_str = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global");
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
    let scope = match args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global")
    {
        "project" => ImportScope::Project,
        _ => ImportScope::Global,
    };
    let workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let names: Option<Vec<String>> = args.get("names").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect()
    });
    let overwrite = args
        .get("overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let imported = state
        .core
        .import_claude_skills(scope, workdir.as_deref(), names.as_deref(), overwrite)
        .map_err(map_core_err)?;
    Ok(serde_json::to_value(imported)?)
}

async fn cmd_core_import_skills_from_dir(state: &ServerState, args: Value) -> Result<Value> {
    use agent_core::storage::skills::ImportScope;
    let scope = match args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global")
    {
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
    let overwrite = args
        .get("overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let selected: Option<Vec<String>> = args
        .get("selectedPaths")
        .or_else(|| args.get("selected_paths"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        });
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
    let scope = match args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global")
    {
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
    let overwrite = args
        .get("overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let selected: Option<Vec<String>> = args
        .get("selectedPaths")
        .or_else(|| args.get("selected_paths"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        });
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
    let source = match args
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("global")
    {
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

// ─── 镜像自 desktop lib.rs / chat.rs 的命令实现 ────────────────────────────
// hebweb 是独立的 surface，所有命令都自己实现。未镜像的（OAuth / Edits / preview_payload
// 等）按需照 desktop 实现照搬过来。

// ─── parity 补齐：subagent / mcp / hooks / skill_collection / plugin ──────────
// 全部委托 state.core（与 desktop 共用同一 LocalCoreClient facade），hebweb 只做参数解析。

// ─── parity 补齐：goal / todos / plan / model_io / 杂项（直接读 agent_core::storage 纯函数）─
// 与 desktop lib.rs 同名命令同实现，只是入口从 Tauri command 换成 WS dispatch。

async fn cmd_list_todos(state: &ServerState, args: Value) -> Result<Value> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let session = sessions_store::load(&state.data_dir, &sid).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(session.todos)?)
}

async fn cmd_get_active_goal(state: &ServerState, args: Value) -> Result<Value> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let session = sessions_store::load(&state.data_dir, &sid).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(session.active_goal)?)
}

async fn cmd_set_active_goal(state: &ServerState, args: Value) -> Result<()> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let condition = arg_str(&args, &["condition"]).ok_or_else(|| anyhow!("missing `condition`"))?;
    let goal = sessions_store::ActiveGoal {
        condition: condition.clone(),
        created_at: chrono::Utc::now().timestamp_millis(),
        iterations: 0,
        last_reason: None,
        pending_set_marker: true,
    };
    sessions_store::set_active_goal(&state.data_dir, &sid, Some(goal)).map_err(|e| anyhow!("{e}"))?;
    let marker = sessions_store::Message {
        id: sessions_store::new_id(),
        role: sessions_store::Role::Marker,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: chrono::Utc::now().timestamp_millis(),
        meta: Some(sessions_store::MessageMeta::GoalOutcome {
            kind: "set".to_string(),
            condition,
            reason: String::new(),
            iteration: 0,
        }),
        subagent_call_id: None,
        run_duration_ms: None,
    };
    sessions_store::append_message(&state.data_dir, &sid, marker).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_clear_active_goal(state: &ServerState, args: Value) -> Result<()> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    sessions_store::set_active_goal(&state.data_dir, &sid, None)
        .map(|_| ())
        .map_err(|e| anyhow!("{e}"))
}

async fn cmd_undo_compaction(state: &ServerState, args: Value) -> Result<Value> {
    let id = arg_str(&args, &["id", "sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `id`"))?;
    let marker_id = arg_str(&args, &["markerId", "marker_id"]).ok_or_else(|| anyhow!("missing `markerId`"))?;
    let s = sessions_store::undo_compaction(&state.data_dir, &id, &marker_id).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(s)?)
}

async fn cmd_list_session_plans(state: &ServerState, args: Value) -> Result<Value> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let session = sessions_store::load(&state.data_dir, &sid).map_err(|e| anyhow!("{e}"))?;
    let active = session.active_plan.clone();
    let dir = agent_core::storage::plans::dir_for_session(&state.data_dir, session.workdir.as_deref(), &sid);
    if !dir.exists() {
        return Ok(json!([]));
    }
    let mut out: Vec<Value> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let plan_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if plan_id.is_empty() {
            continue;
        }
        let updated_at_ms = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let title = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.lines().next().map(|l| l.trim_start_matches('#').trim().to_string()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| plan_id.clone());
        let plan_path_str = path.display().to_string();
        let is_active = active.as_deref() == Some(plan_path_str.as_str());
        out.push(json!({
            "plan_id": plan_id,
            "plan_path": plan_path_str,
            "title": title,
            "updated_at_ms": updated_at_ms,
            "is_active": is_active,
        }));
    }
    out.sort_by(|a, b| b["updated_at_ms"].as_i64().cmp(&a["updated_at_ms"].as_i64()));
    Ok(Value::Array(out))
}

async fn cmd_read_plan_markdown(state: &ServerState, args: Value) -> Result<Value> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let plan_id = arg_str(&args, &["planId", "plan_id"]).ok_or_else(|| anyhow!("missing `planId`"))?;
    let session = sessions_store::load(&state.data_dir, &sid).map_err(|e| anyhow!("{e}"))?;
    let path = agent_core::storage::plans::dir_for_session(&state.data_dir, session.workdir.as_deref(), &sid)
        .join(format!("{plan_id}.md"));
    let bytes = agent_core::storage::lock::read_locked(&path).map_err(|e| anyhow!("{e}"))?;
    Ok(Value::String(String::from_utf8_lossy(&bytes).to_string()))
}

async fn cmd_update_plan_markdown(state: &ServerState, args: Value) -> Result<()> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let plan_id = arg_str(&args, &["planId", "plan_id"]).ok_or_else(|| anyhow!("missing `planId`"))?;
    let markdown = arg_str(&args, &["markdown"]).ok_or_else(|| anyhow!("missing `markdown`"))?;
    let session = sessions_store::load(&state.data_dir, &sid).map_err(|e| anyhow!("{e}"))?;
    let path = agent_core::storage::plans::dir_for_session(&state.data_dir, session.workdir.as_deref(), &sid)
        .join(format!("{plan_id}.md"));
    agent_core::storage::lock::write_atomic(&path, markdown.as_bytes()).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_list_plan_comments(state: &ServerState, args: Value) -> Result<Value> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let plan_id = arg_str(&args, &["planId", "plan_id"]).ok_or_else(|| anyhow!("missing `planId`"))?;
    let session = sessions_store::load(&state.data_dir, &sid).map_err(|e| anyhow!("{e}"))?;
    let comments = agent_core::storage::plan_comments::list_comments(
        &state.data_dir,
        session.workdir.as_deref(),
        &sid,
        &plan_id,
    )
    .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(comments)?)
}

async fn cmd_list_claude_sessions(_state: &ServerState) -> Result<Value> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("找不到用户主目录"))?;
    let dir = home.join(".claude").join("projects");
    let list = agent_core::storage::import_claude::list_importable(&dir).map_err(|e| anyhow!("{e}"))?;
    let out: Vec<Value> = list
        .into_iter()
        .map(|i| {
            json!({
                "path": i.path.to_string_lossy(),
                "uuid": i.uuid,
                "title": i.title,
                "cwd": i.cwd,
                "message_count": i.message_count,
                "modified_ms": i.modified_ms,
            })
        })
        .collect();
    Ok(Value::Array(out))
}

async fn cmd_import_claude_session(state: &ServerState, args: Value) -> Result<Value> {
    let path = arg_str(&args, &["path"]).ok_or_else(|| anyhow!("missing `path`"))?;
    let project_id = arg_str(&args, &["projectId", "project_id"]);
    let workdir = arg_str(&args, &["workdir"]);
    let content = std::fs::read_to_string(&path).map_err(|e| anyhow!("读取失败：{e}"))?;
    let parsed = agent_core::storage::import_claude::parse_claude_jsonl(&content).map_err(|e| anyhow!("{e}"))?;
    let session_workdir = workdir.map(std::path::PathBuf::from).or(parsed.workdir);
    let mut session = sessions_store::create_with_workspace(
        &state.data_dir,
        String::new(),
        parsed.model,
        None,
        None,
        "claude".into(),
        project_id,
        session_workdir,
        Vec::new(),
    )
    .map_err(|e| anyhow!("{e}"))?;
    session.title = parsed.title;
    session.messages = parsed.messages;
    let saved = sessions_store::save(&state.data_dir, session).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(saved)?)
}

async fn cmd_import_vscode_project(state: &ServerState, args: Value) -> Result<Value> {
    let path = arg_str(&args, &["path"]).ok_or_else(|| anyhow!("missing `path`"))?;
    let name = arg_str(&args, &["name"]);
    let content = std::fs::read_to_string(&path).map_err(|e| anyhow!("{e}"))?;
    let project = agent_core::storage::projects::import_vscode_workspace(
        &state.data_dir,
        &content,
        name,
        Some(std::path::Path::new(&path)),
    )
    .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(project)?)
}

async fn cmd_refresh_models_catalog(state: &ServerState) -> Result<Value> {
    let updated = agent_core::storage::models_catalog::refresh_catalog(&state.data_dir).await;
    Ok(Value::Bool(updated))
}

async fn cmd_add_plan_comment(state: &ServerState, args: Value) -> Result<Value> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let plan_id = arg_str(&args, &["planId", "plan_id"]).ok_or_else(|| anyhow!("missing `planId`"))?;
    let anchor = arg_str(&args, &["anchor"]).unwrap_or_default();
    let body = arg_str(&args, &["body"]).ok_or_else(|| anyhow!("missing `body`"))?;
    let session = sessions_store::load(&state.data_dir, &sid).map_err(|e| anyhow!("{e}"))?;
    let comment = protocol::todo::PlanComment {
        id: format!("pc-{}", sessions_store::new_id()),
        plan_id: plan_id.clone(),
        anchor,
        body,
        created_at_ms: 0,
        consumed: false,
    };
    let saved = agent_core::storage::plan_comments::append_comment(
        &state.data_dir,
        session.workdir.as_deref(),
        &sid,
        &plan_id,
        comment,
    )
    .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(saved)?)
}

async fn cmd_read_skill_md(_state: &ServerState, args: Value) -> Result<Value> {
    let path = arg_str(&args, &["path"]).ok_or_else(|| anyhow!("missing `path`"))?;
    let p = std::path::Path::new(&path);
    let is_skill = p.file_name().map(|n| n == std::ffi::OsStr::new("SKILL.md")).unwrap_or(false);
    if !is_skill {
        return Err(anyhow!("仅允许读取 SKILL.md 文件"));
    }
    let content = std::fs::read_to_string(p).map_err(|e| anyhow!("读取 {} 失败：{e}", p.display()))?;
    Ok(Value::String(content))
}

async fn cmd_import_project_file(state: &ServerState, args: Value) -> Result<Value> {
    let path = arg_str(&args, &["path"]).ok_or_else(|| anyhow!("missing `path`"))?;
    let content = std::fs::read_to_string(&path).map_err(|e| anyhow!("{e}"))?;
    let project: projects_store::WorkspaceProject =
        serde_json::from_str(&content).map_err(|e| anyhow!("解析项目文件失败：{e}"))?;
    let workdir = project.workdir().cloned().unwrap_or_default();
    let allowed_paths = project.allowed_paths();
    let input = projects_store::WorkspaceProjectInput {
        id: Some(project.id.clone()),
        name: project.name.clone(),
        workdir,
        allowed_paths,
        source: project.source.clone(),
    };
    let saved = state.core.save_project(input).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(saved)?)
}

async fn cmd_switch_provider_model(state: &ServerState, args: Value) -> Result<Value> {
    let id = arg_str(&args, &["id", "sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `id`"))?;
    let new_provider_id = arg_str(&args, &["newProviderId", "new_provider_id"]).ok_or_else(|| anyhow!("missing `newProviderId`"))?;
    let new_model = arg_str(&args, &["newModel", "new_model"]).ok_or_else(|| anyhow!("missing `newModel`"))?;
    let dd = &state.data_dir;
    let cur = sessions_store::load(dd, &id).map_err(|e| anyhow!("{e}"))?;
    let cur_provider = model_gateway::config::get(dd, &cur.provider_id).ok();
    let new_provider = model_gateway::config::get(dd, &new_provider_id).ok();
    let from_provider = cur_provider.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| cur.provider_id.clone());
    let to_provider = new_provider.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| new_provider_id.clone());
    if cur.provider_id == new_provider_id && cur.model == new_model {
        return Ok(serde_json::to_value(cur)?);
    }
    // 模型系列锁定：有真实对话后 DeepSeek 与其他系列不可互切（web 编码与协议不同）。
    let has_real_turn = cur
        .messages
        .iter()
        .any(|m| matches!(m.role, sessions_store::Role::User | sessions_store::Role::Assistant));
    if has_real_turn {
        if let (Some(c), Some(n)) = (cur_provider.as_ref(), new_provider.as_ref()) {
            let cur_ds = matches!(c.kind, model_gateway::config::ProviderKind::Deepseek);
            let new_ds = matches!(n.kind, model_gateway::config::ProviderKind::Deepseek);
            if cur_ds != new_ds {
                return Err(anyhow!("本会话已锁定模型系列：DeepSeek 与其他模型之间不可互相切换，请新建会话。"));
            }
        }
    }
    let meta = sessions_store::MessageMeta::Switch {
        from_provider,
        from_model: cur.model.clone(),
        to_provider,
        to_model: new_model.clone(),
    };
    sessions_store::insert_switch_marker(dd, &id, meta).map_err(|e| anyhow!("{e}"))?;
    let mut updated = sessions_store::load(dd, &id).map_err(|e| anyhow!("{e}"))?;
    updated.provider_id = new_provider_id;
    updated.model = new_model;
    let supports = common::reasoning::anthropic_supports_thinking(&updated.model)
        || common::reasoning::openai_supports_reasoning(&updated.model);
    if supports {
        if updated.reasoning.is_none() {
            updated.reasoning = Some(common::ReasoningConfig {
                enabled: Some(true),
                effort: Some(common::ReasoningEffort::Extra),
                long_context: None,
            });
        }
    } else {
        updated.reasoning = None;
    }
    let saved = sessions_store::save(dd, updated).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(saved)?)
}

async fn cmd_fetch_provider_usage(state: &ServerState, args: Value) -> Result<Value> {
    use model_gateway::config::AuthMode;
    let provider_id = arg_str(&args, &["providerId", "provider_id"]).ok_or_else(|| anyhow!("missing `providerId`"))?;
    let dir = &state.data_dir;
    let file = model_gateway::config::load(dir).map_err(|e| anyhow!("read providers: {e}"))?;
    let provider = file
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| anyhow!("provider not found: {provider_id}"))?;
    if provider.auth_mode == AuthMode::OauthClaudeCode {
        let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(dir, provider.clone())
            .await
            .map_err(|e| anyhow!("refresh token: {e}"))?;
        let info = model_gateway::usage::fetch_claude_usage(&provider.api_key)
            .await
            .map_err(|e| anyhow!("fetch claude usage: {e}"))?;
        return Ok(json!({ "kind": "claude", "info": info }));
    }
    if provider.base_url.contains("api.deepseek.com") {
        let balances = model_gateway::usage::fetch_deepseek_balance(&provider.api_key)
            .await
            .map_err(|e| anyhow!("fetch deepseek balance: {e}"))?;
        return Ok(json!({ "kind": "deepseek", "balances": balances }));
    }
    Ok(json!({ "kind": "unsupported" }))
}

async fn cmd_export_session_to_claude(state: &ServerState, args: Value) -> Result<Value> {
    let sid = arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let include_thinking = args.get("includeThinking").or_else(|| args.get("include_thinking")).and_then(|v| v.as_bool()).unwrap_or(false);
    let home = dirs::home_dir().ok_or_else(|| anyhow!("找不到用户主目录"))?;
    let export = agent_core::storage::export_claude::build_claude_resume(&state.data_dir, &sid, include_thinking, &home)
        .map_err(|e| anyhow!("{e}"))?;
    let dir = home.join(".claude").join("projects").join(&export.dir_name);
    std::fs::create_dir_all(&dir).map_err(|e| anyhow!("创建目录失败：{e}"))?;
    let path = dir.join(format!("{}.jsonl", export.session_uuid));
    std::fs::write(&path, export.lines.join("\n")).map_err(|e| anyhow!("写入失败：{e}"))?;
    let resume_command = format!("cd {} && claude --resume {}", shell_quote_min(&export.cwd), export.session_uuid);
    Ok(json!({
        "resume_command": resume_command,
        "session_uuid": export.session_uuid,
        "path": path.to_string_lossy(),
    }))
}

/// 最小 shell 引用：路径含空格 / 特殊字符时用单引号包裹。
fn shell_quote_min(s: &str) -> String {
    if s.is_empty() || s.chars().any(|c| !c.is_ascii_alphanumeric() && !matches!(c, '/' | '.' | '_' | '-')) {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

async fn cmd_discover_all_rules(_state: &ServerState, args: Value) -> Result<Value> {
    use agent_core::rules::{RuleFileInfo, RuleSource};
    let workdir = arg_str(&args, &["workdir"]).map(std::path::PathBuf::from);
    let allowed_paths: Vec<std::path::PathBuf> = args
        .get("allowedPaths")
        .or_else(|| args.get("allowed_paths"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(std::path::PathBuf::from)).collect())
        .unwrap_or_default();
    let mut out: Vec<RuleFileInfo> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for g in agent_core::rules::default_global_rules() {
        if !g.exists() {
            continue;
        }
        let key = g.display().to_string();
        if seen.insert(key.clone()) {
            out.push(RuleFileInfo { path: key, source: RuleSource::Global });
        }
    }
    if let Some(wd) = workdir {
        for f in agent_core::rules::discover(&wd, &allowed_paths) {
            let key = f.path.display().to_string();
            if seen.insert(key.clone()) {
                out.push(RuleFileInfo { path: key, source: f.source });
            }
        }
    }
    Ok(serde_json::to_value(out)?)
}

/// 路径访问审批：按 scope 落 storage（this_session→session.allowed_paths /
/// global→settings.conversation.allowed_paths / this_project,once→不持久化），
/// 再把 ApprovalDecision 投回 run 的 pending_approvals oneshot（与 cmd_approve_permission 同链路）。
async fn cmd_approve_path_access(
    state: &ServerState,
    args: Value,
    session_id: Option<String>,
) -> Result<()> {
    let request_id =
        arg_str(&args, &["requestId", "request_id"]).ok_or_else(|| anyhow!("missing `requestId`"))?;
    let scope = arg_str(&args, &["scope"]).ok_or_else(|| anyhow!("missing `scope`"))?;
    let paths: Vec<PathBuf> = args
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(PathBuf::from))
                .collect()
        })
        .unwrap_or_default();

    // scope 命名（与 desktop 同步）：
    //   once          → 仅本次，不持久化
    //   this_session  → 仅当前对话（写 session.allowed_paths）
    //   this_project  → 当前 workdir 所有对话（PermissionStore Project FilePath 规则，由 run 在 AllowAndRemember 时落）
    //   global        → 任意对话（写 settings.conversation.allowed_paths）
    let decision = match scope.as_str() {
        "this_session" => {
            let sid = session_id
                .clone()
                .ok_or_else(|| anyhow!("approve_path_access: this_session 需要 sessionId"))?;
            sessions_store::update_meta(&state.data_dir, &sid, |s| {
                let mut existing = s.allowed_paths.take().unwrap_or_default();
                for p in &paths {
                    if !existing.iter().any(|path| path == p) {
                        existing.push(p.clone());
                    }
                }
                s.allowed_paths = Some(existing);
                Ok(())
            })
            .map_err(|e| anyhow!("{e}"))?;
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Session,
                pattern: None,
                extra_patterns: Vec::new(),
            }
        }
        "global" => {
            let mut settings = settings_store::load(&state.data_dir);
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
            settings_store::save(&state.data_dir, &settings).map_err(|e| anyhow!("{e}"))?;
            ApprovalDecision::AllowAndRemember {
                scope: PermissionScope::Global,
                pattern: None,
                extra_patterns: Vec::new(),
            }
        }
        "this_project" => ApprovalDecision::AllowAndRemember {
            scope: PermissionScope::Project,
            pattern: None,
            extra_patterns: Vec::new(),
        },
        "once" => ApprovalDecision::AllowOnce,
        other => return Err(anyhow!("未知 scope: {other}")),
    };

    let sid = need_session(session_id)?;
    info!(
        session_id = %sid,
        request_id = %request_id,
        scope = %scope,
        "permission.approval: web backend received path approval"
    );
    let runtime = state.ensure_runtime(&sid).await?;
    let tx = runtime
        .state
        .pending_approvals
        .lock()
        .unwrap()
        .remove(&request_id);
    match tx {
        Some(tx) => {
            let _ = tx.send(decision);
            Ok(())
        }
        None => Err(anyhow!("unknown request_id: {request_id}")),
    }
}

/// 探测单条粘贴路径形态（file/dir/missing）。委托 agent_core::attach（与 desktop 同实现）。
async fn cmd_attach_path(args: Value) -> Result<Value> {
    let path = arg_str(&args, &["path"]).ok_or_else(|| anyhow!("missing `path`"))?;
    Ok(serde_json::to_value(agent_core::attach::attach_path(&path))?)
}

/// 批量分流拖拽路径（小图片/文本读成附件，其余引用）。委托 agent_core::attach。
async fn cmd_drop_paths(args: Value) -> Result<Value> {
    let paths: Vec<String> = args
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(serde_json::to_value(agent_core::attach::drop_paths(paths))?)
}

/// 「显示原始 JSON」预览：复刻 agent_loop 进入模型前的拼装，不发请求不改 session。
/// 委托 agent_core::preview_payload（与 desktop 同实现）。
async fn cmd_preview_session_payload(state: &ServerState, args: Value) -> Result<Value> {
    let sid = arg_str(&args, &["sessionId", "session_id"])
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let upto = arg_str(&args, &["uptoMessageId", "upto_message_id"]);
    agent_core::preview_payload::build_preview_payload(&state.data_dir, &sid, upto.as_deref())
        .await
        .map_err(|e| anyhow!("{e}"))
}

// ─── parity 补齐：OAuth 登录 / deepseek 登录 / 日志（纯 model_gateway::auth + fs，无 Tauri）─
//
// OAuth 全链路（start 取授权 URL / device code、exchange 用 code 换 token、refresh 刷新、
// import 读本机 CLI 凭证）都在 model_gateway::auth，纯 reqwest 实现，零 Tauri 依赖。
// 浏览器 surface 里 OAuth 反而更自然——前端本就在浏览器，能直接跳转授权页 + 回调拿 code。
// desktop 只是借 Tauri shell 打开系统浏览器，那层壳与登录逻辑无关。

async fn cmd_oauth_codex_start() -> Result<Value> {
    Ok(serde_json::to_value(
        model_gateway::auth::codex_start().await.map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_codex_poll(args: Value) -> Result<Value> {
    let device_code =
        arg_str(&args, &["deviceCode", "device_code"]).ok_or_else(|| anyhow!("missing `deviceCode`"))?;
    Ok(serde_json::to_value(
        model_gateway::auth::codex_poll(&device_code)
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_codex_refresh(args: Value) -> Result<Value> {
    let refresh_token = arg_str(&args, &["refreshToken", "refresh_token"])
        .ok_or_else(|| anyhow!("missing `refreshToken`"))?;
    Ok(serde_json::to_value(
        model_gateway::auth::codex_refresh(&refresh_token)
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_openai_start() -> Result<Value> {
    Ok(serde_json::to_value(
        model_gateway::auth::openai_oauth_start().map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_openai_exchange(args: Value) -> Result<Value> {
    let session_id =
        arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let code = arg_str(&args, &["code"]).ok_or_else(|| anyhow!("missing `code`"))?;
    let state = arg_str(&args, &["state"]);
    Ok(serde_json::to_value(
        model_gateway::auth::openai_oauth_exchange(&session_id, &code, state.as_deref())
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_claude_start() -> Result<Value> {
    Ok(serde_json::to_value(
        model_gateway::auth::claude_oauth_start().map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_claude_exchange(args: Value) -> Result<Value> {
    let session_id =
        arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let code = arg_str(&args, &["code"]).ok_or_else(|| anyhow!("missing `code`"))?;
    Ok(serde_json::to_value(
        model_gateway::auth::claude_oauth_exchange(&session_id, &code)
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_claude_refresh(args: Value) -> Result<Value> {
    let refresh_token = arg_str(&args, &["refreshToken", "refresh_token"])
        .ok_or_else(|| anyhow!("missing `refreshToken`"))?;
    Ok(serde_json::to_value(
        model_gateway::auth::claude_oauth_refresh(&refresh_token)
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_claude_code_import() -> Result<Value> {
    Ok(serde_json::to_value(
        model_gateway::auth::claude_code_import()
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_gemini_start() -> Result<Value> {
    Ok(serde_json::to_value(
        model_gateway::auth::gemini_oauth_start().map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_gemini_exchange(args: Value) -> Result<Value> {
    let session_id =
        arg_str(&args, &["sessionId", "session_id"]).ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let code = arg_str(&args, &["code"]).ok_or_else(|| anyhow!("missing `code`"))?;
    Ok(serde_json::to_value(
        model_gateway::auth::gemini_oauth_exchange(&session_id, &code)
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_gemini_refresh(args: Value) -> Result<Value> {
    let refresh_token = arg_str(&args, &["refreshToken", "refresh_token"])
        .ok_or_else(|| anyhow!("missing `refreshToken`"))?;
    let client_id =
        arg_str(&args, &["clientId", "client_id"]).ok_or_else(|| anyhow!("missing `clientId`"))?;
    let client_secret = arg_str(&args, &["clientSecret", "client_secret"])
        .ok_or_else(|| anyhow!("missing `clientSecret`"))?;
    Ok(serde_json::to_value(
        model_gateway::auth::gemini_refresh(&refresh_token, &client_id, &client_secret)
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_oauth_gemini_cli_import() -> Result<Value> {
    Ok(serde_json::to_value(
        model_gateway::auth::gemini_cli_import()
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_deepseek_login(args: Value) -> Result<Value> {
    let input: model_gateway::auth::deepseek::DeepseekLoginInput = serde_json::from_value(
        args.get("input").cloned().unwrap_or(Value::Null),
    )
    .map_err(|e| anyhow!("invalid `input`: {e}"))?;
    Ok(serde_json::to_value(
        model_gateway::auth::deepseek::deepseek_login(input)
            .await
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

/// 读今天的日志文件内容（供 LogPane 历史展示）。文件不存在返回空串。纯 fs，无 Tauri。
async fn cmd_read_log_file() -> Result<Value> {
    let content = match observability::today_log_path() {
        Some(p) if p.exists() => std::fs::read_to_string(&p).map_err(|e| anyhow!("{e}"))?,
        _ => String::new(),
    };
    Ok(Value::String(content))
}

// ─── parity 补齐：subagent / mcp / hooks / skill_collection / plugin ──────────
// 全部委托 state.core（与 desktop 共用同一 LocalCoreClient facade），hebweb 只做参数解析。

fn arg_str(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| args.get(k).and_then(|v| v.as_str()))
        .map(str::to_string)
}

async fn cmd_list_subagents(state: &ServerState, args: Value) -> Result<Value> {
    let workdir = arg_str(&args, &["workdir"]).map(std::path::PathBuf::from);
    Ok(serde_json::to_value(
        state.core.list_subagents(workdir.as_deref()),
    )?)
}

async fn cmd_get_subagent(state: &ServerState, args: Value) -> Result<Value> {
    let name = arg_str(&args, &["name"]).ok_or_else(|| anyhow!("missing `name`"))?;
    Ok(serde_json::to_value(
        state.core.get_subagent(&name).map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_save_subagent(state: &ServerState, args: Value) -> Result<()> {
    let name = arg_str(&args, &["name"]).ok_or_else(|| anyhow!("missing `name`"))?;
    let content = arg_str(&args, &["content"]).ok_or_else(|| anyhow!("missing `content`"))?;
    state
        .core
        .save_subagent(&name, &content)
        .map_err(|e| anyhow!("{e}"))
}

async fn cmd_delete_subagent(state: &ServerState, args: Value) -> Result<()> {
    let name = arg_str(&args, &["name"]).ok_or_else(|| anyhow!("missing `name`"))?;
    let workdir = arg_str(&args, &["workdir"]).map(std::path::PathBuf::from);
    state
        .core
        .delete_subagent(&name, workdir.as_deref())
        .map_err(|e| anyhow!("{e}"))
}

async fn cmd_set_subagent_enabled(state: &ServerState, args: Value) -> Result<()> {
    let name = arg_str(&args, &["name"]).ok_or_else(|| anyhow!("missing `name`"))?;
    let enabled = args
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow!("missing `enabled`"))?;
    // scope：{"Global"} 或 {"Project":"<path>"}（与 SubagentScope serde 一致）。
    let scope = args
        .get("scope")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| anyhow!("invalid `scope`: {e}"))?
        .unwrap_or(agent_core::core_client::SubagentScope::Global);
    state
        .core
        .set_subagent_enabled(&name, scope, enabled)
        .map_err(|e| anyhow!("{e}"))
}

async fn cmd_load_subagent_run(state: &ServerState, args: Value) -> Result<Value> {
    let parent = arg_str(&args, &["parentSessionId", "parent_session_id"])
        .ok_or_else(|| anyhow!("missing `parentSessionId`"))?;
    let child = arg_str(&args, &["childSessionId", "child_session_id"])
        .ok_or_else(|| anyhow!("missing `childSessionId`"))?;
    Ok(serde_json::to_value(
        state
            .core
            .load_subagent_run(&parent, &child)
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_get_mcp_config(state: &ServerState) -> Result<Value> {
    Ok(serde_json::to_value(state.core.get_mcp_config())?)
}

async fn cmd_save_mcp_config(state: &ServerState, args: Value) -> Result<()> {
    let config = serde_json::from_value(
        args.get("config")
            .cloned()
            .ok_or_else(|| anyhow!("missing `config`"))?,
    )
    .map_err(|e| anyhow!("invalid `config`: {e}"))?;
    state.core.save_mcp_config(config).map_err(|e| anyhow!("{e}"))
}

async fn cmd_discover_mcp_tools(state: &ServerState) -> Result<Value> {
    Ok(serde_json::to_value(state.core.discover_mcp_tools().await)?)
}

async fn cmd_get_hooks_raw(state: &ServerState) -> Result<Value> {
    Ok(Value::String(state.core.get_hooks_raw()))
}

async fn cmd_save_hooks_raw(state: &ServerState, args: Value) -> Result<()> {
    let raw = arg_str(&args, &["raw"]).ok_or_else(|| anyhow!("missing `raw`"))?;
    state.core.save_hooks_raw(&raw).map_err(|e| anyhow!("{e}"))
}

async fn cmd_list_skill_collections(state: &ServerState) -> Result<Value> {
    Ok(serde_json::to_value(state.core.list_skill_collections())?)
}

async fn cmd_delete_skill_collection(state: &ServerState, args: Value) -> Result<Value> {
    let id = arg_str(&args, &["id"]).ok_or_else(|| anyhow!("missing `id`"))?;
    Ok(serde_json::to_value(
        state
            .core
            .delete_skill_collection(&id)
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_plugin_marketplace_add(state: &ServerState, args: Value) -> Result<Value> {
    let source = arg_str(&args, &["source"]).ok_or_else(|| anyhow!("missing `source`"))?;
    Ok(Value::String(
        state
            .core
            .plugin_marketplace_add(&source)
            .map_err(|e| anyhow!("{e}"))?,
    ))
}

async fn cmd_plugin_marketplace_list(state: &ServerState) -> Result<Value> {
    Ok(serde_json::to_value(state.core.plugin_marketplace_list())?)
}

async fn cmd_plugin_marketplace_list_plugins(state: &ServerState, args: Value) -> Result<Value> {
    let name = arg_str(&args, &["name"]).ok_or_else(|| anyhow!("missing `name`"))?;
    Ok(serde_json::to_value(
        state
            .core
            .plugin_marketplace_list_plugins(&name)
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_plugin_marketplace_remove(state: &ServerState, args: Value) -> Result<()> {
    let name = arg_str(&args, &["name"]).ok_or_else(|| anyhow!("missing `name`"))?;
    state
        .core
        .plugin_marketplace_remove(&name)
        .map_err(|e| anyhow!("{e}"))
}

async fn cmd_plugin_install(state: &ServerState, args: Value) -> Result<Value> {
    let name = arg_str(&args, &["name"]).ok_or_else(|| anyhow!("missing `name`"))?;
    let marketplace = arg_str(&args, &["marketplace"]);
    Ok(serde_json::to_value(
        state
            .core
            .plugin_install(&name, marketplace.as_deref())
            .map_err(|e| anyhow!("{e}"))?,
    )?)
}

async fn cmd_plugin_uninstall(state: &ServerState, args: Value) -> Result<()> {
    let name = arg_str(&args, &["name"]).ok_or_else(|| anyhow!("missing `name`"))?;
    state.core.plugin_uninstall(&name).map_err(|e| anyhow!("{e}"))
}

async fn cmd_plugin_list(state: &ServerState) -> Result<Value> {
    Ok(serde_json::to_value(state.core.plugin_list())?)
}

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

/// 「重新生成标题」命令（手动入口）：自动生成已下沉到 agent_core，由 Harness::spawn_run
/// 在首轮 TurnFinished 后异步触发并通过 `WireEvent::SessionTitleChanged` 推到前端。
/// 本命令是手动重生成入口——无视当前 title，强制走一次。
async fn cmd_generate_session_title(state: &ServerState, args: Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `id`"))?;
    let session = agent_core::session_titler::regenerate_session_title(&state.data_dir, id)
        .await
        .map_err(|e| anyhow!("{e}"))?;
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
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(PathBuf::from))
                .collect()
        })
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

/// 读某个后台 task 的增量输出（语义同 desktop `read_background_task_output`）：
/// 按调用方传入的 cursor 取，用 `read_at` 不动 shell 内部 read_cursor，多读者互不干扰。
/// task 已不在注册表 → 空 chunk + state="exited"，让前端回落到 message.tool_call.result。
async fn cmd_read_background_task_output_local(args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let task_id = args
        .get("taskId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `taskId`"))?;
    let cursor = args.get("cursor").and_then(|v| v.as_u64()).unwrap_or(0);
    let shells = agent_core::tools::background::registry_for_session(sid);
    let Some(shell) = shells.get(task_id) else {
        return Ok(json!({
            "total_bytes": cursor,
            "chunk": "",
            "state": "exited",
            "bytes_dropped": 0,
        }));
    };
    let snap = shell.read_at(cursor);
    Ok(json!({
        "total_bytes": snap.total_bytes,
        "chunk": snap.content,
        "state": snap.state.label().to_string(),
        "bytes_dropped": snap.bytes_dropped,
    }))
}

/// 读 session 的 `model_io.jsonl`，返回 `Vec<DumpEntry-as-Value>`。
/// 与 desktop `list_session_model_io` 同语义；直接读 hebweb 自己的 data_dir。
async fn cmd_list_session_model_io(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let entries = agent_core::storage::model_io::read_session_summaries(&state.data_dir, sid)
        .map_err(|e| anyhow!("读 model_io.jsonl 失败：{e}"))?;
    Ok(Value::Array(entries))
}

async fn cmd_get_session_model_io_entry(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let index = args
        .get("index")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("missing `index`"))? as usize;
    let entry = agent_core::storage::model_io::read_session_entry(&state.data_dir, sid, index)
        .map_err(|e| anyhow!("读 model_io entry 失败：{e}"))?;
    Ok(entry.unwrap_or(Value::Null))
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
            arr.iter()
                .filter_map(|v| v.as_str().map(PathBuf::from))
                .collect()
        })
    };
    let take_strs = |key: &str| -> Option<Vec<String>> {
        args.get(key).and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
    };
    let take_bool =
        |key: &str| -> bool { args.get(key).and_then(|v| v.as_bool()).unwrap_or(false) };

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

// ─── Edits Worktree（架构 §4.13） ─────────────────────────────────────────
//
// 镜像 desktop 的 4 个 Tauri command。hebweb 是单浏览器 tab 一个 ws，
// 没有 desktop 的多窗口同步问题，revert 不再向其他客户端广播 edit-reverted；
// 前端 revertEdit 拿到 success 后会自己 refreshEdits 拉权威列表兜底。

fn build_edits_worktree_for(state: &ServerState, session_id: &str) -> Result<EditsWorktree> {
    let session = sessions_store::load(&state.data_dir, session_id)
        .map_err(|e| anyhow!("加载 session 失败: {e}"))?;
    let settings = settings_store::load(&state.data_dir);
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
    Ok(EditsWorktree::new(&state.data_dir, session_id, &workspace))
}

async fn cmd_list_edits(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let wd = edits::metadata::worktree_dir(&state.data_dir, sid);
    let meta = edits::metadata::load_metadata(&wd).map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::to_value(meta.runs)?)
}

async fn cmd_diff_edit(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let run_id = args
        .get("runId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `runId`"))?;
    let file_path = args
        .get("filePath")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `filePath`"))?;
    let worktree = build_edits_worktree_for(state, sid)?;
    if !worktree.enabled().await {
        return Err(anyhow!("git 不可用，无法生成 diff"));
    }
    let runs = worktree.list_runs().map_err(|e| anyhow!("{e}"))?;
    let run = runs
        .into_iter()
        .find(|e| e.run_id == run_id)
        .ok_or_else(|| anyhow!("找不到该轮修改"))?;
    let file = run
        .files
        .into_iter()
        .find(|f| f.real_path == file_path)
        .ok_or_else(|| anyhow!("找不到该文件修改"))?;
    let (before_text, after_text) = worktree
        .diff_text(&file)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(json!({
        "before_text": before_text,
        "after_text": after_text,
        "before_sha": file.before_sha,
        "after_sha": file.after_sha,
        "file_path": file.real_path,
        "action": format!("{:?}", file.action).to_lowercase(),
    }))
}

async fn cmd_read_text_file(args: Value) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `path`"))?;
    let path = std::path::PathBuf::from(path);
    let meta = std::fs::metadata(&path).map_err(|e| anyhow!("{e}"))?;
    if !meta.is_file() {
        return Err(anyhow!("not a regular file"));
    }
    if meta.len() > 8 * 1024 * 1024 {
        return Err(anyhow!("file too large"));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| anyhow!("{e}"))?;
    Ok(Value::String(text))
}

/// 列目录直接子项（语义同 desktop `read_dir`）：dir-first，再按名字排序，隐藏项靠后。
async fn cmd_read_dir(args: Value) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `path`"))?;
    let path = std::path::PathBuf::from(path);
    let meta = std::fs::metadata(&path).map_err(|e| anyhow!("{e}"))?;
    if !meta.is_dir() {
        return Err(anyhow!("not a directory"));
    }
    let mut entries: Vec<Value> = Vec::new();
    let mut rows: Vec<(String, String, bool)> = std::fs::read_dir(&path)
        .map_err(|e| anyhow!("{e}"))?
        .filter_map(|e| e.ok())
        .map(|e| {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (
                e.file_name().to_string_lossy().into_owned(),
                e.path().to_string_lossy().into_owned(),
                is_dir,
            )
        })
        .collect();
    rows.sort_by(|a, b| {
        let a_hidden = a.0.starts_with('.');
        let b_hidden = b.0.starts_with('.');
        b.2.cmp(&a.2)
            .then(a_hidden.cmp(&b_hidden))
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    for (name, p, is_dir) in rows {
        entries.push(serde_json::json!({ "name": name, "path": p, "is_dir": is_dir }));
    }
    Ok(Value::Array(entries))
}

/// 把编辑器内容写回磁盘（语义同 desktop `write_text_file`）：仅覆盖已存在的常规文件。
async fn cmd_write_text_file(args: Value) -> Result<()> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `path`"))?;
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `content`"))?;
    let path = std::path::PathBuf::from(path);
    let meta = std::fs::metadata(&path).map_err(|e| anyhow!("{e}"))?;
    if !meta.is_file() {
        return Err(anyhow!("not a regular file"));
    }
    std::fs::write(&path, content).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

async fn cmd_revert_edit(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let run_id = args
        .get("runId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `runId`"))?;
    let worktree = build_edits_worktree_for(state, sid)?;
    if !worktree.enabled().await {
        return Err(anyhow!("git 不可用，回退功能已禁用"));
    }
    let runs = worktree.list_runs().map_err(|e| anyhow!("{e}"))?;
    let entry = runs
        .into_iter()
        .find(|e| e.run_id == run_id)
        .ok_or_else(|| anyhow!("找不到该轮修改"))?;
    if entry.reverted {
        return Err(anyhow!("该轮修改已回退过"));
    }
    match worktree.revert_run(&entry).await {
        Ok(()) => {
            worktree
                .mark_run_reverted(run_id)
                .map_err(|e| anyhow!("{e}"))?;
            Ok(json!({ "success": true }))
        }
        Err(e) => Ok(json!({ "success": false, "error": e.to_string() })),
    }
}

async fn cmd_edits_worktree_status(state: &ServerState, args: Value) -> Result<Value> {
    let sid = args
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing `sessionId`"))?;
    let worktree = build_edits_worktree_for(state, sid)?;
    let enabled = worktree.enabled().await;
    let entry_count = if enabled {
        worktree.list_runs().map(|e| e.len()).unwrap_or(0)
    } else {
        0
    };
    Ok(json!({
        "enabled": enabled,
        "entry_count": entry_count,
    }))
}
