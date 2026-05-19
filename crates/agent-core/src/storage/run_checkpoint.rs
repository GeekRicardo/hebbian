//! Run 挂起态 checkpoint 落盘（架构 §4.12.3）。
//!
//! 当模型调 `WaitForTask` / `ScheduleWakeup` 后，本 Run 的 ToolStep 结束 →
//! agent_loop 发现 phase ≠ Ready → 把运行时态序列化到
//! `~/.hebbian/sessions/<sid>/run_checkpoint.json` → 函数 return。WakeupScheduler
//! 后来要 resume 时从这里读回。
//!
//! transcript 不进 checkpoint——agent_loop 唤醒时从 session.jsonl 重建（§4.12.3）。
//! 只持久化"重启后能再画一次"的运行时计数器与 phase 标记。

use std::path::{Path, PathBuf};

use common::AppResult;
use serde::{Deserialize, Serialize};

use super::lock;
use super::sessions_dir::session_dir;

/// 文件名：每个 session 至多一份。
const FILE_NAME: &str = "run_checkpoint.json";

/// agent_loop 当前阶段（架构 §4.12.3）。
///
/// `Ready` 是运行时态，不会被序列化（默认值是 Suspended* 任一种才有意义）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunPhase {
    /// 等指定 BackgroundShell task_id 完成。
    AwaitingBackgroundTask {
        task_id: String,
        /// 兜底 cron：超时未完成则按这个 ms 时间戳触发唤醒。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_wait_until_ms: Option<i64>,
    },
    /// 等 cron 时间到点。
    AwaitingCron { fire_at_ms: i64, reason: String },
}

/// 落盘的 RunCheckpoint。重启后 WakeupScheduler 不会自动 resume（§13 决策），
/// 只是 UI 可看；用户主动恢复时把它读出来重建 RunRuntime。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCheckpoint {
    pub run_id: String,
    pub session_id: String,
    pub agent: String,
    pub run_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,

    /// agent_loop 内的迭代计数（用于 MAX_TOOL_ITERATIONS 续算）。
    pub iteration: u32,
    pub model_step_index: u32,
    pub tool_step_index: u32,
    pub tool_call_dispatch_offset: usize,

    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,

    pub phase: RunPhase,
    pub suspended_at_ms: i64,
}

pub fn path(data_dir: &Path, session_id: &str) -> PathBuf {
    session_dir(data_dir, session_id).join(FILE_NAME)
}

/// 原子写——避免 agent_loop 已经 return 但 surface / scheduler 读到半文件。
pub fn save(data_dir: &Path, ck: &RunCheckpoint) -> AppResult<()> {
    let dir = session_dir(data_dir, &ck.session_id);
    std::fs::create_dir_all(&dir)?;
    let p = path(data_dir, &ck.session_id);
    let bytes = serde_json::to_vec_pretty(ck)
        .map_err(|e| common::AppError::msg(format!("checkpoint serialize: {e}")))?;
    lock::write_atomic(&p, &bytes)?;
    Ok(())
}

pub fn load(data_dir: &Path, session_id: &str) -> AppResult<Option<RunCheckpoint>> {
    let p = path(data_dir, session_id);
    if !p.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&p)?;
    let ck: RunCheckpoint = serde_json::from_slice(&bytes)
        .map_err(|e| common::AppError::msg(format!("checkpoint parse: {e}")))?;
    Ok(Some(ck))
}

pub fn delete(data_dir: &Path, session_id: &str) -> AppResult<()> {
    let p = path(data_dir, session_id);
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_delete_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "20260512-suspend1";
        std::fs::create_dir_all(tmp.path().join("sessions").join(sid)).unwrap();
        let ck = RunCheckpoint {
            run_id: "r1".into(),
            session_id: sid.into(),
            agent: "default".into(),
            run_mode: "AskBeforeEdits".into(),
            model_id: Some("claude-opus-4-7".into()),
            iteration: 3,
            model_step_index: 4,
            tool_step_index: 3,
            tool_call_dispatch_offset: 5,
            total_input_tokens: 100,
            total_output_tokens: 200,
            total_cache_read_tokens: 0,
            total_cache_creation_tokens: 0,
            phase: RunPhase::AwaitingBackgroundTask {
                task_id: "bash_001".into(),
                max_wait_until_ms: Some(1_700_000_000_000),
            },
            suspended_at_ms: 1_700_000_000_000,
        };
        save(tmp.path(), &ck).unwrap();
        let back = load(tmp.path(), sid).unwrap().unwrap();
        assert_eq!(back.run_id, "r1");
        assert!(matches!(
            back.phase,
            RunPhase::AwaitingBackgroundTask { ref task_id, .. } if task_id == "bash_001"
        ));
        delete(tmp.path(), sid).unwrap();
        assert!(load(tmp.path(), sid).unwrap().is_none());
    }

    #[test]
    fn cron_phase_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "20260512-suspend2";
        std::fs::create_dir_all(tmp.path().join("sessions").join(sid)).unwrap();
        let ck = RunCheckpoint {
            run_id: "r2".into(),
            session_id: sid.into(),
            agent: "default".into(),
            run_mode: "AskBeforeEdits".into(),
            model_id: None,
            iteration: 1,
            model_step_index: 2,
            tool_step_index: 1,
            tool_call_dispatch_offset: 1,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_creation_tokens: 0,
            phase: RunPhase::AwaitingCron {
                fire_at_ms: 1_700_000_060_000,
                reason: "check build progress".into(),
            },
            suspended_at_ms: 1_700_000_000_000,
        };
        save(tmp.path(), &ck).unwrap();
        let back = load(tmp.path(), sid).unwrap().unwrap();
        assert!(
            matches!(back.phase, RunPhase::AwaitingCron { ref reason, .. } if reason == "check build progress")
        );
    }
}
