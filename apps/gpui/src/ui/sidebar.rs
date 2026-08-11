//! 左侧栏。对应原前端 `DesktopSidebar.tsx` + `desktopShell.css` 的 `.dsp-sidebar*` 一族。
//!
//! 尺寸全部照抄 CSS 最终层叠结果：栏宽 252px、卡片圆角 22px、tabs 高 34px、
//! 项目标题行高 34px、会话行 24px。改任何一个数之前先去 CSS 里找对应规则。

use gpui::{div, prelude::*, px, Context, Window};

use crate::assets::Icon;
use crate::state::{build_buckets, relative_time, ProjectBucket, SidebarTab};
use crate::ui::hue;
use crate::ui::widgets::{h_flex, now_ms, v_flex};
use crate::ui::HebbianApp;

/// `.dsp-shell` 第三遍覆写里的 `grid-template-columns: 252px ...`。
pub const SIDEBAR_WIDTH: f32 = 252.0;

pub fn render(app: &mut HebbianApp, _window: &mut Window, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    v_flex()
        .w(px(SIDEBAR_WIDTH))
        .flex_none()
        .h_full()
        .px(px(12.))
        .pb(px(12.))
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.line)
        // `.dsp-window-space`：给 macOS 交通灯让出的 22px 空档。
        .child(div().h(px(22.)).flex_none())
        .child(card(app, cx))
}

/// `.dsp-sidebar-card`：浮起来的圆角卡片，装下 tabs / 工具区 / 列表 / footer。
fn card(app: &mut HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    v_flex()
        .mt(px(5.))
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .rounded(px(22.))
        .border_1()
        .border_color(theme.card_line)
        .bg(theme.surface_veil)
        .child(tabs(app, cx))
        .when(app.state.tab == SidebarTab::Chat, |this| {
            this.child(new_chat_button(app, cx))
        })
        .child(toolbar(app, cx))
        .child(project_groups(app, cx))
        .child(footer(app, cx))
}

/// `.dsp-sidebar-tabs`：code / chat 两个胶囊。
fn tabs(app: &mut HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let current = app.state.tab;

    let tab_button = |tab: SidebarTab, icon: Icon, label: &'static str| {
        let active = current == tab;
        let theme = theme.clone();
        h_flex()
            .id(label)
            .flex_1()
            .h_full()
            .justify_center()
            .gap(px(8.))
            .rounded(px(6.))
            .text_size(px(12.))
            .cursor_pointer()
            .when(active, |this| {
                this.bg(theme.tab_active_bg).text_color(theme.tab_active_text)
            })
            .when(!active, |this| this.text_color(theme.tab_text))
            .child(
                // 选中态图标走强调色，未选中跟随文字色（CSS: `.is-active svg { color: accent }`）。
                icon.el(px(14.), if active { theme.accent } else { theme.tab_text }),
            )
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.tab = tab;
                cx.notify();
            }))
    };

    h_flex()
        .mx(px(8.))
        .mt(px(8.))
        .h(px(34.))
        .flex_none()
        .p(px(4.))
        .gap(px(4.))
        .rounded(px(7.))
        .border_1()
        .border_color(theme.card_line)
        .bg(theme.tabs_bg)
        .child(tab_button(SidebarTab::Code, Icon::Code2, "code"))
        .child(tab_button(SidebarTab::Chat, Icon::Edit3, "chat"))
}

/// `.dsp-sidebar-new-chat`：仅 chat 标签页出现。
fn new_chat_button(app: &mut HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    h_flex()
        .id("new-chat")
        .mx(px(8.))
        .mt(px(10.))
        .mb(px(20.))
        .h(px(34.))
        .flex_none()
        .justify_center()
        .gap(px(7.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme.new_chat_line)
        .bg(theme.new_chat_bg_b)
        .text_color(theme.new_chat_text)
        .text_size(px(12.))
        .cursor_pointer()
        .hover(|this| this.bg(theme.new_chat_bg_a))
        .child(Icon::MessageSquarePlus.el(px(14.), theme.new_chat_text))
        .child("新建对话")
        .on_click(cx.listener(|this, _, _, _| {
            this.state.core.create_session(None, None);
        }))
}

/// `.dsp-project-toolbar`：标题 +（code 页）三个纵向入口 + 搜索框 + 下边线。
fn toolbar(app: &mut HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let is_code = app.state.tab == SidebarTab::Code;

    v_flex()
        .flex_none()
        .mx(px(10.))
        .mb(px(10.))
        .pb(px(10.))
        .gap(px(8.))
        .border_b_1()
        .border_color(theme.line)
        .child(
            div()
                .min_h(px(24.))
                .text_size(px(14.))
                .font_weight(gpui::FontWeight(720.))
                .text_color(theme.text)
                .child(if is_code { "项目" } else { "对话" }),
        )
        .when(is_code, |this| {
            this.child(
                v_flex()
                    .gap(px(4.))
                    .child(toolbar_action(
                        app,
                        cx,
                        Icon::Plus,
                        "新建项目",
                        ToolbarAction::NewProject,
                    ))
                    .child(toolbar_action(
                        app,
                        cx,
                        Icon::Import,
                        "导入项目",
                        ToolbarAction::ImportProject,
                    ))
                    .child(toolbar_action(
                        app,
                        cx,
                        Icon::FolderOpen,
                        "导入 VS Code",
                        ToolbarAction::ImportVscode,
                    )),
            )
        })
        .child(search_box(app, cx))
}

/// `.dsp-project-actions button`：h 26 / 圆角 8 / 图标 13。
/// 项目工具区的三个入口。
#[derive(Debug, Clone, Copy)]
enum ToolbarAction {
    NewProject,
    ImportProject,
    ImportVscode,
}

fn toolbar_action(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    icon: Icon,
    label: &'static str,
    action: ToolbarAction,
) -> impl IntoElement {
    let theme = app.theme.clone();
    h_flex()
        .id(label)
        .w_full()
        .h(px(26.))
        .px(px(8.))
        .gap(px(7.))
        .rounded(px(8.))
        .text_size(px(12.))
        .text_color(theme.muted)
        .cursor_pointer()
        .hover(|this| this.bg(theme.accent_soft).text_color(theme.text))
        .child(icon.el(px(13.), theme.muted))
        .child(label)
        .on_click(cx.listener(move |this, _, _, cx| match action {
            ToolbarAction::NewProject => this.pick_project_dir(cx),
            // 导入项目 / 导入 VS Code 读的是 .code-workspace / json 文件，
            // 解析规则在 core 里还没暴露成 CoreRequest，先明确说明而不是假装能点。
            ToolbarAction::ImportProject | ToolbarAction::ImportVscode => {
                this.state.error =
                    Some(format!("「{label}」还没接上，先用原来的桌面端导入，两边读同一份配置"));
                cx.notify();
            }
        }))
}

/// `.dsp-project-search`：一个 28px 高的搜索输入。
fn search_box(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    h_flex()
        .w_full()
        .h(px(28.))
        .px(px(8.))
        .gap(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme.line)
        .bg(theme.surface_veil)
        .text_color(theme.muted)
        .child(Icon::Search.el(px(13.), theme.muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.))
                .text_color(theme.input_text)
                .child(gpui_component::input::Input::new(&app.search).appearance(false)),
        )
        .child(search_toggle(app, cx, "Aa", app.state.search_case, true))
        .child(search_toggle(app, cx, ".*", app.state.search_regex, false))
}

/// 搜索框右侧的两个小开关：区分大小写 / 正则。与原前端 `.dsp-search-options` 一致。
fn search_toggle(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    label: &'static str,
    active: bool,
    is_case: bool,
) -> impl IntoElement {
    let theme = app.theme.clone();
    div()
        .id(label)
        .h(px(22.))
        .min_w(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .text_size(px(10.))
        .cursor_pointer()
        .when(active, |this| this.bg(theme.accent_soft).text_color(theme.accent))
        .when(!active, |this| this.text_color(theme.faint))
        .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
        .child(label)
        .on_click(cx.listener(move |this, _, _, cx| {
            if is_case {
                this.state.search_case = !this.state.search_case;
            } else {
                this.state.search_regex = !this.state.search_regex;
            }
            cx.notify();
        }))
}

/// `.dsp-project-groups`：可滚动的项目 / 会话列表。
fn project_groups(app: &mut HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let buckets = build_buckets(
        &app.state.projects,
        &app.state.sessions,
        &app.state.query,
        app.state.search_case,
        app.state.search_regex,
    );
    let buckets: Vec<ProjectBucket> = match app.state.tab {
        // chat 页只看没有项目归属的对话；code 页反之。
        SidebarTab::Chat => buckets.into_iter().filter(|b| b.project_id.is_none()).collect(),
        SidebarTab::Code => buckets.into_iter().filter(|b| b.project_id.is_some()).collect(),
    };

    let mut list = v_flex()
        .id("project-groups")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(6.))
        .pb(px(12.));

    if app.state.tab == SidebarTab::Chat {
        for bucket in &buckets {
            for session in &bucket.sessions {
                list = list.child(session_row(app, cx, session));
            }
        }
        return list;
    }

    for bucket in &buckets {
        list = list.child(project_group(app, cx, bucket));
    }
    list
}

fn project_group(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    bucket: &ProjectBucket,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let collapsed = app.state.collapsed.contains(&bucket.id);
    let bucket_id = bucket.id.clone();
    let project_id = bucket.project_id.clone();
    let group_name = gpui::SharedString::from(format!("proj-row-{}", bucket.id));
    let count = bucket.sessions.len();

    let mut group = v_flex().flex_none().mb(px(4.)).child(
        h_flex()
            .id(gpui::SharedString::from(format!("proj-{}", bucket.id)))
            .group(group_name.clone())
            .relative()
            .w_full()
            .min_h(px(34.))
            .pl(px(16.))
            .pr(px(58.))
            .gap(px(5.))
            .items_center()
            .rounded(px(9.))
            .cursor_pointer()
            .hover(|this| this.bg(theme.surface_veil))
            .child(
                (if collapsed { Icon::Folder } else { Icon::FolderOpen })
                    .el(px(15.), theme.muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight(500.))
                    .child(bucket.name.clone()),
            )
            .child(
                div()
                    .min_w(px(22.))
                    .text_center()
                    .text_size(px(11.))
                    .text_color(theme.faint)
                    .child(count.to_string()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.state.collapsed.remove(&bucket_id) {
                    this.state.collapsed.insert(bucket_id.clone());
                }
                cx.notify();
            }))
            // 悬停时右侧浮出「在这个项目里新建对话」。
            .child(
                div()
                    .id("add")
                    .absolute()
                    .right(px(28.))
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_color(theme.faint)
                    .cursor_pointer()
                    // 常态隐形、hover 整行才显出（CSS `.dsp-project-add { opacity: 0 }`）。
                    .invisible()
                    .group_hover(group_name.clone(), |this| this.visible())
                    .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
                    .child(Icon::Plus.el(px(13.), theme.faint))
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.state.core.create_session(project_id.clone(), None);
                    })),
            ),
    );

    if !collapsed {
        let mut list = v_flex().pl(px(16.)).pr(px(4.)).pb(px(4.));
        for session in &bucket.sessions {
            list = list.child(session_row(app, cx, session));
        }
        group = group.child(list);
    }
    group
}

/// `.dsp-session-row`：24px 高的一行，hover 出时间与删除按钮。
fn session_row(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    session: &agent_core::storage::sessions::SessionMeta,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let id = session.id.clone();
    let active = app.state.current_id() == Some(session.id.as_str());
    let running = app.state.running.contains(&session.id);
    let unread = app.state.unread.contains(&session.id);
    let pending = app.state.pending_approvals.contains_key(&session.id)
        || app.state.pending_questions.contains_key(&session.id);
    let time = relative_time(session.updated_at, now_ms());
    let open_id = id.clone();
    let delete_id = id.clone();
    let group_name = gpui::SharedString::from(format!("sess-row-{id}"));

    h_flex()
        .id(gpui::SharedString::from(format!("sess-{id}")))
        .group(group_name.clone())
        .relative()
        .w_full()
        .min_h(px(24.))
        .pl(px(8.))
        .pr(px(30.))
        .py(px(4.))
        .items_center()
        .rounded(px(8.))
        .cursor_pointer()
        .when(active, |this| {
            this.bg(theme.session_active())
                .border_1()
                .border_color(theme.session_active_ring())
        })
        .when(!active, |this| {
            this.hover(|this| this.bg(theme.session_hover()))
        })
        // 状态点挂在行左外侧（CSS `left: -10px`）。
        .child(
            div()
                .absolute()
                .left(px(-10.))
                .size(px(7.))
                .rounded_full()
                .when(pending, |this| this.bg(theme.amber))
                .when(!pending && running, |this| this.bg(theme.accent))
                .when(!pending && !running && unread, |this| this.bg(theme.green)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight(500.))
                .text_color(theme.session_title)
                .child(session.title.clone()),
        )
        .child(
            div()
                .flex_none()
                .ml(px(6.))
                .text_size(px(10.))
                .text_color(theme.faint)
                // CSS `.dsp-session-time { display: none }` + hover 时 inline-block。
                .invisible()
                .group_hover(group_name.clone(), |this| this.visible())
                .child(time),
        )
        .on_click(cx.listener(move |this, _, _, _| {
            this.state.core.open_session(open_id.clone());
        }))
        .child(
            div()
                .id("del")
                .absolute()
                .right(px(4.))
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_color(theme.faint)
                .cursor_pointer()
                .invisible()
                .group_hover(group_name.clone(), |this| this.visible())
                .hover(|this| this.bg(gpui::rgba(0xd35b5b1a)).text_color(theme.danger))
                .child(Icon::Trash2.el(px(12.), theme.faint))
                .on_click(cx.listener(move |this, _, _, _| {
                    this.state.core.delete_session(delete_id.clone());
                })),
        )
}

/// `.dsp-sidebar-footer`：设置 + 调色盘。
fn footer(app: &mut HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    h_flex()
        .relative()
        .flex_none()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .pt(px(14.))
        .px(px(18.))
        .pb(px(18.))
        .border_t_1()
        .border_color(theme.card_line)
        .child(
            h_flex()
                .id("settings")
                .gap(px(8.))
                .text_size(px(12.))
                .text_color(theme.muted)
                .cursor_pointer()
                .hover(|this| this.text_color(theme.text))
                .child(Icon::Settings.el(px(14.), theme.muted))
                .child("设置")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_settings(window, cx);
                })),
        )
        .child(hue::control(app, cx))
        .children(hue::popover_for_footer(app, cx))
}
