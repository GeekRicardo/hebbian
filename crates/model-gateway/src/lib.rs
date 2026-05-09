pub mod auth;
pub mod client;
pub mod config;
pub mod context_window;
pub mod discovery;
pub mod health;
pub mod instrument;
pub mod protocols;
pub mod providers;
pub mod types;

use client::DynModelClient;
use config::{Provider, ProviderKind};
use instrument::InstrumentedClient;
use types::ModelError;

pub fn build_client(provider: Provider) -> Result<DynModelClient, ModelError> {
    use std::sync::Arc;
    let (system, inner): (&'static str, Arc<dyn client::ModelClient>) = match provider.kind {
        ProviderKind::Openai => (
            "openai",
            Arc::new(providers::openai::OpenAiClient::new(provider)?),
        ),
        ProviderKind::Anthropic => (
            "anthropic",
            Arc::new(providers::anthropic::AnthropicClient::new(provider)?),
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
    Ok(Arc::new(InstrumentedClient::new(inner, system)))
}
