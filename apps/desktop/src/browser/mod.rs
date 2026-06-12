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

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Rect, Url, WebviewUrl,
    WebviewWindowBuilder, WindowEvent,
};

use url_policy::{validate_preview_url, PreviewOrigin};

use crate::chat;
use crate::engine::EngineEvent;
use agent_core::storage::sessions::Message;

const POPOUT_LABEL: &str = "preview-popout";
const POPOUT_PAGE_LABEL: &str = "preview-popout-page";
/// popout 顶部栏总高（logical px）= macOS 系统 titlebar 让位区 28 + 工具栏按钮/地址栏 44。
/// 页面子 webview 从这个 y 起——避开系统 titlebar 与工具栏，渲染区与它们物理分离。
/// （popout 窗口的 webview 内容会延伸到系统 titlebar 下方，故工具栏顶部要为 titlebar 让位。）
const POPOUT_TOOLBAR_H: f64 = 72.0;
const INSPECTOR_JS: &str = include_str!("inspector.js");
const POPOUT_TOOLBAR_HTML: &str = include_str!("popout_toolbar.html");
/// 给 webview 设完整 Safari UA——WKWebView 默认 UA 缺 `Version/Safari` 后缀，部分站点
/// （如 baidu）据此判定非标准浏览器、返回空白/简化页。补全后它们当正常浏览器渲染。
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";
const BLANK_PAGE_HTML: &str = "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>新页面</title></head><body style=\"margin:0;background:#fff\"></body></html>";

/// base64 data URL——WKWebView 对 about:blank 不执行 initialization_script，但对正常 data
/// 文档会执行（工具栏 HTML / 空白页都靠它注入脚本）。
fn data_url(html: &str) -> String {
    use base64::Engine;
    format!(
        "data:text/html;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(html)
    )
}

/// 拼下行 inspector 脚本（embedded 子 webview 与 popout 页面 webview 共用）。
fn rx_js(ty: &str, payload: serde_json::Value) -> String {
    let msg = serde_json::json!({ "source": "hebbian-host", "type": ty, "payload": payload });
    format!("window.__HEB_RX__ && window.__HEB_RX__({msg})")
}

/// 拼下行 popout 工具栏脚本（更新地址栏 / 前进后退 / 选取态）。
fn tb_js(payload: serde_json::Value) -> String {
    format!("window.__HEB_TB__ && window.__HEB_TB__({payload})")
}

/// 每个对话一个子 webview，label 按 session_id 区分（多对话多实例）。
fn webview_label(session_id: &str) -> String {
    format!("heb-preview-{session_id}")
}

/// 注入到子 webview 的引导脚本：先装 inspector.js，再安装上行 bridge。
/// inspector.js 内部检测到非 iframe 环境（子 webview）时，上行走 heb-bridge 导航，
/// 由本模块在 webview 创建时通过 init script 提供（inspector 自身已实现，无需重复）。
fn init_script() -> String {
    INSPECTOR_JS.to_string()
}

/// 子 webview 实例 + 自维护导航历史（Webview API 未暴露 go_back/forward）。
struct BrowserInstance {
    session_id: String,
    webview: tauri::Webview,
    history: Vec<String>,
    cursor: usize,
    /// 标记下一次 on_navigation 是程序触发（navigate/back/forward），避免重复入栈。
    programmatic: bool,
    picker_active: bool,
}

/// popout 独立窗口实例：主 webview = 工具栏，page = add_child 的目标页面子 webview。
/// 全局单例（同一时刻只弹一个），绑定打开它的对话 session_id（注释/旁支结论提交回它）。
struct PopoutInstance {
    session_id: String,
    window: tauri::WebviewWindow,
    page: tauri::Webview,
    history: Vec<String>,
    cursor: usize,
    /// 下一次 page on_navigation 是程序触发（navigate/back/forward），避免重复入栈。
    programmatic: bool,
    picker_active: bool,
}

/// 多对话多实例：每个对话一个浏览器实例，懒创建（对话里实际打开网页才建）。
/// session_id 天然就是「绑定的对话」——注释/队列/旁支结论提交回它，不串。
#[derive(Default)]
pub struct BrowserState {
    instances: Mutex<HashMap<String, BrowserInstance>>,
    popout: Mutex<Option<PopoutInstance>>,
    /// 旁支会话（元素对话）的纯内存历史（架构 §8.5）：旁支不落盘、关掉浏览器即消失。
    /// 外层 key = 绑定的主对话 id（浏览器实例），内层 key = 旁支 id（inspector 侧当不透明
    /// token 用，按 elementKey 索引），value = 多轮历史。按主对话分组让 `browser_close`
    /// 能随实例一并清理。模型 IO 仍写进主对话的 model_io.jsonl（kind=aside），供面板查看。
    asides: Mutex<HashMap<String, HashMap<String, Vec<Message>>>>,
}

#[derive(Serialize, Clone)]
struct BrowserStateEvent {
    session_id: String,
    url: String,
    can_go_back: bool,
    can_go_forward: bool,
    loading: bool,
}

fn emit_state(app: &AppHandle, inst: &BrowserInstance, loading: bool) {
    let evt = BrowserStateEvent {
        session_id: inst.session_id.clone(),
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
    let encoded = url
        .query_pairs()
        .find(|(k, _)| k == "d")
        .map(|(_, v)| v.into_owned())?;
    serde_json::from_str(&encoded).ok()
}

/// 把下行消息 eval 进 inspector。
fn send_down(inst: &BrowserInstance, ty: &str, payload: serde_json::Value) -> Result<(), String> {
    inst.webview.eval(&rx_js(ty, payload)).map_err(|e| e.to_string())
}

/// 创建子 webview 并加载首个 URL。已存在则复用（先关再建，保证 init script 干净）。
#[tauri::command]
pub fn browser_open(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    session_id: String,
    url: String,
    origin: PreviewOrigin,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<String, String> {
    let target = validate_preview_url(&url, origin)?;
    let target_str = target.to_string();

    // 已有该对话的实例：直接导航（保留每对话独立的页面 / 历史，不重建）
    {
        let mut guard = state.instances.lock().unwrap();
        if let Some(inst) = guard.get_mut(&session_id) {
            inst.history.truncate(inst.cursor + 1);
            inst.history.push(target_str.clone());
            inst.cursor = inst.history.len() - 1;
            inst.programmatic = true;
            inst.webview.navigate(target.clone()).map_err(|e| e.to_string())?;
            emit_state(&app, inst, true);
            return Ok(target_str);
        }
    }

    // 懒创建：该对话首次实际打开网页时才建实例（用户输网址 / agent 触发）
    let window = app
        .get_window("main")
        .ok_or_else(|| "主窗口不存在".to_string())?;
    let label = webview_label(&session_id);
    let sid_nav = session_id.clone();
    let app_for_nav = app.clone();
    let sid_load = session_id.clone();
    let app_for_load = app.clone();
    let builder = tauri::webview::WebviewBuilder::new(&label, WebviewUrl::External(target.clone()))
        .user_agent(BROWSER_UA)
        .initialization_script(init_script())
        .on_navigation(move |url: &Url| handle_navigation(&app_for_nav, &sid_nav, url))
        .on_page_load(move |_wv, payload| {
            let loading = matches!(payload.event(), tauri::webview::PageLoadEvent::Started);
            if let Some(st) = app_for_load.try_state::<BrowserState>() {
                if let Some(inst) = st.instances.lock().unwrap().get(&sid_load) {
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
        session_id: session_id.clone(),
        webview,
        history: vec![target_str.clone()],
        cursor: 0,
        programmatic: true, // 首次加载也算程序触发，on_navigation 不重复入栈
        picker_active: false,
    };
    state.instances.lock().unwrap().insert(session_id, inst);
    Ok(target_str)
}

/// on_navigation 回调：bridge 消息转发给前端；真实导航做安全校验 + 历史维护。
/// 返回 false 阻断本次导航。session_id 标识来自哪个对话的实例。
fn handle_navigation(app: &AppHandle, session_id: &str, url: &Url) -> bool {
    // 上行 bridge：解析转发，永不真导航
    if let Some(msg) = decode_bridge(url) {
        forward_inspector_message(app, session_id, msg);
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
                let mut guard = st.instances.lock().unwrap();
                if let Some(inst) = guard.get_mut(session_id) {
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
            let _ = app.emit(
                "browser://escaped",
                serde_json::json!({ "sessionId": session_id, "url": url.to_string(), "reason": reason }),
            );
            false
        }
    }
}

fn forward_inspector_message(app: &AppHandle, session_id: &str, msg: serde_json::Value) {
    let ty = msg.get("type").and_then(|v| v.as_str()).unwrap_or_default();
    let payload = msg
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if std::env::var("HEBBIAN_WEBVIEW_SPIKE").as_deref() == Ok("1") {
        let preview = serde_json::to_string(&payload).unwrap_or_default();
        let preview: String = preview.chars().take(240).collect();
        tracing::info!(target: "webview_spike", "fwd {ty}: {preview}");
    }
    // 多实例下 session_id 就是绑定的对话——注释/队列/标题事件都带上它，前端按它路由 + 提交回去
    let with_session = |mut p: serde_json::Value| -> serde_json::Value {
        if let Some(obj) = p.as_object_mut() {
            obj.insert("boundSessionId".to_string(), serde_json::Value::String(session_id.to_string()));
            obj.insert("sessionId".to_string(), serde_json::Value::String(session_id.to_string()));
        }
        p
    };
    match ty {
        // 页面内注释卡片提交：{snapshot, comment, styleDiff} → 主窗口 React 组装成 user message
        "heb:annotation:submit" => {
            let _ = app.emit("browser://annotation", with_session(payload));
        }
        // 修改队列：多元素改动统一提交
        "heb:annotation:submit-batch" => {
            let _ = app.emit("browser://annotation-batch", with_session(payload));
        }
        // 注释列表统一提交：LLM 合并总结成一条消息发主对话
        "heb:annotation:submit-all" => handle_annotation_submit_all(app, session_id, &payload),
        // 未提交注释数变化 → 前端工具栏防丢失警告用
        "heb:annotation:dirty" => {
            let _ = app.emit("browser://annotation-dirty", with_session(payload));
        }
        // 元素对话（旁支会话，机制 B）——session_id 即主对话
        "heb:aside:send" => handle_aside_send(app, session_id, &payload),
        "heb:aside:submit" => handle_aside_submit(app, session_id, &payload),
        "heb:aside:models:request" => {
            let surface = payload
                .get("surface")
                .and_then(|v| v.as_str())
                .unwrap_or("embedded");
            send_aside_models(app, session_id, surface);
        }
        "heb:picker:cancelled" => {
            let _ = app.emit("browser://picker-off", serde_json::json!({ "sessionId": session_id }));
        }
        "heb:ready" | "heb:navigated" => {
            // 携带 title，补一条 state（loading=false）
            let _ = app.emit("browser://title", with_session(payload));
        }
        _ => {}
    }
}

/// 读 providers.json 拼模型列表 + 当前对话的 provider/model，下发给卡片的模型选择器。
fn send_aside_models(app: &AppHandle, session_id: &str, surface: &str) {
    let dd = agent_core::storage::default_data_dir();
    let mut list: Vec<serde_json::Value> = Vec::new();
    if let Ok(txt) = std::fs::read_to_string(dd.join("providers.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(provs) = v.get("providers").and_then(|p| p.as_array()) {
                for p in provs {
                    if p.get("enabled").and_then(|e| e.as_bool()) == Some(false) {
                        continue;
                    }
                    let pid = p.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let pname = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    if let Some(models) = p.get("models").and_then(|m| m.as_array()) {
                        for m in models {
                            if let Some(mn) = m.as_str() {
                                list.push(serde_json::json!({ "providerId": pid, "model": mn, "label": format!("{mn} · {pname}") }));
                            }
                        }
                    }
                }
            }
        }
    }
    let current = agent_core::storage::sessions::load(&dd, session_id)
        .ok()
        .map(|s| serde_json::json!({ "providerId": s.provider_id, "model": s.model }));
    eval_aside_down(
        app,
        session_id,
        surface,
        "heb:aside:models",
        serde_json::json!({ "list": list, "current": current }),
    );
}

#[tauri::command]
pub fn browser_navigate(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    session_id: String,
    url: String,
) -> Result<String, String> {
    // 地址栏输入 = user 档
    let target = validate_preview_url(&url, PreviewOrigin::User)?;
    let mut guard = state.instances.lock().unwrap();
    let inst = guard.get_mut(&session_id).ok_or_else(|| "浏览器未打开".to_string())?;
    inst.history.truncate(inst.cursor + 1);
    inst.history.push(target.to_string());
    inst.cursor = inst.history.len() - 1;
    inst.programmatic = true;
    inst.webview
        .navigate(target.clone())
        .map_err(|e| e.to_string())?;
    emit_state(&app, inst, true);
    Ok(target.to_string())
}

#[tauri::command]
pub fn browser_back(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    let mut guard = state.instances.lock().unwrap();
    let inst = guard.get_mut(&session_id).ok_or_else(|| "浏览器未打开".to_string())?;
    if inst.cursor == 0 {
        return Ok(());
    }
    inst.cursor -= 1;
    inst.programmatic = true;
    let url: Url = inst.history[inst.cursor]
        .parse()
        .map_err(|_| "历史地址异常".to_string())?;
    inst.webview.navigate(url).map_err(|e| e.to_string())?;
    emit_state(&app, inst, true);
    Ok(())
}

#[tauri::command]
pub fn browser_forward(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    let mut guard = state.instances.lock().unwrap();
    let inst = guard.get_mut(&session_id).ok_or_else(|| "浏览器未打开".to_string())?;
    if inst.cursor + 1 >= inst.history.len() {
        return Ok(());
    }
    inst.cursor += 1;
    inst.programmatic = true;
    let url: Url = inst.history[inst.cursor]
        .parse()
        .map_err(|_| "历史地址异常".to_string())?;
    inst.webview.navigate(url).map_err(|e| e.to_string())?;
    emit_state(&app, inst, true);
    Ok(())
}

#[tauri::command]
pub fn browser_reload(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    let mut guard = state.instances.lock().unwrap();
    let inst = guard.get_mut(&session_id).ok_or_else(|| "浏览器未打开".to_string())?;
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
    session_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let guard = state.instances.lock().unwrap();
    let inst = match guard.get(&session_id) {
        Some(i) => i,
        None => return Ok(()), // 该对话还没浏览器实例时静默
    };
    inst.webview
        .set_bounds(Rect {
            position: LogicalPosition::new(x, y).into(),
            size: LogicalSize::new(width.max(1.0), height.max(1.0)).into(),
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn browser_set_visible(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
    visible: bool,
) -> Result<(), String> {
    let guard = state.instances.lock().unwrap();
    if let Some(inst) = guard.get(&session_id) {
        if visible {
            inst.webview.show().map_err(|e| e.to_string())?;
        } else {
            inst.webview.hide().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 隐藏除 keep_session 外所有实例（切对话时只显示当前对话的浏览器）。
#[tauri::command]
pub fn browser_hide_others(
    state: tauri::State<'_, BrowserState>,
    keep_session: String,
) -> Result<(), String> {
    let guard = state.instances.lock().unwrap();
    for (sid, inst) in guard.iter() {
        if *sid != keep_session {
            let _ = inst.webview.hide();
        }
    }
    Ok(())
}

#[tauri::command]
pub fn browser_close(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    if let Some(inst) = state.instances.lock().unwrap().remove(&session_id) {
        inst.webview.close().map_err(|e| e.to_string())?;
    }
    // 旁支会话纯内存、与浏览器实例同生命周期：实例关闭即丢弃这段对话的所有元素历史。
    state.asides.lock().unwrap().remove(&session_id);
    Ok(())
}

/// 把当前页面弹出成独立可缩放窗口（测响应式样式用）。窗口主 webview 加载工具栏 HTML，
/// 目标页面是 add_child 的子 webview 浮在工具栏下方——工具栏与页面渲染区物理分离，页面不
/// 被注入任何工具栏 DOM。注释卡片仍由 inspector 在页面内渲染、经 heb-bridge 上行回主进程，
/// 再由主窗口 React 发进绑定的对话。
#[tauri::command]
pub fn browser_popout(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    // 取该对话实例的当前页；没有则弹空白页，用户在工具栏地址栏里输网址。
    let cur = state
        .instances
        .lock()
        .unwrap()
        .get(&session_id)
        .and_then(|i| i.history.get(i.cursor).cloned());
    let (page_url, history): (Url, Vec<String>) = match cur {
        Some(u) => (u.parse().map_err(|_| "当前地址异常".to_string())?, vec![u]),
        None => (
            data_url(BLANK_PAGE_HTML)
                .parse()
                .map_err(|_| "空白页生成失败".to_string())?,
            vec![],
        ),
    };

    // 已有 popout 先收掉（全局单例）
    if let Some(p) = state.popout.lock().unwrap().take() {
        let _ = p.window.close();
    }
    if let Some(w) = app.get_window(POPOUT_LABEL) {
        let _ = w.close();
    }

    // 主 webview = 工具栏（data URL，无 Tauri IPC → 同 inspector 走 heb-bridge 上行）
    let toolbar_url: Url = data_url(POPOUT_TOOLBAR_HTML)
        .parse()
        .map_err(|_| "工具栏生成失败".to_string())?;
    let app_tb = app.clone();
    let mut tb_builder = WebviewWindowBuilder::new(&app, POPOUT_LABEL, WebviewUrl::External(toolbar_url))
        .title("页面预览（可缩放测样式）")
        .inner_size(1280.0, 800.0)
        .resizable(true)
        .on_navigation(move |url: &Url| handle_toolbar_nav(&app_tb, url));
    // popout 用 overlay titlebar（与主窗口一致）：webview 内容区 = 整个窗口（含 titlebar 区），
    // add_child 的坐标系才和主窗口统一。否则标准 titlebar 下 add_child 的 y 相对窗口外框，
    // page 会被上移一个 titlebar 高度盖住工具栏。工具栏 HTML 顶部已留 28px 给系统红绿灯。
    #[cfg(target_os = "macos")]
    {
        tb_builder = tb_builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    let win = tb_builder.build().map_err(|e| e.to_string())?;

    // 页面子 webview：注入 inspector（注释/选取/旁支），不再注入工具栏
    let window = app
        .get_window(POPOUT_LABEL)
        .ok_or_else(|| "popout 窗口异常".to_string())?;
    let app_pg = app.clone();
    let page = window
        .add_child(
            tauri::webview::WebviewBuilder::new(POPOUT_PAGE_LABEL, WebviewUrl::External(page_url))
                .user_agent(BROWSER_UA)
                .initialization_script(format!("window.__HEB_POPOUT__=true;\n{INSPECTOR_JS}"))
                .on_navigation(move |url: &Url| handle_popout_page_nav(&app_pg, url)),
            LogicalPosition::new(0.0, POPOUT_TOOLBAR_H),
            LogicalSize::new(1280.0, (800.0 - POPOUT_TOOLBAR_H).max(1.0)),
        )
        .map_err(|e| e.to_string())?;

    // add_child 的子 webview 默认不保证可见（embedded 也是靠 setVisible 显式 show 才出现）。
    let _ = page.show();

    *state.popout.lock().unwrap() = Some(PopoutInstance {
        session_id: session_id.clone(),
        window: win.clone(),
        page,
        history,
        cursor: 0,
        programmatic: true,
        picker_active: false,
    });

    // 窗口 resize → 页面 webview 重新铺满工具栏下方；关窗 → 清实例 + 通知前端恢复内嵌
    let app_evt = app.clone();
    let sid_evt = session_id.clone();
    win.on_window_event(move |event| match event {
        WindowEvent::Resized(_) => popout_resize(&app_evt),
        WindowEvent::Destroyed | WindowEvent::CloseRequested { .. } => {
            if let Some(st) = app_evt.try_state::<BrowserState>() {
                *st.popout.lock().unwrap() = None;
            }
            let _ = app_evt.emit(
                "browser://popout",
                serde_json::json!({ "sessionId": sid_evt, "open": false }),
            );
        }
        _ => {}
    });

    // 用实际 client area 修正页面 webview 的位置/尺寸（固定 1280×800 可能与真实窗口尺寸/
    // titlebar 不符，导致页面跑偏或被遮）+ 初始同步地址栏。
    popout_resize(&app);
    send_toolbar_state(&app);
    let _ = app.emit(
        "browser://popout",
        serde_json::json!({ "sessionId": session_id, "open": true }),
    );
    Ok(())
}

/// 关闭独立预览窗口。
#[tauri::command]
pub fn browser_close_popout(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
) -> Result<(), String> {
    if let Some(p) = state.popout.lock().unwrap().take() {
        p.window.close().map_err(|e| e.to_string())?;
    } else if let Some(w) = app.get_window(POPOUT_LABEL) {
        w.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 取一份 popout 实例的可变引用做操作；无 popout 时静默。
fn with_popout<F: FnOnce(&mut PopoutInstance)>(app: &AppHandle, f: F) {
    if let Some(st) = app.try_state::<BrowserState>() {
        if let Some(p) = st.popout.lock().unwrap().as_mut() {
            f(p);
        }
    }
}

/// 把当前 history 状态推给工具栏 webview（地址栏 + 前进后退可用态）。
fn send_toolbar_state(app: &AppHandle) {
    with_popout(app, |p| {
        let url = p.history.get(p.cursor).cloned().unwrap_or_default();
        let payload = serde_json::json!({
            "url": url,
            "canBack": p.cursor > 0,
            "canForward": p.cursor + 1 < p.history.len(),
        });
        let _ = p.window.eval(&tb_js(payload));
    });
}

/// 窗口 resize：页面子 webview 铺满工具栏下方区域。
fn popout_resize(app: &AppHandle) {
    let Some(w) = app.get_window(POPOUT_LABEL) else {
        return;
    };
    let (Ok(size), Ok(sf)) = (w.inner_size(), w.scale_factor()) else {
        return;
    };
    let lw = size.width as f64 / sf;
    let lh = size.height as f64 / sf;
    with_popout(app, |p| {
        let _ = p.page.set_bounds(Rect {
            position: LogicalPosition::new(0.0, POPOUT_TOOLBAR_H).into(),
            size: LogicalSize::new(lw.max(1.0), (lh - POPOUT_TOOLBAR_H).max(1.0)).into(),
        });
    });
}

/// 工具栏 webview 的上行消息（地址栏导航 / 前进后退 / 刷新 / 选取 / 收回）。
fn handle_toolbar_nav(app: &AppHandle, url: &Url) -> bool {
    let Some(msg) = decode_bridge(url) else {
        return true; // 初始 data URL 加载放行
    };
    let ty = msg.get("type").and_then(|v| v.as_str()).unwrap_or_default();
    let payload = msg.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    match ty {
        "tb:navigate" => {
            if let Some(raw) = payload.get("url").and_then(|v| v.as_str()) {
                popout_navigate(app, raw);
            }
        }
        "tb:back" => popout_go(app, -1),
        "tb:forward" => popout_go(app, 1),
        "tb:reload" => popout_reload(app),
        "tb:picker" => popout_toggle_picker(app),
        // 窗口 resize 由工具栏 webview 的 JS resize 事件上行触发——on_window_event 的
        // Resized 在多 webview 窗口不可靠（实测不触发），工具栏主 webview 随窗口缩放、
        // 它的 window.onresize 一定触发，是可靠的 resize 钩子。
        "tb:resize" => popout_resize(app),
        // 顶部 titlebar 让位区按下拖窗口——data URL webview 的 -webkit-app-region 不生效，
        // 用 startDragging。
        "tb:drag" => {
            if let Some(w) = app.get_window(POPOUT_LABEL) {
                let _ = w.start_dragging();
            }
        }
        "tb:close" => {
            if let Some(w) = app.get_window(POPOUT_LABEL) {
                let _ = w.close();
            }
        }
        _ => {}
    }
    false
}

/// 页面子 webview 的上行消息：注释/旁支提交回对话（复用 embedded 路径），选取/导航态反馈
/// 到工具栏 webview（popout 没有 React，UI 反馈不走 browser:// 事件而是直接 eval 工具栏）。
fn handle_popout_page_nav(app: &AppHandle, url: &Url) -> bool {
    if let Some(msg) = decode_bridge(url) {
        let ty = msg.get("type").and_then(|v| v.as_str()).unwrap_or_default();
        match ty {
            "heb:picker:cancelled" => {
                with_popout(app, |p| p.picker_active = false);
                send_toolbar_picker(app, false);
            }
            "heb:ready" | "heb:navigated" => send_toolbar_state(app),
            _ => {
                // annotation / aside：提交回打开 popout 的对话
                let sid = app
                    .try_state::<BrowserState>()
                    .and_then(|st| st.popout.lock().unwrap().as_ref().map(|p| p.session_id.clone()));
                if let Some(sid) = sid {
                    forward_inspector_message(app, &sid, msg);
                }
            }
        }
        return false;
    }
    if url.scheme() != "http" && url.scheme() != "https" {
        return true;
    }
    match validate_preview_url(url.as_str(), PreviewOrigin::User) {
        Ok(_) => {
            with_popout(app, |p| {
                if p.programmatic {
                    p.programmatic = false;
                } else {
                    p.history.truncate(p.cursor + 1);
                    p.history.push(url.to_string());
                    p.cursor = p.history.len() - 1;
                }
            });
            send_toolbar_state(app);
            true
        }
        Err(_) => false,
    }
}

fn send_toolbar_picker(app: &AppHandle, active: bool) {
    with_popout(app, |p| {
        let _ = p.window.eval(&tb_js(serde_json::json!({ "picker": active })));
    });
}

fn popout_navigate(app: &AppHandle, raw_url: &str) {
    let target = match validate_preview_url(raw_url, PreviewOrigin::User) {
        Ok(u) => u,
        Err(_) => return,
    };
    with_popout(app, |p| {
        p.history.truncate(p.cursor + 1);
        p.history.push(target.to_string());
        p.cursor = p.history.len() - 1;
        p.programmatic = true;
        let _ = p.page.navigate(target.clone());
    });
    send_toolbar_state(app);
}

fn popout_go(app: &AppHandle, delta: isize) {
    with_popout(app, |p| {
        let ni = p.cursor as isize + delta;
        if ni < 0 || ni as usize >= p.history.len() {
            return;
        }
        p.cursor = ni as usize;
        if let Ok(u) = p.history[p.cursor].parse::<Url>() {
            p.programmatic = true;
            let _ = p.page.navigate(u);
        }
    });
    send_toolbar_state(app);
}

fn popout_reload(app: &AppHandle) {
    with_popout(app, |p| {
        if let Some(u) = p.history.get(p.cursor).and_then(|s| s.parse::<Url>().ok()) {
            p.programmatic = true;
            let _ = p.page.navigate(u);
        }
    });
}

fn popout_toggle_picker(app: &AppHandle) {
    let mut active = false;
    with_popout(app, |p| {
        p.picker_active = !p.picker_active;
        active = p.picker_active;
        let ty = if active { "heb:picker:start" } else { "heb:picker:stop" };
        let _ = p.page.eval(&rx_js(ty, serde_json::json!({})));
    });
    send_toolbar_picker(app, active);
}

// 以下「命令 inspector」的命令在没有 webview 时是无操作（返回 Ok）——没浏览器可命令
// 就啥也不做，不是错误。否则面板挂载/切 tab 等 fire-and-forget 调用会抛"浏览器未打开"
// 被全局 unhandledrejection 弹 toast（启动即弹的根因）。

#[tauri::command]
pub fn browser_picker(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
    active: bool,
) -> Result<(), String> {
    let mut guard = state.instances.lock().unwrap();
    let Some(inst) = guard.get_mut(&session_id) else {
        return Ok(());
    };
    inst.picker_active = active;
    send_down(
        inst,
        if active {
            "heb:picker:start"
        } else {
            "heb:picker:stop"
        },
        serde_json::json!({}),
    )
}

#[tauri::command]
pub fn browser_style_apply(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
    prop: String,
    value: String,
) -> Result<(), String> {
    let guard = state.instances.lock().unwrap();
    let Some(inst) = guard.get(&session_id) else {
        return Ok(());
    };
    send_down(
        inst,
        "heb:style:apply",
        serde_json::json!({ "prop": prop, "value": value }),
    )
}

#[tauri::command]
pub fn browser_style_revert(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    let guard = state.instances.lock().unwrap();
    let Some(inst) = guard.get(&session_id) else {
        return Ok(());
    };
    send_down(inst, "heb:style:revert", serde_json::json!({}))
}

/// 请求 inspector 回吐当前 styleDiff（异步：结果经 browser://style-diff 事件返回）。
#[tauri::command]
pub fn browser_style_take_diff(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    let guard = state.instances.lock().unwrap();
    let Some(inst) = guard.get(&session_id) else {
        return Ok(());
    };
    send_down(inst, "heb:style:take-diff", serde_json::json!({}))
}

/// 用户已确认丢弃未提交注释——给 inspector 发一次性放行标志，
/// 让接下来的刷新/导航不再被 beforeunload 兜底拦一遍（避免双弹）。
#[tauri::command]
pub fn browser_allow_unload(
    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    let guard = state.instances.lock().unwrap();
    let Some(inst) = guard.get(&session_id) else {
        return Ok(());
    };
    send_down(inst, "heb:unload:allow", serde_json::json!({}))
}

/// 清除选中态（注释卡片关闭 / 切走 tab 时调）。无浏览器时无操作。
#[tauri::command]
pub fn browser_clear_selection(    state: tauri::State<'_, BrowserState>,
    session_id: String,
) -> Result<(), String> {
    let guard = state.instances.lock().unwrap();
    let Some(inst) = guard.get(&session_id) else {
        return Ok(());
    };
    send_down(inst, "heb:selection:clear", serde_json::json!({}))
}

// ─────────────────────── 元素对话（旁支会话，机制 B）───────────────────────

// system prompt 不嵌具体元素定位（保 prompt cache 命中 + 支持中途追加元素）；
// 元素定位作为每轮 user content 前缀由 handle_aside_send 拼进去。
fn aside_system_prompt() -> String {
    "你是网页预览里的「样式调整助手」。用户在页面上圈了一个或多个元素（编号 @1、@2…），\
     你帮他在预览上**临时**调整看效果。每轮用户消息前会附上当前选中元素的定位信息。\n\n\
     你的工具：\n\
     • PreviewStyle(prop, value, target) —— 实时改某个元素的内联样式（color / font-size / \
     padding / border-radius / background-color 等），用户立刻看到效果。target 填 @N（缺省 @1）。\
     一次改一个属性，想微调再调一次。\n\
     • PreviewMutate(op, target, …) —— 改 DOM 结构：op=append（target 内追加 html 片段）/ \
     remove（移除 target）/ setText（改 target 文本为 text）。同样是预览草稿，刷新即消失。\n\
     • PreviewAct(action, target, …) —— 和页面交互：click / type(text) / hover / press(key) / \
     scroll(delta)，用来触发弹窗、hover 菜单、表单校验等交互态。\n\n\
     重要——理解你的定位：\n\
     • 你做的都是**临时预览效果**，不是最终实现。用户满意后，会由「主对话」**修改源代码**真正落地。\n\
     • 所以你心里要清楚「这个效果对应到源码该怎么改」，过程中可以简短点一下。\n\
     • 删除元素用 PreviewMutate remove（预览态），并说明真正删除要在源码里移除该元素/组件。\n\
     • 别假设源码怎么存放。你只负责把效果调好，并说清楚改了什么、对应什么源码改动。\n\n\
     先理解用户要的效果，再动手。保持简洁。"
        .to_string()
}

/// 把一条下行消息 eval 到来源 webview（embedded 子 webview 或 popout 窗口）。
/// 把下行消息 eval 到来源 webview。host_session = 发起这次对话的主对话（embedded 实例所属）；
/// popout 走全局 POPOUT_LABEL 窗口。
fn eval_aside_down(
    app: &AppHandle,
    host_session: &str,
    surface: &str,
    ty: &str,
    payload: serde_json::Value,
) {
    let js = rx_js(ty, payload);
    if surface == "popout" {
        // popout 的旁支面板在页面子 webview（窗口主 webview 现在是工具栏）
        if let Some(st) = app.try_state::<BrowserState>() {
            if let Some(p) = st.popout.lock().unwrap().as_ref() {
                let _ = p.page.eval(&js);
            }
        }
    } else if let Some(st) = app.try_state::<BrowserState>() {
        if let Some(inst) = st.instances.lock().unwrap().get(host_session) {
            let _ = inst.webview.eval(&js);
        }
    }
}

/// 把旁支会话的事件流路由下发到来源 webview：文本增量 + PreviewStyle 实时应用 + 结束。
/// host_session = embedded 实例所属的主对话；aside_session = 旁支会话 id（事件标识）。
fn route_aside_event(
    app: &AppHandle,
    host_session: &str,
    surface: &str,
    aside_session: &str,
    event: EngineEvent,
) {
    match event {
        EngineEvent::TextDelta { text, .. } => {
            eval_aside_down(
                app,
                host_session,
                surface,
                "heb:aside:delta",
                serde_json::json!({ "sessionId": aside_session, "text": text }),
            );
        }
        EngineEvent::ToolStart { name, input, .. } if name == "PreviewStyle" => {
            let prop = input
                .get("prop")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let value = input
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let target = input
                .get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("@1")
                .to_string();
            // 实时应用到页面 + 在聊天里显示这一步
            eval_aside_down(
                app,
                host_session,
                surface,
                "heb:aside:apply",
                serde_json::json!({ "sessionId": aside_session, "prop": prop, "value": value, "target": target }),
            );
        }
        EngineEvent::ToolStart { name, mut input, .. } if name == "PreviewMutate" => {
            if input.get("target").and_then(|v| v.as_str()).is_none() {
                input["target"] = serde_json::json!("@1");
            }
            input["sessionId"] = serde_json::json!(aside_session);
            eval_aside_down(app, host_session, surface, "heb:aside:mutate", input);
        }
        EngineEvent::ToolStart { name, mut input, .. } if name == "PreviewAct" => {
            if input.get("target").and_then(|v| v.as_str()).is_none() {
                input["target"] = serde_json::json!("@1");
            }
            input["sessionId"] = serde_json::json!(aside_session);
            eval_aside_down(app, host_session, surface, "heb:aside:act", input);
        }
        EngineEvent::RunFinished { .. } => {
            eval_aside_down(
                app,
                host_session,
                surface,
                "heb:aside:done",
                serde_json::json!({ "sessionId": aside_session }),
            );
        }
        _ => {}
    }
}

fn fresh_cancel() -> common::CancelFlag {
    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
}


/// 处理 heb:aside:send：建/续旁支会话，驱动一轮，事件流下发卡片。
fn handle_aside_send(app: &AppHandle, main_session_id: &str, payload: &serde_json::Value) {
    let surface = payload
        .get("surface")
        .and_then(|v| v.as_str())
        .unwrap_or("embedded")
        .to_string();
    let element_key = payload
        .get("elementKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // inspector 把后端回传的 aside id 当不透明 token（按 elementKey 索引）。
    // 首轮为空 → 这里新建一个内存 aside；后续轮带回它定位历史。
    let aside_id_opt = payload
        .get("sessionId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let text = payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let element_desc = payload
        .get("element")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // 多元素 @N → 定位映射（inspector 每轮全量带上）；拼成 user content 前缀，
    // system prompt 不嵌定位 → 保 prompt cache + 支持中途追加元素。
    let elements_block = payload
        .get("elements")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let r = e.get("ref").and_then(|v| v.as_str())?;
                    let loc = e.get("locator").and_then(|v| v.as_str()).unwrap_or("");
                    Some(format!("{r}:\n{loc}"))
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .filter(|s| !s.is_empty())
        // 旧单元素载荷兜底：把 element 当 @1
        .unwrap_or_else(|| {
            if element_desc.is_empty() {
                String::new()
            } else {
                format!("@1:\n{element_desc}")
            }
        });
    // 卡片里模型选择器选的 provider/model（可空 → 用上下文默认）
    let sel_provider = payload
        .get("providerId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let sel_model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let main_sid = main_session_id.to_string();
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let dd = agent_core::storage::default_data_dir();
        // 默认用主对话（浏览器绑定的对话）的 provider/model，卡片选了模型就用选的
        let (def_provider, def_model) = agent_core::storage::sessions::load(&dd, &main_sid)
            .map(|s| (s.provider_id, s.model))
            .unwrap_or_default();
        let provider_id = sel_provider.unwrap_or(def_provider);
        let model = sel_model.unwrap_or(def_model);
        if provider_id.is_empty() {
            eval_aside_down(
                &app2,
                &main_sid,
                &surface,
                "heb:aside:error",
                serde_json::json!({ "message": "这个对话还没配置模型，没法开元素对话" }),
            );
            return;
        }

        // 旁支历史纯内存持有（架构 §8.5）：首轮新建 aside id 并回填给 inspector，
        // 后续轮取出已有历史续接。
        let aside_id = match aside_id_opt {
            Some(id) => id,
            None => {
                let id = format!("aside-{}", agent_core::storage::sessions::new_id());
                eval_aside_down(
                    &app2,
                    &main_sid,
                    &surface,
                    "heb:aside:session",
                    serde_json::json!({ "elementKey": element_key, "sessionId": id }),
                );
                id
            }
        };
        let history = app2
            .try_state::<BrowserState>()
            .map(|st| {
                st.asides
                    .lock()
                    .unwrap()
                    .get(&main_sid)
                    .and_then(|m| m.get(&aside_id))
                    .cloned()
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let app3 = app2.clone();
        let surface2 = surface.clone();
        let host = main_sid.clone();
        let aside_for_event = aside_id.clone();
        // 元素定位放每轮 user content 前缀（非 system prompt）
        let user_content = if elements_block.is_empty() {
            text
        } else {
            format!("<selected_elements>\n{elements_block}\n</selected_elements>\n\n{text}")
        };
        let result = chat::send_aside(
            &dd,
            &main_sid,
            &provider_id,
            &model,
            aside_system_prompt(),
            history,
            user_content,
            fresh_cancel(),
            move |event| route_aside_event(&app3, &host, &surface2, &aside_for_event, event),
        )
        .await;
        match result {
            Ok((updated_history, _)) => {
                if let Some(st) = app2.try_state::<BrowserState>() {
                    st.asides
                        .lock()
                        .unwrap()
                        .entry(main_sid.clone())
                        .or_default()
                        .insert(aside_id, updated_history);
                }
            }
            Err(e) => {
                eval_aside_down(
                    &app2,
                    &main_sid,
                    &surface,
                    "heb:aside:error",
                    serde_json::json!({ "message": format!("助手出错：{e}") }),
                );
            }
        }
    });
}

/// 处理 heb:aside:submit：让旁支总结改动，注入主对话（复用 App 级 aside-result 监听）。
fn handle_aside_submit(app: &AppHandle, main_session_id: &str, payload: &serde_json::Value) {
    let surface = payload
        .get("surface")
        .and_then(|v| v.as_str())
        .unwrap_or("embedded")
        .to_string();
    let main_sid = main_session_id.to_string();
    let aside_id = payload
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let element_desc = payload
        .get("element")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if aside_id.is_empty() {
        return;
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let dd = agent_core::storage::default_data_dir();
        let (provider_id, model) = agent_core::storage::sessions::load(&dd, &main_sid)
            .map(|s| (s.provider_id, s.model))
            .unwrap_or_default();
        let history = match app2.try_state::<BrowserState>() {
            Some(st) => st
                .asides
                .lock()
                .unwrap()
                .get(&main_sid)
                .and_then(|m| m.get(&aside_id))
                .cloned(),
            None => None,
        };
        let Some(history) = history else {
            return; // 没有这段旁支历史（浏览器已关 / 从没对话过）
        };
        if provider_id.is_empty() {
            eval_aside_down(
                &app2,
                &main_sid,
                &surface,
                "heb:aside:error",
                serde_json::json!({ "message": "这个对话还没配置模型，没法总结" }),
            );
            return;
        }
        let prompt = format!(
            "现在把这次调整总结成一段给「主对话」看的话，让它去改源代码真正实现这些效果。\n\n\
             元素定位信息（务必带进总结，让主对话能精确找到源码）：\n{element_desc}\n\n\
             总结要包含：\n\
             ① 改了什么视觉效果——逐条列「CSS 属性 → 目标值」，或结构改动如「隐藏/移除」；\n\
             ② 对应到源码怎么改——结合上面的源码位置（file:line）/ 组件名 / 元素文本，明确指出该改哪个文件里的哪个元素，\
             给的定位锚点要足够让人 grep 到（别只说 div+class，要带文本内容或组件名）；\n\
             ③ 若是删除类操作，说明要在源码里移除该元素，而不是加 display:none。\n\n\
             只输出这段总结，不要再调工具。"
        );
        // 总结轮不需要把结果回写历史（旁支即将提交完结），事件流也不下发（纯文本总结）。
        let result = chat::send_aside(
            &dd,
            &main_sid,
            &provider_id,
            &model,
            aside_system_prompt(),
            history,
            prompt,
            fresh_cancel(),
            |_event| {},
        )
        .await;
        match result {
            Ok((_, msg)) => {
                // 发到浏览器绑定的对话（main_sid），不串到当前打开的别的对话
                let _ = app2.emit(
                    "browser://aside-result",
                    serde_json::json!({ "summary": msg.content, "element": element_desc, "boundSessionId": main_sid.clone() }),
                );
                eval_aside_down(
                    &app2,
                    &main_sid,
                    &surface,
                    "heb:aside:submitted",
                    serde_json::json!({ "sessionId": aside_id }),
                );
            }
            Err(e) => {
                eval_aside_down(
                    &app2,
                    &main_sid,
                    &surface,
                    "heb:aside:error",
                    serde_json::json!({ "message": format!("总结失败：{e}") }),
                );
            }
        }
    });
}

/// 处理 heb:annotation:submit-all：注释列表全部提交——LLM 把多条注释（多元素 +
/// 对话 + 样式 diff + 结构改动）合并总结成一条给主对话的消息。
fn handle_annotation_submit_all(app: &AppHandle, main_session_id: &str, payload: &serde_json::Value) {
    let surface = payload
        .get("surface")
        .and_then(|v| v.as_str())
        .unwrap_or("embedded")
        .to_string();
    let items = payload.get("items").cloned().unwrap_or(serde_json::json!([]));
    if items.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return;
    }
    let main_sid = main_session_id.to_string();
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let dd = agent_core::storage::default_data_dir();
        let (provider_id, model) = agent_core::storage::sessions::load(&dd, &main_sid)
            .map(|s| (s.provider_id, s.model))
            .unwrap_or_default();
        if provider_id.is_empty() {
            eval_aside_down(
                &app2,
                &main_sid,
                &surface,
                "heb:aside:error",
                serde_json::json!({ "message": "这个对话还没配置模型，没法提交注释" }),
            );
            return;
        }
        let items_json = serde_json::to_string_pretty(&items).unwrap_or_default();
        let prompt = format!(
            "下面是用户在网页预览里收集的若干条注释（JSON）。每条含：选中元素的定位快照（elements，\
             含 selectorPath / xpath / react 组件链 / 文本）、与样式助手的对话原文（conversation）、\
             实际调过的样式（styleDiffs，prop: before → after）、结构改动（structuralChanges）。\n\n\
             {items_json}\n\n\
             把它们合并总结成**一段给「主对话」看的话**，让它去修改源代码真正实现这些效果。要求：\n\
             ① 按元素分组，逐条列「CSS 属性 → 目标值」和结构改动；\n\
             ② 结合 react 组件链 / 元素文本给出足够 grep 的源码定位锚点（别只说 div+class）；\n\
             ③ 对话里用户表达的意图（如「要再醒目一点」）要保留为上下文；\n\
             ④ 删除类操作说明要在源码里移除元素，而不是 display:none。\n\n\
             只输出这段总结，不要再调工具。"
        );
        let result = chat::send_aside(
            &dd,
            &main_sid,
            &provider_id,
            &model,
            aside_system_prompt(),
            Vec::new(),
            prompt,
            fresh_cancel(),
            |_event| {},
        )
        .await;
        match result {
            Ok((_, msg)) => {
                let _ = app2.emit(
                    "browser://annotation-summary",
                    serde_json::json!({ "summary": msg.content, "boundSessionId": main_sid.clone() }),
                );
                eval_aside_down(
                    &app2,
                    &main_sid,
                    &surface,
                    "heb:aside:submitted",
                    serde_json::json!({}),
                );
            }
            Err(e) => {
                eval_aside_down(
                    &app2,
                    &main_sid,
                    &surface,
                    "heb:aside:error",
                    serde_json::json!({ "message": format!("提交注释失败：{e}") }),
                );
            }
        }
    });
}
