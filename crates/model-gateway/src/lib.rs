pub mod auth;
pub mod client;
pub mod config;
pub mod context_window;
pub mod discovery;
pub mod health;
pub mod instrument;
pub mod model_io;
pub mod protocols;
pub mod providers;
pub mod types;
pub mod usage;

use client::DynModelClient;
use config::{Provider, ProviderKind};
use instrument::InstrumentedClient;
use types::ModelError;

pub fn build_client(provider: Provider) -> Result<DynModelClient, ModelError> {
    build_client_inner(provider, None)
}

/// 带 data_dir 版：Anthropic 分支启用 401 自愈刷新（长跑 / 长 HITL 审批后 token 失效会
/// 自动续期重试）。主对话路径（desktop/cli/web 的模型请求）用这个；健康检查 / 标题生成 /
/// 测试用上面的 [`build_client`]（不需要长跑续期）。
pub fn build_client_with_data_dir(
    provider: Provider,
    data_dir: std::path::PathBuf,
) -> Result<DynModelClient, ModelError> {
    build_client_inner(provider, Some(data_dir))
}

fn build_client_inner(
    provider: Provider,
    data_dir: Option<std::path::PathBuf>,
) -> Result<DynModelClient, ModelError> {
    use std::sync::Arc;
    let (system, inner): (&'static str, Arc<dyn client::ModelClient>) = match provider.kind {
        ProviderKind::Openai => (
            "openai",
            Arc::new(providers::openai::OpenAiClient::new(provider)?),
        ),
        ProviderKind::Anthropic => (
            "anthropic",
            Arc::new(providers::anthropic::AnthropicClient::with_data_dir(
                provider,
                data_dir.clone(),
            )?),
        ),
        ProviderKind::Gemini => (
            "gemini",
            Arc::new(providers::gemini::GeminiClient::new(provider)?),
        ),
        ProviderKind::Deepseek => (
            "deepseek",
            Arc::new(providers::deepseek::DeepseekClient::new(provider)?),
        ),
    };
    Ok(Arc::new(InstrumentedClient::new(inner, system, data_dir)))
}
