//! CEF Client + 各 handler（架构 §8.5 M2）。
//!
//! Client 聚合：LifeSpanHandler（拿 browser 句柄 / 关闭生命周期）+ LoadHandler
//! （加载完注入 inspector.js + 回传加载态）+ DisplayHandler（URL/标题变化回传）。
//! browser 句柄在 `on_after_created` 异步回调里经共享 slot 回传给 CefBrowser。
//!
//! 导航事件（URL / 标题 / 加载态）经 `NavCb` 回调上抛给 mod.rs，emit 成
//! `browser://state` / `browser://title`（与 wry 路径同事件名），前端地址栏/历史
//! 才能跟随 302 跳转、页面内导航更新。

use std::sync::{Arc, Mutex};

use cef::*;

/// browser 句柄回传槽：create 时建空 slot，on_after_created 回调填入。
pub type BrowserSlot = Arc<Mutex<Option<Browser>>>;

/// 导航事件回调：mod.rs 提供，内部持 AppHandle emit browser:// 事件。
#[derive(Clone)]
pub enum NavUpdate {
    /// 地址变化（含 302 跟随后的真实 URL）。
    Url(String),
    /// 标题变化。
    Title(String),
    /// 加载态变化（true=加载中）。
    Loading(bool),
}
pub type NavCb = Arc<dyn Fn(NavUpdate) + Send + Sync>;

wrap_client! {
    pub struct HebClient {
        slot: BrowserSlot,
        init_script: Arc<String>,
        nav: NavCb,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(HebLifeSpan::new(self.slot.clone()))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(HebLoad::new(self.init_script.clone(), self.nav.clone()))
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(HebDisplay::new(self.nav.clone()))
        }
    }
}

impl HebClient {
    pub fn make(slot: BrowserSlot, init_script: Arc<String>, nav: NavCb) -> Client {
        Self::new(slot, init_script, nav)
    }
}

wrap_life_span_handler! {
    pub struct HebLifeSpan {
        slot: BrowserSlot,
    }

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(b) = browser {
                *self.slot.lock().unwrap() = Some(b.clone());
            }
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            *self.slot.lock().unwrap() = None;
        }
    }
}

wrap_load_handler! {
    pub struct HebLoad {
        init_script: Arc<String>,
        nav: NavCb,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            _can_go_back: ::std::os::raw::c_int,
            _can_go_forward: ::std::os::raw::c_int,
        ) {
            (self.nav)(NavUpdate::Loading(is_loading == 1));
        }

        // 主框架加载完成 → 注入 inspector.js（等价 wry 的 initialization_script）。
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            let Some(frame) = frame else { return };
            // 只在主框架注入，避免 iframe 重复注入 inspector。
            if frame.is_main() != 1 {
                return;
            }
            frame.execute_java_script(
                Some(&CefString::from(self.init_script.as_str())),
                Some(&CefString::from("heb://inspector")),
                0,
            );
        }
    }
}

wrap_display_handler! {
    pub struct HebDisplay {
        nav: NavCb,
    }

    impl DisplayHandler {
        fn on_address_change(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            // 只认主框架地址（iframe 的 src 变化不该更新地址栏）。
            let is_main = frame.map(|f| f.is_main() == 1).unwrap_or(false);
            if !is_main {
                return;
            }
            if let Some(u) = url {
                (self.nav)(NavUpdate::Url(u.to_string()));
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if let Some(t) = title {
                (self.nav)(NavUpdate::Title(t.to_string()));
            }
        }
    }
}
