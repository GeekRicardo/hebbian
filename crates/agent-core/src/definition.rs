use serde::{Deserialize, Serialize};

pub use protocol::ContextPolicy;

// ── AgentDefinition：描述一个 agent 角色的静态配置 ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: String,
    pub display_name: String,
    pub system_prompt: String,
    /// 允许使用的工具名称列表（空 = 不限制）
    pub allowed_tools: Vec<String>,
    pub context_policy: ContextPolicy,
    pub permission_policy: PermissionPolicy,
    pub compaction_policy: CompactionPolicy,
}

impl Default for AgentDefinition {
    fn default() -> Self {
        Self {
            id: "default".into(),
            display_name: "Assistant".into(),
            system_prompt: String::new(),
            allowed_tools: vec![],
            context_policy: ContextPolicy::default(),
            permission_policy: PermissionPolicy::default(),
            compaction_policy: CompactionPolicy::default(),
        }
    }
}

// ── 权限策略 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPolicy {
    /// 无需询问直接执行的工具
    pub auto_approve: Vec<String>,
    /// 总是需要用户确认的工具
    pub always_ask: Vec<String>,
    /// 对未匹配工具的默认行为
    pub default_action: DefaultPermission,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self {
            auto_approve: vec![
                "web_search".into(),
                "web_fetch".into(),
                "Read".into(),
                "Grep".into(),
                "Skill".into(),
            ],
            always_ask: vec!["Bash".into(), "Write".into()],
            default_action: DefaultPermission::Auto,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DefaultPermission {
    /// 自动批准
    Auto,
    /// 总是询问用户
    Ask,
    /// 总是拒绝
    Deny,
}

// ── 上下文压缩策略 ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionPolicy {
    /// token 预算上限，超出后触发压缩
    pub token_budget: usize,
    /// 压缩后保留最近 N 轮
    pub keep_recent_turns: usize,
    pub strategy: CompactionStrategy,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            token_budget: 80_000,
            keep_recent_turns: 8,
            strategy: CompactionStrategy::Structural,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// 结构化裁剪：保留 system + 最近 N 轮
    Structural,
    /// 先结构化裁剪，再用 LLM 生成摘要（将来实现）
    LlmSummary,
}
