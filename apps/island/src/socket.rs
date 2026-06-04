use crate::protocol::SocketMessage;
use crate::window;
use crate::IslandState;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

/// 全局 action 回传路由：msg_id → 该通知所在连接的写端 channel
type ActionRoutes = Arc<RwLock<HashMap<String, mpsc::UnboundedSender<String>>>>;

/// 启动 Unix socket 监听
pub async fn run(app: AppHandle) {
    let sock_path = dirs::home_dir()
        .unwrap()
        .join(".hebbian")
        .join("island.sock");

    let _ = std::fs::remove_file(&sock_path);
    if let Some(parent) = sock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("绑定 island.sock 失败: {e}");
            return;
        }
    };

    info!("hebisland daemon 监听: {}", sock_path.display());

    let routes: ActionRoutes = Arc::new(RwLock::new(HashMap::new()));

    // 启动 action 分发任务：Tauri command 的 action → 通过路由回写给 client
    let state = app.state::<IslandState>();
    let mut action_rx = state.action_tx.subscribe();
    let routes_for_dispatch = routes.clone();
    let app_for_dispatch = app.clone();
    tokio::spawn(async move {
        while let Ok(action) = action_rx.recv().await {
            // 关闭窗口
            window::close_notification_window(&app_for_dispatch, &action.msg_id);
            // 回传 action 给 surface
            let routes = routes_for_dispatch.read().await;
            if let Some(tx) = routes.get(&action.msg_id) {
                let msg = serde_json::to_string(&action).unwrap_or_default();
                let _ = tx.send(msg);
            }
        }
    });

    // accept loop
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let app_clone = app.clone();
                let routes_clone = routes.clone();
                tokio::spawn(async move {
                    handle_client(stream, app_clone, routes_clone).await;
                });
            }
            Err(e) => {
                warn!("accept 失败: {e}");
            }
        }
    }
}

async fn handle_client(stream: tokio::net::UnixStream, app: AppHandle, routes: ActionRoutes) {
    let state = app.state::<IslandState>();
    let (reader_half, mut writer_half) = stream.into_split();
    let reader = tokio::io::BufReader::new(reader_half);
    let mut lines = reader.lines();

    // 本连接注册的 msg_id 列表，连接结束时清理
    let mut registered_ids: Vec<String> = Vec::new();

    // 写端 channel：action dispatch 往里写，这里往 socket 里写
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();

    // 写任务：从 channel 读 action → 写回 socket
    tokio::spawn(async move {
        while let Some(msg) = write_rx.recv().await {
            let mut bytes = msg.into_bytes();
            bytes.push(b'\n');
            if writer_half.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });

    // 读循环
    while let Ok(Some(line)) = lines.next_line().await {
        match serde_json::from_str::<SocketMessage>(&line) {
            Ok(SocketMessage::Show { id, card }) => {
                state
                    .notifications
                    .write()
                    .await
                    .insert(id.clone(), card.clone());
                // 注册 action 路由
                routes.write().await.insert(id.clone(), write_tx.clone());
                registered_ids.push(id.clone());
                if let Err(e) = window::spawn_notification_window(&app, &id, &card) {
                    warn!("创建通知窗口失败: {e}");
                }
            }
            Ok(SocketMessage::Dismiss { id }) => {
                window::close_notification_window(&app, &id);
            }
            Ok(SocketMessage::Action { id, action }) => {
                info!("收到 action 回传: {id} -> {action}");
            }
            Err(e) => {
                warn!("解析 socket 消息失败: {e} | line: {line}");
            }
        }
    }

    // 连接断开，清理路由
    let mut routes = routes.write().await;
    for id in &registered_ids {
        routes.remove(id);
    }
}