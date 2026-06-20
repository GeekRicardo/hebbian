//! 主对话 HITL 转发到聊天渠道（架构 §7.5.1，2026-06-20）。
//!
//! 机主离开电脑（系统空闲达阈值）时，桌面主对话里待审批/待回答的 HITL 会发到已连接的
//! 渠道（当前是微信）。机主在手机上回复 → 渠道侧 `ChannelBridge` 解析 → 经
//! [`DesktopHitlResolver`] 回落到本进程的 [`HitlState`]，与本地审批走同一落地路径。
//!
//! 渠道未连接、机主从未发过消息（无回复目标）或系统仍活跃时，本模块静默跳过——
//! 主对话照常走灵动岛 + 前端弹窗，互不影响（两端先回先赢）。

use std::sync::Arc;

use channel_core::bridge::RemoteHitlResolver;
use protocol::{ApprovalDecision, QuestionOption, UserAnswer};
use tauri::{AppHandle, Manager};

use crate::engine::{AskQuestionDto, EngineEvent, QuestionOptionDto};
use crate::hitl::HitlState;
use crate::wechat::WeChatState;

/// 把渠道回复落回本进程 HitlState 的 resolver。
struct DesktopHitlResolver {
    app: AppHandle,
}

impl RemoteHitlResolver for DesktopHitlResolver {
    fn resolve_approval(&self, request_id: &str, decision: ApprovalDecision) {
        if let Some(state) = self.app.try_state::<Arc<HitlState>>() {
            if let Err(err) = state.resolve_approval(request_id, decision) {
                tracing::warn!(error = %err, request_id, "渠道审批回落失败");
            }
        }
    }

    fn answer_question(&self, request_id: &str, answer: UserAnswer) {
        if let Some(state) = self.app.try_state::<Arc<HitlState>>() {
            if let Err(err) = state.answer_question(request_id, answer) {
                tracing::warn!(error = %err, request_id, "渠道问答回落失败");
            }
        }
    }
}

/// 主对话产生 HITL 事件时尝试转发到渠道。仅在系统空闲达阈值且渠道在线时转发。
///
/// `PermissionResolved` / `UserQuestionAnswered` 到达时撤销对应的渠道待办（已在本地处理）。
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
            if *auto_handled || !idle_enough(app) {
                return;
            }
            let resolver = Arc::new(DesktopHitlResolver { app: app.clone() });
            let text = format!(
                "{header}\n\n⚠️ 需要审批：{detail}\n\n回复 1/y 通过，回复 n 拒绝，回复「deny 原因」拒绝并反馈。",
                header = session_header(app, session_id),
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
            if !idle_enough(app) {
                return;
            }
            let resolver = Arc::new(DesktopHitlResolver { app: app.clone() });
            // 渠道只支持单层选项；多题场景退化为铺开展示、收自由文本。
            let (body, opts, is_multi) = if questions.is_empty() {
                (question.clone(), to_proto_options(options), *multi)
            } else {
                (render_multi_questions(questions), Vec::new(), false)
            };
            let text = format!(
                "{header}\n\n❓ {body}{choices}",
                header = session_header(app, session_id),
                choices = render_choices(&opts, is_multi),
            );
            bridge.forward_question(request_id, &text, opts, is_multi, resolver);
        }
        EngineEvent::PermissionResolved { request_id, .. }
        | EngineEvent::UserQuestionAnswered { request_id, .. } => {
            bridge.cancel_forwarded(request_id);
        }
        _ => {}
    }
}

/// 系统空闲是否达到配置阈值（分钟）。阈值 0 = 关闭转发。
fn idle_enough(app: &AppHandle) -> bool {
    let settings = agent_core::storage::settings::load(&data_dir(app));
    crate::idle::is_idle_for(settings.general.channel_idle_forward_minutes)
}

fn data_dir(_app: &AppHandle) -> std::path::PathBuf {
    agent_core::storage::default_data_dir()
}

/// 转发头部：会话标题 + 最近一段 AI 输出，让机主在手机上有判断依据。
fn session_header(app: &AppHandle, session_id: &str) -> String {
    let Ok(session) = agent_core::storage::sessions::load(&data_dir(app), session_id) else {
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