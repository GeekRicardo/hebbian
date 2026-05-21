use serde::Serialize;
use serde_json::Value;

/// 桌面端引擎事件——通过 Tauri Channel 序列化推送给前端。
///
/// 维护注意：此枚举必须与前端 `types.ts` 的 `EngineEvent` 类型保持字段级同步。
/// 新增/修改 EventPayload variant 时需同时更新：
/// 1. protocol::event::EventPayload（crates/protocol/src/event.rs）
/// 2. 本文件 EngineEvent
/// 3. chat.rs agent_event_to_engine_event 翻译函数
/// 4. frontend types.ts EngineEvent + useStore.ts applyEventToSlot
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    TextDelta {
        text: String,
    },
    TextDone {
        full_text: String,
    },
    /// 模型的思维链 / 推理过程增量（DeepSeek `reasoning_content`、
    /// Anthropic `thinking_delta` 等）。前端通常以折叠块单独渲染。
    Reasoning {
        text: String,
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
        /// 工具输出超阈值时落盘的工件路径（架构 §4.4.9）。surface 用它在
        /// MessageBubble 渲染「📎 完整输出」可点链接。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_path: Option<String>,
    },
    /// Run 进入挂起态（架构 §4.12）。surface 据此渲染 BackgroundTaskPanel 占位。
    RunSuspended {
        /// "background_task" / "cron" / "manual"
        reason: String,
        /// cron 路径：自动唤醒时间（Unix ms）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumes_at_ms: Option<i64>,
        /// bg-task 路径：等的 task_id 列表。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waiting_for_task_ids: Vec<String>,
    },
    /// Run 从挂起态恢复。
    RunResumed {
        /// 简短描述：bg_task_finished / cron_fired / user_message_arrived / manual_resume
        cause: String,
    },
    /// 工具需要用户审批（HITL）。前端弹出审批 UI 后通过
    /// `approve_permission` / `approve_path_access` 命令回应。
    PermissionRequested {
        request_id: String,
        /// "tool_call" / "path_access" / "plan" / "continue_long_run"
        kind: String,
        tool_name: String,
        input: Value,
        summary: String,
        risk: String,
        /// PathAccess 时的越界路径列表
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        paths: Vec<String>,
        /// ToolCall 时的命令级记忆指纹（[`protocol::PermissionKind::ToolCall::fingerprint`]）。
        /// 仅 BashTool 当前会带，UI 据此渲染"记住 git status / 记住 git"两档按钮。
        #[serde(skip_serializing_if = "Option::is_none", default)]
        fingerprint: Option<String>,
        /// Bash / PowerShell 的所有段 fingerprint（架构 §4.4.2）。
        /// compound 命令 `cd /tmp && touch foo` → `["cd /tmp", "touch foo"]`。
        /// UI 据此展示每段独立 allow 按钮 + 「整条都允许」按钮。
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        command_segments: Vec<String>,
    },
    /// 审批已被回应（无论 approve / deny）。前端关闭弹窗。
    PermissionResolved {
        request_id: String,
        decision: String, // "allow_once" / "allow_and_remember" / "deny" / "deny_with_feedback"
    },
    /// AutoMode judge 自动给出决策（架构 §4.4.4）。前端可在消息流里渲染
    /// 「AutoMode 自动放行 / 拒绝 / 转人工」，作为审计证据。
    PermissionAutoJudged {
        tool_name: String,
        /// "allow" / "deny" / "ask"
        decision: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        reason: Option<String>,
    },
    /// Step 粒度起始（架构 §4.2）。step_kind = `"model"` 表示一次模型调用，
    /// `"tool"` 表示一批 tool_call。前端可用 metrics / 进度条。
    StepStarted {
        step_kind: String,
        step_index: u32,
    },
    StepFinished {
        step_kind: String,
        step_index: u32,
    },
    /// 运行模式切换（架构 §10.2）。前端用来刷新状态栏 mode 标签。
    RunModeChanged {
        from: String,
        to: String,
    },
    /// agent 主动向用户提问（ask 工具）。前端弹出选项 + 自由输入框，用户回应通过
    /// `answer_question` Tauri 命令回到 core。
    UserQuestionRequested {
        request_id: String,
        question: String,
        options: Vec<QuestionOptionDto>,
        /// 是否允许多选
        #[serde(default)]
        multi: bool,
    },
    /// 用户已回应提问。前端关闭弹窗。
    UserQuestionAnswered {
        request_id: String,
        /// "selected" / "selected_multi" / "custom" / "cancelled"
        kind: String,
        /// selected 时是 label，selected_multi 时是 "、" 拼接的 labels，
        /// custom 时是 text，cancelled 时为空
        text: String,
    },
    /// Edit 工具快照已创建（架构 §4.13）。前端 EditTree 用它对文件操作排序展示。
    EditSnapshotCreated {
        call_id: String,
        snapshot_id: String,
        file_path: String,
        /// "create" / "overwrite" / "modify"
        action: String,
        before_sha: String,
        after_sha: String,
        before_bytes: u64,
        after_bytes: u64,
    },
    /// Edit 回退成功。
    EditReverted {
        snapshot_id: String,
        file_path: String,
    },
    /// Edit 回退失败。
    EditRevertFailed {
        snapshot_id: String,
        file_path: String,
        error: String,
    },
    /// 新会话首轮跑完后，agent_core 后台 task 异步生成的标题已落盘 jsonl。
    /// 前端用它更新 sidebar / chat header；落盘已由 agent_core 完成，前端只需 setState。
    SessionTitleChanged {
        session_id: String,
        title: String,
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
