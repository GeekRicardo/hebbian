use std::sync::{atomic::AtomicBool, Arc};

use serde::Serialize;

use crate::{
    build_client,
    config::Provider,
    types::{ModelError, ModelRequest, ModelResponse, TranscriptEntry, Usage, UserEntry},
};

const PROBE_PROMPT: &str = "hi";
const PROBE_MAX_TOKENS: u32 = 32;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ProviderModelTestResult {
    pub model: String,
    pub prompt: String,
    pub response_preview: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub fn build_probe_request(model: impl Into<String>) -> ModelRequest {
    ModelRequest {
        model: model.into(),
        system: None,
        entries: vec![TranscriptEntry::User(UserEntry::text(PROBE_PROMPT))],
        tools: Vec::new(),
        max_tokens: PROBE_MAX_TOKENS,
        reasoning: None,
        compact_prompt_cache_key: None,
        meta: Default::default(),
    }
}

pub async fn test_provider_model(
    provider: Provider,
    model: String,
) -> Result<ProviderModelTestResult, ModelError> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err(ModelError::Other("请先填写要测试的模型".into()));
    }
    if provider.api_key.trim().is_empty() {
        return Err(ModelError::Other("请先填写 API Key 或 Access Token".into()));
    }

    let client = build_client(provider)?;
    let response = client
        .complete(
            build_probe_request(model.clone()),
            Arc::new(AtomicBool::new(false)),
        )
        .await?;

    let (text, usage) = response_parts(response);
    Ok(ProviderModelTestResult {
        model,
        prompt: PROBE_PROMPT.to_string(),
        response_preview: text.chars().take(200).collect(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
    })
}

fn response_parts(response: ModelResponse) -> (String, Usage) {
    match response {
        ModelResponse::Done { text, usage, .. } => (text, usage),
        ModelResponse::ToolCalls { text, usage, .. } => (text, usage),
    }
}
