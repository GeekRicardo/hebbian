//! hebcore — 常驻核心进程（架构 §7.8.1）。
//!
//! 持有唯一 dispatch（[`core_rpc::dispatch`]）+ 同步 API facade（`LocalCoreClient`）+
//! 全部活对话 session（`RuntimeRegistry`），对外开 unix-socket transport 让 desktop / heb /
//! hebweb 作为客户端连入。连接处理逻辑在 [`surface_session::transport`]（hebcore 进程与
//! hebweb 升格时共用同一份，消除重复）。
//!
//! - **单例锁**：`~/.hebbian/hebcore.lock` 排他锁，同一数据目录最多一个 hebcore 实例
//!   （对标 hebisland daemon「能否拿锁判存活」范式，§7.8.1）。
//! - **unix-socket transport**：`~/.hebbian/hebcore.sock`，每连接逐行 JSON
//!   （Rpc / StartRun / Subscribe / Approve / Answer / Interrupt / Inject / SetRunMode）。
//!
//! ws transport（浏览器 / 远程）在步骤⑤接入。

use std::path::PathBuf;
use std::sync::Arc;

use agent_core::core_client::LocalCoreClient;
use agent_core::permissions::PermissionStore;
use anyhow::{Context, Result};
use clap::Parser;
use fs2::FileExt;
use surface_session::transport::{handle_connection, TransportCtx};
use surface_session::RuntimeRegistry;
use tokio::net::UnixListener;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "hebcore", about = "Hebbian 常驻核心进程（§7.8）", version)]
struct Args {
    /// 数据目录（默认 ~/.hebbian）
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

fn resolve_data_dir(args: &Args) -> PathBuf {
    args.data_dir
        .clone()
        .unwrap_or_else(|| dirs::home_dir().expect("no home dir").join(".hebbian"))
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
    let data_dir = resolve_data_dir(&args);
    std::fs::create_dir_all(&data_dir).ok();

    // 单例：拿不到锁就退出（已有实例）。_lock 持有到 main 结束。
    let _lock = acquire_singleton_lock(&data_dir)?;

    let permission_store = PermissionStore::open(&data_dir).ok().map(Arc::new);
    let core = Arc::new(LocalCoreClient::new(
        None,
        data_dir.clone(),
        permission_store.clone(),
    ));
    let ctx = Arc::new(TransportCtx {
        data_dir: data_dir.clone(),
        core,
        permission_store,
        runtimes: RuntimeRegistry::new(),
    });

    // wakeup resume handler 必须在 run 所在进程（= hebcore）注册：后台任务 / cron 唤醒
    // 在本进程的 WakeupScheduler 触发，没有 handler 会被丢弃、挂起 run 永不 resume（§4.12.5）。
    surface_session::register_wakeup_resume_handler(ctx.clone());

    // unix-socket transport：清理旧 sock（上次异常退出残留），bind 新的。
    let sock = sock_path(&data_dir);
    let _ = std::fs::remove_file(&sock);
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("bind hebcore socket {sock:?}"))?;
    info!(socket = %sock.display(), data_dir = %data_dir.display(), "hebcore listening");

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, ctx).await {
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
