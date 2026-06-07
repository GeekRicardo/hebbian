//! 运行模式（架构 §4.4.3）：决定派发器对工具调用的审批策略。
//!
//! - `AskBeforeEdits`：destructive 工具（Bash/PowerShell/Edit/Write）都要审批
//! - `EditAutomatically`：编辑类自动放行；命令类仍要审批
//! - `PlanMode`：工具列表过滤删除 Edit/Write/Bash/PowerShell，注入 ExitPlanMode（本期占位 TODO）
//! - `AutoMode`：调一次轻量 LLM judge 决定 Allow / Deny / Ask（仅 claude-opus-4-7 启用）

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunMode {
    AskBeforeEdits,
    EditAutomatically,
    PlanMode,
    AutoMode,
}

impl Default for RunMode {
    fn default() -> Self {
        RunMode::AskBeforeEdits
    }
}

impl RunMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunMode::AskBeforeEdits => "AskBeforeEdits",
            RunMode::EditAutomatically => "EditAutomatically",
            RunMode::PlanMode => "PlanMode",
            RunMode::AutoMode => "AutoMode",
        }
    }

    /// 从协议字符串解析（接受 kebab-case 与 PascalCase）。
    /// `Op::SwitchRunMode { new_mode: String }` 在 actor 路径上调用本函数。
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ask-before-edits" | "askbeforeedits" | "ask" => Some(RunMode::AskBeforeEdits),
            "edit-automatically" | "editautomatically" | "edit-auto" | "auto-edit" => {
                Some(RunMode::EditAutomatically)
            }
            "plan-mode" | "planmode" | "plan" => Some(RunMode::PlanMode),
            "auto-mode" | "automode" | "auto" => Some(RunMode::AutoMode),
            _ => None,
        }
    }
}

/// 运行中的 `run_mode` 共享句柄。
///
/// Harness::spawn_run 创建并注册到全局表（keyed by session_id），
/// agent_loop / ToolDispatcher 通过同一个 Arc 实时读取当前模式。
/// Surface 侧的 `set_run_mode` 调 `LiveRunModeRegistry::set` 即可
/// 让下一次 dispatch 立即看到新值。
pub type SharedRunMode = Arc<Mutex<RunMode>>;

/// 进程级注册表：session_id → 运行中的 SharedRunMode。
///
/// Run 启动时 register，结束时 unregister。Surface 的 `set_run_mode` 命令
/// 先写 jsonl 持久化，再调 `set` 更新运行中的 Arc。
pub struct LiveRunModeRegistry {
    inner: Mutex<HashMap<String, SharedRunMode>>,
}

impl LiveRunModeRegistry {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 全局单例。
    pub fn global() -> &'static Self {
        use std::sync::OnceLock;
        static INSTANCE: OnceLock<LiveRunModeRegistry> = OnceLock::new();
        INSTANCE.get_or_init(LiveRunModeRegistry::new)
    }

    /// Harness spawn_run 时注册。返回注册的 SharedRunMode 供 LoopParams 使用。
    pub fn register(&self, session_id: String, shared: SharedRunMode) {
        self.inner.lock().unwrap().insert(session_id, shared);
    }

    /// Run 结束时反注册。
    pub fn unregister(&self, session_id: &str) {
        self.inner.lock().unwrap().remove(session_id);
    }

    /// Surface 的 set_run_mode 调用：更新运行中的 Arc，返回 true 表示命中了活跃 run。
    pub fn set(&self, session_id: &str, mode: RunMode) -> bool {
        if let Some(shared) = self.inner.lock().unwrap().get(session_id) {
            *shared.lock().unwrap() = mode;
            true
        } else {
            false
        }
    }

    /// 读当前值（调试 / 内省用）。
    pub fn get(&self, session_id: &str) -> Option<RunMode> {
        self.inner
            .lock()
            .unwrap()
            .get(session_id)
            .map(|s| *s.lock().unwrap())
    }
}
