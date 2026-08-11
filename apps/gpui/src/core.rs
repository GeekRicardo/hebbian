//! Core facade 的薄适配层（架构 §7.1）。
//!
//! 这里只做两件事：把 UI 动作翻译成 core 请求、把 core 的 `WireEvent` 搬回 UI 线程。
//! 对话链路一律走 `surface-session` 的 `RuntimeRegistry → SessionRuntime`，同步能力一律走
//! `core_rpc::dispatch`——不在 surface 里复制任何 storage / provider / HITL 业务流程。
//!
//! 线程模型：agent-core 内部需要 tokio 运行时（reqwest / 文件锁 / 后台任务），而 gpui 有
//! 自己的执行器。所以这里常驻一个多线程 tokio Runtime 跑 core 侧的一切，UI 侧通过
//! `tokio::sync::mpsc` 把事件拉回 gpui 的 async 任务里——mpsc 的 `recv()` 不依赖 tokio
//! 运行时上下文，可以安全地在 gpui 执行器上 await。

use std::path::PathBuf;
use std::sync::Arc;

use agent_core::permissions::PermissionStore;
use agent_core::storage::projects::WorkspaceProject;
use agent_core::storage::sessions::{self, Session, SessionMeta};
use anyhow::{anyhow, Result};
use agent_core::core_client::{CoreClient, LocalCoreClient};
use protocol::{ApprovalDecision, UserAnswer, WireEvent};
use surface_session::{RuntimeRegistry, TurnInput};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// UI 侧收到的一条更新。`WireEvent` 之外还有几种「core 调用返回了」的通知。
#[derive(Debug)]
pub enum CoreUpdate {
    /// 活 run 推来的事件。
    Wire { session_id: String, event: WireEvent },
    /// 会话列表 / 项目列表刷新完成。
    Catalog {
        sessions: Vec<SessionMeta>,
        projects: Vec<WorkspaceProject>,
    },
    /// 某个会话的完整历史读完了。
    SessionLoaded(Box<Session>),
    /// 新建会话成功，附带要打开的 id。
    SessionCreated(String),
    /// 任何一步失败。文案直接进 toast。
    Failed(String),
}

/// 常驻的 core 句柄。克隆代价只有几个 Arc。
#[derive(Clone)]
pub struct Core {
    inner: Arc<CoreInner>,
}

struct CoreInner {
    rt: Runtime,
    data_dir: PathBuf,
    permission_store: Option<Arc<PermissionStore>>,
    runtimes: RuntimeRegistry,
    tx: UnboundedSender<CoreUpdate>,
}

impl Core {
    /// 启动 core：建 tokio 运行时、打开权限库、注册 wakeup 续跑处理器。
    ///
    /// 返回的 receiver 交给 gpui 的 async 任务消费；它是 UI 唯一的事件入口。
    pub fn start() -> Result<(Self, UnboundedReceiver<CoreUpdate>)> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let data_dir = agent_core::storage::default_data_dir();
        let permission_store = PermissionStore::open(data_dir.clone()).ok().map(Arc::new);
        let runtimes = RuntimeRegistry::new();

        // 后端自主发起的 run（wakeup / 定时续跑）也要能在本 surface 里跑起来，
        // 否则挂起的长任务只有 Desktop 开着时才会醒。
        let _guard = rt.enter();
        surface_session::register_wakeup_resume_handler(
            data_dir.clone(),
            permission_store.clone(),
            runtimes.clone(),
        );
        drop(_guard);

        let (tx, rx) = mpsc::unbounded_channel();
        Ok((
            Self {
                inner: Arc::new(CoreInner {
                    rt,
                    data_dir,
                    permission_store,
                    runtimes,
                    tx,
                }),
            },
            rx,
        ))
    }

    pub fn data_dir(&self) -> &PathBuf {
        &self.inner.data_dir
    }

    /// 同步能力的 facade。这里不挂 Harness——每个 `SessionRuntime` 自己跑 agent_loop，
    /// facade 只承担 list / load / delete 这类同步转发（与 hebweb 的构造方式一致）。
    fn local_client(&self) -> LocalCoreClient {
        LocalCoreClient::new(
            None,
            self.inner.data_dir.clone(),
            self.inner.permission_store.clone(),
        )
    }

    fn emit(&self, update: CoreUpdate) {
        let _ = self.inner.tx.send(update);
    }

    fn emit_err(&self, err: impl std::fmt::Display) {
        self.emit(CoreUpdate::Failed(err.to_string()));
    }

    /// 拉一次会话 + 项目列表。两者都走 `LocalCoreClient`，与 Desktop / hebweb 同一实现。
    pub fn refresh_catalog(&self) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let client = this.local_client();
            match (client.list_sessions(), client.list_projects()) {
                (Ok(sessions), Ok(projects)) => {
                    this.emit(CoreUpdate::Catalog { sessions, projects })
                }
                (Err(err), _) | (_, Err(err)) => this.emit_err(err),
            }
        });
    }

    /// 打开一个会话：读全量 transcript（带 partial 恢复，崩溃后半条也能读回来）。
    pub fn open_session(&self, session_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match sessions::load_with_partial_recovery(&this.inner.data_dir, &session_id) {
                Ok(session) => {
                    this.subscribe(session_id.clone());
                    this.emit(CoreUpdate::SessionLoaded(Box::new(session)));
                }
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 新建会话。provider / model 取第一个启用的供应商及其默认模型。
    pub fn create_session(&self, project_id: Option<String>, workdir: Option<String>) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this.create_session_blocking(project_id, workdir) {
                Ok(id) => {
                    this.refresh_catalog();
                    this.emit(CoreUpdate::SessionCreated(id));
                }
                Err(err) => this.emit_err(err),
            }
        });
    }

    fn create_session_blocking(
        &self,
        project_id: Option<String>,
        workdir: Option<String>,
    ) -> Result<String> {
        let data_dir = &self.inner.data_dir;
        let providers = model_gateway::config::load(data_dir)?;
        let provider = providers
            .providers
            .iter()
            .find(|p| p.enabled)
            .or_else(|| providers.providers.first())
            .ok_or_else(|| anyhow!("还没有配置模型供应商"))?;
        let model = provider
            .default_model
            .clone()
            .or_else(|| provider.models.first().cloned())
            .ok_or_else(|| anyhow!("供应商 {} 还没有可用模型", provider.name))?;
        let session = sessions::create_with_source(
            data_dir,
            provider.id.clone(),
            model,
            workdir,
            project_id,
            "gpui".to_string(),
        )?;
        agent_core::storage::sessions_dir::ensure_session_dirs(data_dir, &session.id)?;
        Ok(session.id)
    }

    pub fn delete_session(&self, session_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let client = this.local_client();
            match client.delete_session(&session_id) {
                Ok(()) => this.refresh_catalog(),
                Err(err) => this.emit_err(err),
            }
        });
    }

    pub fn delete_project(&self, project_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let client = this.local_client();
            match client.delete_project(&project_id) {
                Ok(()) => this.refresh_catalog(),
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 发一条用户消息。活 run 中则插队，否则起新 turn——与 Desktop 完全同一语义。
    pub fn send_message(&self, session_id: String, text: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let runtime = match this
                .inner
                .runtimes
                .ensure(
                    &this.inner.data_dir,
                    this.inner.permission_store.clone(),
                    &session_id,
                )
                .await
            {
                Ok(runtime) => runtime,
                Err(err) => return this.emit_err(err),
            };
            this.subscribe(session_id.clone());

            if runtime.is_active() {
                if !runtime.inject(TurnInput::text(text)) {
                    this.emit_err("当前对话正在运行，插队失败");
                }
                return;
            }
            if runtime.input_tx.send(TurnInput::text(text)).is_err() {
                this.emit_err("运行通道已关闭");
            }
        });
    }

    /// 订阅某个会话的事件流。重复调用是安全的：每次订阅拿的是同一个 broadcast 的新读端，
    /// 收到 `RunFinished` / `Error` 后自行退出，不会常驻堆积。
    fn subscribe(&self, session_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let runtime = match this
                .inner
                .runtimes
                .ensure(
                    &this.inner.data_dir,
                    this.inner.permission_store.clone(),
                    &session_id,
                )
                .await
            {
                Ok(runtime) => runtime,
                Err(err) => return this.emit_err(err),
            };
            let mut events = runtime.state.subscribe();
            while let Ok(envelope) = events.recv().await {
                let finished = matches!(
                    envelope.event,
                    WireEvent::RunFinished { .. } | WireEvent::Error { .. }
                );
                this.emit(CoreUpdate::Wire {
                    session_id: session_id.clone(),
                    event: envelope.event,
                });
                if finished {
                    break;
                }
            }
        });
    }

    pub fn resolve_approval(&self, session_id: String, request_id: String, decision: ApprovalDecision) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this
                .inner
                .runtimes
                .ensure(
                    &this.inner.data_dir,
                    this.inner.permission_store.clone(),
                    &session_id,
                )
                .await
            {
                Ok(runtime) => {
                    runtime.state.resolve_approval(&request_id, decision);
                }
                Err(err) => this.emit_err(err),
            }
        });
    }

    pub fn answer_question(&self, session_id: String, request_id: String, answer: UserAnswer) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this
                .inner
                .runtimes
                .ensure(
                    &this.inner.data_dir,
                    this.inner.permission_store.clone(),
                    &session_id,
                )
                .await
            {
                Ok(runtime) => {
                    runtime.state.answer_question(&request_id, answer);
                }
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 中断当前 run。
    pub fn interrupt(&self, session_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this
                .inner
                .runtimes
                .ensure(
                    &this.inner.data_dir,
                    this.inner.permission_store.clone(),
                    &session_id,
                )
                .await
            {
                Ok(runtime) => runtime.stop(),
                Err(err) => this.emit_err(err),
            }
        });
    }
}
