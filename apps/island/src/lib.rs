pub mod protocol;
pub mod socket;
pub mod window;

use protocol::{ActionEvent, NotificationCard};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// 全局共享状态
pub struct IslandState {
    /// 前端可查询的 card 数据（socket 收到 show 后写入，前端 mount 时读取）
    pub notifications: Arc<RwLock<HashMap<String, NotificationCard>>>,
    /// action 广播：Tauri command 发送 → socket.rs 接收并关窗口
    pub action_tx: broadcast::Sender<ActionEvent>,
    /// 右上角窗口堆叠顺序（底部 → 顶部），用于计算 y 坐标
    pub window_stack: Arc<RwLock<Vec<String>>>,
}

/// 前端查询 card 数据：mount 时 invoke 获取当前通知的完整数据
#[tauri::command]
async fn island_get_card(
    state: tauri::State<'_, IslandState>,
    id: String,
) -> Result<Option<NotificationCard>, String> {
    Ok(state.notifications.read().await.get(&id).cloned())
}

/// 前端用户操作（审批 allow/deny、打开 open、关闭 dismiss）
#[tauri::command]
fn island_action(state: tauri::State<'_, IslandState>, id: String, action: String) {
    let _ = state.action_tx.send(ActionEvent {
        msg_id: id,
        action,
    });
}

/// 启动 Tauri 应用
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let (action_tx, _) = broadcast::channel::<ActionEvent>(64);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(IslandState {
            notifications: Arc::new(RwLock::new(HashMap::new())),
            action_tx,
            window_stack: Arc::new(RwLock::new(Vec::new())),
        })
        .invoke_handler(tauri::generate_handler![island_get_card, island_action])
        .setup(|app| {
            let app_handle = app.handle().clone();
            // 在 Tauri 事件循环内启动 socket 监听
            tauri::async_runtime::spawn(async move {
                socket::run(app_handle).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动 hebisland 失败");
}