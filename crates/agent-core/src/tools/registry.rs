use std::collections::HashMap;
use std::sync::Arc;

use super::Tool;
use model_gateway::types::ToolDefinition;

/// 工具注册表：持有所有可用工具，支持按名称查找
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Self {
        let tools = tools
            .into_iter()
            .map(|t| {
                let name = t.name().to_string();
                (name, Arc::from(t))
            })
            .collect();
        Self { tools }
    }

    /// 从已 wrap 的 `Arc<dyn Tool>` 列表构造——给 [`crate::subagent::SubagentRunner`]
    /// 过滤父 registry 后构造子 registry 用。重名后者覆盖前者。
    pub fn from_arcs(tools: Vec<Arc<dyn Tool>>) -> Self {
        let tools = tools
            .into_iter()
            .map(|t| (t.name().to_string(), t))
            .collect();
        Self { tools }
    }

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// 按注册顺序遍历所有工具的 `Arc`。给 [`crate::subagent::SubagentRunner`] 构造子 registry 用。
    /// `HashMap` 不保证迭代顺序，调用方对顺序敏感时应自行排序。
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.values()
    }

    /// 当前 registry 包含的所有工具名（顺序与 [`iter`] 一致）。
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 返回工具定义列表，供 model_gateway 注入到 ModelRequest
    pub fn definitions(&self, filter: &[String]) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .filter(|t| filter.is_empty() || filter.iter().any(|n| n == t.name()))
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect()
    }

    pub fn mcp_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .filter(|t| t.name().starts_with("Mcp__"))
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
