//! hebweb — 浏览器 surface 的 HTTP + WebSocket server。
//!
//! # 启动
//!
//! ```bash
//! hebweb                                    # 默认 127.0.0.1:3030，data_dir ~/.hebbian
//! hebweb --port 4040                        # 自定义端口
//! hebweb --static-dir apps/desktop/dist  # 指定前端打包产物
//! hebweb --data-dir /tmp/hebbian-test       # 隔离的数据目录（多 AI 测试用）
//! ```
//!
//! # 一个 hebweb 进程 = 一组 session 的服务端
//!
//! 多个浏览器 / Playwright 各自打开 WS 连接，subscribe 到不同 session_id 即可看到
//! 各自的事件流（按 session 路由）。同一进程内多个 session 各自独立，互不阻塞。
//! 多 AI 调试推荐每人自己开 `hebweb --port <random>`——进程间通过 `~/.hebbian/`
//! 文件锁保证写安全。

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing::{info, warn};

mod chat_helpers;
mod protocol;
mod server;

#[derive(Parser, Debug)]
#[command(
    name = "hebweb",
    about = "Hebbian web surface（HTTP + WebSocket）",
    version
)]
struct Args {
    /// 监听地址（默认 127.0.0.1:3030）
    #[arg(long, default_value = "127.0.0.1:3030")]
    addr: String,

    /// 监听端口（覆盖 --addr 的端口）
    #[arg(long, short = 'p')]
    port: Option<u16>,

    /// 数据目录（默认 ~/.hebbian）
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// 前端静态文件目录（默认自动探测 apps/desktop/dist）
    #[arg(long)]
    static_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    observability::init("info");

    let args = Args::parse();
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(agent_core::storage::default_data_dir);
    std::fs::create_dir_all(&data_dir)?;

    let static_dir = args.static_dir.clone().or_else(autodetect_static_dir);

    let state = server::ServerState::new(data_dir.clone());

    // hebweb 升格为 hebcore（架构 §7.8.6 步骤⑤）：除浏览器 ws/HTTP 外，额外开 hebcore
    // unix-socket transport，让 desktop / heb 作为客户端连入、看同一份活对话状态。用 hebcore
    // 单例锁守 sock——拿到锁 = 本进程当 hebcore；拿不到（已有独立 hebcore 在跑）则跳过，
    // 只服务浏览器。锁 + listener 句柄 spawn 到后台常驻。
    spawn_hebcore_transport(&data_dir, &state);

    let app = server::build_router(state, static_dir.clone());

    // 解析监听地址
    let mut addr: SocketAddr = args.addr.parse()?;
    if let Some(p) = args.port {
        addr.set_port(p);
    }

    info!(
        addr = %addr,
        data_dir = %data_dir.display(),
        static_dir = ?static_dir,
        "hebweb starting"
    );
    eprintln!(
        "hebweb listening on http://{addr}  (data_dir={})",
        data_dir.display()
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// 尝试以 hebcore 身份开 unix-socket transport（§7.8.6 步骤⑤）。拿到单例锁则 bind
/// `<data_dir>/hebcore.sock`、spawn accept 循环；拿不到锁（已有独立 hebcore）则静默跳过。
fn spawn_hebcore_transport(data_dir: &std::path::Path, state: &server::ServerState) {
    use fs2::FileExt;

    let lock_path = data_dir.join("hebcore.lock");
    let lock = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, "打开 hebcore 单例锁失败，跳过 unix-socket transport");
            return;
        }
    };
    if lock.try_lock_exclusive().is_err() {
        info!("已有 hebcore 实例占用单例锁，hebweb 只服务浏览器（不开 unix-socket）");
        return;
    }

    let sock = data_dir.join("hebcore.sock");
    let _ = std::fs::remove_file(&sock);
    let ctx = std::sync::Arc::new(surface_session::transport::TransportCtx {
        data_dir: data_dir.to_path_buf(),
        core: state.core.clone(),
        permission_store: state.permission_store.clone(),
        runtimes: state.runtimes.clone(),
        // §7.8.7：bin_name="hebweb" 让 desktop 识别"运行中核心是 hebweb 兼任"，不误杀。
        build_version: env!("HEBBIAN_BUILD_VERSION").to_string(),
        bin_name: "hebweb".to_string(),
    });
    surface_session::register_wakeup_resume_handler(ctx.clone());
    let sock_for_log = sock.clone();
    tokio::spawn(async move {
        // 锁句柄 move 进 task 常驻持有（task 活着 = 锁不释放）。
        let _lock = lock;
        let listener = match tokio::net::UnixListener::bind(&sock) {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "bind hebcore.sock 失败，unix-socket transport 未启动");
                return;
            }
        };
        info!(socket = %sock_for_log.display(), "hebweb 兼任 hebcore：unix-socket transport 就绪");
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let ctx = ctx.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            surface_session::transport::handle_connection(stream, ctx).await
                        {
                            warn!(error = %e, "hebcore connection 处理失败");
                        }
                    });
                }
                Err(e) => warn!(error = %e, "hebcore accept 失败"),
            }
        }
    });
}

/// 探测前端 dist 目录。优先 cwd 下 `apps/desktop/dist`，
/// 找不到时返回 None（用户用 vite dev server 自行访问）。
fn autodetect_static_dir() -> Option<PathBuf> {
    let candidates = [
        "apps/desktop/dist",
        "../apps/desktop/dist",
        "../../apps/desktop/dist",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .or_else(|| {
            // 同时尝试 CARGO_MANIFEST_DIR 相邻
            let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            here.join("../desktop/dist")
                .canonicalize()
                .ok()
                .filter(|p| p.exists())
        })
}
