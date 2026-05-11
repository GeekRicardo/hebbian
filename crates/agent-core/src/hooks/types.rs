use serde_json::Value;

/// Hook 触发时机（架构 §4.8）。点位分两组：
///
/// **内置 4 点（可改 state）**：core 内部 Rust 代码注册的高耦合 hook
/// - [`BeforeModelCall`]：改 ModelRequest
/// - [`OnPermissionCheck`]：旁路 HitlGate
/// - [`OnToolResult`]：改写 result
/// - [`OnCompaction`]：替换压缩策略
///
/// **外部 11 点（CodeIsland 标准）**：用户在 `~/.hebbian/hooks.json` 注册的
/// socket+JSON 外部脚本钩子，与 CodeIsland / Claude Code / Cursor 等生态互操作
/// - [`SessionStart`] / [`SessionEnd`]
/// - [`UserPromptSubmit`]
/// - [`PreToolUse`] / [`PostToolUse`] / [`PostToolUseFailure`]
/// - [`PermissionRequest`]
/// - [`PreCompact`] / [`PostCompact`]
/// - [`Notification`]
/// - [`Stop`]
///
/// [`BeforeModelCall`]: HookPoint::BeforeModelCall
/// [`SessionStart`]: HookPoint::SessionStart
/// [`Event`]: protocol::Event
/// [`RunHandle::recv`]: crate::RunHandle::recv
#[derive(Debug, Clone)]
pub enum HookPoint {
    // —— 内置 4 点（可改 state） ——
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

    // —— 外部 11 点（CodeIsland 互操作） ——
    /// Session 创建时（架构 §4.8.1）。
    SessionStart { session_id: String, workdir: String },
    /// Session 关闭时。
    SessionEnd { session_id: String },
    /// 用户提交一条 user message 之前（可拦截 / 改写）。
    UserPromptSubmit { session_id: String, text: String },
    /// 派发器执行工具之前（可拒绝 / 改 input）。
    PreToolUse {
        session_id: String,
        tool_name: String,
        input: Value,
    },
    /// 工具执行成功之后（可改 result）。
    PostToolUse {
        session_id: String,
        tool_name: String,
        result: String,
    },
    /// 工具执行失败之后（可拦截错误）。
    PostToolUseFailure {
        session_id: String,
        tool_name: String,
        error: String,
    },
    /// HITL 审批请求触发时（可旁路自动 Allow/Deny）。
    PermissionRequest {
        session_id: String,
        tool_name: String,
        input: Value,
    },
    /// 压缩之前（可拒绝压缩 / 自定义策略）。
    PreCompact {
        session_id: String,
        strategy: String,
    },
    /// 压缩之后。
    PostCompact {
        session_id: String,
        before_tokens: usize,
        after_tokens: usize,
    },
    /// 运行时通知（错误 / 长任务进度）。
    Notification {
        session_id: String,
        level: String,
        message: String,
    },
    /// 外部请求停止（cancel）。
    Stop { session_id: String, reason: String },
}

impl HookPoint {
    /// 点位名（serde-friendly，对外 JSON 协议用）。
    pub fn event_name(&self) -> &'static str {
        match self {
            HookPoint::BeforeModelCall { .. } => "BeforeModelCall",
            HookPoint::OnPermissionCheck { .. } => "OnPermissionCheck",
            HookPoint::OnToolResult { .. } => "OnToolResult",
            HookPoint::OnCompaction { .. } => "OnCompaction",
            HookPoint::SessionStart { .. } => "SessionStart",
            HookPoint::SessionEnd { .. } => "SessionEnd",
            HookPoint::UserPromptSubmit { .. } => "UserPromptSubmit",
            HookPoint::PreToolUse { .. } => "PreToolUse",
            HookPoint::PostToolUse { .. } => "PostToolUse",
            HookPoint::PostToolUseFailure { .. } => "PostToolUseFailure",
            HookPoint::PermissionRequest { .. } => "PermissionRequest",
            HookPoint::PreCompact { .. } => "PreCompact",
            HookPoint::PostCompact { .. } => "PostCompact",
            HookPoint::Notification { .. } => "Notification",
            HookPoint::Stop { .. } => "Stop",
        }
    }
}

/// Hook 改写 payload 的补丁（架构 §4.8.2 / §4.8.4）。
///
/// 字段都是可选的：hook 想改哪个填哪个。dispatcher 在对应点位按字段类型识别：
/// - `input`：PreToolUse 改写工具入参
/// - `result`：PostToolUse 改写工具结果
/// - `system_prefix`：BeforeModelCall 前置一段到 system prompt（与原 PrependSystem 等价）
#[derive(Debug, Clone, Default)]
pub struct HookPatch {
    /// PreToolUse：改写工具入参。
    pub input: Option<Value>,
    /// PostToolUse：改写工具结果文本。
    pub result: Option<String>,
    /// BeforeModelCall：在 system prompt 前置一段。
    pub system_prefix: Option<String>,
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
    /// 改写当前 payload。dispatcher 按点位类型识别 patch 内可用字段。
    Modify(HookPatch),
}
