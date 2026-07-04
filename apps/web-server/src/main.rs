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
use tracing::info;

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
    surface_session::register_wakeup_resume_handler(
        data_dir.clone(),
        state.permission_store.clone(),
        state.runtimes.clone(),
    );

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
