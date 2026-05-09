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
    /// workspace 越界路径访问审批（Bash/Read/Write/Grep）
    PathAccess {
        tool_name: String,
        paths: Vec<String>,
    },
    /// 计划审批（"按这个计划继续吗？"）
    Plan { steps: Vec<String> },
    /// 长 run 继续审批
    ContinueLongRun { iterations_used: u32 },
}

// ── Ask：agent 主动向用户提问 ────────────────────────────────────────────────

/// 一个候选选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    /// 短标签（按钮文字 / 选项行首），建议 1-12 字
    pub label: String,
    /// 详细说明（可空），用于 hover / 子行展示
    #[serde(default)]
    pub description: String,
}

/// 用户对一次 ask 的回应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserAnswer {
    /// 单选：选了某个固定选项（带回 label）
    Selected { label: String },
    /// 多选：选了若干固定选项（带回 labels，按用户勾选顺序）
    SelectedMulti { labels: Vec<String> },
    /// 用户在自由输入框写的文字
    Custom { text: String },
    /// 用户取消（TUI 中按 ESC、UI 关闭弹窗等）
    Cancelled,
}

impl UserAnswer {
    /// 把答案规约成将要注入下一轮的 tool_result 文本
    pub fn to_agent_text(&self) -> String {
        match self {
            UserAnswer::Selected { label } => format!("用户选择：{label}"),
            UserAnswer::SelectedMulti { labels } => {
                if labels.is_empty() {
                    "[用户未选任何选项]".to_string()
                } else {
                    format!("用户选择（多选）：{}", labels.join("、"))
                }
            }
            UserAnswer::Custom { text } => format!("用户输入：{text}"),
            UserAnswer::Cancelled => "[用户取消了提问]".to_string(),
        }
    }
}
