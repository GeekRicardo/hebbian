//! 对话设置弹窗。对应原前端 `SessionSettingsDialog.tsx`：居中卡片，
//! 展示当前对话的供应商 / 模型 / Agent / 流式开关 / 目录与工具 / Skills / 规则。

use gpui::{div, prelude::*, px, Context};

use crate::assets::Icon;
use crate::ui::widgets::{h_flex, shadow_lifted, v_flex};
use crate::ui::HebbianApp;

pub fn render(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> Option<impl IntoElement> {
    if !app.session_settings_open {
        return None;
    }
    let session = app.state.current.as_ref()?;
    let theme = app.theme.clone();

    let provider_name = app
        .state
        .providers
        .iter()
        .find(|p| p.id == session.provider_id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| session.provider_id.clone());

    let workdir = session
        .workdir
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "跟随全局默认".to_string());

    Some(
        // 遮罩：点空白处关闭，和原来的 Dialog 行为一致。
        div()
            .id("session-settings-scrim")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x0f172a40))
            .on_click(cx.listener(|this, _, _, cx| {
                this.session_settings_open = false;
                cx.notify();
            }))
            .child(
                v_flex()
                    .id("session-settings")
                    .w(px(560.))
                    .max_h(px(620.))
                    .rounded(px(18.))
                    .border_1()
                    .border_color(theme.card_line)
                    .bg(theme.card_strong)
                    .shadow(shadow_lifted(gpui::rgba(0x2d3d5333).into()))
                    // 卡片自己吞掉点击，否则点内容会穿到遮罩上把弹窗关掉。
                    .on_click(|_, _, _| {})
                    .child(header(app, cx))
                    .child(
                        v_flex()
                            .id("session-settings-body")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(20.))
                            .py(px(16.))
                            .gap(px(2.))
                            .child(field(&theme, "供应商", provider_name))
                            .child(field(&theme, "模型", session.model.clone()))
                            .child(field(
                                &theme,
                                "Agent",
                                session
                                    .prompt_id
                                    .clone()
                                    .unwrap_or_else(|| "无 Agent".to_string()),
                            ))
                            .child(field(
                                &theme,
                                "流式输出",
                                if session.stream { "开" } else { "关" }.to_string(),
                            ))
                            .child(field(&theme, "工作目录", workdir))
                            .child(field(
                                &theme,
                                "允许访问的路径",
                                match session.allowed_paths.as_ref() {
                                    Some(paths) if !paths.is_empty() => {
                                        format!("{} 条", paths.len())
                                    }
                                    _ => "跟随全局默认".to_string(),
                                },
                            ))
                            .child(field(
                                &theme,
                                "推理强度",
                                session
                                    .reasoning
                                    .as_ref()
                                    .map(|r| format!("{:?}", r.effort))
                                    .unwrap_or_else(|| "不启用".to_string()),
                            ))
                            .child(
                                div()
                                    .mt(px(14.))
                                    .p(px(12.))
                                    .rounded(px(10.))
                                    .bg(theme.accent_soft)
                                    .text_size(px(12.))
                                    .text_color(theme.muted)
                                    .child(
                                        "这些值目前只读。改对话设置的表单还没搬过来，\
                                         要改先在原来的桌面端改，两边读的是同一个对话文件。",
                                    ),
                            ),
                    ),
            ),
    )
}

fn header(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    h_flex()
        .h(px(56.))
        .flex_none()
        .px(px(20.))
        .justify_between()
        .border_b_1()
        .border_color(theme.line)
        .child(
            v_flex()
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight(650.))
                        .child("对话设置"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child("这个对话用的供应商、模型、Agent 与目录"),
                ),
        )
        .child(
            div()
                .id("close-session-settings")
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent_soft))
                .child(Icon::X.el(px(13.), theme.faint))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.session_settings_open = false;
                    cx.notify();
                })),
        )
}

fn field(
    theme: &crate::theme::Theme,
    label: &'static str,
    value: String,
) -> impl IntoElement {
    h_flex()
        .h(px(36.))
        .justify_between()
        .gap(px(16.))
        .border_b_1()
        .border_color(theme.line)
        .child(
            div()
                .flex_none()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(label),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(12.))
                .child(value),
        )
}
