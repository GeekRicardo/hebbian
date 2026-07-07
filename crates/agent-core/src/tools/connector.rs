//! 渠道连接器工具：ListConnectors / SendChannelMessage（架构 §7.5.1 扩展）。
//!
//! agent 通过这两个工具向已连接的 IM 渠道（微信等）主动推送消息。
//! 连接器注册表是进程级全局单例，surface 启动时设置实现；未设置时两个工具
//! 静默返回空 / 错误提示。

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use super::Tool;
use common::AppResult;

// ---------------------------------------------------------------------------
// ConnectorRegistry trait
// ---------------------------------------------------------------------------

/// 当前活跃的连接器信息。
#[derive(Debug, Clone, Serialize)]
pub struct ConnectorInfo {
    /// 连接器标识（如 `wechat`），用于 SendChannelMessage 的 connector_id 参数。
    pub id: String,
    /// 人类可读名称（如「微信」）。
    pub display_name: String,
    /// 账号标识（如微信 bot_id）。
    pub account_id: Option<String>,
}

/// 连接器注册表：surface 在启动时设置全局实现，agent tool 通过它发现活跃连接器并发送消息。
#[async_trait]
pub trait ConnectorRegistry: Send + Sync {
    /// 返回当前所有活跃连接器。
    fn list(&self) -> Vec<ConnectorInfo>;

    /// 通过指定连接器向用户发送文本消息。
    /// `connector_id` 对应 `ConnectorInfo.id`。
    async fn send(&self, connector_id: &str, to: &str, text: &str) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// 全局单例
// ---------------------------------------------------------------------------

static GLOBAL: OnceLock<Arc<dyn ConnectorRegistry>> = OnceLock::new();

/// 设置全局连接器注册表。仅首次调用有效（进程级单例）。
pub fn set_global_registry(registry: Arc<dyn ConnectorRegistry>) {
    let _ = GLOBAL.set(registry);
}

/// 获取全局连接器注册表。未设置时返回 None。
fn registry() -> Option<&'static Arc<dyn ConnectorRegistry>> {
    GLOBAL.get()
}

// ---------------------------------------------------------------------------
// ListConnectors
// ---------------------------------------------------------------------------

/// 列出当前所有已连接的渠道连接器（如微信）。
pub struct ListConnectorsTool;

#[async_trait]
impl Tool for ListConnectorsTool {
    fn name(&self) -> &str {
        "ListConnectors"
    }

    fn description(&self) -> &str {
        "列出当前已连接的所有即时通讯渠道（如微信），返回每个渠道的标识、名称和账号。"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _input: Value) -> AppResult<String> {
        let Some(reg) = registry() else {
            return Ok("当前没有已连接的渠道。".into());
        };
        let connectors = reg.list();
        if connectors.is_empty() {
            return Ok("当前没有已连接的渠道。".into());
        }
        Ok(serde_json::to_string_pretty(&connectors).unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// SendChannelMessage
// ---------------------------------------------------------------------------

/// 通过指定连接器向用户发送消息。
pub struct SendChannelMessageTool;

#[async_trait]
impl Tool for SendChannelMessageTool {
    fn name(&self) -> &str {
        "SendChannelMessage"
    }

    fn description(&self) -> &str {
        "通过已连接的即时通讯渠道（如微信）发送消息给指定用户。\
         需要知道对方的用户 ID（可通过 ListConnectors 查看可用渠道，\
         从对话历史中获取用户 ID）。"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "connector_id": {
                    "type": "string",
                    "description": "连接器标识，如 wechat。可通过 ListConnectors 获取可用值。"
                },
                "to": {
                    "type": "string",
                    "description": "接收者的渠道内用户 ID。"
                },
                "text": {
                    "type": "string",
                    "description": "消息内容。"
                }
            },
            "required": ["connector_id", "to", "text"]
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let Some(reg) = registry() else {
            return Ok("当前没有已连接的渠道，无法发送消息。请先在设置中连接微信等渠道。".into());
        };

        let connector_id = input
            .get("connector_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let to = input.get("to").and_then(Value::as_str).unwrap_or("");
        let text = input.get("text").and_then(Value::as_str).unwrap_or("");

        if connector_id.is_empty() || to.is_empty() || text.is_empty() {
            return Ok("参数不完整：connector_id、to、text 均为必填。".into());
        }

        match reg.send(connector_id, to, text).await {
            Ok(()) => Ok(format!("消息已通过 {connector_id} 发送给 {to}。")),
            Err(e) => Ok(format!("发送失败：{e}")),
        }
    }
}
