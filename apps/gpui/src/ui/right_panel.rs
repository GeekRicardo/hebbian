//! 右侧工作台：文件目录面板 + 最右侧的图标竖条。
//!
//! 对应 `RightSidebar.tsx` / `FileTreePanel.tsx`。图标条上每个按钮切换一个工作台
//! 面板（文件 / 编辑 / 目标 / Git / 待办 / 计划 / 对话 / 浏览器 / 终端）。

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, Window};

use crate::assets::Icon;
use crate::core::DirEntry;
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

    /// 还没实现的面板里那句「这块以后放什么」。
    fn blurb(self) -> &'static str {
        match self {
            Workbench::Files => "当前对话工作目录里的文件。",
            Workbench::Editor => "点开文件后在这里改，还没搬过来。",
            Workbench::Target => "这轮对话的目标与验收条件，还没搬过来。",
            Workbench::Git => "改了哪些文件、能不能一键回退，还没搬过来。",
            Workbench::Todo => "这轮的待办清单。",
            Workbench::Plan => "计划模式下的方案与批注，还没搬过来。",
            Workbench::Chat => "分叉出去的旁支对话，还没搬过来。",
            Workbench::Browser => "内置浏览器预览，还没搬过来。",
            Workbench::Terminal => "内置终端，还没搬过来。",
        }
    }

    pub fn title(self) -> &'static str {
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
    let workdir = app.state.current.as_ref().and_then(|s| s.workdir.clone());

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
                .child(app.workbench.title())
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
        .child(match app.workbench {
            Workbench::Files => file_panel(app, cx, workdir).into_any_element(),
            Workbench::Todo => todo_panel(app).into_any_element(),
            other => empty_panel(app, other).into_any_element(),
        })
}

/// 文件面板：一行小标题 + 刷新，下面是树。
fn file_panel(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    workdir: Option<PathBuf>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let refresh_dir = workdir.clone();
    v_flex()
        .flex_1()
        .min_h_0()
        .child(
            h_flex()
                .h(px(30.))
                .px(px(12.))
                .justify_between()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child("文件目录")
                .child(
                    div()
                        .id("refresh-tree")
                        .cursor_pointer()
                        .child(Icon::RefreshCw.el(px(12.), theme.faint))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // 目录没有 watch，改动后手动刷一下：把已展开的目录全部重读。
                            let dirs: Vec<PathBuf> =
                                this.state.expanded_dirs.iter().cloned().collect();
                            for dir in dirs {
                                this.state.core.list_dir(dir);
                            }
                            if let Some(root) = refresh_dir.clone() {
                                this.state.core.list_dir(root);
                            }
                            cx.notify();
                        })),
                ),
        )
        .child(match workdir {
            Some(dir) => tree(app, cx, dir).into_any_element(),
            None => div()
                .p(px(14.))
                .text_size(px(12.))
                .text_color(theme.muted)
                .child("这个对话还没绑定文件夹")
                .into_any_element(),
        })
}

/// 待办面板。数据来自 `TodoListUpdated` 事件——TodoWrite 工具每改一次就推一次，
/// 不额外落盘，所以切会话要清空（见 state）。
fn todo_panel(app: &HebbianApp) -> impl IntoElement {
    let theme = app.theme.clone();
    if app.state.todos.is_empty() {
        return div()
            .p(px(14.))
            .text_size(px(12.))
            .text_color(theme.muted)
            .child("这轮还没有待办")
            .into_any_element();
    }

    let mut list = v_flex().px(px(12.)).id("todo-list").overflow_y_scroll();
    for todo in &app.state.todos {
        // 线 DTO 里 status 是字符串（`pending` / `in_progress` / `completed`）。
        let done = todo.status == "completed";
        let running = todo.status == "in_progress";
        // 进行中的项显示进行时文案（"Running tests"），与 TodoWrite 协议一致。
        let text = if running && !todo.active_form.is_empty() {
            todo.active_form.clone()
        } else {
            todo.content.clone()
        };
        list = list.child(
            h_flex()
                .py(px(7.))
                .gap(px(8.))
                .items_center()
                .border_b_1()
                .border_color(theme.line)
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(
                    div()
                        .size(px(7.))
                        .flex_none()
                        .rounded_full()
                        .bg(if done {
                            theme.green
                        } else if running {
                            theme.accent
                        } else {
                            theme.faint
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(text),
                ),
        );
    }
    list.into_any_element()
}

/// 还没搬过来的面板：给出这块将来放什么，而不是一片空白。
fn empty_panel(app: &HebbianApp, item: Workbench) -> impl IntoElement {
    let theme = app.theme.clone();
    v_flex()
        .p(px(14.))
        .gap(px(6.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.text)
                .child(item.title()),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.muted)
                .child(item.blurb()),
        )
}

/// 文件树。按需读盘：只有展开过的目录才会被读，读到的结果缓存在 state 里。
fn tree(app: &HebbianApp, cx: &mut Context<HebbianApp>, dir: PathBuf) -> impl IntoElement {
    let theme = app.theme.clone();
    let leaf = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| dir.to_string_lossy().to_string());

    let mut list = v_flex()
        .id("file-tree")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(8.))
        .pb(px(8.))
        // 根节点：始终展开，代表这个对话的工作目录。
        .child(
            h_flex()
                .h(px(24.))
                .gap(px(4.))
                .text_size(px(12.))
                .text_color(theme.text)
                .child(Icon::ChevronDown.el(px(12.), theme.faint))
                .child(Icon::Folder.el(px(13.), theme.muted))
                .child(leaf),
        );

    for row in flatten(app, &dir, 1) {
        list = list.child(node_row(app, cx, row));
    }
    list
}

/// 展开状态下的一行。`depth` 决定缩进。
struct TreeRow {
    entry: DirEntry,
    depth: usize,
    expanded: bool,
}

/// 把「已展开的目录树」压平成一串可渲染的行。深度优先，与文件树的视觉顺序一致。
fn flatten(app: &HebbianApp, dir: &PathBuf, depth: usize) -> Vec<TreeRow> {
    let Some(entries) = app.state.dirs.get(dir) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries {
        let expanded = entry.is_dir && app.state.expanded_dirs.contains(&entry.path);
        let child_path = entry.path.clone();
        rows.push(TreeRow {
            entry: entry.clone(),
            depth,
            expanded,
        });
        if expanded {
            rows.extend(flatten(app, &child_path, depth + 1));
        }
    }
    rows
}

fn node_row(app: &HebbianApp, cx: &mut Context<HebbianApp>, row: TreeRow) -> impl IntoElement {
    let theme = app.theme.clone();
    let path = row.entry.path.clone();
    let is_dir = row.entry.is_dir;
    let indent = 12. + row.depth as f32 * 12.;

    h_flex()
        .id(gpui::SharedString::from(format!("node-{}", path.display())))
        .h(px(24.))
        .pl(px(indent))
        .pr(px(6.))
        .gap(px(4.))
        .rounded(px(6.))
        .text_size(px(12.))
        .text_color(theme.text)
        .cursor_pointer()
        .hover(|this| this.bg(theme.accent_soft))
        .child(if is_dir {
            if row.expanded {
                Icon::ChevronDown.el(px(12.), theme.faint)
            } else {
                Icon::ChevronRight.el(px(12.), theme.faint)
            }
        } else {
            // 文件没有折叠箭头，但要占同样的位置，否则同层的名字对不齐。
            Icon::ChevronRight.el(px(12.), gpui::transparent_black())
        })
        .child(if is_dir {
            Icon::Folder.el(px(13.), theme.muted)
        } else {
            Icon::File.el(px(13.), theme.faint)
        })
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(row.entry.name.clone()),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if !is_dir {
                this.state.core.read_file(path.clone());
                cx.notify();
                return;
            }
            if this.state.expanded_dirs.remove(&path) {
                cx.notify();
                return;
            }
            this.state.expanded_dirs.insert(path.clone());
            // 没读过才读；已缓存的直接展开，避免每次点都打一次盘。
            if !this.state.dirs.contains_key(&path) {
                this.state.core.list_dir(path.clone());
            }
            cx.notify();
        }))
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
        let active = app.workbench == item && !app.right_collapsed;
        rail = rail.child(
            div()
                .id(item.title())
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.))
                .cursor_pointer()
                .when(active, |this| this.bg(theme.accent_soft))
                .hover(|this| this.bg(theme.accent_soft))
                .child(item.icon().el(
                    px(15.),
                    if active { theme.accent } else { theme.muted },
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    // 点已选中的那个 = 收起面板，与原 UI 的开合手感一致。
                    if this.workbench == item && !this.right_collapsed {
                        this.right_collapsed = true;
                    } else {
                        this.workbench = item;
                        this.right_collapsed = false;
                    }
                    cx.notify();
                })),
        );
    }
    rail
}
