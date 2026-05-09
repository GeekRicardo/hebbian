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
    hooks::HookManager,
    tools::{default_tools, skill::default_skill_dirs, Tool},
    workspace::Workspace,
    Harness, Recorder,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use clap::Parser;
use model_gateway::config::{Provider, ProvidersFile};
use model_gateway::{
    client::{DynModelClient, ModelClient},
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use platform::reasoning::{ReasoningConfig, ReasoningEffort};
use platform::CancelFlag;

mod mock_provider;
mod render;
mod session;

use session::{CliSession, ConvoInput};

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

    /// 不把事件流落盘（默认会写到 data_dir/sessions/<ts>-<uuid>.jsonl）
    #[arg(long)]
    no_record: bool,

    /// 开启 thinking / reasoning（claude-opus-4* / claude-sonnet-4* / gpt-5* / o-series）
    #[arg(long)]
    thinking: bool,

    /// 思考强度 low|medium|high|extra（默认 extra）
    #[arg(long, value_parser = parse_effort, default_value = "extra")]
    effort: ReasoningEffort,

    /// Anthropic 1M 上下文 beta header
    #[arg(long = "long-context")]
    long_context: bool,
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

    // 不再用 --workdir：直接用 CLI 进程当前目录
    let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // allowed_dirs：CLI 参数优先，否则用全局 settings 默认
    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);

    if let Some(command) = &cli.providers {
        return handle_providers_command(command, &data_dir);
    }
    if let Some(target) = &cli.provider_set {
        return set_default_provider_model(&data_dir, target);
    }

    let settings = platform::config::settings::load(&data_dir);
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
            provider: cli.provider.clone(),
            model: cli.model.clone(),
            data_dir: Some(data_dir.clone()),
            reasoning,
        },
        workspace.clone(),
    )
    .await?;

    let recorder = if cli.no_record {
        None
    } else {
        Some(Recorder::open(rollout_path(&data_dir)).await?)
    };

    let mut session = CliSession::new(
        built.harness,
        built.client,
        cli.system.clone(),
        enabled_tools,
        cli.auto_approve,
        workspace,
        built.provider_display,
        recorder,
    );

    match (cli.prompt, cli.json) {
        (None, None) => session.run_loop().await,
        (Some(prompt), None) => session.run_single(prompt).await,
        (None, Some(json_arg)) => {
            let raw = read_json_arg(&json_arg)?;
            let convo: ConvoInput =
                serde_json::from_str(&raw).map_err(|e| anyhow!("无效 JSON：{e}"))?;
            session.run_with_history(convo.messages).await
        }
        (Some(_), Some(_)) => unreachable!("clap 通过 conflicts_with 已阻止"),
    }
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
}

async fn build_harness_and_client(
    opts: BuildOpts,
    workspace: Arc<Workspace>,
) -> Result<BuiltClient> {
    let skill_dirs = default_skill_dirs(workspace.workdir());
    let tools: Vec<Box<dyn Tool>> = default_tools(workspace, &skill_dirs);
    let harness = Harness::new(tools, HookManager::empty());

    if opts.mock {
        return Ok(BuiltClient {
            harness,
            client: Arc::new(mock_provider::MockClient::new()),
            provider_display: "mock·mock".to_string(),
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

fn handle_providers_command(command: &str, data_dir: &std::path::Path) -> Result<()> {
    match command {
        "list" => list_providers(data_dir),
        other => Err(anyhow!("未知 --providers 命令：{other}（目前支持 list）")),
    }
}

fn list_providers(data_dir: &std::path::Path) -> Result<()> {
    let file = model_gateway::config::load(data_dir).map_err(|e| anyhow!("加载 providers：{e}"))?;
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

fn set_default_provider_model(data_dir: &std::path::Path, target: &str) -> Result<()> {
    let (provider_key, model) = parse_provider_model_target(target);
    let model = model.ok_or_else(|| anyhow!("--provider set 需要格式：name/model_id"))?;
    let mut file =
        model_gateway::config::load(data_dir).map_err(|e| anyhow!("加载 providers：{e}"))?;
    let provider = file
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_key || provider.name == provider_key)
        .ok_or_else(|| anyhow!("provider 不存在：{provider_key}"))?;
    provider.default_model = Some(model.to_string());
    let provider_id = provider.id.clone();
    let provider_name = provider.name.clone();
    file.default_provider_id = Some(provider_id);
    model_gateway::config::save(data_dir, &file).map_err(|e| anyhow!("保存 providers：{e}"))?;
    println!("Default provider set to {provider_name}·{model}");
    Ok(())
}

/// 本次 CLI 启动的事件日志路径：`<data_dir>/sessions/<ts>-<uuid>.jsonl`。
fn rollout_path(data_dir: &std::path::Path) -> PathBuf {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let uuid = uuid::Uuid::new_v4();
    data_dir
        .join("sessions")
        .join(format!("rollout-{ts}-{uuid}.jsonl"))
}

/// 与 desktop 共享同一 data_dir（Tauri bundle id：dev.ricardo.hebbian）
fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("dev.ricardo.hebbian"))
        .unwrap_or_else(|| PathBuf::from(".hebbian"))
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
