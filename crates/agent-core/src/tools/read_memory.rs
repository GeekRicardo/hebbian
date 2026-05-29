//! `ReadMemory` 工具（架构 §4.14）：按 id 读一条记忆的详情。
//!
//! id 来自首条 user message 注入的 `<memory-index>` 块（L0 清单）。模型扫一眼摘要
//! 决定要不要看详情，再用本工具取 L1 概览或 L2 全文——把「向量初筛」换成「模型自己挑」。

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};

use common::{AppError, AppResult};

use super::Tool;
use crate::storage::memory::{self, MemoryLevel};

pub const READ_MEMORY_TOOL_NAME: &str = "ReadMemory";

pub struct ReadMemoryTool {
    data_dir: Option<PathBuf>,
    /// 当前对话绑定的项目 workdir；未绑定项目时 `None`（只能读 global 记忆）。
    project_workdir: Option<PathBuf>,
}

impl ReadMemoryTool {
    pub fn new(data_dir: Option<PathBuf>, project_workdir: Option<PathBuf>) -> Self {
        Self {
            data_dir,
            project_workdir,
        }
    }
}

#[async_trait]
impl Tool for ReadMemoryTool {
    fn name(&self) -> &str {
        READ_MEMORY_TOOL_NAME
    }

    fn description(&self) -> &str {
        "按 id 读取一条记忆的详情。id 来自首条消息的 <memory-index> 清单。\
         level=overview 只看概览，full（默认）看全文。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {
                    "type": "string",
                    "description": "记忆 id，形如 proj/architecture 或 global/lang-pref"
                },
                "level": {
                    "type": "string",
                    "enum": ["overview", "full"],
                    "description": "overview=只看概览；full=看全文（默认）"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let data_dir = self
            .data_dir
            .as_ref()
            .ok_or_else(|| AppError::msg("记忆功能不可用（当前无数据目录）"))?;
        let id = input
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::msg("缺少参数 id"))?;
        let level = match input.get("level").and_then(|v| v.as_str()) {
            Some("overview") => MemoryLevel::Overview,
            _ => MemoryLevel::Full,
        };
        memory::read(data_dir, self.project_workdir.as_deref(), id, level)
    }
}
