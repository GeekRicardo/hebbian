use chrono::DateTime;
use reqwest::header;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageProgress {
    /// 使用率 0.0–100.0（百分比）
    pub utilization: f64,
    pub resets_at: Option<String>,
    pub remaining_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeUsageInfo {
    pub five_hour: Option<UsageProgress>,
    pub seven_day: Option<UsageProgress>,
    pub seven_day_sonnet: Option<UsageProgress>,
    /// 账号邮箱（来自 /api/oauth/profile，拉取失败为 None）。
    #[serde(default)]
    pub email: Option<String>,
    /// 订阅档位标签：Max / Pro / Free 等（同源 profile）。
    #[serde(default)]
    pub plan: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekBalanceEntry {
    pub currency: String,
    pub total_balance: String,
    pub granted_balance: String,
    pub topped_up_balance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekBalanceInfo {
    pub available: bool,
    pub entries: Vec<DeepSeekBalanceEntry>,
}

fn make_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

/// 从 Anthropic OAuth usage 接口拉取当前账号的 5h / 7d 使用率。
/// access_token 是 OAuth 获得的 Bearer token。
pub async fn fetch_claude_usage(access_token: &str) -> Result<ClaudeUsageInfo, String> {
    let client = make_client()?;
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
        .header(header::ACCEPT, "application/json, text/plain, */*")
        .header(header::CONTENT_TYPE, "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header(header::USER_AGENT, "claude-code/2.1.7")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("status {status}: {body}"));
    }

    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    // 邮箱 + 订阅档位另从 profile 接口取（usage 接口不带），失败不影响用量展示。
    let (email, plan) = fetch_claude_profile(&client, access_token)
        .await
        .unwrap_or((None, None));
    Ok(ClaudeUsageInfo {
        five_hour: parse_progress(&data["five_hour"]),
        seven_day: parse_progress(&data["seven_day"]),
        seven_day_sonnet: parse_progress(&data["seven_day_sonnet"]),
        email,
        plan,
    })
}

/// 从 `/api/oauth/profile` 取账号邮箱 + 订阅档位，给 usage 指示器展示。
/// 任何失败都返回 None——纯展示信息，不该影响用量拉取主流程。
async fn fetch_claude_profile(
    client: &reqwest::Client,
    access_token: &str,
) -> Option<(Option<String>, Option<String>)> {
    let resp = client
        .get("https://api.anthropic.com/api/oauth/profile")
        .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
        .header(header::ACCEPT, "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let email = v["account"]["email"].as_str().map(|s| s.to_string());
    // 优先用 has_claude_max/pro 给出干净标签，兜底取 organization_type（去掉 claude_ 前缀）。
    let plan = if v["account"]["has_claude_max"].as_bool().unwrap_or(false) {
        Some("Max".to_string())
    } else if v["account"]["has_claude_pro"].as_bool().unwrap_or(false) {
        Some("Pro".to_string())
    } else {
        v["organization"]["organization_type"]
            .as_str()
            .map(|s| s.trim_start_matches("claude_").to_string())
    };
    Some((email, plan))
}

fn parse_progress(v: &serde_json::Value) -> Option<UsageProgress> {
    if v.is_null() || v.is_object() && v.as_object()?.is_empty() {
        return None;
    }
    let utilization = v["utilization"].as_f64()?;
    let resets_at = v["resets_at"].as_str().map(|s| s.to_string());
    let remaining_seconds = resets_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| {
            let diff = t
                .signed_duration_since(chrono::Utc::now())
                .num_seconds();
            diff.max(0)
        })
        .unwrap_or(0);
    Some(UsageProgress {
        utilization,
        resets_at,
        remaining_seconds,
    })
}

/// 从 DeepSeek 平台 API 查询账户余额（`/user/balance`）。
/// api_key 是平台分配的 API Key（Bearer token）。
pub async fn fetch_deepseek_balance(api_key: &str) -> Result<DeepSeekBalanceInfo, String> {
    let client = make_client()?;
    let resp = client
        .get("https://api.deepseek.com/user/balance")
        .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("status {status}: {body}"));
    }

    #[derive(Deserialize)]
    struct Raw {
        is_available: bool,
        balance_infos: Vec<RawEntry>,
    }
    #[derive(Deserialize)]
    struct RawEntry {
        currency: String,
        total_balance: String,
        granted_balance: String,
        topped_up_balance: String,
    }

    let raw: Raw = resp.json().await.map_err(|e| e.to_string())?;
    Ok(DeepSeekBalanceInfo {
        available: raw.is_available,
        entries: raw
            .balance_infos
            .into_iter()
            .map(|b| DeepSeekBalanceEntry {
                currency: b.currency,
                total_balance: b.total_balance,
                granted_balance: b.granted_balance,
                topped_up_balance: b.topped_up_balance,
            })
            .collect(),
    })
}
