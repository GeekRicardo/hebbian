//! TodoWrite 工具（架构 §4.4.6）。
//!
//! 模型用它维护一份"当前 session 的 todo 列表"以组织长任务。每次调用
//! **整列表覆盖**前一次——简化心智，避免增量合并的 corner case。
//!
//! 落盘 + 事件 emit 由 [`crate::dispatch`] 的 short-circuit 分支处理；
//! 本工具本身只做：解析 input + 校验 + 返回一行汇总文本（"Updated 5 todos:
//! 2 completed, 1 in_progress, 2 pending"）。Dispatcher 落盘后 emit
//! [`protocol::EventPayload::TodoListUpdated`]。
//!
//! 为什么不在 execute 里直接落盘：execute 不持有 `data_dir + session_id`
//! 上下文，加这两个参数会污染 Tool trait（其他工具用不到）。Dispatcher 已经
//! 有这两个参数，让它处理落盘 / emit 是最干净的分工——与 [`crate::tools::hitl`]
//! 把 HITL 路由收到 dispatcher 同款思路。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use common::{AppError, AppResult};
use protocol::todo::{TodoItem, TodoStatus};

use crate::tools::Tool;

pub const TODO_WRITE_TOOL_NAME: &str = "TodoWrite";

#[derive(Debug, Deserialize)]
pub struct TodoWriteInput {
    pub todos: Vec<TodoInputItem>,
}

#[derive(Debug, Deserialize)]
pub struct TodoInputItem {
    #[serde(default)]
    pub id: Option<String>,
    pub content: String,
    /// 模型协议字段名（驼峰）与协议 [`TodoItem::active_form`] 对齐。
    #[serde(rename = "activeForm")]
    pub active_form: String,
    pub status: TodoStatus,
}

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        TODO_WRITE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Maintain a structured todo list for the current session. \
         Use it for any task with 3+ steps to track progress, organize work, \
         and surface status to the user. Each call REPLACES the prior list \
         (send the full set every time). Each item carries: \
         `content` (imperative form, e.g. \"Run tests\"), \
         `activeForm` (present continuous, e.g. \"Running tests\"), and \
         `status` ∈ pending / in_progress / completed. \
         Exactly one item should be `in_progress` at a time."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["todos"],
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["content", "activeForm", "status"],
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Optional stable id (ulid-like). Reuse it across calls \
                                                to update the same item; omit for new items."
                            },
                            "content": {
                                "type": "string",
                                "description": "Imperative form. e.g. \"Run tests\""
                            },
                            "activeForm": {
                                "type": "string",
                                "description": "Present continuous form. e.g. \"Running tests\""
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            }
                        }
                    }
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let parsed = parse_input(input)?;
        let todos = normalize(parsed.todos);
        Ok(summary(&todos))
    }
}

/// 解析 model 传入的 input。错误会被转成 `AppError::msg`。
pub fn parse_input(input: Value) -> AppResult<TodoWriteInput> {
    serde_json::from_value::<TodoWriteInput>(input)
        .map_err(|e| AppError::msg(format!("invalid TodoWrite input: {e}")))
}

/// 把 input items 规约成协议 [`TodoItem`]——补 id（缺时用 content+activeForm 的
/// 稳定 hash）、修剪空白。
///
/// **为什么要稳定 id 而不是随机**：sidebar 需要把"同一次任务清单的多次更新（pending →
/// in_progress → completed）"识别为**同一个块**。判定依据是 todo id 重叠——如果模型
/// 不传 id 我们就发随机 id，每次都不同，会被前端误认为新建块。content+activeForm
/// 的 hash 保证模型按同样描述重传时 id 一致。
pub fn normalize(items: Vec<TodoInputItem>) -> Vec<TodoItem> {
    items
        .into_iter()
        .map(|it| {
            let content = it.content.trim().to_string();
            let active_form = it.active_form.trim().to_string();
            let id = it
                .id
                .unwrap_or_else(|| stable_todo_id(&content, &active_form));
            TodoItem {
                id,
                content,
                active_form,
                status: it.status,
            }
        })
        .collect()
}

/// 汇总文本：`"Updated 5 todos (2 completed, 1 in_progress, 2 pending)"`。
pub fn summary(todos: &[TodoItem]) -> String {
    let mut completed = 0;
    let mut in_progress = 0;
    let mut pending = 0;
    for t in todos {
        match t.status {
            TodoStatus::Completed => completed += 1,
            TodoStatus::InProgress => in_progress += 1,
            TodoStatus::Pending => pending += 1,
        }
    }
    format!(
        "Updated {} todos ({} completed, {} in_progress, {} pending)",
        todos.len(),
        completed,
        in_progress,
        pending
    )
}

/// 从 content + active_form 计算稳定 id——同 content 重传保证 id 相同。
/// 用 FNV-1a 32-bit hash 足够：碰撞概率在 session 内 <50 条 todo 量级可忽略。
fn stable_todo_id(content: &str, active_form: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in content.bytes().chain([0u8]).chain(active_form.bytes()) {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("td-{hash:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn execute_returns_summary() {
        let tool = TodoWriteTool;
        let input = json!({
            "todos": [
                { "content": "Write tests", "activeForm": "Writing tests", "status": "in_progress" },
                { "content": "Ship feature", "activeForm": "Shipping feature", "status": "pending" },
            ]
        });
        let out = tool.execute(input).await.unwrap();
        assert!(out.contains("Updated 2 todos"));
        assert!(out.contains("1 in_progress"));
        assert!(out.contains("1 pending"));
    }

    #[test]
    fn normalize_fills_missing_id() {
        let items = vec![TodoInputItem {
            id: None,
            content: "  trim me  ".into(),
            active_form: "trimming".into(),
            status: TodoStatus::Pending,
        }];
        let out = normalize(items);
        assert_eq!(out[0].content, "trim me");
        assert!(out[0].id.starts_with("td-"));
    }

    /// 稳定 id：同 content + activeForm 重传应得到同 id——
    /// sidebar 才能把"pending → completed"识别为同一条 todo 而非新建。
    #[test]
    fn stable_id_idempotent_for_same_content() {
        let a = stable_todo_id("写代码", "正在写代码");
        let b = stable_todo_id("写代码", "正在写代码");
        assert_eq!(a, b);
        let c = stable_todo_id("跑测试", "正在跑测试");
        assert_ne!(a, c);
    }
}
