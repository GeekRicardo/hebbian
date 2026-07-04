//! 跨边界消息元数据类型。
//!
//! [`PendingMessageMeta`] 是 `PendingUserInput` 携带的元数据：定义在 `protocol` 以便
//! `common`（`PendingUserInput` 所在 crate）引用，不破坏 crate DAG。
//! `agent-core` 负责 `PendingMessageMeta → MessageMeta` 的转换。

use serde::{Deserialize, Serialize};

/// `PendingUserInput` 携带的元数据（`common` 可引用，不依赖 `agent-core`）。
///
/// 当前唯一变体是 `SystemNotification`（wakeup / cron 触发）。未来如需在注入路径
/// 携带其他元数据类型，在此追加变体并补齐 `agent-core` 侧的转换。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PendingMessageMeta {
    /// 系统注入的通知（wakeup / cron），对应 `agent_core::storage::sessions::MessageMeta::SystemNotification`。
    SystemNotification {
        /// 通知来源类别：`bg_task_finished` / `cron_fired`。
        kind: String,
        /// 关联的后台 task_id（`bg_task_finished` 才有）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// 触发该通知的 tool_call.id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
}
