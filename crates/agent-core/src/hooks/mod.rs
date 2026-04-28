pub mod types;

use async_trait::async_trait;
pub use types::{HookOutcome, HookPoint};

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
                return outcome;
            }
        }
        HookOutcome::Continue
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}
