//! CEF Client + 各 handler（架构 §8.5 M2）。
//!
//! Client 聚合 LifeSpanHandler（拿 browser 句柄 / 关闭生命周期）+ LoadHandler
//! （页面加载完注入 inspector.js）。browser 句柄在 `on_after_created` 异步回调里
//! 经共享 slot 回传给 CefBrowser——这是 CEF 创建浏览器的标准异步模式。
//!
//! ⚠️ dev 验证点：on_after_created 是否在 create_browser 后被回调、inspector 注入
//! 时机是否正确——需 `pnpm tauri dev --features cef-preview` 真机确认。

use std::sync::{Arc, Mutex};

use cef::*;

/// browser 句柄回传槽：create 时建空 slot，on_after_created 回调填入。
pub type BrowserSlot = Arc<Mutex<Option<Browser>>>;

wrap_client! {
    pub struct HebClient {
        slot: BrowserSlot,
        init_script: Arc<String>,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(HebLifeSpan::new(self.slot.clone()))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(HebLoad::new(self.init_script.clone()))
        }
    }
}

impl HebClient {
    pub fn make(slot: BrowserSlot, init_script: Arc<String>) -> Client {
        Self::new(slot, init_script)
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
    }

    impl LoadHandler {
        // 主框架加载完成 → 注入 inspector.js（等价 wry 的 initialization_script）。
        // is_main_frame 判断避免 iframe 重复注入。
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
