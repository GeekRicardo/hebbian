//! 聊天列：header + 消息画布 + 输入区。
//!
//! 对应 `ChatView.tsx` 与 `.dsp-chat-host` / `.dsp-composer*`。空会话时走
//! `DesktopEmptyState`（居中的品牌卡片 + 「你想用 Hebbian 做什么」）。

use agent_core::storage::sessions::{Message, MessagePart, Role};
use gpui::{div, prelude::*, px, AnyElement, Context, Window};

use crate::assets::Icon;
use crate::state::StreamingTurn;
use crate::ui::widgets::{h_flex, now_ms, shadow_lifted, v_flex};
use crate::ui::HebbianApp;

pub fn render(app: &mut HebbianApp, window: &mut Window, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    v_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_hidden()
        // `.dsp-chat-host` 的多层 radial 叠加 gpui 画不了；用同色系的竖向渐变
        // 保住「顶部略带色、往下化开」的观感（CSS 最后一层就是这条 linear-gradient）。
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(theme.chat_panel, 0.),
            gpui::linear_color_stop(theme.chat_panel_end, 0.58),
        ))
        .child(header(app, cx))
        .child(canvas(app, window, cx))
        .child(composer(app, cx))
}

/// `ChatView` 的 header：h-14、左标题右会话号。
fn header(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let title = app
        .state
        .current
        .as_ref()
        .map(|s| s.title.clone())
        .unwrap_or_default();
    let session_id = app.state.current_id().unwrap_or_default().to_string();

    h_flex()
        .h(px(56.))
        .flex_none()
        .px(px(16.))
        .justify_between()
        .child(
            h_flex()
                .gap(px(8.))
                .min_w_0()
                .child(if app.title_editing {
                    div()
                        .w(px(260.))
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme.accent)
                        .text_size(px(14.))
                        .child(
                            gpui_component::input::Input::new(&app.title_input)
                                .appearance(false),
                        )
                        .into_any_element()
                } else {
                    div()
                        .id("session-title")
                        .max_w(px(260.))
                        .px(px(4.))
                        .rounded(px(4.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight(500.))
                        .cursor_text()
                        .hover(|this| this.bg(theme.accent_soft))
                        .child(title)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_title_edit(window, cx);
                        }))
                        .into_any_element()
                })
                .child(
                    div()
                        .id("regen-title")
                        .p(px(4.))
                        .rounded(px(6.))
                        .text_color(theme.muted)
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.accent_soft))
                        .child(Icon::Sparkles.el(px(14.), theme.muted))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.error = Some("重新生成标题还没接上".to_string());
                            cx.notify();
                        })),
                ),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.faint)
                        .child(session_id),
                )
                .child(
                    div()
                        .id("session-settings-open")
                        .p(px(4.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.accent_soft))
                        .child(Icon::Settings.el(px(14.), theme.faint))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.session_settings_open = true;
                            this.state.core.refresh_providers();
                            cx.notify();
                        })),
                ),
        )
}

/// 消息画布。没有会话时显示欢迎页。
fn canvas(
    app: &mut HebbianApp,
    _window: &mut Window,
    cx: &mut Context<HebbianApp>,
) -> AnyElement {
    // 两种「空」是不同的画面，原前端分得很清楚，我之前混成了一个：
    // 压根没选会话 → ChatView 自己那块（渐变方块 + 两个按钮）；
    // 选了会话但还没消息 → DesktopShell 传进来的品牌卡片。
    let Some(session) = app.state.current.as_ref() else {
        return no_session_state(app, cx).into_any_element();
    };
    // 只有「真的什么都没有」才显示欢迎页。有待审批 / 待回答时不能早返回——
    // 否则审批卡片会被欢迎页顶掉，run 卡在那儿而界面上什么都没有（实测踩到过）。
    let has_pending = app
        .state
        .current_id()
        .is_some_and(|id| {
            app.state.pending_approvals.contains_key(id)
                || app.state.pending_questions.contains_key(id)
        });
    if session.messages.is_empty()
        && app.state.messages.is_empty()
        && app.state.streaming.is_empty()
        && !has_pending
    {
        return empty_state(app).into_any_element();
    }

    // 消息列表**虚拟化**：只渲染视口内那几条。
    //
    // 非虚拟化版本在真实体量下是撑不住的：一段 33 条消息、439 次工具调用的对话
    // （从 Claude 导入的真日志）打开要一分多钟才画得出来。虚拟化之后是常数开销，
    // 多长的对话都一样快。
    //
    // 流式气泡与审批 / 提问卡片挂在列表**末尾的虚拟项**里，而不是列表外面——
    // 挂外面它们就不跟着内容滚了，会一直钉在底部挡住消息。
    let ctx = std::rc::Rc::new(RenderCtx::snapshot(app, cx));
    let messages = std::rc::Rc::new(app.state.messages.clone());
    let streaming = std::rc::Rc::new(app.state.streaming.clone());
    let pending_approval = app
        .state
        .current_id()
        .and_then(|id| app.state.pending_approvals.get(id))
        .cloned();
    let pending_question = app
        .state
        .current_id()
        .and_then(|id| app.state.pending_questions.get(id))
        .cloned();

    // 尾部这几项按需出现，算进总数好让它们也参与虚拟化与滚动。
    let mut tail: Vec<TailItem> = Vec::new();
    if !streaming.is_empty() {
        tail.push(TailItem::Streaming);
    }
    let tail = std::rc::Rc::new(tail);
    let count = messages.len() + tail.len();

    let list_state = match app.messages_list.as_ref() {
        Some(state) if state.item_count() == count => state.clone(),
        _ => {
            // 新会话 / 条数变了就重建，并停在底部——聊天区的默认视角是最新一条。
            let state = gpui::ListState::new(count, gpui::ListAlignment::Bottom, px(800.));
            app.messages_list = Some(state.clone());
            state
        }
    };

    let messages_list = gpui::list(list_state, {
        let ctx = ctx.clone();
        move |ix, window, cx| {
            let inner = |el: gpui::AnyElement| {
                div()
                    .w_full()
                    .max_w(px(880.))
                    .mx_auto()
                    .px(px(32.))
                    .child(el)
                    .into_any_element()
            };
            if let Some(message) = messages.get(ix) {
                return inner(bubble(&ctx, message, window, cx).into_any_element());
            }
            match tail.get(ix - messages.len()) {
                Some(TailItem::Streaming) => {
                    inner(streaming_bubble(&ctx, &streaming, window, cx).into_any_element())
                }
                None => div().into_any_element(),
            }
        }
    })
    .flex_1();

    // 审批 / 提问压在消息流末尾（列表是底对齐的，视觉上就接在最后一条后面）。
    let mut column = v_flex().flex_1().min_h_0().child(messages_list);
    if let Some(pending) = pending_approval.as_ref() {
        column = column.child(approval_card(app, cx, pending));
    }
    if let Some(question) = pending_question.as_ref() {
        column = column.child(question_card(app, cx, question));
    }
    column.into_any_element()
}

/// 消息之后那几个「跟着一起滚」的东西。目前只有流式气泡：
/// 审批 / 提问卡片留在列表外面（见下面 `render` 尾部），它们那几个监听器都在
/// HITL 主路径上，为了虚拟化去改写不划算，而且它们本来就只出现在对话末尾。
#[derive(Clone, Copy)]
enum TailItem {
    Streaming,
}


/// 还没选任何会话时的画面。对应 `ChatView` 里 `!currentSession` 那一段：
/// 56px 渐变圆角方块 + 标题 + 一句说明 + 「新建对话 / 供应商配置」两个按钮。
fn no_session_state(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let button = |label: &'static str, primary: bool, theme: crate::theme::Theme| {
        h_flex()
            .id(label)
            .h(px(32.))
            .px(px(14.))
            .rounded(px(8.))
            .text_size(px(13.))
            .cursor_pointer()
            .when(primary, |this| {
                this.bg(theme.accent).text_color(gpui::white())
            })
            .when(!primary, |this| {
                this.border_1().border_color(theme.line).text_color(theme.text)
            })
            .child(label)
    };

    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .px(px(24.))
        .child(
            div()
                .size(px(56.))
                .mb(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(16.))
                // 原前端是 sky-500 → indigo-600 的斜向渐变。
                .bg(gpui::linear_gradient(
                    135.,
                    gpui::linear_color_stop(theme.accent, 0.),
                    gpui::linear_color_stop(theme.accent_2, 1.),
                ))
                .child(Icon::Sparkles.el(px(28.), gpui::white())),
        )
        .child(
            div()
                .text_size(px(18.))
                .font_weight(gpui::FontWeight(600.))
                .child("开始一场新的对话"),
        )
        .child(
            div()
                .mt(px(4.))
                .max_w(px(384.))
                .text_size(px(13.))
                .text_color(theme.muted)
                .child("在左侧点击「新建对话」，或先前往供应商配置添加你的 API Key。"),
        )
        .child(
            h_flex()
                .mt(px(20.))
                .gap(px(8.))
                .child(
                    button("新建对话", true, theme.clone()).on_click(cx.listener(
                        |this, _, _, _| {
                            this.state.core.create_session(None, None);
                        },
                    )),
                )
                .child(
                    button("供应商配置", false, theme).on_click(cx.listener(
                        |this, _, window, cx| {
                            this.open_settings(window, cx);
                            this.settings_tab = crate::ui::settings::SettingsTab::Providers;
                            cx.notify();
                        },
                    )),
                ),
        )
}

/// `.dsp-empty-state`：品牌卡片 + 一行大标题。
fn empty_state(app: &HebbianApp) -> impl IntoElement {
    let theme = app.theme.clone();
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .pt(px(120.))
        .child(
            div()
                .w(px(430.))
                .h(px(176.))
                .mb(px(28.))
                .rounded(px(24.))
                .border_1()
                .border_color(theme.card_line)
                .bg(gpui::linear_gradient(
                    105.,
                    gpui::linear_color_stop(theme.card_strong, 0.),
                    gpui::linear_color_stop(theme.right_bg_bottom, 1.),
                ))
                .shadow(shadow_lifted(gpui::rgba(0x394d6614).into()))
                .child(
                    // 卡片中央的小方块 logo 位（CSS `.dsp-hero-cardlet`）。
                    div()
                        .absolute()
                        .size(px(72.))
                        .rounded(px(24.))
                        .bg(theme.surface_veil),
                ),
        )
        .child(
            div()
                .text_size(px(26.))
                .font_weight(gpui::FontWeight(700.))
                .child("你想用 Hebbian 做什么"),
        )
}

/// 一条落盘消息。用户消息靠右、助手消息占满宽——与 `.dsp-message.is-user` 一致。
fn bubble(
    ctx: &RenderCtx,
    message: &Message,
    window: &mut Window,
    cx: &mut gpui::App,
) -> impl IntoElement {
    let theme = ctx.theme.clone();
    let is_user = matches!(message.role, Role::User);

    if matches!(message.role, Role::Marker) {
        return div()
            .mx_auto()
            .my(px(20.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(999.))
            .border_1()
            .border_color(theme.line)
            .bg(theme.card)
            .text_size(px(12.))
            .text_color(theme.muted)
            .child(message.content.clone());
    }

    // 正文直接铺在画布上，不套气泡边框——这与实际跑起来的 `MessageBubble` 一致
    // （`.dsp-message-body` 那套卡片是更早一版设计，现行 shell 没有启用）。
    let mut body = v_flex().flex_1().min_w_0().gap(px(8.));

    if message.parts.is_empty() {
        // 老 jsonl 没有 parts，只有一整块 content。
        body = body.child(markdown(
            format!("msg-{}", message.id),
            message.content.clone(),
            &theme,
            window,
            cx,
        ));
        for (i, call) in message.tool_calls.iter().enumerate() {
            let key = format!("{}-tc{}", message.id, i);
            body = body.child(tool_card(
                &ctx.theme,
                &ctx.entity,
                &key,
                ctx.call_expanded(&key, Some(&call.id)),
                ctx.flashed(&call.id),
                &call.name,
                &call.input,
                call.result.as_deref(),
                call.duration_ms,
                call.is_error,
            ));
        }
    } else {
        // 有 parts 就按落盘的时序渲染：文本 / 思考 / 工具调用交错，
        // 与模型实际产出的顺序一致（原前端也是按 parts 走）。
        for (i, part) in message.parts.iter().enumerate() {
            let key = format!("{}-p{}", message.id, i);
            body = match part {
                MessagePart::Text { text } => body.child(markdown(
                    key,
                    text.clone(),
                    &theme,
                    window,
                    cx,
                )),
                MessagePart::Reasoning { text, duration_ms } => {
                    body.child(reasoning_block(ctx, &key, text, *duration_ms))
                }
                MessagePart::ToolCall {
                    id,
                    name,
                    input,
                    result,
                    duration_ms,
                    is_error,
                    ..
                } => body.child(tool_card(
                    &ctx.theme,
                    &ctx.entity,
                    &key,
                    ctx.call_expanded(&key, Some(id)),
                    ctx.flashed(id),
                    name,
                    input,
                    result.as_deref(),
                    *duration_ms,
                    *is_error,
                )),
            };
        }
    }

    body = body.child(meta_row(ctx, message, is_user));

    h_flex()
        .group(gpui::SharedString::from(format!("msg-{}", message.id)))
        .items_start()
        .gap(px(12.))
        .mb(px(20.))
        .child(avatar(&ctx.theme, is_user))
        .child(body)
}

/// Markdown 正文。gpui-component 的 `TextView` 负责解析与代码块高亮；
/// 外层只负责把字号 / 行高 / 颜色拉回本主题，避免用它自带的配色。
fn markdown(
    id: String,
    text: String,
    theme: &crate::theme::Theme,
    window: &mut Window,
    cx: &mut gpui::App,
) -> impl IntoElement {
    div()
        .text_size(px(14.))
        .line_height(px(24.))
        .text_color(theme.text)
        .child(gpui_component::text::TextView::markdown(
            gpui::SharedString::from(id),
            text,
            window,
            cx,
        ))
}

/// 思考过程折叠块。对应原 UI 的「⏱ 思考过程 N ms ⌄」——默认收起，
/// 展开后正文用弱化色 + 左侧竖线，与正式回答区分开。
fn reasoning_block(
    ctx: &RenderCtx,
    key: &str,
    text: &str,
    duration_ms: Option<u64>,
) -> impl IntoElement {
    let theme = ctx.theme.clone();
    let expanded = ctx.expanded_parts.contains(key);
    let entity = ctx.entity.clone();
    let key_owned = key.to_string();
    let duration = match duration_ms {
        Some(ms) if ms >= 1000 => format!("{:.1}s", ms as f64 / 1000.),
        Some(ms) => format!("{ms}ms"),
        None => String::new(),
    };
    let body = text.to_string();

    v_flex()
        .child(
            h_flex()
                .id(gpui::SharedString::from(format!("reason-{key}")))
                .gap(px(5.))
                .text_size(px(12.))
                .text_color(theme.muted)
                .cursor_pointer()
                .hover(|this| this.text_color(theme.text))
                .child(Icon::Clock.el(px(12.), theme.faint))
                .child("思考过程")
                .child(div().text_color(theme.faint).child(duration))
                .child(
                    if expanded {
                        Icon::ChevronDown.el(px(12.), theme.faint)
                    } else {
                        Icon::ChevronRight.el(px(12.), theme.faint)
                    },
                )
                .on_click(move |_, _, cx: &mut gpui::App| {
                    let Some(app) = entity.upgrade() else { return };
                    let key = key_owned.clone();
                    app.update(cx, |this, cx| {
                        if !this.state.expanded_parts.remove(&key) {
                            this.state.expanded_parts.insert(key);
                        }
                        cx.notify();
                    });
                }),
        )
        .when(expanded && !body.is_empty(), |this| {
            this.child(
                div()
                    .mt(px(6.))
                    .pl(px(12.))
                    .border_l_2()
                    .border_color(theme.line)
                    .text_size(px(12.))
                    .line_height(px(20.))
                    .text_color(theme.muted)
                    .child(body),
            )
        })
}

/// 工具调用卡片。收起时是一行「图标 + 工具名 + 摘要 + 耗时」，
/// 展开后把入参与结果原样摊开——排查 agent 行为时看的就是这两块。
#[allow(clippy::too_many_arguments)]
/// 渲染一条消息所需的全部状态。
///
/// 存在的理由：消息列表要虚拟化，而虚拟化列表的渲染回调只拿得到 `&mut App`，
/// 拿不到 `Context<HebbianApp>`，也不方便在布局阶段回读 entity。所以把这几样
/// 在建元素时先快照一份传进去，回调里只读它。
pub(crate) struct RenderCtx {
    pub theme: crate::theme::Theme,
    pub entity: gpui::WeakEntity<HebbianApp>,
    pub expanded_parts: std::collections::HashSet<String>,
    pub expanded_calls: std::collections::HashSet<String>,
    pub flash_tool_call: Option<String>,
}

impl RenderCtx {
    pub fn snapshot(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> Self {
        Self {
            theme: app.theme.clone(),
            entity: cx.entity().downgrade(),
            expanded_parts: app.state.expanded_parts.clone(),
            expanded_calls: app.state.expanded_calls.clone(),
            flash_tool_call: app.state.flash_tool_call.clone(),
        }
    }

    fn call_expanded(&self, key: &str, call_id: Option<&str>) -> bool {
        self.expanded_parts.contains(key)
            || call_id.is_some_and(|id| self.expanded_calls.contains(id))
    }

    fn flashed(&self, call_id: &str) -> bool {
        self.flash_tool_call.as_deref() == Some(call_id)
    }
}

/// 这张卡片是不是展开着。两个来源：用户自己点开的（按 key 记），
/// 以及从「后台任务」面板点名跳过来的（按调用 id 记）。
pub(crate) fn is_call_expanded(app: &HebbianApp, key: &str, call_id: Option<&str>) -> bool {
    app.state.expanded_parts.contains(key)
        || call_id.is_some_and(|id| app.state.expanded_calls.contains(id))
}

/// 工具调用卡片。
///
/// 参数刻意不收 `&HebbianApp` / `&mut Context`，只收算好的 `expanded` / `flashed` 和一个
/// 弱引用：这样**聊天区和导入预览能共用同一份实现**。预览跑在虚拟化列表的渲染回调里，
/// 那里只拿得到 `&mut App`，收 `Context` 的版本进不去；而预览若另写一份卡片，
/// 两处样式迟早会走偏。
pub(crate) fn tool_card(
    theme: &crate::theme::Theme,
    entity: &gpui::WeakEntity<HebbianApp>,
    key: &str,
    expanded: bool,
    flashed: bool,
    name: &str,
    input: &serde_json::Value,
    result: Option<&str>,
    duration_ms: Option<u64>,
    is_error: bool,
) -> impl IntoElement {
    let theme = theme.clone();
    let key_owned = key.to_string();
    // 卡片头是三段：工具名（粗）+ 这次在做什么 + 作用对象（等宽）。
    let description = crate::tool_label::call_description(name, input);
    let summary = crate::tool_label::call_summary(name, input);
    let duration = match duration_ms {
        Some(ms) if ms >= 1000 => format!("{:.1}s", ms as f64 / 1000.),
        Some(ms) => format!("{ms}ms"),
        None => String::new(),
    };
    // **只有展开时才去序列化入参 / 拷贝结果**。收起态的卡片只显示一行标题，
    // 却照样把每次调用的 JSON 全 pretty-print 一遍、把结果全文再拷一份——
    // 一段几百次工具调用的对话（读文件、跑命令，结果动辄几十 KB）光这一步
    // 每帧就要搬几百 MB，实测能让预览弹窗几十秒出不来、CPU 一直满着。
    let (args, result_text) = if expanded {
        (
            serde_json::to_string_pretty(input).unwrap_or_default(),
            result.unwrap_or("").to_string(),
        )
    } else {
        (String::new(), String::new())
    };

    // 从「后台任务」面板跳过来的那张卡片描一圈重色——长对话滚过去之后，
    // 不给个落点用户根本不知道该看哪一行。
    v_flex()
        .rounded(px(5.))
        .border_1()
        .border_color(if is_error {
            theme.danger
        } else if flashed {
            theme.accent
        } else {
            theme.line
        })
        .when(flashed, |this| this.bg(theme.accent_soft))
        .when(!flashed, |this| this.bg(theme.card))
        .child(
            h_flex()
                .id(gpui::SharedString::from(format!("tool-{key}")))
                .h(px(30.))
                .px(px(8.))
                .gap(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent_soft))
                .child(
                    if is_error {
                        Icon::Ban.el(px(12.), theme.danger)
                    } else {
                        Icon::CircleCheck.el(px(12.), theme.green)
                    },
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight(600.))
                        .text_color(theme.text)
                        .child(name.to_string()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted)
                        .child(description),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family("monospace")
                        .text_size(px(11.))
                        .text_color(theme.text)
                        .child(summary),
                )
                .child(div().text_size(px(11.)).text_color(theme.faint).child(duration))
                .child(
                    if expanded {
                        Icon::ChevronDown.el(px(12.), theme.faint)
                    } else {
                        Icon::ChevronRight.el(px(12.), theme.faint)
                    },
                )
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx: &mut gpui::App| {
                        let Some(app) = entity.upgrade() else { return };
                        let key = key_owned.clone();
                        app.update(cx, |this, cx| {
                            if !this.state.expanded_parts.remove(&key) {
                                this.state.expanded_parts.insert(key.clone());
                            }
                            // 不主动去 splice / reset 那条：两者都会动到列表的锚点，
                            // 表现为「点开一张卡片，列表咣一下跳走」。视图重绘时
                            // 渲染回调会被重新调用，高度自然就重量了。
                            cx.notify();
                        });
                    }
                }),
        )
        .when(expanded, |this| {
            this.child(
                v_flex()
                    .px(px(8.))
                    .pb(px(8.))
                    .gap(px(6.))
                    .border_t_1()
                    .border_color(theme.line)
                    .child(mono_block(&theme, "入参", args.clone()))
                    .when(!result_text.is_empty(), |this| {
                        this.child(mono_block(&theme, "结果", result_text.clone()))
                    }),
            )
        })
}

/// 等宽小块：工具入参 / 结果都用它，超长时内部滚动而不是把气泡撑爆。
fn mono_block(
    theme: &crate::theme::Theme,
    label: &'static str,
    body: String,
) -> impl IntoElement {
    v_flex()
        .gap(px(3.))
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme.faint)
                .child(label),
        )
        .child(
            div()
                .id(gpui::SharedString::from(format!("mono-{label}")))
                .max_h(px(260.))
                .overflow_y_scroll()
                .p(px(8.))
                .rounded(px(6.))
                .bg(theme.right_bg_top)
                .font_family("monospace")
                .text_size(px(11.))
                .line_height(px(17.))
                .text_color(theme.text)
                .child(body),
        )
}

/// 消息左侧的头像位：用户是圆形底色块，助手是小星标。
fn avatar(theme: &crate::theme::Theme, is_user: bool) -> impl IntoElement {
    let theme = theme.clone();
    div()
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .when(is_user, |this| {
            this.bg(theme.accent_soft).text_color(theme.accent)
        })
        .when(!is_user, |this| this.text_color(theme.accent))
        .child(if is_user {
            Icon::User.el(px(14.), theme.accent)
        } else {
            Icon::Sparkles.el(px(15.), theme.accent)
        })
}

/// 气泡底部那行「时间 · 耗时 + 复制 / 分叉 / 编辑 / 重新生成」。
///
/// 与原前端逐项对齐（`MessageBubble.tsx`）：整行 **hover 才显出**（原来是
/// `opacity-0 group-hover:opacity-100`，我之前一直常显）；时间当天只显时分；
/// **重新生成对用户消息也有**（用户那条是「用同样内容重跑」，我之前只给了助手）。
fn meta_row(ctx: &RenderCtx, message: &Message, is_user: bool) -> impl IntoElement {
    let theme = ctx.theme.clone();
    let entity = ctx.entity.clone();
    let group = gpui::SharedString::from(format!("msg-{}", message.id));

    let action = |id: gpui::SharedString,
                  icon: Icon,
                  label: &'static str,
                  theme: crate::theme::Theme| {
        h_flex()
            .id(id)
            .px(px(6.))
            .py(px(4.))
            .gap(px(4.))
            .rounded(px(4.))
            .text_size(px(10.))
            .text_color(theme.faint)
            .cursor_pointer()
            .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
            .child(icon.el(px(13.), theme.faint))
            .when(!label.is_empty(), |this| this.child(label))
    };

    let stamp = crate::state::format_message_time(message.created_at, now_ms());
    let content = message.content.clone();
    let fork_id = message.id.clone();
    let edit_id = message.id.clone();
    let regen_id = message.id.clone();

    h_flex()
        .mt(px(8.))
        .gap(px(2.))
        .items_center()
        // 整行常态隐形，hover 到这条消息才显出。
        .invisible()
        .group_hover(group, |this| this.visible())
        .child(
            div()
                .px(px(6.))
                .text_size(px(10.))
                .text_color(theme.faint)
                .child(stamp),
        )
        .when_some(message.run_duration_ms, |this, ms| {
            this.child(
                div()
                    .px(px(4.))
                    .text_size(px(10.))
                    .text_color(theme.faint)
                    .child(format!("· {:.1}s", ms as f64 / 1000.)),
            )
        })
        .child(
            action(
                gpui::SharedString::from(format!("copy-{}", message.id)),
                Icon::Copy,
                "",
                theme.clone(),
            )
            .on_click(move |_, _, cx: &mut gpui::App| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(content.clone()));
            }),
        )
        .child(
            action(
                gpui::SharedString::from(format!("fork-{}", message.id)),
                Icon::GitBranch,
                "分叉",
                theme.clone(),
            )
            .on_click({
                let entity = entity.clone();
                move |_, _, cx: &mut gpui::App| {
                    let Some(app) = entity.upgrade() else { return };
                    app.update(cx, |this, _| {
                if let Some(sid) = this.state.current_id().map(str::to_string) {
                    this.state.core.fork_session(sid, fork_id.clone());
                }
                    });
                }
            }),
        )
        .when(is_user, |row| {
            row.child(
                action(
                    gpui::SharedString::from(format!("edit-{}", message.id)),
                    Icon::Pencil,
                    "编辑",
                    theme.clone(),
                )
                .on_click({
                let entity = entity.clone();
                move |_, _, cx: &mut gpui::App| {
                    let Some(app) = entity.upgrade() else { return };
                    app.update(cx, |this, _| {
                    if let Some(sid) = this.state.current_id().map(str::to_string) {
                        this.state.core.edit_message(sid, edit_id.clone());
                    }
                    });
                }
            }),
            )
        })
        .child(
            action(
                gpui::SharedString::from(format!("regen-{}", message.id)),
                Icon::RefreshCw,
                "重新生成",
                theme,
            )
            .on_click({
                let entity = entity.clone();
                move |_, _, cx: &mut gpui::App| {
                    let Some(app) = entity.upgrade() else { return };
                    app.update(cx, |this, _| {
                if let Some(sid) = this.state.current_id().map(str::to_string) {
                    this.state.core.regenerate(sid, regen_id.clone(), is_user);
                }
                    });
                }
            }),
        )
}


/// 流式进行中的助手气泡。
fn streaming_bubble(
    ctx: &RenderCtx,
    turn: &StreamingTurn,
    window: &mut Window,
    cx: &mut gpui::App,
) -> impl IntoElement {
    let theme = ctx.theme.clone();
    let mut body = v_flex()
        .flex_1()
        .min_w_0()
        .px(px(15.))
        .py(px(13.))
        .rounded(px(18.))
        .border_1()
        .border_color(theme.line)
        .bg(theme.card_strong);

    if !turn.reasoning.is_empty() {
        body = body.child(
            div()
                .mb(px(8.))
                .pl(px(12.))
                .border_l_2()
                .border_color(theme.line)
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(turn.reasoning.clone()),
        );
    }
    if !turn.text.is_empty() {
        body = body.child(markdown(
            "msg-streaming".to_string(),
            turn.text.clone(),
            &theme,
            window,
            cx,
        ));
    }
    if !turn.tools.is_empty() {
        let mut strip = h_flex().flex_wrap().gap(px(7.)).mt(px(10.));
        for tool in &turn.tools {
            strip = strip.child(tool_chip(&theme, &tool.name, tool.done, tool.is_error));
        }
        body = body.child(strip);
    }

    h_flex().items_start().gap(px(12.)).mb(px(20.)).child(body)
}

fn tool_chip(theme: &crate::theme::Theme, name: &str, done: bool, is_error: bool) -> impl IntoElement {
    let theme = theme.clone();
    h_flex()
        .gap(px(5.))
        .px(px(8.))
        .py(px(5.))
        .rounded(px(999.))
        .border_1()
        .border_color(theme.line)
        .bg(theme.card)
        .text_size(px(11.))
        .text_color(if is_error { theme.danger } else { theme.muted })
        .child(
            (if is_error {
                Icon::Ban
            } else if done {
                Icon::CircleCheck
            } else {
                Icon::LoaderCircle
            })
            .el(px(11.), if is_error { theme.danger } else { theme.muted }),
        )
        .child(name.to_string())
}

/// 工具审批卡片（架构 §4.5 HITL）。
///
/// 按钮集与原前端 `PermissionApprovalPopup` 的主行一致：**允许此次 / 拒绝并说明 / 拒绝**。
///
/// **刻意不放「本对话允许」那种一键记忆按钮**：原 UI 的「记住」走的是二级区——
/// 先让用户勾选具体 pattern 再选 scope，就是为了避免在 Bash 上按工具名整体放行
/// （那样后续 `rm -rf /` 也会免审批）。这里没实现 pattern 多选，就不提供退化成
/// 工具名级的快捷放行，宁可少一个按钮也不放宽审批面。
fn approval_card(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    pending: &crate::state::PendingApproval,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let session_id = app.state.current_id().unwrap_or_default().to_string();
    // 「记住」区必须在下面那个 button 闭包之前算完：闭包会捕获 cx，
    // 之后再对 cx 可变借用就冲突了。
    let remember = remember_section(app, pending, cx);

    let button = |label: &'static str,
                  decision: protocol::ApprovalDecision,
                  primary: bool,
                  danger: bool,
                  theme: crate::theme::Theme,
                  session_id: String| {
        h_flex()
            .id(label)
            .h(px(32.))
            .px(px(12.))
            .gap(px(6.))
            .rounded(px(8.))
            .text_size(px(13.))
            .cursor_pointer()
            .when(primary, |this| {
                this.bg(theme.accent).text_color(gpui::white())
            })
            .when(danger, |this| {
                this.bg(crate::theme::with_alpha(theme.danger, 0.12))
                    .text_color(theme.danger)
            })
            .when(!primary && !danger, |this| this.text_color(theme.muted))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(pending) = this.state.take_approval(&session_id) {
                    this.state.core.resolve_approval(
                        session_id.clone(),
                        pending.request_id,
                        decision.clone(),
                    );
                }
                this.deny_feedback_open = false;
                cx.notify();
            }))
    };

    v_flex()
        .mx_auto()
        .mb(px(20.))
        .w_full()
        .max_w(px(720.))
        .p(px(14.))
        .gap(px(10.))
        .rounded(px(16.))
        .border_1()
        .border_color(theme.amber)
        .bg(theme.card_strong)
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight(650.))
                .child(format!("需要确认：{}", pending.tool_name)),
        )
        .children(remember)
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(pending.summary.clone()),
        )
        // 「拒绝并说明」展开后先填理由再提交——这段话会回灌给模型。
        .when(app.deny_feedback_open, |this| {
            this.child(
                div()
                    .p(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.line)
                    .text_size(px(12.))
                    .child(gpui_component::input::Input::new(&app.deny_feedback)),
            )
        })
        .child(
            h_flex()
                .gap(px(8.))
                .when(!app.deny_feedback_open, |row| {
                    row.child(button(
                        "允许此次",
                        protocol::ApprovalDecision::AllowOnce,
                        true,
                        false,
                        theme.clone(),
                        session_id.clone(),
                    ))
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .id("deny-with-feedback")
                            .h(px(32.))
                            .px(px(12.))
                            .rounded(px(8.))
                            .text_size(px(13.))
                            .text_color(theme.muted)
                            .cursor_pointer()
                            .hover(|this| this.bg(theme.accent_soft))
                            .child("拒绝并说明")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.deny_feedback_open = true;
                                this.deny_feedback
                                    .update(cx, |state, cx| state.focus(window, cx));
                                cx.notify();
                            })),
                    )
                    .child(button(
                        "拒绝",
                        protocol::ApprovalDecision::Deny,
                        false,
                        true,
                        theme.clone(),
                        session_id.clone(),
                    ))
                })
                .when(app.deny_feedback_open, |row| {
                    let sid = session_id.clone();
                    let theme2 = theme.clone();
                    row.child(
                        h_flex()
                            .id("cancel-feedback")
                            .h(px(32.))
                            .px(px(12.))
                            .rounded(px(8.))
                            .text_size(px(13.))
                            .text_color(theme.muted)
                            .cursor_pointer()
                            .child("取消")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.deny_feedback_open = false;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .id("submit-feedback")
                            .h(px(32.))
                            .px(px(12.))
                            .rounded(px(8.))
                            .bg(crate::theme::with_alpha(theme2.danger, 0.12))
                            .text_color(theme2.danger)
                            .text_size(px(13.))
                            .cursor_pointer()
                            .child("提交拒绝")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let feedback =
                                    this.deny_feedback.read(cx).value().trim().to_string();
                                if let Some(pending) = this.state.take_approval(&sid) {
                                    this.state.core.resolve_approval(
                                        sid.clone(),
                                        pending.request_id,
                                        protocol::ApprovalDecision::DenyWithFeedback {
                                            feedback,
                                        },
                                    );
                                }
                                this.deny_feedback_open = false;
                                cx.notify();
                            })),
                    )
                }),
        )
}

/// 审批卡片的「记住」二级区：勾选要记的 pattern，再选生效范围写规则。
///
/// **候选 pattern 与它们的状态全部由 core 随事件发来**（`segments`），UI 不自己解析
/// 命令——段级判定规则在 core，前端再推一遍必然走样。各状态的处理与原前端一致：
/// 只读段灰显、已白名单段划掉、**不可记忆段（rm/dd 之类）红色且不可勾选**。
/// core 说 `refuse_remember` 时整个区不出现——不是灰掉，是根本不给。
fn remember_section(
    app: &HebbianApp,
    pending: &crate::state::PendingApproval,
    cx: &mut Context<HebbianApp>,
) -> Option<impl IntoElement> {
    use protocol::ApprovalSegmentStatus as St;
    if pending.refuse_remember || pending.segments.is_empty() {
        return None;
    }
    let theme = app.theme.clone();
    let session_id = app.state.current_id().unwrap_or_default().to_string();

    let mut list = v_flex().gap(px(4.));
    for seg in &pending.segments {
        let fp = seg.fingerprint.clone();
        let picked = app.state.approval_picked.contains(&fp);
        let selectable = matches!(seg.status, St::NeedsApproval);
        let (color, note) = match seg.status {
            St::Readonly => (theme.faint, "只读，免审"),
            St::Whitelisted => (theme.green, "已允许"),
            St::Unmemorable => (theme.danger, "每次都要确认，不能记住"),
            St::NeedsApproval => (theme.text, ""),
        };
        let fp_click = fp.clone();
        list = list.child(
            h_flex()
                .id(gpui::SharedString::from(format!("seg-{fp}")))
                .gap(px(8.))
                .py(px(3.))
                .items_center()
                .when(selectable, |this| {
                    this.cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(i) =
                                this.state.approval_picked.iter().position(|p| p == &fp_click)
                            {
                                this.state.approval_picked.remove(i);
                            } else {
                                this.state.approval_picked.push(fp_click.clone());
                            }
                            cx.notify();
                        }))
                })
                .child(
                    div()
                        .size(px(12.))
                        .flex_none()
                        .rounded(px(3.))
                        .border_1()
                        .border_color(if selectable { theme.line } else { theme.faint })
                        .when(picked && selectable, |this| this.bg(theme.accent)),
                )
                .child(
                    div()
                        .font_family("monospace")
                        .text_size(px(11.))
                        .text_color(color)
                        .child(fp.clone()),
                )
                .when(!note.is_empty(), |this| {
                    this.child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.faint)
                            .child(note),
                    )
                }),
        );
    }

    let scope_button = |label: &'static str,
                        scope: protocol::PermissionScope,
                        theme: crate::theme::Theme,
                        session_id: String| {
        h_flex()
            .id(label)
            .h(px(26.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme.line)
            .text_size(px(11.))
            .text_color(theme.muted)
            .cursor_pointer()
            .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                let mut picked = this.state.approval_picked.clone();
                if picked.is_empty() {
                    this.state.error = Some("先勾上要记住的那几段".to_string());
                    cx.notify();
                    return;
                }
                // 协议是「第一条进 pattern，其余进 extra_patterns」。
                let first = picked.remove(0);
                if let Some(p) = this.state.take_approval(&session_id) {
                    this.state.core.resolve_approval(
                        session_id.clone(),
                        p.request_id,
                        protocol::ApprovalDecision::AllowAndRemember {
                            scope,
                            pattern: Some(first),
                            extra_patterns: picked,
                        },
                    );
                }
                this.state.approval_picked.clear();
                cx.notify();
            }))
    };

    Some(
        v_flex()
            .gap(px(8.))
            .p(px(10.))
            .rounded(px(10.))
            .bg(theme.right_bg_top)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.muted)
                    .child("以后不用再问我："),
            )
            .child(list)
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(scope_button(
                        "本对话",
                        protocol::PermissionScope::Session,
                        theme.clone(),
                        session_id.clone(),
                    ))
                    .child(scope_button(
                        "本项目",
                        protocol::PermissionScope::Project,
                        theme.clone(),
                        session_id.clone(),
                    ))
                    .child(scope_button(
                        "所有对话",
                        protocol::PermissionScope::Global,
                        theme,
                        session_id,
                    )),
            ),
    )
}

/// 模型提问卡片。
///
/// 按原前端 `UserQuestionPopup` 对齐：单选点一下即回答；多选勾完点「提交」；
/// 另有「其他回答」自由输入与「取消」（取消 = `UserAnswer::Cancelled`，
/// 让模型知道用户主动放弃了这轮提问，而不是干等）。
fn question_card(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    question: &crate::state::PendingQuestion,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let session_id = app.state.current_id().unwrap_or_default().to_string();
    let multi = question.multi;

    let mut options = v_flex().gap(px(6.));
    for option in &question.options {
        let label = option.label.clone();
        let sid = session_id.clone();
        let picked = app.question_picked.contains(&label);
        let label_for_click = label.clone();
        options = options.child(
            h_flex()
                .id(gpui::SharedString::from(format!("opt-{label}")))
                .px(px(12.))
                .py(px(8.))
                .gap(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(if picked { theme.accent } else { theme.line })
                .when(picked, |this| this.bg(theme.accent_soft))
                .text_size(px(12.))
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent_soft))
                // 多选时给一个勾选指示，否则看不出已选了哪些。
                .when(multi, |this| {
                    this.child(
                        div()
                            .size(px(12.))
                            .flex_none()
                            .rounded(px(3.))
                            .border_1()
                            .border_color(if picked { theme.accent } else { theme.line })
                            .when(picked, |this| this.bg(theme.accent)),
                    )
                })
                .child(label.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if multi {
                        // 多选只切换勾选状态，等用户点「提交」再一次性回答。
                        if let Some(i) =
                            this.question_picked.iter().position(|l| l == &label_for_click)
                        {
                            this.question_picked.remove(i);
                        } else {
                            this.question_picked.push(label_for_click.clone());
                        }
                        cx.notify();
                        return;
                    }
                    if let Some(pending) = this.state.take_question(&sid) {
                        this.state.core.answer_question(
                            sid.clone(),
                            pending.request_id,
                            protocol::UserAnswer::Selected {
                                label: label_for_click.clone(),
                            },
                        );
                    }
                    this.question_picked.clear();
                    cx.notify();
                })),
        );
    }

    let sid_submit = session_id.clone();
    let sid_custom = session_id.clone();
    let sid_cancel = session_id.clone();

    v_flex()
        .mx_auto()
        .mb(px(20.))
        .w_full()
        .max_w(px(720.))
        .p(px(14.))
        .gap(px(10.))
        .rounded(px(16.))
        .border_1()
        .border_color(theme.accent)
        .bg(theme.card_strong)
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight(650.))
                .child(question.question.clone()),
        )
        .child(options)
        .child(
            h_flex()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.line)
                        .text_size(px(12.))
                        .child(gpui_component::input::Input::new(&app.question_custom)),
                )
                .child(
                    h_flex()
                        .id("answer-custom")
                        .h(px(30.))
                        .px(px(12.))
                        .rounded(px(8.))
                        .bg(theme.accent)
                        .text_color(gpui::white())
                        .text_size(px(12.))
                        .cursor_pointer()
                        .child("发送")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let text = this.question_custom.read(cx).value().trim().to_string();
                            if text.is_empty() {
                                return;
                            }
                            if let Some(pending) = this.state.take_question(&sid_custom) {
                                this.state.core.answer_question(
                                    sid_custom.clone(),
                                    pending.request_id,
                                    protocol::UserAnswer::Custom { text },
                                );
                            }
                            this.question_custom
                                .update(cx, |state, cx| state.set_value("", window, cx));
                            this.question_picked.clear();
                            cx.notify();
                        })),
                ),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .child(
                    h_flex()
                        .id("answer-cancel")
                        .h(px(30.))
                        .px(px(12.))
                        .rounded(px(8.))
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.accent_soft))
                        .child("取消")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(pending) = this.state.take_question(&sid_cancel) {
                                this.state.core.answer_question(
                                    sid_cancel.clone(),
                                    pending.request_id,
                                    protocol::UserAnswer::Cancelled,
                                );
                            }
                            this.question_picked.clear();
                            cx.notify();
                        })),
                )
                .child(div().flex_1())
                .when(multi, |row| {
                    row.child(
                        h_flex()
                            .id("answer-submit")
                            .h(px(30.))
                            .px(px(14.))
                            .rounded(px(8.))
                            .bg(theme.accent)
                            .text_color(gpui::white())
                            .text_size(px(12.))
                            .cursor_pointer()
                            .child(format!("提交（已选 {}）", app.question_picked.len()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.question_picked.is_empty() {
                                    return;
                                }
                                let labels = this.question_picked.clone();
                                if let Some(pending) = this.state.take_question(&sid_submit) {
                                    this.state.core.answer_question(
                                        sid_submit.clone(),
                                        pending.request_id,
                                        protocol::UserAnswer::SelectedMulti { labels },
                                    );
                                }
                                this.question_picked.clear();
                                cx.notify();
                            })),
                    )
                }),
        )
}

/// `.dsp-composer`：输入框 + 底部工具条。
fn composer(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();
    let running = app.state.is_running();
    let model = app
        .state
        .current
        .as_ref()
        .map(|s| s.model.clone())
        .unwrap_or_default();

    v_flex()
        .flex_none()
        .px(px(72.))
        .pb(px(28.))
        .items_center()
        .child(
            v_flex()
                .w_full()
                .max_w(px(682.))
                .rounded(px(18.))
                .border_1()
                .border_color(theme.card_line)
                .bg(theme.card_strong)
                .shadow(shadow_lifted(gpui::rgba(0x36475c17).into()))
                .children(project_chip(app))
                .child(
                    div()
                        .px(px(16.))
                        .pt(px(14.))
                        .pb(px(8.))
                        .min_h(px(42.))
                        .text_size(px(13.))
                        .text_color(theme.input_text)
                        .child(
                            gpui_component::input::Input::new(&app.composer)
                                .appearance(false),
                        ),
                )
                .child(toolbar(app, cx, running, model)),
        )
        .child(info_row(app, cx))
}

/// 输入框工具条上的一个小按钮。
fn composer_tool(
    theme: &crate::theme::Theme,
    icon: Icon,
    id: &'static str,
) -> gpui::Stateful<gpui::Div> {
    let theme = theme.clone();
    h_flex()
        .id(id)
        .h(px(26.))
        .px(px(4.))
        .gap(px(5.))
        .rounded(px(6.))
        .text_color(theme.faint)
        .cursor_pointer()
        .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
        .child(icon.el(px(14.), theme.faint))
}

/// `//` 命令面板。内置控制命令（架构 §8.2 表 A）+ 已启用的 skill（表 B）。
/// 选中后把命令文本填进输入框，让用户补参数再发——与原前端一致。
fn slash_menu(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> Option<impl IntoElement> {
    if !app.slash_open {
        return None;
    }
    let theme = app.theme.clone();

    /// (命令, 说明)。与架构 §8.2 表 A 登记的一致。
    const BUILTINS: [(&str, &str); 3] = [
        ("//hands-off", "放手跑：本对话内自动放行，跑完再回来看"),
        ("//run-mode", "切换这个对话的运行模式"),
        ("//goal", "给这个对话挂一个完成条件，没达成就自动接着跑"),
    ];

    let mut list = v_flex()
        .id("slash-list")
        .max_h(px(320.))
        .overflow_y_scroll();

    list = list.child(section_title(&theme, "命令"));
    for (cmd, desc) in BUILTINS {
        list = list.child(slash_row(app, cx, cmd.to_string(), desc.to_string()));
    }

    let enabled: Vec<_> = app.state.skills.iter().filter(|s| s.enabled).collect();
    if !enabled.is_empty() {
        list = list.child(section_title(&theme, "Skills"));
        for skill in enabled {
            let name = skill.alias.clone().unwrap_or_else(|| skill.name.clone());
            list = list.child(slash_row(
                app,
                cx,
                format!("//{name}"),
                skill.description.clone(),
            ));
        }
    }

    Some(
        v_flex()
            .absolute()
            .bottom(px(34.))
            .left(px(0.))
            .w(px(320.))
            .rounded(px(14.))
            .border_1()
            .border_color(theme.card_line)
            .bg(theme.card_strong)
            .shadow(shadow_lifted(gpui::rgba(0x2d3d5324).into()))
            .child(list),
    )
}

fn section_title(theme: &crate::theme::Theme, label: &'static str) -> impl IntoElement {
    div()
        .px(px(12.))
        .py(px(6.))
        .text_size(px(10.))
        .text_color(theme.faint)
        .child(label)
}

fn slash_row(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    cmd: String,
    desc: String,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let fill = cmd.clone();
    v_flex()
        .id(gpui::SharedString::from(format!("slash-{cmd}")))
        .px(px(12.))
        .py(px(6.))
        .cursor_pointer()
        .hover(|this| this.bg(theme.accent_soft))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.text)
                .child(cmd),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.muted)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(desc),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            // 只填命令不直接发：多数命令还要补参数。
            this.composer.update(cx, |state, cx| {
                state.set_value(format!("{fill} "), window, cx);
            });
            this.slash_open = false;
            cx.notify();
        }))
}

/// 模型选择器弹窗。对应 `.model-picker-popup`：256px 宽、18px 圆角、
/// 供应商一行 38px，展开后列出该供应商的模型（40px 一行）。
fn model_picker(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> Option<impl IntoElement> {
    if !app.model_picker_open {
        return None;
    }
    let theme = app.theme.clone();
    let current_model = app.state.current.as_ref().map(|s| s.model.clone());
    let current_provider = app.state.current.as_ref().map(|s| s.provider_id.clone());

    let mut list = v_flex().max_h(px(420.)).id("provider-list").overflow_y_scroll();

    if app.state.providers.is_empty() {
        list = list.child(
            div()
                .p(px(12.))
                .text_size(px(12.))
                .text_color(theme.muted)
                .child("还没有配置模型供应商"),
        );
    }

    for provider in &app.state.providers {
        let expanded = app.model_picker_provider.as_deref() == Some(provider.id.as_str());
        let selected = current_provider.as_deref() == Some(provider.id.as_str());
        let pid = provider.id.clone();

        list = list.child(
            h_flex()
                .id(gpui::SharedString::from(format!("prov-{}", provider.id)))
                .h(px(38.))
                .px(px(12.))
                .gap(px(6.))
                .justify_between()
                .text_size(px(12.))
                .text_color(if selected { theme.accent } else { theme.muted })
                .cursor_pointer()
                .when(selected, |this| this.bg(theme.accent_soft))
                .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
                .child(div().overflow_hidden().text_ellipsis().child(provider.name.clone()))
                .child(
                    if expanded {
                        Icon::ChevronDown.el(px(12.), theme.faint)
                    } else {
                        Icon::ChevronRight.el(px(12.), theme.faint)
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model_picker_provider = if this.model_picker_provider.as_deref()
                        == Some(pid.as_str())
                    {
                        None
                    } else {
                        Some(pid.clone())
                    };
                    cx.notify();
                })),
        );

        if !expanded {
            continue;
        }
        for model in provider.models.iter() {
            let is_current = current_model.as_deref() == Some(model.as_str());
            let label = model.clone();
            let provider_id = provider.id.clone();
            list = list.child(
                h_flex()
                    .id(gpui::SharedString::from(format!("model-{}-{}", provider.id, model)))
                    .min_h(px(40.))
                    .px(px(12.))
                    .pl(px(24.))
                    .text_size(px(12.))
                    .text_color(if is_current { theme.accent } else { theme.text })
                    .cursor_pointer()
                    .when(is_current, |this| this.bg(theme.accent_soft))
                    .hover(|this| this.bg(theme.accent_soft))
                    .child(div().overflow_hidden().text_ellipsis().child(label.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(session_id) = this.state.current_id().map(str::to_string)
                        else {
                            return;
                        };
                        this.state.core.switch_model(
                            session_id,
                            provider_id.clone(),
                            label.clone(),
                        );
                        this.model_picker_open = false;
                        cx.notify();
                    })),
            );
        }
    }

    Some(
        v_flex()
            .absolute()
            .bottom(px(34.))
            .left(px(0.))
            .w(px(256.))
            .rounded(px(18.))
            .bg(theme.card_strong)
            .border_1()
            .border_color(theme.card_line)
            .shadow(shadow_lifted(gpui::rgba(0x2d3d5324).into()))
            .child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight(650.))
                    .text_color(theme.muted)
                    .bg(theme.right_bg_top)
                    .child("选择模型"),
            )
            .child(list)
            .children(thinking_toggle(app, cx)),
    )
}

/// 模型菜单底部的「思考」开关。原 UI 把它放在这儿（`.model-picker-selected-controls`）
/// 而不是强度 pill 上——pill 关掉后自己就不可点了，开关必须在别处才回得来。
fn thinking_toggle(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
) -> Option<impl IntoElement> {
    let theme = app.theme.clone();
    let session = app.state.current.as_ref()?;
    let provider_kind = app
        .state
        .providers
        .iter()
        .find(|p| p.id == session.provider_id)
        .map(|p| format!("{:?}", p.kind).to_ascii_lowercase())
        .unwrap_or_default();
    if !common::reasoning::model_supports_reasoning(&provider_kind, &session.model) {
        return None;
    }
    let reasoning = session.reasoning.clone().unwrap_or_default();
    let on = reasoning.enabled.unwrap_or(true);

    Some(
        h_flex()
            .px(px(12.))
            .py(px(8.))
            .justify_between()
            .border_t_1()
            .border_color(theme.line)
            .bg(theme.right_bg_top)
            .text_size(px(12.))
            .child("思考")
            .child(
                gpui_component::switch::Switch::new("thinking-toggle")
                    .checked(on)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        let Some(id) = this.state.current_id().map(str::to_string) else {
                            return;
                        };
                        let mut next = reasoning.clone();
                        next.enabled = Some(*checked);
                        this.state.core.set_reasoning(id, Some(next));
                        cx.notify();
                    })),
            ),
    )
}

/// 输入框顶部的项目胶囊：当前对话绑在哪个项目 / 目录上。
/// 没绑目录就不显示——原前端也是「有才画」，不占位。
fn project_chip(app: &HebbianApp) -> Option<impl IntoElement> {
    let theme = app.theme.clone();
    let session = app.state.current.as_ref()?;
    let name = session
        .project_id
        .as_ref()
        .and_then(|pid| app.state.projects.iter().find(|p| &p.id == pid))
        .map(|p| p.name.clone())
        .or_else(|| {
            session
                .workdir
                .as_ref()?
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
        })?;

    Some(
        h_flex().px(px(12.)).pt(px(10.)).child(
            h_flex()
                .gap(px(5.))
                .px(px(8.))
                .py(px(3.))
                .rounded(px(7.))
                .bg(theme.accent_soft)
                .text_size(px(11.))
                .text_color(theme.accent)
                .child(Icon::Folder.el(px(12.), theme.accent))
                .child(name),
        ),
    )
}

/// `.dsp-composer-info`：输入框下面那行「全速模式 / 极高 / cache·ctx」。
/// 三个都是当前 run 的只读指示，点开的下拉还没接上。
fn info_row(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    let theme = app.theme.clone();

    h_flex()
        .w_full()
        .max_w(px(682.))
        .mt(px(6.))
        .items_center()
        .text_size(px(11.))
        .text_color(theme.muted)
        .child(run_mode_chip(app, cx))
        .children(reasoning_pill(app, cx))
        .child(div().flex_1())
        .child(context_meter(app))
}

/// 思考强度 pill。**只在当前模型确实启用了推理时才显示**——
/// 不支持推理的模型上原前端是隐藏这个 pill 的，硬画一个「极高」是假状态。
/// 档位名逐字取自原前端 `REASONING_EFFORT_LABEL`。
fn reasoning_pill(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
) -> Option<impl IntoElement> {
    use common::ReasoningEffort;
    let theme = app.theme.clone();
    let session = app.state.current.as_ref()?;
    // 隐藏与否只看**模型支不支持推理**（与原前端一致）。thinking 被关掉时
    // 不能隐藏——那样就再也点不开、没法重新打开了；原前端是置灰不可点。
    let provider_kind = app
        .state
        .providers
        .iter()
        .find(|p| p.id == session.provider_id)
        .map(|p| format!("{:?}", p.kind).to_ascii_lowercase())
        .unwrap_or_default();
    if !common::reasoning::model_supports_reasoning(&provider_kind, &session.model) {
        return None;
    }
    let reasoning = session.reasoning.clone().unwrap_or_default();
    let thinking_on = reasoning.enabled.unwrap_or(true);
    let label = match reasoning.effort.unwrap_or_default() {
        ReasoningEffort::Low => "低",
        ReasoningEffort::Medium => "中",
        ReasoningEffort::High => "高",
        ReasoningEffort::Extra => "极高",
        ReasoningEffort::Max => "最高",
    };
    // 档位顺序与原前端菜单一致。`Max` 只有部分模型支持，core 侧会钳到 high，
    // 这里照列——钳不钳是 core 的事，UI 不替它做判断。
    const LEVELS: [(ReasoningEffort, &str); 5] = [
        (ReasoningEffort::Low, "低"),
        (ReasoningEffort::Medium, "中"),
        (ReasoningEffort::High, "高"),
        (ReasoningEffort::Extra, "极高"),
        (ReasoningEffort::Max, "最高"),
    ];
    let current = reasoning.effort.unwrap_or_default();
    let base = reasoning.clone();

    let mut menu = v_flex()
        .absolute()
        .bottom(px(24.))
        .left(px(0.))
        .w(px(120.))
        .p(px(4.))
        .rounded(px(10.))
        .border_1()
        .border_color(theme.card_line)
        .bg(theme.card_strong)
        .shadow(shadow_lifted(gpui::rgba(0x2d3d5324).into()));
    for (effort, name) in LEVELS {
        let active = effort == current;
        let mut next = base.clone();
        next.effort = Some(effort);
        menu = menu.child(
            h_flex()
                .id(name)
                .px(px(8.))
                .py(px(5.))
                .rounded(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.accent_soft).text_color(theme.accent)
                })
                .hover(|this| this.bg(theme.accent_soft))
                .child(name)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(id) = this.state.current_id().map(str::to_string) {
                        this.state.core.set_reasoning(id, Some(next.clone()));
                    }
                    this.reasoning_open = false;
                    cx.notify();
                })),
        );
    }

    Some(
        div()
            .relative()
            .child(
                h_flex()
                    .id("reasoning-pill")
                    .gap(px(4.))
                    .pr(px(8.))
                    .min_h(px(20.))
                    // thinking 关掉时置灰且不可点，鼠标移上去也没有反馈——
                    // 与原前端 `disabled:opacity-40 disabled:pointer-events-none` 一致。
                    .when(thinking_on, |this| {
                        this.cursor_pointer()
                            .hover(|this| this.text_color(theme.text))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reasoning_open = !this.reasoning_open;
                                cx.notify();
                            }))
                    })
                    .when(!thinking_on, |this| this.opacity(0.4))
                    .child(Icon::Gauge.el(px(12.), theme.muted))
                    .child(label)
                    .child(Icon::ChevronDown.el(px(10.), theme.faint)),
            )
            .when(app.reasoning_open && thinking_on, |this| this.child(menu)),
    )
}

/// 输入框右下角：上下文占用环 + `cache x% / ctx y%`。
/// 算不出来（还没跑过 run / 读不到会话）就整块不显示——编个数字充数不如空着。
fn context_meter(app: &HebbianApp) -> impl IntoElement {
    let theme = app.theme.clone();
    let Some((used, budget, cache_pct)) = app.state.context_usage else {
        return div().into_any_element();
    };
    let ctx_pct = if budget > 0 {
        ((used as f64 / budget as f64) * 100.0).min(150.0) as u32
    } else {
        0
    };
    // 与原前端同样的告警配色：≥90% 转红，≥70% 转琥珀。
    let ring_color = if ctx_pct >= 90 {
        theme.danger
    } else if ctx_pct >= 70 {
        theme.amber
    } else {
        theme.accent
    };

    h_flex()
        .gap(px(6.))
        .child(
            div()
                .size(px(18.))
                .rounded_full()
                .border_2()
                .border_color(ring_color),
        )
        .child(format!("cache {cache_pct}% / ctx {ctx_pct}%"))
        .into_any_element()
}

/// 运行模式 chip + 下拉。四个模式的名字与说明逐字取自原前端 `RunModeChip`。
fn run_mode_chip(app: &HebbianApp, cx: &mut Context<HebbianApp>) -> impl IntoElement {
    use agent_core::run_mode::RunMode;
    let theme = app.theme.clone();

    /// (模式, 图标, 名字, 一句说明)
    const MODES: [(RunMode, &str, &str); 4] = [
        (RunMode::Default, "默认", "工作区内改文件直接执行，运行命令前会询问"),
        (RunMode::PlanMode, "计划模式", "只读模式，先规划再动手"),
        (RunMode::AutoMode, "自动模式", "让 AI 自己判断哪些操作可以放行"),
        (
            RunMode::Yolo,
            "全速模式",
            "全部自动执行、不打断，只拦最危险的不可逆操作",
        ),
    ];

    let current = app.state.run_mode;
    let icon_for = |mode: RunMode| match mode {
        RunMode::Default => Icon::Gauge,
        RunMode::PlanMode => Icon::List,
        RunMode::AutoMode => Icon::Sparkles,
        RunMode::Yolo => Icon::Zap,
    };
    let label = MODES
        .iter()
        .find(|(m, _, _)| *m == current)
        .map(|(_, l, _)| *l)
        .unwrap_or("默认");

    let mut menu = v_flex()
        .absolute()
        .bottom(px(24.))
        .left(px(0.))
        .w(px(300.))
        .p(px(4.))
        .rounded(px(12.))
        .border_1()
        .border_color(theme.card_line)
        .bg(theme.card_strong)
        .shadow(shadow_lifted(gpui::rgba(0x2d3d5324).into()));
    for (mode, name, desc) in MODES {
        let active = mode == current;
        menu = menu.child(
            h_flex()
                .id(name)
                .items_start()
                .gap(px(8.))
                .p(px(8.))
                .rounded(px(8.))
                .cursor_pointer()
                .when(active, |this| this.bg(theme.accent_soft))
                .hover(|this| this.bg(theme.accent_soft))
                .child(icon_for(mode).el(
                    px(13.),
                    if active { theme.accent } else { theme.muted },
                ))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(if active { theme.accent } else { theme.text })
                                .child(name),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.muted)
                                .child(desc),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(id) = this.state.current_id().map(str::to_string) {
                        this.state.core.set_run_mode(id, mode);
                    }
                    // 切到自动模式不关面板——原 UI 在这里还要接着调「全自动」开关。
                    if mode != RunMode::AutoMode {
                        this.run_mode_open = false;
                    }
                    cx.notify();
                })),
        );
    }

    div()
        .relative()
        .child(
            h_flex()
                .id("run-mode-chip")
                .gap(px(4.))
                .pr(px(8.))
                .min_h(px(20.))
                .cursor_pointer()
                .hover(|this| this.text_color(theme.text))
                .child(icon_for(current).el(px(12.), theme.muted))
                .child(label)
                .child(Icon::ChevronDown.el(px(10.), theme.faint))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.run_mode_open = !this.run_mode_open;
                    cx.notify();
                })),
        )
        .when(app.run_mode_open, |this| this.child(menu))
}


fn toolbar(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    running: bool,
    model: String,
) -> impl IntoElement {
    let theme = app.theme.clone();


    h_flex()
        .min_h(px(36.))
        .px(px(10.))
        .pb(px(8.))
        .gap(px(10.))
        .justify_between()
        .border_t_1()
        .border_color(theme.line)
        .child(
            h_flex()
                .gap(px(8.))
                .min_w_0()
                .child(
                    composer_tool(&theme, Icon::Plus, "attach").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.pick_attachments(cx);
                        },
                    )),
                )
                .child(
                    div()
                        .relative()
                        .child(composer_tool(&theme, Icon::Slash, "slash").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.slash_open = !this.slash_open;
                                if this.slash_open {
                                    if let Some(workdir) = this
                                        .state
                                        .current
                                        .as_ref()
                                        .and_then(|s| s.workdir.clone())
                                    {
                                        this.state.core.refresh_skills(workdir);
                                    }
                                }
                                cx.notify();
                            }),
                        ))
                        .children(slash_menu(app, cx)),
                )
                .child(
                    div()
                        .relative()
                        .child(
                            h_flex()
                                .id("model-picker")
                                .h(px(28.))
                                .max_w(px(230.))
                                .px(px(8.))
                                .gap(px(4.))
                                .rounded(px(999.))
                                .text_size(px(12.))
                                .text_color(theme.muted)
                                .cursor_pointer()
                                .hover(|this| {
                                    this.bg(gpui::rgba(0x203648_0f)).text_color(theme.text)
                                })
                                .child(div().overflow_hidden().text_ellipsis().child(model))
                                .child(Icon::ChevronDown.el(px(12.), theme.muted))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.model_picker_open = !this.model_picker_open;
                                    if this.model_picker_open {
                                        // 每次展开都重拉一次，供应商可能在别处被改过。
                                        this.state.core.refresh_providers();
                                    }
                                    cx.notify();
                                })),
                        )
                        .children(model_picker(app, cx)),
                ),
        )
        .child(
            h_flex().gap(px(8.)).child(
                div()
                    .id("send")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .text_color(if running { theme.danger } else { theme.muted })
                    .hover(|this| this.bg(theme.accent_soft).text_color(theme.accent))
                    .child(if running {
                        Icon::Square.el(px(14.), theme.danger)
                    } else {
                        Icon::ArrowUp.el(px(14.), theme.muted)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.state.is_running() {
                            if let Some(id) = this.state.current_id().map(str::to_string) {
                                this.state.core.interrupt(id);
                            }
                        } else {
                            this.send_current_input(window, cx);
                        }
                    })),
            ),
        )
}
