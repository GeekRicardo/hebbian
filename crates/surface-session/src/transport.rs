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
    /// 本进程 binary 的版本号字符串（§7.8.7 版本协商）。**由各 bin 的 main 用
    /// `env!("HEBBIAN_BUILD_VERSION")` 传入**，不能在本 lib 里 `env!`——lib 编译产物会被
    /// 缓存（bin 重编了 lib 未必重编），lib 里的 `env!` 会固化成旧值。
    pub build_version: String,
    /// 本进程 binary 名（`"hebcore"` / `"hebweb"`）。desktop 据此识别"运行中的核心是不是
    /// hebweb 兼任"，避免把正常的 hebweb 当 stale hebcore 误杀。
    pub bin_name: String,
}

/// hebcore unix-socket 入站消息（一行一个 JSON）。
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HebcoreRequest {
    /// 同步 API：内嵌一个 [`core_rpc::CoreRequest`]，走 dispatch。
    Rpc { req: core_rpc::CoreRequest },
    /// 启动一个对话 turn：把 user 文本投进 session 输入循环（异步跑，事件走 broadcast）。
    StartRun {
        session_id: String,
        text: String,
        #[serde(default)]
        attachments: Vec<common::attachments::MessageAttachment>,
    },
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
    Inject {
        session_id: String,
        text: String,
        #[serde(default)]
        attachments: Vec<common::attachments::MessageAttachment>,
    },
    /// 即时切换 run mode。
    SetRunMode { session_id: String, mode: String },
    /// 报告本 hebcore 进程的版本身份（desktop connect 后做版本协商，§7.8.7）。
    /// 连接级请求：要读 ctx.build_version + ctx.runtimes 判活跃 run，dispatch 拿不到这俩，
    /// 故走 HebcoreRequest 而非 core_rpc::CoreRequest。
    GetVersion,
    /// 优雅关停本进程（desktop 检测磁盘 binary 已更新后换版）。有活跃 run 时拒绝——
    /// 避免杀掉正在写 partial 的 run（§4.9.2）。
    Shutdown,
    /// 订阅本 hebcore 进程的全局日志流（§4.10）：run 移 hebcore 后 agent_loop 的日志都在
    /// 本进程，surface 连过来订阅、把每条 LogLine 注入自己的 LOG_TX 喂日志面板。连接级、
    /// 跨 session（不带 session_id）。
    SubscribeLogs,
}

/// hebcore unix-socket 出站消息（一行一个 JSON）。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HebcoreResponse<'a> {
    Rpc {
        resp: &'a core_rpc::CoreResponse,
    },
    Accepted,
    Subscribed {
        session_id: String,
    },
    Event {
        event: protocol::WireEvent,
    },
    Error {
        message: String,
    },
    /// GetVersion 应答（§7.8.7）。`build_version` 跨进程比对（同次 build 的两 binary 注入
    /// 相同 `HEBBIAN_BUILD_VERSION`，字符串相等 = 同版本）；`bin_name` 区分 hebcore /
    /// hebweb 兼任（不误杀 hebweb）；`has_active_run` 门控 Shutdown。
    Version {
        build_version: String,
        bin_name: String,
        pid: u32,
        has_active_run: bool,
    },
    /// 一条转发给 surface 的日志行（应 [`HebcoreRequest::SubscribeLogs`]，§4.10）。
    Log {
        line: observability::LogLine,
    },
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
            Ok(HebcoreRequest::StartRun {
                session_id,
                text,
                attachments,
            }) => {
                let resp = match ctx
                    .runtimes
                    .ensure(&ctx.data_dir, ctx.permission_store.clone(), &session_id)
                    .await
                {
                    Ok(rt) => match rt.input_tx.send(crate::TurnInput::new(text, attachments)) {
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
                // 时序容忍（架构 §7.8.6）：客户端收到 PermissionRequested 事件后可能比
                // agent_loop 注册 gate 更快发来 Approve（事件 broadcast 与 gate 注册无全局
                // 顺序保证）。短重试等 gate 就绪——同进程注册是微秒级，几次必命中。
                let resp = match ctx.runtimes.get(&session_id).await {
                    Some(rt) => {
                        if resolve_approval_with_retry(&rt.state, &request_id, decision).await {
                            HebcoreResponse::Accepted
                        } else {
                            // 失败必须留痕：此前只回 Error 不打日志，导致「审批回应失败」在
                            // 服务端零日志、无从排查（用户实测痛点）。gate 无此 pending 多因
                            // run 已结束 / judge 已自动结算 / request_id 不匹配。
                            tracing::warn!(%session_id, %request_id, "Approve 失败：gate 无此待结算审批");
                            HebcoreResponse::Error {
                                message: format!("未找到待结算审批 {request_id}"),
                            }
                        }
                    }
                    None => {
                        tracing::warn!(%session_id, %request_id, "Approve 失败：session 未激活");
                        HebcoreResponse::Error {
                            message: format!("session {session_id} 未激活"),
                        }
                    }
                };
                write_line(&mut write_half, &resp).await?;
            }
            Ok(HebcoreRequest::Answer {
                session_id,
                request_id,
                answer,
            }) => {
                let resp = match ctx.runtimes.get(&session_id).await {
                    Some(rt) => {
                        if answer_question_with_retry(&rt.state, &request_id, answer).await {
                            HebcoreResponse::Accepted
                        } else {
                            tracing::warn!(%session_id, %request_id, "Answer 失败：gate 无此待结算提问");
                            HebcoreResponse::Error {
                                message: format!("未找到待结算提问 {request_id}"),
                            }
                        }
                    }
                    None => {
                        tracing::warn!(%session_id, %request_id, "Answer 失败：session 未激活");
                        HebcoreResponse::Error {
                            message: format!("session {session_id} 未激活"),
                        }
                    }
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
            Ok(HebcoreRequest::Inject {
                session_id,
                text,
                attachments,
            }) => {
                let resp = match ctx.runtimes.get(&session_id).await {
                    Some(rt) if rt.inject(crate::TurnInput::new(text, attachments)) => {
                        HebcoreResponse::Accepted
                    }
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
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue
                                }
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
            Ok(HebcoreRequest::SubscribeLogs) => {
                // 订阅本进程全局日志，逐行推给 surface（§4.10 多进程日志聚合）。run 移
                // hebcore 后 agent_loop 日志都在这进程，desktop 面板靠这条流才看得到。
                match observability::log_sender() {
                    Some(tx) => {
                        let mut rx = tx.subscribe();
                        loop {
                            match rx.recv().await {
                                Ok(line) => {
                                    if write_line(&mut write_half, &HebcoreResponse::Log { line })
                                        .await
                                        .is_err()
                                    {
                                        return Ok(());
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    None => {
                        write_line(
                            &mut write_half,
                            &HebcoreResponse::Error {
                                message: "日志系统未初始化".to_string(),
                            },
                        )
                        .await?;
                    }
                }
            }
            Ok(HebcoreRequest::GetVersion) => {
                let has_active_run = ctx.runtimes.has_active_run().await;
                write_line(
                    &mut write_half,
                    &HebcoreResponse::Version {
                        build_version: ctx.build_version.clone(),
                        bin_name: ctx.bin_name.clone(),
                        pid: std::process::id(),
                        has_active_run,
                    },
                )
                .await?;
            }
            Ok(HebcoreRequest::Shutdown) => {
                // 有活跃 run 拒绝关停——避免杀掉正在写 partial 的 run（§4.9.2 happens-before）。
                if ctx.runtimes.has_active_run().await {
                    write_line(
                        &mut write_half,
                        &HebcoreResponse::Error {
                            message: "有对话正在运行，拒绝关停".into(),
                        },
                    )
                    .await?;
                } else {
                    // 已确认无活跃 run → 无 partial 在写，process::exit 跳过 Drop 是安全的
                    // （§4.9.2）。OS 在进程死时释放单例锁；残留 sock 由下个 hebcore 启动 remove
                    // （hebcore main 的 bind 前 remove_file）。先 flush 应答 + 短暂 drain 到 peer
                    // 再退，让 desktop 确认收到 Accepted。
                    write_line(&mut write_half, &HebcoreResponse::Accepted).await?;
                    let _ = write_half.flush().await;
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    std::process::exit(0);
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
async fn write_line(w: &mut (impl AsyncWriteExt + Unpin), msg: &HebcoreResponse<'_>) -> Result<()> {
    let mut out = serde_json::to_string(msg)
        .unwrap_or_else(|e| format!("{{\"kind\":\"error\",\"message\":\"序列化失败: {e}\"}}"));
    out.push('\n');
    w.write_all(out.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

/// 审批结算的时序容忍重试（§7.8.6）：gate 由 agent_loop 在 emit 事件后紧接着注册，
/// 客户端的 Approve 可能抢先到达。重试 ~500ms（每 10ms 一次）等 gate 就绪。
async fn resolve_approval_with_retry(
    rt: &agent_core::session_hub::SessionRuntimeState,
    request_id: &str,
    decision: protocol::ApprovalDecision,
) -> bool {
    for _ in 0..50 {
        if rt.resolve_approval(request_id, decision.clone()) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

/// 提问结算的时序容忍重试（同 [`resolve_approval_with_retry`]）。
async fn answer_question_with_retry(
    rt: &agent_core::session_hub::SessionRuntimeState,
    request_id: &str,
    answer: protocol::UserAnswer,
) -> bool {
    for _ in 0..50 {
        if rt.answer_question(request_id, answer.clone()) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SubscribeLogs` 请求 + `Log` 响应的 wire 格式：desktop 侧 `Req`/`Resp` 是独立定义的
    /// 镜像 enum，靠 serde tag 对齐，这测试钉死格式防两边漂移（§4.10 多进程日志聚合）。
    #[test]
    fn subscribe_logs_and_log_frame_wire_format() {
        // desktop 发 {"kind":"subscribe_logs"} → hebcore 反序列化成 SubscribeLogs
        let req: HebcoreRequest = serde_json::from_str(r#"{"kind":"subscribe_logs"}"#).unwrap();
        assert!(matches!(req, HebcoreRequest::SubscribeLogs));

        // hebcore 发 Log 帧 → desktop Resp::Log 按同 tag / 字段反序列化
        let line = observability::LogLine {
            level: "INFO".to_string(),
            target: "model".to_string(),
            message: "[Model:Request] 发起模型请求".to_string(),
            ts: "12:00:00.000".to_string(),
        };
        let json = serde_json::to_string(&HebcoreResponse::Log { line }).unwrap();
        assert!(json.contains(r#""kind":"log""#), "tag 应为 log: {json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["line"]["target"], "model");
        assert_eq!(v["line"]["level"], "INFO");
        assert_eq!(v["line"]["message"], "[Model:Request] 发起模型请求");
    }

    #[test]
    fn start_run_and_inject_preserve_attachments_on_wire() {
        let start: HebcoreRequest = serde_json::from_str(
            r#"{"kind":"start_run","session_id":"s1","text":"看图","attachments":[{"kind":"image","name":"p.png","media_type":"image/png","data":"iVBORw0KGgo="}]}"#,
        )
        .unwrap();
        match start {
            HebcoreRequest::StartRun { attachments, .. } => {
                assert_eq!(attachments.len(), 1);
            }
            other => panic!("应是 StartRun，实际 {other:?}"),
        }

        let inject: HebcoreRequest = serde_json::from_str(
            r#"{"kind":"inject","session_id":"s1","text":"补一张","attachments":[{"kind":"image","name":"p.png","media_type":"image/png","data":"iVBORw0KGgo="}]}"#,
        )
        .unwrap();
        match inject {
            HebcoreRequest::Inject { attachments, .. } => {
                assert_eq!(attachments.len(), 1);
            }
            other => panic!("应是 Inject，实际 {other:?}"),
        }
    }
}
