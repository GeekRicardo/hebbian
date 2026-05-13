//! WaitForTask 工具：架构 §4.12.4。
//!
//! 让模型显式挂起本 Run 等指定 BackgroundShell `task_id` 完成。工具本身只做
//! 三件事：
//! 1. 校验 task_id 真存在；
//! 2. 把 [`RunPhase::AwaitingBackgroundTask`] 写到 phase channel；
//! 3. 返回一段"已挂起"提示文本。
//!
//! 真正"等"由 agent_loop + WakeupScheduler 完成（本 ToolStep 跑完 → emit
//! RunSuspended → task return → 后台 watcher 看到 task 终态 → 注入 `<wakeup>` →
//! Harness.spawn_run 复活同一个 Run）。
//!
//! `EffectClass::ReadOnly`，不审批；上限只能挂 1 个 task_id（v1）。

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};

use super::background::BackgroundShells;
use super::Tool;
use crate::storage::run_checkpoint::RunPhase;
use crate::wakeup::PhaseChannel;

const MAX_WAIT_SECS_CAP: u64 = 3_600; // 与 ScheduleWakeup 对齐

pub struct WaitForTaskTool {
    shells: BackgroundShells,
    phase: PhaseChannel,
}

impl WaitForTaskTool {
    pub fn new(shells: BackgroundShells, phase: PhaseChannel) -> Self {
        Self { shells, phase }
    }
}

#[async_trait]
impl Tool for WaitForTaskTool {
    fn name(&self) -> &str {
        "WaitForTask"
    }

    fn description(&self) -> &str {
        "挂起当前对话直到指定后台 Bash 任务（task_id）完成，再被自动唤醒。\
         挂起期间你不会被反复轮询占 turn；任务结束时系统会把 <wakeup> 通知\
         作为一条新的 user message 注入。\
         适用于：长跑命令明知道短时间内不会完成、模型想去做别的事或先停下。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Bash 转后台时返回的 task_id（形如 bash_001）。"
                },
                "max_wait_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_WAIT_SECS_CAP,
                    "description": "兜底等待秒数：到点仍未完成也会唤醒（带 timeout 标记）。\
                                    缺省时只等任务真完成。上限 3600。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let task_id = input["task_id"]
            .as_str()
            .ok_or_else(|| AppError::msg("WaitForTask: 缺少 task_id"))?;
        let shell = self
            .shells
            .get(task_id)
            .ok_or_else(|| AppError::msg(format!("WaitForTask: 未找到 task_id={task_id}")))?;

        if shell.state().is_terminal() {
            return Ok(format!(
                "[WaitForTask] task_id={task_id} 已经处于终态，无需挂起。"
            ));
        }

        let max_wait_until_ms = input["max_wait_secs"].as_u64().map(|s| {
            let s = s.min(MAX_WAIT_SECS_CAP);
            chrono::Utc::now().timestamp_millis() + (s as i64) * 1000
        });

        *self.phase.lock().unwrap() = Some(RunPhase::AwaitingBackgroundTask {
            task_id: task_id.to_string(),
            max_wait_until_ms,
        });

        Ok(format!(
            "[WaitForTask] 已挂起等待 task_id={task_id}。本轮 tool_call 结束后 Run 暂停；\
             task 完成时系统会自动唤醒并把结果以 <wakeup> user message 注入。"
        ))
    }
}
