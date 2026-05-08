use serde_json::Value;

/// Hook 触发时机。
///
/// 只保留**能改变 control flow 或数据**的拦截点；纯观察类需求（"run 启动"、
/// "turn 结束"、"工具开始"等）一律走 [`Event`] 流（surface 通过 [`RunHandle::recv`]
/// 或 `Harness::subscribe` 订阅）。
///
/// [`Event`]: protocol::Event
/// [`RunHandle::recv`]: crate::RunHandle::recv
#[derive(Debug, Clone)]
pub enum HookPoint {
    /// 模型调用前：可改写 ModelRequest（注入记忆 / 改 system / 加 tool 定义）。
    BeforeModelCall { turn: u32 },
    /// 工具审批检查时：可旁路 HitlGate（学习规则 / 自动审批 / 强制询问）。
    OnPermissionCheck { tool_name: String, input: Value },
    /// 工具执行返回后：可改写 ToolResult（截短 / 落 blob / 脱敏）。
    OnToolResult { tool_name: String, content: String },
    /// 上下文压缩发生时：可替换默认压缩策略。
    OnCompaction {
        before_tokens: usize,
        after_tokens: usize,
    },
}

/// Hook 的处理结果。
#[derive(Debug, Clone)]
pub enum HookOutcome {
    /// 不做修改，继续流程。
    Continue,
    /// 在 system prefix 加一段（memory 注入用）。
    PrependSystem(String),
    /// 阻断当前操作并返回错误。
    Block(String),
}
