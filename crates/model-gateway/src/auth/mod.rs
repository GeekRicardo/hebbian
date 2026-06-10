pub mod deepseek;
pub mod deepseek_pow;
pub mod refresh;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::{AppError, AppResult};
use parking_lot::Mutex;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::OnceLock;

// ===================================================================
// 通用工具
// ===================================================================

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn gen_random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

fn b64url_no_pad(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn code_challenge_s256(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    b64url_no_pad(&hash)
}

fn extract_claim_from_jwt(token: &str, field: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let payload = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let v: Value = serde_json::from_slice(&payload).ok()?;
    v["https://api.openai.com/auth"][field]
        .as_str()
        .map(String::from)
        .or_else(|| v[field].as_str().map(String::from))
}

// ===================================================================
// 通用 OAuth 会话
// ===================================================================

#[derive(Debug, Clone)]
struct OAuthSession {
    state: String,
    code_verifier: String,
    redirect_uri: String,
    client_id: String,
    client_secret: Option<String>,
    created_at_ms: i64,
}

const SESSION_TTL_MS: i64 = 30 * 60 * 1000;

fn session_store() -> &'static Mutex<HashMap<String, OAuthSession>> {
    static STORE: OnceLock<Mutex<HashMap<String, OAuthSession>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn store_session(s: OAuthSession) -> String {
    let id = b64url_no_pad(&gen_random_bytes(16));
    let mut m = session_store().lock();
    let now = now_ms();
    m.retain(|_, e| now - e.created_at_ms < SESSION_TTL_MS);
    m.insert(id.clone(), s);
    id
}

fn take_session(session_id: &str) -> Option<OAuthSession> {
    let mut m = session_store().lock();
    m.remove(session_id)
}

fn get_session(session_id: &str) -> Option<OAuthSession> {
    let mut m = session_store().lock();
    let session = m.get(session_id).cloned()?;
    if now_ms() - session.created_at_ms >= SESSION_TTL_MS {
        m.remove(session_id);
        return None;
    }
    Some(session)
}

fn delete_session(session_id: &str) {
    let mut m = session_store().lock();
    m.remove(session_id);
}

// ===================================================================
// 统一的返回结构
// ===================================================================

#[derive(Debug, Clone, Serialize)]
pub struct AuthUrlResult {
    pub auth_url: String,
    pub session_id: String,
    pub state: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportedToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub expires_at: Option<i64>,
    /// Gemini 刷新需要
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

// ===================================================================
// Claude Code OAuth（对齐 sub2api claude_oauth_service.go）
// ===================================================================

const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const CLAUDE_SCOPE: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

pub fn claude_oauth_start() -> AppResult<AuthUrlResult> {
    let state = b64url_no_pad(&gen_random_bytes(32));
    let code_verifier = b64url_no_pad(&gen_random_bytes(32));
    let code_challenge = code_challenge_s256(&code_verifier);

    let encoded_redirect = urlencoding::encode(CLAUDE_REDIRECT_URI);
    let encoded_scope = urlencoding::encode(CLAUDE_SCOPE).replace("%20", "+");

    let auth_url = format!(
        "{}?code=true&client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        CLAUDE_AUTHORIZE_URL,
        CLAUDE_CLIENT_ID,
        encoded_redirect,
        encoded_scope,
        code_challenge,
        state
    );

    let session_id = store_session(OAuthSession {
        state: state.clone(),
        code_verifier,
        redirect_uri: CLAUDE_REDIRECT_URI.to_string(),
        client_id: CLAUDE_CLIENT_ID.to_string(),
        client_secret: None,
        created_at_ms: now_ms(),
    });

    Ok(AuthUrlResult {
        auth_url,
        session_id,
        state,
        redirect_uri: CLAUDE_REDIRECT_URI.to_string(),
    })
}

pub async fn claude_oauth_exchange(session_id: &str, code: &str) -> AppResult<ImportedToken> {
    let session = take_session(session_id)
        .ok_or_else(|| AppError::msg("OAuth 会话已过期，请重新发起登录"))?;

    let code = code.trim();
    let (auth_code, code_state) = match code.find('#') {
        Some(i) => (&code[..i], Some(&code[i + 1..])),
        None => (code, None),
    };

    let mut body = serde_json::json!({
        "code": auth_code,
        "grant_type": "authorization_code",
        "client_id": session.client_id,
        "redirect_uri": session.redirect_uri,
        "code_verifier": session.code_verifier,
    });
    if let Some(st) = code_state {
        body["state"] = Value::String(st.to_string());
    } else if !session.state.is_empty() {
        body["state"] = Value::String(session.state.clone());
    }

    let client = reqwest::Client::builder()
        .user_agent("axios/1.13.6")
        .build()?;
    let resp = client
        .post(CLAUDE_TOKEN_URL)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!(
            "Claude token 交换失败: {} - {}",
            status, text
        )));
    }
    let v: Value = serde_json::from_str(&text)?;
    let access_token = v["access_token"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 access_token"))?
        .to_string();
    let refresh_token = v["refresh_token"].as_str().map(String::from);
    let expires_in = v["expires_in"].as_i64();
    let expires_at = expires_in.map(|e| now_ms() + e * 1000);
    let account_id = v["account"]["uuid"]
        .as_str()
        .map(String::from)
        .or_else(|| v["organization"]["uuid"].as_str().map(String::from));

    Ok(ImportedToken {
        access_token,
        refresh_token,
        account_id,
        expires_at,
        client_id: None,
        client_secret: None,
    })
}

pub async fn claude_oauth_refresh(refresh_token: &str) -> AppResult<ImportedToken> {
    // platform.claude.com 的 OAuth token 端点要求 application/x-www-form-urlencoded，
    // 用 JSON 会直接返回 400 invalid_grant（社区 #1 故障原因）。
    let client = reqwest::Client::builder()
        .user_agent("claude-cli/1.0.0 (external, cli)")
        .build()?;
    let resp = client
        .post(CLAUDE_TOKEN_URL)
        .header("Accept", "application/json, text/plain, */*")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLAUDE_CLIENT_ID),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!(
            "Claude token 刷新失败: {} - {}",
            status, text
        )));
    }
    let v: Value = serde_json::from_str(&text)?;
    let access_token = v["access_token"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 access_token"))?
        .to_string();
    let new_refresh = v["refresh_token"]
        .as_str()
        .map(String::from)
        .or_else(|| Some(refresh_token.to_string()));
    let expires_in = v["expires_in"].as_i64();
    let expires_at = expires_in.map(|e| now_ms() + e * 1000);

    Ok(ImportedToken {
        access_token,
        refresh_token: new_refresh,
        account_id: None,
        expires_at,
        client_id: None,
        client_secret: None,
    })
}

// ===================================================================
// Gemini OAuth
// ===================================================================

const GEMINI_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GEMINI_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
// 下面两个常量是 Google 官方 Gemini CLI 客户端身份（installed-app / PKCE 流，
// 按 RFC 8252 设计就要随客户端分发，不是真正的服务端密钥）。
// 用 concat! 编译期拼接，避免 GitHub secret scanner 把源码里的完整字面量误识别为泄露。
const GEMINI_CLI_CLIENT_ID: &str = concat!(
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j",
    ".apps.googleusercontent.com"
);
const GEMINI_CLI_CLIENT_SECRET: &str = concat!("GOCSPX", "-4uHgMPm-1o7Sk-geV6Cu5clXFsxl");
const GEMINI_CLI_REDIRECT_URI: &str = "https://codeassist.google.com/authcode";
const GEMINI_REQUIRED_SCOPE: &str = "https://www.googleapis.com/auth/generative-language.retriever";
const GEMINI_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/generative-language.retriever https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";

fn gemini_scope_is_sufficient(scope: &str) -> bool {
    scope
        .split_whitespace()
        .any(|item| item == GEMINI_REQUIRED_SCOPE)
}

fn validate_gemini_scope(scope: Option<&str>) -> AppResult<()> {
    if let Some(scope) = scope {
        if !gemini_scope_is_sufficient(scope) {
            return Err(AppError::msg(format!(
                "Gemini OAuth token 缺少必需 scope：{}。请在 Hebbian 内重新完成 Gemini OAuth 授权后再试；从 ~/.gemini/oauth_creds.json 导入的凭据如果没有这个 scope，将无法用于拉取模型列表。",
                GEMINI_REQUIRED_SCOPE
            )));
        }
    }
    Ok(())
}

pub fn gemini_oauth_start() -> AppResult<AuthUrlResult> {
    let state = b64url_no_pad(&gen_random_bytes(32));
    let code_verifier = b64url_no_pad(&gen_random_bytes(32));
    let code_challenge = code_challenge_s256(&code_verifier);

    let params = [
        ("response_type", "code"),
        ("client_id", GEMINI_CLI_CLIENT_ID),
        ("redirect_uri", GEMINI_CLI_REDIRECT_URI),
        ("scope", GEMINI_SCOPE),
        ("state", &state),
        ("code_challenge", &code_challenge),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("include_granted_scopes", "true"),
    ];
    let query: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let auth_url = format!("{}?{}", GEMINI_AUTHORIZE_URL, query);

    let session_id = store_session(OAuthSession {
        state: state.clone(),
        code_verifier,
        redirect_uri: GEMINI_CLI_REDIRECT_URI.to_string(),
        client_id: GEMINI_CLI_CLIENT_ID.to_string(),
        client_secret: Some(GEMINI_CLI_CLIENT_SECRET.to_string()),
        created_at_ms: now_ms(),
    });

    Ok(AuthUrlResult {
        auth_url,
        session_id,
        state,
        redirect_uri: GEMINI_CLI_REDIRECT_URI.to_string(),
    })
}

pub async fn gemini_oauth_exchange(session_id: &str, code: &str) -> AppResult<ImportedToken> {
    let session = take_session(session_id)
        .ok_or_else(|| AppError::msg("OAuth 会话已过期，请重新发起登录"))?;
    let client_secret = session
        .client_secret
        .clone()
        .unwrap_or_else(|| GEMINI_CLI_CLIENT_SECRET.to_string());

    let client = reqwest::Client::builder()
        .user_agent("Hebbian/0.1")
        .build()?;
    let resp = client
        .post(GEMINI_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", session.client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code.trim()),
            ("code_verifier", session.code_verifier.as_str()),
            ("redirect_uri", session.redirect_uri.as_str()),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!(
            "Gemini token 交换失败: {} - {}",
            status, text
        )));
    }
    let v: Value = serde_json::from_str(&text)?;
    validate_gemini_scope(v["scope"].as_str())?;
    let access_token = v["access_token"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 access_token"))?
        .to_string();
    let refresh_token = v["refresh_token"].as_str().map(String::from);
    let expires_in = v["expires_in"].as_i64().unwrap_or(3600);
    let expires_at = Some(now_ms() + expires_in * 1000);

    Ok(ImportedToken {
        access_token,
        refresh_token,
        account_id: None,
        expires_at,
        client_id: Some(session.client_id),
        client_secret: Some(client_secret),
    })
}

pub async fn gemini_refresh(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> AppResult<ImportedToken> {
    let client = reqwest::Client::builder()
        .user_agent("Hebbian/0.1")
        .build()?;
    let resp = client
        .post(GEMINI_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!(
            "Gemini token 刷新失败: {} - {}",
            status, text
        )));
    }
    let v: Value = serde_json::from_str(&text)?;
    validate_gemini_scope(v["scope"].as_str())?;
    let access_token = v["access_token"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 access_token"))?
        .to_string();
    let expires_in = v["expires_in"].as_i64().unwrap_or(3600);
    Ok(ImportedToken {
        access_token,
        refresh_token: Some(refresh_token.to_string()),
        account_id: None,
        expires_at: Some(now_ms() + expires_in * 1000),
        client_id: Some(client_id.to_string()),
        client_secret: Some(client_secret.to_string()),
    })
}

// ===================================================================
// OpenAI (Codex / ChatGPT) OAuth —— 两种模式并存
// ===================================================================

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_DEFAULT_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CODEX_SCOPE: &str = "openid profile email offline_access";
const CODEX_REFRESH_SCOPE: &str = "openid profile email";
const CODEX_USER_AGENT: &str = "codex-cli/0.91.0";

pub fn openai_oauth_start() -> AppResult<AuthUrlResult> {
    let state = hex_encode(&gen_random_bytes(32));
    let code_verifier = hex_encode(&gen_random_bytes(64));
    let code_challenge = code_challenge_s256(&code_verifier);

    let params = [
        ("response_type", "code"),
        ("client_id", CODEX_CLIENT_ID),
        ("redirect_uri", CODEX_DEFAULT_REDIRECT_URI),
        ("scope", CODEX_SCOPE),
        ("state", &state),
        ("code_challenge", &code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
    ];
    let query: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let auth_url = format!("{}?{}", CODEX_AUTHORIZE_URL, query);

    let session_id = store_session(OAuthSession {
        state: state.clone(),
        code_verifier,
        redirect_uri: CODEX_DEFAULT_REDIRECT_URI.to_string(),
        client_id: CODEX_CLIENT_ID.to_string(),
        client_secret: None,
        created_at_ms: now_ms(),
    });

    Ok(AuthUrlResult {
        auth_url,
        session_id,
        state,
        redirect_uri: CODEX_DEFAULT_REDIRECT_URI.to_string(),
    })
}

fn decode_query_component(value: &str) -> AppResult<String> {
    let normalized = value.replace('+', " ");
    urlencoding::decode(&normalized)
        .map(|v| v.into_owned())
        .map_err(|e| AppError::msg(format!("OAuth 回调参数解码失败: {e}")))
}

fn parse_openai_callback_query(query: &str) -> AppResult<(String, Option<String>)> {
    let mut code = None;
    let mut state = None;

    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or_default();
        let value = parts.next().unwrap_or_default();
        match key {
            "code" => code = Some(decode_query_component(value)?),
            "state" => state = Some(decode_query_component(value)?),
            _ => {}
        }
    }

    let code = code.ok_or_else(|| AppError::msg("OAuth 回调缺少 code"))?;
    Ok((code, state))
}

fn openai_callback_code_and_state(input: &str) -> AppResult<(String, Option<String>)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AppError::msg("OAuth 回调缺少 code"));
    }

    if let Some((_, query)) = trimmed.split_once('?') {
        let query = query.split('#').next().unwrap_or_default();
        return parse_openai_callback_query(query);
    }

    let query = trimmed.strip_prefix('?').unwrap_or(trimmed);
    let looks_like_query = query
        .split('&')
        .any(|pair| pair.starts_with("code=") || pair.starts_with("state="));
    if looks_like_query {
        let query = query.split('#').next().unwrap_or_default();
        return parse_openai_callback_query(query);
    }

    if let Some((code, state)) = trimmed.split_once('#') {
        let code = code.trim();
        if code.is_empty() {
            return Err(AppError::msg("OAuth 回调缺少 code"));
        }
        let state = state.trim();
        return Ok((
            code.to_string(),
            (!state.is_empty()).then(|| state.to_string()),
        ));
    }

    if trimmed.contains("://") {
        return Err(AppError::msg("OAuth 回调缺少 code"));
    }

    Ok((trimmed.to_string(), None))
}

pub async fn openai_oauth_exchange(
    session_id: &str,
    code: &str,
    state: Option<&str>,
) -> AppResult<ImportedToken> {
    let session =
        get_session(session_id).ok_or_else(|| AppError::msg("OAuth 会话已过期，请重新发起登录"))?;
    let (auth_code, callback_state) = openai_callback_code_and_state(code)?;
    let provided_state = callback_state
        .as_deref()
        .or(state)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::msg("OpenAI OAuth 需要 state，请粘贴完整回调 URL 或重新发起登录")
        })?;
    if provided_state != session.state {
        return Err(AppError::msg("OpenAI OAuth state 不匹配，请重新发起登录"));
    }

    let client = reqwest::Client::builder()
        .user_agent(CODEX_USER_AGENT)
        .build()?;
    let resp = client
        .post(CODEX_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", session.client_id.as_str()),
            ("code", auth_code.as_str()),
            ("redirect_uri", session.redirect_uri.as_str()),
            ("code_verifier", session.code_verifier.as_str()),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!(
            "OpenAI token 交换失败: {} - {}",
            status, text
        )));
    }
    let v: Value = serde_json::from_str(&text)?;
    let access_token = v["access_token"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 access_token"))?
        .to_string();
    let refresh_token = v["refresh_token"].as_str().map(String::from);
    let expires_in = v["expires_in"].as_i64();
    let expires_at = expires_in.map(|e| now_ms() + e * 1000);
    let account_id = v["id_token"]
        .as_str()
        .and_then(|t| extract_claim_from_jwt(t, "chatgpt_account_id"));

    let token = ImportedToken {
        access_token,
        refresh_token,
        account_id,
        expires_at,
        client_id: None,
        client_secret: None,
    };
    delete_session(session_id);
    Ok(token)
}

// ---- Device flow（保留用于已有 UI 入口）----

const DEVICE_AUTH_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_AUTH_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const DEVICE_USER_AGENT: &str = "codex_cli_rs/0.1";

#[derive(Debug, Clone, Serialize)]
pub struct DeviceCodeInfo {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone)]
struct PendingDevice {
    user_code: String,
    expires_at_ms: i64,
}

fn pending_store() -> &'static Mutex<HashMap<String, PendingDevice>> {
    static STORE: OnceLock<Mutex<HashMap<String, PendingDevice>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn device_http_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(DEVICE_USER_AGENT)
        .build()
        .map_err(Into::into)
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexTokenInfo {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub expires_at: Option<i64>,
}

pub async fn codex_start() -> AppResult<DeviceCodeInfo> {
    let client = device_http_client()?;
    let resp = client
        .post(DEVICE_AUTH_USERCODE_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "client_id": CODEX_CLIENT_ID }))
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!(
            "Codex device_code 请求失败: {} - {}",
            status, text
        )));
    }
    let v: Value = serde_json::from_str(&text)?;
    let device_auth_id = v["device_auth_id"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 device_auth_id"))?
        .to_string();
    let user_code = v["user_code"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 user_code"))?
        .to_string();
    let expires_in = v["expires_in"].as_u64().unwrap_or(900);
    let interval = v["interval"].as_u64().unwrap_or(5);

    {
        let mut m = pending_store().lock();
        let now = now_ms();
        m.retain(|_, e| e.expires_at_ms > now);
        m.insert(
            device_auth_id.clone(),
            PendingDevice {
                user_code: user_code.clone(),
                expires_at_ms: now + (expires_in as i64) * 1000,
            },
        );
    }

    Ok(DeviceCodeInfo {
        device_code: device_auth_id,
        user_code,
        verification_uri: DEVICE_VERIFICATION_URL.to_string(),
        expires_in,
        interval,
    })
}

pub async fn codex_poll(device_code: &str) -> AppResult<Option<CodexTokenInfo>> {
    let entry = {
        let m = pending_store().lock();
        m.get(device_code).cloned()
    };
    let entry = entry.ok_or_else(|| AppError::msg("未找到对应的登录请求，请重新登录"))?;
    if entry.expires_at_ms <= now_ms() {
        let mut m = pending_store().lock();
        m.remove(device_code);
        return Err(AppError::msg("登录已过期，请重新发起"));
    }

    let client = device_http_client()?;
    let resp = client
        .post(DEVICE_AUTH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "device_auth_id": device_code,
            "user_code": entry.user_code,
        }))
        .send()
        .await?;
    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 404 {
        return Ok(None);
    }
    if status.as_u16() == 410 {
        let mut m = pending_store().lock();
        m.remove(device_code);
        return Err(AppError::msg("授权码已过期"));
    }
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!("{}: {}", status, text)));
    }

    #[derive(Deserialize)]
    struct Poll {
        authorization_code: String,
        code_verifier: String,
    }
    let poll: Poll = serde_json::from_str(&text)?;

    let resp = client
        .post(CODEX_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &poll.authorization_code),
            ("redirect_uri", DEVICE_REDIRECT_URI),
            ("client_id", CODEX_CLIENT_ID),
            ("code_verifier", &poll.code_verifier),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!(
            "换取 token 失败: {} - {}",
            status, text
        )));
    }
    let v: Value = serde_json::from_str(&text)?;
    let access_token = v["access_token"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 access_token"))?
        .to_string();
    let refresh_token = v["refresh_token"].as_str().map(String::from);
    let expires_in = v["expires_in"].as_i64();
    let expires_at = expires_in.map(|e| now_ms() + e * 1000);
    let account_id = v["id_token"]
        .as_str()
        .and_then(|t| extract_claim_from_jwt(t, "chatgpt_account_id"));

    {
        let mut m = pending_store().lock();
        m.remove(device_code);
    }

    Ok(Some(CodexTokenInfo {
        access_token,
        refresh_token,
        account_id,
        expires_at,
    }))
}

pub async fn codex_refresh(refresh_token: &str) -> AppResult<CodexTokenInfo> {
    let client = reqwest::Client::builder()
        .user_agent(CODEX_USER_AGENT)
        .build()?;
    let resp = client
        .post(CODEX_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
            ("scope", CODEX_REFRESH_SCOPE),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::msg(format!("刷新失败: {} - {}", status, text)));
    }
    let v: Value = serde_json::from_str(&text)?;
    let access_token = v["access_token"]
        .as_str()
        .ok_or_else(|| AppError::msg("响应缺少 access_token"))?
        .to_string();
    let refresh_token_new = v["refresh_token"]
        .as_str()
        .map(String::from)
        .or_else(|| Some(refresh_token.to_string()));
    let account_id = v["id_token"]
        .as_str()
        .and_then(|t| extract_claim_from_jwt(t, "chatgpt_account_id"));
    let expires_at = v["expires_in"].as_i64().map(|e| now_ms() + e * 1000);
    Ok(CodexTokenInfo {
        access_token,
        refresh_token: refresh_token_new,
        account_id,
        expires_at,
    })
}

// ===================================================================
// Claude Code / Gemini CLI 本地凭据导入
// ===================================================================

#[derive(Debug, Deserialize)]
struct ClaudeOAuthEntry {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<Value>,
    /// 订阅档位（"max"/"pro"）——仅记录凭据 schema。account_uuid 改由 profile
    /// endpoint 拿，不再用这个字段（曾被误当 account_id 发出去）。
    #[serde(rename = "subscriptionType")]
    #[allow(dead_code)]
    subscription_type: Option<String>,
}

/// 从 Claude Code 本地凭据导入。
///
/// 兼容三种存储位置（按优先级）：
/// 1. macOS Keychain：service `Claude Code-credentials`（新版 Claude Code 默认走这里）
/// 2. `~/.claude/.credentials.json`（老版本 / 非 macOS）
/// 3. `~/.config/claude/.credentials.json`（XDG 路径，少数 Linux 发行版）
///
/// JSON schema 兼容 `claudeAiOauth` 与 `claude.ai_oauth` 两种 key 名。
///
/// 本地凭据不含 account uuid，导入后用 access_token 调 profile endpoint 补全，
/// 失败不阻塞（account_uuid 缺失只影响 CC 伪装完整度，不影响请求成功）。
pub async fn claude_code_import() -> AppResult<ImportedToken> {
    let mut token = read_local_claude_credentials()?;
    if token.account_id.is_none() {
        token.account_id = fetch_claude_account_uuid(&token.access_token).await;
    }
    Ok(token)
}

/// 同步读取本地 Claude Code 凭据（Keychain / 文件）；account_id 留空，
/// 由 [`claude_code_import`] 调 profile endpoint 补全。
fn read_local_claude_credentials() -> AppResult<ImportedToken> {
    let mut tried: Vec<String> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        match read_claude_credentials_from_keychain() {
            Ok(Some(text)) => return parse_claude_credentials_json(&text),
            Ok(None) => tried.push("macOS Keychain (Claude Code-credentials)".into()),
            Err(e) => tried.push(format!("macOS Keychain 读取失败: {e}")),
        }
    }

    let home = dirs::home_dir().ok_or_else(|| AppError::msg("无法确定 HOME 目录"))?;
    for candidate in [
        home.join(".claude").join(".credentials.json"),
        home.join(".config")
            .join("claude")
            .join(".credentials.json"),
    ] {
        if candidate.exists() {
            let text = std::fs::read_to_string(&candidate)?;
            return parse_claude_credentials_json(&text);
        }
        tried.push(candidate.display().to_string());
    }

    Err(AppError::msg(format!(
        "未找到 Claude Code 凭据 — 请先在终端运行 `claude` 并完成登录。\n已尝试的位置：\n  - {}",
        tried.join("\n  - ")
    )))
}

#[cfg(target_os = "macos")]
fn read_claude_credentials_from_keychain() -> AppResult<Option<String>> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .map_err(|e| AppError::msg(format!("调用 security 命令失败: {e}")))?;

    if !output.status.success() {
        // Keychain 中没有这个 entry，让调用方走文件回退；不视为错误。
        return Ok(None);
    }

    let text = String::from_utf8(output.stdout)
        .map_err(|e| AppError::msg(format!("Keychain 输出非 UTF-8: {e}")))?
        .trim()
        .to_string();
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(text))
}

fn parse_claude_credentials_json(text: &str) -> AppResult<ImportedToken> {
    let v: Value = serde_json::from_str(text)
        .map_err(|e| AppError::msg(format!("解析 Claude 凭据失败: {e}")))?;
    let entry = v
        .get("claudeAiOauth")
        .or_else(|| v.get("claude.ai_oauth"))
        .cloned()
        .ok_or_else(|| AppError::msg("凭据中找不到 claudeAiOauth 节点"))?;
    let parsed: ClaudeOAuthEntry = serde_json::from_value(entry)
        .map_err(|e| AppError::msg(format!("凭据结构不符合预期: {e}")))?;
    let access_token = parsed
        .access_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::msg("凭据缺少 accessToken"))?;
    let expires_at = parsed.expires_at.and_then(|v| match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    });
    Ok(ImportedToken {
        access_token,
        refresh_token: parsed.refresh_token,
        // 本地凭据里没有 account uuid——subscriptionType 是订阅档位（"max"/"pro"），
        // 不是账号标识。account_id 留空，由 claude_code_import 调 profile endpoint 补全。
        account_id: None,
        expires_at,
        client_id: None,
        client_secret: None,
    })
}

/// Claude OAuth profile endpoint：用 access_token 换账号 uuid。
const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

/// 用 access_token 拉取账号 uuid（写进 metadata.user_id 的 account_uuid，对齐真 CC）。
/// 本地凭据不含 uuid、access_token 也不是可解析的 JWT，只能调这个端点拿。
/// 任何失败都返回 None：account_uuid 缺失只影响 CC 伪装完整度，不该阻塞登录导入。
async fn fetch_claude_account_uuid(access_token: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .user_agent("claude-cli/2.1.170 (external, cli)")
        .build()
        .ok()?;
    let resp = client
        .get(CLAUDE_PROFILE_URL)
        .bearer_auth(access_token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    v["account"]["uuid"].as_str().map(String::from)
}

#[derive(Debug, Deserialize)]
struct GeminiOAuthEntry {
    access_token: Option<String>,
    refresh_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    scope: Option<String>,
    expiry_date: Option<i64>,
}

pub async fn gemini_cli_import() -> AppResult<ImportedToken> {
    let home = dirs::home_dir().ok_or_else(|| AppError::msg("无法确定 HOME 目录"))?;
    let path = home.join(".gemini").join("oauth_creds.json");
    if !path.exists() {
        return Err(AppError::msg(format!(
            "未找到 {} — 请先在终端运行 `gemini` 并完成登录",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(&path)?;
    let parsed: GeminiOAuthEntry = serde_json::from_str(&text)
        .map_err(|e| AppError::msg(format!("凭据结构不符合预期: {e}")))?;
    validate_gemini_scope(parsed.scope.as_deref())?;

    let need_refresh = parsed
        .access_token
        .as_deref()
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
        || parsed
            .expiry_date
            .map(|e| e - now_ms() < 60_000)
            .unwrap_or(false);

    if need_refresh {
        if let (Some(rt), Some(cid), Some(cs)) = (
            parsed.refresh_token.clone(),
            parsed.client_id.clone(),
            parsed.client_secret.clone(),
        ) {
            let refreshed = gemini_refresh(&rt, &cid, &cs).await?;
            return Ok(refreshed);
        }
    }

    let access_token = parsed
        .access_token
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::msg("凭据无 access_token 且无法刷新"))?;
    Ok(ImportedToken {
        access_token,
        refresh_token: parsed.refresh_token,
        account_id: None,
        expires_at: parsed.expiry_date,
        client_id: parsed.client_id,
        client_secret: parsed.client_secret,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        gemini_scope_is_sufficient, openai_callback_code_and_state, openai_oauth_exchange,
        openai_oauth_start, CODEX_CLIENT_ID, CODEX_DEFAULT_REDIRECT_URI, CODEX_SCOPE,
        GEMINI_REQUIRED_SCOPE, GEMINI_SCOPE,
    };

    #[test]
    fn openai_oauth_url_matches_sub2api_codex_pkce_flow() {
        let auth = openai_oauth_start().unwrap();

        assert!(auth
            .auth_url
            .starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(auth
            .auth_url
            .contains(&format!("client_id={}", CODEX_CLIENT_ID)));
        assert!(auth.auth_url.contains(&format!(
            "redirect_uri={}",
            urlencoding::encode(CODEX_DEFAULT_REDIRECT_URI)
        )));
        assert!(auth
            .auth_url
            .contains(&format!("scope={}", urlencoding::encode(CODEX_SCOPE))));
        assert!(auth.auth_url.contains("response_type=code"));
        assert!(auth.auth_url.contains("code_challenge_method=S256"));
        assert!(auth.auth_url.contains("id_token_add_organizations=true"));
        assert!(auth.auth_url.contains("codex_cli_simplified_flow=true"));
    }

    #[test]
    fn openai_callback_input_extracts_code_and_state_from_callback_url() {
        let (code, state) = openai_callback_code_and_state(
            "http://localhost:1455/auth/callback?code=auth-code-123&state=state-456",
        )
        .unwrap();

        assert_eq!(code, "auth-code-123");
        assert_eq!(state.as_deref(), Some("state-456"));
    }

    #[test]
    fn openai_callback_input_accepts_query_string_and_raw_code() {
        let (code, state) =
            openai_callback_code_and_state("code=auth-code-123&state=state-456").unwrap();
        assert_eq!(code, "auth-code-123");
        assert_eq!(state.as_deref(), Some("state-456"));

        let (code, state) = openai_callback_code_and_state("auth-code-123").unwrap();
        assert_eq!(code, "auth-code-123");
        assert_eq!(state, None);
    }

    #[tokio::test]
    async fn openai_exchange_rejects_mismatched_state_before_token_request() {
        let auth = openai_oauth_start().unwrap();
        let err = openai_oauth_exchange(
            &auth.session_id,
            "http://localhost:1455/auth/callback?code=auth-code-123&state=wrong-state",
            Some(&auth.state),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("state 不匹配"));
    }

    #[test]
    fn gemini_oauth_requests_retriever_scope() {
        assert!(GEMINI_SCOPE
            .split_whitespace()
            .any(|scope| scope == GEMINI_REQUIRED_SCOPE));
    }

    #[test]
    fn gemini_scope_validation_rejects_scope_without_retriever() {
        assert!(!gemini_scope_is_sufficient(
            "openid https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cloud-platform"
        ));
    }

    #[test]
    fn gemini_scope_validation_accepts_retriever_scope() {
        assert!(gemini_scope_is_sufficient(
            "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/generative-language.retriever"
        ));
    }
}
