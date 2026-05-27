//! WebSocket 协议：client ↔ server 消息类型。
//!
//! 设计目标：与 desktop 的 Tauri invoke/emit 语义 1:1 对应，
//! 让前端只需 runtime detect 切换 transport 即可复用同一份业务代码。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// client → server
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientMessage {
    /// 绑定到某个 session 的事件流。建立 WS 连接后第一条必须是 subscribe。
    /// 同一连接可以多次 subscribe 切 session（前一个绑定自动取消）。
    Subscribe { session_id: String },
    /// 取消订阅（不关连接）
    Unsubscribe,
    /// 调用一个 server-side command（对应 Tauri invoke）
    /// id 由 client 生成，用于匹配响应；cmd 是命令名（snake_case）；args 是参数对象
    Invoke {
        id: String,
        cmd: String,
        #[serde(default)]
        args: Value,
        /// 可选：本次 invoke 关联的 session_id（多数 command 都需要）
        #[serde(default)]
        session_id: Option<String>,
    },
}

/// server → client
#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerMessage {
    /// 连接握手回应（subscribe 之前）
    Hello { server_version: &'static str },
    /// 订阅成功
    Subscribed { session_id: String },
    /// invoke 的响应（按 id 匹配 client 请求）
    InvokeResponse {
        id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// 来自 agent_core 的事件（按 session_id 路由）
    /// 形如 Tauri 的 emit("engine-event", payload)
    Event {
        session_id: String,
        name: String,
        payload: Value,
    },
}

impl WsServerMessage {
    pub fn ok(id: String, data: Option<Value>) -> Self {
        Self::InvokeResponse {
            id,
            ok: true,
            data,
            error: None,
        }
    }
    pub fn err(id: String, error: impl ToString) -> Self {
        Self::InvokeResponse {
            id,
            ok: false,
            data: None,
            error: Some(error.to_string()),
        }
    }
}
