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
use surface_session::transport::{handle_connection, SurfaceLifecycle, TransportCtx};
use surface_session::RuntimeRegistry;
use tokio::net::UnixListener;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "hebcore", about = "Hebbian 常驻核心进程（§7.8）", version)]
struct Args {
    /// 数据目录（默认 ~/.hebbian）
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// unix-socket 路径（§7.8.8 per-app sock）。desktop 按 app 安装位置派生后用
    /// `--sock-path` 传入，让不同 build 各自 sock、可并存；单例锁落在同名 `.lock`。
    /// 不传则回落 `<data_dir>/hebcore.sock`（向后兼容 / 独立启动）。
    #[arg(long)]
    sock_path: Option<PathBuf>,
}

fn resolve_data_dir(args: &Args) -> PathBuf {
    args.data_dir
        .clone()
        .unwrap_or_else(|| dirs::home_dir().expect("no home dir").join(".hebbian"))
}

/// 单例锁：排他锁住给定 `.lock` 文件，进程存活期间持有。拿不到 = 已有实例在跑。
/// 返回的 `File` 必须保活（drop 即释放锁），故由 main 持有到退出。
fn acquire_singleton_lock(lock_path: &std::path::Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("打开单例锁文件 {lock_path:?}"))?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("已有 hebcore 实例在运行（{lock_path:?} 锁被占用）"))?;
    Ok(file)
}

/// sock 路径：优先 `--sock-path`（per-app，§7.8.8），否则 `<data_dir>/hebcore.sock`。
fn sock_path(args: &Args, data_dir: &std::path::Path) -> PathBuf {
    args.sock_path
        .clone()
        .unwrap_or_else(|| data_dir.join("hebcore.sock"))
}

#[tokio::main]
async fn main() -> Result<()> {
    observability::init("info");
    let args = Args::parse();
    let data_dir = resolve_data_dir(&args);
    std::fs::create_dir_all(&data_dir).ok();

    // sock + 单例锁按 per-app sock 路径走（§7.8.8）：sock 的父目录（如 <data_dir>/run）
    // 可能还不存在，先建好。单例锁落在 sock 同名 `.lock`——不同 build 各自 sock/lock，
    // 可并存（数据目录仍共享）。
    let sock = sock_path(&args, &data_dir);
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let lock_path = sock.with_extension("lock");

    // 单例：拿不到锁就退出（已有同一 sock 的实例）。_lock 持有到 main 结束。
    let _lock = acquire_singleton_lock(&lock_path)?;

    // 启动时扫描所有 session，把上次进程崩溃残留的 dead partial 折叠进 session.jsonl。
    // 不依赖用户打开对应 session 才触发恢复（架构 §4.9.3 / §7.8.5）。
    agent_core::storage::sessions::recover_all_dead_partials(&data_dir);

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
        // 版本号在 hebcore bin 编译期由 build.rs 固化（§7.8.7）；不在 surface-session lib 里
        // env! —— lib 产物会被缓存，env! 会读到旧值。
        build_version: env!("HEBBIAN_BUILD_VERSION").to_string(),
        bin_name: "hebcore".to_string(),
    });

    // wakeup resume handler 必须在 run 所在进程（= hebcore）注册：后台任务 / cron 唤醒
    // 在本进程的 WakeupScheduler 触发，没有 handler 会被丢弃、挂起 run 永不 resume（§4.12.5）。
    surface_session::register_wakeup_resume_handler(ctx.clone());

    // unix-socket transport：清理旧 sock（上次异常退出残留），bind 新的。
    let _ = std::fs::remove_file(&sock);
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("bind hebcore socket {sock:?}"))?;
    info!(socket = %sock.display(), data_dir = %data_dir.display(), "hebcore listening");

    let lifecycle = SurfaceLifecycle::default();
    let mut exit_rx = lifecycle.subscribe_exit();

    loop {
        tokio::select! {
            changed = exit_rx.changed() => {
                if changed.is_ok() && *exit_rx.borrow() {
                    info!("最后一个 surface 连接已断开，按用户主动退出语义关闭 hebcore");
                    ctx.runtimes.cancel_active_runs_and_wait().await;
                    break;
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let ctx = ctx.clone();
                        let connection = lifecycle.attach().await;
                        tokio::spawn(async move {
                            let _connection = connection;
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
    }

    Ok(())
}
