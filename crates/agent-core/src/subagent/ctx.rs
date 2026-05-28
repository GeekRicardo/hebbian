//! Subagent 运行时上下文：跑 NestedRun 需要、[`crate::dispatch::ToolDispatcher`] 自身没用的字段。
//!
//! 这里只放**跨 run 静态**的依赖（client / hooks / data_dir / subagents 快照等），per-run
//! 动态字段（parent_run_id / model_id / agent 等）由 dispatcher 在 spawn_task 时直接从自身
//! 已有字段（self.state.run_id / self.model_id ...）取，不重复存储。

use std::path::PathBuf;
use std::sync::Arc;

use model_gateway::client::ModelClient;

use crate::definition::CompactionPolicy;
use crate::hooks::HookManager;
use crate::storage::subagents::SubagentDefinition;

/// 跑 NestedRun 必备但 [`crate::dispatch::ToolDispatcher`] 自身没用的字段集合。
pub struct SubagentCtx {
    /// 主 ModelClient（与父 Run 同源）。子 NestedRun 直接复用。
    pub client: Arc<dyn ModelClient>,
    /// Hook 管理器（架构 §4.8）。父子共享。
    pub hooks: Arc<HookManager>,
    /// 压缩策略。本期子直接套父的策略（最简）。
    pub compaction_policy: CompactionPolicy,
    /// 数据目录根。子 session 落盘到 `<data_dir>/sessions/<parent_sid>/subagents/<child_sid>/`（P3 阶段）。
    pub data_dir: Option<PathBuf>,
    /// 父 session id。子 session 子目录拼路径用（P3 阶段）。
    pub parent_session_id: Option<String>,
    /// 是否走流式（沿用父 Run 设定）。
    pub stream: bool,
    /// 当前可用 subagent 定义（已合并启用状态、过滤掉 enabled=false）。
    /// SubagentRunner 按 `subagent_type` 在此列表里查；找不到时返回错误而不重新读盘——
    /// 一次 Task 调用期间盘上的定义文件可能变化，但本次以注入时的快照为准更稳定。
    pub subagents: Arc<Vec<SubagentDefinition>>,
}

impl SubagentCtx {
    pub fn find(&self, name: &str) -> Option<&SubagentDefinition> {
        self.subagents.iter().find(|d| d.name == name)
    }
}
