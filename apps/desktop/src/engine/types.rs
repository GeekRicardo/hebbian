// engine/types.rs
use serde::Serialize;
use serde_json::Value;

/// 引擎事件——通过 Tauri Channel 从后端流式推送到前端
///
/// 前端监听这些事件来更新对话 UI：
/// - 收到 TextDelta → 追加文字到消息气泡
/// - 收到 ToolStart → 显示"正在调用工具…"提示
/// - 收到 ToolDone  → 显示工具执行结果摘要
/// - 收到 TextDone  → 流式结束，保存完整消息
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    /// AI 生成的文字增量（流式一片一片推过来）
    TextDelta { text: String },
    /// 本轮对话的完整文本（流式结束信号）
    TextDone { full_text: String },
    /// 模型流式吐出的工具调用片段
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    },
    /// AI 准备调用工具，前端可以显示加载态
    ToolStart {
        index: usize,
        id: String,
        name: String,
        input: Value,
    },
    /// 工具执行完毕，result 是前 200 字的摘要
    ToolDone {
        index: usize,
        id: String,
        result: String,
        duration_ms: u64,
    },
    /// 出错
    Error { message: String },
}

/// AI 发起的工具调用请求
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// 由 AI 分配的工具调用唯一 ID（不同 API 格式会有不同命名）
    pub id: String,
    /// 工具名称，如 "web_search"
    pub name: String,
    /// 工具的输入参数（JSON 对象）
    pub input: Value,
}

/// 单次 AI 请求的两种可能结果
pub enum TurnResult {
    /// AI 直接回复了文字，不需要工具
    Done(String),
    /// AI 要求调用工具，text 是调用前可能存在的前置文字
    ToolCalls { text: String, calls: Vec<ToolCall> },
}

/// Tool trait——re-export 自 agent_core，保持向后兼容
///
/// 所有工具实现此 trait 即可被 engine 和 agent_core 共同使用。
pub use crate::agent_core::tools::Tool;
