use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use super::{Tool, ToolClass};
use async_trait::async_trait;
use platform::{AppError, AppResult};
use reqwest::{
    header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    StatusCode, Url,
};
use serde_json::{json, Value};

const MAX_URL_LENGTH: usize = 2_000;
const MAX_HTTP_CONTENT_LENGTH: usize = 10 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 10;
const MAX_MARKDOWN_LENGTH: usize = 100_000;
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone)]
struct NormalizedFetchUrl {
    original_url: Url,
    request_url: Url,
}

#[derive(Debug, Clone)]
struct FetchedContent {
    final_url: Url,
    bytes: usize,
    code: StatusCode,
    code_text: String,
    content_type: String,
    markdown: String,
}

#[derive(Debug, Clone)]
struct RedirectInfo {
    original_url: Url,
    redirect_url: Url,
    status: StatusCode,
}

#[derive(Debug, Clone)]
enum FetchOutcome {
    Content(FetchedContent),
    Redirect(RedirectInfo),
}

#[derive(Debug, Clone)]
struct CacheEntry {
    fetched_at: Instant,
    content: FetchedContent,
}

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL, convert readable web content to markdown, and return content relevant \
         to the provided prompt. HTTP URLs are upgraded to HTTPS. Cross-host redirects are \
         reported instead of followed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "format": "uri",
                    "description": "The fully-formed http:// or https:// URL to fetch"
                },
                "prompt": {
                    "type": "string",
                    "description": "What information to extract or focus on from the fetched content"
                }
            },
            "required": ["url", "prompt"]
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let url = input["url"]
            .as_str()
            .ok_or_else(|| AppError::msg("web_fetch: 缺少 url 参数"))?;
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| AppError::msg("web_fetch: 缺少 prompt 参数"))?
            .trim();
        if prompt.is_empty() {
            return Err(AppError::msg("web_fetch: prompt 不能为空"));
        }

        fetch_page(url, prompt).await
    }

    fn classify(&self, _input: &Value) -> ToolClass {
        ToolClass::Network
    }
}

async fn fetch_page(url: &str, prompt: &str) -> AppResult<String> {
    let normalized = normalize_fetch_url(url)?;
    let outcome = get_url_markdown_content(&normalized).await?;

    match outcome {
        FetchOutcome::Content(content) => Ok(format_fetch_result(prompt, &content)),
        FetchOutcome::Redirect(redirect) => Ok(format_redirect_result(prompt, &redirect)),
    }
}

fn normalize_fetch_url(url: &str) -> AppResult<NormalizedFetchUrl> {
    if url.len() > MAX_URL_LENGTH {
        return Err(AppError::msg(format!(
            "web_fetch: URL 超过最大长度 {MAX_URL_LENGTH}"
        )));
    }

    let original_url =
        Url::parse(url).map_err(|_| AppError::msg(format!("web_fetch: 无效 URL {url}")))?;
    match original_url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(AppError::msg(format!(
                "web_fetch: 不支持的 URL 协议 {scheme}"
            )))
        }
    }

    if !original_url.username().is_empty() || original_url.password().is_some() {
        return Err(AppError::msg("web_fetch: URL 不能包含用户名或密码凭据"));
    }

    let host = original_url
        .host_str()
        .ok_or_else(|| AppError::msg("web_fetch: URL 缺少 host"))?;
    if !is_public_fetch_host(host) {
        return Err(AppError::msg(format!(
            "web_fetch: 出于安全原因拒绝抓取非公网 host {host}"
        )));
    }

    let mut request_url = original_url.clone();
    if request_url.scheme() == "http" {
        request_url
            .set_scheme("https")
            .map_err(|_| AppError::msg("web_fetch: 无法将 HTTP URL 升级为 HTTPS"))?;
    }

    Ok(NormalizedFetchUrl {
        original_url,
        request_url,
    })
}

fn is_public_fetch_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".local") {
        return false;
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(ip) => {
                !(ip.is_private()
                    || ip.is_loopback()
                    || ip.is_link_local()
                    || ip.is_broadcast()
                    || ip.is_unspecified())
            }
            IpAddr::V6(ip) => {
                !(ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local())
            }
        };
    }

    host.contains('.')
}

async fn get_url_markdown_content(normalized: &NormalizedFetchUrl) -> AppResult<FetchOutcome> {
    if let Some(content) = get_cached_content(normalized.original_url.as_str()) {
        return Ok(FetchOutcome::Content(content));
    }

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; Hebbian/0.1; +https://github.com)")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(FETCH_TIMEOUT)
        .build()?;

    let outcome = get_with_permitted_redirects(&client, normalized).await?;
    if let FetchOutcome::Content(content) = &outcome {
        set_cached_content(normalized.original_url.as_str(), content.clone());
    }
    Ok(outcome)
}

async fn get_with_permitted_redirects(
    client: &reqwest::Client,
    normalized: &NormalizedFetchUrl,
) -> AppResult<FetchOutcome> {
    let mut current = normalized.request_url.clone();

    for _ in 0..=MAX_REDIRECTS {
        let response = client
            .get(current.clone())
            .header(
                "Accept",
                "text/markdown, text/html, text/plain, application/json, */*",
            )
            .send()
            .await?;
        let status = response.status();

        if is_manual_redirect(status) {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| AppError::msg("web_fetch: redirect 缺少 Location header"))?;
            let redirect_url = current
                .join(location)
                .map_err(|_| AppError::msg("web_fetch: 无效 redirect URL"))?;

            if is_permitted_redirect(&current, &redirect_url) {
                current = redirect_url;
                continue;
            }

            return Ok(FetchOutcome::Redirect(RedirectInfo {
                original_url: current,
                redirect_url,
                status,
            }));
        }

        if !status.is_success() {
            return Err(AppError::msg(format!(
                "web_fetch: HTTP {} from {}",
                status, current
            )));
        }

        if let Some(content_length) = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
        {
            if content_length > MAX_HTTP_CONTENT_LENGTH {
                return Err(AppError::msg(format!(
                    "web_fetch: 响应过大，超过 {} bytes",
                    MAX_HTTP_CONTENT_LENGTH
                )));
            }
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        if is_binary_content_type(&content_type) {
            return Err(AppError::msg(format!(
                "web_fetch: 暂不支持二进制内容类型 {content_type}"
            )));
        }

        let bytes = response.bytes().await?;
        if bytes.len() > MAX_HTTP_CONTENT_LENGTH {
            return Err(AppError::msg(format!(
                "web_fetch: 响应过大，超过 {} bytes",
                MAX_HTTP_CONTENT_LENGTH
            )));
        }

        let body = String::from_utf8_lossy(&bytes).to_string();
        let markdown = if content_type.to_ascii_lowercase().contains("html") {
            html_to_markdown(&body)
        } else {
            normalize_text(&decode_html_entities(&body))
        };

        return Ok(FetchOutcome::Content(FetchedContent {
            final_url: current,
            bytes: bytes.len(),
            code: status,
            code_text: status.canonical_reason().unwrap_or("").to_string(),
            content_type,
            markdown,
        }));
    }

    Err(AppError::msg(format!(
        "web_fetch: redirect 次数超过 {MAX_REDIRECTS}"
    )))
}

fn get_cached_content(url: &str) -> Option<FetchedContent> {
    let mut cache = content_cache().lock().ok()?;
    let entry = cache.get(url)?;
    if entry.fetched_at.elapsed() <= CACHE_TTL {
        return Some(entry.content.clone());
    }
    cache.remove(url);
    None
}

fn set_cached_content(url: &str, content: FetchedContent) {
    if let Ok(mut cache) = content_cache().lock() {
        cache.insert(
            url.to_string(),
            CacheEntry {
                fetched_at: Instant::now(),
                content,
            },
        );
    }
}

fn content_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_manual_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn is_permitted_redirect(original_url: &Url, redirect_url: &Url) -> bool {
    if original_url.scheme() != redirect_url.scheme() {
        return false;
    }
    if original_url.port_or_known_default() != redirect_url.port_or_known_default() {
        return false;
    }
    if !redirect_url.username().is_empty() || redirect_url.password().is_some() {
        return false;
    }

    let original_host = original_url
        .host_str()
        .unwrap_or("")
        .trim_start_matches("www.");
    let redirect_host = redirect_url
        .host_str()
        .unwrap_or("")
        .trim_start_matches("www.");
    original_host.eq_ignore_ascii_case(redirect_host)
}

#[cfg(test)]
fn is_permitted_redirect_url(original_url: &str, redirect_url: &str) -> bool {
    match (Url::parse(original_url), Url::parse(redirect_url)) {
        (Ok(original), Ok(redirect)) => is_permitted_redirect(&original, &redirect),
        _ => false,
    }
}

fn is_binary_content_type(content_type: &str) -> bool {
    let lower = content_type.to_ascii_lowercase();
    lower.starts_with("image/")
        || lower.starts_with("audio/")
        || lower.starts_with("video/")
        || lower.contains("application/octet-stream")
        || lower.contains("application/zip")
        || lower.contains("application/x-")
        || lower.contains("font/")
}

fn format_redirect_result(prompt: &str, redirect: &RedirectInfo) -> String {
    let status_text = redirect.status.canonical_reason().unwrap_or("");
    format!(
        "REDIRECT DETECTED: The URL redirects to a different host.\n\n\
         Original URL: {}\n\
         Redirect URL: {}\n\
         Status: {} {}\n\n\
         To complete the request, call web_fetch again with:\n\
         - url: \"{}\"\n\
         - prompt: \"{}\"",
        redirect.original_url,
        redirect.redirect_url,
        redirect.status.as_u16(),
        status_text,
        redirect.redirect_url,
        prompt
    )
}

fn format_fetch_result(prompt: &str, content: &FetchedContent) -> String {
    let markdown = if content.markdown.len() > MAX_MARKDOWN_LENGTH {
        format!(
            "{}\n\n[Content truncated due to length: showing first {} chars of {} chars]",
            content
                .markdown
                .chars()
                .take(MAX_MARKDOWN_LENGTH)
                .collect::<String>(),
            MAX_MARKDOWN_LENGTH,
            content.markdown.len()
        )
    } else {
        content.markdown.clone()
    };

    format!(
        "Fetched URL: {}\n\
         Status: {} {}\n\
         Bytes: {}\n\
         Content-Type: {}\n\
         Prompt: {}\n\n\
         Page content:\n{}",
        content.final_url,
        content.code.as_u16(),
        content.code_text,
        content.bytes,
        if content.content_type.is_empty() {
            "unknown"
        } else {
            content.content_type.as_str()
        },
        prompt,
        markdown
    )
}

fn html_to_markdown(html: &str) -> String {
    let mut html = html.to_string();
    for tag in ["script", "style", "noscript", "svg"] {
        html = remove_tag_block(&html, tag);
    }

    let mut output = String::with_capacity(html.len() / 2);
    let mut rest = html.as_str();
    let mut active_link: Option<(String, String)> = None;

    while let Some(tag_start) = rest.find('<') {
        push_text(&mut output, &mut active_link, &rest[..tag_start]);
        let after_start = &rest[tag_start + 1..];
        let Some(tag_end) = after_start.find('>') else {
            push_text(&mut output, &mut active_link, rest);
            rest = "";
            break;
        };

        let raw_tag = after_start[..tag_end].trim();
        handle_html_tag(raw_tag, &mut output, &mut active_link);
        rest = &after_start[tag_end + 1..];
    }
    push_text(&mut output, &mut active_link, rest);

    if let Some((href, text)) = active_link.take() {
        push_markdown_link(&mut output, &href, &text);
    }

    normalize_markdown(&decode_html_entities(&output))
}

fn push_text(output: &mut String, active_link: &mut Option<(String, String)>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some((_href, link_text)) = active_link.as_mut() {
        link_text.push_str(text);
    } else {
        output.push_str(text);
    }
}

fn handle_html_tag(raw_tag: &str, output: &mut String, active_link: &mut Option<(String, String)>) {
    let lower = raw_tag.to_ascii_lowercase();
    let is_closing = lower.starts_with('/');
    let name = lower
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('/');

    if is_closing && name == "a" {
        if let Some((href, text)) = active_link.take() {
            push_markdown_link(output, &href, &text);
        }
        return;
    }

    if !is_closing && name == "a" {
        if let Some(href) = extract_attr(raw_tag, "href") {
            *active_link = Some((decode_html_entities(&href), String::new()));
        }
        return;
    }

    match (is_closing, name) {
        (false, "h1") => output.push_str("\n# "),
        (false, "h2") => output.push_str("\n## "),
        (false, "h3") => output.push_str("\n### "),
        (false, "h4") => output.push_str("\n#### "),
        (false, "h5") => output.push_str("\n##### "),
        (false, "h6") => output.push_str("\n###### "),
        (true, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") => output.push_str("\n\n"),
        (false, "p" | "div" | "section" | "article" | "main" | "header" | "footer") => {
            output.push_str("\n\n")
        }
        (true, "p" | "div" | "section" | "article" | "main" | "header" | "footer") => {
            output.push_str("\n\n")
        }
        (false, "br") => output.push('\n'),
        (false, "li") => output.push_str("\n- "),
        (true, "li") => output.push('\n'),
        _ => {}
    }
}

fn push_markdown_link(output: &mut String, href: &str, text: &str) {
    let label = normalize_inline_text(&decode_html_entities(text));
    if label.is_empty() {
        return;
    }
    if href.trim().is_empty() {
        output.push_str(&label);
    } else {
        output.push_str(&format!("[{}]({})", label, href.trim()));
    }
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{attr}=");
    let start = lower.find(&needle)? + needle.len();
    let value = tag[start..].trim_start();
    let quote = value.chars().next()?;

    if quote == '"' || quote == '\'' {
        let end = value[1..].find(quote)?;
        Some(value[1..1 + end].to_string())
    } else {
        let end = value.find(char::is_whitespace).unwrap_or(value.len());
        Some(value[..end].trim_end_matches('/').to_string())
    }
}

fn normalize_markdown(text: &str) -> String {
    let mut lines = Vec::new();
    let mut previous_blank = true;

    for line in text.lines() {
        let normalized = normalize_inline_text(line);
        if normalized.is_empty() {
            if !previous_blank {
                lines.push(String::new());
                previous_blank = true;
            }
        } else {
            lines.push(normalized);
            previous_blank = false;
        }
    }

    lines.join("\n").trim().to_string()
}

fn normalize_text(text: &str) -> String {
    text.lines()
        .map(normalize_inline_text)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_inline_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "-")
        .replace("&ndash;", "-")
}

fn remove_tag_block(html: &str, tag: &str) -> String {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut result = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(start) = rest.to_ascii_lowercase().find(&open) {
        result.push_str(&rest[..start]);
        let lower_tail = rest[start..].to_ascii_lowercase();
        if let Some(end_offset) = lower_tail.find(&close) {
            rest = &rest[start + end_offset + close.len()..];
        } else {
            rest = "";
            break;
        }
    }

    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_schema_requires_prompt() {
        let schema = WebFetchTool.parameters_schema();

        assert_eq!(schema["required"], json!(["url", "prompt"]));
        assert!(schema["properties"]["prompt"].is_object());
    }

    #[test]
    fn normalize_fetch_url_rejects_credentials_and_upgrades_http() {
        assert!(normalize_fetch_url("https://user:pass@example.com").is_err());
        assert!(normalize_fetch_url("https://localhost/docs").is_err());

        let normalized = normalize_fetch_url("http://example.com/docs").unwrap();

        assert_eq!(normalized.request_url.as_str(), "https://example.com/docs");
    }

    #[test]
    fn redirect_policy_allows_only_same_site_redirects() {
        assert!(is_permitted_redirect_url(
            "https://example.com/docs",
            "https://www.example.com/guide"
        ));
        assert!(is_permitted_redirect_url(
            "https://www.example.com/docs",
            "https://example.com/guide"
        ));
        assert!(!is_permitted_redirect_url(
            "https://example.com/docs",
            "https://evil.example.net/guide"
        ));
        assert!(!is_permitted_redirect_url(
            "https://example.com/docs",
            "http://example.com/guide"
        ));
    }

    #[test]
    fn html_to_markdown_keeps_readable_structure_and_links() {
        let markdown = html_to_markdown(
            r#"
            <html>
              <body>
                <h1>Docs</h1>
                <p>Hello <a href="https://example.com/start">start</a>.</p>
                <ul><li>First</li><li>Second</li></ul>
                <script>hidden()</script>
              </body>
            </html>
            "#,
        );

        assert!(markdown.contains("# Docs"));
        assert!(markdown.contains("[start](https://example.com/start)"));
        assert!(markdown.contains("- First"));
        assert!(markdown.contains("- Second"));
        assert!(!markdown.contains("hidden"));
    }
}
