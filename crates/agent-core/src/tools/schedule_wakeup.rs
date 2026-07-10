//! ScheduleWakeup 工具：架构 §4.12.4。
//!
//! 让模型显式挂起本 Run 等定时唤醒。例如「60 秒后回来看 build 进度」。
//! 上限 604800 秒（7 天，§13 决策）。支持延迟秒数（delay_secs）或绝对时间
//! （fire_at，ISO 8601）两种指定方式；同时提供时 fire_at 优先。
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
        "挂起当前对话，支持延迟秒数或指定时刻自动唤醒（cron 风格，进程内调度）。\
         挂起期间不占 turn；到点时系统会把 <wakeup> 通知作为一条新的 user message 注入。\
         上限 604800 秒（7 天）。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["reason"],
            "properties": {
                "delay_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_DELAY_SECS,
                    "description": "多少秒后唤醒。与 fire_at 二选一（同时提供时 fire_at 优先）。1-604800（7 天）。"
                },
                "fire_at": {
                    "type": "string",
                    "description": "指定唤醒时刻（ISO 8601 格式，如 2026-07-06T10:00:00Z）。不能超过当前时间 7 天。与 delay_secs 二选一（同时提供时 fire_at 优先）。"
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
        let reason = input["reason"]
            .as_str()
            .ok_or_else(|| AppError::msg("ScheduleWakeup: 缺少 reason"))?
            .trim()
            .to_string();
        if reason.is_empty() {
            return Err(AppError::msg("ScheduleWakeup: reason 不能为空"));
        }
        if reason.chars().count() > 200 {
            return Err(AppError::msg("ScheduleWakeup: reason 不能超过 200 字"));
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        let max_fire_ms = now_ms + (MAX_DELAY_SECS as i64) * 1000;

        let (fire_at_ms, source_label): (i64, String) = if let Some(fire_at_str) =
            input["fire_at"].as_str()
        {
            let parsed = chrono::DateTime::parse_from_rfc3339(fire_at_str)
                    .map_err(|e| {
                        AppError::msg(format!(
                            "ScheduleWakeup: fire_at 格式无效（需要 ISO 8601，如 2026-07-06T10:00:00Z）：{e}"
                        ))
                    })?;
            let parsed_ms = parsed.timestamp_millis();
            if parsed_ms <= now_ms {
                return Err(AppError::msg("ScheduleWakeup: fire_at 必须在未来"));
            }
            if parsed_ms > max_fire_ms {
                return Err(AppError::msg(format!(
                    "ScheduleWakeup: fire_at 不能超过当前时间 {} 天",
                    MAX_DELAY_SECS / 86400,
                )));
            }
            (parsed_ms, "fire_at".into())
        } else if let Some(raw_delay) = input["delay_secs"].as_u64() {
            let delay = raw_delay.min(MAX_DELAY_SECS).max(1);
            (now_ms + (delay as i64) * 1000, "delay_secs".into())
        } else {
            return Err(AppError::msg("ScheduleWakeup: 需要 delay_secs 或 fire_at"));
        };

        *self.phase.lock().unwrap() = Some(RunPhase::AwaitingCron {
            fire_at_ms,
            reason: reason.clone(),
        });

        let delay_secs = ((fire_at_ms - now_ms) / 1000) as u64;
        let iso = chrono::DateTime::from_timestamp_millis(fire_at_ms)
            .map(|d| d.to_rfc3339())
            .unwrap_or_default();
        Ok(format!(
            "[ScheduleWakeup] 已设置在 {iso}（{delay_secs}s 后）唤醒（reason: {reason}，来源: {source_label}）。\
             本轮 tool_call 结束后 Run 暂停。"
        ))
    }
}
