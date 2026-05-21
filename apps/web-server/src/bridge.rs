//! Tauri 前端 invoke proxy 桥接。
//!
//! desktop 启动时前端 outbound 连 `ws://<mediator>/ws/bridge`，注册自己为 invoke 代理。
//! mediator 收到浏览器 client 的 invoke 命令时，如果有 bridge 在，转发给 bridge，
//! 由 Tauri 前端调真实 `invoke(cmd, args)` 返回结果——desktop 全部命令（包括 OAuth、
//! EditsWorktree 等 hebweb 自己没镜像的）瞬间可用。
//!
//! Step 1：仅代理 sync invoke。流式 Channel / 全局 listen 待 Step 2。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::protocol::BridgeOutbound;

const INVOKE_TIMEOUT: Duration = Duration::from_secs(60);

/// 一个 Tauri 前端 bridge 注册到 mediator 后的代表。
///
/// `outbound_tx` 把 `BridgeOutbound`（如 `ProxyInvoke`）写到该 bridge 的 WS。
/// `pending` 用 req_id 索引等待响应的 oneshot——bridge 回 `ProxyResponse` 时按 id 唤醒。
pub struct BridgeClient {
    pub label: String,
    outbound_tx: mpsc::UnboundedSender<BridgeOutbound>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<ProxyResult>>>>,
}

#[derive(Debug)]
pub struct ProxyResult {
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

impl BridgeClient {
    pub fn new(label: String, outbound_tx: mpsc::UnboundedSender<BridgeOutbound>) -> Self {
        Self {
            label,
            outbound_tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn pending(&self) -> Arc<Mutex<HashMap<String, oneshot::Sender<ProxyResult>>>> {
        self.pending.clone()
    }

    /// 让 bridge 端执行一次 Tauri invoke，阻塞等响应（或超时）。
    pub async fn proxy_invoke(&self, cmd: String, args: Value) -> Result<ProxyResult> {
        let req_id = format!("p_{}", uuid::Uuid::new_v4());
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(req_id.clone(), tx);

        self.outbound_tx
            .send(BridgeOutbound::ProxyInvoke {
                req_id: req_id.clone(),
                cmd,
                args,
            })
            .map_err(|_| anyhow!("bridge outbound channel closed"))?;

        match tokio::time::timeout(INVOKE_TIMEOUT, rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(anyhow!("bridge dropped before responding")),
            Err(_) => {
                // 超时：清掉 pending 防止泄漏
                self.pending.lock().await.remove(&req_id);
                Err(anyhow!("bridge proxy timeout after {INVOKE_TIMEOUT:?}"))
            }
        }
    }
}

/// 共享 bridge 注册表：单进程允许多个 bridge 注册（多 desktop 窗口场景），
/// 当前实现总用最近注册的一个（如果有的话）。
#[derive(Default, Clone)]
pub struct BridgeRegistry {
    inner: Arc<Mutex<Vec<Arc<BridgeClient>>>>,
}

impl BridgeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, bridge: Arc<BridgeClient>) {
        self.inner.lock().await.push(bridge);
    }

    pub async fn unregister(&self, label: &str) {
        self.inner.lock().await.retain(|b| b.label != label);
    }

    /// 取最近注册的 bridge（如果有）。
    pub async fn pick(&self) -> Option<Arc<BridgeClient>> {
        self.inner.lock().await.last().cloned()
    }

    pub async fn count(&self) -> usize {
        self.inner.lock().await.len()
    }
}
