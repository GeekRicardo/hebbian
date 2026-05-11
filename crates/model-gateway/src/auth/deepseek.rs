//! DeepSeek 账号登录（chat.deepseek.com 入口）。
//!
//! 对齐 [ds2api `internal/deepseek/client/client_auth.go`](https://github.com/notnotalice/ds2api):
//!
//! - `POST https://chat.deepseek.com/api/v0/users/login`
//! - body：`email + password` 或 `mobile + area_code + password`
//! - 成功返回 `data.biz_data.user.token`
//!
//! 这个 token 可以作为 Bearer 用于 chat.deepseek.com 的 web API（PoW + path-SSE
//! 协议另行实现）。当前 desktop UI 把它落在 provider.api_key 上，作为后续 web
//! provider 的入口。
//!
//! 错误返回的 `code != 0` 时按 `biz_msg` / `msg` 提示用户。

use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const LOGIN_URL: &str = "https://chat.deepseek.com/api/v0/users/login";

/// 登录入参。`email` 与 `mobile` 二选一；`area_code` 仅当 `mobile` 时有效，
/// 默认 `+86`，例如 `+86`、`+1`。
#[derive(Debug, Clone, Deserialize)]
pub struct DeepseekLoginInput {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub mobile: Option<String>,
    #[serde(default)]
    pub area_code: Option<String>,
    pub password: String,
    /// 设备标识。不影响认证，但会出现在风控审计里。
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeepseekLoginToken {
    pub token: String,
    /// 登录用的账号（email 或 +86xxxx），方便 UI 把名字塞回 provider.name。
    pub login: String,
}

pub async fn deepseek_login(input: DeepseekLoginInput) -> AppResult<DeepseekLoginToken> {
    let DeepseekLoginInput {
        email,
        mobile,
        area_code,
        password,
        device_id,
    } = input;

    let password = password.trim().to_string();
    if password.is_empty() {
        return Err(AppError::msg("密码不能为空"));
    }

    let device_id = device_id.unwrap_or_else(|| "hebbian".to_string());

    let (mut body, login_label) = match (
        email.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        mobile.as_deref().map(str::trim).filter(|s| !s.is_empty()),
    ) {
        (Some(e), _) => (
            json!({
                "email": e,
                "password": password,
                "device_id": device_id,
                "os": "android",
            }),
            e.to_string(),
        ),
        (None, Some(m)) => {
            let area = area_code
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("+86")
                .to_string();
            let label = format!("{area} {m}");
            (
                json!({
                    "mobile": m,
                    "area_code": area,
                    "password": password,
                    "device_id": device_id,
                    "os": "android",
                }),
                label,
            )
        }
        (None, None) => {
            return Err(AppError::msg("请提供邮箱或手机号"));
        }
    };
    let _ = body.as_object_mut(); // keep mut for future fields

    let client = reqwest::Client::builder()
        .user_agent("DeepSeek/2.0.4 Android/35")
        .build()?;
    let resp = client
        .post(LOGIN_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("x-client-platform", "android")
        .header("x-client-version", "2.0.4")
        .header("x-client-locale", "zh_CN")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!(
            "DeepSeek 登录请求失败: {status} - {text}"
        )));
    }

    let v: Value = serde_json::from_str(&text)
        .map_err(|e| AppError::msg(format!("解析登录响应失败: {e} - {text}")))?;

    let code = v["code"].as_i64().unwrap_or_default();
    if code != 0 {
        let msg = v["data"]["biz_msg"]
            .as_str()
            .or_else(|| v["msg"].as_str())
            .unwrap_or("登录失败")
            .to_string();
        return Err(AppError::msg(format!("DeepSeek 登录失败：{msg}")));
    }
    let biz_code = v["data"]["biz_code"].as_i64().unwrap_or_default();
    if biz_code != 0 {
        let msg = v["data"]["biz_msg"]
            .as_str()
            .unwrap_or("登录失败")
            .to_string();
        return Err(AppError::msg(format!("DeepSeek 登录失败：{msg}")));
    }

    let token = v["data"]["biz_data"]["user"]["token"]
        .as_str()
        .ok_or_else(|| AppError::msg("登录响应缺少 token"))?
        .to_string();

    Ok(DeepseekLoginToken {
        token,
        login: login_label,
    })
}
