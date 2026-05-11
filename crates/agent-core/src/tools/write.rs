//! Write 工具：创建或覆盖文件。
//!
//! - destructive：默认走 PermissionGate
//! - 自动创建父目录
//! - 路径必须在 workspace 允许范围内

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};
use tokio::fs;

use super::Tool;
use crate::workspace::Workspace;

const MAX_CONTENT_BYTES: usize = 5 * 1024 * 1024;

pub struct WriteTool;

impl WriteTool {
    pub fn new(_workspace: Arc<Workspace>) -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "创建或覆盖一个文件。如果父目录不存在会自动创建。\
         需要审批（覆盖会丢失原文件内容）。\
         路径必须在对话允许的目录范围内。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path", "content"],
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "文件绝对路径"
                },
                "content": {
                    "type": "string",
                    "description": "文件完整内容（会覆盖原文件）"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let file_path_str = input["file_path"]
            .as_str()
            .ok_or_else(|| AppError::msg("Write: 缺少 file_path"))?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| AppError::msg("Write: 缺少 content"))?;

        if content.len() > MAX_CONTENT_BYTES {
            return Err(AppError::msg(format!(
                "Write: content 过大（{} 字节，>{}MB）",
                content.len(),
                MAX_CONTENT_BYTES / 1024 / 1024
            )));
        }

        let file_path = PathBuf::from(file_path_str);
        // 越界检查在 agent_loop 统一做

        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::msg(format!("Write: 创建父目录失败 {e}")))?;
        }

        let existed = fs::try_exists(&file_path).await.unwrap_or(false);
        fs::write(&file_path, content)
            .await
            .map_err(|e| AppError::msg(format!("Write: 写入失败 {file_path_str}: {e}")))?;

        let line_count = content.lines().count();
        let action = if existed { "覆盖" } else { "创建" };
        Ok(format!(
            "已{action}文件 {file_path_str}（{} 字节，{line_count} 行）",
            content.len()
        ))
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_at(path: &std::path::Path) -> Arc<Workspace> {
        Workspace::new(path, Vec::new())
    }

    #[tokio::test]
    async fn write_creates_file_and_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("nested/dir/out.txt");
        let tool = WriteTool::new(workspace_at(tmp.path()));

        let result = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "content": "hello",
            }))
            .await
            .unwrap();
        assert!(result.contains("创建"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    }

    #[tokio::test]
    async fn write_overwrites_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a.txt");
        std::fs::write(&target, "old").unwrap();
        let tool = WriteTool::new(workspace_at(tmp.path()));

        let result = tool
            .execute(json!({
                "file_path": target.to_string_lossy(),
                "content": "new",
            }))
            .await
            .unwrap();
        assert!(result.contains("覆盖"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }

}
