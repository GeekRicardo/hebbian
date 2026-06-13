//! iLink Bot 协议类型。

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct QrCodeResponse {
    pub qrcode: String,
    pub qrcode_img_content: String,
}

#[derive(Debug, Deserialize)]
pub struct QrCodeStatus {
    pub status: String,
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub ilink_bot_id: Option<String>,
    #[serde(default)]
    pub ilink_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotCredentials {
    pub bot_token: String,
    pub bot_id: String,
    pub user_id: String,
}

/// 扫码登录的单次轮询结果。GUI 每隔几秒调一次 `poll_qrcode_status` 推进状态机。
#[derive(Debug, Clone)]
pub enum QrLoginStatus {
    /// 等待扫码。
    Waiting,
    /// 已扫码，等手机确认。
    Scanned,
    /// 已确认，拿到凭证。
    Confirmed(BotCredentials),
    /// 二维码过期，需重新申请。
    Expired,
}

#[derive(Debug, Serialize)]
pub struct GetUpdatesRequest {
    pub get_updates_buf: String,
    pub base_info: BaseInfo,
}

#[derive(Debug, Default, Deserialize)]
pub struct GetUpdatesResponse {
    #[serde(default)]
    pub msgs: Vec<InboundMsg>,
    #[serde(default)]
    pub get_updates_buf: String,
}

#[derive(Debug, Deserialize)]
pub struct InboundMsg {
    #[serde(default)]
    pub from_user_id: String,
    #[serde(default)]
    pub context_token: String,
    #[serde(default)]
    pub item_list: Vec<MsgItem>,
}

#[derive(Debug, Serialize)]
pub struct SendMessageRequest {
    pub msg: OutboundMsg,
    pub base_info: BaseInfo,
}

#[derive(Debug, Serialize)]
pub struct OutboundMsg {
    pub from_user_id: String,
    pub to_user_id: String,
    pub client_id: String,
    pub message_type: u32,
    pub message_state: u32,
    pub context_token: String,
    pub item_list: Vec<MsgItem>,
}

#[derive(Debug, Serialize)]
pub struct SendTypingRequest {
    pub to_user_id: String,
    pub typing_ticket: String,
    pub typing_action: u32,
    pub base_info: BaseInfo,
}

#[derive(Debug, Serialize)]
pub struct GetConfigRequest {
    pub base_info: BaseInfo,
}

#[derive(Debug, Default, Deserialize)]
pub struct GetConfigResponse {
    #[serde(default)]
    pub typing_ticket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsgItem {
    #[serde(rename = "type")]
    pub item_type: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_item: Option<TextItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextItem {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseInfo {
    pub channel_version: String,
}

impl Default for BaseInfo {
    fn default() -> Self {
        Self {
            channel_version: "1.0.3".into(),
        }
    }
}
