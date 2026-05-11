//! hebbian-cli — 终端 surface
//!
//! 三种用法：
//!
//! 1. **交互 loop（默认）**：`hebbian-cli`
//!    进入 readline 循环，每行一个 user turn，多 turn 上下文自动累积。
//!
//! 2. **单次 query**：`hebbian-cli "你好"`
//!    发起一次请求，流式输出回复后退出。
//!
//! 3. **JSON 多轮上下文**：`hebbian-cli --json '{"messages":[...]}'`
//!    一次性吃下完整对话历史，跑最后一条 user message 那一轮。
//!    `--json -` 表示从 stdin 读 JSON。

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use agent_core::{
    core_client::{CoreClient, LocalCoreClient},
    hooks::HookManager,
    tools::{default_tools, skill::default_skill_dirs, Tool},
    workspace::Workspace,
    Harness,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Local;
use clap::Parser;
use model_gateway::config::{Provider, ProvidersFile};
use model_gateway::{
    client::{DynModelClient, ModelClient},
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use common::reasoning::{ReasoningConfig, ReasoningEffort};
use agent_core::storage::sessions::{self, Session};
use common::CancelFlag;

mod mock_provider;
mod render;
mod session;
mod tui;

use session::{CliSession, ConvoInput, SessionPersist};

#[derive(Parser, Debug)]
#[command(name = "hebbian-cli", about = "Hebbian agent 终端 surface")]
struct Cli {
    /// 单次 query。不传则进入交互 loop
    prompt: Option<String>,

    /// 多轮 JSON 输入。值为 `-` 时从 stdin 读
    #[arg(long, conflicts_with = "prompt")]
    json: Option<String>,

    /// 使用 mock provider（无需配 API key）
    #[arg(long)]
    mock: bool,

    /// provider id/name，或 name/model_id 临时覆盖本次调用
    #[arg(long)]
    provider: Option<String>,

    /// 设置后续默认 provider/model：--provider set name/model_id
    #[arg(long, hide = true)]
    provider_set: Option<String>,

    /// provider 管理命令：--providers list
    #[arg(long)]
    providers: Option<String>,

    /// 模型 id
    #[arg(long, short = 'm')]
    model: Option<String>,

    /// system prompt
    #[arg(long, short = 's')]
    system: Option<String>,

    /// 启用的工具，逗号分隔
    #[arg(long, value_delimiter = ',')]
    tools: Vec<String>,

    /// 额外允许访问的目录（可重复）。不传则用全局 settings.conversation.allowed_dirs。
    #[arg(long = "allowed-dir", value_name = "DIR")]
    allowed_dirs: Vec<PathBuf>,

    /// data dir（默认与 desktop 共享：~/Library/Application Support/dev.ricardo.hebbian/）
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// 收到 PermissionRequested 时自动允许
    #[arg(long, default_value_t = true)]
    auto_approve: bool,

    /// 不把对话写入 `<data_dir>/sessions/`。`--mock` 也隐含 `--no-record`。
    #[arg(long)]
    no_record: bool,

    /// 加载已有 session 续聊。值是 `list_sessions` 显示的 id。
    /// 与 `--json` 互斥（JSON 多轮自带 messages，不需要再 seed）。
    #[arg(long, value_name = "SESSION_ID", conflicts_with = "json")]
    history: Option<String>,

    /// 打印 `<data_dir>/sessions/` 里的所有 session 列表，然后退出。
    #[arg(long)]
    list_history: bool,

    /// 开启 thinking / reasoning（claude-opus-4* / claude-sonnet-4* / gpt-5* / o-series）
    #[arg(long)]
    thinking: bool,

    /// 思考强度 low|medium|high|extra（默认 extra）
    #[arg(long, value_parser = parse_effort, default_value = "extra")]
    effort: ReasoningEffort,

    /// Anthropic 1M 上下文 beta header
    #[arg(long = "long-context")]
    long_context: bool,

    /// 运行模式（架构 §4.4.3）。
    /// 取值：`ask-before-edits` | `edit-automatically` | `plan-mode` | `auto-mode`
    #[arg(long = "run-mode", value_parser = parse_run_mode, default_value = "ask-before-edits")]
    run_mode: agent_core::run_mode::RunMode,

    /// 显式启用 ratatui 全屏 TUI（架构 §8）。默认在 isatty 时也自动启 TUI。
    #[arg(long, conflicts_with_all = &["repl", "prompt", "json"])]
    tui: bool,

    /// 显式启用 rustyline REPL 简易模式。非 TUI 路径的回退。
    #[arg(long, conflicts_with_all = &["tui", "prompt", "json"])]
    repl: bool,
}

/// 终端是否 isatty——TUI 路径自动判断。stdout / stderr / stdin 都是 tty 才进 TUI，
/// 否则降级 REPL（兼容管道 / CI / log capture）。
fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
}

fn parse_run_mode(s: &str) -> Result<agent_core::run_mode::RunMode, String> {
    agent_core::run_mode::RunMode::parse(s)
        .ok_or_else(|| format!("无效 run-mode：{s}（ask-before-edits|edit-automatically|plan-mode|auto-mode）"))
}

fn parse_effort(s: &str) -> Result<ReasoningEffort, String> {
    match s.to_ascii_lowercase().as_str() {
        "low" => Ok(ReasoningEffort::Low),
        "medium" | "med" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "extra" | "xhigh" => Ok(ReasoningEffort::Extra),
        other => Err(format!("无效 effort：{other}（low|medium|high|extra）")),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _otel_guard = observability::init("hebbian-cli", "warn");
    let cli = Cli::parse_from(normalize_provider_args(std::env::args()));

    // 让 sessions::create 写入的 jsonl Meta.source = "cli"
    sessions::set_default_source("cli");

    // 不再用 --workdir：直接用 CLI 进程当前目录
    let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // allowed_dirs：CLI 参数优先，否则用全局 settings 默认
    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);

    // 架构 §7：CLI 通过 CoreClient 调同步 API。Harness 仍在 build_harness_and_client
    // 里按需构造（CLI 长生命周期，但跑 run 时才需要 Harness），故 CoreClient 不挂 harness。
    let permission_store_for_core = match agent_core::permissions::PermissionStore::open(&data_dir)
    {
        Ok(s) => Some(Arc::new(s)),
        Err(_) => None,
    };
    let core_client: Arc<dyn CoreClient> = Arc::new(LocalCoreClient::new(
        None,
        data_dir.clone(),
        permission_store_for_core.clone(),
    ));

    if cli.list_history {
        return print_history_list(core_client.as_ref());
    }
    if let Some(command) = &cli.providers {
        return handle_providers_command(command, core_client.as_ref());
    }
    if let Some(target) = &cli.provider_set {
        return set_default_provider_model(core_client.as_ref(), target);
    }

    // 如果 --history，先把已有 session 加载出来。后续 provider/model/system 都可由它兜底。
    let existing: Option<Session> = match &cli.history {
        Some(id) => Some(
            sessions::load(&data_dir, id)
                .map_err(|e| anyhow!("加载 session {id} 失败：{e}"))?,
        ),
        None => None,
    };

    let settings = agent_core::storage::settings::load(&data_dir);
    let allowed_dirs = if !cli.allowed_dirs.is_empty() {
        cli.allowed_dirs.clone()
    } else {
        settings.conversation.allowed_dirs.clone()
    };
    let enabled_tools = if !cli.tools.is_empty() {
        cli.tools.clone()
    } else {
        settings.conversation.enabled_tools.clone()
    };
    let workspace = Workspace::new(workdir.clone(), allowed_dirs);

    let reasoning = if cli.thinking || cli.long_context {
        Some(ReasoningConfig {
            enabled: cli.thinking.then_some(true),
            effort: Some(cli.effort),
            long_context: cli.long_context.then_some(true),
        })
    } else {
        None
    };

    let built = build_harness_and_client(
        BuildOpts {
            mock: cli.mock,
            // existing session 的 provider/model 作为兜底
            provider: cli
                .provider
                .clone()
                .or_else(|| existing.as_ref().map(|s| s.provider_id.clone())),
            model: cli
                .model
                .clone()
                .or_else(|| existing.as_ref().map(|s| s.model.clone())),
            data_dir: Some(data_dir.clone()),
            reasoning: reasoning.clone(),
        },
        workspace.clone(),
    )
    .await?;

    // 是否落盘对话：--no-record 跳过；--json 跳过（一次性测试）。
    // mock 默认仍落盘，方便开发调试；不想落盘加 --no-record。
    let persist = !cli.no_record && cli.json.is_none();
    let session_record = if persist {
        Some(match existing {
            Some(s) => s,
            None => {
                let s = sessions::create_with_source(
                    &data_dir,
                    built
                        .provider_id
                        .clone()
                        .unwrap_or_else(|| if cli.mock { "mock".into() } else { "unknown".into() }),
                    built
                        .model
                        .clone()
                        .unwrap_or_else(|| if cli.mock { "mock".into() } else { "unknown".into() }),
                    cli.system.clone(),
                    None,
                    "cli".to_string(),
                )
                .map_err(|e| anyhow!("创建 session 失败：{e}"))?;
                // 架构 §4.9.1：同步建立 session 目录布局 + meta.json
                if let Err(e) = ensure_session_layout(&data_dir, &s) {
                    tracing::warn!(error = %e, session_id = %s.id, "初始化 session 目录失败");
                }
                s
            }
        })
    } else {
        None
    };

    let system = cli
        .system
        .clone()
        .or_else(|| session_record.as_ref().and_then(|s| s.system_prompt.clone()));

    // HEBBIAN_DUMP_MODEL_IO=1 时把每次模型请求的 request/response 落到
    // <data_dir>/sessions/<session_id>.model_io.jsonl 方便调试。需要 session_id，
    // 因此 --no-record / --json 这类无 session_record 的路径不开启。
    let model_io_dump = match session_record.as_ref() {
        Some(s) => agent_core::model_io_dump::open_for_session_if_enabled(&data_dir, &s.id).await,
        None => None,
    };
    // 退出前 flush dump 的句柄。Drop 时直接结束 tokio runtime 会让 spawned
    // writer task 来不及落盘（jsonl 文件 0 字节）。在 main 末尾显式 flush。
    let dump_for_flush = model_io_dump.clone();

    // 架构 §4.6.2：CLI 启动时打开 PermissionStore，注入到 Session。
    // 已在前面 core_client 构造时共用同一份。
    let permission_store = permission_store_for_core.clone();

    // 非默认模式（AutoMode / EditAutomatically / PlanMode）有专门的派发器决策路径；
    // CLI observer 的 auto_approve 在这些模式下应让位，否则会抢先短路 dispatch
    // 的 judge / 工具过滤 / 编辑放行逻辑。
    let effective_auto_approve = matches!(cli.run_mode, agent_core::run_mode::RunMode::AskBeforeEdits)
        .then_some(cli.auto_approve)
        .unwrap_or(false);

    let mut session = CliSession::new(
        built.harness,
        built.client,
        system,
        enabled_tools,
        effective_auto_approve,
        workspace,
        built.provider_display,
        session_record.map(|s| SessionPersist {
            data_dir: data_dir.clone(),
            session_id: s.id,
            seed_messages: s.messages,
        }),
        model_io_dump,
        permission_store,
        cli.run_mode,
        built.model.clone().unwrap_or_default(),
    );

    // 路由（架构 §8.3）：
    // - 显式 --tui / --repl：按用户选择
    // - 默认：终端 isatty 且 stdout/stderr 都是 tty → TUI；否则 REPL（兼容管道）
    let use_tui = if cli.tui {
        true
    } else if cli.repl {
        false
    } else {
        cli.prompt.is_none() && cli.json.is_none() && is_tty()
    };
    let outcome = match (cli.prompt, cli.json) {
        (None, None) if use_tui => {
            let (inner_session, provider_display, run_mode, persist) = session.into_tui_parts();
            tui::run_tui(inner_session, provider_display, run_mode, persist).await
        }
        (None, None) => session.run_loop().await,
        (Some(prompt), None) => session.run_single(prompt).await,
        (None, Some(json_arg)) => {
            let raw = read_json_arg(&json_arg)?;
            let convo: ConvoInput =
                serde_json::from_str(&raw).map_err(|e| anyhow!("无效 JSON：{e}"))?;
            session.run_with_history(convo.messages).await
        }
        (Some(_), Some(_)) => unreachable!("clap 通过 conflicts_with 已阻止"),
    };

    // 退出前等 dump writer task 把缓冲落盘，否则 jsonl 0 字节。
    if let Some(dump) = dump_for_flush {
        if let Err(e) = dump.flush().await {
            tracing::warn!(error = %e, "model_io_dump flush on exit failed");
        }
    }

    outcome
}

fn print_history_list(core: &dyn CoreClient) -> Result<()> {
    let metas = core
        .list_sessions()
        .map_err(|e| anyhow!("读 sessions：{e}"))?;
    if metas.is_empty() {
        println!("（无 session）");
        return Ok(());
    }
    for m in &metas {
        let when = chrono::DateTime::<Local>::from(
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(m.updated_at as u64),
        );
        println!(
            "{:>3} 条  {}  {}/{}  {}  {}",
            m.message_count,
            when.format("%Y-%m-%d %H:%M"),
            m.provider_id,
            m.model,
            m.id,
            m.title,
        );
    }
    Ok(())
}

fn normalize_provider_args(args: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--provider" && iter.peek().map(String::as_str) == Some("set") {
            out.push("--provider-set".to_string());
            let _ = iter.next();
            if let Some(target) = iter.next() {
                out.push(target);
            }
        } else if arg == "--provider=set" {
            out.push("--provider-set".to_string());
            if let Some(target) = iter.next() {
                out.push(target);
            }
        } else {
            out.push(arg);
        }
    }
    out
}

fn read_json_arg(arg: &str) -> Result<String> {
    if arg == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| anyhow!("读 stdin 失败：{e}"))?;
        Ok(buf)
    } else {
        Ok(arg.to_string())
    }
}

struct BuildOpts {
    mock: bool,
    provider: Option<String>,
    model: Option<String>,
    data_dir: Option<PathBuf>,
    reasoning: Option<ReasoningConfig>,
}

struct BuiltClient {
    harness: Harness,
    client: Arc<dyn ModelClient>,
    provider_display: String,
    provider_id: Option<String>,
    model: Option<String>,
}

async fn build_harness_and_client(
    opts: BuildOpts,
    workspace: Arc<Workspace>,
) -> Result<BuiltClient> {
    let skill_dirs = default_skill_dirs(workspace.workdir());
    let tools: Vec<Box<dyn Tool>> = default_tools(workspace, &skill_dirs);
    // 加载 ~/.hebbian/hooks.json 把外部 hook 接进 HookManager（架构 §4.8 / Step 11）。
    let hook_data_dir = opts.data_dir.clone().unwrap_or_else(default_data_dir);
    let hook_cfg = agent_core::hooks::load_hooks_config(&hook_data_dir);
    let external_hooks = agent_core::hooks::ExternalHook::from_config(hook_cfg);
    let harness = Harness::new(tools, HookManager::new(external_hooks));

    if opts.mock {
        return Ok(BuiltClient {
            harness,
            client: Arc::new(mock_provider::MockClient::new()),
            provider_display: "mock·mock".to_string(),
            provider_id: None,
            model: None,
        });
    }

    let data_dir = opts.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir).ok();

    let file =
        model_gateway::config::load(&data_dir).map_err(|e| anyhow!("加载 providers：{e}"))?;
    let selection =
        resolve_runtime_selection(&file, opts.provider.as_deref(), opts.model.as_deref())?;
    let provider = selection.provider;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(&data_dir, provider)
        .await
        .map_err(|e| anyhow!("刷新 token 失败：{e}"))?;
    let model = selection.model;
    let provider_name = provider.name.clone();
    let provider_id = provider.id.clone();

    let inner = model_gateway::build_client(provider).map_err(|e| anyhow!("build client：{e}"))?;
    let client: Arc<dyn ModelClient> = Arc::new(NamedModelClient::with_reasoning(
        inner,
        model.clone(),
        opts.reasoning.clone(),
    ));
    Ok(BuiltClient {
        harness,
        client,
        provider_display: format!("{provider_name}·{model}"),
        provider_id: Some(provider_id),
        model: Some(model),
    })
}

struct RuntimeSelection {
    provider: Provider,
    model: String,
}

fn resolve_runtime_selection(
    file: &ProvidersFile,
    provider_arg: Option<&str>,
    model_arg: Option<&str>,
) -> Result<RuntimeSelection> {
    let (provider_key, model_from_provider_arg) = match provider_arg {
        Some(arg) => parse_provider_model_target(arg),
        None => (
            file.default_provider_id.as_deref().ok_or_else(|| {
                anyhow!("未指定 --provider 且无默认 provider（先在 desktop 配一个）")
            })?,
            None,
        ),
    };
    let provider = find_provider(file, provider_key)?.clone();
    let model = model_arg
        .map(str::to_string)
        .or_else(|| model_from_provider_arg.map(str::to_string))
        .or_else(|| provider.default_model.clone())
        .ok_or_else(|| {
            anyhow!(
                "未指定 model。可用 --provider {}/<model_id> 或 --model <model_id>",
                provider.name
            )
        })?;
    Ok(RuntimeSelection { provider, model })
}

fn parse_provider_model_target(target: &str) -> (&str, Option<&str>) {
    target
        .rsplit_once('/')
        .map(|(provider, model)| (provider, Some(model)))
        .unwrap_or((target, None))
}

fn find_provider<'a>(file: &'a ProvidersFile, key: &str) -> Result<&'a Provider> {
    file.providers
        .iter()
        .find(|provider| provider.id == key || provider.name == key)
        .ok_or_else(|| anyhow!("provider 不存在：{key}"))
}

fn handle_providers_command(command: &str, core: &dyn CoreClient) -> Result<()> {
    match command {
        "list" => list_providers(core),
        other => Err(anyhow!("未知 --providers 命令：{other}（目前支持 list）")),
    }
}

fn list_providers(core: &dyn CoreClient) -> Result<()> {
    let file = core.list_providers().map_err(|e| anyhow!("加载 providers：{e}"))?;
    if file.providers.is_empty() {
        println!("No providers configured.");
        return Ok(());
    }

    for provider in &file.providers {
        let default_provider = file.default_provider_id.as_deref() == Some(provider.id.as_str());
        let marker = if default_provider { "*" } else { " " };
        let default_model = provider.default_model.as_deref().unwrap_or("-");
        println!(
            "{marker} {} ({}) · default={}",
            provider.name, provider.id, default_model
        );
        for model in &provider.models {
            let model_marker = if provider.default_model.as_deref() == Some(model.as_str()) {
                "*"
            } else {
                "-"
            };
            println!("    {model_marker} {}/{}", provider.name, model);
        }
        if provider.models.is_empty() && provider.default_model.is_some() {
            println!("    * {}/{}", provider.name, default_model);
        }
    }
    Ok(())
}

fn set_default_provider_model(core: &dyn CoreClient, target: &str) -> Result<()> {
    let (provider_key, model) = parse_provider_model_target(target);
    let model = model.ok_or_else(|| anyhow!("--provider set 需要格式：name/model_id"))?;
    let mut file = core.list_providers().map_err(|e| anyhow!("加载 providers：{e}"))?;
    let provider = file
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_key || provider.name == provider_key)
        .ok_or_else(|| anyhow!("provider 不存在：{provider_key}"))?;
    provider.default_model = Some(model.to_string());
    let provider_id = provider.id.clone();
    let provider_name = provider.name.clone();
    file.default_provider_id = Some(provider_id);
    core.save_providers(file)
        .map_err(|e| anyhow!("保存 providers：{e}"))?;
    println!("Default provider set to {provider_name}·{model}");
    Ok(())
}

/// 共享数据目录（架构 §6.1 / 决策 D10）：`~/.hebbian/`。
///
/// 启动时检测 Tauri bundle 老路径（dirs::data_dir()/dev.ricardo.hebbian）若存在，
/// 自动迁移到新路径并打 info log。完整逻辑见
/// [`agent_core::storage::default_data_dir`]。
fn default_data_dir() -> PathBuf {
    agent_core::storage::default_data_dir()
}

/// 给 sessions::create_with_source 之后初始化 session 目录布局。
fn ensure_session_layout(
    data_dir: &std::path::Path,
    session: &agent_core::storage::sessions::Session,
) -> common::AppResult<()> {
    use agent_core::storage::sessions_dir;
    sessions_dir::ensure_session_dirs(data_dir, &session.id)?;
    sessions_dir::save_meta(
        data_dir,
        &sessions_dir::SessionDirMeta {
            session_id: session.id.clone(),
            created_at: session.created_at,
            agent: session.prompt_id.clone().unwrap_or_default(),
            workdir: session.workdir.clone(),
            provider: session.provider_id.clone(),
            model: session.model.clone(),
            last_interrupted_at: None,
        },
    )?;
    Ok(())
}

/// 把 model id + reasoning 配置注入每次请求（与 desktop 的 ModelWithName 同思路）
struct NamedModelClient {
    inner: DynModelClient,
    model: String,
    reasoning: Option<ReasoningConfig>,
}

impl NamedModelClient {
    fn new(inner: DynModelClient, model: String) -> Self {
        Self {
            inner,
            model,
            reasoning: None,
        }
    }

    fn with_reasoning(
        inner: DynModelClient,
        model: String,
        reasoning: Option<ReasoningConfig>,
    ) -> Self {
        Self {
            inner,
            model,
            reasoning,
        }
    }

    fn patch(&self, mut req: ModelRequest) -> ModelRequest {
        req.model = self.model.clone();
        if req.reasoning.is_none() {
            req.reasoning = self.reasoning.clone();
        }
        req
    }
}

#[async_trait]
impl ModelClient for NamedModelClient {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }
    fn supports_streaming_tools(&self) -> bool {
        self.inner.supports_streaming_tools()
    }
    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        self.inner.complete(self.patch(req), cancel).await
    }
    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        self.inner.stream(self.patch(req), cancel, on_event).await
    }
}
