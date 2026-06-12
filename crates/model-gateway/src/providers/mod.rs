pub mod anthropic;
pub mod deepseek;
pub mod gemini;
pub mod openai;

use crate::config::{AuthMode, Provider, ProviderKind};
use crate::types::ModelError;
use bytes::Bytes;
use common::{runtime as cancellation, CancelFlag};
use futures_util::{Stream, StreamExt};
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
    // 用 connect_timeout + read_timeout 替代 total timeout：
    // - `.timeout()` 是「整个请求」（含 stream body 接收）的总时长，长 SSE 流（thinking
    //   模式 / 多工具轮 / 长上下文）很容易超过 120s 触发"error decoding response body"。
    // - `.connect_timeout()`：握手 + TLS 阶段超时（避免 DNS/TCP 卡死）。
    // - `.read_timeout()`（reqwest 0.12+）：「两个 chunk 之间」的最大空闲时间，给 SSE
    //   足够空间——只要服务端持续在 push token 就不会触发。
    reqwest::Client::builder()
        .user_agent("Hebbian/0.1")
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(180))
        .pool_idle_timeout(Duration::from_secs(90))
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
            // anthropic-beta 末两项 cache-diagnosis-* / server-side-fallback-* 给 body 里的
            // diagnostics / fallbacks 字段开 schema——字段与 beta 必须成对，缺则服务端 400。
            req = req
                .bearer_auth(&provider.api_key)
                .header("anthropic-version", "2023-06-01")
                .header(
                    "anthropic-beta",
                    "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,context-management-2025-06-27,prompt-caching-scope-2026-01-05,extended-cache-ttl-2025-04-11,effort-2025-11-24,cache-diagnosis-2026-04-07,server-side-fallback-2026-06-01",
                )
                .header("user-agent", "claude-cli/2.1.170 (external, cli)")
                .header("x-app", "cli")
                .header("anthropic-dangerous-direct-browser-access", "true")
                .header("x-stainless-lang", "js")
                .header("x-stainless-package-version", "0.94.0")
                .header("x-stainless-os", "MacOS")
                .header("x-stainless-arch", "arm64")
                .header("x-stainless-runtime", "node")
                .header("x-stainless-runtime-version", "v24.3.0")
                .header("x-stainless-retry-count", "0")
                .header("x-stainless-timeout", "3000");
        }
        (ProviderKind::Anthropic, _) => {
            req = req
                .header("x-api-key", &provider.api_key)
                .header("anthropic-version", "2023-06-01")
                .header(
                    "anthropic-beta",
                    "claude-code-20250219,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,context-management-2025-06-27,prompt-caching-scope-2026-01-05,extended-cache-ttl-2025-04-11,effort-2025-11-24",
                )
                .header("user-agent", "claude-cli/2.1.170 (external, cli)")
                .header("x-app", "cli")
                .header("anthropic-dangerous-direct-browser-access", "true")
                .header("x-stainless-lang", "js")
                .header("x-stainless-package-version", "0.94.0")
                .header("x-stainless-os", "MacOS")
                .header("x-stainless-arch", "arm64")
                .header("x-stainless-runtime", "node")
                .header("x-stainless-runtime-version", "v24.3.0")
                .header("x-stainless-retry-count", "0")
                .header("x-stainless-timeout", "3000");
        }
        (ProviderKind::Gemini, AuthMode::OauthGeminiCli) => {
            req = req
                .bearer_auth(&provider.api_key)
                .header("x-goog-api-client", "GeminiCLI/1.0");
        }
        (ProviderKind::Gemini, _) => {}
        (ProviderKind::Deepseek, _) => {
            // chat.deepseek.com web 协议：Bearer + 客户端指纹头（与 ds2api/internal/deepseek/protocol/constants.go 对齐）
            req = req
                .bearer_auth(&provider.api_key)
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .header("accept-charset", "UTF-8")
                .header("Host", "chat.deepseek.com")
                .header("User-Agent", "DeepSeek/2.0.4 Android/35")
                .header("x-client-platform", "android")
                .header("x-client-version", "2.0.4")
                .header("x-client-locale", "zh_CN");
        }
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

        // 用 select! 竞争：cancel 先到就立即返回，不等 HTTP 响应。
        let result = tokio::select! {
            result = op() => result,
            _ = wait_for_cancel(cancel.clone()) => return Err(ModelError::Cancelled),
        };
        match result {
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
        ModelError::Json(_)
        | ModelError::Cancelled
        | ModelError::Suspended
        | ModelError::Other(_) => false,
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

    /// CC OAuth 请求体带 fallbacks / diagnostics 顶层字段，服务端必须看到对应的
    /// enabling beta 才认这两个字段——否则 400 "Extra inputs are not permitted"。
    /// 这条把「字段与 beta 成对」固化：删掉任一 beta 都会 fail。
    #[test]
    fn oauth_anthropic_beta_carries_cc_field_enabling_betas() {
        use crate::config::{AuthMode, Provider, ProviderKind};

        let provider = Provider {
            id: "p".into(),
            name: "p".into(),
            kind: ProviderKind::Anthropic,
            enabled: true,
            auth_mode: AuthMode::OauthClaudeCode,
            base_url: "https://api.anthropic.com".into(),
            api_key: "sk-test".into(),
            refresh_token: None,
            token_expires_at: None,
            account_id: Some("acct".into()),
            extra_headers: Default::default(),
            models: vec![],
            fetched_models: None,
            model_context_windows: Default::default(),
            default_model: None,
            title_gen_enabled: false,
            title_gen_model: None,
            judge_provider_id: None,
            judge_model: None,
            claude_code_compat: false,
        };

        let http = reqwest::Client::new();
        let built = apply_auth(http.post(&provider.base_url), &provider)
            .build()
            .unwrap();
        let beta = built
            .headers()
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        assert!(
            beta.contains("cache-diagnosis-2026-04-07"),
            "diagnostics 字段缺 enabling beta: {beta}"
        );
        assert!(
            beta.contains("server-side-fallback-2026-06-01"),
            "fallbacks 字段缺 enabling beta: {beta}"
        );
    }
}
