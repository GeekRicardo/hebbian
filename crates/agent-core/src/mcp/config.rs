use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    #[serde(default, alias = "servers", alias = "mcp_servers")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    #[serde(skip)]
    pub name: String,
    #[serde(default)]
    pub transport: Option<McpTransport>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
    /// stdio 子进程的工作目录。由 surface 在拿到 enabled_servers 后注入，
    /// 通常是当前 session 的 workdir；落盘配置不带这个字段。
    #[serde(skip)]
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum McpConfigError {
    #[error("invalid MCP config JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("enabled MCP server '{server}' must define command for stdio transport")]
    MissingCommand { server: String },
    #[error("enabled MCP server '{server}' must define url for {transport} transport")]
    MissingUrl {
        server: String,
        transport: McpTransport,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    Stdio,
    StreamableHttp,
    Sse,
}

impl std::fmt::Display for McpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdio => f.write_str("stdio"),
            Self::StreamableHttp => f.write_str("streamable_http"),
            Self::Sse => f.write_str("sse"),
        }
    }
}

impl McpServerConfig {
    pub fn transport(&self) -> McpTransport {
        self.transport.unwrap_or_else(|| {
            if self.url.as_deref().unwrap_or_default().trim().is_empty() {
                McpTransport::Stdio
            } else {
                McpTransport::StreamableHttp
            }
        })
    }
}

impl McpConfig {
    pub fn parse_json(input: &str) -> Result<Self, McpConfigError> {
        if input.trim().is_empty() {
            return Ok(Self::default());
        }

        let mut config: Self = serde_json::from_str(input)?;
        for (name, server) in &mut config.mcp_servers {
            server.name = name.clone();
            if !server.disabled {
                match server.transport() {
                    McpTransport::Stdio => {
                        if server
                            .command
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                        {
                            return Err(McpConfigError::MissingCommand {
                                server: name.clone(),
                            });
                        }
                    }
                    McpTransport::StreamableHttp | McpTransport::Sse => {
                        if server.url.as_deref().unwrap_or_default().trim().is_empty() {
                            return Err(McpConfigError::MissingUrl {
                                server: name.clone(),
                                transport: server.transport(),
                            });
                        }
                    }
                }
            }
        }
        Ok(config)
    }

    pub fn enabled_servers(&self) -> Vec<McpServerConfig> {
        self.mcp_servers
            .values()
            .filter(|server| !server.disabled)
            .cloned()
            .collect()
    }

    /// 给所有 server（含 disabled，便于设置页一并展示）注入 stdio 子进程的工作目录。
    /// surface 在每次 session 起跑时调用一次：HTTP/SSE server 也带上无害——transport 决定是否真用。
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        for server in self.mcp_servers.values_mut() {
            server.cwd = Some(cwd.clone());
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_style_stdio_servers() {
        let config = McpConfig::parse_json(
            r#"{
              "mcpServers": {
                "filesystem": {
                  "command": "npx",
                  "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
                  "env": {"DEBUG": "1"}
                },
                "disabled": {
                  "command": "node",
                  "args": ["server.js"],
                  "disabled": true
                }
              }
            }"#,
        )
        .expect("valid config");

        assert_eq!(config.enabled_servers().len(), 1);
        let server = &config.enabled_servers()[0];
        assert_eq!(server.name, "filesystem");
        assert_eq!(server.command.as_deref(), Some("npx"));
        assert_eq!(
            server.args,
            vec!["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        );
        assert_eq!(server.env.get("DEBUG").map(String::as_str), Some("1"));
    }

    #[test]
    fn rejects_enabled_stdio_server_without_command() {
        let error = McpConfig::parse_json(
            r#"{
              "mcpServers": {
                "broken": {"args": ["server.js"]}
              }
            }"#,
        )
        .expect_err("missing command should fail");

        assert!(error.to_string().contains("broken"));
        assert!(error.to_string().contains("command"));
    }

    #[test]
    fn empty_json_uses_empty_config() {
        let config = McpConfig::parse_json("").expect("empty config is allowed");
        assert!(config.enabled_servers().is_empty());
    }

    #[test]
    fn parses_servers_alias_and_streamable_http_transport() {
        let config = McpConfig::parse_json(
            r#"{
              "servers": {
                "docs": {
                  "transport": "streamable_http",
                  "url": "https://example.com/mcp",
                  "headers": {"Authorization": "Bearer token"}
                }
              }
            }"#,
        )
        .expect("streamable http config");

        let server = &config.enabled_servers()[0];
        assert_eq!(server.name, "docs");
        assert_eq!(server.transport(), McpTransport::StreamableHttp);
        assert_eq!(server.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(
            server.headers.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
    }

    #[test]
    fn accepts_legacy_sse_transport_with_url() {
        let config = McpConfig::parse_json(
            r#"{
              "mcpServers": {
                "legacy": {
                  "transport": "sse",
                  "url": "https://example.com/sse"
                }
              }
            }"#,
        )
        .expect("legacy sse config");

        let server = &config.enabled_servers()[0];
        assert_eq!(server.transport(), McpTransport::Sse);
        assert_eq!(server.url.as_deref(), Some("https://example.com/sse"));
    }

    #[test]
    fn rejects_enabled_http_server_without_url() {
        let error = McpConfig::parse_json(
            r#"{
              "mcpServers": {
                "broken": {"transport": "streamable_http"}
              }
            }"#,
        )
        .expect_err("missing url should fail");

        assert!(error.to_string().contains("broken"));
        assert!(error.to_string().contains("url"));
    }
}
