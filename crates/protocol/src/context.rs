use serde::{Deserialize, Serialize};

use crate::ids::MessageId;

/// 子 agent 如何继承父 agent 的上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextPolicy {
    /// 完全隔离，只接受任务 prompt
    Isolated,
    /// 继承最近 N 条消息
    InheritRecent { messages: usize },
    /// 继承父上下文的压缩摘要
    InheritSummary,
    /// 父 agent 显式指定需要传递的消息
    InheritSelected { ids: Vec<MessageId> },
    /// 默认不给，但子 agent 可通过只读工具按需查询父上下文
    OnDemand,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self::Isolated
    }
}

/// Token 预算
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokenBudget {
    /// 软上限（接近时开始压缩）
    pub soft_limit: usize,
    /// 硬上限（超过强制截断或失败）
    pub hard_limit: usize,
    /// 单 turn 最大迭代轮数
    pub max_iterations: u32,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            soft_limit: 80_000,
            hard_limit: 160_000,
            max_iterations: 10,
        }
    }
}

/// 启动 run 时对默认 TurnContext 的覆盖
///
/// AgentDefinition 提供基线，TurnOverrides 是针对这一次的修改。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnOverrides {
    pub model: Option<String>,
    pub system_prompt_suffix: Option<String>,
    pub additional_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    pub stream: Option<bool>,
}
