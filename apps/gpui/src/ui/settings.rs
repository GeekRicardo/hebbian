//! 设置面板。对应原前端 `AppSettingsDialog.tsx`：整屏覆盖，左侧 252px 导航
//! （四个分组共 14 项），右侧顶部是标题 + 取消 / 保存 / 关闭。

use gpui::{div, prelude::*, px, Context};

use crate::assets::Icon;
use crate::ui::widgets::{h_flex, v_flex};
use crate::ui::HebbianApp;

/// 左侧导航的一项。分组顺序与原前端 `TAB_GROUPS` 一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Conversation,
    Appearance,
    Roles,
    Providers,
    Agents,
    Memory,
    Permissions,
    Skills,
    Plugins,
    Hooks,
    Mcp,
    Channels,
    Logs,
}

impl SettingsTab {
    /// (分组, 标签, 图标)。顺序即渲染顺序。
    const ALL: [(&'static str, SettingsTab, &'static str); 14] = [
        ("基础", SettingsTab::General, "通用"),
        ("基础", SettingsTab::Conversation, "对话"),
        ("基础", SettingsTab::Appearance, "外观"),
        ("基础", SettingsTab::Roles, "角色"),
        ("基础", SettingsTab::Providers, "供应商"),
        ("Agent", SettingsTab::Agents, "Agents"),
        ("Agent", SettingsTab::Memory, "记忆"),
        ("Agent", SettingsTab::Permissions, "权限"),
        ("扩展", SettingsTab::Skills, "Skills"),
        ("扩展", SettingsTab::Plugins, "插件"),
        ("扩展", SettingsTab::Hooks, "Hooks"),
        ("扩展", SettingsTab::Mcp, "MCP"),
        ("扩展", SettingsTab::Channels, "连接器"),
        ("调试", SettingsTab::Logs, "日志"),
    ];

    const GROUPS: [&'static str; 4] = ["基础", "Agent", "扩展", "调试"];

    fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, tab, _)| *tab == self)
            .map(|(_, _, label)| *label)
            .unwrap_or("设置")
    }

    fn icon(self) -> Icon {
        match self {
            SettingsTab::General => Icon::Settings,
            SettingsTab::Conversation => Icon::FolderOpen,
            SettingsTab::Appearance => Icon::Palette,
            SettingsTab::Roles => Icon::User,
            SettingsTab::Providers => Icon::Globe,
            SettingsTab::Agents => Icon::Bot,
            SettingsTab::Memory => Icon::Braces,
            SettingsTab::Permissions => Icon::Check,
            SettingsTab::Skills => Icon::Sparkles,
            SettingsTab::Plugins => Icon::Braces,
            SettingsTab::Hooks => Icon::GitBranch,
            SettingsTab::Mcp => Icon::Terminal,
            SettingsTab::Channels => Icon::MessageSquare,
            SettingsTab::Logs => Icon::FileText,
        }
    }
}

/// 整屏覆盖的设置面板。没打开就不渲染。
pub fn render(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> Option<impl IntoElement> {
    if !app.settings_open {
        return None;
    }
    let theme = app.theme.clone();

    Some(
        // 注意用 div().flex() 而不是 h_flex()：后者带 items_center，
        // 会把左右两列按内容高度垂直居中，标题栏就被顶到屏幕中间去了。
        div()
            .flex()
            .flex_row()
            .absolute()
            .inset_0()
            .bg(theme.card_strong)
            .text_color(theme.text)
            .child(nav(app, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(header(app, cx))
                    .child(body(app, cx)),
            ),
    )
}

fn nav(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let mut nav = v_flex()
        .id("settings-nav")
        .w(px(252.))
        .flex_none()
        .h_full()
        .overflow_y_scroll()
        .px(px(12.))
        .py(px(20.))
        .border_r_1()
        .border_color(theme.card_line)
        .bg(theme.sidebar)
        .child(
            h_flex()
                .mb(px(24.))
                .px(px(8.))
                .gap(px(12.))
                .child(
                    div()
                        .size(px(36.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(12.))
                        .bg(theme.card_strong)
                        .child(Icon::Settings.el(px(16.), theme.accent)),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(gpui::FontWeight(600.))
                                .child("设置"),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.faint)
                                .child("全局偏好与扩展"),
                        ),
                ),
        );

    for group in SettingsTab::GROUPS {
        nav = nav.child(
            div()
                .mt(px(14.))
                .mb(px(4.))
                .px(px(8.))
                .text_size(px(11.))
                .font_weight(gpui::FontWeight(500.))
                .text_color(theme.faint)
                .child(group),
        );
        for (g, tab, label) in SettingsTab::ALL {
            if g != group {
                continue;
            }
            let active = app.settings_tab == tab;
            nav = nav.child(
                h_flex()
                    .id(label)
                    .h(px(32.))
                    .px(px(10.))
                    .gap(px(8.))
                    .rounded(px(8.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .when(active, |this| {
                        this.bg(theme.card_strong)
                            .text_color(theme.text)
                            .font_weight(gpui::FontWeight(500.))
                    })
                    .when(!active, |this| {
                        this.text_color(theme.muted)
                            .hover(|this| this.bg(theme.surface_veil).text_color(theme.text))
                    })
                    .child(tab.icon().el(
                        px(14.),
                        if active { theme.accent } else { theme.muted },
                    ))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_tab = tab;
                        cx.notify();
                    })),
            );
        }
    }
    nav
}

fn header(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    h_flex()
        .h(px(64.))
        .flex_none()
        .px(px(32.))
        .justify_between()
        .border_b_1()
        .border_color(theme.card_line)
        .child(
            v_flex()
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child("Hebbian settings"),
                )
                .child(
                    div()
                        .text_size(px(18.))
                        .font_weight(gpui::FontWeight(600.))
                        .child(app.settings_tab.label()),
                ),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .child(
                    h_flex()
                        .id("settings-cancel")
                        .h(px(30.))
                        .px(px(14.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.line)
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.accent_soft).text_color(theme.text))
                        .child("取消")
                        .on_click(cx.listener(|this, _, _, cx| {
                            // 直接丢掉草稿，磁盘上的值不动。
                            this.settings_draft = None;
                            this.settings_open = false;
                            cx.notify();
                        })),
                )
                .child(
                    h_flex()
                        .id("settings-save")
                        .h(px(30.))
                        .px(px(16.))
                        .rounded(px(8.))
                        .bg(theme.accent)
                        .text_size(px(12.))
                        .text_color(gpui::white())
                        .cursor_pointer()
                        .child("保存")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(draft) = this.settings_draft.take() {
                                this.state.core.save_settings(draft);
                            }
                            this.settings_open = false;
                            cx.notify();
                        })),
                ),
        )
}

fn body(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let content = match app.settings_tab {
        SettingsTab::General => general_pane(app, cx).into_any_element(),
        SettingsTab::Providers => providers_pane(app).into_any_element(),
        SettingsTab::Conversation => conversation_pane(app, cx).into_any_element(),
        SettingsTab::Appearance => appearance_pane(app, cx).into_any_element(),
        other => not_yet(app, other.label()).into_any_element(),
    };

    div()
        .id("settings-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(32.))
        .py(px(24.))
        .text_color(theme.text)
        .child(content)
}

/// 通用页。改动落在 `settings_draft` 上，点保存才写盘。
fn general_pane(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    // 草稿没建起来（理论上不会）就退回已加载的值，至少不空白。
    let general = app
        .settings_draft
        .as_ref()
        .map(|d| &d.general)
        .unwrap_or(&app.state.settings.general);

    use agent_core::storage::settings::{AppLanguage, EditBackend};

    v_flex()
        .gap(px(2.))
        .child(segmented(
            &theme,
            cx,
            "界面语言",
            vec![
                ("简体中文", general.language == AppLanguage::ZhCn, 0usize),
                ("English", general.language == AppLanguage::En, 1),
            ],
            |draft, index| {
                draft.general.language = if index == 0 {
                    AppLanguage::ZhCn
                } else {
                    AppLanguage::En
                };
            },
        ))
        .child(switch_row(
            &theme,
            cx,
            "launch-at-login",
            "开机启动",
            general.launch_at_login,
            |draft, on| draft.general.launch_at_login = on,
        ))
        .child(switch_row(
            &theme,
            cx,
            "grep-path",
            "Grep 结果显示搜索路径",
            general.show_grep_search_path,
            |draft, on| draft.general.show_grep_search_path = on,
        ))
        .child(switch_row(
            &theme,
            cx,
            "log-enabled",
            "工具调度日志落盘",
            general.log_enabled,
            |draft, on| draft.general.log_enabled = on,
        ))
        .child(
            h_flex()
                .h(px(44.))
                .justify_between()
                .gap(px(16.))
                .border_b_1()
                .border_color(theme.line)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child("工具执行 shell"),
                )
                .child(
                    div()
                        .w(px(280.))
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.line)
                        .text_size(px(12.))
                        .child(
                            gpui_component::input::Input::new(&app.shell_input)
                                .appearance(false),
                        ),
                ),
        )
        .child(segmented(
            &theme,
            cx,
            "改文件的方式",
            vec![
                (
                    "精确替换原文",
                    general.edit_backend == EditBackend::StringReplace,
                    0usize,
                ),
                (
                    "按行号打补丁",
                    general.edit_backend == EditBackend::Hashline,
                    1,
                ),
            ],
            |draft, index| {
                draft.general.edit_backend = if index == 0 {
                    EditBackend::StringReplace
                } else {
                    EditBackend::Hashline
                };
            },
        ))
}

/// 一行开关。
fn switch_row(
    theme: &crate::theme::Theme,
    cx: &mut Context<HebbianApp>,
    id: &'static str,
    label: &'static str,
    value: bool,
    apply: fn(&mut agent_core::storage::settings::Settings, bool),
) -> impl IntoElement {
    h_flex()
        .h(px(44.))
        .justify_between()
        .border_b_1()
        .border_color(theme.line)
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(label),
        )
        .child(
            gpui_component::switch::Switch::new(id)
                .checked(value)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    if let Some(draft) = this.settings_draft.as_mut() {
                        apply(draft, *checked);
                        cx.notify();
                    }
                })),
        )
}

/// 二选一的分段控件。枚举类设置用它，比下拉更省一次点击。
fn segmented(
    theme: &crate::theme::Theme,
    cx: &mut Context<HebbianApp>,
    label: &'static str,
    options: Vec<(&'static str, bool, usize)>,
    apply: fn(&mut agent_core::storage::settings::Settings, usize),
) -> impl IntoElement {
    let mut group = h_flex()
        .p(px(3.))
        .gap(px(3.))
        .rounded(px(8.))
        .bg(theme.right_bg_top);

    for (text, active, index) in options {
        let theme = theme.clone();
        group = group.child(
            div()
                .id(text)
                .px(px(10.))
                .py(px(4.))
                .rounded(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.card_strong).text_color(theme.text)
                })
                .when(!active, |this| this.text_color(theme.muted))
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(draft) = this.settings_draft.as_mut() {
                        apply(draft, index);
                        cx.notify();
                    }
                })),
        );
    }

    h_flex()
        .h(px(44.))
        .justify_between()
        .border_b_1()
        .border_color(theme.line)
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(label),
        )
        .child(group)
}

/// 对话页：新对话继承的默认值。
fn conversation_pane(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let conv = app
        .settings_draft
        .as_ref()
        .map(|d| &d.conversation)
        .unwrap_or(&app.state.settings.conversation);

    v_flex()
        .gap(px(2.))
        .child(
            h_flex()
                .h(px(44.))
                .justify_between()
                .gap(px(16.))
                .border_b_1()
                .border_color(theme.line)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child("新对话的默认文件夹"),
                )
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .max_w(px(280.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(12.))
                                .child(
                                    conv.workdir
                                        .as_ref()
                                        .map(|p| p.to_string_lossy().to_string())
                                        .unwrap_or_else(|| "用户主目录".to_string()),
                                ),
                        )
                        .child(
                            div()
                                .id("pick-default-workdir")
                                .px(px(10.))
                                .py(px(4.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(theme.line)
                                .text_size(px(11.))
                                .cursor_pointer()
                                .hover(|this| this.bg(theme.accent_soft))
                                .child("选择…")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pick_default_workdir(cx);
                                })),
                        ),
                ),
        )
        .child(row(
            &theme,
            "额外允许访问的路径",
            if conv.allowed_paths.is_empty() {
                "无".to_string()
            } else {
                format!("{} 条", conv.allowed_paths.len())
            },
        ))
        .child(row(
            &theme,
            "默认启用的工具",
            if conv.enabled_tools.is_empty() {
                "只用内置工具".to_string()
            } else {
                format!("{} 个", conv.enabled_tools.len())
            },
        ))
        .child(row(
            &theme,
            "全局规则文件",
            format!("{} 个", conv.global_rules.len()),
        ))
        .child(stepper(
            &theme,
            cx,
            "改动快照保留天数",
            conv.edits_worktree_ttl_days,
            |draft, days| draft.conversation.edits_worktree_ttl_days = days,
        ))
        .child(hint(
            &theme,
            "改完记得点右上角保存。路径与工具清单的编辑还没搬过来，先在原来的桌面端改。",
        ))
}

/// 数值加减控件。天数这种小整数用它比输入框省事，也不会输入非法值。
fn stepper(
    theme: &crate::theme::Theme,
    cx: &mut Context<HebbianApp>,
    label: &'static str,
    value: u32,
    apply: fn(&mut agent_core::storage::settings::Settings, u32),
) -> impl IntoElement {
    let button = |id: &'static str, text: &'static str, delta: i64| {
        let theme = theme.clone();
        div()
            .id(id)
            .size(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .border_1()
            .border_color(theme.line)
            .text_size(px(12.))
            .cursor_pointer()
            .hover(|this| this.bg(theme.accent_soft))
            .child(text)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(draft) = this.settings_draft.as_mut() {
                    let current = value as i64;
                    // 夹在 1..=365：0 天等于立刻清掉快照，太容易误伤。
                    let next = (current + delta).clamp(1, 365) as u32;
                    apply(draft, next);
                    cx.notify();
                }
            }))
    };

    h_flex()
        .h(px(44.))
        .justify_between()
        .border_b_1()
        .border_color(theme.line)
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(label),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .child(button("dec", "−", -1))
                .child(div().text_size(px(12.)).child(format!("{value} 天")))
                .child(button("inc", "+", 1)),
        )
}

/// 外观：色系与深浅由左下角调色盘控制，这里给出当前值与入口说明。
fn appearance_pane(app: &HebbianApp, _cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    v_flex()
        .gap(px(2.))
        .child(row(&theme, "当前色系", app.preset.label().to_string()))
        .child(row(&theme, "色相", format!("{}", app.hue as u32)))
        .child(hint(
            &theme,
            "换色系用左下角那个调色盘按钮，改完整个界面立刻跟着变。",
        ))
}

/// 供应商：真实读 providers.json，展示每家启用状态与模型数。
fn providers_pane(app: &HebbianApp) -> impl IntoElement {
    let theme = app.theme.clone();
    let mut list = v_flex().gap(px(8.));

    if app.state.providers.is_empty() {
        return list
            .child(hint(&theme, "还没有配置模型供应商。"))
            .into_any_element();
    }

    for provider in &app.state.providers {
        list = list.child(
            v_flex()
                .p(px(14.))
                .gap(px(6.))
                .rounded(px(12.))
                .border_1()
                .border_color(theme.line)
                .bg(theme.card)
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight(600.))
                                .child(provider.name.clone()),
                        )
                        .child(
                            div()
                                .px(px(8.))
                                .py(px(2.))
                                .rounded(px(999.))
                                .text_size(px(11.))
                                .bg(if provider.enabled {
                                    theme.accent_soft
                                } else {
                                    theme.line
                                })
                                .text_color(if provider.enabled {
                                    theme.accent
                                } else {
                                    theme.muted
                                })
                                .child(if provider.enabled { "已启用" } else { "已停用" }),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.muted)
                        .child(format!(
                            "{} · {} 个模型",
                            provider.base_url,
                            provider.models.len()
                        )),
                ),
        );
    }
    list.into_any_element()
}

fn not_yet(app: &HebbianApp, label: &'static str) -> impl IntoElement {
    let theme = app.theme.clone();
    hint(
        &theme,
        format!("「{label}」这一页还在搬运中，先去原来的桌面端改，配置是同一份。"),
    )
}

fn row(
    theme: &crate::theme::Theme,
    label: &'static str,
    value: String,
) -> impl IntoElement {
    h_flex()
        .h(px(38.))
        .justify_between()
        .border_b_1()
        .border_color(theme.line)
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(label),
        )
        .child(div().text_size(px(12.)).child(value))
}

fn hint(theme: &crate::theme::Theme, text: impl Into<String>) -> impl IntoElement {
    div()
        .mt(px(16.))
        .p(px(12.))
        .rounded(px(10.))
        .bg(theme.accent_soft)
        .text_size(px(12.))
        .text_color(theme.muted)
        .child(text.into())
}

/// 枚举变体名（ZhCn / StringReplace）绝不能直接进 UI——那是内部命名，
/// 用户看不懂。这里翻成人话。
fn language_label(language: agent_core::storage::settings::AppLanguage) -> String {
    match language {
        agent_core::storage::settings::AppLanguage::ZhCn => "简体中文",
        agent_core::storage::settings::AppLanguage::En => "English",
    }
    .to_string()
}

fn edit_backend_label(backend: agent_core::storage::settings::EditBackend) -> String {
    match backend {
        agent_core::storage::settings::EditBackend::StringReplace => "精确替换原文（默认）",
        agent_core::storage::settings::EditBackend::Hashline => "按行号打补丁（实验）",
    }
    .to_string()
}


