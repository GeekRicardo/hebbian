//! 内置浏览器的 URL 归一化与两档安全校验（架构 §8.5-4）。
//!
//! Rust 侧是安全边界：`on_navigation` 回调里强制执行，页面内跳转同样拦截；
//! 前端 previewUrl.ts 的同名校验只是 UX。
//!
//! ⚠️ 共享 case 清单：本文件单测与
//! apps/desktop/frontend/src/desktop/ui/lib/previewUrl.test.ts 的「两档校验」
//! 用例一一对应，改任何一侧必须同步另一侧。

use tauri::Url;

/// 导航来源档位：自动通道只允许本地网段，用户主动导航放行公网 http(s)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewOrigin {
    /// 聊天流检测 / auto-follow / agent 触发
    Auto,
    /// 用户在地址栏输入或页面内点击
    User,
}

fn strip_brackets(host: &str) -> &str {
    host.trim_start_matches('[').trim_end_matches(']')
}

fn parse_ipv4(host: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        octets[i] = part.parse().ok()?;
    }
    Some(octets)
}

/// 本地网段：localhost / *.localhost / host.docker.internal / *.local /
/// ::1 / 127.x / 10.x / 172.16-31.x / 192.168.x
pub fn is_local_preview_host(hostname: &str) -> bool {
    let host = strip_brackets(hostname.trim()).to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host == "host.docker.internal"
        || host.ends_with(".local")
        || host == "::1"
        || host == "0.0.0.0" // dev server bind-all，归一化时重写成 127.0.0.1
        || host == "::"
    {
        return true;
    }
    match parse_ipv4(&host) {
        Some([a, b, _, _]) => {
            a == 10 || a == 127 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
        }
        None => false,
    }
}

/// 探测式地址硬黑名单：169.254.0.0/16 链路本地段（含云元数据 169.254.169.254）
/// 与 GCP 元数据域名。任何档位都拒绝。
pub fn is_blocked_probe_host(hostname: &str) -> bool {
    let host = strip_brackets(hostname.trim()).to_ascii_lowercase();
    if host == "metadata.google.internal" || host == "metadata.goog" {
        return true;
    }
    matches!(parse_ipv4(&host), Some([169, 254, _, _]))
}

/// 宽松输入 → 规范 URL。规则与前端 normalizePreviewUrlInput 一致：
/// 纯端口数字补 127.0.0.1、无 scheme 补 http、0.0.0.0/:: 重写、仅 http(s)。
pub fn normalize_preview_url(input: &str) -> Option<Url> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.len() >= 2
        && trimmed.len() <= 5
        && trimmed.bytes().all(|b| b.is_ascii_digit())
    {
        format!("http://127.0.0.1:{trimmed}")
    } else if !trimmed.contains("://") {
        // 像真浏览器一样补 scheme：本地地址用 http，公网域名默认 https
        let bare_host = trimmed.split('/').next().unwrap_or(trimmed).split(':').next().unwrap_or(trimmed);
        let scheme = if is_local_preview_host(bare_host) { "http" } else { "https" };
        format!("{scheme}://{trimmed}")
    } else {
        trimmed.to_string()
    };
    let mut url = Url::parse(&with_scheme).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_string();
    if strip_brackets(&host) == "0.0.0.0" || strip_brackets(&host) == "::" {
        url.set_host(Some("127.0.0.1")).ok()?;
    }
    Some(url)
}

/// 两档校验入口。通过返回规范化 Url，拒绝返回 Err(人话原因)。
pub fn validate_preview_url(input: &str, origin: PreviewOrigin) -> Result<Url, String> {
    let url = normalize_preview_url(input).ok_or_else(|| "这个地址没法打开".to_string())?;
    let host = url.host_str().unwrap_or_default().to_string();
    if is_blocked_probe_host(&host) {
        return Err("这个地址不允许访问".to_string());
    }
    if origin == PreviewOrigin::Auto && !is_local_preview_host(&host) {
        return Err("只有本地开发地址会自动打开，外部网址请在地址栏自己输入".to_string());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rules() {
        assert_eq!(
            normalize_preview_url("3000").unwrap().as_str(),
            "http://127.0.0.1:3000/"
        );
        assert_eq!(
            normalize_preview_url("localhost:5173").unwrap().as_str(),
            "http://localhost:5173/"
        );
        // 像真浏览器：公网域名默认 https，本地用 http
        assert_eq!(
            normalize_preview_url("example.com").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            normalize_preview_url("example.com:8443/app").unwrap().as_str(),
            "https://example.com:8443/app"
        );
        assert_eq!(
            normalize_preview_url("192.168.1.5:8080").unwrap().as_str(),
            "http://192.168.1.5:8080/"
        );
        assert_eq!(
            normalize_preview_url("http://0.0.0.0:5173").unwrap().as_str(),
            "http://127.0.0.1:5173/"
        );
        assert_eq!(
            normalize_preview_url("127.0.0.1:3000/settings?a=1")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:3000/settings?a=1"
        );
        assert!(normalize_preview_url("ftp://127.0.0.1/x").is_none());
        assert!(normalize_preview_url("   ").is_none());
        assert!(normalize_preview_url("http://").is_none());
    }

    #[test]
    fn local_hosts() {
        for host in [
            "localhost",
            "app.localhost",
            "host.docker.internal",
            "mymac.local",
            "127.0.0.1",
            "127.1.2.3",
            "10.0.0.8",
            "172.16.0.2",
            "172.31.255.1",
            "192.168.1.10",
            "::1",
        ] {
            assert!(is_local_preview_host(host), "{host} should be local");
        }
        for host in ["172.32.0.1", "example.com", "8.8.8.8"] {
            assert!(!is_local_preview_host(host), "{host} should not be local");
        }
    }

    #[test]
    fn blocked_probe_hosts() {
        assert!(is_blocked_probe_host("169.254.169.254"));
        assert!(is_blocked_probe_host("169.254.1.1"));
        assert!(is_blocked_probe_host("metadata.google.internal"));
        assert!(!is_blocked_probe_host("example.com"));
    }

    #[test]
    fn two_tier_validation() {
        // 与 previewUrl.test.ts「两档校验」case 一一对应
        assert_eq!(
            validate_preview_url("localhost:3000", PreviewOrigin::Auto)
                .unwrap()
                .as_str(),
            "http://localhost:3000/"
        );
        assert!(validate_preview_url("https://example.com", PreviewOrigin::Auto).is_err());
        assert_eq!(
            validate_preview_url("https://example.com", PreviewOrigin::User)
                .unwrap()
                .as_str(),
            "https://example.com/"
        );
        assert_eq!(
            validate_preview_url("3000", PreviewOrigin::User).unwrap().as_str(),
            "http://127.0.0.1:3000/"
        );
        assert!(validate_preview_url("http://169.254.169.254/latest", PreviewOrigin::User).is_err());
        assert!(validate_preview_url("169.254.169.254", PreviewOrigin::Auto).is_err());
        assert!(validate_preview_url("ftp://example.com", PreviewOrigin::User).is_err());
        assert_eq!(
            validate_preview_url("0.0.0.0:5173", PreviewOrigin::Auto)
                .unwrap()
                .as_str(),
            "http://127.0.0.1:5173/"
        );
    }
}
