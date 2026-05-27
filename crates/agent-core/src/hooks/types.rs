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
    /// Turn 自然结束（模型 stop_reason=end_turn 且无 pending tool_call）。
    ///
    /// 与 Claude Code 2.1 / Codex codex-rs::hooks 的 Stop 语义对齐：用于挂"后置 verify"
    /// 脚本（cargo check / tsc / 跑测试），脚本失败时返回 [`HookOutcome::InjectFollowup`]
    /// 让 agent_loop 续跑修复（架构 §4.8.3）。外部 cancel 走 [`HookPoint::Notification`]
    /// `{ level: "cancel" }`，不再占用 Stop。
    ///
    /// `workdir` 是 session 的工作目录绝对路径——外部 hook spawn 子进程时设为
    /// `current_dir(workdir)`，让 `cargo check` / `pnpm tsc` 这类相对路径命令在
    /// 用户项目根目录里跑。`None` 时不设 cwd（继承 daemon 启动目录，仅在 surface
    /// 没绑定 workspace 的场景出现）。
    Stop {
        session_id: String,
        reason: String,
        workdir: Option<String>,
    },
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
    /// 注入一段 reminder 文本作为下一轮 user message，让 agent 续跑（架构 §4.8.3）。
    ///
    /// 仅在 [`HookPoint::Stop`] 点位由 agent_loop 消费——拿到后包成
    /// `<hook-feedback source="<event>">...</hook-feedback>` push 进 transcript，
    /// 不退出 loop，进入下一 turn。其它点位返回该值视为 [`Continue`] 忽略。
    ///
    /// 防死循环：每个 Run 最多注入 `MAX_STOP_INJECTIONS = 3` 次，超过即放弃注入正常出 turn。
    ///
    /// [`Continue`]: HookOutcome::Continue
    InjectFollowup(String),
}
