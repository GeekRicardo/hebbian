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

    /// Run 进入挂起态（架构 §4.12）。模型调 `WaitForTask` / `ScheduleWakeup` 后
    /// 当前 ToolStep 完成、agent_loop 落 RunCheckpoint 并退出 task 时 emit。
    /// surface 看到这条不要清 slot——稍后会有 `RunResumed`。
    RunSuspended {
        reason: SuspendReason,
        /// cron 路径：什么时刻自动唤醒（Unix epoch ms）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumes_at_ms: Option<i64>,
        /// bg-task 路径：等哪些后台 task_id。v1 数组里至多一项。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waiting_for_task_ids: Vec<String>,
    },
    /// Run 从挂起态恢复。surface 用 `cause` 在 UI 标明唤醒原因（bg 完成 / cron 触发 /
    /// 用户消息 / 手动 resume）。
    RunResumed {
        cause: ResumeCause,
    },

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
        /// 工具输出超阈值时落到磁盘的工件路径（架构 §4.4.9 / §4.12.11 Phase 2）。
        /// surface 端用它渲染「📎 完整输出」可点链接；模型从 `result` 文本里的
        /// 指针拿到等价信息。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_path: Option<String>,
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

    /// 新会话首轮跑完后，agent_core 异步生成的标题已落盘。
    /// surface 端拿这条更新侧边栏 / 头部标题——无需再主动 invoke 重生成。
    /// 携带 `session_id` 是因为标题属于 session 级状态而非 run 级（一个 session
    /// 的多个 run 共用 title），surface 端可能在切换会话时仍要消费这条。
    SessionTitleChanged {
        session_id: String,
        title: String,
    },

    // —— 编辑快照（§4.13） ——
    EditSnapshotCreated {
        call_id: String,
        snapshot_id: String,
        file_path: String,
        action: EditAction,
        before_sha: String,
        after_sha: String,
        before_bytes: u64,
        after_bytes: u64,
    },
    EditReverted {
        snapshot_id: String,
        file_path: String,
    },
    EditRevertFailed {
        snapshot_id: String,
        file_path: String,
        error: String,
    },

    // —— 调试 ——
    Log {
        level: LogLevel,
        message: String,
    },
}

/// Edit 工具的操作类型（§4.13.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditAction {
    Create,
    Overwrite,
    Modify,
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

/// Run 挂起的原因（架构 §4.12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspendReason {
    /// 等 BackgroundShell task 完成（`WaitForTask` 工具触发）。
    BackgroundTask,
    /// 等定时唤醒（`ScheduleWakeup` 工具触发）。
    Cron,
    /// 模型其他显式挂起（保留）。
    Manual,
}

/// Run 被唤醒的原因（架构 §4.12）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResumeCause {
    /// 关联 task 完成。
    BgTaskFinished {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    /// cron 到点。
    CronFired {
        /// 原 `ScheduleWakeup` 的 reason 字符串。
        original_reason: String,
    },
    /// 用户在 session 发了新消息，触发了 resume。
    UserMessageArrived,
    /// surface 手动点 resume。
    ManualResume,
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
