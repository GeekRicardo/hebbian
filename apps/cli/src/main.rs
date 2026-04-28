//! hebbian-cli
//!
//! 一个轻量 surface，主要用途：
//! 1. 端到端验证 Hebbian 协议（事件流、HITL）—— 配合 scripts/test.py
//! 2. 在没有 GUI 时跑一次模型对话
//!
//! 输出协议：stdout 每行一个 `protocol::Event` 的 JSON。stderr 用于人类日志。
//! 输入协议（interactive 模式）：stdin 每行一个 `protocol::Submission` 的 JSON。

use std::io::{BufRead, Write};

fn emit_ndjson(event: &protocol::Event) {
    if let Ok(line) = serde_json::to_string(event) {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        let _ = writeln!(handle, "{line}");
        let _ = handle.flush();
    }
}
use std::path::PathBuf;
use std::sync::Arc;

use agent_core::{
    context::transcript::Transcript,
    definition::AgentDefinition,
    harness::RunParams,
    hooks::HookManager,
    tools::{default_tools, permissions::PermissionGate, Tool},
    Harness,
};
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use platform::{runtime, CancelFlag};
use protocol::{AgentRef, Op, Submission};
mod mock_provider;

#[derive(Parser, Debug)]
#[command(name = "hebbian-cli", about = "Hebbian agent CLI / protocol harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 一次性发起 prompt，输出 NDJSON 事件流后退出
    Run {
        /// 用户输入
        prompt: String,

        /// 使用 mock provider（不调真实模型，用于协议测试）
        #[arg(long)]
        mock: bool,

        /// mock 模式下，模型是否请求一次工具调用（用于测试 ToolCall*/HITL 路径）
        #[arg(long, default_value_t = false)]
        mock_tool_call: bool,

        /// mock 工具是否需要审批（用于测试 PermissionRequested/Resolved）
        #[arg(long, default_value_t = false)]
        mock_needs_approval: bool,

        /// 真实 provider id（与 desktop 共享 ~/.hebbian 数据）
        #[arg(long)]
        provider: Option<String>,

        /// 模型 id
        #[arg(long)]
        model: Option<String>,

        /// 系统提示
        #[arg(long)]
        system: Option<String>,

        /// 启用的工具，逗号分隔
        #[arg(long, value_delimiter = ',')]
        tools: Vec<String>,

        /// data dir（默认 ~/.hebbian-cli）
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },

    /// 交互模式：从 stdin 读 Submission（每行 JSON），向 stdout 写 Event
    Interactive {
        /// 自动批准所有审批请求（测试 happy path 用）
        #[arg(long)]
        auto_approve: bool,

        #[arg(long)]
        mock: bool,

        #[arg(long, default_value_t = false)]
        mock_tool_call: bool,

        #[arg(long, default_value_t = false)]
        mock_needs_approval: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run {
            prompt,
            mock,
            mock_tool_call,
            mock_needs_approval,
            provider,
            model,
            system,
            tools,
            data_dir,
        } => {
            run_once(RunOpts {
                prompt,
                mock,
                mock_tool_call,
                mock_needs_approval,
                provider,
                model,
                system,
                tools,
                data_dir,
            })
            .await
        }
        Cmd::Interactive {
            auto_approve,
            mock,
            mock_tool_call,
            mock_needs_approval,
        } => {
            interactive(InteractiveOpts {
                auto_approve,
                mock,
                mock_tool_call,
                mock_needs_approval,
            })
            .await
        }
    }
}

struct RunOpts {
    prompt: String,
    mock: bool,
    mock_tool_call: bool,
    mock_needs_approval: bool,
    provider: Option<String>,
    model: Option<String>,
    system: Option<String>,
    tools: Vec<String>,
    data_dir: Option<PathBuf>,
}

async fn run_once(opts: RunOpts) -> Result<()> {
    let (harness, client) = build_harness_and_client(BuildHarnessOpts {
        mock: opts.mock,
        mock_tool_call: opts.mock_tool_call,
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        data_dir: opts.data_dir.clone(),
    })
    .await?;

    let mut definition = AgentDefinition::default();
    if opts.mock_needs_approval {
        definition.permission_policy.always_ask =
            vec!["mock_tool".into(), "web_search".into(), "web_fetch".into()];
    }

    // 必须在 spawn_run 之前 subscribe，否则错过 RunStarted
    let mut events_rx = harness.subscribe();

    let cancel: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate = Arc::new(PermissionGate::new(definition.permission_policy.clone()));

    let mut transcript = Transcript::new(opts.system.clone());
    transcript.push_user(opts.prompt.clone(), Vec::new());

    let run_id = harness.spawn_run(
        client,
        RunParams {
            agent: AgentRef::new(&definition.id),
            gate,
            transcript,
            enabled_tools: opts.tools.clone(),
            compaction_policy: definition.compaction_policy.clone(),
            stream: true,
            cancel,
            parent: None,
        },
    );

    // 消费事件流直到本 run 结束
    while let Ok(event) = events_rx.recv().await {
        if event.run_id != run_id {
            continue;
        }
        emit_ndjson(&event);
        if matches!(
            event.payload,
            protocol::EventPayload::RunFinished { .. }
                | protocol::EventPayload::RunFailed { .. }
                | protocol::EventPayload::RunCancelled
        ) {
            return Ok(());
        }
    }

    Err(anyhow!("事件流意外关闭"))
}

struct InteractiveOpts {
    auto_approve: bool,
    mock: bool,
    mock_tool_call: bool,
    mock_needs_approval: bool,
}

async fn interactive(opts: InteractiveOpts) -> Result<()> {
    let (harness, client) = build_harness_and_client(BuildHarnessOpts {
        mock: opts.mock,
        mock_tool_call: opts.mock_tool_call,
        provider: None,
        model: None,
        data_dir: None,
    })
    .await?;
    let harness = Arc::new(harness);

    // stdout pump
    let mut events_rx = harness.subscribe();
    let stdout_task = tokio::spawn(async move {
        while let Ok(event) = events_rx.recv().await {
            emit_ndjson(&event);
        }
    });

    // auto_approve：监听所有 run 的审批事件，自动 AllowOnce
    let auto_approve_handle = if opts.auto_approve {
        let harness = harness.clone();
        let mut events_rx = harness.subscribe();
        Some(tokio::spawn(async move {
            while let Ok(event) = events_rx.recv().await {
                if let protocol::EventPayload::PermissionRequested { request_id, .. } =
                    &event.payload
                {
                    let _ = harness.resolve_permission(
                        &event.run_id,
                        request_id,
                        protocol::ApprovalDecision::AllowOnce,
                    );
                }
            }
        }))
    } else {
        None
    };

    let mut definition = AgentDefinition::default();
    if opts.mock_needs_approval {
        definition.permission_policy.always_ask =
            vec!["mock_tool".into(), "web_search".into(), "web_fetch".into()];
    }

    let enabled_tools_arg: Vec<String> = if opts.mock_tool_call {
        vec!["mock_tool".into()]
    } else {
        Vec::new()
    };

    let stdin = std::io::stdin();
    let stdin = stdin.lock();
    let mut active_runs: Vec<protocol::RunId> = Vec::new();

    for line in stdin.lines() {
        let line = line.context("read stdin")?;
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let submission: Submission = match serde_json::from_str(&line) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[cli] 无效 Submission JSON: {e}");
                continue;
            }
        };

        match submission.op {
            Op::StartRun {
                input,
                turn_overrides,
                ..
            } => {
                let cancel: CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let stream = turn_overrides
                    .as_ref()
                    .and_then(|o| o.stream)
                    .unwrap_or(true);
                let gate = Arc::new(PermissionGate::new(definition.permission_policy.clone()));
                let mut transcript = Transcript::new(None);
                transcript.push_user(input.text.clone(), Vec::new());

                let run_id = harness.spawn_run(
                    client.clone(),
                    RunParams {
                        agent: AgentRef::new(&definition.id),
                        gate,
                        transcript,
                        enabled_tools: enabled_tools_arg.clone(),
                        compaction_policy: definition.compaction_policy.clone(),
                        stream,
                        cancel,
                        parent: None,
                    },
                );
                active_runs.push(run_id);
            }
            Op::Approve { .. } => {
                let _ = harness.submit(submission);
            }
            Op::Interrupt { run_id } => {
                let _ = harness.interrupt(&run_id);
            }
            other => {
                eprintln!("[cli] op 暂不支持: {other:?}");
            }
        }
    }

    // 等所有 run 都收到终止事件
    if !active_runs.is_empty() {
        let mut events_rx = harness.subscribe();
        let mut remaining: std::collections::HashSet<_> = active_runs.into_iter().collect();
        while !remaining.is_empty() {
            match events_rx.recv().await {
                Ok(event) if matches!(
                    event.payload,
                    protocol::EventPayload::RunFinished { .. }
                        | protocol::EventPayload::RunFailed { .. }
                        | protocol::EventPayload::RunCancelled
                ) =>
                {
                    remaining.remove(&event.run_id);
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    if let Some(h) = auto_approve_handle {
        h.abort();
    }
    stdout_task.abort();
    Ok(())
}

struct BuildHarnessOpts {
    mock: bool,
    mock_tool_call: bool,
    provider: Option<String>,
    model: Option<String>,
    data_dir: Option<PathBuf>,
}

async fn build_harness_and_client(
    opts: BuildHarnessOpts,
) -> Result<(Harness, Arc<dyn ModelClient>)> {
    let tools: Vec<Box<dyn Tool>> = default_tools();
    let harness = Harness::new(tools, HookManager::empty());

    if opts.mock {
        let client = Arc::new(mock_provider::MockClient::new(opts.mock_tool_call));
        return Ok((harness, client));
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
        .ok_or_else(|| anyhow!("未指定 --provider 且无默认 provider"))?;
    let provider = model_gateway::config::get(&data_dir, &provider_id)
        .map_err(|e| anyhow!("加载 provider 失败: {e}"))?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(&data_dir, provider)
        .await
        .map_err(|e| anyhow!("刷新 token 失败: {e}"))?;
    let model = opts
        .model
        .or_else(|| provider.default_model.clone())
        .ok_or_else(|| anyhow!("未指定 --model 且 provider 无默认 model"))?;

    let inner = model_gateway::build_client(provider).map_err(|e| anyhow!("build client: {e}"))?;
    let client: Arc<dyn ModelClient> = Arc::new(NamedModelClient::new(inner, model));
    Ok((harness, client))
}

/// CLI 默认与 desktop 共享同一个 data_dir（Tauri bundle id：dev.ricardo.hebbian）。
/// 这样在 desktop 里配过的 provider / OAuth 凭据可以直接被 CLI 使用。
fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("dev.ricardo.hebbian"))
        .unwrap_or_else(|| PathBuf::from(".hebbian"))
}

// 让 model client 在每次请求里附上指定的 model id（与 desktop 的 ModelWithName 同思路）
use async_trait::async_trait;
use model_gateway::{
    client::{DynModelClient, ModelClient},
    types::{ModelError, ModelRequest, ModelResponse, ModelStreamEvent},
};

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

#[allow(dead_code)]
fn _force_runtime_link() {
    let _ = runtime::is_cancelled;
}
