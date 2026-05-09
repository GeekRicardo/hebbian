//! Provider OAuth token 自动刷新。
//!
//! 调用方在 `build_client` 之前先 `ensure_fresh_provider_token`，
//! 保证拿到的 Provider 里的 access_token 仍在有效期内。
//! 新 token 会写回 `providers.json`，之后再发请求就用新的。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;

use parking_lot::Mutex as SyncMutex;
use platform::AppResult;

use super::{claude_code_import, claude_oauth_refresh};
use crate::config::{self, AuthMode, Provider, ProviderKind};

/// 距离过期时间小于这个值就提前刷新，避免请求发到一半 token 失效。
const REFRESH_LEEWAY_MS: i64 = 5 * 60 * 1000;

type RefreshLock = Arc<tokio::sync::Mutex<()>>;

fn refresh_locks() -> &'static SyncMutex<HashMap<String, RefreshLock>> {
    static LOCKS: OnceLock<SyncMutex<HashMap<String, RefreshLock>>> = OnceLock::new();
    LOCKS.get_or_init(|| SyncMutex::new(HashMap::new()))
}

fn lock_for(provider_id: &str) -> RefreshLock {
    let mut map = refresh_locks().lock();
    map.entry(provider_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn needs_refresh(provider: &Provider) -> bool {
    if provider.refresh_token.as_deref().unwrap_or("").is_empty() {
        return false;
    }
    match (provider.kind, provider.auth_mode) {
        (ProviderKind::Anthropic, AuthMode::OauthClaudeCode) => {}
        _ => return false,
    }
    match provider.token_expires_at {
        Some(exp) => now_ms() + REFRESH_LEEWAY_MS >= exp,
        None => true,
    }
}

/// 如果 provider 的 OAuth token 即将过期，刷新并持久化到 providers.json。
/// 始终返回最新版本的 Provider（即便没刷新也可能从磁盘重读到他人刚写入的版本）。
pub async fn ensure_fresh_provider_token(
    data_dir: &Path,
    mut provider: Provider,
) -> AppResult<Provider> {
    if !needs_refresh(&provider) {
        return Ok(provider);
    }

    let lock = lock_for(&provider.id);
    let _guard = lock.lock().await;

    // double-check：可能在等锁期间别的任务已经刷过了
    let latest = config::get(data_dir, &provider.id).unwrap_or_else(|_| provider.clone());
    if !needs_refresh(&latest) {
        return Ok(latest);
    }
    provider = latest;

    let refresh_token = provider
        .refresh_token
        .clone()
        .expect("needs_refresh 已检查过 refresh_token 非空");

    let refreshed = match claude_oauth_refresh(&refresh_token).await {
        Ok(t) => t,
        Err(refresh_err) => {
            // refresh 失败的常见原因是 refresh_token 已被 Anthropic 服务端 revoke
            // （政策更新或长期未用）。此时如果用户本机装了 Claude Code 且最近登录过，
            // 直接从 keychain / ~/.claude/.credentials.json 读最新凭据顶上去，比让用户
            // 在 UI 里重走一遍 OAuth 友好得多。读取顺序与 claude_code_import 一致。
            tracing::warn!(
                error = %refresh_err,
                "Claude OAuth refresh 失败，尝试从本地 Claude Code 凭据恢复"
            );
            match claude_code_import() {
                Ok(imported) if !imported.access_token.is_empty() => {
                    tracing::info!("已从本地 Claude Code 凭据恢复 access_token");
                    imported
                }
                _ => return Err(refresh_err),
            }
        }
    };

    provider.api_key = refreshed.access_token;
    if let Some(rt) = refreshed.refresh_token.filter(|s| !s.is_empty()) {
        provider.refresh_token = Some(rt);
    }
    if refreshed.expires_at.is_some() {
        provider.token_expires_at = refreshed.expires_at;
    }

    let saved = config::upsert(data_dir, provider)?;
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn provider(refresh: Option<&str>, expires_at: Option<i64>) -> Provider {
        Provider {
            id: "anthropic-oauth".into(),
            name: "Claude".into(),
            kind: ProviderKind::Anthropic,
            enabled: true,
            auth_mode: AuthMode::OauthClaudeCode,
            base_url: "https://api.anthropic.com".into(),
            api_key: "old".into(),
            refresh_token: refresh.map(String::from),
            token_expires_at: expires_at,
            account_id: None,
            extra_headers: BTreeMap::new(),
            models: vec![],
            default_model: None,
        }
    }

    #[test]
    fn no_refresh_when_token_fresh() {
        let p = provider(Some("rt"), Some(now_ms() + 60 * 60 * 1000));
        assert!(!needs_refresh(&p));
    }

    #[test]
    fn refresh_when_within_leeway() {
        let p = provider(Some("rt"), Some(now_ms() + 60 * 1000));
        assert!(needs_refresh(&p));
    }

    #[test]
    fn refresh_when_expired() {
        let p = provider(Some("rt"), Some(now_ms() - 10_000));
        assert!(needs_refresh(&p));
    }

    #[test]
    fn no_refresh_when_no_refresh_token() {
        let p = provider(None, Some(now_ms() - 10_000));
        assert!(!needs_refresh(&p));
    }

    #[test]
    fn no_refresh_when_apikey_mode() {
        let mut p = provider(Some("rt"), Some(now_ms() - 10_000));
        p.auth_mode = AuthMode::ApiKey;
        assert!(!needs_refresh(&p));
    }
}
