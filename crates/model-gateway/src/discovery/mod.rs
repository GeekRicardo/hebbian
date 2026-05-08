use crate::config::{AuthMode, Provider, ProviderKind};
use platform::AppError;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct FetchedModel {
    pub id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
}

const CODEX_OAUTH_MODELS: &[&str] = &["gpt-5.4", "gpt-5.4-mini"];

fn fetch_openai_models(
    client: &reqwest::Client,
    provider: &Provider,
) -> impl std::future::Future<Output = Result<reqwest::Response, reqwest::Error>> {
    let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
    client.get(url).bearer_auth(&provider.api_key).send()
}

fn fetch_anthropic_models(
    client: &reqwest::Client,
    provider: &Provider,
) -> impl std::future::Future<Output = Result<reqwest::Response, reqwest::Error>> {
    let url = format!("{}/v1/models", provider.base_url.trim_end_matches('/'));
    let request = if matches!(provider.auth_mode, AuthMode::OauthClaudeCode) {
        client
            .get(url)
            .bearer_auth(&provider.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "oauth-2025-04-20")
    } else {
        client
            .get(url)
            .header("x-api-key", &provider.api_key)
            .header("anthropic-version", "2023-06-01")
    };
    request.send()
}

fn fetch_gemini_models(
    client: &reqwest::Client,
    provider: &Provider,
) -> impl std::future::Future<Output = Result<reqwest::Response, reqwest::Error>> {
    let base = provider.base_url.trim_end_matches('/');
    let request = if matches!(provider.auth_mode, AuthMode::OauthGeminiCli) {
        client
            .get(format!("{}/v1beta/models", base))
            .bearer_auth(&provider.api_key)
            .header("x-goog-api-client", "GeminiCLI/1.0")
    } else {
        client.get(format!("{}/v1beta/models?key={}", base, provider.api_key))
    };
    request.send()
}

pub async fn fetch(provider: &Provider) -> platform::AppResult<Vec<FetchedModel>> {
    let client = reqwest::Client::builder()
        .user_agent("hebbian/0.1")
        .build()?;
    match provider.kind {
        ProviderKind::Openai => {
            if matches!(provider.auth_mode, AuthMode::OauthCodex) {
                return Ok(CODEX_OAUTH_MODELS
                    .iter()
                    .map(|id| FetchedModel {
                        id: (*id).to_string(),
                        owned_by: Some("ChatGPT / Codex OAuth".to_string()),
                    })
                    .collect());
            }
            let resp = fetch_openai_models(&client, provider).await?;
            let status = resp.status();
            let text = resp.text().await?;
            if !status.is_success() {
                return Err(AppError::msg(format!("{}: {}", status, text)));
            }
            let v: Value = serde_json::from_str(&text)?;
            let mut out = Vec::new();
            if let Some(arr) = v["data"].as_array() {
                for m in arr {
                    if let Some(id) = m["id"].as_str() {
                        out.push(FetchedModel {
                            id: id.to_string(),
                            owned_by: m["owned_by"].as_str().map(String::from),
                        });
                    }
                }
            }
            Ok(out)
        }
        ProviderKind::Anthropic => {
            let resp = fetch_anthropic_models(&client, provider).await?;
            let status = resp.status();
            let text = resp.text().await?;
            if !status.is_success() {
                return Err(AppError::msg(format!("{}: {}", status, text)));
            }
            let v: Value = serde_json::from_str(&text)?;
            let mut out = Vec::new();
            if let Some(arr) = v["data"].as_array() {
                for m in arr {
                    if let Some(id) = m["id"].as_str() {
                        out.push(FetchedModel {
                            id: id.to_string(),
                            owned_by: m["display_name"].as_str().map(String::from),
                        });
                    }
                }
            }
            Ok(out)
        }
        ProviderKind::Gemini => {
            let resp = fetch_gemini_models(&client, provider).await?;
            let status = resp.status();
            let text = resp.text().await?;
            if !status.is_success() {
                if matches!(provider.auth_mode, AuthMode::OauthGeminiCli)
                    && text.contains("ACCESS_TOKEN_SCOPE_INSUFFICIENT")
                {
                    return Err(AppError::msg(
                        "Gemini OAuth token scope 不足，当前凭据不能用于拉取模型列表。",
                    ));
                }
                return Err(AppError::msg(format!("{}: {}", status, text)));
            }
            let v: Value = serde_json::from_str(&text)?;
            let mut out = Vec::new();
            if let Some(arr) = v["models"].as_array() {
                for m in arr {
                    if let Some(name) = m["name"].as_str() {
                        let id = name.strip_prefix("models/").unwrap_or(name).to_string();
                        out.push(FetchedModel {
                            id,
                            owned_by: m["displayName"].as_str().map(String::from),
                        });
                    }
                }
            }
            Ok(out)
        }
        ProviderKind::Deepseek => {
            // chat.deepseek.com web 协议没有「列模型」端点，给出固定清单。
            Ok([
                "deepseek-v4-pro",
                "deepseek-v4-flash",
                "deepseek-v4-pro-search",
                "deepseek-v4-flash-search",
                "deepseek-v4-vision",
                "deepseek-v4-pro-nothinking",
                "deepseek-v4-flash-nothinking",
            ]
            .iter()
            .map(|id| FetchedModel {
                id: (*id).to_string(),
                owned_by: Some("DeepSeek Web".to_string()),
            })
            .collect())
        }
    }
}
