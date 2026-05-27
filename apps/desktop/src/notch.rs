// 右上角事件弹窗提醒
//
// 在 hebbian 窗口不在前台时，通过无边框、置顶、透明的 webview 窗口
// 在屏幕右上角显示事件通知。纯通知，不内嵌交互操作。
//
// 行为：
// - pending 类（approval/question）：持续显示，可折叠/拖拽
// - info 类（turn_completed）：3s 自动消失
// - hebbian 在前台时不弹通知

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Listener, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::engine::EngineEvent;

/// 通知类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NotchType {
    Pending,
    Info,
}

/// 通知事件来源
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NotificationKind {
    PermissionRequested,
    UserQuestion,
    TurnCompleted,
}

/// 通过 Tauri 事件传输的通知 payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NotificationPayload {
    #[serde(rename = "type")]
    pub notch_type: NotchType,
    pub kind: NotificationKind,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
struct NotchEntry {
    payload: NotificationPayload,
}

// 内部状态用单一 Mutex<NotchState> 避免多锁死锁
struct NotchState {
    queue: VecDeque<NotchEntry>,
    active_type: Option<NotchType>,
}

/// 通过 Tauri state 管理的共享状态
pub(crate) struct NotchSharedState(pub(crate) Arc<Mutex<NotchState>>);

pub(crate) fn create_notch_state() -> NotchSharedState {
    NotchSharedState(Arc::new(Mutex::new(NotchState {
        queue: VecDeque::new(),
        active_type: None,
    })))
}

pub(crate) fn initialize_notch(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let hebbian_foreground = Arc::new(AtomicBool::new(true));
    let state = app.state::<NotchSharedState>().inner().0.clone();

    // 监听 hebbian 主窗口的前台/后台变化
    let hf_focus = hebbian_foreground.clone();
    let state_focus = state.clone();
    let app_focus = app.clone();
    let main_window = app
        .get_webview_window("main")
        .ok_or("main window not found")?;
    main_window.on_window_event(move |event| {
        use tauri::WindowEvent;
        if let WindowEvent::Focused(focused) = event {
            hf_focus.store(*focused, Ordering::Relaxed);
            if *focused {
                // hebbian 回到前台 → 隐藏所有通知并清空队列
                if let Some(w) = app_focus.get_webview_window(WINDOW_LABEL) {
                    let _ = w.hide();
                }
                let mut s = state_focus.lock().expect("notch state poisoned");
                s.queue.clear();
                s.active_type = None;
            }
        }
    });

    // 监听 "notification" 事件（由 chat.rs emit）
    let hf_emit = hebbian_foreground.clone();
    let state_emit = state.clone();
    let app_emit = app.clone();
    app.listen("notification", move |event| {
        let payload: NotificationPayload = match serde_json::from_str(event.payload()) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse notification payload");
                return;
            }
        };

        if hf_emit.load(Ordering::Relaxed) {
            tracing::debug!("hebbian in foreground, suppressing notification");
            return;
        }

        let mut s = state_emit.lock().expect("notch state poisoned");

        match &payload.notch_type {
            NotchType::Pending => {
                // pending 立即抢占，替换掉当前 info（如果有）
                s.active_type = Some(NotchType::Pending);
                // 丢掉队列里所有 info，只保留 pending
                s.queue
                    .retain(|e| e.payload.notch_type == NotchType::Pending);
                drop(s);
                show_or_update_notch(&app_emit, &payload);
            }
            NotchType::Info => {
                if s.active_type.is_some() {
                    // 有其他通知正在显示，排队
                    s.queue.push_back(NotchEntry { payload });
                } else {
                    s.active_type = Some(NotchType::Info);
                    drop(s);
                    show_or_update_notch(&app_emit, &payload);
                }
            }
        }
    });

    // 提前创建窗口（初始隐藏），避免首次通知延迟
    ensure_notch_window(app);

    Ok(())
}

const WINDOW_LABEL: &str = "notch";
const WINDOW_W: f64 = 360.0;

fn ensure_notch_window(app: &AppHandle) {
    if app.get_webview_window(WINDOW_LABEL).is_some() {
        return;
    }
    let result = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("/?notch=1".into()))
        .decorations(false)
        .always_on_top(true)
        .visible(false)
        .transparent(true)
        .skip_taskbar(true)
        .focused(false)
        // 宽固定，高由前端内容决定（会通过 notify_resize 命令调整）
        .inner_size(WINDOW_W, 120.0)
        .build();

    match result {
        Ok(_) => tracing::info!("notch window created"),
        Err(e) => tracing::error!(error = %e, "failed to create notch window"),
    }
}

/// 消费队列中的下一个通知（最高优先级），如果队列非空则显示。
fn flush(app: &AppHandle) {
    let state = app.state::<Arc<Mutex<NotchState>>>();
    let mut s = state.lock().unwrap();
    // 如果正在显示，先 dismiss
    if s.active_type.is_some() {
        return; // 已有活跃通知，等 dismiss 时再 flush
    }
    if let Some(entry) = s.queue.pop_front() {
        s.active_type = Some(entry.payload.notch_type.clone());
        drop(s);
        show_or_update_notch(app, &entry.payload);
    }
}

fn show_or_update_notch(app: &AppHandle, payload: &NotificationPayload) {
    ensure_notch_window(app);

    let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
        return;
    };

    // 把 payload 序列化成 JSON 字符串，再 base64/转义确保 eval 安全
    // 用 JSON.parse(atob(...)) 避免单引号/反斜杠等字符 break eval
    let json_bytes = serde_json::to_vec(payload).unwrap_or_default();
    let b64 = base64_encode(&json_bytes);
    let _ = window.eval(&format!(
        "window.dispatchEvent(new CustomEvent('notch-update',{{detail:JSON.parse(atob('{}'))}}));",
        b64
    ));

    // 定位到屏幕右上角（使用 LogicalPosition，避免 DPI 双重缩放）
    // 仅首次显示时定位，之后用户拖动后不再重设
    if !window.is_visible().unwrap_or(false) {
        if let Ok(Some(monitor)) = window.primary_monitor() {
            let size = monitor.size();
            let scale = monitor.scale_factor();
            let lw = size.width as f64 / scale;
            let x = lw - WINDOW_W - 20.0;
            let y = 30.0;
            let _ = window.set_position(tauri::LogicalPosition::new(x, y));
        }
    }

    let _ = window.show();
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        out.push(CHARS[(b0 >> 2)] as char);
        out.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((b1 & 0xf) << 2) | (b2 >> 6)] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[b2 & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// --- Tauri 命令 ---

/// 用户关闭当前通知（前端 ✕ 按钮触发）
#[tauri::command]
pub(crate) fn notify_dismiss(app: AppHandle) {
    dismiss_current(&app);
}

/// 用户点击卡片主体 → 关闭通知并调起 hebbian 主窗口
#[tauri::command]
pub(crate) fn notify_click(app: AppHandle) {
    dismiss_current(&app);
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
    }
}

/// 前端拖拽结束后更新窗口位置
#[tauri::command]
pub(crate) fn notify_set_position(app: AppHandle, x: f64, y: f64) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.set_position(tauri::LogicalPosition::new(x, y));
    }
}

/// 前端折叠/展开后更新窗口高度
#[tauri::command]
pub(crate) fn notify_resize(app: AppHandle, height: f64) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.set_size(tauri::LogicalSize::new(WINDOW_W, height));
    }
}

fn dismiss_current(app: &AppHandle) {
    let state = app.state::<NotchSharedState>();
    let mut s = state.inner().0.lock().expect("notch state poisoned");
    s.active_type = None;

    if let Some(next) = s.queue.pop_front() {
        s.active_type = Some(next.payload.notch_type.clone());
        let payload = next.payload.clone();
        drop(s);
        show_or_update_notch(app, &payload);
    } else {
        drop(s);
        if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
            let _ = window.hide();
        }
    }
}

/// 从 chat.rs 的 event loop 调用：将 EngineEvent 转为 notch 通知并 push 到队列
pub(crate) fn emit_notification(app: &AppHandle, event: &EngineEvent) {
    let payload = match event {
        EngineEvent::PermissionRequested {
            tool_name, input, ..
        } => NotificationPayload {
            notch_type: NotchType::Pending,
            kind: NotificationKind::PermissionRequested,
            title: "需要你的审批".into(),
            summary: format!(
                "{} {}",
                tool_name,
                input.to_string().chars().take(80).collect::<String>()
            ),
        },
        EngineEvent::UserQuestionRequested { question, .. } => NotificationPayload {
            notch_type: NotchType::Pending,
            kind: NotificationKind::UserQuestion,
            title: "需要你的回答".into(),
            summary: question.clone(),
        },
        EngineEvent::TurnFinished { .. } => NotificationPayload {
            notch_type: NotchType::Info,
            kind: NotificationKind::TurnCompleted,
            title: "回答完成".into(),
            summary: "Agent 已完成当前回合".into(),
        },
        _ => return,
    };

    // 获取 state 并插入队列
    if let Some(state) = app.try_state::<Arc<Mutex<NotchState>>>() {
        let mut s = state.lock().unwrap();
        // 同类 pending 事件：替换而不是追加（避免重复）
        if payload.notch_type == NotchType::Pending {
            s.queue
                .retain(|e| e.payload.notch_type != NotchType::Pending);
        }
        s.queue.push_back(NotchEntry { payload });
        drop(s);
        flush(app);
    }
}
