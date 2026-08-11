//! 聊天列：header + 消息画布 + 输入区。
//!
//! 对应 `ChatView.tsx` 与 `.dsp-chat-host` / `.dsp-composer*`。空会话时走
//! `DesktopEmptyState`（居中的品牌卡片 + 「你想用 Hebbian 做什么」）。

use agent_core::storage::sessions::{Message, MessagePart, Role};
use gpui::{div, prelude::*, px, AnyElement, Context, Window};

use crate::assets::Icon;
use crate::state::StreamingTurn;
use crate::ui::widgets::{h_flex, shadow_lifted, v_flex};
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
                .child(
                    div()
                        .max_w(px(260.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight(500.))
                        .child(title),
                )
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
            div()
                .text_size(px(11.))
                .text_color(theme.faint)
                .child(session_id),
        )
}

/// 消息画布。没有会话时显示欢迎页。
fn canvas(
    app: &HebbianApp,
    window: &mut Window,
    cx: &mut Context<HebbianApp>,
) -> AnyElement {
    if app.state.current.is_none() {
        return empty_state(app).into_any_element();
    }

    // 先把消息渲染成元素：markdown 视图要同时借 window 与 App，
    // 与后面 `cx.listener(...)` 的借用错开，避免同一表达式里双份可变借用。
    let bubbles: Vec<gpui::AnyElement> = app
        .state
        .messages
        .iter()
        .map(|m| bubble(app, m, window, cx).into_any_element())
        .collect();
    let streaming = if app.state.streaming.is_empty() {
        None
    } else {
        Some(streaming_bubble(app, &app.state.streaming, window, cx).into_any_element())
    };

    let mut list = v_flex()
        .id("messages")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(
            v_flex()
                .w_full()
                .max_w(px(880.))
                .mx_auto()
                .px(px(32.))
                .pt(px(34.))
                .pb(px(42.))
                .children(bubbles)
                .children(streaming),
        );

    // 待审批 / 待回答的卡片压在消息流末尾，与原前端的行内弹层同位。
    if let Some(session_id) = app.state.current_id() {
        if let Some(pending) = app.state.pending_approvals.get(session_id) {
            list = list.child(approval_card(app, cx, pending));
        }
        if let Some(question) = app.state.pending_questions.get(session_id) {
            list = list.child(question_card(app, cx, question));
        }
    }
    list.into_any_element()
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
    app: &HebbianApp,
    message: &Message,
    window: &mut Window,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
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
            body = body.child(tool_card(
                app,
                cx,
                &format!("{}-tc{}", message.id, i),
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
                    body.child(reasoning_block(app, cx, &key, text, *duration_ms))
                }
                MessagePart::ToolCall {
                    name,
                    input,
                    result,
                    duration_ms,
                    is_error,
                    ..
                } => body.child(tool_card(
                    app,
                    cx,
                    &key,
                    name,
                    input,
                    result.as_deref(),
                    *duration_ms,
                    *is_error,
                )),
            };
        }
    }

    body = body.child(meta_row(app, message, is_user));

    h_flex()
        .items_start()
        .gap(px(12.))
        .mb(px(20.))
        .child(avatar(app, is_user))
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
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    key: &str,
    text: &str,
    duration_ms: Option<u64>,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let expanded = app.state.expanded_parts.contains(key);
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.state.expanded_parts.remove(&key_owned) {
                        this.state.expanded_parts.insert(key_owned.clone());
                    }
                    cx.notify();
                })),
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
fn tool_card(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    key: &str,
    name: &str,
    input: &serde_json::Value,
    result: Option<&str>,
    duration_ms: Option<u64>,
    is_error: bool,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let expanded = app.state.expanded_parts.contains(key);
    let key_owned = key.to_string();
    let summary = tool_summary(input);
    let duration = match duration_ms {
        Some(ms) if ms >= 1000 => format!("{:.1}s", ms as f64 / 1000.),
        Some(ms) => format!("{ms}ms"),
        None => String::new(),
    };
    let args = serde_json::to_string_pretty(input).unwrap_or_default();
    let result_text = result.unwrap_or("").to_string();

    v_flex()
        .rounded(px(5.))
        .border_1()
        .border_color(if is_error { theme.danger } else { theme.line })
        .bg(theme.card)
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
                        .font_weight(gpui::FontWeight(600.))
                        .text_color(theme.text)
                        .child(name.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(theme.muted)
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.state.expanded_parts.remove(&key_owned) {
                        this.state.expanded_parts.insert(key_owned.clone());
                    }
                    cx.notify();
                })),
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

/// 工具卡片收起时那句摘要：优先挑最能说明「这次调用干了什么」的字段。
fn tool_summary(input: &serde_json::Value) -> String {
    for key in ["command", "file_path", "path", "pattern", "query", "url", "description"] {
        if let Some(value) = input.get(key).and_then(|v| v.as_str()) {
            return value.to_string();
        }
    }
    String::new()
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
fn avatar(app: &HebbianApp, is_user: bool) -> impl IntoElement {
    let theme = app.theme.clone();
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

/// 气泡底部那行「日期 · 耗时 + 复制 / 分叉 / 重新生成」。
fn meta_row(app: &HebbianApp, message: &Message, is_user: bool) -> impl IntoElement {
    let theme = app.theme.clone();

    let action = |icon: Icon, label: &'static str| {
        h_flex()
            .gap(px(4.))
            .text_size(px(11.))
            .text_color(theme.faint)
            .child(icon.el(px(12.), theme.faint))
            .child(label)
    };

    let stamp = chrono::DateTime::from_timestamp_millis(message.created_at)
        .map(|dt| dt.format("%m/%d").to_string())
        .unwrap_or_default();

    h_flex()
        .mt(px(8.))
        .gap(px(12.))
        .items_center()
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.faint)
                .child(stamp),
        )
        .when_some(message.run_duration_ms, |this, ms| {
            this.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.faint)
                    .child(format!("· {:.1}s", ms as f64 / 1000.)),
            )
        })
        .child(action(Icon::Copy, ""))
        .child(action(Icon::GitBranch, "分叉"))
        .child(if is_user {
            action(Icon::Pencil, "编辑")
        } else {
            action(Icon::RefreshCw, "重新生成")
        })
}

/// 流式进行中的助手气泡。
fn streaming_bubble(
    app: &HebbianApp,
    turn: &StreamingTurn,
    window: &mut Window,
    cx: &mut Context<HebbianApp>,
) -> impl IntoElement {
    let theme = app.theme.clone();
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
            strip = strip.child(tool_chip(app, &tool.name, tool.done, tool.is_error));
        }
        body = body.child(strip);
    }

    h_flex().items_start().gap(px(12.)).mb(px(20.)).child(body)
}

fn tool_chip(app: &HebbianApp, name: &str, done: bool, is_error: bool) -> impl IntoElement {
    let theme = app.theme.clone();
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
fn approval_card(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    pending: &crate::state::PendingApproval,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let session_id = app.state.current_id().unwrap_or_default().to_string();

    let button = |label: &'static str, decision: protocol::ApprovalDecision, primary: bool| {
        let theme = theme.clone();
        let session_id = session_id.clone();
        h_flex()
            .id(label)
            .px(px(12.))
            .h(px(30.))
            .rounded(px(8.))
            .text_size(px(12.))
            .cursor_pointer()
            .when(primary, |this| {
                this.bg(theme.accent).text_color(gpui::white())
            })
            .when(!primary, |this| {
                this.border_1().border_color(theme.line).text_color(theme.muted)
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(pending) = this.state.take_approval(&session_id) {
                    this.state.core.resolve_approval(
                        session_id.clone(),
                        pending.request_id,
                        decision.clone(),
                    );
                }
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
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted)
                .child(pending.summary.clone()),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .child(button(
                    "允许一次",
                    protocol::ApprovalDecision::AllowOnce,
                    true,
                ))
                .child(button(
                    "本对话允许",
                    protocol::ApprovalDecision::AllowAndRemember {
                        scope: protocol::PermissionScope::Session,
                        pattern: None,
                        extra_patterns: Vec::new(),
                    },
                    false,
                ))
                .child(button("拒绝", protocol::ApprovalDecision::Deny, false)),
        )
}

/// 模型提问卡片。
fn question_card(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    question: &crate::state::PendingQuestion,
) -> impl IntoElement {
    let theme = app.theme.clone();
    let session_id = app.state.current_id().unwrap_or_default().to_string();

    let mut options = v_flex().gap(px(6.));
    for option in &question.options {
        let label = option.label.clone();
        let session_id = session_id.clone();
        options = options.child(
            div()
                .id(gpui::SharedString::from(format!("opt-{label}")))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(theme.line)
                .text_size(px(12.))
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent_soft))
                .child(label.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(pending) = this.state.take_question(&session_id) {
                        this.state.core.answer_question(
                            session_id.clone(),
                            pending.request_id,
                            protocol::UserAnswer::Selected {
                                label: label.clone(),
                            },
                        );
                    }
                    cx.notify();
                })),
        );
    }

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
        .child(info_row(app))
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
                        // 切模型不是纯写字段：会话里要插一条 switch marker、
                        // DeepSeek 与其他系列之间还有锁定规则、推理参数要跟着模型默认值走。
                        // 这套业务规则现在只存在于 desktop / hebweb 的命令壳里，gpui 不再抄第三份
                        // ——等它收进 core-rpc 之后再接上，在此之前明确告诉用户而不是假装切了。
                        this.state.error = Some(format!(
                            "切换到「{label}」还没接上：切模型要连带插切换标记、系列锁定与推理参数，\
                             这套规则得先收进公共入口，我不想在这里再抄一份走样的"
                        ));
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
            .child(list),
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
fn info_row(app: &HebbianApp) -> impl IntoElement {
    let theme = app.theme.clone();

    let chip = |icon: Icon, label: &'static str| {
        h_flex()
            .gap(px(4.))
            .pr(px(8.))
            .min_h(px(20.))
            .text_size(px(11.))
            .text_color(theme.muted)
            .child(icon.el(px(12.), theme.muted))
            .child(label)
            .child(Icon::ChevronDown.el(px(10.), theme.faint))
    };

    h_flex()
        .w_full()
        .max_w(px(682.))
        .mt(px(6.))
        .items_center()
        .text_size(px(11.))
        .text_color(theme.muted)
        .child(chip(Icon::Zap, "全速模式"))
        .child(chip(Icon::Gauge, "极高"))
        .child(div().flex_1())
        .child(
            h_flex()
                .gap(px(6.))
                .child(
                    // 上下文占用环。真实占比接进来之前先画空环，位置尺寸与原前端一致。
                    div()
                        .size(px(18.))
                        .rounded_full()
                        .border_2()
                        .border_color(theme.accent),
                )
                .child(format!(
                    "cache {}% / ctx {}%",
                    0,
                    app.state
                        .current
                        .as_ref()
                        .map(|s| context_percent(s.messages.len()))
                        .unwrap_or(0)
                )),
        )
}

/// 上下文占用的粗略估计：消息条数占 200 条软上限的比例。
/// 真正的 token 统计要等 `TokenStats` 事件接进来，这里先给一个不误导的近似。
fn context_percent(message_count: usize) -> usize {
    (message_count * 100 / 200).min(100)
}

fn toolbar(
    app: &HebbianApp,
    cx: &mut Context<HebbianApp>,
    running: bool,
    model: String,
) -> impl IntoElement {
    let theme = app.theme.clone();

    let tool = |icon: Icon, id: &'static str| {
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
    };

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
                .child(tool(Icon::Plus, "attach"))
                .child(tool(Icon::Slash, "slash"))
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
