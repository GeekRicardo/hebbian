//! Subagent 子运行（NestedRun，架构 §4.4.11）。
//!
//! 父 agent 调 `Task(subagent_type, prompt, mode, run_in_background)` 时，dispatcher 的
//! short-circuit 分支把执行委托给 [`SubagentRunner`]：
//!
//! - 构造子 Session 起手 transcript（isolated = 仅 prompt；inherit = 父 transcript 副本 + prompt，P3 阶段实现）
//! - 用 subagent 定义的 `system_prompt` 替换默认 system 段（不组装 6 段）
//! - ToolRegistry 过滤为 subagent 定义的工具白名单（剔除 Task 自身防止多层嵌套）
//! - 共享父 [`HitlGate`] / [`crate::workspace::Workspace`] / [`crate::read_state::ReadStateTracker`] / edits-worktree
//! - 子事件经 [`SubagentSinkDecorator`] 装饰后转发到父事件流，每条带 `subagent_call_id = 父 Task 工具调用 call_id`
//! - 子 jsonl 落盘到 `~/.hebbian/sessions/<parent_sid>/subagents/<child_sid>/session.jsonl`（P3 阶段实现）

pub mod ctx;
pub mod runner;

pub use ctx::SubagentCtx;
pub use runner::SubagentRunner;
