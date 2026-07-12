use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use common::attachments::MessageAttachment;
pub use common::{ReasoningConfig, ReasoningEffort};

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
    /// Anthropic thinking block 的签名（API 颁发，回填时必须原样带回，否则 400）。
    /// 非 Anthropic provider 为空字符串。
    pub reasoning_signature: String,
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
    /// 给模型看的 inline 内容。超阈值时由 dispatcher 改为「头部预览 + 工件指针」
    /// 文本，原始全量落到 `artifact.path`（架构 §4.4.9 / §4.12.11 Phase 2）。
    pub content: String,
    /// 超阈值时的 artifact 元数据；未触发落盘时为 `None`。
    pub artifact: Option<ToolArtifact>,
    /// 工具产出的多模态附件（架构 §4.4.1）。首期仅 `Read` 读图片时非空。
    /// 协议层把它编码进模型上下文（强模型原生图片块 / 弱模型 VisionBridge 转文字）。
    pub attachments: Vec<MessageAttachment>,
}

/// 工具输出落盘后的元数据。dispatcher 在 `materialize_tool_output` 里产出，
/// surface 端用 `path` 渲染「📎 完整输出 N KB」链接，Read 工具直接按
/// offset/limit 翻页（架构 §4.4.9）。
#[derive(Debug, Clone)]
pub struct ToolArtifact {
    pub path: std::path::PathBuf,
    pub bytes: u64,
    pub line_count: Option<u32>,
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
    TextDelta {
        text: String,
    },
    /// 思维链 / 推理过程增量。Anthropic 的 `thinking_delta`、
    /// OpenAI / DeepSeek / Qwen 等的 `reasoning_content` 都映射到这一路。
    ReasoningDelta {
        text: String,
    },
    /// Anthropic thinking block 的签名（流式路径下 `signature_delta` 帧，一次性整体到达）。
    /// 其他 provider 不发此事件。
    ReasoningSignature {
        signature: String,
    },
    /// 思考（thinking block）的墙钟时长，在该块结束（`content_block_stop`）时 emit 一次。
    /// OAuth 直连官方时 thinking 文本被清空、拿不到 `ReasoningDelta`，但 block 的
    /// start/stop 边界仍在，故时长是这条路上唯一能展示的「思考用时」信号。
    ReasoningDuration {
        ms: u64,
    },
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

/// 一次模型调用的**内部上下文**（落盘 model_io / 打日志用，**不发往 provider**——各
/// provider 的 build_body 只读 model/system/entries/tools/max_tokens/reasoning，天然不会
/// 把 meta 外泄）。让 model-gateway 层能统一打 `[model]` 日志、按 tag 落 model_io，而不管是
/// 哪个调用点（主 chat / judge / 压缩 / 旁支…）发起的（架构 §4.11）。
#[derive(Debug, Clone, Default)]
pub struct ModelCallMeta {
    /// 所属会话 id（跨 surface 共享的对话标识）。
    pub session_id: Option<String>,
    /// 所属 run id。
    pub run_id: Option<String>,
    /// 轮次（同一 run 内多次模型调用递增）。
    pub turn: u32,
    /// 触发本次调用的 assistant message id（主 chat 有；judge / 旁支 / 派生调用为 None）。
    pub message_id: Option<String>,
    /// 调用类别——区分主 chat / judge / 压缩 / 标题 / 旁支等，落盘与日志据此打 tag。
    pub tag: ModelCallTag,
}

/// 模型调用的类别标签（架构 §4.11）。主 chat 不额外标记（`Main`），其余子调用各自成 tag，
/// 让 model_io / 日志能区分「agent 替我跑的各类模型调用」。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelCallTag {
    /// 主对话（前端 ModelIoInspector 约定：tag=main 不打额外标签）。
    #[default]
    Main,
    /// AutoMode 判官。
    Judge,
    /// Bash 段前缀分类器（Classifier A）。
    Classifier,
    /// 内置浏览器旁支会话。
    Aside,
    /// 上下文压缩摘要。
    Compaction,
    /// 会话标题生成。
    Title,
    /// //goal 完成度裁决。
    Goal,
    /// 记忆抽取。
    Memory,
    /// 视觉桥接（图像理解 / 转描述）。
    Vision,
    /// Task 工具派生的子 agent（NestedRun，架构 §4.4.11）。
    Subagent,
}

impl ModelCallTag {
    /// 落盘 / 日志用的短标签。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Judge => "judge",
            Self::Classifier => "classifier",
            Self::Aside => "aside",
            Self::Compaction => "compaction",
            Self::Title => "title",
            Self::Goal => "goal",
            Self::Memory => "memory",
            Self::Vision => "vision",
            Self::Subagent => "subagent",
        }
    }
}

impl std::fmt::Display for ModelCallTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 发送给模型的统一请求
#[derive(Debug, Clone, Default)]
pub struct ModelRequest {
    pub model: String,
    pub system: Option<String>,
    pub entries: Vec<TranscriptEntry>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    /// 推理 / thinking 行为。`None` = 沿用模型默认（多数模型默认关闭）。
    /// 由 surface 层（[`ModelWithName`] 等 wrapper）按 session 配置注入。
    pub reasoning: Option<ReasoningConfig>,
    /// 仅远端 compact 请求使用：当 provider 支持 `/responses/compact` 时，
    /// 用它提示服务端把本次调用视为一次自动压缩。
    pub compact_prompt_cache_key: Option<String>,
    /// 内部调用上下文（落盘 / 日志用，不发往 provider）。见 [`ModelCallMeta`]。
    pub meta: ModelCallMeta,
}

/// 模型「非工具调用结束」的归一原因（架构 §4.11.4）。各 provider 把原始
/// `finish_reason`/`stop_reason` 映射到这里，让 `agent_loop` 用统一口径判断
/// 是否正常退出。`ToolUse` 不在此枚举——由 [`ModelResponse::ToolCalls`] 变体表达。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FinishReason {
    /// 正常完成。唯一静默项；其余都会被 surface 成 toast + continue 入口。
    #[default]
    Stop,
    /// 被 max_tokens 截断，回答不完整——可续写。
    Length,
    /// 模型主动拒答。
    Refusal,
    /// 被内容安全策略拦截。
    ContentFilter,
    /// 未识别的原始结束值，原文透传给 UI 兜底。
    Other(String),
}

/// 模型完成响应
#[derive(Debug, Clone)]
pub enum ModelResponse {
    Done {
        text: String,
        /// 这一轮累计的思维链。对接 transcript 时会回填，让下一轮模型看到。
        #[doc(hidden)]
        reasoning: String,
        /// Anthropic thinking block 的签名（非流式路径从响应体读取）。其他 provider 为空。
        reasoning_signature: String,
        attachments: Vec<MessageAttachment>,
        usage: Usage,
        /// 归一后的结束原因（架构 §4.11.4）。非 `Stop` 时 `agent_loop` toast + 写 pending_continue。
        finish: FinishReason,
    },
    ToolCalls {
        text: String,
        reasoning: String,
        /// Anthropic thinking block 的签名（非流式路径从响应体读取）。其他 provider 为空。
        reasoning_signature: String,
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

/// Token 用量统计。
///
/// `input_tokens` / `output_tokens` 始终是「计费 token」总数（与 provider 账单对齐）。
/// `cache_read_tokens` 是命中缓存读出来的部分，**已计入** `input_tokens`，单独展示
/// 给用户用来评估缓存命中率（命中越高越省钱）。`cache_creation_tokens` 是这次写入
/// 缓存花的输入（只在 Anthropic 上有意义；OpenAI / DeepSeek 没有显式 creation 计费）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 命中前缀缓存的部分，**已包含在 `input_tokens` 中**。
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// 写入前缀缓存的部分，**已包含在 `input_tokens` 中**。
    /// Anthropic 的 `cache_creation_input_tokens`；其他 provider 通常为 0。
    #[serde(default)]
    pub cache_creation_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn accumulate(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
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

    /// Run 进入挂起态——不是真错误，是 agent_loop 通过 Err 路径 break 出去
    /// 让 harness 走"任务退出但 Run 未结束"分支（架构 §4.12）。
    #[error("已挂起，等待 wakeup")]
    Suspended,

    #[error("{0}")]
    Other(String),
}

impl ModelError {
    /// Provider 返回的上下文超限错误。不同厂商错误体没有统一字段，统一在 gateway
    /// 类型层集中识别，agent-core 只依赖这个结构化判断，不在业务路径散落字符串匹配。
    pub fn is_context_too_long(&self) -> bool {
        let body = match self {
            ModelError::Http { status, body } if *status == 400 || *status == 413 => body,
            ModelError::Other(body) => body,
            _ => return false,
        };
        let body = body.to_ascii_lowercase();
        [
            "prompt_too_long",
            "context_length_exceeded",
            "context length",
            "maximum context",
            "max context",
            "context window",
            "input tokens",
            "too many tokens",
            "token limit",
            "request too large",
        ]
        .iter()
        .any(|needle| body.contains(needle))
    }
}

#[cfg(test)]
mod model_error_tests {
    use super::ModelError;

    #[test]
    fn detects_provider_context_too_long_errors() {
        let anthropic = ModelError::Http {
            status: 400,
            body: "prompt_too_long: input tokens exceed context window".to_string(),
        };
        let openai = ModelError::Http {
            status: 400,
            body: "context_length_exceeded: maximum context length is 128000".to_string(),
        };
        let payload = ModelError::Http {
            status: 413,
            body: "request too large".to_string(),
        };

        assert!(anthropic.is_context_too_long());
        assert!(openai.is_context_too_long());
        assert!(payload.is_context_too_long());
    }

    #[test]
    fn does_not_treat_rate_limit_as_context_too_long() {
        let rate_limited = ModelError::Http {
            status: 429,
            body: "too many requests".to_string(),
        };
        assert!(!rate_limited.is_context_too_long());
    }
}
