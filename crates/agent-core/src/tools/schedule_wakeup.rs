//! ScheduleWakeup 工具：架构 §4.12.4。
//!
//! 让模型显式挂起本 Run 等定时唤醒。例如「60 秒后回来看 build 进度」。
//! 上限 3600 秒（1 小时，§13 决策）；要更久就串多次 ScheduleWakeup。
//!
//! `EffectClass::ReadOnly`，不审批。

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};

use super::Tool;
use crate::storage::run_checkpoint::RunPhase;
use crate::wakeup::PhaseChannel;

const MAX_DELAY_SECS: u64 = 3_600;

pub struct ScheduleWakeupTool {
    phase: PhaseChannel,
}

impl ScheduleWakeupTool {
    pub fn new(phase: PhaseChannel) -> Self {
        Self { phase }
    }
}

#[async_trait]
impl Tool for ScheduleWakeupTool {
    fn name(&self) -> &str {
        "ScheduleWakeup"
    }

    fn description(&self) -> &str {
        "挂起当前对话指定秒数后自动唤醒（cron 风格，进程内调度）。\
         挂起期间不占 turn；到点时系统会把 <wakeup> 通知作为一条新的 user message 注入。\
         上限 3600 秒；想等更久请串多次调用。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["delay_secs", "reason"],
            "properties": {
                "delay_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_DELAY_SECS,
                    "description": "多少秒后唤醒。1-3600。"
                },
                "reason": {
                    "type": "string",
                    "description": "为什么要定时唤醒（4-30 字）。会回放进 <wakeup> 的 original_reason 字段，\
                                    帮助你醒来时记起当时的意图。"
                }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let delay = input["delay_secs"]
            .as_u64()
            .ok_or_else(|| AppError::msg("ScheduleWakeup: 缺少 delay_secs"))?
            .min(MAX_DELAY_SECS)
            .max(1);
        let reason = input["reason"]
            .as_str()
            .ok_or_else(|| AppError::msg("ScheduleWakeup: 缺少 reason"))?
            .trim()
            .to_string();
        if reason.is_empty() {
            return Err(AppError::msg("ScheduleWakeup: reason 不能为空"));
        }
        let fire_at_ms = chrono::Utc::now().timestamp_millis() + (delay as i64) * 1000;
        *self.phase.lock().unwrap() = Some(RunPhase::AwaitingCron {
            fire_at_ms,
            reason: reason.clone(),
        });
        Ok(format!(
            "[ScheduleWakeup] 已设置 {delay}s 后唤醒（reason: {reason}）。本轮 tool_call 结束后 Run 暂停。"
        ))
    }
}
