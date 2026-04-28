pub mod permissions;
pub mod registry;
pub mod web_fetch;
pub mod web_search;

use async_trait::async_trait;
use model_gateway::types::{ToolDefinition, IMAGE_GENERATION_TOOL_NAME};
use serde_json::Value;

use platform::AppResult;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, input: Value) -> AppResult<String>;
}

pub fn default_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(web_search::WebSearchTool),
        Box::new(web_fetch::WebFetchTool),
    ]
}

pub fn hosted_tool_definitions(filter: &[String]) -> Vec<ToolDefinition> {
    if filter.iter().any(|name| name == IMAGE_GENERATION_TOOL_NAME) {
        vec![ToolDefinition {
            name: IMAGE_GENERATION_TOOL_NAME.to_string(),
            description: "生成或编辑图片".into(),
            parameters: serde_json::json!({"type": "object"}),
        }]
    } else {
        Vec::new()
    }
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub icon: String,
}

pub fn tool_manifest() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "web_search".into(),
            description: "DuckDuckGo 网络搜索".into(),
            icon: "search".into(),
        },
        ToolInfo {
            name: "web_fetch".into(),
            description: "抓取网页内容".into(),
            icon: "globe".into(),
        },
        ToolInfo {
            name: IMAGE_GENERATION_TOOL_NAME.into(),
            description: "生成图片".into(),
            icon: "image".into(),
        },
    ]
}
