//! 对外线协议事件 DTO（架构 §3.1.1）。
//!
//! [`crate::event::EventPayload`] 是 core 内部的**领域模型**：嵌套 enum
//! （`PermissionKind` / `ResumeCause` / `UserAnswer`）、强类型，给 Rust 业务逻辑用。
//! surface 的客户端（前端 React / heb 脚本）要的是**线协议 DTO**：字段拍平、enum
//! 降成 `tag + 散字段`，能被 TS 直接消费。两者形态不同，中间这层转换是合理的边界。
//!
//! 历史上这层转换在 desktop / cli / web 各写一遍且不一致（cli 截 result、desktop
//! 丢 token、`RunFailed` 翻成不同 variant）。这里定**唯一的无损 DTO `WireEvent`** +
//! **唯一的转换 [`to_wire`]**，三 surface 共享：
//!
//! - **无损**：DTO 带全部字段，不在转换层做任何精简。
//! - **差异下沉渲染层**：surface 各自的展示偏好（heb CLI 截断 result、忽略 index）
//!   放进各自渲染层，不进 DTO / 转换层。
//!
//! 线形态与早期 desktop `EngineEvent` 完全一致（`tag = "type"` + snake_case），
//! 前端无感切换。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{
    Event, EventPayload, ResumeCause, RiskLevel, StepKind, StopReason, SuspendReason,
    TurnFileChange,
};
use crate::memory::MemoryWriteItem;
use crate::permission::{
    ApprovalDecision, ApprovalSegment, AskQuestion, PermissionKind, QuestionOption, UserAnswer,
};
use crate::todo::{PlanComment, TodoItem};

/// 对外线协议事件（架构 §3.1.1）。`EventPayload` 拍平后的无损 DTO。
///
/// `subagent_call_id`（来自 [`Event`] 外层）按 variant 内联进各事件，前端据此把子
/// NestedRun 事件嵌套渲染到父 Task 卡片内部（架构 §4.4.11.8）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireEvent {
    // —— 模型流 ——
    TextDelta {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    TextDone {
        full_text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },

    // —— 工具 ——
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    ToolStart {
        index: usize,
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    ToolOutputDelta {
        index: usize,
        id: String,
        chunk: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    ToolDone {
        index: usize,
        id: String,
        result: String,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_path: Option<String>,
        #[serde(default)]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },

    // —— 生命周期 ——
    /// 注：只携带 `duration_ms`。token 用量走 turn 级 [`WireEvent::Usage`] 实时累加，
    /// surface 不从这里取 token（与早期 desktop 行为一致）。
    RunFinished {
        duration_ms: u64,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
    },
    RunSuspended {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumes_at_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waiting_for_task_ids: Vec<String>,
    },
    RunResumed {
        cause: String,
    },

    // —— Step / Turn / 重试 / 模式 ——
    StepStarted {
        step_kind: String,
        step_index: u32,
    },
    StepFinished {
        step_kind: String,
        step_index: u32,
    },
    ModelRetry {
        attempt: u32,
        max: u32,
        delay_ms: u64,
        reason: String,
    },
    TurnFinished {
        stop_reason: String,
    },
    RunModeChanged {
        from: String,
        to: String,
    },
    ContextCompactionStarted {
        before_tokens: usize,
    },
    ContextCompactionProgress {
        output_tokens: usize,
    },
    ContextCompacted {
        before_tokens: usize,
        after_tokens: usize,
    },

    // —— 审批 ——
    PermissionRequested {
        request_id: String,
        kind: String,
        tool_name: String,
        input: Value,
        summary: String,
        risk: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command_segments: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        segments: Vec<ApprovalSegment>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refuse_remember: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<WirePlanPermission>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        auto_handled: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        call_id: String,
    },
    PermissionResolved {
        request_id: String,
        decision: String,
    },
    PermissionAutoJudged {
        request_id: String,
        tool_name: String,
        decision: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        requires_human: bool,
    },
    Notice {
        level: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dedup_key: Option<String>,
    },

    // —— 提问 ——
    UserQuestionRequested {
        request_id: String,
        question: String,
        options: Vec<QuestionOption>,
        #[serde(default)]
        multi: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        questions: Vec<AskQuestion>,
    },
    UserQuestionAnswered {
        request_id: String,
        /// `selected` / `selected_multi` / `custom` / `cancelled` / `multi`
        kind: String,
        text: String,
    },

    // —— 编辑快照 ——
    RunEditsCommitted {
        run_id: String,
        files: Vec<TurnFileChange>,
    },
    RunEditsReverted {
        run_id: String,
    },
    RunEditsRevertFailed {
        run_id: String,
        file_path: String,
        error: String,
    },

    // —— 标题 / 记忆 / Todo / Plan / Goal ——
    SessionTitleChanged {
        session_id: String,
        title: String,
    },
    SessionTitleGenerationFailed {
        session_id: String,
        reason: String,
    },
    TodoListUpdated {
        todos: Vec<WireTodoItem>,
    },
    PlanReady {
        plan_id: String,
        plan_path: String,
        plan_markdown: String,
        summary: String,
    },
    PlanCommentAdded {
        plan_id: String,
        comment: PlanComment,
    },
    MemoryExtracted {
        session_id: String,
        items: Vec<MemoryWriteItem>,
    },
    MemoryExtractionFailed {
        session_id: String,
        reason: String,
    },
    GoalAchieved {
        condition: String,
        reason: String,
    },
    GoalImpossible {
        condition: String,
        reason: String,
    },
    GoalProgress {
        iteration: u32,
        reason: String,
    },

    /// [`EventPayload::RunFailed`] 的对外形态：只暴露一句错误文本。
    Error {
        message: String,
    },
}

/// Plan 审批的线 DTO（[`PermissionKind::Plan`] 拍平）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePlanPermission {
    pub plan_id: String,
    pub plan_path: String,
    pub plan_markdown: String,
    pub summary: String,
}

/// Todo 项的线 DTO。仅 `active_form` 需重命名为 camelCase `activeForm`
/// （前端历史契约），其余字段与 [`TodoItem`] 一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTodoItem {
    pub id: String,
    pub content: String,
    #[serde(rename = "activeForm")]
    pub active_form: String,
    /// `pending` / `in_progress` / `completed`
    pub status: String,
}

impl From<&TodoItem> for WireTodoItem {
    fn from(t: &TodoItem) -> Self {
        use crate::todo::TodoStatus;
        Self {
            id: t.id.clone(),
            content: t.content.clone(),
            active_form: t.active_form.clone(),
            status: match t.status {
                TodoStatus::Pending => "pending",
                TodoStatus::InProgress => "in_progress",
                TodoStatus::Completed => "completed",
            }
            .to_string(),
        }
    }
}

/// 唯一的 `EventPayload → WireEvent` 转换（架构 §3.1.1）。
///
/// 三 surface 共享这一份。返回 `None` 表示该事件不对外（如 `RunStarted` /
/// `RunCancelled` / `TurnStarted` / `Log` 等纯内部 / 调试事件，早期三 surface
/// 也都不向客户端投递）。`subagent_call_id` 从外层 [`Event`] 取，内联进携带它的
/// variant。
pub fn to_wire(event: &Event) -> Option<WireEvent> {
    let sub = event.subagent_call_id.clone();
    let wire = match &event.payload {
        // —— 模型流 ——
        EventPayload::TextDelta { text } => WireEvent::TextDelta {
            text: text.clone(),
            subagent_call_id: sub,
        },
        EventPayload::TextDone { full_text } => WireEvent::TextDone {
            full_text: full_text.clone(),
            subagent_call_id: sub,
        },
        EventPayload::Reasoning { text } => WireEvent::Reasoning {
            text: text.clone(),
            subagent_call_id: sub,
        },

        // —— 工具 ——
        EventPayload::ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } => WireEvent::ToolCallDelta {
            index: *index,
            id: id.clone(),
            name: name.clone(),
            arguments_delta: arguments_delta.clone(),
            subagent_call_id: sub,
        },
        EventPayload::ToolCallStarted {
            index,
            call_id,
            name,
            input,
        } => WireEvent::ToolStart {
            index: *index,
            id: call_id.clone(),
            name: name.clone(),
            input: input.clone(),
            subagent_call_id: sub,
        },
        EventPayload::ToolCallOutputDelta {
            index,
            call_id,
            chunk,
        } => WireEvent::ToolOutputDelta {
            index: *index,
            id: call_id.clone(),
            chunk: chunk.clone(),
            subagent_call_id: sub,
        },
        EventPayload::ToolCallFinished {
            index,
            call_id,
            result,
            duration_ms,
            artifact_path,
            is_error,
            ..
        } => WireEvent::ToolDone {
            index: *index,
            id: call_id.clone(),
            result: result.clone(),
            duration_ms: *duration_ms,
            artifact_path: artifact_path.clone(),
            is_error: *is_error,
            subagent_call_id: sub,
        },

        // —— 生命周期 ——
        EventPayload::RunFinished { duration_ms, .. } => WireEvent::RunFinished {
            duration_ms: *duration_ms,
        },
        EventPayload::RunFailed { error } => WireEvent::Error {
            message: error.message.clone(),
        },
        EventPayload::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        } => WireEvent::Usage {
            input_tokens: *input_tokens,
            output_tokens: *output_tokens,
            cache_read_tokens: *cache_read_tokens,
            cache_creation_tokens: *cache_creation_tokens,
        },
        EventPayload::RunSuspended {
            reason,
            resumes_at_ms,
            waiting_for_task_ids,
        } => WireEvent::RunSuspended {
            reason: suspend_reason_str(reason).to_string(),
            resumes_at_ms: *resumes_at_ms,
            waiting_for_task_ids: waiting_for_task_ids.clone(),
        },
        EventPayload::RunResumed { cause } => WireEvent::RunResumed {
            cause: resume_cause_str(cause),
        },

        // —— Step / Turn / 重试 / 模式 ——
        EventPayload::StepStarted {
            step_kind,
            step_index,
        } => WireEvent::StepStarted {
            step_kind: step_kind_str(step_kind).to_string(),
            step_index: *step_index,
        },
        EventPayload::StepFinished {
            step_kind,
            step_index,
        } => WireEvent::StepFinished {
            step_kind: step_kind_str(step_kind).to_string(),
            step_index: *step_index,
        },
        EventPayload::ModelRetry {
            attempt,
            max,
            delay_ms,
            reason,
        } => WireEvent::ModelRetry {
            attempt: *attempt,
            max: *max,
            delay_ms: *delay_ms,
            reason: reason.clone(),
        },
        EventPayload::TurnFinished { stop_reason, .. } => WireEvent::TurnFinished {
            stop_reason: stop_reason_str(stop_reason).to_string(),
        },
        EventPayload::RunModeChanged { from, to } => WireEvent::RunModeChanged {
            from: from.clone(),
            to: to.clone(),
        },
        EventPayload::ContextCompactionStarted { before_tokens } => {
            WireEvent::ContextCompactionStarted {
                before_tokens: *before_tokens,
            }
        }
        EventPayload::ContextCompactionProgress { output_tokens } => {
            WireEvent::ContextCompactionProgress {
                output_tokens: *output_tokens,
            }
        }
        EventPayload::ContextCompacted {
            before_tokens,
            after_tokens,
        } => WireEvent::ContextCompacted {
            before_tokens: *before_tokens,
            after_tokens: *after_tokens,
        },

        // —— 审批 ——
        EventPayload::PermissionRequested {
            request_id,
            kind,
            summary,
            risk,
            auto_handled,
            call_id,
        } => {
            let mut ev = PermissionFields::from_kind(kind);
            ev.request_id = request_id.0.clone();
            ev.summary = summary.clone();
            ev.risk = risk_str(risk);
            ev.auto_handled = *auto_handled;
            ev.call_id = call_id.clone();
            ev.into_wire()
        }
        EventPayload::PermissionResolved {
            request_id,
            decision,
        } => WireEvent::PermissionResolved {
            request_id: request_id.0.clone(),
            decision: approval_decision_str(decision).to_string(),
        },
        EventPayload::PermissionAutoJudged {
            request_id,
            tool_name,
            decision,
            reason,
            requires_human,
        } => WireEvent::PermissionAutoJudged {
            request_id: request_id
                .as_ref()
                .map(|id| id.0.clone())
                .unwrap_or_default(),
            tool_name: tool_name.clone(),
            decision: decision.clone(),
            reason: reason.clone(),
            requires_human: *requires_human,
        },
        EventPayload::Notice {
            level,
            message,
            dedup_key,
        } => WireEvent::Notice {
            level: notice_level_str(level).to_string(),
            message: message.clone(),
            dedup_key: dedup_key.clone(),
        },

        // —— 提问 ——
        EventPayload::UserQuestionRequested {
            request_id,
            question,
            options,
            multi,
            questions,
        } => WireEvent::UserQuestionRequested {
            request_id: request_id.0.clone(),
            question: question.clone(),
            options: options.clone(),
            multi: *multi,
            questions: questions.clone(),
        },
        EventPayload::UserQuestionAnswered { request_id, answer } => {
            let (kind, text) = user_answer_kind_text(answer);
            WireEvent::UserQuestionAnswered {
                request_id: request_id.0.clone(),
                kind: kind.to_string(),
                text,
            }
        }

        // —— 编辑快照 ——
        EventPayload::RunEditsCommitted { run_id, files } => WireEvent::RunEditsCommitted {
            run_id: run_id.0.clone(),
            files: files.clone(),
        },
        EventPayload::RunEditsReverted { run_id } => WireEvent::RunEditsReverted {
            run_id: run_id.0.clone(),
        },
        EventPayload::RunEditsRevertFailed {
            run_id,
            file_path,
            error,
        } => WireEvent::RunEditsRevertFailed {
            run_id: run_id.0.clone(),
            file_path: file_path.clone(),
            error: error.clone(),
        },

        // —— 标题 / 记忆 / Todo / Plan / Goal ——
        EventPayload::SessionTitleChanged { session_id, title } => WireEvent::SessionTitleChanged {
            session_id: session_id.clone(),
            title: title.clone(),
        },
        EventPayload::SessionTitleGenerationFailed { session_id, reason } => {
            WireEvent::SessionTitleGenerationFailed {
                session_id: session_id.clone(),
                reason: reason.clone(),
            }
        }
        EventPayload::TodoListUpdated { todos } => WireEvent::TodoListUpdated {
            todos: todos.iter().map(WireTodoItem::from).collect(),
        },
        EventPayload::PlanReady {
            plan_id,
            plan_path,
            plan_markdown,
            summary,
        } => WireEvent::PlanReady {
            plan_id: plan_id.clone(),
            plan_path: plan_path.clone(),
            plan_markdown: plan_markdown.clone(),
            summary: summary.clone(),
        },
        EventPayload::PlanCommentAdded { plan_id, comment } => WireEvent::PlanCommentAdded {
            plan_id: plan_id.clone(),
            comment: comment.clone(),
        },
        EventPayload::MemoryExtracted { session_id, items } => WireEvent::MemoryExtracted {
            session_id: session_id.clone(),
            items: items.clone(),
        },
        EventPayload::MemoryExtractionFailed { session_id, reason } => {
            WireEvent::MemoryExtractionFailed {
                session_id: session_id.clone(),
                reason: reason.clone(),
            }
        }
        EventPayload::GoalAchieved { condition, reason } => WireEvent::GoalAchieved {
            condition: condition.clone(),
            reason: reason.clone(),
        },
        EventPayload::GoalImpossible { condition, reason } => WireEvent::GoalImpossible {
            condition: condition.clone(),
            reason: reason.clone(),
        },
        EventPayload::GoalProgress { iteration, reason } => WireEvent::GoalProgress {
            iteration: *iteration,
            reason: reason.clone(),
        },

        // —— 纯内部 / 调试，不对外 ——
        EventPayload::RunStarted { .. }
        | EventPayload::RunCancelled
        | EventPayload::TurnStarted { .. }
        | EventPayload::ReasoningSignature { .. }
        | EventPayload::ReasoningDuration { .. }
        | EventPayload::Log { .. } => return None,
    };
    Some(wire)
}

// ── enum → 字符串 的集中映射（避免散落多处不一致）─────────────────────────────
// 三 surface（desktop to_wire / web to_wire / cli DaemonEvent）统一复用这些 mapper，
// 保证业务事件字段的线上形态逐字节一致（架构 §3.1.1）。

/// RiskLevel → 小写字符串（`critical` / `high` / `medium` / `low`）。
pub fn risk_str(r: &RiskLevel) -> String {
    format!("{r:?}").to_lowercase()
}

pub fn suspend_reason_str(r: &SuspendReason) -> &'static str {
    match r {
        SuspendReason::BackgroundTask => "background_task",
        SuspendReason::Cron => "cron",
        SuspendReason::Manual => "manual",
    }
}

pub fn resume_cause_str(c: &ResumeCause) -> String {
    match c {
        ResumeCause::BgTaskFinished { task_id, .. } => format!("bg_task_finished:{task_id}"),
        ResumeCause::CronFired { original_reason } => format!("cron_fired:{original_reason}"),
        ResumeCause::UserMessageArrived => "user_message_arrived".to_string(),
        ResumeCause::ManualResume => "manual_resume".to_string(),
    }
}

fn step_kind_str(k: &StepKind) -> &'static str {
    match k {
        StepKind::Model => "model",
        StepKind::Tool => "tool",
    }
}

fn stop_reason_str(s: &StopReason) -> &'static str {
    match s {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxIterations => "max_iterations",
        StopReason::PermissionDenied => "permission_denied",
        StopReason::Cancelled => "cancelled",
        StopReason::Failed => "failed",
    }
}

pub fn approval_decision_str(d: &ApprovalDecision) -> &'static str {
    match d {
        ApprovalDecision::AllowOnce => "allow_once",
        ApprovalDecision::AllowAndRemember { .. } => "allow_and_remember",
        ApprovalDecision::Deny => "deny",
        ApprovalDecision::DenyWithFeedback { .. } => "deny_with_feedback",
    }
}

/// `Notice` 的级别归并：Trace/Debug/Info → `info`（与早期 surface 行为一致）。
fn notice_level_str(l: &crate::event::LogLevel) -> &'static str {
    use crate::event::LogLevel;
    match l {
        LogLevel::Trace | LogLevel::Debug | LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

/// `UserAnswer` 降成 `(kind, text)`：text 是给客户端展示的人话拼接。
fn user_answer_kind_text(answer: &UserAnswer) -> (&'static str, String) {
    match answer {
        UserAnswer::Selected { label } => ("selected", label.clone()),
        UserAnswer::SelectedMulti { labels } => ("selected_multi", labels.join("、")),
        UserAnswer::Custom { text } => ("custom", text.clone()),
        UserAnswer::Cancelled => ("cancelled", String::new()),
        UserAnswer::Multi { items } => {
            let text = items
                .iter()
                .map(|item| format!("{}: {}", item.title, item.answer.to_agent_text()))
                .collect::<Vec<_>>()
                .join("；");
            ("multi", text)
        }
    }
}

/// `PermissionKind` 嵌套 enum 拍平的中转：把四个 variant 的散字段收齐，再组装 `WireEvent`。
#[derive(Default)]
struct PermissionFields {
    request_id: String,
    kind: String,
    tool_name: String,
    input: Value,
    summary: String,
    risk: String,
    paths: Vec<String>,
    fingerprint: Option<String>,
    command_segments: Vec<String>,
    segments: Vec<ApprovalSegment>,
    refuse_remember: bool,
    plan: Option<WirePlanPermission>,
    auto_handled: bool,
    call_id: String,
}

impl PermissionFields {
    fn from_kind(kind: &PermissionKind) -> Self {
        let mut f = Self {
            input: Value::Null,
            ..Default::default()
        };
        match kind {
            PermissionKind::ToolCall {
                tool_name,
                input,
                fingerprint,
                command_segments,
                segments,
                refuse_remember,
            } => {
                f.kind = "tool_call".to_string();
                f.tool_name = tool_name.clone();
                f.input = input.clone();
                f.fingerprint = fingerprint.clone();
                f.command_segments = command_segments.clone();
                f.segments = segments.clone();
                f.refuse_remember = *refuse_remember;
            }
            PermissionKind::PathAccess { tool_name, paths } => {
                f.kind = "path_access".to_string();
                f.tool_name = tool_name.clone();
                f.paths = paths.clone();
            }
            PermissionKind::Plan {
                plan_id,
                plan_path,
                plan_markdown,
                summary,
                ..
            } => {
                f.kind = "plan".to_string();
                f.plan = Some(WirePlanPermission {
                    plan_id: plan_id.clone(),
                    plan_path: plan_path.clone(),
                    plan_markdown: plan_markdown.clone(),
                    summary: summary.clone(),
                });
            }
            PermissionKind::ContinueLongRun { .. } => {
                f.kind = "continue_long_run".to_string();
            }
        }
        f
    }

    fn into_wire(self) -> WireEvent {
        WireEvent::PermissionRequested {
            request_id: self.request_id,
            kind: self.kind,
            tool_name: self.tool_name,
            input: self.input,
            summary: self.summary,
            risk: self.risk,
            paths: self.paths,
            fingerprint: self.fingerprint,
            command_segments: self.command_segments,
            segments: self.segments,
            refuse_remember: self.refuse_remember,
            plan: self.plan,
            auto_handled: self.auto_handled,
            call_id: self.call_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{PermissionRequestId, RunId};
    use serde_json::json;

    fn wrap(payload: EventPayload) -> Event {
        Event::now(RunId::new(), 0, payload)
    }

    /// 序列化后 JSON 的 `type` 标签与早期 desktop EngineEvent 一致。
    #[test]
    fn tool_started_serializes_as_tool_start_with_id() {
        let ev = wrap(EventPayload::ToolCallStarted {
            index: 2,
            call_id: "call-1".into(),
            name: "Bash".into(),
            input: json!({"command": "ls"}),
        });
        let wire = to_wire(&ev).unwrap();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["type"], "tool_start");
        assert_eq!(v["id"], "call-1"); // call_id → id
        assert_eq!(v["index"], 2);
        assert_eq!(v["name"], "Bash");
        assert!(v.get("call_id").is_none());
    }

    #[test]
    fn tool_finished_keeps_full_result_lossless() {
        let big = "x".repeat(2000);
        let ev = wrap(EventPayload::ToolCallFinished {
            index: 0,
            call_id: "c".into(),
            result: big.clone(),
            duration_ms: 5,
            truncated: false,
            artifact_path: None,
            is_error: false,
        });
        let wire = to_wire(&ev).unwrap();
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["type"], "tool_done");
        // 无损：不在转换层截断（cli 的 500 字符截断下沉到 render.rs）
        assert_eq!(v["result"].as_str().unwrap().len(), 2000);
    }

    #[test]
    fn run_failed_becomes_error() {
        let ev = wrap(EventPayload::RunFailed {
            error: crate::error::ErrorReport::other("boom"),
        });
        let v = serde_json::to_value(to_wire(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["message"], "boom");
    }

    #[test]
    fn permission_tool_call_flattens_kind() {
        let ev = wrap(EventPayload::PermissionRequested {
            request_id: PermissionRequestId("r1".into()),
            kind: PermissionKind::ToolCall {
                tool_name: "Bash".into(),
                input: json!({"command": "rm -rf /"}),
                fingerprint: Some("rm -rf".into()),
                command_segments: vec!["rm -rf /".into()],
                segments: vec![],
                refuse_remember: true,
            },
            summary: "危险命令".into(),
            risk: crate::event::RiskLevel::Critical,
            auto_handled: false,
            call_id: "c9".into(),
        });
        let v = serde_json::to_value(to_wire(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "permission_requested");
        assert_eq!(v["kind"], "tool_call");
        assert_eq!(v["tool_name"], "Bash");
        assert_eq!(v["risk"], "critical");
        assert_eq!(v["refuse_remember"], true);
        assert_eq!(v["call_id"], "c9");
    }

    #[test]
    fn user_answer_multi_降成_kind_text() {
        use crate::permission::{MultiQuestionAnswer, SingleAnswer};
        let ev = wrap(EventPayload::UserQuestionAnswered {
            request_id: PermissionRequestId("q1".into()),
            answer: UserAnswer::Multi {
                items: vec![MultiQuestionAnswer {
                    title: "题一".into(),
                    answer: SingleAnswer::Selected { label: "A".into() },
                }],
            },
        });
        let v = serde_json::to_value(to_wire(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "user_question_answered");
        assert_eq!(v["kind"], "multi");
        assert!(v["text"].as_str().unwrap().contains("题一"));
    }

    #[test]
    fn todo_uses_camel_active_form() {
        let ev = wrap(EventPayload::TodoListUpdated {
            todos: vec![TodoItem {
                id: "t1".into(),
                content: "做事".into(),
                active_form: "做事中".into(),
                status: crate::todo::TodoStatus::InProgress,
            }],
        });
        let v = serde_json::to_value(to_wire(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "todo_list_updated");
        assert_eq!(v["todos"][0]["activeForm"], "做事中"); // camelCase 适配
        assert_eq!(v["todos"][0]["status"], "in_progress");
    }

    #[test]
    fn subagent_call_id_inlined_from_outer_event() {
        let ev = Event::now_subagent(
            RunId::new(),
            0,
            "parent-task-1",
            EventPayload::TextDelta { text: "hi".into() },
        );
        let v = serde_json::to_value(to_wire(&ev).unwrap()).unwrap();
        assert_eq!(v["subagent_call_id"], "parent-task-1");
    }

    #[test]
    fn internal_events_not_exposed() {
        assert!(to_wire(&wrap(EventPayload::RunCancelled)).is_none());
        assert!(to_wire(&wrap(EventPayload::Log {
            level: crate::event::LogLevel::Debug,
            message: "x".into(),
        }))
        .is_none());
    }
}
