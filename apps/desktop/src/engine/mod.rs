use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    TextDelta {
        text: String,
    },
    TextDone {
        full_text: String,
    },
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    },
    ToolStart {
        index: usize,
        id: String,
        name: String,
        input: Value,
    },
    ToolDone {
        index: usize,
        id: String,
        result: String,
        duration_ms: u64,
    },
    /// 工具需要用户审批（HITL）。前端弹出审批 UI 后通过
    /// `approve_permission` 命令回应。
    PermissionRequested {
        request_id: String,
        tool_name: String,
        input: Value,
        summary: String,
        risk: String,
    },
    /// 审批已被回应（无论 approve / deny）。前端关闭弹窗。
    PermissionResolved {
        request_id: String,
        decision: String, // "allow_once" / "allow_and_remember" / "deny" / "deny_with_feedback"
    },
    /// agent 主动向用户提问（ask 工具）。前端弹出选项 + 自由输入框，用户回应通过
    /// `answer_question` Tauri 命令回到 core。
    UserQuestionRequested {
        request_id: String,
        question: String,
        options: Vec<QuestionOptionDto>,
    },
    /// 用户已回应提问。前端关闭弹窗。
    UserQuestionAnswered {
        request_id: String,
        /// "selected" / "custom" / "cancelled"
        kind: String,
        /// selected 时是 label，custom 时是 text，cancelled 时为空
        text: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionOptionDto {
    pub label: String,
    pub description: String,
}
