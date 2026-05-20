//! IPC 协议：daemon ↔ client 通过 Unix socket 交互。
//!
//! 传输格式：每次交互一条 JSON line（\n 结尾），client 发 IpcCommand，
//! daemon 回 IpcResponse。daemon 向 stdout 持续输出 DaemonEvent NDJSON。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 客户端 → daemon 的命令
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcCommand {
    /// 发送用户输入（运行中则注入当前 run，否则开新 run）
    Send { text: String },
    /// 强制注入到运行中的 run（无 active run 时报错）
    Inject { text: String },
    /// 批准审批
    Allow {
        request_id: String,
        /// "once" | "session" | "project" | "global"
        #[serde(default = "default_once")]
        scope: String,
        /// 命令前缀（Bash 命令级记忆）；scope != "once" 时生效
        #[serde(default)]
        pattern: Option<String>,
        /// compound 命令场景的额外段前缀（架构 §4.4.2）。
        /// 例：`cd /tmp && touch foo` 用户想一次性允许两段 →
        /// `pattern = "cd"`, `extra_patterns = ["touch"]`。
        #[serde(default)]
        extra_patterns: Vec<String>,
    },
    /// 拒绝审批
    Deny { request_id: String },
    /// 拒绝并注入反馈
    DenyWithFeedback { request_id: String, feedback: String },
    /// 回答 agent 提问
    Answer {
        request_id: String,
        /// "selected" | "custom" | "cancelled"
        kind: String,
        /// selected → option label；custom → 自由文本；cancelled → 空
        #[serde(default)]
        value: String,
    },
    /// 停止当前 run（设 cancel flag）
    Stop,
    /// 切换 run mode
    Mode { mode: String },
    /// 检测 daemon 存活
    Ping,
}

fn default_once() -> String {
    "once".to_string()
}

/// daemon → client 的响应（每条命令对应一条）
#[derive(Debug, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl IpcResponse {
    pub fn ok() -> Self {
        Self { ok: true, error: None, data: None }
    }
    pub fn err(msg: impl ToString) -> Self {
        Self { ok: false, error: Some(msg.to_string()), data: None }
    }
    pub fn with_data(data: Value) -> Self {
        Self { ok: true, error: None, data: Some(data) }
    }
}

/// daemon 持续输出到 stdout 的事件（NDJSON，每行一个 JSON 对象）
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DaemonEvent {
    /// daemon 启动完成，输出 session_id
    Started { session_id: String },
    RunStarted,
    RunFinished {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        duration_ms: u64,
    },
    RunFailed { error: String },
    RunCancelled,
    RunSuspended { reason: String },
    RunResumed { cause: String },
    TextDelta { text: String },
    TextDone { full_text: String },
    Reasoning { text: String },
    ToolStart { id: String, name: String, input: Value },
    ToolDone { id: String, result: String, duration_ms: u64 },
    PermissionRequested {
        request_id: String,
        kind: String,
        tool_name: String,
        summary: String,
        risk: String,
    },
    PermissionResolved { request_id: String, decision: String },
    QuestionRequested {
        request_id: String,
        question: String,
        options: Vec<QuestionOptionDto>,
        multi: bool,
    },
    QuestionAnswered { request_id: String },
    RunModeChanged { from: String, to: String },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOptionDto {
    pub label: String,
    pub description: String,
}
