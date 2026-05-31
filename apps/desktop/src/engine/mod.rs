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
        /// 子 NestedRun 事件来源标识（架构 §4.4.11.8）。前端按此字段嵌套渲染到父 Task 卡片内部。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    TextDone {
        full_text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    /// 模型的思维链 / 推理过程增量（DeepSeek `reasoning_content`、
    /// Anthropic `thinking_delta` 等）。前端通常以折叠块单独渲染。
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    ToolStart {
        index: usize,
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
    },
    /// 工具执行中的流式输出片段（架构 §4.4.1）。Bash 前台等待期间的
    /// stdout/stderr 增量按 chunk 推过来；前端把 chunk 追加到对应工具卡片的
    /// 实时输出区，`ToolDone.result` 仍是聚合后的最终文本。
    ToolOutputDelta {
        index: usize,
        id: String,
        chunk: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_call_id: Option<String>,
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
        /// Plan 审批专属：plan markdown + 元信息，仅 kind="plan" 时填（架构 §4.4.5）。
        /// 前端用它在 PermissionApprovalPopup 里渲染完整 plan 预览 + 三按钮
        /// （通过 / 编辑后通过 / 重新规划带反馈）。
        #[serde(skip_serializing_if = "Option::is_none", default)]
        plan: Option<PlanPermissionDto>,
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
    /// 给用户的一次性轻量通知（前端渲染成 toast）。例：AutoMode 模型不在白名单时
    /// 提示「已转手动审批」。`dedup_key` 非空时前端按它去重避免刷屏。
    Notice {
        /// "info" / "warn" / "error"
        level: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        dedup_key: Option<String>,
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
    /// Turn 边界——一次"模型请求 + 可选 tool_call 批"结束（架构 §3 / §4.2）。
    /// surface 用它把 streaming bubble 冻结成"已完成 turn 快照"；下一个 Turn 的输出
    /// 起一个新的 streaming bubble，从而保证 streaming 中的插队 user message 总是
    /// 落在它真正回应的那个 Turn 之后、下一个 Turn 之前。
    TurnFinished {
        /// "end_turn" / "max_iterations" / "cancelled"
        stop_reason: String,
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
    /// TodoWrite 工具更新了 todo 列表（架构 §4.4.6）。前端用整列表覆盖右
    /// sidebar 的 Todos tab。落盘由 agent_core 完成，前端只需刷视图。
    TodoListUpdated {
        todos: Vec<TodoItemDto>,
    },
    /// PlanMode 下 ExitPlanMode 落盘了一份 plan markdown，紧随后会有
    /// `PermissionRequested { kind: "plan" }` 走审批闸口（架构 §4.4.5）。
    PlanReady {
        plan_id: String,
        plan_path: String,
        plan_markdown: String,
        summary: String,
    },
    /// 用户在 plan tab / 审批 popup 加了一条 plan 评论。surface 追加到面板；
    /// 下一轮 user message 发送时 agent_core 把未消费 comments 包成
    /// `<plan_comments>` 段注入。
    PlanCommentAdded {
        plan_id: String,
        comment: PlanCommentDto,
    },
    /// 一个 Run 跑完后，后台记忆抽取写入了若干条记忆（架构 §4.14）。
    /// 前端在该会话末尾渲染一行「本轮写入 N 条记忆」摘要，可展开看明细。
    MemoryExtracted {
        session_id: String,
        items: Vec<protocol::MemoryWriteItem>,
    },
    /// 后台记忆抽取的 fallback 模型链全部失败（架构 §4.14）。前端弹 toast 提示。
    MemoryExtractionFailed {
        session_id: String,
        reason: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanPermissionDto {
    pub plan_id: String,
    pub plan_path: String,
    pub plan_markdown: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TodoItemDto {
    pub id: String,
    pub content: String,
    #[serde(rename = "activeForm")]
    pub active_form: String,
    /// "pending" / "in_progress" / "completed"
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanCommentDto {
    pub id: String,
    pub plan_id: String,
    pub anchor: String,
    pub body: String,
    pub created_at_ms: i64,
    pub consumed: bool,
}

impl From<protocol::todo::TodoItem> for TodoItemDto {
    fn from(t: protocol::todo::TodoItem) -> Self {
        Self {
            id: t.id,
            content: t.content,
            active_form: t.active_form,
            status: match t.status {
                protocol::todo::TodoStatus::Pending => "pending".into(),
                protocol::todo::TodoStatus::InProgress => "in_progress".into(),
                protocol::todo::TodoStatus::Completed => "completed".into(),
            },
        }
    }
}

impl From<protocol::todo::PlanComment> for PlanCommentDto {
    fn from(c: protocol::todo::PlanComment) -> Self {
        Self {
            id: c.id,
            plan_id: c.plan_id,
            anchor: c.anchor,
            body: c.body,
            created_at_ms: c.created_at_ms,
            consumed: c.consumed,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionOptionDto {
    pub label: String,
    pub description: String,
}
