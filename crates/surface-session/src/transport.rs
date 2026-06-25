//! hebcore unix-socket transport（架构 §7.8.1 / §7.8.6 步骤③）。
//!
//! 把"连接处理"从 hebcore 进程提取为通用件：hebcore 二进制与 hebweb（升格为 hebcore 时）
//! 都用同一份 transport handler，避免重复。每连接逐行 JSON：
//! - `Rpc` → [`core_rpc::dispatch`]（同步 API）
//! - `StartRun` / `Inject` → session 输入循环
//! - `Subscribe` → 本连接转事件流逐 [`protocol::WireEvent`] 推
//! - `Approve` / `Answer` / `Interrupt` / `SetRunMode` → 运行时控制 Op 路由（§7.8.5）

use std::path::PathBuf;
use std::sync::Arc;

use agent_core::core_client::LocalCoreClient;
use agent_core::permissions::PermissionStore;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::RuntimeRegistry;

/// transport handler 的依赖：同步 API facade + 活 session 表 + 数据目录 + 权限库。
/// hebcore 进程 / hebweb 升格时各自构造一份，共享同一处理逻辑。
#[derive(Clone)]
pub struct TransportCtx {
    pub data_dir: PathBuf,
    pub core: Arc<LocalCoreClient>,
    pub permission_store: Option<Arc<PermissionStore>>,
    pub runtimes: RuntimeRegistry,
}

/// hebcore unix-socket 入站消息（一行一个 JSON）。
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HebcoreRequest {
    /// 同步 API：内嵌一个 [`core_rpc::CoreRequest`]，走 dispatch。
    Rpc { req: core_rpc::CoreRequest },
    /// 启动一个对话 turn：把 user 文本投进 session 输入循环（异步跑，事件走 broadcast）。
    StartRun { session_id: String, text: String },
    /// 订阅一个 session 的事件流：本连接转为只读事件流，持续推 [`protocol::WireEvent`]。
    Subscribe { session_id: String },
    /// 结算一条审批（HITL）。
    Approve {
        session_id: String,
        request_id: String,
        decision: protocol::ApprovalDecision,
    },
    /// 结算一条提问（HITL）。
    Answer {
        session_id: String,
        request_id: String,
        answer: protocol::UserAnswer,
    },
    /// 中断当前 run。
    Interrupt { session_id: String },
    /// 把一条 user 输入插进当前活 run 的队列。
    Inject { session_id: String, text: String },
    /// 即时切换 run mode。
    SetRunMode { session_id: String, mode: String },
}

/// hebcore unix-socket 出站消息（一行一个 JSON）。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HebcoreResponse<'a> {
    Rpc { resp: &'a core_rpc::CoreResponse },
    Accepted,
    Subscribed { session_id: String },
    Event { event: protocol::WireEvent },
    Error { message: String },
}

/// 处理一条 unix-socket 连接：逐行读 [`HebcoreRequest`]，按通路分派。
pub async fn handle_connection(stream: UnixStream, ctx: Arc<TransportCtx>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<HebcoreRequest>(line) {
            Ok(HebcoreRequest::Rpc { req }) => {
                let resp = core_rpc::dispatch(req, &*ctx.core).await;
                write_line(&mut write_half, &HebcoreResponse::Rpc { resp: &resp }).await?;
            }
            Ok(HebcoreRequest::StartRun { session_id, text }) => {
                let resp = match ctx
                    .runtimes
                    .ensure(&ctx.data_dir, ctx.permission_store.clone(), &session_id)
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
            Ok(HebcoreRequest::Approve {
                session_id,
                request_id,
                decision,
            }) => {
                let resp = match ctx.runtimes.get(&session_id).await {
                    Some(rt) if rt.state.resolve_approval(&request_id, decision) => {
                        HebcoreResponse::Accepted
                    }
                    Some(_) => HebcoreResponse::Error {
                        message: format!("未找到待结算审批 {request_id}"),
                    },
                    None => HebcoreResponse::Error {
                        message: format!("session {session_id} 未激活"),
                    },
                };
                write_line(&mut write_half, &resp).await?;
            }
            Ok(HebcoreRequest::Answer {
                session_id,
                request_id,
                answer,
            }) => {
                let resp = match ctx.runtimes.get(&session_id).await {
                    Some(rt) if rt.state.answer_question(&request_id, answer) => {
                        HebcoreResponse::Accepted
                    }
                    Some(_) => HebcoreResponse::Error {
                        message: format!("未找到待结算提问 {request_id}"),
                    },
                    None => HebcoreResponse::Error {
                        message: format!("session {session_id} 未激活"),
                    },
                };
                write_line(&mut write_half, &resp).await?;
            }
            Ok(HebcoreRequest::Interrupt { session_id }) => {
                let resp = match ctx.runtimes.get(&session_id).await {
                    Some(rt) => {
                        rt.stop();
                        HebcoreResponse::Accepted
                    }
                    None => HebcoreResponse::Error {
                        message: format!("session {session_id} 未激活"),
                    },
                };
                write_line(&mut write_half, &resp).await?;
            }
            Ok(HebcoreRequest::Inject { session_id, text }) => {
                let resp = match ctx.runtimes.get(&session_id).await {
                    Some(rt) if rt.inject(text) => HebcoreResponse::Accepted,
                    Some(_) => HebcoreResponse::Error {
                        message: "无活跃 run，无法注入".into(),
                    },
                    None => HebcoreResponse::Error {
                        message: format!("session {session_id} 未激活"),
                    },
                };
                write_line(&mut write_half, &resp).await?;
            }
            Ok(HebcoreRequest::SetRunMode { session_id, mode }) => {
                let resp = match agent_core::run_mode::RunMode::parse(&mode) {
                    Some(m) => match ctx.runtimes.get(&session_id).await {
                        Some(rt) => {
                            rt.state.set_run_mode(m);
                            agent_core::run_mode::LiveRunModeRegistry::global().set(&session_id, m);
                            HebcoreResponse::Accepted
                        }
                        None => HebcoreResponse::Error {
                            message: format!("session {session_id} 未激活"),
                        },
                    },
                    None => HebcoreResponse::Error {
                        message: format!("非法 run mode: {mode}"),
                    },
                };
                write_line(&mut write_half, &resp).await?;
            }
            Ok(HebcoreRequest::Subscribe { session_id }) => {
                match ctx
                    .runtimes
                    .ensure(&ctx.data_dir, ctx.permission_store.clone(), &session_id)
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
                        let mut rx = rt.state.subscribe();
                        loop {
                            match rx.recv().await {
                                Ok(event) => {
                                    if write_line(&mut write_half, &HebcoreResponse::Event { event })
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
