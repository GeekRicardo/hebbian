//! 视图层。结构与原 Web 前端一一对应：
//! `DesktopShell` = 左侧栏 + 聊天列 + 右侧工作台，弹层挂在最外层。

mod chat;
mod hue;
mod right_panel;
mod sidebar;
mod widgets;

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, Window,
};
use gpui_component::input::{InputEvent, InputState};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::core::{Core, CoreUpdate};
use crate::state::AppState;
use crate::theme::{Theme, ThemePreset};

pub fn init(_cx: &mut App) {}

/// 根视图。整个应用只有这一个 `Render` 实体持有状态——子视图都是纯函数式的
/// `IntoElement`，直接读 `&AppState` 渲染。原前端也是「单 store + 无状态组件」的形态，
/// 保持一致能让两边对照着改。
pub struct HebbianApp {
    pub state: AppState,
    pub theme: Theme,
    pub preset: ThemePreset,
    pub hue: f32,

    /// 色系弹窗是否展开（footer 调色盘按钮）。
    pub hue_popover_open: bool,
    /// 右侧工作台是否折叠。
    pub right_collapsed: bool,

    pub composer: Entity<InputState>,
    pub search: Entity<InputState>,
    focus: FocusHandle,
}

impl HebbianApp {
    pub fn new(
        core: Core,
        mut updates: UnboundedReceiver<CoreUpdate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(1, 6)
                .placeholder("输入消息，Enter 发送，Shift+Enter 换行…")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索"));

        // 输入框回车即发送。Shift+Enter 由 InputState 自己插换行，不会走到这里。
        cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.send_current_input(window, cx);
                }
            },
        )
        .detach();

        // 搜索框改动直接进 state，项目分桶下一帧就按新词过滤。
        cx.subscribe_in(
            &search,
            window,
            |this, state, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.state.query = state.read(cx).value().to_string();
                    cx.notify();
                }
            },
        )
        .detach();

        // core → UI 的唯一事件泵。mpsc 的 recv 不需要 tokio 运行时上下文，
        // 可以直接在 gpui 执行器上 await。
        cx.spawn(async move |this, cx| {
            while let Some(update) = updates.recv().await {
                let alive = this
                    .update(cx, |this, cx| {
                        if this.state.apply(update) {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        core.refresh_catalog();

        // 调试入口：`HEBBIAN_GPUI_OPEN=<session_id>` 启动即打开某个对话。
        // 无人值守截图 / 排查渲染时不用先点一下侧栏，与 heb CLI 的脚本化调试同一思路。
        if let Ok(session_id) = std::env::var("HEBBIAN_GPUI_OPEN") {
            if !session_id.is_empty() {
                core.open_session(session_id);
            }
        }

        Self {
            state: AppState::new(core),
            theme: Theme::new(ThemePreset::Glacier, 208.0),
            preset: ThemePreset::Glacier,
            hue: 208.0,
            hue_popover_open: false,
            right_collapsed: false,
            composer,
            search,
            focus: cx.focus_handle(),
        }
    }

    pub fn set_theme(&mut self, preset: ThemePreset, hue: f32, cx: &mut Context<Self>) {
        self.preset = preset;
        self.hue = hue;
        self.theme = Theme::new(preset, hue);
        cx.notify();
    }

    /// 发送输入框里的内容。空白不发；发完清空。
    pub fn send_current_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.current_id().map(str::to_string) else {
            self.state.error = Some("先新建或选择一个对话".to_string());
            cx.notify();
            return;
        };
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.state.core.send_message(session_id, text);
        cx.notify();
    }
}

impl Focusable for HebbianApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for HebbianApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        div()
            .id("dsp-shell")
            .track_focus(&self.focus)
            .flex()
            .size_full()
            .overflow_hidden()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.))
            .child(sidebar::render(self, window, cx))
            .child(chat::render(self, window, cx))
            .child(right_panel::render(self, window, cx))
            .children(self.state.error.clone().map(|message| toast(&theme, message, cx)))
    }
}

/// 顶部居中的错误提示条，对应原前端 sonner 的 `toast.error`。
/// 点一下消失——没有自动淡出定时器，出错信息留在眼前更容易被看见。
fn toast(
    theme: &Theme,
    message: String,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    div()
        .absolute()
        .top(px(16.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .id("toast")
                .max_w(px(520.))
                .px(px(14.))
                .py(px(10.))
                .rounded(px(10.))
                .border_1()
                .border_color(theme.danger)
                .bg(theme.card_strong)
                .text_size(px(12.))
                .text_color(theme.text)
                .cursor_pointer()
                .child(message)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.error = None;
                    cx.notify();
                })),
        )
}
