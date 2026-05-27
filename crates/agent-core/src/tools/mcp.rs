use std::sync::Arc;

use async_trait::async_trait;
use common::{AppError, AppResult};
use serde_json::{json, Value};

use super::Tool;
use crate::mcp::{
    client::{self, McpToolInfo},
    config::McpServerConfig,
};

pub struct McpTool {
    runtime_name: String,
    original_name: String,
    description: String,
    input_schema: Value,
    server: Arc<McpServerConfig>,
}

impl McpTool {
    pub fn from_info(info: McpToolInfo, server: Arc<McpServerConfig>) -> Self {
        let runtime_name = if info.runtime_name.trim().is_empty() {
            client::tool_runtime_name(&info.server_name, &info.name)
        } else {
            info.runtime_name.clone()
        };
        let description = if info.description.trim().is_empty() {
            format!("MCP tool {} from {}", info.name, info.server_name)
        } else {
            format!("{} (MCP: {})", info.description, info.server_name)
        };
        Self {
            runtime_name,
            original_name: info.name,
            description,
            input_schema: normalize_schema(info.input_schema),
            server,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.runtime_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.input_schema.clone()
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        client::call_tool(&self.server, &self.original_name, input)
            .await
            .map_err(|e| AppError::msg(format!("{}: {e}", self.runtime_name)))
    }
}

fn normalize_schema(schema: Value) -> Value {
    if schema.is_object() {
        schema
    } else {
        json!({"type": "object"})
    }
}

pub async fn discover_tools(config: &crate::mcp::config::McpConfig) -> Vec<Box<dyn Tool>> {
    let mut out: Vec<Box<dyn Tool>> = Vec::new();
    for server in config.enabled_servers() {
        match client::list_tools(&server).await {
            Ok(infos) => {
                let server = Arc::new(server);
                for info in infos {
                    out.push(Box::new(McpTool::from_info(info, server.clone())));
                }
            }
            Err(e) => {
                tracing::warn!(
                    server = %server.name,
                    transport = %server.transport(),
                    error = %e,
                    "MCP tool discovery failed"
                );
            }
        }
    }
    out
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpToolReport {
    pub server_name: String,
    pub transport: crate::mcp::config::McpTransport,
    pub disabled: bool,
    pub tools: Vec<McpToolInfo>,
    pub error: Option<String>,
}

pub async fn discover_tool_reports(config: &crate::mcp::config::McpConfig) -> Vec<McpToolReport> {
    let mut out = Vec::new();
    for server in config.mcp_servers.values() {
        if server.disabled {
            out.push(McpToolReport {
                server_name: server.name.clone(),
                transport: server.transport(),
                disabled: true,
                tools: Vec::new(),
                error: None,
            });
            continue;
        }

        match client::list_tools(server).await {
            Ok(tools) => out.push(McpToolReport {
                server_name: server.name.clone(),
                transport: server.transport(),
                disabled: false,
                tools,
                error: None,
            }),
            Err(e) => out.push(McpToolReport {
                server_name: server.name.clone(),
                transport: server.transport(),
                disabled: false,
                tools: Vec::new(),
                error: Some(e.to_string()),
            }),
        }
    }
    out
}

pub fn manifest(config: &crate::mcp::config::McpConfig) -> Vec<super::ToolInfo> {
    config
        .enabled_servers()
        .into_iter()
        .map(|server| super::ToolInfo {
            name: format!("MCP: {}", server.name),
            description: match server.transport() {
                crate::mcp::config::McpTransport::Stdio => {
                    format!("本地 MCP server：{}", server.command.unwrap_or_default())
                }
                crate::mcp::config::McpTransport::StreamableHttp => {
                    format!("MCP Streamable HTTP：{}", server.url.unwrap_or_default())
                }
                crate::mcp::config::McpTransport::Sse => {
                    format!("MCP SSE：{}", server.url.unwrap_or_default())
                }
            },
            icon: "plug".into(),
        })
        .collect()
}
