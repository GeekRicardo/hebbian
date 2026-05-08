use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use platform::attachments::MessageAttachment;

pub const IMAGE_GENERATION_TOOL_NAME: &str = "image_generation";

pub fn has_image_generation_tool(tools: &[ToolDefinition]) -> bool {
    tools
        .iter()
        .any(|tool| tool.name == IMAGE_GENERATION_TOOL_NAME)
}

// ── 规范化的会话消息 ──────────────────────────────────────────────────────────

/// 单轮会话条目（model gateway 的规范化消息格式）
#[derive(Debug, Clone)]
pub enum TranscriptEntry {
    User(UserEntry),
    Assistant(AssistantEntry),
    ToolResults(Vec<ToolResult>),
}

#[derive(Debug, Clone)]
pub struct UserEntry {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
}

impl UserEntry {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AssistantEntry {
    pub text: String,
    /// 上一轮模型的思维链 / 推理过程（DeepSeek `reasoning_content` /
    /// chat.deepseek.com `<think>` block 等），下一轮重发时回填给模型。
    /// 不参与 UI 显示——UI 走 `MessagePart::Reasoning`。
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallStreamDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStreamEvent {
    TextDelta { text: String },
    /// 思维链 / 推理过程增量。Anthropic 的 `thinking_delta`、
    /// OpenAI / DeepSeek / Qwen 等的 `reasoning_content` 都映射到这一路。
    ReasoningDelta { text: String },
    ToolCallDelta(ToolCallStreamDelta),
}

// ── 工具定义 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

// ── 请求 / 响应 ───────────────────────────────────────────────────────────────

/// 发送给模型的统一请求
#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub model: String,
    pub system: Option<String>,
    pub entries: Vec<TranscriptEntry>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
}

/// 模型完成响应
#[derive(Debug, Clone)]
pub enum ModelResponse {
    Done {
        text: String,
        /// 这一轮累计的思维链。对接 transcript 时会回填，让下一轮模型看到。
        #[doc(hidden)]
        reasoning: String,
        attachments: Vec<MessageAttachment>,
        usage: Usage,
    },
    ToolCalls {
        text: String,
        reasoning: String,
        calls: Vec<ToolCall>,
        attachments: Vec<MessageAttachment>,
        usage: Usage,
    },
}

impl ModelResponse {
    pub fn usage(&self) -> &Usage {
        match self {
            Self::Done { usage, .. } | Self::ToolCalls { usage, .. } => usage,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AssistantOutput {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
}

/// Token 用量统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn accumulate(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
    }
}

// ── 错误 ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("请求失败: {0}")]
    Request(#[from] reqwest::Error),

    #[error("JSON 解析失败: {0}")]
    Json(#[from] serde_json::Error),

    #[error("已取消")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}
