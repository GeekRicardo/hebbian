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
    tools::{default_tools, Tool},
    Harness,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use clap::Parser;
use model_gateway::{
    client::{DynModelClient, ModelClient},
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};
use platform::CancelFlag;

mod mock_provider;
mod render;
mod session;

use session::{ConvoInput, Session};

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

    /// provider id（与 desktop 共享 data_dir）
    #[arg(long)]
    provider: Option<String>,

    /// 模型 id
    #[arg(long, short = 'm')]
    model: Option<String>,

    /// system prompt
    #[arg(long, short = 's')]
    system: Option<String>,

    /// 启用的工具，逗号分隔
    #[arg(long, value_delimiter = ',')]
    tools: Vec<String>,

    /// data dir（默认与 desktop 共享：~/Library/Application Support/dev.ricardo.hebbian/）
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// 收到 PermissionRequested 时自动允许
    #[arg(long, default_value_t = true)]
    auto_approve: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    let (harness, client) = build_harness_and_client(BuildOpts {
        mock: cli.mock,
        provider: cli.provider.clone(),
        model: cli.model.clone(),
        data_dir: cli.data_dir.clone(),
    })
    .await?;

    let mut session = Session::new(
        harness,
        client,
        cli.system.clone(),
        cli.tools.clone(),
        cli.auto_approve,
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

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
}

struct BuildOpts {
    mock: bool,
    provider: Option<String>,
    model: Option<String>,
    data_dir: Option<PathBuf>,
}

async fn build_harness_and_client(
    opts: BuildOpts,
) -> Result<(Harness, Arc<dyn ModelClient>)> {
    let tools: Vec<Box<dyn Tool>> = default_tools();
    let harness = Harness::new(tools, HookManager::empty());

    if opts.mock {
        return Ok((harness, Arc::new(mock_provider::MockClient::new())));
    }

    let data_dir = opts.data_dir.unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir).ok();

    let provider_id = opts
        .provider
        .or_else(|| {
            model_gateway::config::load(&data_dir)
                .ok()
                .and_then(|f| f.default_provider_id)
        })
        .ok_or_else(|| anyhow!("未指定 --provider 且无默认 provider（先在 desktop 配一个）"))?;
    let provider = model_gateway::config::get(&data_dir, &provider_id)
        .map_err(|e| anyhow!("加载 provider 失败：{e}"))?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(&data_dir, provider)
        .await
        .map_err(|e| anyhow!("刷新 token 失败：{e}"))?;
    let model = opts
        .model
        .or_else(|| provider.default_model.clone())
        .ok_or_else(|| anyhow!("未指定 --model 且 provider 无默认 model"))?;

    let inner = model_gateway::build_client(provider).map_err(|e| anyhow!("build client：{e}"))?;
    let client: Arc<dyn ModelClient> = Arc::new(NamedModelClient::new(inner, model));
    Ok((harness, client))
}

/// 与 desktop 共享同一 data_dir（Tauri bundle id：dev.ricardo.hebbian）
fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("dev.ricardo.hebbian"))
        .unwrap_or_else(|| PathBuf::from(".hebbian"))
}

/// 把 model id 注入每次请求（与 desktop 的 ModelWithName 同思路）
struct NamedModelClient {
    inner: DynModelClient,
    model: String,
}

impl NamedModelClient {
    fn new(inner: DynModelClient, model: String) -> Self {
        Self { inner, model }
    }
    fn patch(&self, mut req: ModelRequest) -> ModelRequest {
        req.model = self.model.clone();
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
