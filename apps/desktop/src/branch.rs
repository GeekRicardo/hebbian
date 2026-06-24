//! 旁支对话（branch）的 Tauri command 壳（架构 §8.5）。
//!
//! 业务逻辑已下沉 [`agent_core::branch`]（surface 无关的 `BranchEngine`）：create / discard /
//! cancel / send 全在那儿，desktop 与 hebweb 复用同一份。这里只做 Tauri 适配——解析入参、
//! 用 `AppHandle` 拿 data_dir 组装 `BranchEngine`、把 `WireEvent` 经 `Channel` 投递给前端。
//!
//! managed state 为 [`agent_core::branch::BranchState`]（持有多条旁支内存历史 + cancel flag），
//! 主对话关闭时 `drop_for_session` 连带清理。

use std::sync::Arc;

use agent_core::branch::{BranchEngine, BranchInfo, BranchState};
use agent_core::storage::sessions::Message;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::error::{AppError, AppResult};

/// 用 AppHandle 的 data_dir + 共享 BranchState 组装引擎。
fn engine(app: &AppHandle, state: &State<'_, Arc<BranchState>>) -> AppResult<BranchEngine> {
    let dd = crate::data_dir(app)?;
    Ok(BranchEngine::with_state(dd, state.inner().clone()))
}

/// 新建一条旁支：从主对话当前聊天记录 fork 一份历史。
/// `up_to_message_id` = 分叉点（含该条）；`None` = 继承全部历史。
#[tauri::command]
pub fn branch_create(
    app: AppHandle,
    branch_state: State<'_, Arc<BranchState>>,
    session_id: String,
    up_to_message_id: Option<String>,
) -> AppResult<BranchInfo> {
    engine(&app, &branch_state)?
        .create(session_id, up_to_message_id)
        .map_err(AppError::msg)
}

/// 丢弃一条旁支（关闭子 tab）。
#[tauri::command]
pub fn branch_discard(branch_state: State<'_, Arc<BranchState>>, branch_id: String) {
    branch_state.discard(&branch_id);
}

/// 停止一条旁支正在跑的 run。
#[tauri::command]
pub fn branch_cancel(branch_state: State<'_, Arc<BranchState>>, branch_id: String) {
    branch_state.cancel(&branch_id);
}

/// 向旁支发一轮消息，流式回 [`agent_core::WireEvent`]（前端按主对话同款渲染）。
#[tauri::command]
pub async fn branch_send(
    app: AppHandle,
    branch_state: State<'_, Arc<BranchState>>,
    branch_id: String,
    content: String,
    attachments: Option<Vec<common::attachments::MessageAttachment>>,
    provider_id: Option<String>,
    model: Option<String>,
    on_event: Channel<protocol::WireEvent>,
) -> AppResult<Message> {
    let eng = engine(&app, &branch_state)?;
    eng.send(
        branch_id,
        content,
        attachments.unwrap_or_default(),
        provider_id,
        model,
        move |event| {
            let _ = on_event.send(event);
        },
    )
    .await
    .map_err(AppError::msg)
}
