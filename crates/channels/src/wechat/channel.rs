//! 微信渠道实现。

use std::sync::Mutex;

use async_trait::async_trait;
use channel_core::contract::Channel;
use channel_core::message::{InboundMessage, OutboundMessage};

use super::client::ILinkClient;
use super::context_store::ContextStore;

pub struct WeChatChannel {
    client: ILinkClient,
    account_id: String,
    cursor: Mutex<String>,
    context_store: Mutex<ContextStore>,
}

impl WeChatChannel {
    pub fn new(token: String, account_id: String, data_dir: &std::path::Path) -> Self {
        Self {
            client: ILinkClient::new(token),
            context_store: Mutex::new(ContextStore::open(data_dir, &account_id)),
            account_id,
            cursor: Mutex::new(String::new()),
        }
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }
}

#[async_trait]
impl Channel for WeChatChannel {
    fn id(&self) -> &str {
        "wechat"
    }

    fn display_name(&self) -> &str {
        "微信"
    }

    async fn poll(&self) -> anyhow::Result<Vec<InboundMessage>> {
        let cursor = self.cursor.lock().unwrap().clone();
        let response = self.client.get_updates(&cursor).await?;
        *self.cursor.lock().unwrap() = response.get_updates_buf;

        let mut messages = Vec::new();
        for msg in response.msgs {
            if !msg.context_token.is_empty() {
                self.context_store
                    .lock()
                    .unwrap()
                    .set(&msg.from_user_id, &msg.context_token);
            }

            let text = msg
                .item_list
                .iter()
                .filter(|item| item.item_type == 1)
                .filter_map(|item| item.text_item.as_ref())
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>()
                .join("");

            if !text.is_empty() {
                messages.push(InboundMessage {
                    channel: "wechat".into(),
                    from: msg.from_user_id.clone(),
                    text,
                    channel_context: serde_json::json!({
                        "context_token": msg.context_token,
                        "from_user_id": msg.from_user_id,
                    }),
                });
            }
        }

        Ok(messages)
    }

    async fn send_text(&self, msg: &OutboundMessage) -> anyhow::Result<()> {
        let context_token = msg
            .channel_context
            .get("context_token")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                self.context_store
                    .lock()
                    .unwrap()
                    .get(&msg.to)
                    .map(str::to_string)
            })
            .unwrap_or_default();

        if context_token.is_empty() {
            anyhow::bail!("无法向 {} 发消息：缺少 context_token", msg.to);
        }

        self.client
            .send_message(&msg.to, &msg.text, &context_token)
            .await
    }

    async fn send_typing(
        &self,
        to: &str,
        _channel_context: &serde_json::Value,
    ) -> anyhow::Result<()> {
        if let Ok(config) = self.client.get_config().await {
            if !config.typing_ticket.is_empty() {
                let _ = self.client.send_typing(to, &config.typing_ticket, true).await;
            }
        }
        Ok(())
    }
}
