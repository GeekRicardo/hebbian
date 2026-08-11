//! 色系选择器。对应 `DesktopHueControl` + `.dsp-hue-*` 一族。
//!
//! 弹窗从 footer 的调色盘按钮向上弹出（CSS: `bottom: 56px; left/right: 18px`）。

use gpui::{div, prelude::*, px, Context};

use crate::assets::Icon;
use crate::theme::ThemePreset;
use crate::ui::widgets::{h_flex, v_flex};
use crate::ui::HebbianApp;

pub fn control(app: &mut HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let open = app.hue_popover_open;

    div()
        .relative()
        .flex_none()
        .child(
            div()
                .id("hue-button")
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.hue_button_bg)
                .text_color(theme.muted)
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent).text_color(gpui::white()))
                .child(Icon::Palette.el(px(15.), theme.muted))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.hue_popover_open = !this.hue_popover_open;
                    cx.notify();
                })),
        )
        .when(open, |_| div())
}

/// 弹窗单独挂在 footer 上：侧栏卡片是 `overflow: hidden`，
/// 挂在 30px 的按钮上会被裁掉左半边，所以按原 CSS 的做法让它横跨整个 footer。
pub fn popover_for_footer(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
) -> Option<impl IntoElement> {
    if !app.hue_popover_open {
        return None;
    }
    Some(popover(app, cx))
}

fn popover(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let mut presets = v_flex().gap(px(6.));

    // CSS 是 2 列网格；这里按两两一行铺，视觉等价。
    for pair in ThemePreset::ALL.chunks(2) {
        let mut row = h_flex().gap(px(6.));
        for preset in pair {
            row = row.child(preset_button(app, cx, *preset));
        }
        row = row.w_full();
        presets = presets.child(row);
    }

    v_flex()
        .absolute()
        .bottom(px(56.))
        .left(px(0.))
        .right(px(0.))
        .p(px(12.))
        .rounded(px(22.))
        .border_1()
        .border_color(theme.line)
        .bg(theme.hue_popover_bg)
        .child(
            div()
                .mb(px(9.))
                .text_size(px(11.))
                .font_weight(gpui::FontWeight(720.))
                .text_color(theme.muted)
                .child("统一色系"),
        )
        .child(presets)
        .child(hue_strip(app, cx))
        .child(
            h_flex()
                .mt(px(8.))
                .justify_between()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight(650.))
                .text_color(theme.muted)
                .child(format!("#{:02X}", app.hue as u32))
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(crate::theme::hsl_deg(app.hue, 92., 45., 1.0)),
                ),
        )
}

fn preset_button(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    preset: ThemePreset,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let active = app.preset == preset;
    let swatch = preset.swatch();

    h_flex()
        .id(preset.id())
        .flex_1()
        .min_w_0()
        .h(px(28.))
        .px(px(6.))
        .gap(px(5.))
        .rounded(px(10.))
        .text_size(px(11.))
        .cursor_pointer()
        .bg(if active {
            theme.accent_soft
        } else {
            theme.theme_preset_bg
        })
        .text_color(if active { theme.text } else { theme.muted })
        .hover(|this| this.bg(theme.accent_soft).text_color(theme.text))
        // 三色渐变色板：gpui 只有线性渐变，取首尾两色近似 CSS 的 135° 三段渐变。
        .child(
            div()
                .size(px(16.))
                .flex_none()
                .rounded_full()
                .bg(gpui::linear_gradient(
                    135.,
                    gpui::linear_color_stop(swatch[0], 0.),
                    gpui::linear_color_stop(swatch[2], 1.),
                )),
        )
        .child(div().whitespace_nowrap().child(preset.label()))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_theme(preset, preset.hue(), cx);
        }))
}

/// CSS 里是一个 conic-gradient 色环；gpui 没有锥形渐变，这里换成等价功能的
/// 色相条——点哪取哪，行为与色环一致（都是「选一个 0..359 的 hue」）。
fn hue_strip(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let mut strip = h_flex()
        .id("hue-strip")
        .mt(px(10.))
        .h(px(16.))
        .w_full()
        .overflow_hidden()
        .rounded(px(999.));

    // 24 段色块拼出色相条：段够细，视觉上是连续的。
    for i in 0..24 {
        let hue = i as f32 * 15.;
        let preset = app.preset;
        strip = strip.child(
            div()
                .id(gpui::SharedString::from(format!("hue-seg-{i}")))
                .flex_1()
                .h_full()
                .bg(crate::theme::hsl_deg(hue, 92., 58., 1.0))
                .cursor_pointer()
                .on_click(cx.listener(move |this: &mut HebbianApp, _, _, cx| {
                    this.set_theme(preset, hue, cx);
                })),
        );
    }
    strip
}

/// 供测试用：主题切换后 accent 必须跟着 hue 走。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_hue_drives_accent() {
        use crate::theme::Theme;
        let a = Theme::new(ThemePreset::Porcelain, ThemePreset::Porcelain.hue());
        let b = Theme::new(ThemePreset::Glacier, ThemePreset::Glacier.hue());
        assert!((a.accent.h - b.accent.h).abs() > 1e-3);
    }
}
