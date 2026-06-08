use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

mod bridge;

#[derive(Parser)]
#[command(name = "heb-channel", about = "Hebbian 渠道网关")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// 数据目录（默认 ~/.hebbian）
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// 微信：扫码登录
    #[command(name = "wechat-login")]
    WeChatLogin,

    /// 微信：启动网关（需已登录）
    #[command(name = "wechat")]
    WeChatRun {
        /// bot_id（登录后显示，也在 ~/.hebbian/channels/wechat/ 下的目录名）
        #[arg(long)]
        bot_id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    observability::init("info");
    let cli = Cli::parse();
    let data_dir = cli
        .data_dir
        .unwrap_or_else(agent_core::storage::default_data_dir);

    match cli.command {
        Commands::WeChatLogin => {
            let credentials = channels::wechat::login::login().await?;
            channels::wechat::login::save_credentials(&data_dir, &credentials)?;
            eprintln!("凭证已保存到 ~/.hebbian/channels/wechat/{}/", credentials.bot_id);
            eprintln!("启动网关：heb-channel wechat --bot-id {}", credentials.bot_id);
            Ok(())
        }
        Commands::WeChatRun { bot_id } => {
            let credentials = channels::wechat::login::load_credentials(&data_dir, &bot_id)?;
            let channel = Arc::new(channels::wechat::channel::WeChatChannel::new(
                credentials.bot_token,
                credentials.bot_id.clone(),
                &data_dir,
            ));
            let mut state = channel_core::owner_state::OwnerState::load(
                &data_dir,
                "wechat",
                &credentials.bot_id,
            );
            let bridge = bridge::ChannelBridge::new(data_dir.clone());
            bridge.run_loop(channel, &mut state, &credentials.bot_id).await
        }
    }
}
