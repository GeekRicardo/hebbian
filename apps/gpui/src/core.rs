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
    /// 这个会话改过的文件（按 run 分组，新的在前）。
    Edits(Vec<agent_core::edits::metadata::RunEditEntry>),
    /// 从这个会话分叉出去的旁支（会话 id + 标题）。
    Branches(Vec<(String, String)>),
    /// 角色（prompts）/ 子 agent / 插件 / MCP / Hooks 的清单，供设置页展示。
    Extras(Box<Extras>),
    /// 最近一份调度日志的尾部（设置里的「日志」页）。
    LogTail { name: String, lines: Vec<String> },
    /// 权限规则读完了（全局层的 allow / deny）。
    Permissions { allow: Vec<String>, deny: Vec<String> },
    /// 上下文占用与缓存命中（输入框右下角那个环 + 百分比）。
    ContextUsage {
        used_tokens: usize,
        budget_tokens: usize,
        cache_hit_pct: u32,
    },
    /// 注册表里还活着的后台任务。
    LiveTasks(Vec<crate::state::LiveTask>),
    /// 运行模式改好了。
    RunModeChanged(agent_core::run_mode::RunMode),
    /// 全局设置读完了。
    Settings(Box<agent_core::storage::settings::Settings>),
    /// 供应商列表刷新完成（模型选择器用）。
    Providers(Vec<model_gateway::config::Provider>),
    /// 某会话的 plan 列表（新到旧），每项是（标题, 正文）。
    Plans(Vec<(String, String)>),
    /// 某个文件的 diff 两侧文本。
    DiffLoaded {
        rel_path: String,
        before: String,
        after: String,
    },
    /// 可用的 skill 列表（`//` 命令面板用）。
    Skills(Vec<agent_core::tools::skill::Skill>),
    /// git 状态读完了。`None` 表示这个目录不是 git 仓库。
    GitStatus(Option<Box<agent_core::git_scm::GitProjectStatus>>),
    /// 某个文件存好了，附上落盘的内容（用来更新「有没有未保存改动」的基线）。
    FileSaved { path: PathBuf, text: String },
    /// 某个文件读完了（编辑区打开）。
    FileLoaded { path: PathBuf, text: String },
    /// 某个目录读完了（文件树按需展开）。
    DirListed {
        path: PathBuf,
        entries: Vec<DirEntry>,
    },
    /// 任何一步失败。文案直接进 toast。
    Failed(String),
}

/// 设置里几页共用的一坨只读清单。一次性取回，省得每页各发一次。
#[derive(Debug, Default)]
pub struct Extras {
    pub prompts: Vec<agent_core::storage::prompts::Prompt>,
    pub subagents: Vec<agent_core::storage::subagents::SubagentDefinition>,
    pub plugins: Vec<agent_core::storage::plugins::PluginListItem>,
    pub mcp_servers: Vec<String>,
    pub hooks_raw: String,
}

/// 文件树的一个条目。
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
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

    /// 拉一次供应商列表。模型选择器展开时用，冷启动也预取一次。
    pub fn refresh_providers(&self) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this.local_client().list_providers() {
                Ok(file) => this.emit(CoreUpdate::Providers(file.providers)),
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 从某条消息处分叉出一个新对话（保留到该消息为止的历史）。
    pub fn fork_session(&self, session_id: String, up_to_message_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match agent_core::storage::sessions::fork(
                &this.inner.data_dir,
                &session_id,
                &up_to_message_id,
            ) {
                Ok(forked) => {
                    this.refresh_catalog();
                    // 分叉完直接切过去——原 UI 也是分叉即进入新对话。
                    this.emit(CoreUpdate::SessionCreated(forked.id));
                }
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 改思考强度。业务规则（插 ReasoningSwitch marker）只在 core 一处。
    pub fn set_reasoning(
        &self,
        session_id: String,
        reasoning: Option<common::ReasoningConfig>,
    ) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this
                .local_client()
                .set_session_reasoning(&session_id, reasoning)
            {
                Ok(session) => this.emit(CoreUpdate::SessionLoaded(Box::new(session))),
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 切运行模式。落两处：会话文件（跨重启保留）+ 活运行时（当前 run 立即生效）。
    /// 只落一处的话，要么重启就丢、要么当前这轮不生效。
    pub fn set_run_mode(&self, session_id: String, mode: agent_core::run_mode::RunMode) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            if let Err(err) = agent_core::storage::sessions::update_meta(
                &this.inner.data_dir,
                &session_id,
                |session| {
                    session.run_mode = mode;
                    Ok(())
                },
            ) {
                return this.emit_err(err);
            }
            if let Ok(runtime) = this
                .inner
                .runtimes
                .ensure(
                    &this.inner.data_dir,
                    this.inner.permission_store.clone(),
                    &session_id,
                )
                .await
            {
                runtime.state.set_run_mode(mode);
            }
            this.emit(CoreUpdate::RunModeChanged(mode));
        });
    }

    /// 切供应商 + 模型。业务规则（switch marker / 系列锁定 / 推理参数重置）
    /// 只在 `LocalCoreClient::switch_session_model` 一处，这里只是转发。
    pub fn switch_model(&self, session_id: String, provider_id: String, model: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this
                .local_client()
                .switch_session_model(&session_id, provider_id, model)
            {
                Ok(session) => {
                    this.emit(CoreUpdate::SessionLoaded(Box::new(session)));
                    this.refresh_catalog();
                }
                // 系列锁定这类拒绝要原样带给用户——那句文案本身就是解释。
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 改对话标题。改完刷新列表，让侧栏与头部同步。
    pub fn rename_session(&self, session_id: String, title: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this.local_client().rename_session(&session_id, title) {
                Ok(session) => {
                    this.emit(CoreUpdate::SessionLoaded(Box::new(session)));
                    this.refresh_catalog();
                }
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 保存全局设置，写完再回读一次让 UI 与磁盘对齐。
    pub fn save_settings(&self, settings: agent_core::storage::settings::Settings) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match this.local_client().save_settings(settings) {
                Ok(()) => this.refresh_settings(),
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 读一次可用 skill。三层目录（全局 / 项目 / 工作区）由 core 统一合并，
    /// 这里只负责把结果搬给 UI。
    pub fn refresh_skills(&self, workdir: PathBuf) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let skills = this.local_client().list_skills(&workdir);
            this.emit(CoreUpdate::Skills(skills));
        });
    }

    /// 读某会话的 plan。plan 按 workdir 归属（项目级 / 全局），
    /// 目录规则由 storage 决定，这里只负责读出来按时间倒序给 UI。
    pub fn refresh_plans(&self, session_id: String, workdir: Option<PathBuf>) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let dir = agent_core::storage::plans::dir_for_session(
                &this.inner.data_dir,
                workdir.as_deref(),
                &session_id,
            );
            let mut plans: Vec<(String, String)> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) != Some("md") {
                        continue;
                    }
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if let Ok(body) = std::fs::read_to_string(&path) {
                        plans.push((name, body));
                    }
                }
            }
            // 文件名带时间戳，倒序即最新在前。
            plans.sort_by(|a, b| b.0.cmp(&a.0));
            this.emit(CoreUpdate::Plans(plans));
        });
    }

    /// 取某个文件的改动两侧文本。`staged` 决定比的是「暂存了什么」还是「还没暂存的」。
    pub fn load_diff(&self, workdir: PathBuf, rel_path: String, staged: bool) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match agent_core::git_scm::diff_file(&workdir, &rel_path, staged) {
                Ok((before, after)) => this.emit(CoreUpdate::DiffLoaded {
                    rel_path,
                    before,
                    after,
                }),
                Err(err) => this.emit_err(format!("取不到这个文件的改动：{err}")),
            }
        });
    }

    /// 读一次工作目录的 git 状态。不是仓库不算错——很多对话的目录本来就没进 git，
    /// 这种情况回 None 让面板显示「不是 git 仓库」，而不是弹一条报错。
    pub fn refresh_git(&self, workdir: PathBuf) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match agent_core::git_scm::status(&workdir) {
                Ok(status) => this.emit(CoreUpdate::GitStatus(Some(Box::new(status)))),
                Err(_) => this.emit(CoreUpdate::GitStatus(None)),
            }
        });
    }

    /// 把编辑区的内容写回磁盘。
    ///
    /// 只写已经存在的文件——编辑区是从文件树打开的，路径必然存在；
    /// 拒绝创建新文件是为了避免手滑把内容写到某个拼错的路径上。
    pub fn write_file(&self, path: PathBuf, text: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            if !path.is_file() {
                return this.emit_err("这个文件不在了，没法保存");
            }
            match std::fs::write(&path, &text) {
                Ok(()) => this.emit(CoreUpdate::FileSaved { path, text }),
                Err(err) => this.emit_err(format!("保存失败：{err}")),
            }
        });
    }

    /// 读一个文件的文本内容，给编辑区用。
    ///
    /// 二进制文件不往编辑区塞——UTF-8 解不出来就直接报错，比塞一堆乱码好。
    /// 同样拒绝超大文件：编辑区一次性载入，几十 MB 会把界面卡住。
    pub fn read_file(&self, path: PathBuf) {
        const MAX_BYTES: u64 = 2 * 1024 * 1024;
        let this = self.clone();
        self.inner.rt.spawn(async move {
            match std::fs::metadata(&path) {
                Ok(meta) if meta.len() > MAX_BYTES => {
                    return this.emit_err("这个文件太大了，编辑区暂时打不开");
                }
                Err(err) => return this.emit_err(format!("打不开这个文件：{err}")),
                _ => {}
            }
            match std::fs::read(&path) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => this.emit(CoreUpdate::FileLoaded { path, text }),
                    Err(_) => this.emit_err("这是个二进制文件，编辑区显示不了"),
                },
                Err(err) => this.emit_err(format!("读不了这个文件：{err}")),
            }
        });
    }

    /// 新建项目：把一个目录登记成 workspace project。
    pub fn create_project(&self, workdir: PathBuf) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let name = workdir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "项目".to_string());
            let input = agent_core::storage::projects::WorkspaceProjectInput {
                id: None,
                name,
                workdir,
                allowed_paths: Vec::new(),
                source: Some("manual".to_string()),
            };
            match this.local_client().save_project(input) {
                Ok(_) => this.refresh_catalog(),
                Err(err) => this.emit_err(err),
            }
        });
    }

    /// 读这个会话的改动记录（edits-worktree 的 run 清单）。
    pub fn refresh_edits(&self, session_id: String, workdir: Option<PathBuf>) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let Some(workdir) = workdir else {
                return this.emit(CoreUpdate::Edits(Vec::new()));
            };
            let workspace = agent_core::Workspace::new(workdir, Vec::new());
            let tree = agent_core::edits::EditsWorktree::new(
                &this.inner.data_dir,
                &session_id,
                &workspace,
            );
            let mut runs = tree.list_runs().unwrap_or_default();
            // 新的在前，与消息流的阅读方向一致。
            runs.reverse();
            this.emit(CoreUpdate::Edits(runs));
        });
    }

    /// 算当前会话的上下文占用与缓存命中率。
    ///
    /// **算法与 Desktop 完全一致**（`apps/desktop/src/chat.rs::context_usage`）：
    /// 用 `calibrated_transcript_tokens`（拿上一次真实 input_tokens 校准估算），
    /// 分母走 provider 的真实 context window。之前这里是我按「消息条数 / 200」瞎凑的
    /// 假百分比——看着像真数据的假数据比不显示更糟。
    pub fn refresh_context_usage(&self, session_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let Ok(session) =
                agent_core::storage::sessions::load(&this.inner.data_dir, &session_id)
            else {
                return;
            };
            let transcript = agent_core::context::transcript::Transcript::from_session(
                session.system_prompt.clone(),
                &session.messages,
            );
            let (last_real, last_estimated) = session
                .token_stats
                .as_ref()
                .map(|s| (s.last_input_tokens, s.last_estimated_tokens))
                .unwrap_or((0, 0));
            let used = agent_core::context::budget::calibrated_transcript_tokens(
                transcript.system.as_deref(),
                &transcript.entries,
                last_real,
                last_estimated,
            );
            let budget = match model_gateway::config::get(
                &this.inner.data_dir,
                &session.provider_id,
            ) {
                Ok(p) => {
                    model_gateway::context_window::resolve_context_window(&p, &session.model).await
                }
                Err(_) => 200_000,
            };
            // 缓存命中率 = 整个对话累计 cache_read / 累计 input（与原前端同口径）。
            let cache_hit_pct = session
                .token_stats
                .as_ref()
                .filter(|s| s.input_tokens > 0)
                .map(|s| ((s.cache_read_tokens as f64 / s.input_tokens as f64) * 100.0) as u32)
                .unwrap_or(0);
            this.emit(CoreUpdate::ContextUsage {
                used_tokens: used,
                budget_tokens: budget,
                cache_hit_pct,
            });
        });
    }

    /// 读这个会话在**本进程**注册表里的活后台任务。
    ///
    /// 注册表是进程内的：gpui 自己跑 run，所以这里看到的就是它自己起的那些。
    /// 别的 surface（Desktop / heb）起的后台任务在它们各自进程里，这边看不到——
    /// 这是 in-process 架构的既有边界（架构 §7.5），不是这里漏读了。
    pub fn refresh_live_tasks(&self, session_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let registry = agent_core::tools::background::registry_for_session(&session_id);
            let tasks = registry
                .list()
                .into_iter()
                .filter(|shell| shell.is_background())
                .map(|shell| {
                    use agent_core::tools::background::ShellState;
                    let state = shell.state();
                    crate::state::LiveTask {
                        task_id: shell.task_id.clone(),
                        command: shell.command.clone(),
                        running: matches!(state, ShellState::Running),
                        exit_code: match state {
                            ShellState::Exited { code } => code,
                            _ => None,
                        },
                    }
                })
                .collect();
            this.emit(CoreUpdate::LiveTasks(tasks));
        });
    }

    /// 回退一整个 run 的改动。
    ///
    /// 这是**破坏性操作**：它会把工作区里的真实文件改回去。所以只回退在清单里
    /// 明确存在、且还没回退过的那一条；成功后立刻重读清单，让「已回退」徽章即时出现。
    pub fn revert_run(&self, session_id: String, workdir: Option<PathBuf>, run_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let Some(workdir) = workdir else {
                return this.emit_err("这个对话没有工作目录，没法回退");
            };
            let workspace = agent_core::Workspace::new(workdir.clone(), Vec::new());
            let tree = agent_core::edits::EditsWorktree::new(
                &this.inner.data_dir,
                &session_id,
                &workspace,
            );
            let runs = match tree.list_runs() {
                Ok(runs) => runs,
                Err(err) => return this.emit_err(err),
            };
            let Some(entry) = runs.iter().find(|r| r.run_id == run_id) else {
                return this.emit_err("找不到这次改动，可能已经被清理了");
            };
            if entry.reverted {
                return this.emit_err("这次改动已经回退过了");
            }
            if let Err(err) = tree.revert_run(entry).await {
                // git 的原始报错对用户没意义（里面全是内部镜像路径），
                // 但完全吞掉又让人不知道为什么失败——给一句人话，细节留日志。
                // 用 error 而不是 warn：默认日志级别看得到。回退是用户主动发起的
                // 破坏性操作，失败了却查不到原因，比多打一行日志糟糕得多。
                tracing::error!(error = %err, run_id = %run_id, "回退失败");
                return this.emit_err(
                    "这轮改动回退不了：改动快照不完整或已被清理，只能手动改回去",
                );
            }
            if let Err(err) = tree.mark_run_reverted(&run_id) {
                return this.emit_err(err);
            }
            this.refresh_edits(session_id, Some(workdir));
        });
    }

    /// 找从这个会话分叉出去的旁支。
    ///
    /// `forked_from` 只写在每个 session.jsonl 的第一行（meta），`SessionMeta` 不带它，
    /// 所以这里逐个读首行。会话数量是几百量级、只读一行，代价可以接受。
    pub fn refresh_branches(&self, parent_id: String) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let mut out = Vec::new();
            let root = this.inner.data_dir.join("sessions");
            if let Ok(entries) = std::fs::read_dir(&root) {
                for entry in entries.flatten() {
                    let path = entry.path().join("session.jsonl");
                    if !path.is_file() {
                        continue;
                    }
                    let Ok(file) = std::fs::File::open(&path) else {
                        continue;
                    };
                    let mut first = String::new();
                    if std::io::BufRead::read_line(
                        &mut std::io::BufReader::new(file),
                        &mut first,
                    )
                    .is_err()
                    {
                        continue;
                    }
                    // 正常的 session.jsonl 第一行就是 meta。但仓库里还兼容老的
                    // pretty-JSON 会话（storage 里有专门的兼容分支），那种文件第一行
                    // 只有一个 `{`——解析失败时整份读一次兜底，否则老会话的旁支会静默漏掉。
                    let meta = match serde_json::from_str::<serde_json::Value>(&first) {
                        Ok(meta) => meta,
                        Err(_) => match std::fs::read_to_string(&path)
                            .ok()
                            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                        {
                            Some(meta) => meta,
                            None => continue,
                        },
                    };
                    if meta.get("forked_from").and_then(|v| v.as_str()) != Some(parent_id.as_str())
                    {
                        continue;
                    }
                    let id = meta
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let title = meta
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("旁支对话")
                        .to_string();
                    if !id.is_empty() {
                        out.push((id, title));
                    }
                }
            }
            this.emit(CoreUpdate::Branches(out));
        });
    }

    /// 一次取回设置里几页要的只读清单。
    pub fn refresh_extras(&self, workdir: Option<PathBuf>) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let client = this.local_client();
            let mcp = client.get_mcp_config();
            let extras = Extras {
                prompts: client.list_prompts().map(|f| f.prompts).unwrap_or_default(),
                subagents: client.list_subagents(workdir.as_deref()),
                plugins: client.plugin_list(),
                mcp_servers: mcp.mcp_servers.keys().cloned().collect(),
                hooks_raw: client.get_hooks_raw(),
            };
            this.emit(CoreUpdate::Extras(Box::new(extras)));
        });
    }

    /// 读最近一份调度日志的尾部。
    ///
    /// 只取末尾若干行：日志按天 rotate，整份可能很大，设置页要的是「刚发生了什么」。
    pub fn refresh_log_tail(&self) {
        const TAIL_LINES: usize = 200;
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let dir = this.inner.data_dir.join("logs");
            let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    let modified = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
                        newest = Some((modified, path));
                    }
                }
            }
            let Some((_, path)) = newest else {
                return this.emit(CoreUpdate::LogTail {
                    name: String::new(),
                    lines: Vec::new(),
                });
            };
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let lines = std::fs::read_to_string(&path)
                .map(|body| {
                    let all: Vec<&str> = body.lines().collect();
                    all[all.len().saturating_sub(TAIL_LINES)..]
                        .iter()
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default();
            this.emit(CoreUpdate::LogTail { name, lines });
        });
    }

    /// 读一次全局层的权限规则。项目 / 会话层要带 workdir、session_id，
    /// 设置面板看的是全局层，与原前端的「权限」页一致。
    pub fn refresh_permissions(&self) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let client = this.local_client();
            let allow = client.list_permissions(
                protocol::PermissionScope::Global,
                None,
                None,
                agent_core::permissions::RuleEffect::Allow,
            );
            let deny = client.list_permissions(
                protocol::PermissionScope::Global,
                None,
                None,
                agent_core::permissions::RuleEffect::Deny,
            );
            this.emit(CoreUpdate::Permissions { allow, deny });
        });
    }

    /// 读一次全局设置。
    pub fn refresh_settings(&self) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let settings = this.local_client().get_settings();
            this.emit(CoreUpdate::Settings(Box::new(settings)));
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

    /// 读一层目录，结果回推 UI。文件树按需展开，不预读整棵树。
    ///
    /// 排序与原前端一致：目录在前、同类按名字（大小写不敏感）排。隐藏文件跟着
    /// `.gitignore` 之外的常识过滤——只滤掉 `.git`，其余点开头的仍然显示，
    /// 因为 agent 的工作目录里 `.env` / `.claude` 这类文件用户是要看见的。
    pub fn list_dir(&self, path: PathBuf) {
        let this = self.clone();
        self.inner.rt.spawn(async move {
            let read = std::fs::read_dir(&path);
            let mut entries = Vec::new();
            match read {
                Ok(iter) => {
                    for entry in iter.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name == ".git" {
                            continue;
                        }
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        entries.push(DirEntry {
                            name,
                            path: entry.path(),
                            is_dir,
                        });
                    }
                }
                Err(err) => return this.emit_err(format!("读不了这个文件夹：{err}")),
            }
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            this.emit(CoreUpdate::DirListed { path, entries });
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
