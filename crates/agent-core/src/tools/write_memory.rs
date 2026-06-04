//! `WriteMemory` 工具（架构 §4.14）：模型主动把一条值得长期记的事实写入记忆。
//!
//! 落盘必经 [`crate::storage::memory`]——不把内部路径暴露给模型。`scope=project` 但
//! 当前对话未绑定项目时降级写 global，并在返回里说明，避免「记了个寂寞」。

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};

use common::{AppError, AppResult};

use super::Tool;
use crate::storage::memory::{self, MemoryScope};

pub const WRITE_MEMORY_TOOL_NAME: &str = "WriteMemory";

pub struct WriteMemoryTool {
    data_dir: Option<PathBuf>,
    /// 当前对话绑定的项目 workdir；未绑定项目时 `None`（scope=project 时降级 global）。
    project_workdir: Option<PathBuf>,
}

impl WriteMemoryTool {
    pub fn new(data_dir: Option<PathBuf>, project_workdir: Option<PathBuf>) -> Self {
        Self {
            data_dir,
            project_workdir,
        }
    }
}

#[async_trait]
impl Tool for WriteMemoryTool {
    fn name(&self) -> &str {
        WRITE_MEMORY_TOOL_NAME
    }

    fn description(&self) -> &str {
        "把一条值得以后复用的事实写入长期记忆。scope=project 记到当前项目（结构 / 架构 / \
         约定 / 坑），global 记到跨项目全局（用户偏好等）。key 是这条记忆的稳定标识，\
         用同一个 key 再写会更新它。summary 是一句话摘要（会出现在以后对话的记忆清单里）。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["scope", "category", "key", "summary", "content"],
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["project", "global"],
                    "description": "project=当前项目；global=跨项目全局"
                },
                "category": {
                    "type": "string",
                    "description": "分类，如 structure / architecture / conventions / pitfalls / preferences"
                },
                "key": {
                    "type": "string",
                    "description": "这条记忆的稳定标识；同 key 再写会更新该条"
                },
                "summary": {
                    "type": "string",
                    "description": "一句话摘要（≤120 字），用于以后对话的记忆清单初筛"
                },
                "content": {
                    "type": "string",
                    "description": "正文。可含 ## 概览 / ## 详情 两段；短记忆只写正文即可"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let data_dir = self
            .data_dir
            .as_ref()
            .ok_or_else(|| AppError::msg("记忆功能不可用（当前无数据目录）"))?;

        let get = |k: &str| input.get(k).and_then(|v| v.as_str());
        let requested = get("scope").unwrap_or("global");
        let category = get("category").ok_or_else(|| AppError::msg("缺少参数 category"))?;
        let key = get("key").ok_or_else(|| AppError::msg("缺少参数 key"))?;
        let summary = get("summary").ok_or_else(|| AppError::msg("缺少参数 summary"))?;
        let content = get("content").ok_or_else(|| AppError::msg("缺少参数 content"))?;

        // scope 解析 + project 降级：未绑定项目时 project → global。
        let (scope, note) = match requested {
            "project" if self.project_workdir.is_some() => (MemoryScope::Project, ""),
            "project" => (MemoryScope::Global, "（当前对话未绑定项目，已改记到全局）"),
            _ => (MemoryScope::Global, ""),
        };
        let workdir = match scope {
            MemoryScope::Project => self.project_workdir.as_deref(),
            MemoryScope::Global => None,
        };

        let l0 = memory::write(data_dir, workdir, scope, key, category, summary, content)?;
        let _ = memory::append_log(
            data_dir,
            workdir,
            scope,
            &memory::MemoryLogEntry::new("wrote", format!("主动写入 {}", l0.id)),
        );
        Ok(format!("已记下记忆 {} [{}]{}", l0.id, l0.category, note))
    }
}
