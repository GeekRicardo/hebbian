use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use common::{AppError, AppResult};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::config::{McpServerConfig, McpTransport};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub server_name: String,
    pub name: String,
    #[serde(default)]
    pub runtime_name: String,
    pub description: String,
    pub input_schema: Value,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub async fn list_tools(server: &McpServerConfig) -> AppResult<Vec<McpToolInfo>> {
    match server.transport() {
        McpTransport::Stdio => list_stdio_tools(server).await,
        McpTransport::StreamableHttp => list_http_tools(server).await,
        McpTransport::Sse => list_legacy_sse_tools(server).await,
    }
}

pub async fn call_tool(
    server: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
) -> AppResult<String> {
    match server.transport() {
        McpTransport::Stdio => call_stdio_tool(server, tool_name, arguments).await,
        McpTransport::StreamableHttp => call_http_tool(server, tool_name, arguments).await,
        McpTransport::Sse => call_legacy_sse_tool(server, tool_name, arguments).await,
    }
}

async fn list_stdio_tools(server: &McpServerConfig) -> AppResult<Vec<McpToolInfo>> {
    let response = with_stdio_session(server, |mut session| async move {
        session.initialize().await?;
        session.notify_initialized().await?;
        session.request("tools/list", json!({})).await
    })
    .await?;
    parse_tools(server, &response)
}

fn parse_tools(server: &McpServerConfig, response: &Value) -> AppResult<Vec<McpToolInfo>> {
    let tools = response
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::msg("MCP tools/list 响应缺少 tools"))?;
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::msg("MCP tool 缺少 name"))?
            .to_string();
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let input_schema = tool
            .get("inputSchema")
            .or_else(|| tool.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"}));
        let runtime_name = tool_runtime_name(&server.name, &name);
        out.push(McpToolInfo {
            server_name: server.name.clone(),
            runtime_name,
            name,
            description,
            input_schema,
        });
    }
    Ok(out)
}

async fn call_stdio_tool(
    server: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
) -> AppResult<String> {
    let response = with_stdio_session(server, |mut session| async move {
        session.initialize().await?;
        session.notify_initialized().await?;
        session
            .request(
                "tools/call",
                json!({
                    "name": tool_name,
                    "arguments": arguments,
                }),
            )
            .await
    })
    .await?;
    Ok(format_tool_call_result(&response))
}

async fn list_http_tools(server: &McpServerConfig) -> AppResult<Vec<McpToolInfo>> {
    let mut session = HttpSession::new(server)?;
    session.initialize().await?;
    session.notify_initialized().await?;
    let response = session.request("tools/list", json!({})).await?;
    parse_tools(server, &response)
}

async fn call_http_tool(
    server: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
) -> AppResult<String> {
    let mut session = HttpSession::new(server)?;
    session.initialize().await?;
    session.notify_initialized().await?;
    let response = session
        .request(
            "tools/call",
            json!({
                "name": tool_name,
                "arguments": arguments,
            }),
        )
        .await?;
    Ok(format_tool_call_result(&response))
}

async fn list_legacy_sse_tools(server: &McpServerConfig) -> AppResult<Vec<McpToolInfo>> {
    let mut session = LegacySseSession::connect(server).await?;
    session.initialize().await?;
    session.notify_initialized_legacy().await?;
    let response = session.request("tools/list", json!({})).await?;
    parse_tools(server, &response)
}

async fn call_legacy_sse_tool(
    server: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
) -> AppResult<String> {
    let mut session = LegacySseSession::connect(server).await?;
    session.initialize().await?;
    session.notify_initialized_legacy().await?;
    let response = session
        .request(
            "tools/call",
            json!({
                "name": tool_name,
                "arguments": arguments,
            }),
        )
        .await?;
    Ok(format_tool_call_result(&response))
}

async fn with_stdio_session<F, Fut, T>(server: &McpServerConfig, f: F) -> AppResult<T>
where
    F: FnOnce(StdioSession) -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    let command = server
        .command
        .as_deref()
        .ok_or_else(|| AppError::msg(format!("MCP server {} 缺少 command", server.name)))?;
    let mut cmd = Command::new(command);
    cmd.args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in &server.env {
        cmd.env(k, v);
    }
    if let Some(path) = crate::shell_env::resolve_shell_path(None).await {
        cmd.env("PATH", path);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::msg(format!("MCP server {} 启动失败：{e}", server.name)))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::msg("MCP stdio stdin 不可用"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::msg("MCP stdio stdout 不可用"))?;
    let session = StdioSession {
        stdin,
        stdout: BufReader::new(stdout),
    };
    let result = f(session).await;
    let _ = child.kill().await;
    result
}

struct StdioSession {
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
}

struct HttpSession {
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    session_id: Option<String>,
}

struct LegacySseSession {
    client: reqwest::Client,
    endpoint: String,
    headers: BTreeMap<String, String>,
    stream: futures_util::stream::BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
    buffer: String,
}

impl StdioSession {
    async fn initialize(&mut self) -> AppResult<()> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "hebbian",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        Ok(())
    }

    async fn notify_initialized(&mut self) -> AppResult<()> {
        self.notify("notifications/initialized", json!({})).await
    }

    async fn request(&mut self, method: &str, params: Value) -> AppResult<Value> {
        let id = next_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&msg).await?;
        loop {
            let response = self.read_message().await?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = response.get("error") {
                return Err(AppError::msg(format!("MCP {method} 失败：{err}")));
            }
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> AppResult<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&msg).await
    }

    async fn write_message(&mut self, msg: &Value) -> AppResult<()> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        self.stdin.write_all(&line).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_message(&mut self) -> AppResult<Value> {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).await?;
        if n == 0 {
            return Err(AppError::msg("MCP server 关闭了 stdout"));
        }
        serde_json::from_str(&line).map_err(AppError::from)
    }
}

impl LegacySseSession {
    async fn connect(server: &McpServerConfig) -> AppResult<Self> {
        let url = server
            .url
            .clone()
            .ok_or_else(|| AppError::msg(format!("MCP server {} 缺少 url", server.name)))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        let mut req = client.get(&url).header(ACCEPT, "text/event-stream");
        for (k, v) in &server.headers {
            req = req.header(k, v);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            return Err(AppError::msg(format!(
                "MCP SSE 连接失败：HTTP {}",
                response.status()
            )));
        }
        let mut stream = response.bytes_stream().boxed();
        let endpoint = read_sse_event_data(&mut stream, Some("endpoint")).await?;
        let endpoint = reqwest::Url::parse(&url)
            .and_then(|base| base.join(&endpoint))
            .map_err(|e| AppError::msg(format!("MCP SSE endpoint 无效：{e}")))?
            .to_string();
        Ok(Self {
            client,
            endpoint,
            headers: server.headers.clone(),
            stream,
            buffer: String::new(),
        })
    }

    async fn initialize(&mut self) -> AppResult<()> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "hebbian",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        Ok(())
    }

    async fn notify_initialized_legacy(&mut self) -> AppResult<()> {
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let headers = self.headers.clone();
        Self::post_initialized_json(client, endpoint, headers).await
    }

    async fn post_initialized_json(
        client: reqwest::Client,
        endpoint: String,
        headers: BTreeMap<String, String>,
    ) -> AppResult<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        });
        Self::post_json(&client, &endpoint, &headers, &msg).await
    }

    async fn request(&mut self, method: &str, params: Value) -> AppResult<Value> {
        let id = next_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        Self::post_json(&self.client, &self.endpoint, &self.headers, &msg).await?;
        loop {
            if let Some(data) = take_sse_data(&mut self.buffer, None) {
                let value: Value = serde_json::from_str(&data)?;
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(err) = value.get("error") {
                        return Err(AppError::msg(format!("MCP {method} 失败：{err}")));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            let Some(chunk) = self.stream.next().await else {
                return Err(AppError::msg("MCP SSE stream closed"));
            };
            self.buffer.push_str(&String::from_utf8_lossy(&chunk?));
        }
    }

    async fn post_json(
        client: &reqwest::Client,
        endpoint: &str,
        headers: &BTreeMap<String, String>,
        msg: &Value,
    ) -> AppResult<()> {
        let mut req = client
            .post(endpoint)
            .header(CONTENT_TYPE, "application/json")
            .json(msg);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            return Err(AppError::msg(format!(
                "MCP SSE POST 失败：HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }
}

impl HttpSession {
    fn new(server: &McpServerConfig) -> AppResult<Self> {
        let url = server
            .url
            .clone()
            .ok_or_else(|| AppError::msg(format!("MCP server {} 缺少 url", server.name)))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            client,
            url,
            headers: server.headers.clone(),
            session_id: None,
        })
    }

    async fn initialize(&mut self) -> AppResult<()> {
        let id = next_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "hebbian",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        });
        let response = self.post_json(&msg).await?;
        if let Some(session_id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(session_id.to_string());
        }
        let _ = parse_http_jsonrpc_response(response, id).await?;
        Ok(())
    }

    async fn notify_initialized(&self) -> AppResult<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        });
        let response = self.post_json(&msg).await?;
        if !response.status().is_success() {
            return Err(AppError::msg(format!(
                "MCP initialized 通知失败：HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> AppResult<Value> {
        let id = next_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.post_json(&msg).await?;
        parse_http_jsonrpc_response(response, id).await
    }

    async fn post_json(&self, msg: &Value) -> AppResult<reqwest::Response> {
        let mut req = self
            .client
            .post(&self.url)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .json(msg);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        if let Some(session_id) = &self.session_id {
            req = req.header("Mcp-Session-Id", session_id);
        }
        req.send().await.map_err(AppError::from)
    }
}

async fn parse_http_jsonrpc_response(response: reqwest::Response, id: u64) -> AppResult<Value> {
    if !response.status().is_success() {
        return Err(AppError::msg(format!(
            "MCP HTTP 失败：HTTP {}",
            response.status()
        )));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let value = if content_type.starts_with("text/event-stream") {
        let mut stream = response.bytes_stream().boxed();
        read_sse_json_for_id(&mut stream, id).await?
    } else {
        response.json::<Value>().await?
    };
    if value.get("id").and_then(Value::as_u64) != Some(id) {
        return Err(AppError::msg("MCP HTTP 响应 id 不匹配"));
    }
    if let Some(err) = value.get("error") {
        return Err(AppError::msg(format!("MCP HTTP JSON-RPC error：{err}")));
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

async fn read_sse_json_for_id(
    stream: &mut (impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin),
    id: u64,
) -> AppResult<Value> {
    let mut buffer = String::new();
    loop {
        if let Some(data) = take_sse_data(&mut buffer, None) {
            let value: Value = serde_json::from_str(&data)?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(value);
            }
        }
        let Some(chunk) = stream.next().await else {
            return Err(AppError::msg("MCP HTTP event stream closed"));
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk?));
    }
}

async fn read_sse_event_data(
    stream: &mut (impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin),
    event_name: Option<&str>,
) -> AppResult<String> {
    let mut buffer = String::new();
    loop {
        if let Some(data) = take_sse_data(&mut buffer, event_name) {
            return Ok(data);
        }
        let Some(chunk) = stream.next().await else {
            return Err(AppError::msg("MCP SSE stream closed"));
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk?));
    }
}

fn take_sse_data(buffer: &mut String, event_name: Option<&str>) -> Option<String> {
    let split_at = buffer.find("\n\n").or_else(|| buffer.find("\r\n\r\n"))?;
    let raw = buffer[..split_at].to_string();
    let drain_to = if buffer[split_at..].starts_with("\r\n\r\n") {
        split_at + 4
    } else {
        split_at + 2
    };
    buffer.drain(..drain_to);
    let mut data = Vec::new();
    let mut event = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.trim_start().to_string());
        }
    }
    if event_name.is_some() && event.as_deref() != event_name {
        return None;
    }
    if data.is_empty() {
        None
    } else {
        Some(data.join("\n"))
    }
}

fn format_tool_call_result(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return result.to_string();
    };
    let mut parts = Vec::new();
    for item in content {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            _ => parts.push(item.to_string()),
        }
    }
    if parts.is_empty() {
        result.to_string()
    } else {
        parts.join("\n")
    }
}

pub fn tool_runtime_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "Mcp__{}__{}",
        sanitize_tool_part(server_name),
        sanitize_tool_part(tool_name)
    )
}

fn sanitize_tool_part(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "tool".to_string()
    } else {
        out
    }
}

pub fn split_runtime_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("Mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    Some((server, tool))
}

pub fn server_index<'a>(
    servers: impl IntoIterator<Item = &'a McpServerConfig>,
) -> BTreeMap<String, &'a McpServerConfig> {
    servers
        .into_iter()
        .map(|server| (sanitize_tool_part(&server.name), server))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_name_is_pascal_safe_prefix() {
        assert_eq!(
            tool_runtime_name("github.com", "search/repo"),
            "Mcp__github_com__search_repo"
        );
        assert_eq!(
            split_runtime_name("Mcp__github_com__search_repo"),
            Some(("github_com", "search_repo"))
        );
    }

    #[test]
    fn formats_text_tool_result() {
        let text = format_tool_call_result(&json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": "world"}
            ]
        }));
        assert_eq!(text, "hello\nworld");
    }
}
