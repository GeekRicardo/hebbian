//! ScheduleWakeup 工具：架构 §4.12.4。
//!
//! 让模型显式挂起本 Run 等定时唤醒。例如「60 秒后回来看 build 进度」。
//! 上限 604800 秒（7 天，§13 决策）。
//!
//! `EffectClass::ReadOnly`，不审批。

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};

use super::Tool;
use crate::storage::run_checkpoint::RunPhase;
use crate::wakeup::PhaseChannel;

const MAX_DELAY_SECS: u64 = 604_800; // 7 天

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
         上限 604800 秒（7 天）。"
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
                    "description": "多少秒后唤醒。1-604800（7 天）。"
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
        let raw_delay = input["delay_secs"]
            .as_u64()
            .ok_or_else(|| AppError::msg("ScheduleWakeup: 缺少 delay_secs"))?;
        let delay = raw_delay.min(MAX_DELAY_SECS).max(1);
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
        let truncate_note = if raw_delay > MAX_DELAY_SECS {
            format!(
                "（您传入的 {raw_delay}s 超过上限 {MAX_DELAY_SECS}s，已自动截断）"
            )
        } else {
            String::new()
        };
        Ok(format!(
            "[ScheduleWakeup] 已设置 {delay}s 后唤醒（reason: {reason}）{truncate_note}。本轮 tool_call 结束后 Run 暂停。"
        ))
    }
}
