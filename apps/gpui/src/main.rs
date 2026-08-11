//! Hebbian 原生 GUI surface（架构 §7.2）。
//!
//! 它与 Desktop / heb / hebweb 一样只是壳：对话走 `surface-session`，同步能力走
//! `core-rpc`，自己不碰 storage / provider / HITL 业务。视觉以现 Desktop 为准
//! （见 `theme.rs` 对 `desktopShell.css` 的逐项对照）。

mod assets;
mod core;
mod diff;
mod prefs;
mod state;
mod terminal;
mod theme;
mod tool_label;
mod ui;

use gpui::{
    point, prelude::*, px, size, AnyView, App, Application, Bounds, TitlebarOptions,
    WindowBounds, WindowOptions,
};
use gpui_component::Root;

use crate::assets::Assets;
use crate::core::Core;
use crate::ui::HebbianApp;

fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let (core, updates) = match Core::start() {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("core 启动失败：{err}");
            std::process::exit(1);
        }
    };

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        ui::init(cx);

        let bounds = Bounds::centered(None, size(px(1440.), px(920.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Hebbian".into()),
                        // 交通灯浮在侧栏顶部的留白上——原 Desktop 也是无标题栏、
                        // `.dsp-window-space` 这块 22px 高的空档就是给它让位的。
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(18.), px(18.))),
                    }),
                    window_min_size: Some(size(px(960.), px(640.))),
                    ..Default::default()
                },
                |window, cx| {
                    let app = cx.new(|cx| HebbianApp::new(core.clone(), updates, window, cx));
                    cx.new(|cx| Root::new(AnyView::from(app), window, cx))
                },
            )
            .expect("打开窗口失败");

        window
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        cx.activate(true);
    });
}
