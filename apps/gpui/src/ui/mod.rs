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
use gpui_component::input::{Input, InputEvent, InputState};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::core::{Core, CoreUpdate};
use crate::state::AppState;
use crate::theme::{Theme, ThemePreset};

pub fn init(_cx: &mut App) {}

/// 悬停浮窗挂在哪个锚点上。
#[derive(Debug, Clone, PartialEq)]
pub enum HoverPopup {
    /// 会话删除键上：导出这条对话到 Claude。
    ExportSession(String),
    /// 项目新建键上：从 Claude 导入一条对话到这个项目。
    ImportToProject(Option<String>),
    /// 「新建对话」键上：从 Claude 导入，不限定项目。
    /// 单独一个变体而不是 `ImportToProject(None)`——「默认项目」分组的 + 键也是 None，
    /// 共用会让两个锚点互相点亮对方的浮窗。
    GlobalImport,
}

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

    /// 悬停浮窗当前挂在哪个锚点上。与原前端一样是「悬停浮出」而不是「点开菜单」：
    /// 它只有一个按钮，为一个按钮多要一次点击不值当。
    pub hover_popup: Option<HoverPopup>,
    /// 「这一次悬停」的编号。鼠标离开锚点时不立刻收，而是排一个延时关闭并记下当时的
    /// 编号；这期间只要鼠标进了浮窗（或换了个锚点），编号一变，延时任务醒来发现
    /// 对不上就什么都不做。
    ///
    /// 这段延时是必须的：鼠标从锚点挪到浮窗要跨过几像素间隙，一离开锚点就收的话
    /// 浮窗在半路就没了，那个按钮肉眼看着在、就是点不到。
    hover_popup_gen: u64,
    /// 鼠标此刻是不是停在浮窗上。
    ///
    /// 只靠上面那个编号不够：离开锚点和进入浮窗是同一帧里的两个事件，**先后顺序不定**。
    /// 要是「进入浮窗」先到、「离开锚点」后到，后者排下的关闭就没人作废得了，
    /// 浮窗照样在 260ms 后消失（实测就是这样时好时坏）。所以最终判据是这个布尔量。
    hover_popup_over: bool,

    /// 「从 Claude 导入」弹窗是否打开，以及限定导进哪个项目。
    pub import_claude_open: bool,
    pub import_claude_project: Option<String>,
    /// 导入弹窗里的搜索框与它当前的关键词。
    pub claude_search: Entity<InputState>,
    pub claude_query: String,

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

    /// 预览消息列表的虚拟化状态。见 `import_preview_view` 里为什么必须虚拟化。
    pub preview_list: Option<gpui::ListState>,

    /// 聊天区消息列表的虚拟化状态。见 `chat.rs` 里为什么必须虚拟化。
    pub messages_list: Option<gpui::ListState>,

    /// 聊天区消息列表的滚动句柄。「跳到这次工具调用」要用它把对应消息滚进视野。
    pub messages_scroll: gpui::ScrollHandle,

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
        let claude_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜标题…"));
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

        // 导入弹窗的搜索框：改一个字就重新过滤列表。
        cx.subscribe_in(
            &claude_search,
            window,
            |this, state, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.claude_query = state.read(cx).value().to_string();
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
            hover_popup: None,
            hover_popup_gen: 0,
            hover_popup_over: false,
            import_claude_open: false,
            import_claude_project: None,
            claude_search,
            claude_query: String::new(),
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
            preview_list: None,
            messages_list: None,
            messages_scroll: gpui::ScrollHandle::new(),
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

    /// 悬停到锚点就显示浮窗；换一个锚点就换一个浮窗。顺带作废在排队的延时关闭。
    pub fn open_hover_popup(&mut self, popup: HoverPopup) {
        self.hover_popup = Some(popup);
        self.hover_popup_over = false;
        self.keep_hover_popup();
    }

    /// 鼠标进了浮窗：让在排队的延时关闭作废。
    pub fn keep_hover_popup(&mut self) {
        self.hover_popup_gen = self.hover_popup_gen.wrapping_add(1);
    }

    /// 记录鼠标是不是在浮窗上。
    pub fn set_hover_popup_over(&mut self, over: bool) {
        self.hover_popup_over = over;
    }

    /// 鼠标离开锚点或浮窗：排一个延时关闭，给「挪到浮窗上」留出时间。
    pub fn schedule_hover_close(&mut self, cx: &mut Context<Self>) {
        self.hover_popup_gen = self.hover_popup_gen.wrapping_add(1);
        let generation = self.hover_popup_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(260))
                .await;
            let _ = this.update(cx, |this, cx| {
                // 编号变了 = 这中间换了锚点；鼠标还停在浮窗上 = 用户正要去点它。
                // 两种情况都不能关。
                if this.hover_popup_gen == generation && !this.hover_popup_over {
                    this.hover_popup = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 「后台任务」面板点了某张卡片：把聊天区滚到对应的工具调用、展开它、闪一下。
    ///
    /// 在 render 里消费而不是在点击时做，是因为滚动要等这一帧的布局——
    /// 点击那一刻消息还没重新排版，滚过去会落在错的位置。
    fn consume_focus_tool_call(&mut self, cx: &mut Context<Self>) {
        let Some(call_id) = self.state.focus_tool_call.take() else {
            return;
        };
        // 占位 id（任务刚起、工具结果还没落进消息）没有可跳的目标，直接算了。
        if call_id.starts_with("pending-") {
            return;
        }
        let message_ix = self
            .state
            .messages
            .iter()
            .position(|m| m.tool_calls.iter().any(|c| c.id == call_id));
        let Some(message_ix) = message_ix else {
            self.state.error = Some("这次任务在当前对话里找不到对应的记录".to_string());
            return;
        };
        self.state.expanded_calls.insert(call_id.clone());
        self.state.flash_tool_call = Some(call_id);
        self.messages_scroll.scroll_to_item(message_ix);

        // 高亮只留一会儿：一直亮着就成了「选中」，会让人以为这张卡片有别的状态。
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1600))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.state.flash_tool_call = None;
                cx.notify();
            });
        })
        .detach();
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
        self.consume_focus_tool_call(cx);
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
            .children(if self.import_claude_open { Some(import_claude_dialog(self, cx)) } else { None })
            .children(
                self.state
                    .claude_exported
                    .clone()
                    .map(|cmd| exported_dialog(&theme, cmd, cx)),
            )
            .children(self.state.confirm.clone().map(|c| confirm_dialog(&theme, c, cx)))
            .children(self.state.error.clone().map(|message| toast(&theme, message, cx)))
    }
}

/// 悬停浮窗：从锚点按钮的右缘浮出的一个小按钮。
///
/// 定位方式是 `absolute` + `left: 100%`，也就是「贴着锚点右边」。试过 `anchored()`——
/// 它落在锚点的**左上角**，锚点要是像「新建对话」那样占满整行，浮窗就直接盖在按钮
/// 文字上了；按锚点宽度的百分比定位则不管锚点多宽都贴在右侧。外面裹 `deferred`
/// 是为了画在同层兄弟节点之上，否则会被后面的行盖住。
///
/// 左边那点内边距是有用的：鼠标从锚点挪到按钮上要经过几像素间隙，
/// 间隙不属于任何元素的话浮窗会在半路收掉，按钮永远点不到。
///
/// **调用方的锚点必须是 `absolute` 或 `relative`**，否则百分比没有参照物。
pub fn hover_popup(
    theme: &Theme,
    popup: HoverPopup,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let (label, icon) = match &popup {
        HoverPopup::ExportSession(_) => ("导出到 Claude", crate::assets::Icon::ArrowUpFromLine),
        HoverPopup::ImportToProject(_) | HoverPopup::GlobalImport => {
            ("从 Claude 导入", crate::assets::Icon::Import)
        }
    };
    gpui::deferred(
        div()
            .id("hover-popup-shell")
            .absolute()
            .left(gpui::relative(1.))
            .top(px(-4.))
            // 这点内边距把锚点和浮窗之间的间隙填上，鼠标挪过去时不会掉到
            // 「谁都不属于」的空隙里。
            .pl(px(8.))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.set_hover_popup_over(*hovered);
                if *hovered {
                    this.keep_hover_popup();
                } else {
                    this.schedule_hover_close(cx);
                }
                cx.notify();
            }))
            .child(
                div()
                    .id("hover-popup-btn")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.))
                    .px(px(11.))
                    .py(px(5.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.line)
                    .bg(theme.card_strong)
                    .text_size(px(11.))
                    .text_color(theme.text)
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(|this| this.text_color(theme.accent).bg(theme.accent_soft))
                    // 点到浮窗以外的任何地方就收起来。这是它唯一的关闭路径——
                    // 见上面 `open_hover_popup` 里为什么不能靠「离开锚点」来收。
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.hover_popup = None;
                        cx.notify();
                    }))
                    .child(icon.el(px(11.), theme.muted))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // 浮窗挂在锚点按钮里面，不截住就会连锚点的动作一起触发
                        // （表现为点「从 Claude 导入」还顺手新建了一个空对话）。
                        cx.stop_propagation();
                        match this.hover_popup.take() {
                            Some(HoverPopup::ExportSession(id)) => {
                                // 跟原 UI 一样固定带上推理链：导出是为了换个工具接着聊，
                                // 丢了推理链等于丢上下文。
                                this.state.core.export_session_to_claude(id, true);
                            }
                            Some(HoverPopup::GlobalImport) => {
                                this.import_claude_project = None;
                                this.import_claude_open = true;
                                this.state.core.refresh_claude_importable();
                            }
                            Some(HoverPopup::ImportToProject(project_id)) => {
                                this.import_claude_project = project_id;
                                this.import_claude_open = true;
                                this.state.core.refresh_claude_importable();
                            }
                            None => {}
                        }
                        cx.notify();
                    })),
            ),
    )
}

/// 「从 Claude 导入」弹窗。两个视图：列表 → 预览 → 确认导入。
///
/// 为什么要先预览：Claude 的会话标题是从第一条消息截出来的，光看标题经常分不清
/// 哪个是想找的那段；而导入是有副作用的（会多出一个会话）。原前端也是
/// 「点击预览内容，满意了再导入」。
fn import_claude_dialog(app: &mut HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    if let Some(preview) = app.state.claude_preview.clone() {
        return import_preview_view(app, preview, cx).into_any_element();
    }

    let query = app.claude_query.to_lowercase();
    // 按工作目录分组：同一个项目下的对话挨在一起，比一长串标题好找得多。
    // 用 Vec 而不是 HashMap 保持列表本来的时间顺序（新的在前）。
    let mut groups: Vec<(String, Vec<&crate::core::ClaudeImportable>)> = Vec::new();
    for item in &app.state.claude_importable {
        if !query.is_empty() && !item.title.to_lowercase().contains(&query) {
            continue;
        }
        let dir = if item.cwd.is_empty() {
            "没记录工作目录".to_string()
        } else {
            item.cwd.clone()
        };
        match groups.iter_mut().find(|(d, _)| d == &dir) {
            Some((_, list)) => list.push(item),
            None => groups.push((dir, vec![item])),
        }
    }

    let mut rows = div()
        .id("claude-import-list")
        .flex()
        .flex_col()
        .gap(px(10.))
        .max_h(px(340.))
        .overflow_y_scroll();
    if groups.is_empty() {
        rows = rows.child(
            div()
                .py(px(20.))
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(if app.claude_query.is_empty() {
                    "这台机器上没找到 Claude 的对话记录"
                } else {
                    "没有匹配的对话"
                }),
        );
    }
    for (dir, list) in groups {
        // 目录只显示末段，全路径挂 tooltip——弹窗里铺绝对路径会把标题挤没。
        let leaf = dir.rsplit(['/', '\\']).next().unwrap_or(&dir).to_string();
        let mut group = div().flex().flex_col().gap(px(2.)).child(
            div()
                .id(gpui::SharedString::from(format!("grp-{dir}")))
                .flex()
                .flex_row()
                .gap(px(6.))
                .text_size(px(10.))
                .text_color(theme.faint)
                .tooltip({
                    let dir = dir.clone();
                    move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(dir.clone()).build(window, cx)
                    }
                })
                .child(leaf)
                .child(format!("· {}", list.len())),
        );
        for item in list {
            let path = item.path.clone();
            let stamp = chrono::DateTime::from_timestamp_millis(item.modified_ms)
                .map(|dt| dt.format("%m/%d %H:%M").to_string())
                .unwrap_or_default();
            group = group.child(
                div()
                    .id(gpui::SharedString::from(format!("imp-{}", item.path)))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.accent_soft))
                    .child(
                        div()
                            .text_size(px(12.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(item.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .text_size(px(10.))
                            .text_color(theme.faint)
                            .child(stamp)
                            .child(format!("{} 条消息", item.message_count)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.core.preview_claude_session(path.clone());
                        cx.notify();
                    })),
            );
        }
        rows = rows.child(group);
    }

    dialog_frame(
        &theme,
        "从 Claude 导入",
        cx,
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .h(px(30.))
                    .px(px(8.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.line)
                    .bg(theme.surface_veil)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .text_color(theme.input_text)
                            .child(Input::new(&app.claude_search).appearance(false)),
                    ),
            )
            .child(rows)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.muted)
                    .child("点一条先看看内容，确认了再导入"),
            ),
    )
    .into_any_element()
}

/// 预览视图：返回列表 + 导入按钮 + 消息正文。
fn import_preview_view(
    app: &mut HebbianApp,
    preview: std::rc::Rc<crate::core::ClaudePreview>,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let path = preview.path.clone();
    let count = preview.messages.len();

    // **必须虚拟化**。真实对话里一段 33 条消息能带 439 次工具调用，
    // 一次性把它们全画出来（哪怕每张卡片都是收起的）就够让弹窗几十秒出不来。
    // `list` 只渲染视口内那几条，多大的对话都是常数开销——这样才用不着
    // 靠「只显示前 N 条」这种把信息藏起来的办法。
    let list_state = match app.preview_list.as_ref() {
        Some(state) if state.item_count() == count => state.clone(),
        _ => {
            let state = gpui::ListState::new(count, gpui::ListAlignment::Top, px(600.));
            app.preview_list = Some(state.clone());
            state
        }
    };

    let entity = cx.entity().downgrade();
    // 哪些工具卡片是展开的，在**建元素时就快照一份**交给闭包。
    // 别在闭包里回头读 entity：那份读发生在布局阶段，和这一帧的状态对不对得上
    // 不好保证。
    let expanded_keys = std::rc::Rc::new(app.state.expanded_parts.clone());
    let body = if count == 0 {
        div()
            .py(px(20.))
            .text_size(px(12.))
            .text_color(theme.muted)
            .child("这段对话是空的")
            .into_any_element()
    } else {
        gpui::list(list_state, {
            let preview = preview.clone();
            let theme = theme.clone();
            let expanded_keys = expanded_keys.clone();
            move |ix, window, cx| {
                let Some(app) = entity.upgrade() else {
                    return div().into_any_element();
                };
                preview_message(&preview, ix, &expanded_keys, &theme, &app, window, cx)
            }
        })
        .flex_1()
        .into_any_element()
    };

    dialog_frame(
        &theme,
        "预览这段对话",
        cx,
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .h(px(380.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .flex_none()
                    .child(
                        div()
                            .id("preview-back")
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .text_size(px(11.))
                            .text_color(theme.muted)
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.surface_veil))
                            .child("返回列表")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.claude_preview = None;
                                this.preview_list = None;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("preview-import")
                            .px(px(12.))
                            .py(px(5.))
                            .rounded(px(8.))
                            .bg(theme.accent)
                            .text_size(px(11.))
                            .text_color(gpui::white())
                            .cursor_pointer()
                            .child("导入此对话")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let project = this.import_claude_project.clone();
                                this.state.core.import_claude_session(path.clone(), project);
                                this.state.claude_preview = None;
                                this.preview_list = None;
                                this.import_claude_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(10.))
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(theme.faint)
                    .child(preview.model.clone())
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(preview.cwd.clone()),
                    )
                    .child(format!("{count} 条消息")),
            )
            .child(body),
    )
}

/// 预览里的一条消息。虚拟化列表按需调它，所以这里只画一条，不碰别的。
#[allow(clippy::too_many_arguments)]
fn preview_message(
    preview: &crate::core::ClaudePreview,
    ix: usize,
    expanded_keys: &std::collections::HashSet<String>,
    theme: &Theme,
    app: &Entity<HebbianApp>,
    _window: &mut Window,
    _cx: &mut App,
) -> gpui::AnyElement {
    let Some(message) = preview.messages.get(ix) else {
        return div().into_any_element();
    };
    let is_user = matches!(message.role, agent_core::storage::sessions::Role::User);
    let text = message.content.trim();

    let mut bubble = div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .pb(px(10.))
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme.faint)
                .child(if is_user { "我" } else { "助手" }),
        );

    if !text.is_empty() {
        bubble = bubble.child(
            div()
                .p(px(8.))
                .rounded(px(8.))
                .bg(if is_user { theme.accent_soft } else { theme.surface_veil })
                .text_size(px(11.))
                .text_color(theme.text)
                .child(text.to_string()),
        );
    }

    // 工具调用用聊天区那张卡片本体，不是另画一个简版：预览和正文里看到的东西
    // 应当长一样，可展开看入参出参。收起态本身很轻（不序列化入参、不拷结果），
    // 加上外面这层虚拟化，铺多少张都不贵。
    for (ci, call) in message.tool_calls.iter().enumerate() {
        let key = format!("preview-{ix}-{ci}");
        bubble = bubble.child(crate::ui::chat::tool_card(
            theme,
            &app.downgrade(),
            &key,
            expanded_keys.contains(&key),
            false,
            &call.name,
            &call.input,
            call.result.as_deref(),
            call.duration_ms,
            call.is_error,
        ));
    }
    bubble.into_any_element()
}

/// 导出成功后把 resume 命令摆出来给用户复制。
/// 只说一句「已导出」没用——用户真正要的是那条能直接粘进终端的命令。
fn exported_dialog(
    theme: &Theme,
    resume_command: String,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let to_copy = resume_command.clone();
    dialog_frame(
        theme,
        "已导出到 Claude",
        cx,
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.muted)
                    .child("在终端里执行这条命令就能接着聊："),
            )
            .child(
                div()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(theme.surface_veil)
                    .font_family("monospace")
                    .text_size(px(11.))
                    .child(resume_command),
            )
            .child(
                div().flex().flex_row().justify_end().child(
                    div()
                        .id("copy-resume")
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .text_size(px(12.))
                        .text_color(theme.accent)
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.accent_soft))
                        .child("复制命令")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(to_copy.clone()));
                            this.state.claude_exported = None;
                            cx.notify();
                        })),
                ),
            ),
    )
}

fn dialog_frame(
    theme: &Theme,
    title: &'static str,
    cx: &mut Context<HebbianApp>,
    content: impl IntoElement,
) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        // 遮罩必须吃掉鼠标事件：不 occlude 的话点击会穿到下面的聊天区，
        // 表现为「点弹窗里的搜索框没反应，打的字全跑到别处」。
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::rgba(0x0000_0059))
        .child(
            div()
                .w(px(460.))
                .p(px(18.))
                .rounded(px(14.))
                .border_1()
                .border_color(theme.line)
                .bg(theme.card_strong)
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight(600.))
                                .child(title),
                        )
                        .child(
                            div()
                                .id("dialog-close")
                                .size(px(22.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .cursor_pointer()
                                .hover(|this| this.bg(theme.surface_veil))
                                .child(crate::assets::Icon::X.el(px(12.), theme.faint))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.import_claude_open = false;
                                    this.state.claude_exported = None;
                                    // 预览与搜索词一起清掉：下次打开不该停在上次的状态
                                    this.state.claude_preview = None;
                                    this.claude_query.clear();
                                    cx.notify();
                                })),
                        ),
                )
                .child(content),
        )
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
        .occlude()
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
