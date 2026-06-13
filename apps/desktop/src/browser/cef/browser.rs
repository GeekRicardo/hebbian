//! CEF 浏览器实例 + per-session 管理（架构 §8.5 M2）。
//!
//! 替代 wry 子 webview：用 CEF `browser_host_create_browser` + `set_as_child` 把真
//! Chromium 视图挂进 Tauri 主窗口的 contentView（PoC 阶段 3 形态）。inspector.js 经
//! LoadHandler on_load_end 注入；操作下行用 frame.execute_java_script，观察（截图/
//! 规则）走 CDP（CdpBridge 连 CEF_CDP_PORT，同一实例）。
//!
//! ⚠️ 子视图挂载 + 句柄回传 + 注入时机需 `pnpm tauri dev --features cef-preview`
//! 真机验收（原生窗口 + CEF 多进程交互无法在 headless / 单测环境验证）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cef::{
    browser_host_create_browser, BrowserSettings, CefString, Client, ImplBrowser, ImplBrowserHost,
    ImplFrame, Rect, WindowInfo,
};

use super::client::{BrowserSlot, HebClient};

/// 单个对话的 CEF 浏览器实例。
pub struct CefBrowser {
    browser: cef::Browser,
    /// 持有 client 保持回调存活（drop 会断开 handler）。
    _client: Client,
    /// 导航历史（与 wry 路径对称，供前进/后退）。
    pub history: Vec<String>,
    pub cursor: usize,
}

impl CefBrowser {
    /// 在宿主 NSView（Tauri 主窗 contentView）的指定矩形内创建浏览器。
    /// `parent_view` 是 `*mut c_void` 形式的 NSView 指针（caller 从 Tauri window 取）。
    ///
    /// browser 句柄经 client 的 on_after_created 异步回调填入 slot。CEF 单进程模式下
    /// create_browser 同步触发回调，返回时 slot 已就绪；若 dev 实测异步，改轮询等待。
    #[cfg(target_os = "macos")]
    pub fn create(
        parent_view: *mut std::ffi::c_void,
        url: &str,
        bounds: Rect,
        init_script: Arc<String>,
    ) -> Option<Self> {
        let slot: BrowserSlot = Arc::new(Mutex::new(None));
        let mut client = HebClient::make(slot.clone(), init_script);
        let window_info = WindowInfo::default().set_as_child(parent_view, &bounds);
        let url_cef = CefString::from(url);
        let settings = BrowserSettings::default();
        let created = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&url_cef),
            Some(&settings),
            None,
            None,
        );
        if created != 1 {
            tracing::warn!("CEF browser_host_create_browser 失败");
            return None;
        }
        let browser = slot.lock().unwrap().take()?;
        Some(Self {
            browser,
            _client: client,
            history: vec![url.to_string()],
            cursor: 0,
        })
    }

    /// 导航到新 URL（main frame load_url）。
    pub fn navigate(&mut self, url: &str) {
        if let Some(frame) = self.browser.main_frame() {
            frame.load_url(Some(&CefString::from(url)));
        }
        self.history.truncate(self.cursor + 1);
        self.history.push(url.to_string());
        self.cursor = self.history.len() - 1;
    }

    pub fn back(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.browser.go_back();
        }
    }

    pub fn forward(&mut self) {
        if self.cursor + 1 < self.history.len() {
            self.cursor += 1;
            self.browser.go_forward();
        }
    }

    pub fn reload(&self) {
        self.browser.reload();
    }

    /// 下行：在 main frame 执行 JS（等价 wry 的 webview.eval，注入 inspector 下行消息）。
    pub fn eval(&self, js: &str) {
        if let Some(frame) = self.browser.main_frame() {
            frame.execute_java_script(Some(&CefString::from(js)), None, 0);
        }
    }

    /// 重设子视图尺寸（bounds 跟随侧边栏）。CEF 子视图随 NSView 走，多数情况无需手动；
    /// 需要时通过 host 通知。dev 验证 bounds 是否自动跟随父 NSView。
    pub fn set_bounds(&self, _bounds: Rect) {
        if let Some(host) = self.browser.host() {
            host.was_resized();
        }
    }

    pub fn close(&self) {
        if let Some(host) = self.browser.host() {
            host.close_browser(1);
        }
    }
}

/// per-session CEF 实例表（与 wry BrowserState.instances 对称）。
#[derive(Default)]
pub struct CefHost {
    instances: Mutex<HashMap<String, CefBrowser>>,
}

impl CefHost {
    pub fn has(&self, session_id: &str) -> bool {
        self.instances.lock().unwrap().contains_key(session_id)
    }

    pub fn insert(&self, session_id: String, browser: CefBrowser) {
        self.instances.lock().unwrap().insert(session_id, browser);
    }

    /// 对指定实例执行操作（导航/eval/bounds 等）。返回 false 表示无该实例。
    pub fn with<R>(&self, session_id: &str, f: impl FnOnce(&mut CefBrowser) -> R) -> Option<R> {
        let mut guard = self.instances.lock().unwrap();
        guard.get_mut(session_id).map(f)
    }

    pub fn remove(&self, session_id: &str) {
        if let Some(b) = self.instances.lock().unwrap().remove(session_id) {
            b.close();
        }
    }
}

