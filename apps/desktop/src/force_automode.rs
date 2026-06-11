//! `//hands-off`「全自动」命令的 desktop 进程级开关（架构 §4.4.4 / §8）。
//!
//! **唯一真源是 agent-core 的 [`LiveForceAutomodeRegistry`]**——本类型只是个薄委托，
//! 让 desktop 既有调用点（Tauri 命令 / `send_and_save`）复用，不必各自引 agent-core：
//! - `set` 直接写 registry：命中活跃 run（agent_loop 已注册共享句柄）时 run 中途下一个
//!   工具调用即生效；无活跃 run 时也存值，下次发消息读它作初值
//! - 不写盘、不随 run 结束清空——重启进程后 registry 为空回到 `false`（危险开关，
//!   重启回归默认更安全）

use agent_core::run_mode::LiveForceAutomodeRegistry;

#[derive(Default)]
pub struct ForceAutomodeState;

impl ForceAutomodeState {
    pub fn is_enabled(&self, session_id: &str) -> bool {
        LiveForceAutomodeRegistry::global().get(session_id)
    }

    pub fn set(&self, session_id: String, enabled: bool) {
        LiveForceAutomodeRegistry::global().set(&session_id, enabled);
    }
}
