use serde::{Deserialize, Serialize};

/// 从 surface（desktop / heb / hebweb）发送给 island 的消息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SocketMessage {
    /// 展示一条新通知
    #[serde(rename = "show")]
    Show { id: String, card: NotificationCard },
    /// 用户动作回传（surface → island，用于取消定时器等）
    #[serde(rename = "action")]
    Action { id: String, action: String },
    /// 关闭通知窗口
    #[serde(rename = "dismiss")]
    Dismiss { id: String },
}

/// 通知卡片数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationCard {
    pub id: String,
    #[serde(rename = "cardType")]
    pub card_type: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// island 回传给 surface 的动作事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEvent {
    pub msg_id: String,
    pub action: String,
}