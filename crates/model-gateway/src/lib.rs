pub mod auth;
pub mod client;
pub mod config;
pub mod discovery;
pub mod health;
pub mod protocols;
pub mod providers;
pub mod types;

use client::DynModelClient;
use config::{Provider, ProviderKind};
use types::ModelError;

pub fn build_client(provider: Provider) -> Result<DynModelClient, ModelError> {
    use std::sync::Arc;
    match provider.kind {
        ProviderKind::Openai => Ok(Arc::new(providers::openai::OpenAiClient::new(provider)?)),
        ProviderKind::Anthropic => Ok(Arc::new(providers::anthropic::AnthropicClient::new(
            provider,
        )?)),
        ProviderKind::Gemini => Ok(Arc::new(providers::gemini::GeminiClient::new(provider)?)),
    }
}
