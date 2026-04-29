pub mod anthropic;
pub mod gemini;
pub mod openai;

use crate::config::{AuthMode, Provider, ProviderKind};
use crate::types::ModelError;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use platform::{runtime as cancellation, CancelFlag};
#[cfg(not(test))]
use rand::Rng;
use reqwest::RequestBuilder;
use std::time::Duration;

const DEFAULT_MAX_RETRIES: u32 = 4;
#[cfg(not(test))]
const BASE_RETRY_DELAY: Duration = Duration::from_millis(500);
#[cfg(not(test))]
const MAX_RETRY_DELAY: Duration = Duration::from_secs(8);

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

pub(crate) async fn retry_request<T, Op, Fut>(
    cancel: CancelFlag,
    mut op: Op,
) -> Result<T, ModelError>
where
    Op: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ModelError>>,
{
    let mut attempt = 0u32;
    loop {
        if cancellation::is_cancelled(&cancel) {
            return Err(ModelError::Cancelled);
        }

        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                attempt += 1;
                if attempt > DEFAULT_MAX_RETRIES || !is_retryable_model_error(&err) {
                    return Err(err);
                }

                sleep_or_cancel(retry_delay(attempt), cancel.clone()).await?;
            }
        }
    }
}

fn is_retryable_model_error(err: &ModelError) -> bool {
    match err {
        ModelError::Http { status, body } => {
            matches!(*status, 408 | 409 | 429 | 500..=599)
                || body.contains("\"type\":\"overloaded_error\"")
                || body.contains("overloaded_error")
        }
        ModelError::Request(err) => err.is_connect() || err.is_timeout(),
        ModelError::Json(_) | ModelError::Cancelled | ModelError::Other(_) => false,
    }
}

fn retry_delay(attempt: u32) -> Duration {
    #[cfg(test)]
    {
        let _ = attempt;
        return Duration::from_millis(1);
    }

    #[cfg(not(test))]
    {
        let exponent = attempt.saturating_sub(1).min(10);
        let base_ms = (BASE_RETRY_DELAY.as_millis() as u64)
            .saturating_mul(2u64.saturating_pow(exponent))
            .min(MAX_RETRY_DELAY.as_millis() as u64);
        let jitter_ms = rand::thread_rng().gen_range(0..=(base_ms / 4).max(1));
        Duration::from_millis(base_ms + jitter_ms)
    }
}

async fn sleep_or_cancel(delay: Duration, cancel: CancelFlag) -> Result<(), ModelError> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        _ = wait_for_cancel(cancel) => Err(ModelError::Cancelled),
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

    #[tokio::test]
    async fn retry_request_retries_transient_http_errors() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_for_op = Arc::clone(&attempts);
        let cancel = Arc::new(AtomicBool::new(false));

        let result = retry_request(cancel, move || {
            let attempts = Arc::clone(&attempts_for_op);
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err(ModelError::Http {
                        status: 500,
                        body: "server busy".to_string(),
                    })
                } else {
                    Ok("ok")
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_request_does_not_retry_client_errors() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_for_op = Arc::clone(&attempts);
        let cancel = Arc::new(AtomicBool::new(false));

        let result = retry_request(cancel, move || {
            let attempts = Arc::clone(&attempts_for_op);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(ModelError::Http {
                    status: 400,
                    body: "unsupported model".to_string(),
                })
            }
        })
        .await;

        assert!(matches!(result, Err(ModelError::Http { status: 400, .. })));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
