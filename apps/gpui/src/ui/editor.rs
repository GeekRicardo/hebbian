//! 编辑区。对应原前端 `EditorPane.tsx`：夹在聊天列与右侧工作台之间，
//! 只有打开了文件才出现，把聊天列挤窄。
//!
//! 语法高亮走 gpui-component 的 code editor（内置 tree-sitter），语言按扩展名推。

use std::path::Path;

use gpui::{div, prelude::*, px, Context, Window};
use gpui_component::input::{Input, InputState};

use crate::assets::Icon;
use crate::ui::widgets::{h_flex, v_flex, EditorDivider, NoDragPreview};
use crate::ui::HebbianApp;

/// 编辑区默认 / 最小 / 最大宽度，与原前端的 VIEWER_* 常量一致。
pub const DEFAULT_WIDTH: f32 = 700.0;
pub const MIN_WIDTH: f32 = 360.0;
pub const MAX_WIDTH: f32 = 1100.0;

/// 按扩展名猜语言。猜不出来就按纯文本走——高亮不对不如不高亮。
pub fn language_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" | "cxx" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "sh" | "bash" | "zsh" => "bash",
        "md" | "markdown" => "markdown",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" => "html",
        "css" => "css",
        "sql" => "sql",
        "swift" => "swift",
        "zig" => "zig",
        "gn" | "gni" => "python", // GN 语法接近 Python，高亮上够用
        _ => "text",
    }
}

/// 没打开文件就不占位——与原前端 `if (!hasTabs) return null` 一致。
pub fn render(
    app: &HebbianApp,
    _window: &mut Window,
    cx: &mut Context<HebbianApp>,
) -> Option<impl IntoElement> {
    if app.state.open_files.is_empty() {
        return None;
    }
    let active = app.state.active_file.as_ref()?;
    let editor = app.editors.get(active)?;
    let theme = app.theme.clone();
    let dir = active
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    Some(
        v_flex()
            .relative()
            .w(px(app.editor_width))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(theme.line)
            .bg(theme.card_strong)
            // 左边缘拖拽改宽：右边缘固定，往左拖变宽（与原前端方向一致）。
            .child(
                div()
                    .id("editor-resize")
                    .absolute()
                    .left(px(0.))
                    .top(px(0.))
                    .w(px(4.))
                    .h_full()
                    .cursor_col_resize()
                    .hover(|this| this.bg(theme.accent_soft))
                    .on_drag(EditorDivider, |_, _, _, cx| cx.new(|_| NoDragPreview))
                    .on_drag_move(cx.listener(
                        |this, e: &gpui::DragMoveEvent<EditorDivider>, _, cx| {
                            let delta = e.bounds.origin.x - e.event.position.x;
                            let next = (this.editor_width + f32::from(delta))
                                .clamp(MIN_WIDTH, MAX_WIDTH);
                            if (next - this.editor_width).abs() > 0.5 {
                                this.editor_width = next;
                                cx.notify();
                            }
                        },
                    )),
            )
            .child(tab_bar(app, &theme, dir, cx))
            // Cmd/Ctrl+S 保存当前标签。
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                let k = &event.keystroke;
                let save = k.key == "s" && (k.modifiers.platform || k.modifiers.control);
                if save {
                    this.save_active_file(cx);
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p(px(8.))
                    .text_size(px(12.))
                    .child(Input::new(editor).appearance(false).h_full()),
            ),
    )
}

/// 顶部标签条：文件名 + 所在目录 + 关闭。
fn tab_bar(
    app: &HebbianApp,
    theme: &crate::theme::Theme,
    dir: String,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let mut tabs = h_flex()
        .id("editor-tabs")
        .flex_1()
        .min_w_0()
        .overflow_x_scroll()
        .gap(px(2.));

    for path in &app.state.open_files {
        let active = app.state.active_file.as_ref() == Some(path);
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let switch_to = path.clone();
        let close = path.clone();
        tabs = tabs.child(
            h_flex()
                .id(gpui::SharedString::from(format!("tab-{}", path.display())))
                .h(px(28.))
                .px(px(8.))
                .gap(px(6.))
                .rounded(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.card_strong).text_color(theme.text)
                })
                .when(!active, |this| {
                    this.text_color(theme.muted)
                        .hover(|this| this.bg(theme.accent_soft))
                })
                .child(Icon::FileText.el(px(12.), theme.faint))
                .child(name)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.active_file = Some(switch_to.clone());
                    cx.notify();
                }))
                .child(
                    div()
                        .id("x")
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.line))
                        .child(Icon::X.el(px(10.), theme.faint))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_file(&close);
                            cx.notify();
                        })),
                ),
        );
    }

    h_flex()
        .h(px(36.))
        .flex_none()
        .px(px(10.))
        .gap(px(8.))
        .justify_between()
        .border_b_1()
        .border_color(theme.line)
        .child(tabs)
        .children(app.state.saved_notice.clone().map(|notice| {
            div()
                .flex_none()
                .text_size(px(11.))
                .text_color(theme.green)
                .child(notice)
        }))
        .child(
            div()
                .flex_none()
                .max_w(px(220.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(11.))
                .text_color(theme.faint)
                .child(dir),
        )
}

/// 文件读回来后重建编辑器实体。每次打开新文件都换一个，
/// 因为语言是建 `InputState` 时定死的，换文件必须换实例。
pub fn open(
    app: &mut HebbianApp,
    path: &Path,
    text: &str,
    window: &mut Window,
    cx: &mut Context<HebbianApp>,
) {
    // 重复打开同一个文件直接复用已有实例。
    if app.editors.contains_key(path) {
        return;
    }
    let language = language_for(path);
    let text = text.to_string();
    let editor = cx.new(|cx| {
        let mut state = InputState::new(window, cx).code_editor(language);
        state.set_value(text, window, cx);
        state
    });
    app.editors.insert(path.to_path_buf(), editor);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn language_is_guessed_from_extension() {
        assert_eq!(language_for(&PathBuf::from("a/b/main.rs")), "rust");
        assert_eq!(language_for(&PathBuf::from("x.TSX")), "typescript");
        assert_eq!(language_for(&PathBuf::from("Makefile")), "text");
    }
}
