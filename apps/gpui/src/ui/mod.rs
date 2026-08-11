//! 视图层。结构与原 Web 前端一一对应：
//! `DesktopShell` = 左侧栏 + 聊天列 + 右侧工作台，弹层挂在最外层。

mod browser;
mod chat;
mod editor;
mod hue;
mod right_panel;
mod session_settings;
pub mod settings;
mod sidebar;
mod widgets;

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, Window,
};
use gpui_component::input::{InputEvent, InputState};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::core::{Core, CoreUpdate};
use crate::state::AppState;
use crate::theme::{Theme, ThemePreset};

pub fn init(_cx: &mut App) {}

/// 根视图。整个应用只有这一个 `Render` 实体持有状态——子视图都是纯函数式的
/// `IntoElement`，直接读 `&AppState` 渲染。原前端也是「单 store + 无状态组件」的形态，
/// 保持一致能让两边对照着改。
pub struct HebbianApp {
    pub state: AppState,
    pub theme: Theme,
    pub preset: ThemePreset,
    pub hue: f32,

    /// 色系弹窗是否展开（footer 调色盘按钮）。
    pub hue_popover_open: bool,
    /// 右侧工作台是否折叠。
    pub right_collapsed: bool,
    /// 模型选择器是否展开，以及展开着哪个供应商。
    pub model_picker_open: bool,
    pub model_picker_provider: Option<String>,
    /// 内置浏览器预览的 webview 与地址栏输入。
    pub webview: Option<Entity<gpui_component::webview::WebView>>,
    pub url_input: Entity<InputState>,

    /// 内置终端会话。第一次打开终端面板时才起 shell。
    pub terminal: Option<std::rc::Rc<crate::terminal::TerminalSession>>,
    /// 终端的焦点句柄——键盘输入要转发进 PTY。
    pub terminal_focus: FocusHandle,

    /// 本机 UI 偏好（项目排序 / 折叠）。
    pub prefs: crate::prefs::UiPrefs,

    /// 正在改标题（点头部标题进入）。
    pub title_editing: bool,
    /// 标题输入框。
    pub title_input: Entity<InputState>,
    /// 多选提问已勾选的选项（按勾选顺序）。
    pub question_picked: Vec<String>,
    /// 提问的「其他回答」自由输入。
    pub question_custom: Entity<InputState>,
    /// 审批卡片上「拒绝并说明」展开的反馈输入。
    pub deny_feedback_open: bool,
    pub deny_feedback: Entity<InputState>,
    /// 思考强度下拉是否展开。
    pub reasoning_open: bool,
    /// 运行模式下拉是否展开。
    pub run_mode_open: bool,
    /// `//` 命令面板是否展开。
    pub slash_open: bool,
    /// 对话设置弹窗是否打开。
    pub session_settings_open: bool,
    /// 设置面板是否打开、停在哪一页。
    pub settings_open: bool,
    pub settings_tab: settings::SettingsTab,
    /// 设置面板的编辑副本。打开时从 state 拷一份，改动落在它身上，
    /// 点保存才写盘、点取消直接丢——与原前端的 draft 语义一致。
    pub settings_draft: Option<agent_core::storage::settings::Settings>,
    /// 「工具执行 shell」那一格的输入框。
    pub shell_input: Entity<InputState>,
    /// 右侧工作台当前显示哪个面板。
    pub workbench: right_panel::Workbench,
    /// 编辑区与右侧工作台的宽度。只活在本次运行里，重启回默认——
    /// 与原前端「宽度不持久化」一致。
    pub editor_width: f32,
    pub right_width: f32,

    /// 编辑区的代码编辑器实体表：一个文件一个（语言在建实例时定死）。
    pub editors: std::collections::HashMap<std::path::PathBuf, Entity<InputState>>,

    /// 下一帧要写进输入框的文本。异步回调里拿不到 `Window`，
    /// 所以先存起来，render 时再写进去。
    pub pending_composer_text: Option<String>,

    pub composer: Entity<InputState>,
    pub search: Entity<InputState>,
    focus: FocusHandle,
}

impl HebbianApp {
    pub fn new(
        core: Core,
        mut updates: UnboundedReceiver<CoreUpdate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(1, 6)
                .placeholder("输入消息，Enter 发送，Shift+Enter 换行…")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索"));
        let shell_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 用系统默认 shell"));
        let title_input = cx.new(|cx| InputState::new(window, cx).placeholder("对话标题"));
        let url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("localhost:5173"));
        let question_custom = cx.new(|cx| {
            InputState::new(window, cx).placeholder("或者直接写你的回答")
        });
        let deny_feedback = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 4)
                .placeholder("说明为什么拒绝，这段话会回给模型")
        });

        // 输入框回车即发送。Shift+Enter 由 InputState 自己插换行，不会走到这里。
        cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.send_current_input(window, cx);
                }
            },
        )
        .detach();

        // 搜索框改动直接进 state，项目分桶下一帧就按新词过滤。
        cx.subscribe_in(
            &search,
            window,
            |this, state, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.state.query = state.read(cx).value().to_string();
                    cx.notify();
                }
            },
        )
        .detach();

        // 「工具执行 shell」输入框改动直接落进设置草稿。
        cx.subscribe_in(
            &shell_input,
            window,
            |this, state, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(draft) = this.settings_draft.as_mut() {
                        let value = state.read(cx).value().to_string();
                        draft.general.shell =
                            if value.trim().is_empty() { None } else { Some(value) };
                        cx.notify();
                    }
                }
            },
        )
        .detach();

        // 标题输入框回车 = 提交改名。
        cx.subscribe_in(
            &title_input,
            window,
            |this, state, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let title = state.read(cx).value().trim().to_string();
                    if let Some(id) = this.state.current_id().map(str::to_string) {
                        if !title.is_empty() {
                            this.state.core.rename_session(id, title);
                        }
                    }
                    this.title_editing = false;
                    cx.notify();
                }
            },
        )
        .detach();

        // 地址栏回车即打开。
        cx.subscribe_in(
            &url_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.open_preview(window, cx);
                }
            },
        )
        .detach();

        // core → UI 的唯一事件泵。mpsc 的 recv 不需要 tokio 运行时上下文，
        // 可以直接在 gpui 执行器上 await。
        cx.spawn_in(window, async move |this, cx| {
            while let Some(update) = updates.recv().await {
                let alive = this
                    .update_in(cx, |this, window, cx| {
                        // 打开文件要建带语法高亮的编辑器实体，那需要 window，
                        // 所以这条更新在进 state 之前先截下来处理。
                        if let CoreUpdate::FileLoaded { path, text } = &update {
                            let (path, text) = (path.clone(), text.clone());
                            editor::open(this, &path, &text, window, cx);
                        }
                        if this.state.apply(update) {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        let prefs = crate::prefs::load(core.data_dir());
        core.refresh_catalog();
        core.refresh_providers();
        core.refresh_settings();

        // 调试入口：`HEBBIAN_GPUI_OPEN=<session_id>` 启动即打开某个对话。
        // 无人值守截图 / 排查渲染时不用先点一下侧栏，与 heb CLI 的脚本化调试同一思路。
        if let Ok(session_id) = std::env::var("HEBBIAN_GPUI_OPEN") {
            if !session_id.is_empty() {
                core.open_session(session_id);
            }
        }

        let mut state = AppState::new(core);
        state.collapsed = prefs.collapsed.clone();

        Self {
            state,
            theme: Theme::new(ThemePreset::Glacier, 208.0),
            preset: ThemePreset::Glacier,
            hue: 208.0,
            hue_popover_open: false,
            right_collapsed: false,
            model_picker_open: false,
            model_picker_provider: None,
            webview: None,
            url_input,
            terminal: None,
            terminal_focus: cx.focus_handle(),
            prefs,
            title_editing: false,
            title_input,
            question_picked: Vec::new(),
            question_custom,
            deny_feedback_open: false,
            deny_feedback,
            reasoning_open: false,
            run_mode_open: false,
            slash_open: false,
            session_settings_open: false,
            settings_open: false,
            settings_tab: settings::SettingsTab::General,
            settings_draft: None,
            shell_input,
            workbench: right_panel::Workbench::Files,
            editor_width: editor::DEFAULT_WIDTH,
            right_width: right_panel::DEFAULT_WIDTH,
            editors: std::collections::HashMap::new(),
            pending_composer_text: None,
            composer,
            search,
            focus: cx.focus_handle(),
        }
    }

    pub fn set_theme(&mut self, preset: ThemePreset, hue: f32, cx: &mut Context<Self>) {
        self.preset = preset;
        self.hue = hue;
        self.theme = Theme::new(preset, hue);
        cx.notify();
    }

    /// 弹系统目录选择器，选中的目录登记成项目。
    ///
    /// `prompt_for_paths` 走的是平台原生对话框，结果异步回来，所以要在 gpui 的
    /// async 任务里等，拿到后再回到主线程改状态。
    pub fn pick_project_dir(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择项目文件夹".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.state.core.create_project(dir);
                cx.notify();
            });
        })
        .detach();
    }

    /// 按地址栏里的内容打开预览。
    pub fn open_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.url_input.read(cx).value().to_string();
        let url = browser::normalize_url(&raw);
        browser::navigate(self, &url, window, cx);
        cx.notify();
    }

    /// 打开终端面板时按需起一个 shell，并开一条轮询把新输出刷上屏。
    ///
    /// alacritty 的事件循环跑在它自己的线程里，我们只能问「脏了没」。
    /// 50ms 一次对交互式输入足够跟手，又不至于空转烧 CPU。
    pub fn ensure_terminal(&mut self, cx: &mut Context<Self>) {
        if self.terminal.is_some() {
            return;
        }
        let cwd = self.state.current.as_ref().and_then(|s| s.workdir.clone());
        match crate::terminal::TerminalSession::spawn(cwd, 100, 30) {
            Ok(session) => {
                self.terminal = Some(std::rc::Rc::new(session));
                cx.spawn(async move |this, cx| loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(50))
                        .await;
                    let keep = this
                        .update(cx, |this, cx| match this.terminal.as_ref() {
                            Some(term) => {
                                if term.take_dirty() {
                                    cx.notify();
                                }
                                true
                            }
                            None => false,
                        })
                        .unwrap_or(false);
                    if !keep {
                        break;
                    }
                })
                .detach();
            }
            Err(err) => {
                self.state.error = Some(format!("起不了终端：{err}"));
            }
        }
        cx.notify();
    }

    /// 记一次界面偏好（项目顺序 / 折叠）。改动即写盘，量很小。
    pub fn save_prefs(&mut self) {
        self.prefs.collapsed = self.state.collapsed.clone();
        crate::prefs::save(self.state.core.data_dir(), &self.prefs);
    }

    /// 发起一次需要确认的破坏性操作。
    pub fn ask_confirm(&mut self, action: crate::state::ConfirmAction) {
        self.state.confirm = Some(crate::state::Confirm { action, asked: 0 });
    }

    /// 用户点了「确认」：第一次只是把问题换一句再问，第二次才真的执行。
    pub fn advance_confirm(&mut self) {
        let Some(confirm) = self.state.confirm.take() else {
            return;
        };
        if confirm.asked == 0 {
            self.state.confirm = Some(crate::state::Confirm {
                asked: 1,
                ..confirm
            });
            return;
        }
        match confirm.action {
            crate::state::ConfirmAction::DeleteSession { id, .. } => {
                self.state.core.delete_session(id)
            }
            crate::state::ConfirmAction::DeleteProject { id, .. } => {
                self.state.core.delete_project(id)
            }
        }
    }

    /// 把 `from` 项目挪到 `to` 的位置，并记住新顺序。
    pub fn reorder_project(&mut self, from: &str, to: &str, ordered_ids: &[String]) {
        if from == to {
            return;
        }
        let mut ids = ordered_ids.to_vec();
        let Some(from_ix) = ids.iter().position(|id| id == from) else {
            return;
        };
        let Some(to_ix) = ids.iter().position(|id| id == to) else {
            return;
        };
        let moved = ids.remove(from_ix);
        ids.insert(to_ix, moved);
        self.prefs.project_order = ids;
        self.save_prefs();
    }

    /// 这个文件有没有未保存的改动。
    pub fn is_dirty(&self, path: &std::path::Path, cx: &App) -> bool {
        let Some(editor) = self.editors.get(path) else {
            return false;
        };
        let Some(baseline) = self.state.file_baselines.get(path) else {
            return false;
        };
        editor.read(cx).value().as_ref() != baseline.as_str()
    }

    /// 保存当前标签的内容。编辑器里的文本才是准的——用户可能改过。
    pub fn save_active_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.state.active_file.clone() else {
            return;
        };
        let Some(editor) = self.editors.get(&path) else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        self.state.core.write_file(path, text);
        cx.notify();
    }

    /// 关掉一个编辑器标签。关的是当前活动标签时，焦点顺延到相邻的那个。
    pub fn close_file(&mut self, path: &std::path::Path) {
        self.editors.remove(path);
        if let Some(index) = self.state.open_files.iter().position(|p| p == path) {
            self.state.open_files.remove(index);
            if self.state.active_file.as_deref() == Some(path) {
                self.state.active_file = self
                    .state
                    .open_files
                    .get(index.saturating_sub(1))
                    .or_else(|| self.state.open_files.last())
                    .cloned();
            }
        }
    }

    /// 进入标题编辑：把当前标题填进输入框。
    pub fn start_title_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.state.current.as_ref().map(|s| s.title.clone()) else {
            return;
        };
        self.title_editing = true;
        self.title_input
            .update(cx, |state, cx| state.set_value(title, window, cx));
        self.title_input
            .update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// 给「新对话的默认文件夹」选目录。选中后落进设置草稿，点保存才生效。
    pub fn pick_default_workdir(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择默认文件夹".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(draft) = this.settings_draft.as_mut() {
                    draft.conversation.workdir = Some(dir);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 打开设置面板：拷一份草稿，并把 shell 输入框填上当前值。
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = true;
        let draft = self.state.settings.clone();
        let shell = draft.general.shell.clone().unwrap_or_default();
        self.settings_draft = Some(draft);
        self.shell_input
            .update(cx, |state, cx| state.set_value(shell, window, cx));
        self.state.core.refresh_providers();
        self.state.core.refresh_permissions();
        self.state.core.refresh_log_tail();
        self.state
            .core
            .refresh_extras(self.state.current.as_ref().and_then(|s| s.workdir.clone()));
        if let Some(workdir) = self.state.current.as_ref().and_then(|s| s.workdir.clone()) {
            self.state.core.refresh_skills(workdir);
        }
        cx.notify();
    }

    /// 选文件当附件。选中的路径以 `@路径` 形式追加进输入框——
    /// 这是 core 侧已经支持的引用写法，不需要单独的附件通道。
    pub fn pick_attachments(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("选择要引用的文件".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let refs = paths
                    .iter()
                    .map(|p| format!("@{}", p.display()))
                    .collect::<Vec<_>>()
                    .join(" ");
                let current = this.composer.read(cx).value().to_string();
                let next = if current.trim().is_empty() {
                    format!("{refs} ")
                } else {
                    format!("{current} {refs} ")
                };
                cx.notify();
                this.pending_composer_text = Some(next);
            });
        })
        .detach();
    }

    /// 发送输入框里的内容。空白不发；发完清空。
    pub fn send_current_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.current_id().map(str::to_string) else {
            self.state.error = Some("先新建或选择一个对话".to_string());
            cx.notify();
            return;
        };
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        // 先把自己这条消息挂上去再发。落盘 + RunFinished 后会用 jsonl 覆盖它，
        // 但在那之前必须立刻可见——否则用户按下回车后屏幕毫无变化，
        // 看起来像没发出去（实测就是这样）。
        self.state.messages.push(agent_core::storage::sessions::Message {
            id: format!("local-{}", crate::ui::widgets::now_ms()),
            role: agent_core::storage::sessions::Role::User,
            content: text.clone(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: crate::ui::widgets::now_ms(),
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        });
        self.state.core.send_message(session_id, text);
        cx.notify();
    }
}

impl Focusable for HebbianApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for HebbianApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 「编辑后重跑」取回的原文也在这里落进输入框。
        if let Some(text) = self.state.edit_draft.take() {
            self.pending_composer_text = Some(text);
        }
        // 异步回调攒下来的输入框文本在这里落地——那边没有 Window 可用。
        if let Some(text) = self.pending_composer_text.take() {
            self.composer
                .update(cx, |state, cx| state.set_value(text, window, cx));
        }
        let theme = self.theme.clone();
        div()
            .id("dsp-shell")
            .track_focus(&self.focus)
            .flex()
            .size_full()
            .overflow_hidden()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.))
            .child(sidebar::render(self, window, cx))
            .child(chat::render(self, window, cx))
            .children(editor::render(self, window, cx))
            .child(right_panel::render(self, window, cx))
            .children(session_settings::render(self, cx))
            .children(settings::render(self, cx))
            .children(self.state.confirm.clone().map(|c| confirm_dialog(&theme, c, cx)))
            .children(self.state.error.clone().map(|message| toast(&theme, message, cx)))
    }
}

/// 破坏性操作的确认弹窗。删对话 / 删项目都从这里过一遍。
///
/// 两遍确认之间只换文案不换按钮位置，是有意的：位置一换，第二次点击就成了
/// 「找按钮」而不是「再想一秒」，防误删的作用反而弱了。
fn confirm_dialog(
    theme: &Theme,
    confirm: crate::state::Confirm,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let body = confirm.action.body(confirm.asked);
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        // 半透明遮罩：点空白处等同取消，与原 UI 的 confirm 一致。
        .bg(gpui::rgba(0x0000_0059))
        .child(
            div()
                .id("confirm-card")
                .w(px(360.))
                .p(px(18.))
                .rounded(px(14.))
                .border_1()
                .border_color(theme.line)
                .bg(theme.card_strong)
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight(600.))
                        .child(confirm.action.title()),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child(body),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            div()
                                .id("confirm-cancel")
                                .px(px(12.))
                                .py(px(6.))
                                .rounded(px(8.))
                                .text_size(px(12.))
                                .text_color(theme.muted)
                                .cursor_pointer()
                                .hover(|this| this.bg(theme.surface_veil))
                                .child("取消")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("confirm-ok")
                                .px(px(12.))
                                .py(px(6.))
                                .rounded(px(8.))
                                .text_size(px(12.))
                                .text_color(theme.danger)
                                .cursor_pointer()
                                .hover(|this| this.bg(gpui::rgba(0xd35b5b1a)))
                                .child(if confirm.asked == 0 { "删除" } else { "确认删除" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.advance_confirm();
                                    cx.notify();
                                })),
                        ),
                ),
        )
}

/// 顶部居中的错误提示条，对应原前端 sonner 的 `toast.error`。
/// 点一下消失——没有自动淡出定时器，出错信息留在眼前更容易被看见。
fn toast(
    theme: &Theme,
    message: String,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    div()
        .absolute()
        .top(px(16.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .id("toast")
                .max_w(px(520.))
                .px(px(14.))
                .py(px(10.))
                .rounded(px(10.))
                .border_1()
                .border_color(theme.danger)
                .bg(theme.card_strong)
                .text_size(px(12.))
                .text_color(theme.text)
                .cursor_pointer()
                .child(message)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.error = None;
                    cx.notify();
                })),
        )
}
