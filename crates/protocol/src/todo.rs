//! Todo / Plan comment 协议类型（架构 §4.4.6 / §4.4.5）。
//!
//! - [`TodoItem`]：TodoWrite 工具维护的单项 todo。前后端共享同一结构。
//! - [`PlanComment`]：PlanMode 下用户对 plan markdown 加的评论；落盘到
//!   `~/.hebbian/sessions/<sid>/plans/<plan_id>.comments.jsonl`，
//!   下一轮 user message 发送时拼到 SEMI 段并 mark consumed。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    /// ulid，前后端贯通，可用于跨次 TodoWrite 增量更新（v1 仍按整列表覆盖）。
    pub id: String,
    pub content: String,
    /// 进行时形式（"Running tests"）。模型按 TodoWrite 协议要求同时提供两种态。
    #[serde(default)]
    pub active_form: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanComment {
    pub id: String,
    pub plan_id: String,
    /// 锚定 plan markdown 中的某段，v1 为纯文本（如 "L12-15" / 选段首尾 7 字）。
    /// v2 再考虑 selection range / 精确 char offset。
    pub anchor: String,
    pub body: String,
    pub created_at_ms: i64,
    /// 是否已注入到下一轮 user message。append-only 文件，状态翻转通过追加
    /// `PlanCommentConsumed` 行或重写文件实现（见 storage::plan_comments）。
    #[serde(default)]
    pub consumed: bool,
}
