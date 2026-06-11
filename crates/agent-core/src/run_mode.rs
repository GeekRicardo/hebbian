//! 运行模式（架构 §4.4.3）：决定派发器对工具调用的审批策略。
//!
//! - `Default`：文件编辑写工作区内的文件直接放行（edits-worktree 整 Run 可回退兜底）；
//!   写工作区外走 PathAccess 审批；改 git 元数据走工具审批；命令类走 §4.4.2 既有审批链
//! - `PlanMode`：工具列表过滤删除 Edit/Write/Bash/PowerShell，注入 ExitPlanMode（本期占位 TODO）
//! - `AutoMode`：调一次轻量 LLM judge 决定 Allow / Deny / Ask（仅模型白名单内启用）

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunMode {
    /// 老 jsonl 里存的 `AskBeforeEdits` / `EditAutomatically` 经 serde alias 映射到此值
    /// （二者语义都被 Default 覆盖，架构 §4.4.3 / §13）。
    #[default]
    #[serde(alias = "AskBeforeEdits", alias = "EditAutomatically")]
    Default,
    PlanMode,
    AutoMode,
}

impl RunMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunMode::Default => "Default",
            RunMode::PlanMode => "PlanMode",
            RunMode::AutoMode => "AutoMode",
        }
    }

    /// 从协议字符串解析（接受 kebab-case 与 PascalCase）。
    /// `Op::SwitchRunMode { new_mode: String }` 在 actor 路径上调用本函数。
    /// 老字符串 `ask-before-edits` / `edit-automatically` 仍映射到 `Default`。
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "default" | "ask-before-edits" | "askbeforeedits" | "ask" | "edit-automatically"
            | "editautomatically" | "edit-auto" | "auto-edit" => Some(RunMode::Default),
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

/// 运行中的 `force_automode`（hands-off「全自动」）共享句柄。
///
/// 与 [`SharedRunMode`] 对称：dispatcher 持有同一个 `Arc<AtomicBool>`，surface 侧
/// `set` 后下一次 dispatch 立即读到——让用户在 run 跑到一半切「全自动」开关时即时生效
/// （架构 §4.4.4）。
pub type SharedForceAutomode = Arc<std::sync::atomic::AtomicBool>;

/// 进程级注册表：session_id → 运行中的 [`SharedForceAutomode`]。
///
/// 用法与 [`LiveRunModeRegistry`] 完全一致：Run 启动 `register`、结束 `unregister`；
/// surface 的 `set_force_automode` 命令 `set` 更新运行中的值。**这是 force_automode 的
/// 唯一进程级真源**——发消息时读它作初值，run 中途也读它实时改。
pub struct LiveForceAutomodeRegistry {
    inner: Mutex<HashMap<String, SharedForceAutomode>>,
}

impl LiveForceAutomodeRegistry {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 全局单例。
    pub fn global() -> &'static Self {
        use std::sync::OnceLock;
        static INSTANCE: OnceLock<LiveForceAutomodeRegistry> = OnceLock::new();
        INSTANCE.get_or_init(LiveForceAutomodeRegistry::new)
    }

    /// Harness spawn_run 时注册运行中的共享句柄。
    pub fn register(&self, session_id: String, shared: SharedForceAutomode) {
        self.inner.lock().unwrap().insert(session_id, shared);
    }

    /// Run 结束时反注册。
    pub fn unregister(&self, session_id: &str) {
        self.inner.lock().unwrap().remove(session_id);
    }

    /// Surface 的 set_force_automode 调用：更新运行中的 Arc（命中活跃 run 时立即生效），
    /// 同时把值记到表里——即使当前没有活跃 run，下次 register 也能读到最近一次设置。
    pub fn set(&self, session_id: &str, enabled: bool) {
        let mut guard = self.inner.lock().unwrap();
        match guard.get(session_id) {
            Some(shared) => {
                shared.store(enabled, std::sync::atomic::Ordering::Relaxed);
            }
            None => {
                guard.insert(
                    session_id.to_string(),
                    Arc::new(std::sync::atomic::AtomicBool::new(enabled)),
                );
            }
        }
    }

    /// 读当前值；无记录视为 `false`（未开启全自动）。
    pub fn get(&self, session_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(session_id)
            .map(|s| s.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 架构 §4.4.3：老 jsonl 里存的 `AskBeforeEdits` / `EditAutomatically` 经 serde
    /// alias 映射到 `Default`，保证升级后加载旧 session 不丢运行模式。
    #[test]
    fn legacy_mode_strings_deserialize_to_default() {
        for legacy in ["\"AskBeforeEdits\"", "\"EditAutomatically\"", "\"Default\""] {
            let mode: RunMode = serde_json::from_str(legacy).unwrap();
            assert_eq!(mode, RunMode::Default, "{legacy} 应反序列化为 Default");
        }
        assert_eq!(
            serde_json::from_str::<RunMode>("\"PlanMode\"").unwrap(),
            RunMode::PlanMode
        );
        assert_eq!(
            serde_json::from_str::<RunMode>("\"AutoMode\"").unwrap(),
            RunMode::AutoMode
        );
    }

    /// serde 序列化只写当前合法值（`Default` 而非老名字），避免新写的 jsonl 再含废弃枚举。
    #[test]
    fn default_mode_serializes_to_default() {
        assert_eq!(
            serde_json::to_string(&RunMode::Default).unwrap(),
            "\"Default\""
        );
    }

    /// 协议字符串解析：老 kebab/Pascal 名字仍接受并落到 Default。
    #[test]
    fn parse_accepts_legacy_and_new_aliases() {
        for s in [
            "default",
            "ask-before-edits",
            "AskBeforeEdits",
            "edit-automatically",
            "EditAutomatically",
        ] {
            assert_eq!(RunMode::parse(s), Some(RunMode::Default), "{s}");
        }
        assert_eq!(RunMode::parse("plan"), Some(RunMode::PlanMode));
        assert_eq!(RunMode::parse("auto"), Some(RunMode::AutoMode));
        assert_eq!(RunMode::parse("nope"), None);
    }
}
