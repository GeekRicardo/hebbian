//! 微信渠道的 Desktop 内嵌运行（架构 §7.5.1，2026-06-13）。
//!
//! 登录与运行都收进 Desktop 进程：设置弹窗「微信」页签点登录 → 前端把二维码 content
//! 渲染成图片 → 轮询扫码状态 → confirmed 后存凭证并在进程内 spawn `ChannelBridge::run_loop`。
//! Desktop 托盘后台常驻，关闭主窗口不退进程，微信渠道继续收发。

use std::path::PathBuf;
use std::sync::Mutex;

use channel_core::bridge::ChannelBridge;
use channel_core::owner_state::OwnerState;
use channels::wechat::channel::WeChatChannel;
use channels::wechat::login;
use channels::wechat::types::QrLoginStatus;
use common::{AppError, AppResult};
use serde::Serialize;
use std::sync::Arc;
use tauri::async_runtime::{spawn, JoinHandle};
use tauri::{AppHandle, Manager};

/// 正在后台运行的渠道：持有 run_loop 任务句柄，停止时 abort。
struct RunningChannel {
    bot_id: String,
    handle: JoinHandle<()>,
}

#[derive(Default)]
pub struct WeChatState {
    running: Mutex<Option<RunningChannel>>,
}

impl WeChatState {
    fn running_bot_id(&self) -> Option<String> {
        self.running
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.bot_id.clone())
    }

    fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            running.handle.abort();
        }
    }

    fn set(&self, bot_id: String, handle: JoinHandle<()>) {
        let mut slot = self.running.lock().unwrap();
        if let Some(prev) = slot.take() {
            prev.handle.abort();
        }
        *slot = Some(RunningChannel { bot_id, handle });
    }
}

#[derive(Serialize)]
pub struct QrCodePayload {
    /// 二维码 SVG 字符串，前端直接 inline 显示，无需二维码库。
    pub svg: String,
    /// 轮询扫码状态用的 id。
    pub qrcode_id: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoginPollResult {
    Waiting,
    Scanned,
    Confirmed { bot_id: String },
    Expired,
}

#[derive(Serialize)]
pub struct WeChatStatus {
    pub logged_in: bool,
    pub running: bool,
    pub bot_id: Option<String>,
}

fn data_dir() -> PathBuf {
    agent_core::storage::default_data_dir()
}

/// 申请登录二维码，返回 SVG（前端 inline 显示）+ qrcode_id（轮询用）。
#[tauri::command]
pub async fn wechat_login_start() -> AppResult<QrCodePayload> {
    let (qrcode_id, content) = login::request_qrcode()
        .await
        .map_err(|e| AppError::msg(format!("申请二维码失败：{e}")))?;
    let svg = login::render_qr_svg(&content)
        .map_err(|e| AppError::msg(format!("渲染二维码失败：{e}")))?;
    Ok(QrCodePayload { svg, qrcode_id })
}

/// 轮询一次扫码状态。confirmed 时存凭证并在进程内启动渠道运行。
#[tauri::command]
pub async fn wechat_login_poll(
    app: AppHandle,
    qrcode_id: String,
) -> AppResult<LoginPollResult> {
    let status = login::poll_qrcode_status(&qrcode_id)
        .await
        .map_err(|e| AppError::msg(format!("查询扫码状态失败：{e}")))?;
    match status {
        QrLoginStatus::Waiting => Ok(LoginPollResult::Waiting),
        QrLoginStatus::Scanned => Ok(LoginPollResult::Scanned),
        QrLoginStatus::Expired => Ok(LoginPollResult::Expired),
        QrLoginStatus::Confirmed(credentials) => {
            let dir = data_dir();
            login::save_credentials(&dir, &credentials)
                .map_err(|e| AppError::msg(format!("保存凭证失败：{e}")))?;
            let bot_id = credentials.bot_id.clone();
            spawn_channel(&app, credentials.bot_token, bot_id.clone());
            Ok(LoginPollResult::Confirmed { bot_id })
        }
    }
}

/// 查询微信渠道状态：是否已登录（有凭证）、是否正在后台运行。
#[tauri::command]
pub fn wechat_status(app: AppHandle) -> AppResult<WeChatStatus> {
    let state = app
        .try_state::<Arc<WeChatState>>()
        .ok_or_else(|| AppError::msg("WeChatState 未注册"))?;
    let running_bot_id = state.running_bot_id();
    let logged_in_bot_id = running_bot_id.clone().or_else(|| latest_credentials_bot_id());
    Ok(WeChatStatus {
        logged_in: logged_in_bot_id.is_some(),
        running: running_bot_id.is_some(),
        bot_id: logged_in_bot_id,
    })
}

/// 用已存凭证启动后台运行（已登录但进程重启后重新拉起）。
#[tauri::command]
pub fn wechat_start(app: AppHandle, bot_id: String) -> AppResult<()> {
    let dir = data_dir();
    let credentials = login::load_credentials(&dir, &bot_id)
        .map_err(|e| AppError::msg(format!("读取凭证失败（请重新扫码登录）：{e}")))?;
    spawn_channel(&app, credentials.bot_token, bot_id);
    Ok(())
}

/// 停止后台运行（不删凭证，可再 start）。
#[tauri::command]
pub fn wechat_stop(app: AppHandle) -> AppResult<()> {
    let state = app
        .try_state::<Arc<WeChatState>>()
        .ok_or_else(|| AppError::msg("WeChatState 未注册"))?;
    state.stop();
    Ok(())
}

/// 在 Desktop 进程内 spawn 渠道运行循环，句柄存进 WeChatState。
fn spawn_channel(app: &AppHandle, bot_token: String, bot_id: String) {
    let Some(state) = app.try_state::<Arc<WeChatState>>() else {
        return;
    };
    let state = state.inner().clone();
    let dir = data_dir();
    let bot_id_for_task = bot_id.clone();
    let handle = spawn(async move {
        let channel = Arc::new(WeChatChannel::new(
            bot_token,
            bot_id_for_task.clone(),
            &dir,
        ));
        let mut owner_state = OwnerState::load(&dir, "wechat", &bot_id_for_task);
        let bridge = ChannelBridge::new(dir.clone());
        if let Err(err) = bridge
            .run_loop(channel, &mut owner_state, &bot_id_for_task)
            .await
        {
            tracing::error!(error = %err, "微信渠道运行循环退出");
        }
    });
    state.set(bot_id, handle);
}

/// 扫已存凭证目录，返回任一已登录账号的 bot_id（首版只支持单账号）。
fn latest_credentials_bot_id() -> Option<String> {
    let dir = data_dir().join("channels").join("wechat");
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        if entry.path().join("credentials.json").is_file() {
            if let Some(name) = entry.file_name().to_str() {
                return Some(name.to_string());
            }
        }
    }
    None
}
