//! 编辑区。对应原前端 `EditorPane.tsx`：夹在聊天列与右侧工作台之间，
//! 只有打开了文件才出现，把聊天列挤窄。
//!
//! 语法高亮走 gpui-component 的 code editor（内置 tree-sitter），语言按扩展名推。

use std::path::Path;

use gpui::{div, prelude::*, px, Context, Window};
use gpui_component::input::{Input, InputState};

use crate::assets::Icon;
use crate::ui::widgets::{h_flex, v_flex};
use crate::ui::HebbianApp;

/// 编辑区默认宽度，与原前端 `VIEWER_DEFAULT_WIDTH` 一致。
const DEFAULT_WIDTH: f32 = 700.0;

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
    let (path, _) = app.state.open_file.as_ref()?;
    let editor = app.editor.as_ref()?;
    let theme = app.theme.clone();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    Some(
        v_flex()
            .w(px(DEFAULT_WIDTH))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(theme.line)
            .bg(theme.card_strong)
            .child(tab_bar(&theme, name, dir, cx))
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
    theme: &crate::theme::Theme,
    name: String,
    dir: String,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    h_flex()
        .h(px(36.))
        .flex_none()
        .px(px(10.))
        .gap(px(8.))
        .justify_between()
        .border_b_1()
        .border_color(theme.line)
        .child(
            h_flex()
                .gap(px(6.))
                .min_w_0()
                .child(Icon::FileText.el(px(13.), theme.muted))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.text)
                        .child(name),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child(dir),
                ),
        )
        .child(
            div()
                .id("close-editor")
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent_soft))
                .child(Icon::X.el(px(12.), theme.faint))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.open_file = None;
                    this.editor = None;
                    cx.notify();
                })),
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
    let language = language_for(path);
    let text = text.to_string();
    let editor = cx.new(|cx| {
        let mut state = InputState::new(window, cx).code_editor(language);
        state.set_value(text, window, cx);
        state
    });
    app.editor = Some(editor);
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
