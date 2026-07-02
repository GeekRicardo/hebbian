//! Desktop 的 hebcore 客户端（架构 §7.8.2 / §7.8.6 步骤⑥）。
//!
//! desktop 对话主链路退化为 hebcore 的客户端：不再进程内嵌 `Harness` 跑 run，而是连
//! 常驻 hebcore 的 unix-socket，发 `StartRun` + `Subscribe`，把 hebcore 推回的
//! `WireEvent` 经 desktop 的 native 出口（灵动岛 / 微信转发 / 前端 Channel）转发。
//! 运行时控制（审批 / 提问 / 中断 / 插队 / 切 mode）经 `Approve` / `Answer` / `Interrupt`
//! / `Inject` / `SetRunMode` 跨进程代理到 hebcore——desktop 不再持有进程内 HitlGate。
//!
//! native 能力（CDP 浏览器 / 终端 / 托盘 / 灵动岛 / 微信 / 快捷键）仍进程内自理（§7.2.1）。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

/// hebcore 的 unix-socket 路径（全局共享，§7.8.1）。版本区分靠 §7.8.7 版本协商——sock 固定，
/// hebcore 报告 build_version，desktop **启动时**核对、旧版提示后杀掉换新；不再按 app 派生
/// per-app sock（§7.8.8 已回退：冷启动 + 版本耦合摩擦过大，且与旧 hebcore 不兼容）。
fn hebcore_sock(data_dir: &Path) -> PathBuf {
    data_dir.join("hebcore.sock")
}

/// 出站消息（对应 surface_session::transport::HebcoreRequest）。
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Req<'a> {
    StartRun {
        session_id: &'a str,
        text: &'a str,
        attachments: &'a [common::attachments::MessageAttachment],
    },
    Subscribe {
        session_id: &'a str,
    },
    Approve {
        session_id: &'a str,
        request_id: &'a str,
        decision: protocol::ApprovalDecision,
    },
    Answer {
        session_id: &'a str,
        request_id: &'a str,
        answer: protocol::UserAnswer,
    },
    Interrupt {
        session_id: &'a str,
    },
    Inject {
        session_id: &'a str,
        text: &'a str,
        attachments: &'a [common::attachments::MessageAttachment],
    },
    SetRunMode {
        session_id: &'a str,
        mode: &'a str,
    },
    /// 查询运行中 hebcore 的版本身份（§7.8.7 版本协商）。
    GetVersion,
    /// 请求运行中 hebcore 优雅关停（换版前；有活跃 run 会被拒）。
    Shutdown,
    /// 订阅 hebcore 全局日志流（§4.10）：把 hebcore 进程日志注入本进程 LOG_TX 喂日志面板。
    SubscribeLogs,
}

/// 入站消息（对应 surface_session::transport::HebcoreResponse）。
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Resp {
    Rpc {
        #[allow(dead_code)]
        resp: serde_json::Value,
    },
    Accepted,
    Subscribed {
        #[allow(dead_code)]
        session_id: String,
    },
    Event {
        event: protocol::WireEvent,
    },
    Error {
        message: String,
    },
    /// GetVersion 应答（§7.8.7）。
    Version {
        build_version: String,
        bin_name: String,
        #[allow(dead_code)]
        pid: u32,
        has_active_run: bool,
    },
    /// hebcore 转发来的一条日志行（应 SubscribeLogs，§4.10）。
    Log {
        line: observability::LogLine,
    },
}

/// 连常驻 hebcore；连不上则拉起内嵌 hebcore 二进制后重试（connect_or_spawn 范式，
/// §7.8.1 对标 hebisland daemon）。返回一条已连接的 unix-socket。
fn connect_or_spawn(app: &AppHandle, sock: &Path) -> std::io::Result<UnixStream> {
    if let Ok(s) = UnixStream::connect(sock) {
        return Ok(s);
    }
    spawn_bundled_hebcore(app);
    // 轮询等 hebcore 把 socket listen 起来，最多 ~20s。release 53MB 二进制**首次**冷启动
    // （磁盘 cache 冷 + per-app sock 是全新的、没有常驻进程可复用）可能要十几秒；5s 太短会
    // 让启动后第一个操作（如打开老对话发消息）撞冷启动窗口失败 No such file（§7.8.8 回归）。
    // 正常情况（hebcore 已 ready）首个 connect 立即成功，不受此上限影响。
    for _ in 0..400 {
        if let Ok(s) = UnixStream::connect(sock) {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    UnixStream::connect(sock)
}

/// 起一条常驻后台线程订阅 hebcore 全局日志流，把每条注入本进程 LOG_TX（§4.10 多进程
/// 日志聚合）：run 移 hebcore 后 agent_loop 的日志都在 hebcore 进程，desktop 日志面板靠
/// 这条流才看得到、才实时刷新。断连（hebcore 重启 / 换版）后每 2s 自动重连。
pub fn spawn_log_forwarder(data_dir: std::path::PathBuf) {
    std::thread::spawn(move || loop {
        let sock = hebcore_sock(&data_dir);
        if let Ok(mut conn) = UnixStream::connect(&sock) {
            if write_req(&mut conn, &Req::SubscribeLogs).is_ok() {
                if let Ok(read) = conn.try_clone() {
                    for line in BufReader::new(read).lines() {
                        let Ok(line) = line else { break };
                        if let Ok(Resp::Log { line: log }) = serde_json::from_str::<Resp>(&line) {
                            if let Some(tx) = observability::log_sender() {
                                let _ = tx.send(log);
                            }
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2)); // 断连 / 连不上：等会儿重连
    });
}

/// 运行中 hebcore 的版本身份（§7.8.7）。
struct RunningVersion {
    build_version: String,
    bin_name: String,
    has_active_run: bool,
}

/// 向已连上的 hebcore 问一次版本。旧 hebcore 不认识 GetVersion → 回 Error/EOF →
/// 返回 `None`，调用方视作"旧到没有版本协议 = 必然 stale"（这是个完美信号）。
fn query_version(sock: &Path) -> Option<RunningVersion> {
    let mut conn = UnixStream::connect(sock).ok()?;
    write_req(&mut conn, &Req::GetVersion).ok()?;
    let mut lines = BufReader::new(conn.try_clone().ok()?).lines();
    let line = lines.next()?.ok()?;
    match serde_json::from_str::<Resp>(&line).ok()? {
        Resp::Version {
            build_version,
            bin_name,
            has_active_run,
            ..
        } => Some(RunningVersion {
            build_version,
            bin_name,
            has_active_run,
        }),
        _ => None,
    }
}

/// 版本协商（§7.8.7）：连上 hebcore 后核对它的版本是否 = 当前磁盘 binary 版本，旧版则弹窗
/// 让用户确认换版（kill 旧 + 起新）。**只在发消息路径调用**（非主线程，native dialog 的
/// blocking_show 安全；启动 setup 在主线程不能 blocking）。返回后保证连的是当前版本的
/// hebcore，或用户明确选择沿用旧版。连不上 hebcore 时直接返回——后续 connect_or_spawn 会
/// 拉起当前版本的新进程。
fn negotiate_version(app: &AppHandle, sock: &Path) {
    let current = env!("HEBBIAN_BUILD_VERSION");
    let Ok(_probe) = UnixStream::connect(sock) else {
        return; // 没有 hebcore 在跑，无需协商
    };
    drop(_probe);

    let running = query_version(sock);
    let (running_ver, bin_name, has_active_run) = match running {
        Some(v) => (v.build_version, v.bin_name, v.has_active_run),
        // 旧 hebcore 不认 GetVersion → 必然 stale。
        None => (
            "（旧版本，无版本协议）".to_string(),
            "hebcore".to_string(),
            false,
        ),
    };

    if running_ver == current {
        return; // 同版本，正常
    }
    // hebweb 兼任 hebcore：它是另一个服务，版本本就不同，不该被当 stale 误杀（§7.8.7 trap B）。
    if bin_name == "hebweb" {
        tracing::info!(running = %running_ver, "运行中核心是 hebweb 兼任，跳过 hebcore 版本协商");
        return;
    }
    if has_active_run {
        app.dialog()
            .message("有对话正在运行，没法切换到最新版核心。等这轮对话跑完再重启 App。")
            .title("核心是旧版本")
            .blocking_show();
        return;
    }

    let confirmed = app
        .dialog()
        .message(format!(
            "检测到正在运行的核心是旧版本，要关掉它、换成最新版吗？\n\n运行中：{running_ver}\n最新：{current}"
        ))
        .title("核心是旧版本")
        .buttons(MessageDialogButtons::OkCancel)
        .blocking_show();
    if !confirmed {
        return; // 用户选择沿用旧版
    }

    // 发 Shutdown 让 hebcore 优雅退（新版认协议、Accepted 后 exit；已确认无活跃 run）。
    // 旧版**不认** Shutdown（回 Error、不退）——靠下面「等死超时 → pkill」兜底强杀。
    if let Ok(mut conn) = UnixStream::connect(sock) {
        let _ = write_req(&mut conn, &Req::Shutdown);
        let _ = BufReader::new(
            conn.try_clone()
                .unwrap_or_else(|_| conn.try_clone().unwrap()),
        )
        .lines()
        .next();
    }
    // 等旧进程真死（~3s）。connect 失败 = 已退（单例锁 + sock 由 OS 释放）。
    let mut died = false;
    for _ in 0..60 {
        if UnixStream::connect(sock).is_err() {
            died = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // 没死 = 旧版不认 Shutdown → pkill 强杀兜底（`-x` 精确进程名 hebcore，不误伤别的）+
    // 清残留 sock。这让「杀旧版换新」对**没有版本协议的旧 hebcore**（首次升级场景）也生效。
    if !died {
        tracing::warn!("hebcore 未响应 Shutdown（可能是旧版不认协议），pkill 强杀");
        let _ = std::process::Command::new("pkill")
            .arg("-x")
            .arg("hebcore")
            .status();
        let _ = std::fs::remove_file(sock);
        for _ in 0..60 {
            if UnixStream::connect(sock).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    // 拉起当前版本的新 hebcore（自带单例锁，重复拉起安全）。
    spawn_bundled_hebcore(app);
    for _ in 0..100 {
        if UnixStream::connect(sock).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    tracing::info!(from = %running_ver, to = %current, "hebcore 已换到当前版本");
}

/// 启动期确保常驻 hebcore 在跑（架构 §7.8.1：任何 surface 启动时都拉起 core，谁先启动
/// 谁负责）。先做版本协商（§7.8.7）：连上现有 hebcore 核对版本，旧版提示后杀掉换新；再
/// connect_or_spawn 确保当前版本 hebcore 在跑。**版本检查在启动时做**（本函数在 setup 的
/// 后台线程跑，native dialog blocking_show 安全），发消息时不再检查、不卡。
pub fn ensure_running(app: &AppHandle, data_dir: &Path) {
    let sock = hebcore_sock(data_dir);
    // 等 app event loop 起来再做版本协商——negotiate_version 可能弹 native dialog，dialog 要
    // 主线程 event loop 在跑才能显示；本函数在 setup 的后台线程，太早弹会卡（dialog 往主线程
    // 的投递没人处理）。等一下让 app.run 起来。
    std::thread::sleep(Duration::from_millis(800));
    // 启动时版本协商：现有 hebcore 是旧版 → 弹窗提示 → Shutdown / pkill 杀掉 → 起当前版本。
    negotiate_version(app, &sock);
    match connect_or_spawn(app, &sock) {
        Ok(_) => tracing::info!(socket = %sock.display(), "hebcore 就绪"),
        Err(e) => {
            tracing::warn!(socket = %sock.display(), error = %e, "hebcore 未就绪（发消息时会重试拉起）")
        }
    }
}

/// 拉起 hebcore 进程：release 用 `resource_dir` 内嵌的 hebcore，dev 用 `target/debug/hebcore`。
/// hebcore 自带单例锁，重复拉起安全。不传额外参数——hebcore 用默认全局 sock（`<data_dir>/
/// hebcore.sock`），兼容旧版 hebcore（§7.8.8 回退后不再传 `--sock-path`）。
fn spawn_bundled_hebcore(app: &AppHandle) {
    let candidates = bundled_hebcore_paths(app);
    for bin in candidates {
        if bin.exists() {
            match std::process::Command::new(&bin).spawn() {
                Ok(_) => {
                    tracing::info!("已拉起 hebcore: {}", bin.display());
                    return;
                }
                Err(e) => tracing::warn!("拉起 hebcore 失败 {}: {e}", bin.display()),
            }
        }
    }
    tracing::warn!("未找到可拉起的 hebcore 二进制（release 内嵌 / dev target/debug）");
}

/// hebcore 二进制候选路径：release 资源目录 + dev target 目录。
fn bundled_hebcore_paths(app: &AppHandle) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        v.push(resource_dir.join("hebcore"));
    }
    // dev：从当前 exe 旁找（target/debug/hebcore）。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("hebcore"));
        }
    }
    v
}

/// 一次对话 run 的事件回调：desktop 把每个 WireEvent 经 native 出口转发。
pub trait RunEventSink: Send {
    /// 返回 false 表示前端通道已关闭，调用方可停止订阅线程。
    fn on_event(&self, event: protocol::WireEvent) -> bool;
}

/// 向 hebcore 投递一轮输入；Accepted 后立即返回，事件由独立 Subscribe 通道长期接收。
pub fn start_run(
    app: &AppHandle,
    data_dir: &Path,
    session_id: &str,
    text: &str,
    attachments: &[common::attachments::MessageAttachment],
) -> Result<(), String> {
    let sock = hebcore_sock(data_dir);
    let mut conn = connect_or_spawn(app, &sock)
        .map_err(|e| format!("连接 hebcore 失败（{}）：{e}", sock.display()))?;
    write_req(
        &mut conn,
        &Req::StartRun {
            session_id,
            text,
            attachments,
        },
    )?;
    let mut lines = BufReader::new(conn.try_clone().map_err(|e| e.to_string())?).lines();
    if let Some(line) = lines.next() {
        let line = line.map_err(|e| e.to_string())?;
        match serde_json::from_str::<Resp>(&line).map_err(|e| e.to_string())? {
            Resp::Accepted => Ok(()),
            Resp::Error { message } => Err(format!("start_run 失败: {message}")),
            _ => Ok(()),
        }
    } else {
        Err("hebcore 无响应".into())
    }
}

/// 长期订阅一个 session 的后续事件。订阅者断开只结束本线程，不影响 hebcore 内的 run。
pub fn subscribe_session_once(
    app: &AppHandle,
    data_dir: &Path,
    session_id: &str,
    sink: &dyn RunEventSink,
) -> Result<SubscribeSessionEnd, String> {
    let sock = hebcore_sock(data_dir);
    let sub = connect_or_spawn(app, &sock)
        .map_err(|e| format!("连接 hebcore 失败（{}）：{e}", sock.display()))?;
    let mut sub_write = sub.try_clone().map_err(|e| e.to_string())?;
    let mut sub_lines = BufReader::new(sub).lines();
    write_req(&mut sub_write, &Req::Subscribe { session_id })?;
    if let Some(line) = sub_lines.next() {
        let line = line.map_err(|e| e.to_string())?;
        match serde_json::from_str::<Resp>(&line).map_err(|e| e.to_string())? {
            Resp::Subscribed { .. } => {}
            Resp::Error { message } => return Err(format!("订阅失败: {message}")),
            _ => {}
        }
    }
    for line in sub_lines {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        match serde_json::from_str::<Resp>(&line) {
            Ok(Resp::Event { event }) => {
                if !sink.on_event(event) {
                    return Ok(SubscribeSessionEnd::SinkClosed);
                }
            }
            Ok(Resp::Error { message }) => return Err(message),
            Ok(_) => {}
            Err(e) => tracing::warn!("解析 hebcore 事件失败: {e}"),
        }
    }
    Ok(SubscribeSessionEnd::Subscribed)
}

/// 长期订阅一个 session 的后续事件；hebcore 断连 / 重启时自动重连。
/// 每次订阅成功都会调用 `on_subscribed`，调用方用它通知前端按 session.jsonl 补一次快照。
pub fn subscribe_session_reconnecting(
    app: &AppHandle,
    data_dir: &Path,
    session_id: &str,
    sink: &dyn RunEventSink,
    mut on_subscribed: impl FnMut(),
) -> Result<(), String> {
    loop {
        match subscribe_session_once(app, data_dir, session_id, sink) {
            Ok(SubscribeSessionEnd::Subscribed) => on_subscribed(),
            Ok(SubscribeSessionEnd::SinkClosed) => return Ok(()),
            Err(e) => tracing::warn!(session_id = %session_id, error = %e, "desktop session 事件订阅断开，准备重连"),
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub enum SubscribeSessionEnd {
    Subscribed,
    SinkClosed,
}

/// 一次性发一个控制请求到 hebcore（审批 / 提问 / 中断 / 插队 / 切 mode），读一行响应。
/// **直接 connect、不轮询等待**——这些命令在 hebcore 没起时本就无意义（inject / setRunMode /
/// interrupt 的调用方会吞掉失败、不打扰用户），加轮询反而让切对话等控制操作卡顿（§7.8.8）。
/// hebcore 已 ready 时（绝大多数情况）connect 立即成功。
fn control_request(data_dir: &Path, req: &Req) -> Result<(), String> {
    let sock = hebcore_sock(data_dir);
    let mut conn = UnixStream::connect(&sock)
        .map_err(|e| format!("连接 hebcore 失败（{}）：{e}", sock.display()))?;
    write_req(&mut conn, req)?;
    let mut lines = BufReader::new(conn.try_clone().map_err(|e| e.to_string())?).lines();
    if let Some(line) = lines.next() {
        let line = line.map_err(|e| e.to_string())?;
        match serde_json::from_str::<Resp>(&line).map_err(|e| e.to_string())? {
            Resp::Accepted | Resp::Rpc { .. } | Resp::Subscribed { .. } => Ok(()),
            Resp::Error { message } => Err(message),
            Resp::Event { .. } | Resp::Version { .. } | Resp::Log { .. } => Ok(()),
        }
    } else {
        Err("hebcore 无响应".into())
    }
}

/// 结算一条审批（HITL）→ hebcore。
pub fn approve(
    data_dir: &Path,
    session_id: &str,
    request_id: &str,
    decision: protocol::ApprovalDecision,
) -> Result<(), String> {
    control_request(
        data_dir,
        &Req::Approve {
            session_id,
            request_id,
            decision,
        },
    )
}

/// 结算一条提问（HITL）→ hebcore。
pub fn answer(
    data_dir: &Path,
    session_id: &str,
    request_id: &str,
    answer: protocol::UserAnswer,
) -> Result<(), String> {
    control_request(
        data_dir,
        &Req::Answer {
            session_id,
            request_id,
            answer,
        },
    )
}

/// 中断当前 run → hebcore。
pub fn interrupt(data_dir: &Path, session_id: &str) -> Result<(), String> {
    control_request(data_dir, &Req::Interrupt { session_id })
}

/// 插队一条 user 输入 → hebcore。
pub fn inject(
    data_dir: &Path,
    session_id: &str,
    text: &str,
    attachments: &[common::attachments::MessageAttachment],
) -> Result<(), String> {
    control_request(
        data_dir,
        &Req::Inject {
            session_id,
            text,
            attachments,
        },
    )
}

/// 即时切换 run mode → hebcore。
pub fn set_run_mode(data_dir: &Path, session_id: &str, mode: &str) -> Result<(), String> {
    control_request(data_dir, &Req::SetRunMode { session_id, mode })
}

/// 写一行 JSON 请求（带换行 + flush）。
fn write_req(w: &mut impl Write, req: &Req) -> Result<(), String> {
    let mut s = serde_json::to_string(req).map_err(|e| e.to_string())?;
    s.push('\n');
    w.write_all(s.as_bytes()).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct ClosedSink;

    impl RunEventSink for ClosedSink {
        fn on_event(&self, _event: protocol::WireEvent) -> bool {
            false
        }
    }

    #[test]
    fn retry_decision_stops_when_shutdown_requested() {
        let shutdown = Arc::new(AtomicBool::new(true));
        assert!(!should_retry_subscription(&ClosedSink, Some(&shutdown)));
    }

    #[test]
    fn retry_decision_continues_while_surface_is_alive() {
        let shutdown = Arc::new(AtomicBool::new(false));
        assert!(should_retry_subscription(&ClosedSink, Some(&shutdown)));
    }
}
