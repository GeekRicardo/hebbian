use serde::{Deserialize, Serialize};

/// 用户对一次审批请求的回应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// 批准这一次
    AllowOnce,
    /// 批准并记住（同 scope 内的同类调用以后不再问）
    AllowAndRemember { scope: PermissionScope },
    /// 拒绝
    Deny,
    /// 拒绝并把反馈作为 user message 注入下一轮
    DenyWithFeedback { feedback: String },
}

/// 审批记忆生效的范围
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    /// 仅本次 run
    Run,
    /// 整个会话
    Session,
    /// 当前项目
    Project,
    /// 全局（所有项目）
    Global,
}

/// 审批请求的类别（用于 UI 渲染分类）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionKind {
    /// 工具调用审批
    ToolCall {
        tool_name: String,
        input: serde_json::Value,
    },
    /// 计划审批（"按这个计划继续吗？"）
    Plan { steps: Vec<String> },
    /// 长 run 继续审批
    ContinueLongRun { iterations_used: u32 },
}
