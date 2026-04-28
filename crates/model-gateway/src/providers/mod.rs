pub mod anthropic;
pub mod gemini;
pub mod openai;

use crate::config::{AuthMode, Provider, ProviderKind};
use crate::types::ModelError;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use platform::{runtime as cancellation, CancelFlag};
use reqwest::RequestBuilder;
use std::time::Duration;

pub fn build_http_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Hebbian/0.1")
        .timeout(Duration::from_secs(120))
        .build()
}

pub fn apply_auth(req: RequestBuilder, provider: &Provider) -> RequestBuilder {
    let mut req = req;
    match (provider.kind, provider.auth_mode) {
        (ProviderKind::Openai, AuthMode::OauthCodex) => {
            req = req
                .bearer_auth(&provider.api_key)
                .header("originator", "hebbian");
            if let Some(acc) = &provider.account_id {
                req = req.header("ChatGPT-Account-Id", acc.as_str());
            }
        }
        (ProviderKind::Openai, _) => {
            req = req.bearer_auth(&provider.api_key);
        }
        (ProviderKind::Anthropic, AuthMode::OauthClaudeCode) => {
            req = req
                .bearer_auth(&provider.api_key)
                .header("anthropic-version", "2023-06-01")
                .header(
                    "anthropic-beta",
                    "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14",
                )
                .header("user-agent", "claude-cli/2.1.22 (external, cli)")
                .header("x-app", "cli")
                .header("anthropic-dangerous-direct-browser-access", "true")
                .header("x-stainless-lang", "js")
                .header("x-stainless-package-version", "0.70.0")
                .header("x-stainless-os", "Linux")
                .header("x-stainless-arch", "arm64")
                .header("x-stainless-runtime", "node")
                .header("x-stainless-runtime-version", "v24.13.0")
                .header("x-stainless-retry-count", "0")
                .header("x-stainless-timeout", "600");
        }
        (ProviderKind::Anthropic, _) => {
            req = req
                .header("x-api-key", &provider.api_key)
                .header("anthropic-version", "2023-06-01");
        }
        (ProviderKind::Gemini, AuthMode::OauthGeminiCli) => {
            req = req
                .bearer_auth(&provider.api_key)
                .header("x-goog-api-client", "GeminiCLI/1.0");
        }
        (ProviderKind::Gemini, _) => {}
    }
    for (k, v) in &provider.extra_headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req
}

pub(crate) async fn next_stream_chunk_or_cancel<S>(
    stream: &mut S,
    cancel: &CancelFlag,
) -> Result<Option<Bytes>, ModelError>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    if cancellation::is_cancelled(cancel) {
        return Err(ModelError::Cancelled);
    }

    tokio::select! {
        chunk = stream.next() => chunk.transpose().map_err(ModelError::from),
        _ = wait_for_cancel(cancel.clone()) => Err(ModelError::Cancelled),
    }
}

async fn wait_for_cancel(cancel: CancelFlag) {
    while !cancellation::is_cancelled(&cancel) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModelError;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn next_stream_chunk_or_cancel_returns_when_cancelled_before_next_chunk() {
        let cancel = Arc::new(AtomicBool::new(true));
        let mut stream = futures_util::stream::pending::<Result<bytes::Bytes, reqwest::Error>>();

        let result = next_stream_chunk_or_cancel(&mut stream, &cancel).await;

        assert!(matches!(result, Err(ModelError::Cancelled)));
        assert!(cancel.load(Ordering::SeqCst));
    }
}
