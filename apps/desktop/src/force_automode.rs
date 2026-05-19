//! `//force-automode` 命令的 desktop 进程级开关（架构 §4.4.4 / §8）。
//!
//! 仅在内存中保留 session_id → bool 映射，不写盘：
//! - 关掉 / 重启 desktop 进程后回到 `false`（这是一个明示「放手跑、不打断我」
//!   的危险开关，重启回归默认更安全）
//! - 多窗口共享同一进程 → 同一张表，并发由 `Mutex` 兜底
//!
//! Tauri 命令 `set_force_automode` 写入这张表；`chat::send_and_save` 在构造
//! `SessionConfig` 时读它填到 `force_automode` 字段。

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct ForceAutomodeState {
    inner: Mutex<HashMap<String, bool>>,
}

impl ForceAutomodeState {
    pub fn is_enabled(&self, session_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(false)
    }

    pub fn set(&self, session_id: String, enabled: bool) {
        let mut guard = self.inner.lock().unwrap();
        if enabled {
            guard.insert(session_id, true);
        } else {
            guard.remove(&session_id);
        }
    }
}
