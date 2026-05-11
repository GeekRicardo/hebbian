use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ErrorReport;
use crate::ids::{AgentRef, PermissionRequestId, RunId, TurnId};
use crate::permission::{ApprovalDecision, PermissionKind};

/// Core 向外的统一输出。一个 run 的事件流是有序、可重放、自描述的。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub run_id: RunId,
    /// 在该 run 内单调递增（per-run，不是全局）
    pub seq: u64,
    /// Unix epoch milliseconds
    pub at_ms: i64,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    // —— 生命周期 ——
    RunStarted {
        agent: AgentRef,
        #[serde(default)]
        parent: Option<RunId>,
    },
    RunFinished {
        total_input_tokens: u64,
        total_output_tokens: u64,
        /// 命中前缀缓存读出的 token 数（已计入 `total_input_tokens`）。
        #[serde(default)]
        total_cache_read_tokens: u64,
        /// 写入前缀缓存的 token 数（Anthropic 专属，已计入 `total_input_tokens`）。
        #[serde(default)]
        total_cache_creation_tokens: u64,
        duration_ms: u64,
    },
    RunFailed {
        error: ErrorReport,
    },
    RunCancelled,

    // —— 单个 turn ——
    TurnStarted {
        turn_id: TurnId,
        turn: u32,
    },
    TurnFinished {
        turn_id: TurnId,
        turn: u32,
        stop_reason: StopReason,
    },

    // —— Step 粒度（架构 §4.2）：ModelStep = 一次 model.stream 调用；
    //    ToolStep = 一批 tool_call 并发执行 ——
    StepStarted {
        step_kind: StepKind,
        step_index: u32,
    },
    StepFinished {
        step_kind: StepKind,
        step_index: u32,
    },

    /// 运行模式切换（架构 §10.2）。actor 收到 [`Op::SwitchRunMode`] 后 emit。
    RunModeChanged {
        from: String,
        to: String,
    },

    // —— 模型流 ——
    TextDelta {
        text: String,
    },
    TextDone {
        full_text: String,
    },
    Reasoning {
        text: String,
    },

    // —— 工具 ——
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    },
    ToolCallStarted {
        index: usize,
        call_id: String,
        name: String,
        input: Value,
    },
    ToolCallFinished {
        index: usize,
        call_id: String,
        result: String,
        duration_ms: u64,
        #[serde(default)]
        truncated: bool,
    },

    // —— 人机协作：审批 ——
    PermissionRequested {
        request_id: PermissionRequestId,
        kind: PermissionKind,
        summary: String,
        risk: RiskLevel,
    },
    PermissionResolved {
        request_id: PermissionRequestId,
        decision: ApprovalDecision,
    },
    /// AutoMode 判官自动给出决策（架构 §4.4.4）。surface 端用来在 UI 上提示
    /// 「agent 替我决定了 X」，并落进 jsonl 作为审计证据。
    PermissionAutoJudged {
        tool_name: String,
        /// `allow` / `deny` / `ask`
        decision: String,
        /// 模型给的简短理由。`Allow` 时通常为空。
        #[serde(default)]
        reason: Option<String>,
    },

    // —— 人机协作：agent 主动提问 ——
    UserQuestionRequested {
        request_id: PermissionRequestId,
        question: String,
        options: Vec<crate::permission::QuestionOption>,
        /// 是否允许多选（true = 用户可勾选多个选项）
        #[serde(default)]
        multi: bool,
    },
    UserQuestionAnswered {
        request_id: PermissionRequestId,
        answer: crate::permission::UserAnswer,
    },

    // —— 上下文 ——
    ContextCompacted {
        before_tokens: usize,
        after_tokens: usize,
    },

    // —— 调试 ——
    Log {
        level: LogLevel,
        message: String,
    },
}

/// Step 粒度（架构 §4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// 一次 model.stream / model.complete 调用。
    Model,
    /// 一批 tool_call 的并发执行。
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxIterations,
    PermissionDenied,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Event {
    pub fn now(run_id: RunId, seq: u64, payload: EventPayload) -> Self {
        Self {
            run_id,
            seq,
            at_ms: chrono::Utc::now().timestamp_millis(),
            payload,
        }
    }
}
