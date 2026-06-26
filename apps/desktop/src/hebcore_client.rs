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
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// hebcore 的 unix-socket 路径（与 hebcore 进程 / hebweb 兼任时一致）。
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
    },
    SetRunMode {
        session_id: &'a str,
        mode: &'a str,
    },
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
}

/// 连常驻 hebcore；连不上则拉起内嵌 hebcore 二进制后重试（connect_or_spawn 范式，
/// §7.8.1 对标 hebisland daemon）。返回一条已连接的 unix-socket。
fn connect_or_spawn(app: &AppHandle, sock: &Path) -> std::io::Result<UnixStream> {
    if let Ok(s) = UnixStream::connect(sock) {
        return Ok(s);
    }
    spawn_bundled_hebcore(app);
    // 轮询等 hebcore 把 socket listen 起来，最多 ~5s（53MB 二进制冷启动需留足时间）。
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(sock) {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    UnixStream::connect(sock)
}

/// 启动期确保常驻 hebcore 在跑（架构 §7.8.1：任何 surface 启动时都拉起 core，谁先启动
/// 谁负责）。连得上 = 已有 hebcore（可能是另一 surface 拉的）；连不上就拉起内嵌二进制，
/// hebcore 自带单例锁，重复拉起安全。`data_dir` 决定 `hebcore.sock` 位置。
pub fn ensure_running(app: &AppHandle, data_dir: &Path) {
    let sock = hebcore_sock(data_dir);
    match connect_or_spawn(app, &sock) {
        Ok(_) => tracing::info!(socket = %sock.display(), "hebcore 就绪"),
        Err(e) => tracing::warn!(socket = %sock.display(), error = %e, "hebcore 未就绪（发消息时会重试拉起）"),
    }
}

/// 拉起 hebcore 进程：release 用 `resource_dir` 内嵌的 hebcore，dev 用 `target/debug/hebcore`。
/// hebcore 自带单例锁，重复拉起安全。
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
    fn on_event(&self, event: protocol::WireEvent);
}

/// 发起一轮对话并阻塞消费事件流到终态（架构 §7.8.6 步骤⑥）。
///
/// 两条连接：subscribe 连接先建立（不漏早期事件），start_run 连接投一次输入即返回。
/// 事件经 `sink` 转发；遇 `run_finished` / `run_failed` / `error` 收尾返回。
pub fn run_conversation(
    app: &AppHandle,
    data_dir: &Path,
    session_id: &str,
    text: &str,
    sink: &dyn RunEventSink,
) -> Result<(), String> {
    let sock = hebcore_sock(data_dir);

    // 订阅连接：先建立，保证不漏 run 早期事件。
    let sub = connect_or_spawn(app, &sock)
        .map_err(|e| format!("连接 hebcore 失败（{}）：{e}", sock.display()))?;
    let mut sub_write = sub.try_clone().map_err(|e| e.to_string())?;
    let mut sub_lines = BufReader::new(sub).lines();
    write_req(
        &mut sub_write,
        &Req::Subscribe { session_id },
    )?;
    // 等订阅确认。
    if let Some(line) = sub_lines.next() {
        let line = line.map_err(|e| e.to_string())?;
        match serde_json::from_str::<Resp>(&line).map_err(|e| e.to_string())? {
            Resp::Subscribed { .. } => {}
            Resp::Error { message } => return Err(format!("订阅失败: {message}")),
            _ => {}
        }
    }

    // 发起 run（另一条连接）。
    let mut run_conn = UnixStream::connect(&sock).map_err(|e| e.to_string())?;
    write_req(&mut run_conn, &Req::StartRun { session_id, text })?;
    let mut run_lines = BufReader::new(run_conn.try_clone().map_err(|e| e.to_string())?).lines();
    if let Some(line) = run_lines.next() {
        let line = line.map_err(|e| e.to_string())?;
        match serde_json::from_str::<Resp>(&line).map_err(|e| e.to_string())? {
            Resp::Accepted => {}
            Resp::Error { message } => return Err(format!("start_run 失败: {message}")),
            _ => {}
        }
    }

    // 消费事件流直到终态。
    for line in sub_lines {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        match serde_json::from_str::<Resp>(&line) {
            Ok(Resp::Event { event }) => {
                let terminal = matches!(
                    &event,
                    protocol::WireEvent::RunFinished { .. }
                        | protocol::WireEvent::Error { .. }
                );
                sink.on_event(event);
                if terminal {
                    break;
                }
            }
            Ok(Resp::Error { message }) => {
                return Err(message);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("解析 hebcore 事件失败: {e}"),
        }
    }
    Ok(())
}

/// 一次性发一个控制请求到 hebcore（审批 / 提问 / 中断 / 插队 / 切 mode），读一行响应。
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
            Resp::Event { .. } => Ok(()),
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
pub fn inject(data_dir: &Path, session_id: &str, text: &str) -> Result<(), String> {
    control_request(data_dir, &Req::Inject { session_id, text })
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
