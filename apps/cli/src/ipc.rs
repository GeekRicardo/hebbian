//! IPC 协议：daemon ↔ client 通过 Unix socket 交互。
//!
//! 传输格式：每次交互一条 JSON line（\n 结尾），client 发 IpcCommand，
//! daemon 回 IpcResponse。daemon 向 stdout 持续输出 DaemonEvent NDJSON。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 客户端 → daemon 的命令
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcCommand {
    /// 发送用户输入（运行中则注入当前 run，否则开新 run）
    Send { text: String },
    /// 强制注入到运行中的 run（无 active run 时报错）
    Inject { text: String },
    /// 批准审批
    Allow {
        request_id: String,
        /// "once" | "session" | "project" | "global"
        #[serde(default = "default_once")]
        scope: String,
        /// 命令前缀（Bash 命令级记忆）；scope != "once" 时生效
        #[serde(default)]
        pattern: Option<String>,
        /// compound 命令场景的额外段前缀（架构 §4.4.2）。
        /// 例：`cd /tmp && touch foo` 用户想一次性允许两段 →
        /// `pattern = "cd"`, `extra_patterns = ["touch"]`。
        #[serde(default)]
        extra_patterns: Vec<String>,
    },
    /// 拒绝审批
    Deny { request_id: String },
    /// 拒绝并注入反馈
    DenyWithFeedback {
        request_id: String,
        feedback: String,
    },
    /// 回答 agent 提问
    Answer {
        request_id: String,
        /// "selected" | "custom" | "cancelled"
        kind: String,
        /// selected → option label；custom → 自由文本；cancelled → 空
        #[serde(default)]
        value: String,
    },
    /// 停止当前 run（设 cancel flag）
    Stop,
    /// 切换 run mode
    Mode { mode: String },
    /// 检测 daemon 存活
    Ping,
    /// 读当前 session 的 model_io.jsonl：返回 `data: [DumpEntry, ...]`
    /// 给 AI 脚本调试 / hebweb 后端做数据源，避免每个 surface 自己解析 jsonl
    ListModelIo,
}

fn default_once() -> String {
    "once".to_string()
}

/// daemon → client 的响应（每条命令对应一条）
#[derive(Debug, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl IpcResponse {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            data: None,
        }
    }
    pub fn err(msg: impl ToString) -> Self {
        Self {
            ok: false,
            error: Some(msg.to_string()),
            data: None,
        }
    }
    pub fn with_data(data: Value) -> Self {
        Self {
            ok: true,
            error: None,
            data: Some(data),
        }
    }
}

/// daemon 持续输出到 stdout 的事件（NDJSON，每行一个 JSON 对象）
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DaemonEvent {
    /// daemon 启动完成，输出 session_id
    Started {
        session_id: String,
    },
    RunStarted,
    RunFinished {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        duration_ms: u64,
    },
    RunFailed {
        error: String,
    },
    RunCancelled,
    RunSuspended {
        reason: String,
    },
    RunResumed {
        cause: String,
    },
    TextDelta {
        text: String,
        /// 子 NestedRun 事件来源标识（架构 §4.4.11.8）。`Some` 时前端按此字段嵌套渲染到父 Task 卡片内。
        #[serde(skip_serializing_if = "Option::is_none", default)]
        subagent_call_id: Option<String>,
    },
    TextDone {
        full_text: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        subagent_call_id: Option<String>,
    },
    Reasoning {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        subagent_call_id: Option<String>,
    },
    ToolStart {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        subagent_call_id: Option<String>,
    },
    /// 工具执行中的流式输出片段（架构 §4.4.1）。Bash 前台等待时按 chunk 推过来；
    /// 自动化脚本可以 tail 这个看命令实时进度，不必等 ToolDone。
    ToolOutputDelta {
        id: String,
        chunk: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        subagent_call_id: Option<String>,
    },
    ToolDone {
        id: String,
        result: String,
        duration_ms: u64,
        /// 这次调用以失败收场（执行错误 / 入参解析失败 / 被拒 / Bash 退出码非 0）。
        #[serde(default)]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        subagent_call_id: Option<String>,
    },
    PermissionRequested {
        request_id: String,
        kind: String,
        tool_name: String,
        summary: String,
        risk: String,
        /// ToolCall 命令级记忆指纹（架构 §4.4.2）；只 Bash 当前会带
        #[serde(skip_serializing_if = "Option::is_none", default)]
        fingerprint: Option<String>,
        /// Bash compound 命令的所有段 fingerprint，让 CLI 看到全段后能用
        /// `--pattern X --extra-pattern Y` 一次允许多前缀
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        command_segments: Vec<String>,
        /// ToolCall 时的工具入参（命令本身、文件路径等），便于 AI 调试看清在审批啥
        #[serde(skip_serializing_if = "Option::is_none", default)]
        input: Option<serde_json::Value>,
        /// PathAccess 越界路径列表
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        paths: Vec<String>,
        /// 这条审批是否会被 AutoMode judge 接管（§4.4.4）。`true` 时脚本**不应**抢着
        /// `heb allow/deny`——judge 会异步出结果（随后 emit PermissionAutoJudged），抢答会
        /// 旁路判官决策；`false` 才是真正需要人工的审批。
        #[serde(default)]
        auto_handled: bool,
        /// 触发本次审批的工具调用 id（ToolCall 审批填，其余为空串），便于脚本关联工具卡。
        #[serde(skip_serializing_if = "String::is_empty", default)]
        call_id: String,
    },
    PermissionResolved {
        request_id: String,
        decision: String,
    },
    /// AutoMode judge 对一条审批的裁决（§4.4.4）。脚本据此知道「agent 替我自动判了什么」；
    /// `requires_human=true` 表示这条仍需 `heb allow/deny` 人工拍板（ASK / 命令类 DENY）。
    PermissionAutoJudged {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        request_id: Option<String>,
        tool_name: String,
        /// `allow` / `deny` / `ask`
        decision: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        reason: Option<String>,
        requires_human: bool,
    },
    QuestionRequested {
        request_id: String,
        question: String,
        options: Vec<QuestionOptionDto>,
        multi: bool,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        questions: Vec<AskQuestionDto>,
    },
    QuestionAnswered {
        request_id: String,
    },
    RunModeChanged {
        from: String,
        to: String,
    },
    /// 新会话首轮跑完后，agent_core 后台 task 生成的标题已落盘 jsonl。
    /// CLI 自动化脚本可监听这条做侧边栏 / 提示更新；落盘已由 agent_core 完成，
    /// 客户端不需要再回写。
    SessionTitleChanged {
        session_id: String,
        title: String,
    },
    /// 新会话首轮的后台自动标题生成失败（模型连不上 / 鉴权过期 / 返回空等）。
    /// 与 `SessionTitleChanged` 互斥。自动化脚本可据此判定标题没生成、提示手动重试。
    SessionTitleGenerationFailed {
        session_id: String,
        reason: String,
    },
    /// 本 Run（整个 agent_loop）结束后汇总的文件净变化（架构 §4.13）。
    /// `files[]` 每项含 real_path / action（create|modify|overwrite|delete）/ before_bytes / after_bytes。
    /// 无文件变化的 Run 不发本事件。旧脚本忽略未知 event。
    RunEditsCommitted {
        run_id: String,
        files: Vec<Value>,
    },
    /// 一个 Run 跑完后，agent_core 后台记忆抽取写入了若干条记忆（架构 §4.14）。
    /// 自动化脚本可监听这条核对落盘结果；记忆已由 agent_core 写盘，客户端无需回写。
    MemoryExtracted {
        session_id: String,
        items: Vec<protocol::MemoryWriteItem>,
    },
    /// 后台记忆抽取的 fallback 模型链全部失败（架构 §4.14）。游标未推进，下个 Run 补抽。
    MemoryExtractionFailed {
        session_id: String,
        reason: String,
    },
    /// 轻量通知（架构 §4.4.4）。例：AutoMode 模型不在白名单 → 转手动审批提示。
    /// Desktop 渲染成 toast；CLI 脚本可监听核对降级行为。
    Notice {
        level: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dedup_key: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOptionDto {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskQuestionDto {
    pub title: String,
    pub description: String,
    pub options: Vec<QuestionOptionDto>,
    pub multi: bool,
}

impl From<protocol::QuestionOption> for QuestionOptionDto {
    fn from(o: protocol::QuestionOption) -> Self {
        Self {
            label: o.label,
            description: o.description,
        }
    }
}

impl From<protocol::AskQuestion> for AskQuestionDto {
    fn from(q: protocol::AskQuestion) -> Self {
        Self {
            title: q.title,
            description: q.description,
            options: q.options.into_iter().map(Into::into).collect(),
            multi: q.multi,
        }
    }
}
