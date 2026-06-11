//! 内置浏览器：Tauri 子 webview 生命周期 + 注释注入（架构 §8.5）。
//!
//! 承载选型 B：Desktop 用 multi-webview（unstable feature）的 `add_child` 叠一个
//! 独立 webview 直接加载目标 URL——真 cookie / 登录 / 任意公网。inspector.js 经
//! `initialization_script` 每次导航自动注入。
//!
//! 双向信道（外部 URL 子 webview 没有 Tauri IPC，remote 域不开 invoke）：
//!   - 上行 inspector → Rust：`location.replace("heb-bridge://...")`，被
//!     `on_navigation` 拦截解析后 return false（页面无感知）。
//!   - 下行 Rust → inspector：`webview.eval("window.__HEB_RX__(<json>)")`。
//!
//! 安全边界：`on_navigation` 对真实导航跑两档校验（§8.5-4），页面内跳转同样拦截。

mod url_policy;

use std::sync::Mutex;

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Rect, Url, WebviewUrl, WindowEvent,
    WebviewWindowBuilder,
};

use url_policy::{validate_preview_url, PreviewOrigin};

use crate::chat::{self, SendArgs};
use crate::engine::EngineEvent;

const WEBVIEW_LABEL: &str = "heb-preview";
const POPOUT_LABEL: &str = "preview-popout";
const INSPECTOR_JS: &str = include_str!("inspector.js");

/// 注入到子 webview 的引导脚本：先装 inspector.js，再安装上行 bridge。
/// inspector.js 内部检测到非 iframe 环境（子 webview）时，上行走 heb-bridge 导航，
/// 由本模块在 webview 创建时通过 init script 提供（inspector 自身已实现，无需重复）。
fn init_script() -> String {
    INSPECTOR_JS.to_string()
}

/// 子 webview 实例 + 自维护导航历史（Webview API 未暴露 go_back/forward）。
struct BrowserInstance {
    webview: tauri::Webview,
    history: Vec<String>,
    cursor: usize,
    /// 标记下一次 on_navigation 是程序触发（navigate/back/forward），避免重复入栈。
    programmatic: bool,
    picker_active: bool,
}

#[derive(Default)]
pub struct BrowserState {
    inner: Mutex<Option<BrowserInstance>>,
    /// 「元素对话」旁支会话用的上下文：主对话 session + provider/model。
    /// 主窗口 React 在浏览器 tab 活跃 / 切会话时通过 browser_set_context 喂进来。
    aside_context: Mutex<Option<AsideContext>>,
}

#[derive(Clone)]
struct AsideContext {
    main_session_id: String,
    provider_id: String,
    model: String,
    /// 可选模型列表（[{providerId, model, label}]），供卡片里的模型选择器用。
    models: serde_json::Value,
}

#[derive(Serialize, Clone)]
struct BrowserStateEvent {
    url: String,
    can_go_back: bool,
    can_go_forward: bool,
    loading: bool,
}

fn emit_state(app: &AppHandle, inst: &BrowserInstance, loading: bool) {
    let evt = BrowserStateEvent {
        url: inst.history.get(inst.cursor).cloned().unwrap_or_default(),
        can_go_back: inst.cursor > 0,
        can_go_forward: inst.cursor + 1 < inst.history.len(),
        loading,
    };
    let _ = app.emit("browser://state", evt);
}

/// 解析 heb-bridge://msg/?d=<urlencoded json> 上行信封。
fn decode_bridge(url: &Url) -> Option<serde_json::Value> {
    if url.scheme() != "heb-bridge" {
        return None;
    }
    let encoded = url.query_pairs().find(|(k, _)| k == "d").map(|(_, v)| v.into_owned())?;
    serde_json::from_str(&encoded).ok()
}

/// 把下行消息 eval 进 inspector。
fn send_down(inst: &BrowserInstance, ty: &str, payload: serde_json::Value) -> Result<(), String> {
    let msg = serde_json::json!({ "source": "hebbian-host", "type": ty, "payload": payload });
    let js = format!("window.__HEB_RX__ && window.__HEB_RX__({})", msg);
    inst.webview.eval(&js).map_err(|e| e.to_string())
}

/// 创建子 webview 并加载首个 URL。已存在则复用（先关再建，保证 init script 干净）。
#[tauri::command]
pub fn browser_open(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    url: String,
    origin: PreviewOrigin,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<String, String> {
    let target = validate_preview_url(&url, origin)?;
    let target_str = target.to_string();

    // 关掉旧实例
    if let Some(old) = state.inner.lock().unwrap().take() {
        let _ = old.webview.close();
    }

    let window = app
        .get_window("main")
        .ok_or_else(|| "主窗口不存在".to_string())?;

    let app_for_nav = app.clone();
    let app_for_load = app.clone();
    let builder = tauri::webview::WebviewBuilder::new(WEBVIEW_LABEL, WebviewUrl::External(target.clone()))
        .initialization_script(init_script())
        .on_navigation(move |url: &Url| handle_navigation(&app_for_nav, url))
        .on_page_load(move |_wv, payload| {
            let loading = matches!(payload.event(), tauri::webview::PageLoadEvent::Started);
            if let Some(st) = app_for_load.try_state::<BrowserState>() {
                if let Some(inst) = st.inner.lock().unwrap().as_ref() {
                    emit_state(&app_for_load, inst, loading);
                }
            }
        });

    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(width.max(1.0), height.max(1.0)),
        )
        .map_err(|e| e.to_string())?;

    let inst = BrowserInstance {
        webview,
        history: vec![target_str.clone()],
        cursor: 0,
        programmatic: true, // 首次加载也算程序触发，on_navigation 不重复入栈
        picker_active: false,
    };
    *state.inner.lock().unwrap() = Some(inst);
    Ok(target_str)
}

/// on_navigation 回调：bridge 消息转发给前端；真实导航做安全校验 + 历史维护。
/// 返回 false 阻断本次导航。
fn handle_navigation(app: &AppHandle, url: &Url) -> bool {
    // 上行 bridge：解析转发，永不真导航
    if let Some(msg) = decode_bridge(url) {
        forward_inspector_message(app, msg);
        return false;
    }
    // about:blank 等内部页放行不记录
    if url.scheme() != "http" && url.scheme() != "https" {
        return true;
    }
    // 页面内跳转按 user 档校验（用户点链接 = 主动行为）
    match validate_preview_url(url.as_str(), PreviewOrigin::User) {
        Ok(_) => {
            if let Some(st) = app.try_state::<BrowserState>() {
                let mut guard = st.inner.lock().unwrap();
                if let Some(inst) = guard.as_mut() {
                    if inst.programmatic {
                        inst.programmatic = false;
                    } else {
                        // 用户点链接：截断前进栈后入栈
                        inst.history.truncate(inst.cursor + 1);
                        inst.history.push(url.to_string());
                        inst.cursor = inst.history.len() - 1;
                    }
                    emit_state(app, inst, true);
                }
            }
            true
        }
        Err(reason) => {
            let _ = app.emit("browser://escaped", serde_json::json!({ "url": url.to_string(), "reason": reason }));
            false
        }
    }
}

fn forward_inspector_message(app: &AppHandle, msg: serde_json::Value) {
    let ty = msg.get("type").and_then(|v| v.as_str()).unwrap_or_default();
    let payload = msg.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    if std::env::var("HEBBIAN_WEBVIEW_SPIKE").as_deref() == Ok("1") {
        let preview = serde_json::to_string(&payload).unwrap_or_default();
        let preview: String = preview.chars().take(240).collect();
        tracing::info!(target: "webview_spike", "fwd {ty}: {preview}");
    }
    match ty {
        // 页面内注释卡片提交：{snapshot, comment, styleDiff} → 主窗口 React 组装成 user message
        "heb:annotation:submit" => {
            let _ = app.emit("browser://annotation", payload);
        }
        // 元素对话（旁支会话，机制 B）
        "heb:aside:send" => handle_aside_send(app, &payload),
        "heb:aside:submit" => handle_aside_submit(app, &payload),
        "heb:aside:models:request" => {
            let surface = payload.get("surface").and_then(|v| v.as_str()).unwrap_or("embedded");
            let ctx = app
                .try_state::<BrowserState>()
                .and_then(|st| st.aside_context.lock().unwrap().clone());
            if let Some(ctx) = ctx {
                eval_aside_down(app, surface, "heb:aside:models", serde_json::json!({
                    "list": ctx.models,
                    "current": { "providerId": ctx.provider_id, "model": ctx.model },
                }));
            }
        }
        "heb:picker:cancelled" => {
            let _ = app.emit("browser://picker-off", ());
        }
        "heb:ready" | "heb:navigated" => {
            // 携带 title，补一条 state（loading=false）
            let _ = app.emit("browser://title", payload);
        }
        _ => {}
    }
}

#[tauri::command]
pub fn browser_navigate(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    url: String,
) -> Result<String, String> {
    // 地址栏输入 = user 档
    let target = validate_preview_url(&url, PreviewOrigin::User)?;
    let mut guard = state.inner.lock().unwrap();
    let inst = guard.as_mut().ok_or_else(|| "浏览器未打开".to_string())?;
    inst.history.truncate(inst.cursor + 1);
    inst.history.push(target.to_string());
    inst.cursor = inst.history.len() - 1;
    inst.programmatic = true;
    inst.webview.navigate(target.clone()).map_err(|e| e.to_string())?;
    emit_state(&app, inst, true);
    Ok(target.to_string())
}

#[tauri::command]
pub fn browser_back(app: AppHandle, state: tauri::State<'_, BrowserState>) -> Result<(), String> {
    let mut guard = state.inner.lock().unwrap();
    let inst = guard.as_mut().ok_or_else(|| "浏览器未打开".to_string())?;
    if inst.cursor == 0 {
        return Ok(());
    }
    inst.cursor -= 1;
    inst.programmatic = true;
    let url: Url = inst.history[inst.cursor].parse().map_err(|_| "历史地址异常".to_string())?;
    inst.webview.navigate(url).map_err(|e| e.to_string())?;
    emit_state(&app, inst, true);
    Ok(())
}

#[tauri::command]
pub fn browser_forward(app: AppHandle, state: tauri::State<'_, BrowserState>) -> Result<(), String> {
    let mut guard = state.inner.lock().unwrap();
    let inst = guard.as_mut().ok_or_else(|| "浏览器未打开".to_string())?;
    if inst.cursor + 1 >= inst.history.len() {
        return Ok(());
    }
    inst.cursor += 1;
    inst.programmatic = true;
    let url: Url = inst.history[inst.cursor].parse().map_err(|_| "历史地址异常".to_string())?;
    inst.webview.navigate(url).map_err(|e| e.to_string())?;
    emit_state(&app, inst, true);
    Ok(())
}

#[tauri::command]
pub fn browser_reload(state: tauri::State<'_, BrowserState>) -> Result<(), String> {
    let mut guard = state.inner.lock().unwrap();
    let inst = guard.as_mut().ok_or_else(|| "浏览器未打开".to_string())?;
    let url: Url = inst
        .history
        .get(inst.cursor)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "无当前地址".to_string())?;
    inst.programmatic = true;
    inst.webview.navigate(url).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn browser_set_bounds(
    state: tauri::State<'_, BrowserState>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let guard = state.inner.lock().unwrap();
    let inst = match guard.as_ref() {
        Some(i) => i,
        None => return Ok(()), // 面板未挂载时静默
    };
    inst.webview
        .set_bounds(Rect {
            position: LogicalPosition::new(x, y).into(),
            size: LogicalSize::new(width.max(1.0), height.max(1.0)).into(),
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn browser_set_visible(state: tauri::State<'_, BrowserState>, visible: bool) -> Result<(), String> {
    let guard = state.inner.lock().unwrap();
    if let Some(inst) = guard.as_ref() {
        if visible {
            inst.webview.show().map_err(|e| e.to_string())?;
        } else {
            inst.webview.hide().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn browser_close(state: tauri::State<'_, BrowserState>) -> Result<(), String> {
    if let Some(inst) = state.inner.lock().unwrap().take() {
        inst.webview.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 把当前页面弹出成一个独立可缩放窗口（测响应式样式用）。窗口直接加载目标 URL，
/// 注入同一份 inspector.js——页面内注释卡片（vanilla DOM）与 embedded 共用，
/// 提交经 heb-bridge 上行回主进程，再由主窗口 React 发进对话。
#[tauri::command]
pub fn browser_popout(app: AppHandle, state: tauri::State<'_, BrowserState>) -> Result<(), String> {
    // 没有当前页时弹一个空白窗口——用户可在 popout 自带的地址栏里输网址
    let url = state
        .inner
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|i| i.history.get(i.cursor).cloned())
        .unwrap_or_else(|| "about:blank".to_string());
    let target: Url = url.parse().map_err(|_| "当前地址异常".to_string())?;

    // 已有 popout 先关，避免重复
    if let Some(w) = app.get_webview_window(POPOUT_LABEL) {
        let _ = w.close();
    }

    let app_for_nav = app.clone();
    // 注入 __HEB_POPOUT__ 标记：inspector 据此在页面内渲染工具栏（地址栏/导航/选取）
    let popout_script = format!("window.__HEB_POPOUT__=true;\n{INSPECTOR_JS}");
    let win = WebviewWindowBuilder::new(&app, POPOUT_LABEL, WebviewUrl::External(target))
        .title("页面预览（可缩放测样式）")
        .inner_size(1280.0, 800.0)
        .resizable(true)
        .initialization_script(popout_script)
        .on_navigation(move |url: &Url| {
            // popout 不维护 embedded 的历史，只做上行转发 + 安全校验
            if let Some(msg) = decode_bridge(url) {
                forward_inspector_message(&app_for_nav, msg);
                return false;
            }
            if url.scheme() != "http" && url.scheme() != "https" {
                return true;
            }
            validate_preview_url(url.as_str(), PreviewOrigin::User).is_ok()
        })
        .build()
        .map_err(|e| e.to_string())?;

    // popout 窗口关闭（OS 关 / 收回）时通知前端恢复内嵌浏览器
    let app_for_close = app.clone();
    win.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed | WindowEvent::CloseRequested { .. }) {
            let _ = app_for_close.emit("browser://popout", serde_json::json!({ "open": false }));
        }
    });

    // 通知前端：已弹出 → 内嵌浏览器让位显示占位
    let _ = app.emit("browser://popout", serde_json::json!({ "open": true }));
    Ok(())
}

/// 关闭独立预览窗口。
#[tauri::command]
pub fn browser_close_popout(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(POPOUT_LABEL) {
        w.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

// 以下「命令 inspector」的命令在没有 webview 时是无操作（返回 Ok）——没浏览器可命令
// 就啥也不做，不是错误。否则面板挂载/切 tab 等 fire-and-forget 调用会抛"浏览器未打开"
// 被全局 unhandledrejection 弹 toast（启动即弹的根因）。

#[tauri::command]
pub fn browser_picker(state: tauri::State<'_, BrowserState>, active: bool) -> Result<(), String> {
    let mut guard = state.inner.lock().unwrap();
    let Some(inst) = guard.as_mut() else { return Ok(()) };
    inst.picker_active = active;
    send_down(inst, if active { "heb:picker:start" } else { "heb:picker:stop" }, serde_json::json!({}))
}

#[tauri::command]
pub fn browser_style_apply(
    state: tauri::State<'_, BrowserState>,
    prop: String,
    value: String,
) -> Result<(), String> {
    let guard = state.inner.lock().unwrap();
    let Some(inst) = guard.as_ref() else { return Ok(()) };
    send_down(inst, "heb:style:apply", serde_json::json!({ "prop": prop, "value": value }))
}

#[tauri::command]
pub fn browser_style_revert(state: tauri::State<'_, BrowserState>) -> Result<(), String> {
    let guard = state.inner.lock().unwrap();
    let Some(inst) = guard.as_ref() else { return Ok(()) };
    send_down(inst, "heb:style:revert", serde_json::json!({}))
}

/// 请求 inspector 回吐当前 styleDiff（异步：结果经 browser://style-diff 事件返回）。
#[tauri::command]
pub fn browser_style_take_diff(state: tauri::State<'_, BrowserState>) -> Result<(), String> {
    let guard = state.inner.lock().unwrap();
    let Some(inst) = guard.as_ref() else { return Ok(()) };
    send_down(inst, "heb:style:take-diff", serde_json::json!({}))
}

/// 清除选中态（注释卡片关闭 / 切走 tab 时调）。无浏览器时无操作。
#[tauri::command]
pub fn browser_clear_selection(state: tauri::State<'_, BrowserState>) -> Result<(), String> {
    let guard = state.inner.lock().unwrap();
    let Some(inst) = guard.as_ref() else { return Ok(()) };
    send_down(inst, "heb:selection:clear", serde_json::json!({}))
}

// ─────────────────────── 元素对话（旁支会话，机制 B）───────────────────────

/// 主窗口 React 喂进当前对话上下文——旁支会话建会话要 provider/model，提交总结要主 session。
#[tauri::command]
pub fn browser_set_context(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
    provider_id: String,
    model: String,
    models: Option<serde_json::Value>,
) -> Result<(), String> {
    *state.aside_context.lock().unwrap() = Some(AsideContext {
        main_session_id: session_id,
        provider_id,
        model,
        models: models.unwrap_or(serde_json::Value::Null),
    });
    Ok(())
}

fn aside_system_prompt(element_desc: &str) -> String {
    format!(
        "你正在帮用户调整一个网页元素的样式。当前元素：{element_desc}。\n\
         你可以调用 PreviewStyle(prop, value) 工具实时改这个元素的外观（颜色 color、字号 font-size、\
         字重 font-weight、间距 padding/margin、圆角 border-radius、边框 border-width/border-color、\
         背景 background-color 等），用户会立刻在页面上看到效果。一次调一个属性，想微调就再调一次。\n\
         先理解用户想要什么视觉效果，再动手改；改完用一句话说明你做了什么。保持简洁，别长篇大论。"
    )
}

/// 把一条下行消息 eval 到来源 webview（embedded 子 webview 或 popout 窗口）。
fn eval_aside_down(app: &AppHandle, surface: &str, ty: &str, payload: serde_json::Value) {
    let msg = serde_json::json!({ "source": "hebbian-host", "type": ty, "payload": payload });
    let js = format!("window.__HEB_RX__ && window.__HEB_RX__({msg})");
    if surface == "popout" {
        if let Some(w) = app.get_webview_window(POPOUT_LABEL) {
            let _ = w.eval(&js);
        }
    } else if let Some(st) = app.try_state::<BrowserState>() {
        if let Some(inst) = st.inner.lock().unwrap().as_ref() {
            let _ = inst.webview.eval(&js);
        }
    }
}

/// 把旁支会话的事件流路由下发到来源 webview：文本增量 + PreviewStyle 实时应用 + 结束。
fn route_aside_event(app: &AppHandle, surface: &str, session_id: &str, event: EngineEvent) {
    match event {
        EngineEvent::TextDelta { text, .. } => {
            eval_aside_down(app, surface, "heb:aside:delta", serde_json::json!({ "sessionId": session_id, "text": text }));
        }
        EngineEvent::ToolStart { name, input, .. } if name == "PreviewStyle" => {
            let prop = input.get("prop").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let value = input.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
            // 实时应用到页面 + 在聊天里显示这一步
            eval_aside_down(app, surface, "heb:aside:apply", serde_json::json!({ "sessionId": session_id, "prop": prop, "value": value }));
        }
        EngineEvent::RunFinished { .. } => {
            eval_aside_down(app, surface, "heb:aside:done", serde_json::json!({ "sessionId": session_id }));
        }
        _ => {}
    }
}

fn fresh_cancel() -> common::CancelFlag {
    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
}

fn aside_send_args(session_id: String, user_content: String, enabled_tools: Vec<String>) -> SendArgs {
    SendArgs {
        session_id,
        user_content,
        attachments: vec![],
        user_meta: None,
        stream: true,
        enabled_tools,
        cancel_flag: fresh_cancel(),
        pending_inputs: None,
        consumed_pending_inputs: None,
        pending_inputs_accepting: None,
        hitl: None,            // 旁支只有 PreviewStyle（无副作用），不触发审批
        permission_store: None,
        force_automode: false,
        request_id: Some(format!("aside-{}", chrono::Utc::now().timestamp_millis())),
        continue_run: false,
    }
}

/// 处理 heb:aside:send：建/续旁支会话，驱动一轮，事件流下发卡片。
fn handle_aside_send(app: &AppHandle, payload: &serde_json::Value) {
    let surface = payload.get("surface").and_then(|v| v.as_str()).unwrap_or("embedded").to_string();
    let element_key = payload.get("elementKey").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let session_id_opt = payload.get("sessionId").and_then(|v| v.as_str()).map(|s| s.to_string());
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let element_desc = payload.get("element").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // 卡片里模型选择器选的 provider/model（可空 → 用上下文默认）
    let sel_provider = payload.get("providerId").and_then(|v| v.as_str()).map(|s| s.to_string());
    let sel_model = payload.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let ctx = app2
            .state::<BrowserState>()
            .aside_context
            .lock()
            .unwrap()
            .clone();
        let Some(ctx) = ctx else {
            eval_aside_down(&app2, &surface, "heb:aside:error", serde_json::json!({ "message": "先在主窗口打开一个对话，元素对话才有上下文" }));
            return;
        };
        let dd = agent_core::storage::default_data_dir();
        let provider_id = sel_provider.unwrap_or_else(|| ctx.provider_id.clone());
        let model = sel_model.unwrap_or_else(|| ctx.model.clone());
        let session_id = match session_id_opt {
            Some(s) => s,
            None => match agent_core::storage::sessions::create(
                &dd,
                provider_id,
                model,
                Some(aside_system_prompt(&element_desc)),
                None,
            ) {
                Ok(s) => {
                    eval_aside_down(&app2, &surface, "heb:aside:session", serde_json::json!({ "elementKey": element_key, "sessionId": s.id }));
                    s.id
                }
                Err(e) => {
                    eval_aside_down(&app2, &surface, "heb:aside:error", serde_json::json!({ "message": format!("建会话失败：{e}") }));
                    return;
                }
            },
        };
        let args = aside_send_args(session_id.clone(), text, vec!["PreviewStyle".to_string()]);
        let app3 = app2.clone();
        let surface2 = surface.clone();
        let sid = session_id.clone();
        let result = chat::send_and_save_in_data_dir(&dd, args, move |event| {
            route_aside_event(&app3, &surface2, &sid, event);
        })
        .await;
        if let Err(e) = result {
            eval_aside_down(&app2, &surface, "heb:aside:error", serde_json::json!({ "message": format!("助手出错：{e}") }));
        }
    });
}

/// 处理 heb:aside:submit：让旁支总结改动，注入主对话（复用 App 级 aside-result 监听）。
fn handle_aside_submit(app: &AppHandle, payload: &serde_json::Value) {
    let surface = payload.get("surface").and_then(|v| v.as_str()).unwrap_or("embedded").to_string();
    let session_id = payload.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let element_desc = payload.get("element").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if session_id.is_empty() {
        return;
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let dd = agent_core::storage::default_data_dir();
        let prompt = "把这次对话里你对这个元素做的修改总结成一段给主对话参考的话：\
            ①改了哪些样式（CSS 属性 → 目标值）；②达到了什么视觉效果；③为什么这么改/怎么实现的。\
            目的是让主对话据此去改源码。只输出这段总结，别再调工具。"
            .to_string();
        let args = aside_send_args(session_id.clone(), prompt, vec![]);
        match chat::send_and_save_in_data_dir(&dd, args, |_| {}).await {
            Ok(msg) => {
                let _ = app2.emit(
                    "browser://aside-result",
                    serde_json::json!({ "summary": msg.content, "element": element_desc }),
                );
                eval_aside_down(&app2, &surface, "heb:aside:submitted", serde_json::json!({ "sessionId": session_id }));
            }
            Err(e) => {
                eval_aside_down(&app2, &surface, "heb:aside:error", serde_json::json!({ "message": format!("总结失败：{e}") }));
            }
        }
    });
}

/// P0 spike 入口（HEBBIAN_WEBVIEW_SPIKE=1 时跑），验证 multi-webview 七项能力。
pub fn run_spike(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<BrowserState>();
    let url = browser_open(
        app.clone(),
        state,
        "https://example.com".to_string(),
        PreviewOrigin::User,
        620.0,
        80.0,
        760.0,
        560.0,
    );
    tracing::info!(target: "webview_spike", "S1 browser_open → {url:?}");

    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let wait = |s: u64| tokio::time::sleep(std::time::Duration::from_secs(s));
        let st = app2.state::<BrowserState>();

        wait(6).await;
        let r = browser_navigate(app2.clone(), st.clone(), "https://httpbin.org/cookies/set?heb=1".into());
        tracing::info!(target: "webview_spike", "S2/S7 navigate set-cookie → {r:?}");

        wait(8).await;
        let r = browser_navigate(app2.clone(), st.clone(), "https://httpbin.org/cookies".into());
        tracing::info!(target: "webview_spike", "S7 navigate cookie echo → {r:?}");

        wait(6).await;
        let r = browser_picker(st.clone(), true);
        tracing::info!(target: "webview_spike", "S3-down picker start → {r:?}");

        // 自动派发一次合成点击到页面里的 <a>，验证 picker → snapshot → 上行事件全链路
        wait(2).await;
        if let Some(inst) = st.inner.lock().unwrap().as_ref() {
            let click_js = r#"
                (function(){
                  var el = document.querySelector('a') || document.body.firstElementChild || document.body;
                  var r = el.getBoundingClientRect();
                  var ev = new MouseEvent('click', {clientX: r.left + r.width/2, clientY: r.top + r.height/2, bubbles: true, cancelable: true});
                  document.dispatchEvent(ev);
                })();
            "#;
            let _ = inst.webview.eval(click_js);
            tracing::info!(target: "webview_spike", "P2 synthetic click dispatched");
        }

        // 诊断：卡片是否渲染 + 按钮数（不点提交，避免与诊断导航冲突）
        wait(2).await;
        if let Some(inst) = st.inner.lock().unwrap().as_ref() {
            let probe_js = r#"
                (function(){
                  var card = document.querySelector('[data-hebbian-overlay="card"]');
                  var report = { hasCard: !!card, btnCount: card ? card.querySelectorAll('button').length : 0,
                                 numInputs: card ? card.querySelectorAll('input[type=number]').length : 0 };
                  window.location.replace('heb-bridge://msg/?d='+encodeURIComponent(JSON.stringify(
                    {source:'hebbian-inspector', type:'heb:debug', payload: report})));
                })();
            "#;
            let _ = inst.webview.eval(probe_js);
            tracing::info!(target: "webview_spike", "P2 card probe dispatched");
        }

        // 单独验证上行通道能承载注释提交（小 payload）
        wait(2).await;
        if let Some(inst) = st.inner.lock().unwrap().as_ref() {
            let submit_js = r#"
                (function(){
                  var card = document.querySelector('[data-hebbian-overlay="card"]');
                  if(!card) return;
                  var btns = card.querySelectorAll('button');
                  for(var i=0;i<btns.length;i++){ if(btns[i].textContent.indexOf('发送')>=0){ btns[i].click(); break; } }
                })();
            "#;
            let _ = inst.webview.eval(submit_js);
            tracing::info!(target: "webview_spike", "P2 synthetic annotation submit dispatched");
        }

        wait(2).await;
        let r = browser_set_bounds(st.clone(), 40.0, 40.0, 420.0, 320.0);
        tracing::info!(target: "webview_spike", "S4 set_bounds → {r:?}");

        // 弹出独立窗口验证
        wait(2).await;
        let r = browser_popout(app2.clone(), st.clone());
        tracing::info!(target: "webview_spike", "popout → {r:?}");

        // 探测 popout 窗口里的工具栏是否渲染
        wait(4).await;
        if let Some(w) = app2.get_webview_window(POPOUT_LABEL) {
            let probe = r#"
                (function(){
                  var bar = document.querySelector('[data-hebbian-overlay="toolbar"]');
                  window.location.replace('heb-bridge://msg/?d='+encodeURIComponent(JSON.stringify({
                    source:'hebbian-inspector', type:'heb:debug',
                    payload:{ isPopout: !!window.__HEB_POPOUT__, hasToolbar: !!bar,
                              inputs: bar ? bar.querySelectorAll('input').length : 0,
                              btns: bar ? bar.querySelectorAll('button').length : 0 }})));
                })();
            "#;
            let _ = w.eval(probe);
            tracing::info!(target: "webview_spike", "popout toolbar probe dispatched");
        }

        // 验证 about:blank 空窗口也注入 + 渲染工具栏（空浏览器 popout 走这条路）
        wait(2).await;
        if let Some(w) = app2.get_webview_window(POPOUT_LABEL) {
            let _ = w.eval("window.location.href='about:blank'");
        }
        wait(4).await;
        if let Some(w) = app2.get_webview_window(POPOUT_LABEL) {
            let probe = r#"
                (function(){
                  var bar = document.querySelector('[data-hebbian-overlay="toolbar"]');
                  window.location.replace('heb-bridge://msg/?d='+encodeURIComponent(JSON.stringify({
                    source:'hebbian-inspector', type:'heb:debug',
                    payload:{ blankPopout:true, isPopout: !!window.__HEB_POPOUT__, hasToolbar: !!bar,
                              href: window.location.href }})));
                })();
            "#;
            let _ = w.eval(probe);
            tracing::info!(target: "webview_spike", "about:blank toolbar probe dispatched");
        }

        wait(2).await;
        let h = browser_set_visible(st.clone(), false);
        wait(1).await;
        let s = browser_set_visible(st.clone(), true);
        tracing::info!(target: "webview_spike", "S6 hide={h:?} show={s:?}");

        tracing::info!(target: "webview_spike", "spike sequence finished");
    });

    Ok(())
}
