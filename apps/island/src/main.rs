// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hebisland", about = "无边框通知 / 审批浮窗")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动通知守护进程
    Daemon,
    /// 发送一条通知到正在运行的守护进程
    Notify {
        /// JSON 格式的 SocketMessage
        #[arg(long)]
        msg: String,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Daemon => {
            hebisland_lib::run();
        }
        Commands::Notify { msg } => {
            notify_sync(&msg);
        }
    }
}

fn notify_sync(msg: &str) {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let sock_path = dirs::home_dir()
        .expect("无法获取 home 目录")
        .join(".hebbian")
        .join("island.sock");

    match UnixStream::connect(&sock_path) {
        Ok(mut stream) => {
            stream
                .write_all(msg.as_bytes())
                .expect("写入 socket 失败");
            stream.write_all(b"\n").expect("写入换行失败");
            println!("ok");
        }
        Err(e) => {
            eprintln!("无法连接到 hebisland daemon: {e}");
            eprintln!("请先运行: hebisland daemon");
            std::process::exit(1);
        }
    }
}