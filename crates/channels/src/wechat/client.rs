//! iLink Bot HTTP 客户端。

use base64::Engine;
use rand::Rng;
use reqwest::Client;

use super::types::*;

const BASE_URL: &str = "https://ilinkai.weixin.qq.com";

pub struct ILinkClient {
    http: Client,
    token: String,
}

impl ILinkClient {
    pub fn new(token: String) -> Self {
        Self {
            http: Client::new(),
            token,
        }
    }

    fn bot_headers(&self) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "AuthorizationType",
            HeaderValue::from_static("ilink_bot_token"),
        );
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", self.token)).expect("valid bearer token"),
        );

        let uin: u32 = rand::thread_rng().gen();
        let uin = base64::engine::general_purpose::STANDARD.encode(uin.to_string());
        headers.insert(
            "X-WECHAT-UIN",
            HeaderValue::from_str(&uin).expect("valid uin"),
        );
        headers
    }

    async fn post_bot<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: &impl serde::Serialize,
    ) -> anyhow::Result<T> {
        let raw = serde_json::to_vec(body)?;
        let response = self
            .http
            .post(format!("{BASE_URL}/ilink/bot/{endpoint}"))
            .headers(self.bot_headers())
            .header("Content-Length", raw.len().to_string())
            .body(raw)
            .timeout(std::time::Duration::from_secs(35))
            .send()
            .await?
            .error_for_status()?;

        let text = response.text().await?;
        let text = text.trim();
        if text.is_empty() {
            return Ok(serde_json::from_str("{}")?);
        }
        Ok(serde_json::from_str(text)?)
    }

    pub async fn get_updates(&self, cursor: &str) -> anyhow::Result<GetUpdatesResponse> {
        self.post_bot(
            "getupdates",
            &GetUpdatesRequest {
                get_updates_buf: cursor.to_string(),
                base_info: BaseInfo::default(),
            },
        )
        .await
    }

    pub async fn send_message(
        &self,
        to_user_id: &str,
        text: &str,
        context_token: &str,
    ) -> anyhow::Result<()> {
        let request = SendMessageRequest {
            msg: OutboundMsg {
                from_user_id: String::new(),
                to_user_id: to_user_id.to_string(),
                client_id: format!("heb-{}", uuid::Uuid::new_v4().simple()),
                message_type: 2,
                message_state: 2,
                context_token: context_token.to_string(),
                item_list: vec![MsgItem {
                    item_type: 1,
                    text_item: Some(TextItem {
                        text: text.to_string(),
                    }),
                }],
            },
            base_info: BaseInfo::default(),
        };
        let _: serde_json::Value = self.post_bot("sendmessage", &request).await?;
        Ok(())
    }

    pub async fn get_config(&self) -> anyhow::Result<GetConfigResponse> {
        self.post_bot(
            "getconfig",
            &GetConfigRequest {
                base_info: BaseInfo::default(),
            },
        )
        .await
    }

    pub async fn send_typing(
        &self,
        to_user_id: &str,
        typing_ticket: &str,
        start: bool,
    ) -> anyhow::Result<()> {
        let request = SendTypingRequest {
            to_user_id: to_user_id.to_string(),
            typing_ticket: typing_ticket.to_string(),
            typing_action: u32::from(start),
            base_info: BaseInfo::default(),
        };
        let _: serde_json::Value = self.post_bot("sendtyping", &request).await?;
        Ok(())
    }
}
