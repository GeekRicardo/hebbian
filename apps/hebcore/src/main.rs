//! hebcore — 常驻核心进程（架构 §7.8.1）。
//!
//! 持有唯一 dispatch（[`core_rpc::dispatch`]）+ 同步 API facade（[`LocalCoreClient`]）+
//! 全部活对话 session（[`RuntimeRegistry`]），对外开 transport 让 desktop / heb / hebweb
//! 作为客户端连入。
//!
//! - **单例锁**：`~/.hebbian/hebcore.lock` 排他锁，同一数据目录最多一个 hebcore 实例
//!   （对标 hebisland daemon「能否拿锁判存活」范式，§7.8.1）。
//! - **unix-socket transport**：`~/.hebbian/hebcore.sock`，每连接逐行 JSON。
//!   - 同步 API（`Rpc`）→ [`core_rpc::dispatch`]，回一行响应。
//!   - 对话主链路：`StartRun` 投进 session 输入循环（异步跑），`Subscribe` 把本连接转为
//!     事件流逐 [`protocol::WireEvent`] 推（§7.8.5 单写者 + 多观察者 broadcast）。
//!
//! ws transport（浏览器 / 远程）在步骤⑤接入。

use std::path::PathBuf;
use std::sync::Arc;

use agent_core::core_client::LocalCoreClient;
use agent_core::permissions::PermissionStore;
use anyhow::{Context, Result};
use clap::Parser;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use surface_session::RuntimeRegistry;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "hebcore", about = "Hebbian 常驻核心进程（§7.8）", version)]
struct Args {
    /// 数据目录（默认 ~/.hebbian）
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

/// hebcore 进程的共享状态：同步 API facade + 活 session 枢纽。
struct CoreState {
    data_dir: PathBuf,
    core: Arc<LocalCoreClient>,
    permission_store: Option<Arc<PermissionStore>>,
    /// 活对话 session 表（§7.8.5 单写者 + 多观察者）。
    runtimes: RuntimeRegistry,
}

/// hebcore 的 unix-socket 入站消息（一行一个 JSON）。同步 API 走 `Rpc`，对话主链路走
/// `StartRun` / `Subscribe`（架构 §3 双通路）。
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HebcoreRequest {
    /// 同步 API：内嵌一个 [`core_rpc::CoreRequest`]，走 dispatch。
    Rpc { req: core_rpc::CoreRequest },
    /// 启动一个对话 turn：把 user 文本投进 session 输入循环（异步跑，事件走 broadcast）。
    StartRun { session_id: String, text: String },
    /// 订阅一个 session 的事件流：本连接转为只读事件流，持续推 [`protocol::WireEvent`]。
    Subscribe { session_id: String },
}

/// hebcore 的出站消息（一行一个 JSON）。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HebcoreResponse<'a> {
    /// 同步 API 的响应。
    Rpc { resp: &'a core_rpc::CoreResponse },
    /// StartRun 已受理（异步跑，事件后续走 broadcast）。
    Accepted,
    /// 订阅已建立。
    Subscribed { session_id: String },
    /// broadcast 推来的一个对话事件。
    Event { event: protocol::WireEvent },
    /// 出错。
    Error { message: String },
}

fn data_dir(args: &Args) -> PathBuf {
    args.data_dir.clone().unwrap_or_else(|| {
        dirs::home_dir()
            .expect("no home dir")
            .join(".hebbian")
    })
}

/// 单例锁：排他锁住 `<data_dir>/hebcore.lock`，进程存活期间持有。拿不到 = 已有实例在跑。
/// 返回的 `File` 必须保活（drop 即释放锁），故由 main 持有到退出。
fn acquire_singleton_lock(data_dir: &std::path::Path) -> Result<std::fs::File> {
    let lock_path = data_dir.join("hebcore.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("打开单例锁文件 {lock_path:?}"))?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("已有 hebcore 实例在运行（{lock_path:?} 锁被占用）"))?;
    Ok(file)
}

fn sock_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("hebcore.sock")
}

#[tokio::main]
async fn main() -> Result<()> {
    observability::init("info");
    let args = Args::parse();
    let data_dir = data_dir(&args);
    std::fs::create_dir_all(&data_dir).ok();

    // 单例：拿不到锁就退出（已有实例）。_lock 持有到 main 结束。
    let _lock = acquire_singleton_lock(&data_dir)?;

    let permission_store = PermissionStore::open(&data_dir).ok().map(Arc::new);
    let core = Arc::new(LocalCoreClient::new(
        None,
        data_dir.clone(),
        permission_store.clone(),
    ));
    let state = Arc::new(CoreState {
        data_dir: data_dir.clone(),
        core,
        permission_store,
        runtimes: RuntimeRegistry::new(),
    });

    // unix-socket transport：清理旧 sock（上次异常退出残留），bind 新的。
    let sock = sock_path(&data_dir);
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)
        .with_context(|| format!("bind hebcore socket {sock:?}"))?;
    info!(socket = %sock.display(), data_dir = %data_dir.display(), "hebcore listening");

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, state).await {
                        warn!(error = %e, "hebcore connection 处理失败");
                    }
                });
            }
            Err(e) => {
                error!(error = %e, "hebcore accept 失败");
            }
        }
    }
}

/// 单连接：逐行读 JSON [`HebcoreRequest`]。
/// - `Rpc` → dispatch，回一行 `HebcoreResponse::Rpc`。
/// - `StartRun` → 投进 session 输入循环，回 `Accepted`（事件异步走 broadcast）。
/// - `Subscribe` → 本连接转为事件流，持续推 `Event`，直到连接断开。
async fn handle_connection(stream: UnixStream, state: Arc<CoreState>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<HebcoreRequest>(line) {
            Ok(HebcoreRequest::Rpc { req }) => {
                let resp = core_rpc::dispatch(req, &*state.core).await;
                write_line(&mut write_half, &HebcoreResponse::Rpc { resp: &resp }).await?;
            }
            Ok(HebcoreRequest::StartRun { session_id, text }) => {
                let resp = match state
                    .runtimes
                    .ensure(&state.data_dir, state.permission_store.clone(), &session_id)
                    .await
                {
                    Ok(rt) => match rt.input_tx.send(text) {
                        Ok(()) => HebcoreResponse::Accepted,
                        Err(_) => HebcoreResponse::Error {
                            message: "session 输入循环已关闭".into(),
                        },
                    },
                    Err(e) => HebcoreResponse::Error {
                        message: e.to_string(),
                    },
                };
                write_line(&mut write_half, &resp).await?;
            }
            Ok(HebcoreRequest::Subscribe { session_id }) => {
                match state
                    .runtimes
                    .ensure(&state.data_dir, state.permission_store.clone(), &session_id)
                    .await
                {
                    Ok(rt) => {
                        write_line(
                            &mut write_half,
                            &HebcoreResponse::Subscribed {
                                session_id: session_id.clone(),
                            },
                        )
                        .await?;
                        // 本连接转为事件流：逐 WireEvent 推到客户端，直到断开 / 通道关闭。
                        let mut rx = rt.state.subscribe();
                        loop {
                            match rx.recv().await {
                                Ok(event) => {
                                    if write_line(
                                        &mut write_half,
                                        &HebcoreResponse::Event { event },
                                    )
                                    .await
                                    .is_err()
                                    {
                                        return Ok(());
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(_) => break,
                            }
                        }
                    }
                    Err(e) => {
                        write_line(
                            &mut write_half,
                            &HebcoreResponse::Error {
                                message: e.to_string(),
                            },
                        )
                        .await?;
                    }
                }
            }
            Err(e) => {
                write_line(
                    &mut write_half,
                    &HebcoreResponse::Error {
                        message: format!("解析请求失败: {e}"),
                    },
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// 写一行 JSON（带换行）。客户端断开时返回 Err，调用方据此结束订阅循环。
async fn write_line(
    w: &mut (impl AsyncWriteExt + Unpin),
    msg: &HebcoreResponse<'_>,
) -> Result<()> {
    let mut out = serde_json::to_string(msg)
        .unwrap_or_else(|e| format!("{{\"kind\":\"error\",\"message\":\"序列化失败: {e}\"}}"));
    out.push('\n');
    w.write_all(out.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}
