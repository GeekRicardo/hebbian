use serde::Serialize;

/// Tauri command 返回类型：session 的 todo 列表（lib.rs `list_todos`）。
/// 字段对齐前端 TodoItem（`activeForm` camelCase、status 字符串）。
#[derive(Debug, Clone, Serialize)]
pub struct TodoItemDto {
    pub id: String,
    pub content: String,
    #[serde(rename = "activeForm")]
    pub active_form: String,
    /// "pending" / "in_progress" / "completed"
    pub status: String,
}

/// Tauri command 返回类型：plan 评论（lib.rs `list_plan_comments` / `add_plan_comment`）。
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
