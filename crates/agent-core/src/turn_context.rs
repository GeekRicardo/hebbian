use protocol::TokenBudget;

use crate::definition::{CompactionPolicy, PermissionPolicy};

/// 一次 turn（"用户输入 → 助手最终输出"）的所有显式参数。
///
/// 显式持有 turn 上下文是为了：
/// - 同一 run 的不同 turn 可以切换 model
/// - Fork / Rollback 时不需要重建配置——直接复制 TurnContext
/// - 所有审批与压缩判定都在 TurnContext 范围内做，避免隐式全局状态
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// 模型 id（具体由 ModelClient 负责解析）
    pub model: String,
    /// 启用的工具名列表（已经过 AgentDefinition.allowed_tools 过滤）
    pub enabled_tools: Vec<String>,
    /// 是否启用流式
    pub stream: bool,
    /// 审批策略（来自 AgentDefinition）
    pub permission_policy: PermissionPolicy,
    /// 压缩策略
    pub compaction_policy: CompactionPolicy,
    /// Token / 迭代预算
    pub budget: TokenBudget,
}

impl TurnContext {
    pub fn new(model: String, enabled_tools: Vec<String>, stream: bool) -> Self {
        Self {
            model,
            enabled_tools,
            stream,
            permission_policy: PermissionPolicy::default(),
            compaction_policy: CompactionPolicy::default(),
            budget: TokenBudget::default(),
        }
    }
}
