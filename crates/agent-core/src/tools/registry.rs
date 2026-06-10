use std::collections::BTreeMap;
use std::sync::Arc;

use super::Tool;
use model_gateway::types::ToolDefinition;

/// 工具注册表：持有所有可用工具，支持按名称查找。
///
/// 用 `BTreeMap`（按 name 字母序）而非 `HashMap`：工具列表会进 ModelRequest 的
/// `tools`，而 Anthropic 的 prompt cache 前缀顺序是 tools→system→messages——tools
/// 在最前，顺序一抖动整个缓存前缀就失效、每轮全部 cache miss。BTreeMap 让迭代
/// 顺序在进程间稳定（HashMap 随机），缓存前缀才能命中。
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
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

    /// 按 name 字母序遍历所有工具的 `Arc`（BTreeMap 保证进程间稳定顺序）。
    /// 给 [`crate::subagent::SubagentRunner`] 构造子 registry 用。
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.values()
    }

    /// 当前 registry 包含的所有工具名（顺序与 [`iter`] 一致）。
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 返回工具定义列表，供 model_gateway 注入到 ModelRequest。
    /// **不含** MCP 工具（`Mcp__` 前缀）——MCP 工具一律走 [`mcp_definitions`]，
    /// 两者互斥成对调用。否则当 filter 里同时出现 MCP 名（如 subagent 把
    /// `tool_names()` 当 fallback 白名单）会与 `mcp_definitions` 重复，上行
    /// 给 server 触发 "tools contains duplicate names" 400。
    pub fn definitions(&self, filter: &[String]) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .filter(|t| !t.name().starts_with("Mcp__"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use common::AppResult;
    use serde_json::{json, Value};

    struct StubTool(&'static str);

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _input: Value) -> AppResult<String> {
            Ok(String::new())
        }
    }

    fn registry_with(names: &[&'static str]) -> ToolRegistry {
        ToolRegistry::new(
            names
                .iter()
                .map(|n| Box::new(StubTool(n)) as Box<dyn Tool>)
                .collect(),
        )
    }

    #[test]
    fn definitions_excludes_mcp_tools() {
        // 回归：definitions 与 mcp_definitions 必须互斥。subagent 在 def.tools=None
        // 时把 tool_names()（含 Mcp__ 名）当 fallback filter，曾导致 definitions 与
        // mcp_definitions 同时吐出 MCP 工具 → server 报 duplicate names 400。
        let reg = registry_with(&["Bash", "Mcp__server__foo"]);
        let all_names: Vec<String> = reg.tool_names();

        let defs = reg.definitions(&all_names);
        assert!(
            defs.iter().all(|d| !d.name.starts_with("Mcp__")),
            "definitions 不应包含 MCP 工具，实际：{:?}",
            defs.iter().map(|d| &d.name).collect::<Vec<_>>()
        );

        // definitions + mcp_definitions 合并后无重名
        let mut combined: Vec<String> = defs.into_iter().map(|d| d.name).collect();
        combined.extend(reg.mcp_definitions().into_iter().map(|d| d.name));
        let mut sorted = combined.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            combined.len(),
            "definitions + mcp_definitions 合并出现重名：{combined:?}"
        );
    }
}
