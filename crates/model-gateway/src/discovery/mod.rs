use crate::config::{AuthMode, Provider, ProviderKind};
use common::AppError;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct FetchedModel {
    pub id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
    /// 模型上下文窗口大小（从 /v1/models 响应中提取，不一定所有 provider 都返回）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<usize>,
}

const CODEX_OAUTH_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// 从模型 JSON 对象中提取 context_length。
/// 各家 provider 返回的字段名不统一，按优先级尝试多个候选字段。
fn extract_context_length(model_obj: &Value) -> Option<usize> {
    // DeepSeek API / 硅基流动 / 火山方舟等用 context_length
    // OpenAI 官方不在 /v1/models 里返回这个字段，但第三方兼容层常加
    for key in &["context_length", "context_window", "max_context_length"] {
        if let Some(n) = model_obj[key].as_u64() {
            if n > 0 {
                return Some(n as usize);
            }
        }
    }
    None
}

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

pub async fn fetch(provider: &Provider) -> common::AppResult<Vec<FetchedModel>> {
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
                        context_length: None,
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
                            context_length: extract_context_length(m),
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
                            context_length: extract_context_length(m),
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
                        // Gemini 用 inputTokenLimit 表示上下文窗口
                        let ctx = m["inputTokenLimit"]
                            .as_u64()
                            .map(|n| n as usize)
                            .or_else(|| extract_context_length(m));
                        out.push(FetchedModel {
                            id,
                            owned_by: m["displayName"].as_str().map(String::from),
                            context_length: ctx,
                        });
                    }
                }
            }
            Ok(out)
        }
        ProviderKind::Deepseek => {
            // chat.deepseek.com web 协议没有「列模型」端点，给出固定清单。
            // context_length 参考 openhanako known-models.json 中 deepseek 分区。
            Ok([
                ("deepseek-v4-pro", 1_000_000),
                ("deepseek-v4-flash", 1_000_000),
                ("deepseek-v4-pro-search", 1_000_000),
                ("deepseek-v4-flash-search", 1_000_000),
                ("deepseek-v4-vision", 1_000_000),
                ("deepseek-v4-pro-nothinking", 1_000_000),
                ("deepseek-v4-flash-nothinking", 1_000_000),
            ]
            .iter()
            .map(|(id, ctx)| FetchedModel {
                id: (*id).to_string(),
                owned_by: Some("DeepSeek Web".to_string()),
                context_length: Some(*ctx),
            })
            .collect())
        }
    }
}

/// 从 /v1/models 获取指定模型的 context_length。
/// 成功拿到则返回 `Some(tokens)`，API 不返回该字段或请求失败则返回 `None`。
pub async fn fetch_context_length(provider: &Provider, model: &str) -> Option<usize> {
    let models = fetch(provider).await.ok()?;
    models
        .into_iter()
        .find(|m| m.id == model)
        .and_then(|m| m.context_length)
}
