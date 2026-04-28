use serde_json::Value;

/// Hook 触发时机
#[derive(Debug, Clone)]
pub enum HookPoint {
    /// agent run 刚启动，上下文还没注入
    BeforeRun,
    /// agent run 完成（无论成功/失败/取消）
    AfterRun,
    /// 一次 turn 开始（一次模型调用 + 工具执行批）
    BeforeTurn { turn: u32 },
    /// 一次 turn 结束
    AfterTurn { turn: u32 },
    /// 模型调用前
    BeforeModelCall { turn: u32 },
    /// 模型调用后
    AfterModelCall { turn: u32 },
    /// 工具调用前（可在此拦截）
    BeforeToolCall { tool_name: String, input: Value },
    /// 工具调用后
    AfterToolCall { tool_name: String, result: String },
    /// 在向用户发起审批前（可用来插入二次校验、自动拒绝等）
    BeforePermissionRequest { tool_name: String },
    /// 上下文压缩发生时
    OnContextCompaction,
}

/// Hook 的处理结果
#[derive(Debug, Clone)]
pub enum HookOutcome {
    /// 不做任何修改，继续流程
    Continue,
    /// 修改系统提示词前缀（例如 memory 注入）
    PrependSystem(String),
    /// 阻断流程并返回错误
    Block(String),
}
