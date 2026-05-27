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

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
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
