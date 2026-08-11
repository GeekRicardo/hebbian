//! 设置面板。对应原前端 `AppSettingsDialog.tsx`：整屏覆盖，左侧 252px 导航
//! （四个分组共 14 项），右侧顶部是标题 + 取消 / 保存 / 关闭。

use gpui::{div, prelude::*, px, Context, Window};

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
            h_flex().gap(px(8.)).child(
                h_flex()
                    .id("settings-close")
                    .h(px(30.))
                    .px(px(14.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.line)
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.accent_soft).text_color(theme.text))
                    .child("关闭")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        cx.notify();
                    })),
            ),
        )
}

fn body(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let content = match app.settings_tab {
        SettingsTab::General => general_pane(app).into_any_element(),
        SettingsTab::Providers => providers_pane(app).into_any_element(),
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

/// 通用：只读展示当前生效的设置。改值要走 `save_settings`，等表单控件补齐后接上。
fn general_pane(app: &HebbianApp) -> impl IntoElement {
    let theme = app.theme.clone();
    let settings = &app.state.settings;

    v_flex()
        .gap(px(2.))
        .child(row(&theme, "界面语言", language_label(settings.general.language)))
        .child(row(
            &theme,
            "开机启动",
            on_off(settings.general.launch_at_login),
        ))
        .child(row(
            &theme,
            "Grep 结果显示搜索路径",
            on_off(settings.general.show_grep_search_path),
        ))
        .child(row(
            &theme,
            "工具调度日志落盘",
            on_off(settings.general.log_enabled),
        ))
        .child(row(
            &theme,
            "工具执行 shell",
            settings
                .general
                .shell
                .clone()
                .unwrap_or_else(|| "系统默认".to_string()),
        ))
        .child(row(&theme, "改文件的方式", edit_backend_label(settings.general.edit_backend)))
        .child(hint(
            &theme,
            "这些值目前只读。改设置的表单控件还没搬过来，要改先在原来的桌面端改，两边读的是同一份配置。",
        ))
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

fn on_off(value: bool) -> String {
    if value { "开" } else { "关" }.to_string()
}
