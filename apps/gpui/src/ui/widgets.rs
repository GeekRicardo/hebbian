//! 通用小件：flex 容器速写、软阴影、时间戳。

use gpui::{div, prelude::*, px, BoxShadow, Div, Hsla, Point};

/// 横向 flex + 垂直居中。CSS 里 `.dsp-*` 一族基本都是这个组合。
pub fn h_flex() -> Div {
    div().flex().flex_row().items_center()
}

/// 纵向 flex。
pub fn v_flex() -> Div {
    div().flex().flex_col()
}

/// `--dsp-shadow-soft`：`0 8px 20px rgba(45,61,83,0.045)`。
pub fn shadow_soft(color: Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color,
        offset: Point {
            x: px(0.),
            y: px(8.),
        },
        blur_radius: px(20.),
        spread_radius: px(0.),
    }]
}

/// `--dsp-shadow`：`0 14px 38px rgba(45,61,83,0.08)`。
pub fn shadow_lifted(color: Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color,
        offset: Point {
            x: px(0.),
            y: px(14.),
        },
        blur_radius: px(38.),
        spread_radius: px(0.),
    }]
}

/// 当前时间（毫秒）。会话列表的「x 分钟前」按它算。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 拖拽把手的载荷类型。每个可拖的边界一个类型，避免不同把手的拖拽事件串台。
#[derive(Clone, Debug)]
pub struct EditorDivider;

#[derive(Clone, Debug)]
pub struct RightDivider;

/// 一个空视图：`on_drag` 要求提供拖拽预览，但改宽度不需要跟手的幽灵元素。
pub struct NoDragPreview;

impl gpui::Render for NoDragPreview {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        div()
    }
}
