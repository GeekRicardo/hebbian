//! 右侧工作台：文件目录面板 + 最右侧的图标竖条。
//!
//! 对应 `RightSidebar.tsx` / `FileTreePanel.tsx`。图标条上每个按钮切换一个工作台
//! 面板（文件 / 编辑 / 目标 / Git / 待办 / 计划 / 对话 / 浏览器 / 终端）。

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, Window};

use crate::assets::Icon;
use crate::core::DirEntry;
use crate::ui::widgets::{h_flex, v_flex, NoDragPreview, RightDivider};
use crate::ui::HebbianApp;

/// 右侧工作台的默认 / 最小 / 最大宽度，与原前端 RightSidebar 的 props 一致。
pub const DEFAULT_WIDTH: f32 = 260.0;
pub const MIN_WIDTH: f32 = 200.0;
pub const MAX_WIDTH: f32 = 960.0;

/// 工作台可选的面板。顺序与截图里的图标条自上而下一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workbench {
    Files,
    Tasks,
    Edits,
    Git,
    Todos,
    Plans,
    Branches,
    Browser,
    Terminal,
}

impl Workbench {
    const ALL: [Workbench; 9] = [
        Workbench::Files,
        Workbench::Tasks,
        Workbench::Edits,
        Workbench::Git,
        Workbench::Todos,
        Workbench::Plans,
        Workbench::Branches,
        Workbench::Browser,
        Workbench::Terminal,
    ];

    fn icon(self) -> Icon {
        match self {
            Workbench::Files => Icon::FileText,
            Workbench::Tasks => Icon::LoaderCircle,
            Workbench::Edits => Icon::Pencil,
            Workbench::Git => Icon::GitBranch,
            Workbench::Todos => Icon::ListTodo,
            Workbench::Plans => Icon::List,
            Workbench::Branches => Icon::MessageSquare,
            Workbench::Browser => Icon::Globe,
            Workbench::Terminal => Icon::Terminal,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Workbench::Files => "文件目录",
            Workbench::Tasks => "后台任务",
            Workbench::Edits => "修改文件",
            Workbench::Git => "源代码管理",
            Workbench::Todos => "任务清单",
            Workbench::Plans => "计划",
            Workbench::Branches => "旁支对话",
            Workbench::Browser => "浏览器",
            Workbench::Terminal => "终端",
        }
    }
}

pub fn render(app: &mut HebbianApp, window: &mut Window, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    h_flex()
        .flex_none()
        .h_full()
        .items_start()
        .border_l_1()
        .border_color(theme.line)
        .when(!app.right_collapsed, |this| this.child(panel(app, window, cx)))
        .child(rail(app, cx))
}

/// 文件目录面板本体。
fn panel(
    app: &HebbianApp,
    window: &mut Window,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let workdir = app.state.current.as_ref().and_then(|s| s.workdir.clone());

    v_flex()
        .relative()
        .w(px(app.right_width))
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
        .child(
            div()
                .id("right-resize")
                .absolute()
                .left(px(0.))
                .top(px(0.))
                .w(px(4.))
                .h_full()
                .cursor_col_resize()
                .hover(|this| this.bg(theme.accent_soft))
                .on_drag(RightDivider, |_, _, _, cx| cx.new(|_| NoDragPreview))
                .on_drag_move(cx.listener(
                    |this, e: &gpui::DragMoveEvent<RightDivider>, _, cx| {
                        let delta = e.bounds.origin.x - e.event.position.x;
                        let next =
                            (this.right_width + f32::from(delta)).clamp(MIN_WIDTH, MAX_WIDTH);
                        if (next - this.right_width).abs() > 0.5 {
                            this.right_width = next;
                            cx.notify();
                        }
                    },
                )),
        )
        .child(match app.workbench {
            Workbench::Files => file_panel(app, cx, workdir).into_any_element(),
            Workbench::Todos => todo_panel(app).into_any_element(),
            Workbench::Git => git_panel(app, cx).into_any_element(),
            Workbench::Edits => edits_panel(app, cx).into_any_element(),
            Workbench::Tasks => tasks_panel(app, cx).into_any_element(),
            Workbench::Branches => branches_panel(app, cx).into_any_element(),
            Workbench::Plans => plan_panel(app, window, cx).into_any_element(),
            Workbench::Terminal => terminal_panel(app, cx).into_any_element(),
            Workbench::Browser => crate::ui::browser::panel(app, cx).into_any_element(),
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

/// Git 面板：分支 + 改动文件清单。状态字符按 porcelain 原样显示，
/// 未跟踪用问号、已暂存走强调色，与命令行里看到的对得上。
fn git_panel(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(git) = app.state.git.as_ref() else {
        return div()
            .p(px(14.))
            .text_size(px(12.))
            .text_color(theme.muted)
            .child("这个目录不是 git 仓库")
            .into_any_element();
    };

    // 文件列表按内容高度（封顶 200px），剩下的全给 diff——
    // 两边都 flex_1 时会按内容比例分，diff 长文本反而被挤没。
    let mut list = v_flex()
        .id("git-list")
        .flex_none()
        .max_h(px(200.))
        .overflow_y_scroll()
        .px(px(12.))
        .child(
            h_flex()
                .py(px(8.))
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(theme.text)
                .child(Icon::GitBranch.el(px(13.), theme.accent))
                .child(git.branch.clone())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child(format!("{} 处改动", git.files.len())),
                ),
        );

    if git.files.is_empty() {
        return list
            .child(
                div()
                    .py(px(8.))
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .child("工作区是干净的"),
            )
            .into_any_element();
    }

    let root = std::path::PathBuf::from(&git.root);
    for file in &git.files {
        let mark = if file.untracked {
            "?".to_string()
        } else if file.staged {
            file.x.clone()
        } else {
            file.y.clone()
        };
        let rel = file.path.clone();
        let staged = file.staged;
        let root = root.clone();
        let selected = app
            .state
            .diff
            .as_ref()
            .is_some_and(|(p, _)| p == &file.path);
        list = list.child(
            h_flex()
                .id(gpui::SharedString::from(format!("git-{}", file.path)))
                .py(px(5.))
                .px(px(4.))
                .rounded(px(6.))
                .gap(px(8.))
                .text_size(px(12.))
                .cursor_pointer()
                .when(selected, |this| this.bg(theme.accent_soft))
                .hover(|this| this.bg(theme.accent_soft))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.core.load_diff(root.clone(), rel.clone(), staged);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(12.))
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(if file.untracked {
                            theme.faint
                        } else if file.staged {
                            theme.green
                        } else {
                            theme.amber
                        })
                        .child(mark),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(theme.muted)
                        .child(file.path.clone()),
                ),
        );
    }
    v_flex()
        .flex_1()
        .min_h_0()
        .child(list)
        .children(diff_view(app))
        .into_any_element()
}

/// 逐行 diff。没变的大段折叠成「省略 N 行」，只留改动附近三行上下文。
fn diff_view(app: &HebbianApp) -> Option<impl IntoElement> {
    let (path, lines) = app.state.diff.as_ref()?;
    let theme = app.theme.clone();
    let (added, removed) = crate::diff::stats(lines);

    let mut body = v_flex()
        .id("diff-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .font_family("monospace")
        .text_size(px(11.));

    for entry in crate::diff::collapse(lines, 3) {
        body = match entry {
            None => body.child(
                div()
                    .px(px(8.))
                    .py(px(2.))
                    .text_color(theme.faint)
                    .bg(theme.right_bg_top)
                    .child("⋯"),
            ),
            Some(line) => {
                use crate::diff::DiffKind;
                let (bg, fg, sign) = match line.kind {
                    DiffKind::Insert => (
                        crate::theme::with_alpha(theme.green, 0.12),
                        theme.text,
                        "+",
                    ),
                    DiffKind::Delete => (
                        crate::theme::with_alpha(theme.danger, 0.12),
                        theme.text,
                        "-",
                    ),
                    DiffKind::Equal => (gpui::transparent_black(), theme.muted, " "),
                };
                body.child(
                    h_flex()
                        .w_full()
                        .px(px(6.))
                        .bg(bg)
                        .text_color(fg)
                        .child(
                            div()
                                .w(px(30.))
                                .flex_none()
                                .text_color(theme.faint)
                                .child(
                                    line.new_no
                                        .or(line.old_no)
                                        .map(|n| n.to_string())
                                        .unwrap_or_default(),
                                ),
                        )
                        .child(div().w(px(10.)).flex_none().child(sign))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(line.text.clone()),
                        ),
                )
            }
        };
    }

    Some(
        v_flex()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(theme.line)
            .child(
                h_flex()
                    .h(px(28.))
                    .px(px(12.))
                    .gap(px(8.))
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .text_color(theme.text)
                            .child(path.clone()),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .flex_none()
                            .text_size(px(11.))
                            .child(div().text_color(theme.green).child(format!("+{added}")))
                            .child(div().text_color(theme.danger).child(format!("−{removed}"))),
                    ),
            )
            .child(body),
    )
}

/// 后台任务面板。与原前端同源：历史从 `session.messages` 派生（跑完的永久保留），
/// 再用本进程注册表 join 出「还在跑」的实时状态。
fn tasks_panel(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let tasks = crate::state::derive_background_tasks(&app.state.messages);
    let live = &app.state.live_tasks;
    if tasks.is_empty() && live.is_empty() {
        return div()
            .p(px(14.))
            .text_size(px(12.))
            .text_color(theme.muted)
            .child("这个对话没有后台任务")
            .into_any_element();
    }

    let mut list = v_flex()
        .id("task-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(12.))
        .py(px(6.));

    // 注册表里还活着的排在最前——它们是此刻正在发生的事。
    for t in live {
        let task_id = t.task_id.clone();
        list = list.child(
            v_flex()
                .py(px(6.))
                .gap(px(3.))
                .border_b_1()
                .border_color(theme.line)
                .child(
                    h_flex()
                        .gap(px(6.))
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child(Icon::Terminal.el(px(11.), theme.faint))
                        // 任务编号要露出来：模型自己用它读输出 / 停任务，
                        // 用户在聊天区看到「已在后台启动 bash_3」时得对得上号。
                        .child(
                            div()
                                .font_family("monospace")
                                .child(t.task_id.clone()),
                        )
                        .child(
                            div()
                                .text_color(if t.running { theme.accent } else { theme.green })
                                .child(if t.running {
                                    "运行中".to_string()
                                } else {
                                    match t.exit_code {
                                        Some(0) | None => "已结束".to_string(),
                                        Some(code) => format!("退出码 {code}"),
                                    }
                                }),
                        )
                        .child(div().flex_1())
                        // 只有还在跑的才给「停止」——已经结束的按了也没意义。
                        .when(t.running, |this| {
                            this.child(
                                div()
                                    .id(gpui::SharedString::from(format!("kill-{task_id}")))
                                    .px(px(6.))
                                    .rounded(px(999.))
                                    .border_1()
                                    .border_color(theme.line)
                                    .text_color(theme.muted)
                                    .cursor_pointer()
                                    .hover(|this| {
                                        this.bg(gpui::rgba(0xd35b5b1a)).text_color(theme.danger)
                                    })
                                    .child("停止")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let Some(sid) =
                                            this.state.current_id().map(str::to_string)
                                        else {
                                            return;
                                        };
                                        this.state.core.kill_task(sid, task_id.clone());
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .child(
                    div()
                        .font_family("monospace")
                        .text_size(px(11.))
                        .text_color(theme.muted)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(t.command.clone()),
                ),
        );
    }

    for task in &tasks {
        use crate::state::BackgroundKind;
        let (icon, tag) = match task.kind {
            BackgroundKind::Bash => (Icon::Terminal, "命令"),
            BackgroundKind::Cron => (Icon::Clock, "定时"),
            BackgroundKind::Subagent => (Icon::Bot, "子任务"),
        };
        let color = if task.is_error {
            theme.danger
        } else if task.finished {
            theme.green
        } else {
            theme.accent
        };
        list = list.child(
            v_flex()
                .py(px(6.))
                .gap(px(3.))
                .border_b_1()
                .border_color(theme.line)
                .child(
                    h_flex()
                        .gap(px(6.))
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child(icon.el(px(11.), theme.faint))
                        .child(tag)
                        .child(
                            div().text_color(color).child(if task.is_error {
                                "失败"
                            } else if task.finished {
                                "已完成"
                            } else {
                                "运行中"
                            }),
                        )
                        .children(task.duration_ms.map(|ms| {
                            div()
                                .text_color(theme.faint)
                                .child(format!("{:.1}s", ms as f64 / 1000.))
                        })),
                )
                .child(
                    div()
                        .font_family("monospace")
                        .text_size(px(11.))
                        .text_color(theme.muted)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(task.label.clone()),
                ),
        );
    }
    list.into_any_element()
}

/// 修改文件面板：这个会话每个 run 改了哪些文件（edits-worktree 的记录）。
fn edits_panel(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    if app.state.edits.is_empty() {
        return div()
            .p(px(14.))
            .text_size(px(12.))
            .text_color(theme.muted)
            .child("这个对话还没改过文件")
            .into_any_element();
    }

    let mut list = v_flex()
        .id("edits-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(12.))
        .py(px(8.))
        .gap(px(10.));

    for run in &app.state.edits {
        let stamp = chrono::DateTime::from_timestamp_millis(run.started_at_ms)
            .map(|dt| dt.format("%m/%d %H:%M").to_string())
            .unwrap_or_default();
        let mut group = v_flex().gap(px(3.)).child(
            h_flex()
                .gap(px(6.))
                .text_size(px(11.))
                .text_color(theme.faint)
                .child(stamp)
                .child(format!("{} 个文件", run.files.len()))
                // 已回退的整组标出来，否则看不出这轮改动其实已经撤销了。
                .when(run.reverted, |this| {
                    this.child(
                        div()
                            .px(px(6.))
                            .rounded(px(999.))
                            .bg(theme.line)
                            .text_color(theme.muted)
                            .child("已回退"),
                    )
                })
                // 没回退过的才给回退入口——回退会真的改工作区里的文件。
                .when(!run.reverted, |this| {
                    let run_id = run.run_id.clone();
                    this.child(
                        div()
                            .id(gpui::SharedString::from(format!("revert-{}", run.run_id)))
                            .px(px(6.))
                            .rounded(px(999.))
                            .border_1()
                            .border_color(theme.line)
                            .text_color(theme.muted)
                            .cursor_pointer()
                            .hover(|this| {
                                this.bg(gpui::rgba(0xd35b5b1a)).text_color(theme.danger)
                            })
                            .child("回退这轮")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let Some(session) = this.state.current.as_ref() else {
                                    return;
                                };
                                let (sid, workdir) =
                                    (session.id.clone(), session.workdir.clone());
                                this.state.core.revert_run(sid, workdir, run_id.clone());
                                cx.notify();
                            })),
                    )
                }),
        );
        for file in &run.files {
            // 用改动前后字节数给一个直观的增减指示。
            let delta = file.after_bytes as i64 - file.before_bytes as i64;
            let (sign, color) = if delta > 0 {
                ("+", theme.green)
            } else if delta < 0 {
                ("−", theme.danger)
            } else {
                ("", theme.muted)
            };
            // 面板只有二百来像素宽，绝对路径铺进去只剩一串目录名，
            // 真正想看的文件名反而被截没了——所以只显示末段，全路径挂 tooltip。
            let leaf = file
                .real_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&file.real_path)
                .to_string();
            let full_path = file.real_path.clone();
            let run_id = run.run_id.clone();
            let selected = app
                .state
                .diff
                .as_ref()
                .is_some_and(|(p, _)| p == &file.real_path);
            group = group.child(
                h_flex()
                    .id(gpui::SharedString::from(format!(
                        "edit-{}-{}",
                        run.run_id, file.real_path
                    )))
                    .gap(px(8.))
                    .py(px(3.))
                    .px(px(4.))
                    .rounded(px(6.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(theme.accent_soft))
                    .hover(|this| this.bg(theme.accent_soft))
                    .tooltip({
                        let path = full_path.clone();
                        move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(path.clone()).build(window, cx)
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(session) = this.state.current.as_ref() else {
                            return;
                        };
                        let (sid, workdir) = (session.id.clone(), session.workdir.clone());
                        this.state.core.load_edit_diff(
                            sid,
                            workdir,
                            run_id.clone(),
                            full_path.clone(),
                        );
                        cx.notify();
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family("monospace")
                            .text_color(if run.reverted { theme.faint } else { theme.muted })
                            .child(leaf),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(color)
                            .child(if delta == 0 {
                                String::new()
                            } else {
                                format!("{sign}{} B", delta.abs())
                            }),
                    ),
            );
        }
        list = list.child(group);
    }
    list.into_any_element()
}

/// 旁支对话面板：从这个对话分叉出去的会话，点了直接切过去。
fn branches_panel(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    if app.state.branches.is_empty() {
        return v_flex()
            .p(px(14.))
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .child("还没有旁支对话"),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.faint)
                    .child("在某条消息上点「分叉」，就会从那里岔出一个新对话。"),
            )
            .into_any_element();
    }

    let mut list = v_flex()
        .id("branch-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(8.))
        .py(px(6.));

    for (id, title) in &app.state.branches {
        let open_id = id.clone();
        list = list.child(
            h_flex()
                .id(gpui::SharedString::from(format!("branch-{id}")))
                .py(px(6.))
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(8.))
                .text_size(px(12.))
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent_soft))
                .child(Icon::GitBranch.el(px(12.), theme.faint))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(title.clone()),
                )
                .on_click(cx.listener(move |this, _, _, _| {
                    this.state.core.open_session(open_id.clone());
                })),
        );
    }
    list.into_any_element()
}

/// 计划面板：PlanMode 落盘的 plan markdown，新的在前。
fn plan_panel(
    app: &HebbianApp,
    window: &mut Window,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    if app.state.plans.is_empty() {
        return div()
            .p(px(14.))
            .text_size(px(12.))
            .text_color(theme.muted)
            .child("这个对话还没有计划")
            .into_any_element();
    }

    let mut list = v_flex()
        .id("plan-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(12.))
        .py(px(8.))
        .gap(px(12.));

    for (name, body) in &app.state.plans {
        list = list.child(
            v_flex()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child(name.clone()),
                )
                .child(
                    div().text_size(px(12.)).child(
                        gpui_component::text::TextView::markdown(
                            gpui::SharedString::from(format!("plan-{name}")),
                            body.clone(),
                            window,
                            cx,
                        ),
                    ),
                ),
        );
    }
    list.into_any_element()
}

/// 终端面板。ANSI 与网格由 alacritty_terminal 处理，这里只画网格 + 转发按键。
fn terminal_panel(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some(session) = app.terminal.as_ref() else {
        return div()
            .id("terminal-start")
            .p(px(14.))
            .text_size(px(12.))
            .text_color(theme.muted)
            .cursor_pointer()
            .child("点这里在当前对话的工作目录起一个终端")
            .on_click(cx.listener(|this, _, _, cx| this.ensure_terminal(cx)))
            .into_any_element();
    };

    // 按面板实际宽度换算列数：等宽 11px 字体的字符宽约 6.6px，
    // 左右各留 8px 内边距。行数按窗口高度估，够用即可。
    let cols = (((app.right_width - 16.) / 6.6) as u16).max(20);
    let rows = 30u16;
    session.resize(cols, rows);

    let mut screen = v_flex()
        .id("terminal-screen")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p(px(8.))
        .font_family("monospace")
        .text_size(px(11.))
        .line_height(px(16.));

    for line in session.visible_lines() {
        let mut row = h_flex().h(px(16.));
        for span in line {
            let color = span
                .fg
                .map(|(r, g, b)| gpui::rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into())
                .unwrap_or(theme.text);
            row = row.child(
                div()
                    .text_color(color)
                    .when(span.bold, |this| {
                        this.font_weight(gpui::FontWeight(700.))
                    })
                    .child(span.text),
            );
        }
        screen = screen.child(row);
    }

    v_flex()
        .flex_1()
        .min_h_0()
        .track_focus(&app.terminal_focus)
        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
            let Some(term) = this.terminal.clone() else {
                return;
            };
            // 把按键翻成 PTY 字节。控制键单独处理，其余取 key_char。
            let keystroke = &event.keystroke;
            // Ctrl+Shift+C / V 走剪贴板，不当普通控制键发下去。
            if keystroke.modifiers.control && keystroke.modifiers.shift {
                match keystroke.key.as_str() {
                    "c" => {
                        if let Some(text) = term.selection_text() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        }
                        return;
                    }
                    "v" => {
                        if let Some(text) =
                            cx.read_from_clipboard().and_then(|item| item.text())
                        {
                            term.write(text.into_bytes());
                            cx.notify();
                        }
                        return;
                    }
                    _ => {}
                }
            }
            let bytes: Vec<u8> = match keystroke.key.as_str() {
                "enter" => vec![b'\r'],
                "backspace" => vec![0x7f],
                "tab" => vec![b'\t'],
                "escape" => vec![0x1b],
                "up" => b"\x1b[A".to_vec(),
                "down" => b"\x1b[B".to_vec(),
                "right" => b"\x1b[C".to_vec(),
                "left" => b"\x1b[D".to_vec(),
                key if keystroke.modifiers.control && key.len() == 1 => {
                    // Ctrl-A..Ctrl-Z → 0x01..0x1a，Ctrl-C 中断就靠这条。
                    let c = key.as_bytes()[0].to_ascii_lowercase();
                    if c.is_ascii_lowercase() {
                        vec![c - b'a' + 1]
                    } else {
                        Vec::new()
                    }
                }
                _ => keystroke
                    .key_char
                    .as_ref()
                    .map(|s| s.as_bytes().to_vec())
                    .unwrap_or_default(),
            };
            if !bytes.is_empty() {
                term.write(bytes);
                cx.notify();
            }
        }))
        .child(screen)
        .child(
            h_flex()
                .h(px(22.))
                .flex_none()
                .px(px(8.))
                .text_size(px(10.))
                .text_color(theme.faint)
                .child(if session.has_exited() {
                    "shell 已退出".to_string()
                } else {
                    format!(
                        "{}×{} · 点一下再打字 · Ctrl+Shift+C/V 复制粘贴",
                        session.cols(),
                        session.rows()
                    )
                }),
        )
        .into_any_element()
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
            .tooltip({
                let label = if app.right_collapsed { "展开工作台" } else { "折叠工作台" };
                move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                }
            })
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
                // 竖条收起时只剩一列图标，没有 tooltip 就只能靠猜哪个是哪个。
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(item.title()).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    // 点已选中的那个 = 收起面板，与原 UI 的开合手感一致。
                    if this.workbench == item && !this.right_collapsed {
                        this.right_collapsed = true;
                    } else {
                        this.workbench = item;
                        this.right_collapsed = false;
                        if item == Workbench::Terminal {
                            this.ensure_terminal(cx);
                        }
                        // 活任务只在切进来时读一次——注册表是进程内内存，
                        // 每帧都读会在没有后台任务时白白抢锁。
                        if item == Workbench::Tasks {
                            if let Some(id) = this.state.current_id().map(str::to_string) {
                                this.state.core.refresh_live_tasks(id);
                            }
                        }
                    }
                    cx.notify();
                })),
        );
    }
    rail
}
