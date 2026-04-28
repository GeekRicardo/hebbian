use std::sync::atomic::{AtomicU64, Ordering};

use protocol::{Event, EventPayload, RunId};

/// 一次 run 的"运行时状态"。被 agent_loop 持有，用于：
/// - 派发 per-run 单调递增的 seq
/// - 持有未来要扩展的 turn 计数、累计 usage 等
pub struct RunState {
    pub run_id: RunId,
    seq: AtomicU64,
    pub turn: AtomicU64,
}

impl RunState {
    pub fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            seq: AtomicU64::new(0),
            turn: AtomicU64::new(0),
        }
    }

    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn current_turn(&self) -> u32 {
        self.turn.load(Ordering::Relaxed) as u32
    }

    pub fn next_turn(&self) -> u32 {
        self.turn.fetch_add(1, Ordering::Relaxed) as u32
    }

    /// 给定 payload 自动构造一个 Event（带 per-run seq + 当前时间戳）
    pub fn event(&self, payload: EventPayload) -> Event {
        Event::now(self.run_id.clone(), self.next_seq(), payload)
    }
}
