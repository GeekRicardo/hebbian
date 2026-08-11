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

    /// 竖条上的图标。
    ///
    /// **这一列用的是 codicon（VS Code 那套），不是 lucide**——原前端这里写的就是
    /// `<Codicon name="files" />`。之前我按感觉挑了几个形近的 lucide 图标顶上，
    /// 把原版跑起来对着看才发现根本不是一套字形。
    fn icon(self) -> Icon {
        match self {
            Workbench::Files => Icon::CoFiles,
            Workbench::Tasks => Icon::CoServerProcess,
            Workbench::Edits => Icon::CoDiffModified,
            Workbench::Git => Icon::CoSourceControl,
            Workbench::Todos => Icon::CoChecklist,
            Workbench::Plans => Icon::CoListTree,
            Workbench::Branches => Icon::CoCommentDiscussion,
            Workbench::Browser => Icon::CoGlobe,
            Workbench::Terminal => Icon::CoTerminal,
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
/// 「后台任务」面板。一张卡片一条任务，点整行展开看详情。
///
/// 三种任务共用一张卡片模板，展开区各不相同：
/// - Bash：实时输出（跑完了就直接看工具结果），运行中还给一个「停止」
/// - 定时唤醒：原因 + 唤醒时刻 + 倒计时
/// - 子任务：子 agent 类型 + 一句「跑完会自动回到这个对话里」
fn tasks_panel(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let tasks = crate::state::derive_background_tasks(
        &app.state.messages,
        &app.state.live_tasks,
        &app.state.pending_crons,
    );
    if tasks.is_empty() {
        return div()
            .p(px(14.))
            .text_size(px(12.))
            .text_color(theme.muted)
            .child("这个对话没有后台任务")
            .into_any_element();
    }

    let now_ms = now_ms();
    let expanded_id = app.state.expanded_task.clone();

    let mut list = v_flex()
        .id("task-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(10.))
        .py(px(6.))
        .gap(px(6.));

    for task in &tasks {
        use crate::state::BackgroundKind;
        // 展开态按 tool_call_id 记，不用 task_id：定时唤醒压根没有 task_id。
        let key = task.tool_call_id.clone();
        let expanded = expanded_id.as_deref() == Some(key.as_str());
        // 原版只有定时唤醒和子任务在编号前放图标；普通命令那行是「状态点 + 编号」，
        // 没有图标。之前我给三种都加了图标，多出来一个终端小图标。
        let (icon, prefix) = match task.kind {
            BackgroundKind::Bash => (None, "$ "),
            BackgroundKind::Cron => (Some(Icon::Clock), "⏰ "),
            BackgroundKind::Subagent => (Some(Icon::Bot), "🤖 "),
        };
        let dot = if task.is_error {
            theme.danger
        } else if task.running {
            theme.amber
        } else {
            theme.green
        };

        // 顶行：状态点 + 编号/倒计时 + 耗时 + 状态徽章 + 展开箭头
        let mut head = h_flex()
            .gap(px(5.))
            .text_size(px(10.))
            .text_color(theme.faint)
            .child(div().size(px(6.)).flex_none().rounded_full().bg(dot))
            .children(icon.map(|i| i.el(px(11.), theme.faint)));
        head = match (&task.cron, &task.task_id) {
            (Some(cron), _) => head.child(if cron.pending {
                format!("{} 后唤醒", countdown(cron.fire_at_ms, now_ms))
            } else {
                format!("已于 {} 唤醒", clock(cron.fire_at_ms))
            }),
            (None, Some(id)) => head.child(div().font_family("monospace").child(id.clone())),
            (None, None) => head.child("待启动"),
        };
        if let Some(secs) = task.elapsed_secs {
            head = head.child(format!("{secs}s"));
        } else if let Some(ms) = task.duration_ms {
            head = head.child(format!("{}s", ms / 1000));
        }
        if !task.running {
            head = head.child(
                div()
                    .px(px(4.))
                    .rounded(px(4.))
                    .bg(theme.line)
                    .text_color(theme.muted)
                    // 徽章文字照搬原版：定时唤醒写「已唤醒」，其余直接是大写的
                    // 运行状态（EXITED / FAILED）。这与本仓库「UI 不出现内部枚举值」
                    // 的惯例相左，但用户要的是和原版一模一样，以原版为准。
                    .child(match task.kind {
                        BackgroundKind::Cron => "已唤醒",
                        _ if task.is_error => "FAILED",
                        _ => "EXITED",
                    }),
            );
        }
        head = head.child(div().flex_1()).child(
            if expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            }
            .el(px(11.), theme.faint),
        );

        let jump_to = task.tool_call_id.clone();
        let task_id = task.task_id.clone();
        let is_bash = task.kind == BackgroundKind::Bash;
        let mut card = v_flex()
            .id(gpui::SharedString::from(format!("task-{key}")))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.line)
            .when(task.running, |this| this.bg(gpui::rgba(0xf59e0b0d)))
            .child(
                v_flex()
                    .id(gpui::SharedString::from(format!("task-head-{key}")))
                    .px(px(10.))
                    .py(px(7.))
                    .gap(px(3.))
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.accent_soft))
                    .child(head)
                    // 命令原文按类型加前缀，一眼能分出是命令、定时还是子任务。
                    .child(
                        div()
                            .font_family("monospace")
                            .text_size(px(11.))
                            .text_color(theme.text)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!("{prefix}{}", task.command)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // 点整行 = 展开/收起 + 跳到聊天区对应的那张工具卡。
                        // 折叠时也跳：用户点它的意图就是「这次任务在对话里哪个位置」。
                        if expanded {
                            this.state.expanded_task = None;
                            this.state.task_output = None;
                            this.state.core.unwatch_task_output();
                        } else {
                            this.state.expanded_task = Some(key.clone());
                            this.state.task_output = None;
                            // 只有 Bash 有实时输出可拉；定时与子任务没有。
                            if is_bash {
                                if let (Some(sid), Some(tid)) = (
                                    this.state.current_id().map(str::to_string),
                                    task_id.clone(),
                                ) {
                                    this.state.core.watch_task_output(sid, tid);
                                }
                            }
                        }
                        this.state.focus_tool_call = Some(jump_to.clone());
                        cx.notify();
                    })),
            );

        if expanded {
            card = card.child(
                v_flex()
                    .px(px(10.))
                    .py(px(8.))
                    .gap(px(6.))
                    .border_t_1()
                    .border_color(theme.line)
                    .child(match task.kind {
                        BackgroundKind::Cron => cron_detail(&theme, task, now_ms).into_any_element(),
                        BackgroundKind::Subagent => {
                            subagent_detail(&theme, task).into_any_element()
                        }
                        BackgroundKind::Bash => bash_detail(app, &theme, task, cx).into_any_element(),
                    }),
            );
        }
        list = list.child(card);
    }
    list.into_any_element()
}

/// 定时唤醒的展开区：为什么要唤醒、什么时候唤醒。
fn cron_detail(
    theme: &crate::theme::Theme,
    task: &crate::state::BackgroundTask,
    now_ms: i64,
) -> impl IntoElement {
    let cron = task.cron.clone().unwrap_or_else(|| crate::state::CronInfo {
        reason: task.command.clone(),
        fire_at_ms: 0,
        pending: false,
    });
    v_flex()
        .gap(px(4.))
        .text_size(px(11.))
        .text_color(theme.text)
        .child(
            div()
                .child(labeled(theme, "原因", cron.reason.clone())),
        )
        .child(labeled(
            theme,
            "唤醒时刻",
            if cron.pending {
                format!(
                    "{}（{} 后）",
                    clock(cron.fire_at_ms),
                    countdown(cron.fire_at_ms, now_ms)
                )
            } else {
                clock(cron.fire_at_ms)
            },
        ))
}

/// 子任务的展开区：它是谁、跑完之后会发生什么。
fn subagent_detail(
    theme: &crate::theme::Theme,
    task: &crate::state::BackgroundTask,
) -> impl IntoElement {
    v_flex()
        .gap(px(4.))
        .text_size(px(11.))
        .child(labeled(theme, "子代理", task.command.clone()))
        .child(
            div().text_color(theme.muted).child(if task.running {
                "正在后台跑。跑完会自动唤醒这个对话，结果直接出现在对话里。"
            } else {
                "已经跑完，并且唤醒过这个对话了。"
            }),
        )
}

/// Bash 的展开区：运行中拉实时输出（带「停止」），跑完了直接看工具结果。
fn bash_detail(
    app: &HebbianApp,
    theme: &crate::theme::Theme,
    task: &crate::state::BackgroundTask,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    // 只认这个任务自己的输出：切着看另一个任务时，上一份还没被覆盖，
    // 不按编号对一下会把别人的输出显示在这张卡片里。
    let live = app
        .state
        .task_output
        .as_ref()
        .filter(|(id, _)| Some(id) == task.task_id.as_ref())
        .map(|(_, text)| text.clone())
        .unwrap_or_default();
    // 跑完的任务不要再显示轮询到的那份：进程一结束注册表就可能被清掉，
    // 那时轮询拿到的是空的，而工具结果是永久留在 transcript 里的。
    let body = if task.running {
        if live.trim().is_empty() {
            "等待输出…".to_string()
        } else {
            live
        }
    } else {
        task.result
            .clone()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| "(无输出)".to_string())
    };

    v_flex()
        .gap(px(6.))
        .when_some(
            task.task_id.clone().filter(|_| task.running),
            |this, task_id| {
                this.child(
                    h_flex().justify_end().child(
                        div()
                            .id(gpui::SharedString::from(format!("kill-{task_id}")))
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(6.))
                            .text_size(px(10.))
                            .text_color(theme.danger)
                            .cursor_pointer()
                            .hover(|this| this.bg(gpui::rgba(0xd35b5b1a)))
                            .child("停止")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(sid) = this.state.current_id().map(str::to_string) {
                                    this.state.core.kill_task(sid, task_id.clone());
                                }
                                cx.notify();
                            })),
                    ),
                )
            },
        )
        // 输出区用深色：终端输出本来就是深底浅字，放在浅色卡片里反而难读。
        .child(
            div()
                .id("task-output")
                .max_h(px(240.))
                .overflow_y_scroll()
                .p(px(8.))
                .rounded(px(6.))
                .bg(gpui::rgb(0x18181b))
                .font_family("monospace")
                .text_size(px(10.))
                .text_color(gpui::rgb(0xe4e4e7))
                .child(body),
        )
}

fn labeled(
    theme: &crate::theme::Theme,
    label: &'static str,
    value: String,
) -> impl IntoElement {
    h_flex()
        .gap(px(4.))
        .items_start()
        .child(div().flex_none().text_color(theme.muted).child(label))
        .child(div().min_w_0().text_color(theme.text).child(value))
}

/// `3h20m` 这样的倒计时。与原前端同一套写法：只显示有值的档位。
fn countdown(fire_at_ms: i64, now_ms: i64) -> String {
    let secs = ((fire_at_ms - now_ms) / 1000).max(0);
    let (days, hours, mins, s) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if mins > 0 {
        parts.push(format!("{mins}m"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{s}s"));
    }
    parts.join("")
}

fn clock(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| {
            chrono::DateTime::<chrono::Local>::from(dt)
                .format("%m/%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "时间未知".to_string())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
                .child(Icon::CoFolderOpened.el(px(15.), theme.muted))
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
        // 文件夹开合两态、文件按扩展名挑图标——与原前端同一套 codicon。
        .child(if is_dir {
            if row.expanded {
                Icon::CoFolderOpened.el(px(15.), theme.muted)
            } else {
                Icon::CoFolder.el(px(15.), theme.muted)
            }
        } else {
            crate::file_icon::file_icon(&row.entry.name).el(px(15.), theme.faint)
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
