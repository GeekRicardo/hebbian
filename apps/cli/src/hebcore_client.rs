//! heb 的 hebcore 客户端（架构 §7.8.4 `--connect` 共享模式）。
//!
//! 连接常驻 hebcore 进程的 unix-socket（`<data_dir>/hebcore.sock`），订阅一个 session
//! 的事件流并发起对话——看到的是与 desktop / hebweb 同一份**活内存状态**（§7.8.5）。
//!
//! 与 daemon 模式（`heb new`，§7.8.4 `--stdio` 隔离）的区别：daemon 自己内嵌 agent_core
//! 跑 run；这里 heb 只是 hebcore 的一个客户端，run 在 hebcore 进程里跑。

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// hebcore 的 unix-socket 路径。
fn hebcore_sock(data_dir: &Path) -> PathBuf {
    data_dir.join("hebcore.sock")
}

fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".hebbian")
}

/// 出站消息（与 hebcore 的 `HebcoreRequest` 对应）。
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Req<'a> {
    StartRun { session_id: &'a str, text: &'a str },
    Subscribe { session_id: &'a str },
}

/// 入站消息（与 hebcore 的 `HebcoreResponse` 对应）。
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Resp {
    Rpc { resp: serde_json::Value },
    Accepted,
    Subscribed { session_id: String },
    Event { event: serde_json::Value },
    Error { message: String },
}

/// `heb connect`：连常驻 hebcore，订阅 `session_id` 的事件流，发起一轮对话（`text`），
/// 把事件以 NDJSON 打到 stdout（与 daemon 模式同款脚本化输出）。
///
/// 订阅与发起走**两条连接**：subscribe 连接持续收事件流，start_run 连接投一次输入即返回。
pub async fn connect_run(
    data_dir: Option<PathBuf>,
    session_id: String,
    text: String,
) -> Result<()> {
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    let sock = hebcore_sock(&data_dir);

    // 订阅连接：先建立，保证不漏 run 早期事件。
    let sub_stream = UnixStream::connect(&sock).await.with_context(|| {
        format!("连接 hebcore 失败（{sock:?}）——常驻 hebcore 是否在运行？")
    })?;
    let (sub_read, mut sub_write) = sub_stream.into_split();
    let subscribe = serde_json::to_string(&Req::Subscribe {
        session_id: &session_id,
    })?;
    sub_write.write_all(subscribe.as_bytes()).await?;
    sub_write.write_all(b"\n").await?;
    sub_write.flush().await?;

    let mut sub_lines = BufReader::new(sub_read).lines();
    // 等订阅确认。
    if let Some(line) = sub_lines.next_line().await? {
        match serde_json::from_str::<Resp>(&line)? {
            Resp::Subscribed { session_id } => {
                println!("{}", serde_json::json!({"event":"subscribed","session_id":session_id}));
            }
            Resp::Error { message } => return Err(anyhow!("订阅失败: {message}")),
            _ => {}
        }
    }

    // 发起 run（另一条连接）。
    let run_stream = UnixStream::connect(&sock).await?;
    let (run_read, mut run_write) = run_stream.into_split();
    let start = serde_json::to_string(&Req::StartRun {
        session_id: &session_id,
        text: &text,
    })?;
    run_write.write_all(start.as_bytes()).await?;
    run_write.write_all(b"\n").await?;
    run_write.flush().await?;
    let mut run_lines = BufReader::new(run_read).lines();
    if let Some(line) = run_lines.next_line().await? {
        match serde_json::from_str::<Resp>(&line)? {
            Resp::Accepted => {}
            Resp::Error { message } => return Err(anyhow!("start_run 失败: {message}")),
            _ => {}
        }
    }

    // 消费事件流直到 run 终态。
    while let Some(line) = sub_lines.next_line().await? {
        match serde_json::from_str::<Resp>(&line) {
            Ok(Resp::Event { event }) => {
                println!("{event}");
                let ty = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if matches!(ty, "run_finished" | "run_failed" | "error") {
                    break;
                }
            }
            Ok(Resp::Error { message }) => {
                eprintln!("hebcore error: {message}");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("解析 hebcore 事件失败: {e}");
            }
        }
    }
    Ok(())
}

/// `heb connect --rpc <method>`：连 hebcore 发一个同步 API 请求，打印响应（调试用）。
pub async fn connect_rpc(data_dir: Option<PathBuf>, method: String) -> Result<()> {
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    let sock = hebcore_sock(&data_dir);
    let stream = UnixStream::connect(&sock)
        .await
        .with_context(|| format!("连接 hebcore 失败（{sock:?}）"))?;
    let (read, mut write) = stream.into_split();
    let req = serde_json::json!({"kind":"rpc","req":{"method": method}});
    write.write_all(req.to_string().as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.flush().await?;
    let mut lines = BufReader::new(read).lines();
    if let Some(line) = lines.next_line().await? {
        match serde_json::from_str::<Resp>(&line)? {
            Resp::Rpc { resp } => println!("{resp}"),
            Resp::Error { message } => return Err(anyhow!("{message}")),
            _ => {}
        }
    }
    Ok(())
}
