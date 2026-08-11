//! 内置浏览器预览面板。
//!
//! 这里用 wry 嵌一个系统 webview——**注意这与「不要 Tauri」不冲突**：用户嫌的是
//! 把整个界面塞进 WebView，而预览面板本身就是个浏览器，原 Desktop 的预览也是
//! wry / CEF。界面主体仍然是 gpui 原生绘制，只有这一块内容区是网页。

use gpui::{div, prelude::*, px, Context, Entity, Window};
use gpui_component::input::{Input, InputState};
use gpui_component::webview::WebView;

use crate::assets::Icon;
use crate::ui::widgets::{h_flex, v_flex};
use crate::ui::HebbianApp;

/// 这个平台能不能挂子 webview。
///
/// 子 webview 要拿主窗口的原生句柄，而 **gpui 0.2.2 的 X11 后端里
/// `HasWindowHandle::window_handle()` 直接 `unimplemented!()`**——调下去不是返回
/// 错误而是整个进程 panic。macOS（AppKit）与 Wayland 都实现了，所以只在 X11 上拦。
/// Linux 下用 `WAYLAND_DISPLAY` 是否存在来判断跑的是哪套。
///
/// 这里宁可少一个功能也不能让点一下就崩——等 gpui 补上 X11 的实现再放开。
fn host_supports_child_webview() -> bool {
    if cfg!(target_os = "linux") {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
    } else {
        true
    }
}

/// 起一个挂在主窗口上的子 webview。
///
/// 失败不是致命错误——没装 webview 运行时的机器上预览用不了，
/// 但别的功能不该跟着崩，所以这里返回 Option 由调用方降级。
pub fn create(url: &str, window: &mut Window, cx: &mut Context<HebbianApp>) -> Option<Entity<WebView>> {
    if !host_supports_child_webview() {
        return None;
    }
    // 必须用 gpui-component 再导出的那份 wry：它内部持有的是同一份类型，
    // 自己再依赖一个 wry 会编出两套互不兼容的 WebView。
    let built = gpui_component::wry::WebViewBuilder::new()
        .with_url(url)
        .build_as_child(&window);
    match built {
        Ok(webview) => Some(cx.new(|cx| WebView::new(webview, window, cx))),
        Err(err) => {
            tracing::warn!(error = %err, "起不了内置浏览器");
            None
        }
    }
}

pub fn panel(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    let Some(webview) = app.webview.as_ref() else {
        return v_flex()
            .p(px(14.))
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .child("在上面输入网址就能开预览"),
            )
            .child(address_bar(app, cx))
            .into_any_element();
    };

    v_flex()
        .flex_1()
        .min_h_0()
        .child(address_bar(app, cx))
        .child(div().flex_1().min_h_0().child(webview.clone()))
        .into_any_element()
}

/// 地址栏：回车即打开。
fn address_bar(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    h_flex()
        .h(px(32.))
        .flex_none()
        .px(px(8.))
        .gap(px(6.))
        .border_b_1()
        .border_color(theme.line)
        .child(Icon::Globe.el(px(13.), theme.faint))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.))
                .child(Input::new(&app.url_input).appearance(false)),
        )
        .child(
            div()
                .id("browser-go")
                .px(px(8.))
                .py(px(3.))
                .rounded(px(6.))
                .text_size(px(11.))
                .text_color(theme.accent)
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent_soft))
                .child("打开")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_preview(window, cx);
                })),
        )
}

/// 建/换 URL 时都走这里。wry 的子 webview 换地址用 `load_url`，不用重建。
pub fn navigate(app: &mut HebbianApp, url: &str, window: &mut Window, cx: &mut Context<HebbianApp>) {
    if let Some(view) = app.webview.as_ref() {
        view.update(cx, |view, _| {
            let _ = view.load_url(url);
        });
        return;
    }
    app.webview = create(url, window, cx);
    if app.webview.is_none() {
        app.state.error = Some(if host_supports_child_webview() {
            "这台机器上起不了内置浏览器".to_string()
        } else {
            "当前显示服务下还开不了内置浏览器（X11 暂不支持，Wayland 与 macOS 可以）"
                .to_string()
        });
    }
}

/// 输入框里的地址补全协议头——用户多半只打 `localhost:5173`。
pub fn normalize_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "about:blank".to_string();
    }
    if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_gets_http_prefix() {
        assert_eq!(normalize_url("localhost:5173"), "http://localhost:5173");
        assert_eq!(normalize_url(" example.com "), "http://example.com");
    }

    #[test]
    fn explicit_scheme_is_kept() {
        assert_eq!(normalize_url("https://a.b"), "https://a.b");
        assert_eq!(normalize_url("file:///tmp/x.html"), "file:///tmp/x.html");
    }

    /// X11 上必须判定为不支持——否则点开预览会 panic 掉整个应用。
    #[test]
    fn x11_is_treated_as_unsupported() {
        if cfg!(target_os = "linux") {
            let had = std::env::var_os("WAYLAND_DISPLAY").is_some();
            assert_eq!(host_supports_child_webview(), had);
        } else {
            assert!(host_supports_child_webview());
        }
    }

    #[test]
    fn empty_goes_blank() {
        assert_eq!(normalize_url("   "), "about:blank");
    }
}
