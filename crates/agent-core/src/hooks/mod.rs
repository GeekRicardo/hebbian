pub mod external;
pub mod types;

use async_trait::async_trait;
pub use external::{
    load_hooks_config, ExternalHook, HookConfig, HookExecMode, HookMatcher, HookRule,
};
pub use types::{HookOutcome, HookPatch, HookPoint};

/// Hook trait：生命周期扩展点
///
/// agent_core 在 run 的关键时机调用已注册的 hook，hook 可以修改上下文
/// 或阻断流程（如 memory 注入、权限检查、审计日志等）。
#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;
    async fn invoke(&self, point: &HookPoint) -> HookOutcome;
}

/// Hook 管理器：持有并按顺序触发所有已注册的 hook
pub struct HookManager {
    hooks: Vec<Box<dyn Hook>>,
}

impl HookManager {
    pub fn new(hooks: Vec<Box<dyn Hook>>) -> Self {
        Self { hooks }
    }

    pub fn empty() -> Self {
        Self { hooks: vec![] }
    }

    /// 触发指定时机的所有 hook，返回第一个非 Continue 的结果
    pub async fn trigger(&self, point: &HookPoint) -> HookOutcome {
        for hook in &self.hooks {
            let outcome = hook.invoke(point).await;
            if !matches!(outcome, HookOutcome::Continue) {
                // 外部 hook（cargo check 等子进程）改变了流程：拦截 / 注入续跑 / 改写入参。
                // passed（Continue）的外部调用日志由各 hook 实现内部打（最贴近子进程命令）。
                tracing::info!(
                    target: "hook",
                    hook = hook.name(),
                    "[Hook] 外部 hook 返回非 Continue（拦截 / 注入 / 改写）"
                );
                return outcome;
            }
        }
        HookOutcome::Continue
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}
