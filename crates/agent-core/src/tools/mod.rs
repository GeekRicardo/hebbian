pub mod permissions;
pub mod question;
pub mod registry;
pub mod web_fetch;
pub mod web_search;

use async_trait::async_trait;
use model_gateway::types::{ToolDefinition, IMAGE_GENERATION_TOOL_NAME};
use serde_json::Value;

use platform::AppResult;

/// 内置 ask 工具的名称。agent_loop 识别这个名字后绕过 ToolRegistry，
/// 走 QuestionGate 通路。
pub const ASK_TOOL_NAME: &str = "ask";

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

/// 由 agent_loop 直接处理、不需要 Tool trait 实现的"虚拟工具"。
/// `enabled_tools` 包含其名字时注入定义到 ModelRequest.tools。
/// 这里只放**用户可选**的 hosted 工具（如 image_generation 由模型 provider 端运行）。
pub fn hosted_tool_definitions(filter: &[String]) -> Vec<ToolDefinition> {
    let mut defs = Vec::new();
    if filter.iter().any(|name| name == IMAGE_GENERATION_TOOL_NAME) {
        defs.push(ToolDefinition {
            name: IMAGE_GENERATION_TOOL_NAME.to_string(),
            description: "生成或编辑图片".into(),
            parameters: serde_json::json!({"type": "object"}),
        });
    }
    defs
}

/// 内置工具定义：每次 ModelRequest 都自动注入，不在 UI 工具菜单里出现，
/// 用户也无法关闭。当前包含 `ask`；未来加 `bash` / `read` / `write` 等。
///
/// 内置工具特征：
/// - 不依赖 provider 能力，agent_loop 自己处理
/// - 与 HITL 紧密耦合（ask 走 QuestionGate；bash/write 等走 PermissionGate）
/// - 是「agent 能力」的一部分，不该让用户误以为关掉会有性能收益
pub fn builtin_tool_definitions() -> Vec<ToolDefinition> {
    vec![ask_tool_definition()]
}

/// `ask` 工具的 schema：让 agent 主动向用户提问，2-5 个候选选项。
pub fn ask_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: ASK_TOOL_NAME.to_string(),
        description: "向用户提问以澄清需求或获取决策。务必同时给出 2-5 个候选选项 \
                      （label 控制在 12 字以内）；用户除了选项之外总能自由输入其他意见，\
                      所以选项不必穷尽所有可能。"
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "required": ["question", "options"],
            "properties": {
                "question": {
                    "type": "string",
                    "description": "提给用户的问题。简短直接，避免冗长背景。"
                },
                "options": {
                    "type": "array",
                    "minItems": 2,
                    "maxItems": 5,
                    "items": {
                        "type": "object",
                        "required": ["label"],
                        "properties": {
                            "label": {
                                "type": "string",
                                "description": "选项的简短文字（按钮文字），1-12 字。"
                            },
                            "description": {
                                "type": "string",
                                "description": "可选的详细说明。"
                            }
                        }
                    }
                }
            }
        }),
    }
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub icon: String,
}

/// 暴露给 UI 的工具菜单。**内置工具**（ask、未来的 bash / read / write）
/// 默认开启且不可见，**不出现**在这个列表中。
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
