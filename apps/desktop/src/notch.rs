// 右上角事件弹窗提醒
//
// 通过无边框、置顶、透明、不抢焦点的 webview 窗口在屏幕右上角显示事件通知。
//
// 行为：
// - pending 类（审批 / 提问）：持续显示，手动关闭；tool_call 审批可在卡片上直接「允许本次 / 拒绝」
// - info 类（回答完成）：整轮 run 结束才弹一次，3s 自动消失
//
// 弹出策略见 [`NOTCH_ALWAYS_POP`]：生产形态仅在主窗口不在前台时弹（前台靠侧边栏光晕 +
// 应用内弹窗），调试期可设 true 让前台也弹。

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::engine::EngineEvent;

/// 弹出策略开关：
/// - `false`（生产）：notch 仅在主窗口**不在前台**时弹——前台时用户已能看到侧边栏光晕
///   和应用内审批弹窗，再弹 notch 纯属打扰。
/// - `true`（调试）：前台也弹，方便不切走窗口就能迭代卡片外观 / 交互。
const NOTCH_ALWAYS_POP: bool = true;

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
    /// pending 类（审批 / 提问）携带对应 HITL 请求 id，用于在审批被解决后
    /// 定向撤销这条通知（见 [`resolve_notification`]）。info 类为 None。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub request_id: Option<String>,
    /// 审批子类型（"tool_call" / "path_access" / "plan"）。仅 "tool_call" 能在卡片上
    /// 直接「允许本次 / 拒绝」（path_access / plan 有各自更复杂的 scope / 编辑流程，
    /// 卡片只给「打开处理」跳回主窗口）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub perm_kind: Option<String>,
}

#[derive(Debug, Clone)]
struct NotchEntry {
    payload: NotificationPayload,
}

// 内部状态用单一 Mutex<NotchState> 避免多锁死锁
struct NotchState {
    queue: VecDeque<NotchEntry>,
    active_type: Option<NotchType>,
    /// 当前正在显示的通知所属的 HITL 请求 id（仅 pending 类有），用于审批
    /// 被解决时判断「正在显示的就是它」并撤销。
    active_request_id: Option<String>,
    /// 主窗口是否在前台。`NOTCH_ALWAYS_POP == false` 时据此抑制前台弹窗。
    main_focused: bool,
}

/// 通过 Tauri state 管理的共享状态
pub(crate) struct NotchSharedState(pub(crate) Arc<Mutex<NotchState>>);

pub(crate) fn create_notch_state() -> NotchSharedState {
    NotchSharedState(Arc::new(Mutex::new(NotchState {
        queue: VecDeque::new(),
        active_type: None,
        active_request_id: None,
        // 进程启动时主窗口默认在前台；Focused 事件会随后纠正。
        main_focused: true,
    })))
}

/// 是否应抑制本次通知（生产形态：主窗口在前台时不弹）。
fn should_suppress(s: &NotchState) -> bool {
    !NOTCH_ALWAYS_POP && s.main_focused
}

pub(crate) fn initialize_notch(app: &AppHandle) {
    // 提前创建窗口（初始隐藏），避免首次通知延迟。
    // 通知的入队 / 展示统一走 emit_notification（由 chat.rs 的 event loop 调用）。
    ensure_notch_window(app);

    // 跟踪主窗口前台状态，供「仅后台弹」策略判断（NOTCH_ALWAYS_POP == false 时）。
    if let Some(main) = app.get_webview_window("main") {
        let state = app.state::<NotchSharedState>().inner().0.clone();
        main.on_window_event(move |event| {
            if let tauri::WindowEvent::Focused(focused) = event {
                if let Ok(mut s) = state.lock() {
                    s.main_focused = *focused;
                }
            }
        });
    }
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
        // 不可获得键盘焦点 → show() 不会把主窗口的焦点抢走（鼠标点击按钮仍可用）。
        .focusable(false)
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
    let state = app.state::<NotchSharedState>();
    let mut s = state.0.lock().unwrap();
    // 如果正在显示，先 dismiss
    if s.active_type.is_some() {
        return; // 已有活跃通知，等 dismiss 时再 flush
    }
    if let Some(entry) = s.queue.pop_front() {
        s.active_type = Some(entry.payload.notch_type.clone());
        s.active_request_id = entry.payload.request_id.clone();
        drop(s);
        show_or_update_notch(app, &entry.payload);
    }
}

fn show_or_update_notch(app: &AppHandle, payload: &NotificationPayload) {
    ensure_notch_window(app);

    let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
        return;
    };

    // payload 序列化成 JSON → base64，避免单引号/反斜杠等字符 break eval。
    // atob 只还原出「每字符=一字节」的 Latin-1 串，不会解 UTF-8 多字节序列，
    // 中文会乱码——故先按字节重建 Uint8Array，再用 TextDecoder 按 UTF-8 解码。
    let json_bytes = serde_json::to_vec(payload).unwrap_or_default();
    let b64 = base64_encode(&json_bytes);
    let _ = window.eval(&format!(
        "window.dispatchEvent(new CustomEvent('notch-update',{{detail:JSON.parse(new TextDecoder().decode(Uint8Array.from(atob('{}'),c=>c.charCodeAt(0))))}}));",
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
    s.active_request_id = None;

    if let Some(next) = s.queue.pop_front() {
        s.active_type = Some(next.payload.notch_type.clone());
        s.active_request_id = next.payload.request_id.clone();
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

/// 从 chat.rs 的 event loop 调用：将审批 / 提问事件转为 notch 通知。
/// 「回答完成」不在这里——它按整轮 run 结束触发，见 [`emit_run_finished`]。
pub(crate) fn emit_notification(app: &AppHandle, event: &EngineEvent) {
    let payload = match event {
        EngineEvent::PermissionRequested {
            request_id,
            kind,
            tool_name,
            input,
            ..
        } => NotificationPayload {
            notch_type: NotchType::Pending,
            kind: NotificationKind::PermissionRequested,
            title: "需要你的审批".into(),
            summary: format!(
                "{} {}",
                tool_name,
                input.to_string().chars().take(80).collect::<String>()
            ),
            request_id: Some(request_id.clone()),
            perm_kind: Some(kind.clone()),
        },
        EngineEvent::UserQuestionRequested {
            request_id,
            question,
            ..
        } => NotificationPayload {
            notch_type: NotchType::Pending,
            kind: NotificationKind::UserQuestion,
            title: "需要你的回答".into(),
            summary: question.clone(),
            request_id: Some(request_id.clone()),
            perm_kind: None,
        },
        // 审批 / 提问被解决 → 撤销对应那条 pending 通知（用户在主窗口已处理）。
        EngineEvent::PermissionResolved { request_id, .. }
        | EngineEvent::UserQuestionAnswered { request_id, .. } => {
            resolve_notification(app, request_id);
            return;
        }
        _ => return,
    };
    enqueue(app, payload);
}

/// 整轮 run（一次用户输入引发的完整回答，可能多回合）结束时弹一次「回答完成」。
/// 由 chat.rs 在 run 成功返回后调用——避免每个 TurnFinished 都弹。
pub(crate) fn emit_run_finished(app: &AppHandle) {
    enqueue(
        app,
        NotificationPayload {
            notch_type: NotchType::Info,
            kind: NotificationKind::TurnCompleted,
            title: "回答完成".into(),
            summary: "Agent 已完成本次回答".into(),
            request_id: None,
            perm_kind: None,
        },
    );
}

/// 入队 + 触发显示。受 [`should_suppress`] 策略门控。
fn enqueue(app: &AppHandle, payload: NotificationPayload) {
    if let Some(state) = app.try_state::<NotchSharedState>() {
        let mut s = state.0.lock().unwrap();
        if should_suppress(&s) {
            return;
        }
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

/// 审批 / 提问被解决时撤销对应的 pending 通知：
/// - 从队列里剔除同 request_id 的待显示条目；
/// - 若正在显示的就是它，则关闭并推进到下一条（或隐藏窗口）。
fn resolve_notification(app: &AppHandle, request_id: &str) {
    let Some(state) = app.try_state::<NotchSharedState>() else {
        return;
    };
    let mut s = state.0.lock().unwrap();
    s.queue
        .retain(|e| e.payload.request_id.as_deref() != Some(request_id));
    let is_active = s.active_request_id.as_deref() == Some(request_id);
    drop(s);
    if is_active {
        dismiss_current(app);
    }
}
