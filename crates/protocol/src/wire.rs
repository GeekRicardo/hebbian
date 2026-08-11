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
    /// 思考块的墙钟时长，块结束时到达一次（架构 §3.1.1）。历史加载时前端用这个落盘值
    /// 定格「思考用时 N 秒」，与流式期间的客户端秒表对齐——修「刷新后思考用时数字变」。
    ReasoningDuration {
        ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    /// Anthropic thinking block 的签名（流式一次性整体到达）。surface 端更新内存里最后一个
    /// Reasoning part 的 signature，落盘随消息持久化。
    ReasoningSignature {
        signature: String,
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
        /// 结果因超阈值被截断（模型看到的 `result` 是截断版，完整版在 `artifact_path`）。
        /// surface 据此渲染「(已截断)」标记——三 surface 现在都能看到，不再只有 CLI TUI。
        #[serde(default)]
        truncated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },

    // —— 生命周期 ——
    /// 一个 run 开始（架构 §3.1.1）。surface-session 在装配活 run 后 emit，携带 run 身份
    /// 与触发源——**不再被 to_wire 丢弃**。前端据 `trigger` 区分是用户发起还是后端自主
    /// （wakeup/cron/队列），后端自发起的 run 也能第一时间建投影、亮运行态。
    RunStarted {
        run_id: String,
        /// `user` / `wakeup` / `cron` / `queue` / `resume`
        trigger: String,
        /// 当前 RunMode（`default` / `plan` / `auto`）。
        mode: String,
    },
    /// 一条消息刚落盘（架构 §3.1 / 提案 P2）。`message` 与 session.jsonl / getSession 的
    /// Message 形态一致，前端投影用同一套解析：user/wakeup 通知气泡实时出现、流式 assistant
    /// 按 id 原地定稿。老前端不识别此 type、忽略之（additive）。
    MessageAppended {
        message: Value,
    },
    /// 注：`duration_ms` + 四个 token 累计。token 之前被 to_wire 丢弃、各 surface 各自
    /// 重算，现补回无损（架构 §3.1.1）。
    RunFinished {
        duration_ms: u64,
        #[serde(default)]
        total_input_tokens: u64,
        #[serde(default)]
        total_output_tokens: u64,
        #[serde(default)]
        total_cache_read_tokens: u64,
        #[serde(default)]
        total_cache_creation_tokens: u64,
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
        /// 人话字符串形态（保留兼容）：`bg_task_finished:{task_id}` / `cron_fired:{reason}` /
        /// `user_message_arrived` / `manual_resume`。
        cause: String,
        /// bg-task 唤醒时关联的后台 task_id（结构化，之前被拼进字符串丢失）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// bg-task 退出码（结构化，之前被 to_wire 整个丢弃）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
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
    MemoryRecalled {
        session_id: String,
        items: Vec<MemoryWriteItem>,
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

/// 事件信封（架构 §3.1）：给每条 [`WireEvent`] 盖上 session 级单调 `seq` + `epoch` + `run_id`。
///
/// - **seq**：session 内单调递增（从 1 起），epoch 内连续无洞。订阅方据此去重、续传、检测 gap。
/// - **epoch**：runtime 创建时的时间戳。runtime 被重建（进程重启 / registry remove 后 ensure）
///   → epoch 变化 → 订阅方必须 resync（seq 从头，不能跨 epoch 比较）。
/// - **run_id**：本事件所属 run；session 级事件（标题 / 记忆等派生态）为 `None`。
///
/// meta 字段用下划线前缀 `#[serde(flatten)]` 到 WireEvent 的 JSON 上（`_epoch`/`_seq`/`_rid`/`_ts`），
/// 老前端只读 `type` 字段、忽略这些——**对现有 surface 完全透明**（P1 additive），新前端读 `_seq`
/// 做投影对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEnvelope {
    #[serde(rename = "_epoch")]
    pub epoch: u64,
    #[serde(rename = "_seq")]
    pub seq: u64,
    #[serde(rename = "_rid", default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(rename = "_ts")]
    pub ts_ms: i64,
    #[serde(flatten)]
    pub event: WireEvent,
}

/// 一个 run 的触发源（架构 §3.1）。surface-session 装配活 run 时据此填
/// [`WireEvent::RunStarted::trigger`]，前端区分用户发起 vs 后端自主。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTrigger {
    /// 用户在 UI / CLI 主动发消息。
    User,
    /// 后台任务完成唤醒。
    Wakeup,
    /// cron 定时到点唤醒。
    Cron,
    /// 排队消息在上一个 run 结束后起的新 run。
    Queue,
    /// 挂起 checkpoint 恢复（进程重启后 resume）。
    Resume,
}

impl RunTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunTrigger::User => "user",
            RunTrigger::Wakeup => "wakeup",
            RunTrigger::Cron => "cron",
            RunTrigger::Queue => "queue",
            RunTrigger::Resume => "resume",
        }
    }
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
        EventPayload::ReasoningDuration { ms } => WireEvent::ReasoningDuration {
            ms: *ms,
            subagent_call_id: sub,
        },
        EventPayload::ReasoningSignature { signature } => WireEvent::ReasoningSignature {
            signature: signature.clone(),
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
            truncated,
        } => WireEvent::ToolDone {
            index: *index,
            id: call_id.clone(),
            result: result.clone(),
            duration_ms: *duration_ms,
            artifact_path: artifact_path.clone(),
            is_error: *is_error,
            truncated: *truncated,
            subagent_call_id: sub,
        },

        // —— 生命周期 ——
        EventPayload::RunFinished {
            duration_ms,
            total_input_tokens,
            total_output_tokens,
            total_cache_read_tokens,
            total_cache_creation_tokens,
        } => WireEvent::RunFinished {
            duration_ms: *duration_ms,
            total_input_tokens: *total_input_tokens,
            total_output_tokens: *total_output_tokens,
            total_cache_read_tokens: *total_cache_read_tokens,
            total_cache_creation_tokens: *total_cache_creation_tokens,
        },
        EventPayload::RunFailed { error } => WireEvent::Error {
            message: error.message.clone(),
        },
        EventPayload::MessageAppended { message } => WireEvent::MessageAppended {
            message: message.clone(),
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
        EventPayload::RunResumed { cause } => {
            let (task_id, exit_code) = match cause {
                ResumeCause::BgTaskFinished { task_id, exit_code } => {
                    (Some(task_id.clone()), *exit_code)
                }
                _ => (None, None),
            };
            WireEvent::RunResumed {
                cause: resume_cause_str(cause),
                task_id,
                exit_code,
            }
        }

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
        EventPayload::MemoryRecalled { session_id, items } => WireEvent::MemoryRecalled {
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
        // RunStarted 由 surface-session 带 trigger 单独 emit（to_wire 拿不到 trigger），
        // 这里的内部 RunStarted 仍丢弃，避免重复。
        EventPayload::RunStarted { .. }
        | EventPayload::RunCancelled
        | EventPayload::TurnStarted { .. }
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

    /// 信封 flatten 序列化：meta 字段用 `_` 前缀与 WireEvent 的 `type` 同层共存，
    /// 老前端只读 `type`、忽略 `_seq`——P1 additive 的关键（架构 §3.1）。
    #[test]
    fn envelope_flattens_meta_alongside_type() {
        let env = SessionEnvelope {
            epoch: 42,
            seq: 7,
            run_id: Some("run_abc".into()),
            ts_ms: 1234,
            event: WireEvent::TextDelta {
                text: "hi".into(),
                subagent_call_id: None,
            },
        };
        let v = serde_json::to_value(&env).unwrap();
        // WireEvent 的判别字段仍在顶层（老前端据此渲染，不受信封影响）。
        assert_eq!(v["type"], "text_delta");
        assert_eq!(v["text"], "hi");
        // 信封 meta 拍平在旁，下划线前缀不与任何 WireEvent 字段冲突。
        assert_eq!(v["_epoch"], 42);
        assert_eq!(v["_seq"], 7);
        assert_eq!(v["_rid"], "run_abc");
        assert_eq!(v["_ts"], 1234);
        // 往返：Rust 端也能读回（in-memory broadcast 用 clone，这里只验 serde 正确性）。
        let back: SessionEnvelope = serde_json::from_value(v).unwrap();
        assert_eq!(back.seq, 7);
        assert!(matches!(back.event, WireEvent::TextDelta { text, .. } if text == "hi"));
    }

    /// run_id 字段是 WireEvent 里已有的（RunEditsCommitted 等），不能被信封的 `_rid` 覆盖。
    #[test]
    fn envelope_meta_does_not_collide_with_wire_run_id() {
        let env = SessionEnvelope {
            epoch: 1,
            seq: 1,
            run_id: Some("outer".into()),
            ts_ms: 0,
            event: WireEvent::RunEditsReverted {
                run_id: "inner".into(),
            },
        };
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["run_id"], "inner", "WireEvent 自己的 run_id 不被信封覆盖");
        assert_eq!(v["_rid"], "outer", "信封 run_id 走 _rid");
    }

    /// MessageAppended 无损透传 message payload（提案 P2）。
    #[test]
    fn message_appended_passes_through() {
        let ev = wrap(EventPayload::MessageAppended {
            message: json!({"id": "msg_1", "role": "user", "content": "hi"}),
        });
        let v = serde_json::to_value(to_wire(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "message_appended");
        assert_eq!(v["message"]["id"], "msg_1");
        assert_eq!(v["message"]["content"], "hi");
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
