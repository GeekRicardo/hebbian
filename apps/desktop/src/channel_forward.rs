//! 主对话 HITL 转发到聊天渠道（架构 §7.5.1，2026-06-20）。
//!
//! 机主离开电脑（系统空闲达阈值）时，桌面主对话里待审批/待回答的 HITL 会发到已连接的
//! 渠道（当前是微信）。机主在手机上回复 → 渠道侧 `ChannelBridge` 解析 → 经
//! [`DesktopHitlResolver`] 回落到本进程的 [`HitlState`]，与本地审批走同一落地路径。
//!
//! 渠道未连接、机主从未发过消息（无回复目标）或系统仍活跃时，本模块静默跳过——
//! 主对话照常走灵动岛 + 前端弹窗，互不影响（两端先回先赢）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_core::storage::sessions::ChannelForwardKind;
use channel_core::bridge::RemoteHitlResolver;
use protocol::{ApprovalDecision, QuestionOption, UserAnswer};
use tauri::{AppHandle, Manager};

use crate::engine::{AskQuestionDto, EngineEvent, QuestionOptionDto};
use crate::hitl::HitlState;
use crate::wechat::WeChatState;

const CHANNEL_ID: &str = "wechat";

/// 在途转发的痕迹定位：`request_id → (session_id, marker_id)`。
///
/// 一条转发的痕迹 marker 会被两端竞争消费：① 机主在渠道（微信）回复 → resolver；
/// ② 机主回电脑本地处理 → `PermissionResolved` / `UserQuestionAnswered`。谁先到谁把
/// marker 改成 Resolved 并从表里移除，另一端发现已不在表里就跳过——保证 marker 只结算一次。
#[derive(Default)]
pub struct ChannelForwardState {
    inflight: Mutex<HashMap<String, (String, String)>>,
}

impl ChannelForwardState {
    fn track(&self, request_id: &str, session_id: &str, marker_id: &str) {
        self.inflight.lock().unwrap().insert(
            request_id.to_string(),
            (session_id.to_string(), marker_id.to_string()),
        );
    }

    /// 取出并移除一条在途转发（谁先结算谁拿到）。
    fn take(&self, request_id: &str) -> Option<(String, String)> {
        self.inflight.lock().unwrap().remove(request_id)
    }
}

/// 把渠道回复落回本进程 HitlState 的 resolver，并把转发痕迹 marker 更新为「已处置」。
///
/// `marker_id` 指向转发时落在 session.jsonl 的 `ChannelForward` marker——机主在渠道侧
/// 回复后，除了走 HitlState 让 agent 继续，还把这条 marker 的 status 改成 Resolved，
/// 让机主回到电脑能看到「这条审批/问题当时转发到微信，结论是 X」（即写即落，架构 §7.5.1）。
struct DesktopHitlResolver {
    app: AppHandle,
    request_id: String,
    session_id: String,
    marker_id: String,
}

impl DesktopHitlResolver {
    /// 渠道回复先到：结算 marker（若本地未抢先）。
    fn settle(&self, outcome: String) {
        if let Some(state) = self.app.try_state::<Arc<ChannelForwardState>>() {
            if state.take(&self.request_id).is_none() {
                return; // 本地已抢先结算
            }
        }
        if let Err(err) = agent_core::storage::sessions::resolve_channel_forward_marker(
            &data_dir(),
            &self.session_id,
            &self.marker_id,
            outcome,
        ) {
            tracing::warn!(error = %err, "更新渠道转发痕迹失败");
        }
    }
}

impl RemoteHitlResolver for DesktopHitlResolver {
    fn resolve_approval(&self, request_id: &str, decision: ApprovalDecision) {
        self.settle(approval_outcome(&decision));
        if let Some(state) = self.app.try_state::<Arc<HitlState>>() {
            if let Err(err) = state.resolve_approval(request_id, decision) {
                tracing::warn!(error = %err, request_id, "渠道审批回落失败");
            }
        }
    }

    fn answer_question(&self, request_id: &str, answer: UserAnswer) {
        self.settle(answer_outcome(&answer));
        if let Some(state) = self.app.try_state::<Arc<HitlState>>() {
            if let Err(err) = state.answer_question(request_id, answer) {
                tracing::warn!(error = %err, request_id, "渠道问答回落失败");
            }
        }
    }
}

/// 主对话产生 HITL 事件时尝试转发到渠道。仅在系统空闲达阈值且渠道在线时转发。
///
/// 转发时在 session.jsonl 落一条 `ChannelForward` Pending marker（即写即落）；机主回复后
/// 由 resolver 原地更新为 Resolved。`PermissionResolved` / `UserQuestionAnswered` 到达时
/// 撤销渠道待办（已在本地处理），并把仍 Pending 的痕迹标成「已在电脑处理」。
pub fn maybe_forward(app: &AppHandle, session_id: &str, event: &EngineEvent) {
    let Some(wechat) = app.try_state::<Arc<WeChatState>>() else {
        return;
    };
    let Some(bridge) = wechat.bridge() else {
        return;
    };

    match event {
        EngineEvent::PermissionRequested {
            request_id,
            kind,
            tool_name,
            summary,
            paths,
            auto_handled,
            ..
        } => {
            if *auto_handled || !idle_enough(app) || !bridge.can_forward() {
                return;
            }
            let Ok(marker_id) = agent_core::storage::sessions::append_channel_forward_marker(
                &data_dir(),
                session_id,
                CHANNEL_ID.to_string(),
                ChannelForwardKind::Approval,
            ) else {
                return;
            };
            track_inflight(app, request_id, session_id, &marker_id);
            let resolver = Arc::new(DesktopHitlResolver {
                app: app.clone(),
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                marker_id,
            });
            let text = format!(
                "{header}\n\n⚠️ 需要审批：{detail}\n\n回复 1/y 通过，回复 n 拒绝，回复「deny 原因」拒绝并反馈。",
                header = session_header(session_id),
                detail = approval_detail(kind, tool_name, summary, paths),
            );
            bridge.forward_approval(request_id, &text, resolver);
        }
        EngineEvent::UserQuestionRequested {
            request_id,
            question,
            options,
            multi,
            questions,
        } => {
            if !idle_enough(app) || !bridge.can_forward() {
                return;
            }
            let Ok(marker_id) = agent_core::storage::sessions::append_channel_forward_marker(
                &data_dir(),
                session_id,
                CHANNEL_ID.to_string(),
                ChannelForwardKind::Question,
            ) else {
                return;
            };
            track_inflight(app, request_id, session_id, &marker_id);
            let resolver = Arc::new(DesktopHitlResolver {
                app: app.clone(),
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                marker_id,
            });
            // 渠道只支持单层选项；多题场景退化为铺开展示、收自由文本。
            let (body, opts, is_multi) = if questions.is_empty() {
                (question.clone(), to_proto_options(options), *multi)
            } else {
                (render_multi_questions(questions), Vec::new(), false)
            };
            let text = format!(
                "{header}\n\n❓ {body}{choices}",
                header = session_header(session_id),
                choices = render_choices(&opts, is_multi),
            );
            bridge.forward_question(request_id, &text, opts, is_multi, resolver);
        }
        EngineEvent::PermissionResolved { request_id, .. }
        | EngineEvent::UserQuestionAnswered { request_id, .. } => {
            bridge.cancel_forwarded(request_id);
            settle_locally(app, request_id);
        }
        _ => {}
    }
}

fn track_inflight(app: &AppHandle, request_id: &str, session_id: &str, marker_id: &str) {
    if let Some(state) = app.try_state::<Arc<ChannelForwardState>>() {
        state.track(request_id, session_id, marker_id);
    }
}

/// 本地处理先到：把仍 Pending 的转发痕迹标成「已在电脑处理」（若渠道未抢先结算）。
fn settle_locally(app: &AppHandle, request_id: &str) {
    let Some(state) = app.try_state::<Arc<ChannelForwardState>>() else {
        return;
    };
    let Some((session_id, marker_id)) = state.take(request_id) else {
        return;
    };
    if let Err(err) = agent_core::storage::sessions::resolve_channel_forward_marker(
        &data_dir(),
        &session_id,
        &marker_id,
        "已在电脑处理".to_string(),
    ) {
        tracing::warn!(error = %err, "更新渠道转发痕迹失败");
    }
}

/// 审批决定 → 人话结论，落进转发痕迹 marker。
fn approval_outcome(decision: &ApprovalDecision) -> String {
    match decision {
        ApprovalDecision::AllowOnce => "已通过".to_string(),
        ApprovalDecision::AllowAndRemember { .. } => "已通过并记住".to_string(),
        ApprovalDecision::Deny => "已拒绝".to_string(),
        ApprovalDecision::DenyWithFeedback { feedback } => format!("已拒绝：{feedback}"),
    }
}

/// 问题答案 → 人话结论。
fn answer_outcome(answer: &UserAnswer) -> String {
    match answer {
        UserAnswer::Selected { label } => format!("选了：{label}"),
        UserAnswer::SelectedMulti { labels } => format!("选了：{}", labels.join("、")),
        UserAnswer::Custom { text } => format!("回复：{text}"),
        UserAnswer::Cancelled => "已取消".to_string(),
        UserAnswer::Multi { .. } => "已回复".to_string(),
    }
}

/// 系统空闲是否达到配置阈值（分钟）。阈值 0 = 关闭转发。
fn idle_enough(_app: &AppHandle) -> bool {
    let settings = agent_core::storage::settings::load(&data_dir());
    crate::idle::is_idle_for(settings.general.channel_idle_forward_minutes)
}

fn data_dir() -> std::path::PathBuf {
    agent_core::storage::default_data_dir()
}

/// 转发头部：会话标题 + 最近一段 AI 输出，让机主在手机上有判断依据。
fn session_header(session_id: &str) -> String {
    let Ok(session) = agent_core::storage::sessions::load(&data_dir(), session_id) else {
        return "📨 桌面对话需要你处理".to_string();
    };
    let title = if session.title.trim().is_empty() {
        "桌面对话".to_string()
    } else {
        session.title.clone()
    };
    let recent = session
        .messages
        .iter()
        .rev()
        .find(|message| {
            matches!(message.role, agent_core::storage::sessions::Role::Assistant)
                && !message.content.trim().is_empty()
        })
        .map(|message| truncate_chars(message.content.trim(), 300))
        .unwrap_or_default();

    if recent.is_empty() {
        format!("📨「{title}」需要你处理")
    } else {
        format!("📨「{title}」\n\n最近 AI 输出：\n{recent}")
    }
}

fn approval_detail(kind: &str, tool_name: &str, summary: &str, paths: &[String]) -> String {
    if kind == "path_access" && !paths.is_empty() {
        return format!("{tool_name} 越界访问：{}", paths.join("、"));
    }
    if !summary.trim().is_empty() {
        return summary.to_string();
    }
    tool_name.to_string()
}

fn render_choices(options: &[QuestionOption], multi: bool) -> String {
    if options.is_empty() {
        return "\n\n直接回复你的答案，或回「取消」放弃。".to_string();
    }
    let mut lines = String::from("\n\n选项：");
    for (index, option) in options.iter().enumerate() {
        lines.push_str(&format!("\n  {}. {}", index + 1, option.label));
    }
    if multi {
        lines.push_str("\n\n回复编号选择（多选用逗号隔开，如 1,3），或回「取消」。");
    } else {
        lines.push_str("\n\n回复编号选择（如 2），或直接回文本，或回「取消」。");
    }
    lines
}

fn to_proto_options(options: &[QuestionOptionDto]) -> Vec<QuestionOption> {
    options
        .iter()
        .map(|option| QuestionOption {
            label: option.label.clone(),
            description: option.description.clone(),
        })
        .collect()
}

fn render_multi_questions(questions: &[AskQuestionDto]) -> String {
    questions
        .iter()
        .map(|q| {
            let labels = q
                .options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            if labels.is_empty() {
                q.title.clone()
            } else {
                format!("{}（{}）", q.title, labels)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let mut out: String = chars.into_iter().take(max).collect();
    out.push('…');
    out
}