//! 渠道规范化消息——与具体 IM 无关。

/// 入站消息（IM → hebbian）。
#[derive(Debug, Clone)]
pub struct InboundMessage {
    /// 渠道 id（如 `wechat`）。
    pub channel: String,
    /// 渠道发送者标识。
    pub from: String,
    /// 文本内容。
    pub text: String,
    /// 渠道侧不透明上下文（如微信 context_token）。
    pub channel_context: serde_json::Value,
}

/// 出站消息（hebbian → IM）。
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    /// 接收者标识。
    pub to: String,
    /// 文本内容。
    pub text: String,
    /// 渠道侧不透明上下文。
    pub channel_context: serde_json::Value,
}
