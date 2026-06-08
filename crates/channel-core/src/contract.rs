//! 渠道契约——所有渠道实现此 trait。

use async_trait::async_trait;

use crate::message::{InboundMessage, OutboundMessage};

#[async_trait]
pub trait Channel: Send + Sync {
    /// 渠道 id（如 `wechat`、`qq`、`feishu`）。
    fn id(&self) -> &str;

    /// 人类可读名称（如「微信」）。
    fn display_name(&self) -> &str;

    /// 长轮询拉取一批入站消息。
    async fn poll(&self) -> anyhow::Result<Vec<InboundMessage>>;

    /// 发送文本消息。
    async fn send_text(&self, msg: &OutboundMessage) -> anyhow::Result<()>;

    /// 发送「正在输入」状态；不支持的渠道可保持默认 no-op。
    async fn send_typing(
        &self,
        to: &str,
        channel_context: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let _ = (to, channel_context);
        Ok(())
    }
}
