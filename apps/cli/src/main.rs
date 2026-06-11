//! heb — Hebbian agent CLI surface（daemon 模式）
//!
//! # 用法
//!
//! ```bash
//! # 终端 A：启动 daemon，持续输出 NDJSON 事件流
//! heb new --provider anthropic --model claude-opus-4-7
//! # → {"event":"started","session_id":"20260520T1234-abc"}
//!
//! # 终端 B：与 daemon 交互
//! heb input  20260520T1234-abc "帮我写一个 Rust 排序算法"
//! heb allow  20260520T1234-abc <request_id>           # 批准一次
//! heb allow  20260520T1234-abc <request_id> session   # 会话级记住
//! heb allow  20260520T1234-abc <request_id> global    # 全局记住
//! heb deny   20260520T1234-abc <request_id>
//! heb answer 20260520T1234-abc <request_id> "Yes"
//! heb stop   20260520T1234-abc
//! heb mode   20260520T1234-abc auto-mode
//! heb ping   20260520T1234-abc
//! heb list-sessions                                  # 列出本机所有存活 daemon
//! ```

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod client;
mod daemon;
mod ipc;

use ipc::IpcCommand;

#[derive(Parser)]
#[command(
    name = "heb",
    about = "Hebbian agent CLI surface — daemon 模式",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 启动 daemon：创建 session 并持续输出 NDJSON 事件到 stdout
    New {
        /// 连接已有 session（不填则新建）
        #[arg(long)]
        session_id: Option<String>,

        /// provider id 或 name，支持 name/model_id 格式
        #[arg(long)]
        provider: Option<String>,

        /// model id（也可写在 --provider name/model_id 里）
        #[arg(long, short = 'm')]
        model: Option<String>,

        /// 工作目录（默认由 session 或全局设置决定）
        #[arg(long)]
        workdir: Option<PathBuf>,

        /// 运行模式：default | plan-mode | auto-mode
        #[arg(long = "mode", default_value = "default")]
        run_mode: String,

        /// 数据目录（默认 ~/.hebbian）
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },

    /// 发送用户输入（有活跃 run 时自动注入，无则开新 run）
    Input { session_id: String, text: String },

    /// 批准权限审批
    Allow {
        session_id: String,
        request_id: String,
        /// 记住范围：once（默认）| session | project | global
        #[arg(default_value = "once")]
        scope: String,
        /// 命令前缀（Bash 命令级记忆），如 `--pattern "git status"` 或 `--pattern git`；
        /// scope != "once" 时生效
        #[arg(long)]
        pattern: Option<String>,
        /// compound 命令的额外段前缀，可多次给：
        /// `--pattern cd --extra-pattern touch --extra-pattern ls` 一次允许 cd && touch && ls
        #[arg(long = "extra-pattern")]
        extra_patterns: Vec<String>,
    },

    /// 拒绝权限审批
    Deny {
        session_id: String,
        request_id: String,
    },

    /// 拒绝并注入反馈文本
    DenyFeedback {
        session_id: String,
        request_id: String,
        feedback: String,
    },

    /// 回答 agent 提问
    Answer {
        session_id: String,
        request_id: String,
        /// 选项 label（--custom 时为自由文本）
        value: String,
        /// 自由输入而非选项
        #[arg(long)]
        custom: bool,
        /// 取消提问
        #[arg(long)]
        cancel: bool,
    },

    /// 停止当前 run
    Stop { session_id: String },

    /// 切换 run mode
    Mode {
        session_id: String,
        /// default | plan-mode | auto-mode
        mode: String,
    },

    /// 检测 daemon 是否存活
    Ping { session_id: String },

    /// 列出本机所有存活的 heb daemon（按 ~/.hebbian/cli-sockets/ 扫描 + ping 测活，自动清理死 socket）
    ListSessions,

    /// 拉当前 session 已记录的所有 model 请求/响应（每个 turn 一条）
    /// → 输出 `{ entries: [DumpEntry, ...] }`，给 AI 脚本排查"模型到底收到了什么"
    ModelIo { session_id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    // memory=info / permission=info：记忆系统（target="memory"，[Memory] 前缀）与权限链路
    // （target="permission"，[Permission:*] / [AutoMode] 前缀）的动作日志默认放行到 info，
    // 让「解析/匹配/审批/记忆/判官」始终可见且可一键 grep，又不抬高全局噪声。
    // RUST_LOG 设置时以其为准。
    observability::init("warn,memory=info,permission=info,cache=info");
    let cli = Cli::parse();

    match cli.command {
        Command::New {
            session_id,
            provider,
            model,
            workdir,
            run_mode,
            data_dir,
        } => {
            daemon::run(daemon::DaemonArgs {
                session_id,
                provider,
                model,
                workdir,
                run_mode,
                data_dir,
            })
            .await
        }

        Command::Input { session_id, text } => {
            client::send_command(&session_id, IpcCommand::Send { text }).await
        }

        Command::Allow {
            session_id,
            request_id,
            scope,
            pattern,
            extra_patterns,
        } => {
            client::send_command(
                &session_id,
                IpcCommand::Allow {
                    request_id,
                    scope,
                    pattern,
                    extra_patterns,
                },
            )
            .await
        }

        Command::Deny {
            session_id,
            request_id,
        } => client::send_command(&session_id, IpcCommand::Deny { request_id }).await,

        Command::DenyFeedback {
            session_id,
            request_id,
            feedback,
        } => {
            client::send_command(
                &session_id,
                IpcCommand::DenyWithFeedback {
                    request_id,
                    feedback,
                },
            )
            .await
        }

        Command::Answer {
            session_id,
            request_id,
            value,
            custom,
            cancel,
        } => {
            let (kind, value) = if cancel {
                ("cancelled".into(), String::new())
            } else if custom {
                ("custom".into(), value)
            } else {
                ("selected".into(), value)
            };
            client::send_command(
                &session_id,
                IpcCommand::Answer {
                    request_id,
                    kind,
                    value,
                },
            )
            .await
        }

        Command::Stop { session_id } => client::send_command(&session_id, IpcCommand::Stop).await,

        Command::Mode { session_id, mode } => {
            client::send_command(&session_id, IpcCommand::Mode { mode }).await
        }

        Command::Ping { session_id } => client::send_command(&session_id, IpcCommand::Ping).await,

        Command::ListSessions => client::list_sessions().await,

        Command::ModelIo { session_id } => {
            client::send_command(&session_id, IpcCommand::ListModelIo).await
        }
    }
}
