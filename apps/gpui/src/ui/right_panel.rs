//! 右侧工作台：文件目录面板 + 最右侧的图标竖条。
//!
//! 对应 `RightSidebar.tsx` / `FileTreePanel.tsx`。图标条上每个按钮切换一个工作台
//! 面板（文件 / 编辑 / 目标 / Git / 待办 / 计划 / 对话 / 浏览器 / 终端）。

use gpui::{div, prelude::*, px, Context, Window};

use crate::assets::Icon;
use crate::ui::widgets::{h_flex, v_flex};
use crate::ui::HebbianApp;

/// 工作台可选的面板。顺序与截图里的图标条自上而下一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workbench {
    Files,
    Editor,
    Target,
    Git,
    Todo,
    Plan,
    Chat,
    Browser,
    Terminal,
}

impl Workbench {
    const ALL: [Workbench; 9] = [
        Workbench::Files,
        Workbench::Editor,
        Workbench::Target,
        Workbench::Git,
        Workbench::Todo,
        Workbench::Plan,
        Workbench::Chat,
        Workbench::Browser,
        Workbench::Terminal,
    ];

    fn icon(self) -> Icon {
        match self {
            Workbench::Files => Icon::FileText,
            Workbench::Editor => Icon::Pencil,
            Workbench::Target => Icon::Target,
            Workbench::Git => Icon::GitBranch,
            Workbench::Todo => Icon::ListTodo,
            Workbench::Plan => Icon::List,
            Workbench::Chat => Icon::MessageSquare,
            Workbench::Browser => Icon::Globe,
            Workbench::Terminal => Icon::Terminal,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Workbench::Files => "文件目录",
            Workbench::Editor => "编辑",
            Workbench::Target => "目标",
            Workbench::Git => "Git",
            Workbench::Todo => "待办",
            Workbench::Plan => "计划",
            Workbench::Chat => "对话",
            Workbench::Browser => "浏览器",
            Workbench::Terminal => "终端",
        }
    }
}

pub fn render(app: &mut HebbianApp, _window: &mut Window, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    h_flex()
        .flex_none()
        .h_full()
        .items_start()
        .border_l_1()
        .border_color(theme.line)
        .when(!app.right_collapsed, |this| this.child(panel(app, cx)))
        .child(rail(app, cx))
}

/// 文件目录面板本体。
fn panel(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let workdir = app
        .state
        .current
        .as_ref()
        .and_then(|s| s.workdir.clone())
        .map(|p| p.to_string_lossy().to_string());

    v_flex()
        .w(px(260.))
        .h_full()
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(theme.right_bg_top, 0.),
            gpui::linear_color_stop(theme.right_bg_bottom, 1.),
        ))
        .child(
            // 顶部标题条（截图里的「文件目录」+ `{}` 与展开箭头）。
            h_flex()
                .h(px(38.))
                .px(px(12.))
                .justify_between()
                .text_size(px(12.))
                .text_color(theme.text)
                .child("文件目录")
                .child(
                    h_flex()
                        .gap(px(6.))
                        .text_color(theme.faint)
                        .child(Icon::Braces.el(px(13.), theme.faint))
                        .child(
                            div()
                                .id("collapse-right")
                                .cursor_pointer()
                                .child(Icon::ChevronRight.el(px(13.), theme.faint))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.right_collapsed = true;
                                    cx.notify();
                                })),
                        ),
                ),
        )
        .child(
            h_flex()
                .h(px(30.))
                .px(px(12.))
                .justify_between()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child("文件目录")
                .child(Icon::RefreshCw.el(px(12.), theme.faint)),
        )
        .child(match workdir {
            Some(dir) => tree_placeholder(app, dir).into_any_element(),
            None => div()
                .p(px(14.))
                .text_size(px(12.))
                .text_color(theme.muted)
                .child("这个对话还没绑定文件夹")
                .into_any_element(),
        })
}

/// 文件树。真正的按需读盘还没接上，先把根目录名按树的样式画出来，
/// 保证布局与真实结构一致，接上目录读取时只换数据源。
fn tree_placeholder(app: &HebbianApp, dir: String) -> impl IntoElement {
    let theme = app.theme.clone();
    let leaf = std::path::Path::new(&dir)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or(dir);

    v_flex().id("file-tree").flex_1().min_h_0().overflow_y_scroll().px(px(8.)).child(
        h_flex()
            .h(px(24.))
            .gap(px(4.))
            .text_size(px(12.))
            .text_color(theme.text)
            .child(Icon::ChevronDown.el(px(12.), theme.faint))
            .child(Icon::Folder.el(px(13.), theme.muted))
            .child(leaf),
    )
}

/// 最右侧的图标竖条。
fn rail(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let mut rail = v_flex()
        .w(px(38.))
        .h_full()
        .flex_none()
        .items_center()
        .pt(px(14.))
        .gap(px(4.))
        .bg(theme.right_bg_bottom);

    // 折叠状态下第一个按钮变成「展开」。
    rail = rail.child(
        div()
            .id("expand-right")
            .size(px(26.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .text_color(theme.muted)
            .cursor_pointer()
            .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
            .child(
                if app.right_collapsed {
                    Icon::PanelRightOpen
                } else {
                    Icon::PanelRightClose
                }
                .el(px(15.), theme.muted),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.right_collapsed = !this.right_collapsed;
                cx.notify();
            })),
    );

    for item in Workbench::ALL {
        rail = rail.child(
            div()
                .id(item.title())
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.))
                .text_color(theme.muted)
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
                .child(item.icon().el(px(15.), theme.muted))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.right_collapsed = false;
                    this.state.error = Some(format!("「{}」面板还在搬运中", item.title()));
                    cx.notify();
                })),
        );
    }
    rail
}
