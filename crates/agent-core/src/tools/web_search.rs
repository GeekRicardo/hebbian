use std::time::Instant;

use super::Tool;
use async_trait::async_trait;
use platform::{AppError, AppResult};
use reqwest::Url;
use serde_json::{json, Value};

const MAX_SEARCH_RESULTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchInput {
    query: String,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchHit {
    title: String,
    url: String,
}

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for current information and return source links. Supports optional \
         allowed_domains or blocked_domains filters. Use this for recent information or facts \
         that need web sources."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 2,
                    "description": "The search query to use"
                },
                "allowed_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only include search results from these domains"
                },
                "blocked_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Never include search results from these domains"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let input = parse_search_input(&input)?;
        search_duckduckgo(&input).await
    }
}

fn parse_search_input(input: &Value) -> AppResult<SearchInput> {
    let query = input["query"]
        .as_str()
        .ok_or_else(|| AppError::msg("web_search: 缺少 query 参数"))?
        .trim()
        .to_string();
    if query.len() < 2 {
        return Err(AppError::msg("web_search: query 至少需要 2 个字符"));
    }

    let allowed_domains = parse_domain_list(input.get("allowed_domains"))?;
    let blocked_domains = parse_domain_list(input.get("blocked_domains"))?;
    if !allowed_domains.is_empty() && !blocked_domains.is_empty() {
        return Err(AppError::msg(
            "web_search: allowed_domains 和 blocked_domains 不能同时指定",
        ));
    }

    Ok(SearchInput {
        query,
        allowed_domains,
        blocked_domains,
    })
}

fn parse_domain_list(value: Option<&Value>) -> AppResult<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(AppError::msg("web_search: 域名过滤参数必须是字符串数组"));
    };

    let mut domains = Vec::new();
    for item in items {
        let domain = item
            .as_str()
            .ok_or_else(|| AppError::msg("web_search: 域名过滤参数必须是字符串数组"))?;
        let normalized = normalize_domain(domain)?;
        if !domains.iter().any(|existing| existing == &normalized) {
            domains.push(normalized);
        }
    }
    Ok(domains)
}

fn normalize_domain(domain: &str) -> AppResult<String> {
    let trimmed = domain
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_matches('/');
    if trimmed.is_empty() {
        return Err(AppError::msg("web_search: 域名不能为空"));
    }

    let candidate = format!("https://{trimmed}");
    let parsed = Url::parse(&candidate)
        .map_err(|_| AppError::msg(format!("web_search: 无效域名 {domain}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::msg(format!("web_search: 无效域名 {domain}")))?
        .to_ascii_lowercase();

    if !host.contains('.') {
        return Err(AppError::msg(format!(
            "web_search: 域名必须包含顶级域名 {domain}"
        )));
    }
    Ok(host)
}

async fn search_duckduckgo(input: &SearchInput) -> AppResult<String> {
    let start = Instant::now();
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; Hebbian/0.1; +https://github.com)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let url = build_search_url(input);
    let response = client.get(&url).send().await?;
    if !response.status().is_success() {
        return Err(AppError::msg(format!(
            "web_search: HTTP {}",
            response.status()
        )));
    }

    let html = response.text().await?;
    let hits = filter_search_hits(parse_duckduckgo_html(&html), input);

    if hits.is_empty() {
        return search_duckduckgo_instant_answer(input, start).await;
    }

    Ok(format_search_results(
        input,
        &hits,
        start.elapsed().as_secs_f64(),
    ))
}

fn build_search_url(input: &SearchInput) -> String {
    let mut query = input.query.clone();
    if !input.allowed_domains.is_empty() {
        let sites = input
            .allowed_domains
            .iter()
            .map(|domain| format!("site:{domain}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        query.push(' ');
        query.push_str(&sites);
    }
    for domain in &input.blocked_domains {
        query.push_str(" -site:");
        query.push_str(domain);
    }

    format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(&query)
    )
}

async fn search_duckduckgo_instant_answer(
    input: &SearchInput,
    start: Instant,
) -> AppResult<String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; Hebbian/0.1; +https://github.com)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
        urlencoding::encode(&input.query)
    );
    let response = client.get(&url).send().await?;
    if !response.status().is_success() {
        return Err(AppError::msg(format!(
            "web_search: HTTP {}",
            response.status()
        )));
    }

    let json: Value = response.json().await?;
    let mut hits = Vec::new();
    if let Some(topics) = json["RelatedTopics"].as_array() {
        collect_instant_answer_topics(topics, &mut hits);
    }
    let hits = filter_search_hits(hits, input);
    if !hits.is_empty() {
        return Ok(format_search_results(
            input,
            &hits,
            start.elapsed().as_secs_f64(),
        ));
    }

    let abstract_text = json["AbstractText"].as_str().unwrap_or("").trim();
    let answer = json["Answer"].as_str().unwrap_or("").trim();
    let source = json["AbstractSource"].as_str().unwrap_or("").trim();
    let source_url = json["AbstractURL"].as_str().unwrap_or("").trim();

    let mut result = format!(
        "Web search results for query: \"{}\"\nDuration: {:.2}s\n\n",
        input.query,
        start.elapsed().as_secs_f64()
    );
    if !answer.is_empty() {
        result.push_str(&format!("Answer: {answer}\n\n"));
    }
    if !abstract_text.is_empty() {
        if source.is_empty() {
            result.push_str(abstract_text);
            result.push_str("\n\n");
        } else {
            result.push_str(&format!("{source}: {abstract_text}\n\n"));
        }
    }
    if !source_url.is_empty() {
        result.push_str("Sources:\n");
        result.push_str(&format!(
            "- [{}]({})\n\n",
            if source.is_empty() {
                source_url
            } else {
                source
            },
            source_url
        ));
    } else {
        result.push_str("Sources:\n");
        result.push_str(&format!(
            "- [DuckDuckGo results](https://duckduckgo.com/?q={})\n\n",
            urlencoding::encode(&input.query)
        ));
    }
    result.push_str("REMINDER: 必须在回答中引用这些来源，并使用 markdown 链接。");
    Ok(result.trim().to_string())
}

fn collect_instant_answer_topics(topics: &[Value], hits: &mut Vec<SearchHit>) {
    for topic in topics {
        if let (Some(title), Some(url)) = (topic["Text"].as_str(), topic["FirstURL"].as_str()) {
            hits.push(SearchHit {
                title: title.to_string(),
                url: url.to_string(),
            });
        }
        if let Some(nested) = topic["Topics"].as_array() {
            collect_instant_answer_topics(nested, hits);
        }
    }
}

fn parse_duckduckgo_html(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut rest = html;

    while let Some(anchor_start) = rest.find("<a") {
        rest = &rest[anchor_start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..=tag_end];
        let after_tag = &rest[tag_end + 1..];
        let Some(anchor_end) = after_tag.find("</a>") else {
            break;
        };
        let body = &after_tag[..anchor_end];
        rest = &after_tag[anchor_end + "</a>".len()..];

        if !tag.contains("result__a") {
            continue;
        }
        let Some(raw_href) = extract_attr(tag, "href") else {
            continue;
        };
        let Some(url) = normalize_result_url(&raw_href) else {
            continue;
        };
        let title = html_to_text(body);
        if title.is_empty() {
            continue;
        }

        hits.push(SearchHit { title, url });
    }

    dedupe_hits(hits)
        .into_iter()
        .take(MAX_SEARCH_RESULTS)
        .collect()
}

fn normalize_result_url(raw_href: &str) -> Option<String> {
    let decoded = decode_html_entities(raw_href);
    let href = decoded.trim();
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') {
        format!("https://duckduckgo.com{href}")
    } else {
        href.to_string()
    };

    let parsed = Url::parse(&absolute).ok()?;
    if parsed
        .host_str()
        .is_some_and(|host| host.ends_with("duckduckgo.com"))
        && parsed.path().starts_with("/l/")
    {
        for (key, value) in parsed.query_pairs() {
            if key == "uddg" {
                return Some(value.into_owned());
            }
        }
    }

    Some(parsed.to_string())
}

fn filter_search_hits(hits: Vec<SearchHit>, input: &SearchInput) -> Vec<SearchHit> {
    hits.into_iter()
        .filter(|hit| {
            if !input.allowed_domains.is_empty()
                && !input
                    .allowed_domains
                    .iter()
                    .any(|domain| url_matches_domain(&hit.url, domain))
            {
                return false;
            }
            if input
                .blocked_domains
                .iter()
                .any(|domain| url_matches_domain(&hit.url, domain))
            {
                return false;
            }
            true
        })
        .take(MAX_SEARCH_RESULTS)
        .collect()
}

fn url_matches_domain(url: &str, domain: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn format_search_results(input: &SearchInput, hits: &[SearchHit], duration_seconds: f64) -> String {
    let mut result = format!(
        "Web search results for query: \"{}\"\nDuration: {:.2}s\n\nSources:\n",
        input.query, duration_seconds
    );

    if hits.is_empty() {
        result.push_str(&format!(
            "- [DuckDuckGo results](https://duckduckgo.com/?q={})\n",
            urlencoding::encode(&input.query)
        ));
    } else {
        for hit in hits {
            result.push_str(&format!("- [{}]({})\n", hit.title, hit.url));
        }
    }

    result.push_str("\nREMINDER: 必须在回答中引用这些来源，并使用 markdown 链接。");
    result
}

fn dedupe_hits(hits: Vec<SearchHit>) -> Vec<SearchHit> {
    let mut deduped = Vec::new();
    for hit in hits {
        if !deduped
            .iter()
            .any(|existing: &SearchHit| existing.url == hit.url)
        {
            deduped.push(hit);
        }
    }
    deduped
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

fn html_to_text(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            _ => text.push(ch),
        }
    }

    decode_html_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_schema_exposes_domain_filters() {
        let schema = WebSearchTool.parameters_schema();

        assert_eq!(schema["required"], json!(["query"]));
        assert!(schema["properties"]["allowed_domains"].is_object());
        assert!(schema["properties"]["blocked_domains"].is_object());
    }

    #[test]
    fn parse_search_input_rejects_conflicting_domain_filters() {
        let input = json!({
            "query": "rust async traits",
            "allowed_domains": ["doc.rust-lang.org"],
            "blocked_domains": ["example.com"]
        });

        assert!(parse_search_input(&input).is_err());
    }

    #[test]
    fn duckduckgo_html_parser_extracts_result_links() {
        let html = r#"
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F&amp;rut=abc">
              The Rust Programming Language
            </a>
            <a class="result__a" href="https://example.com/nope">Ignored</a>
        "#;

        let hits = parse_duckduckgo_html(html);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "The Rust Programming Language");
        assert_eq!(hits[0].url, "https://doc.rust-lang.org/book/");
    }

    #[test]
    fn domain_filters_and_formatting_keep_sources_visible() {
        let hits = vec![
            SearchHit {
                title: "Rust Book".into(),
                url: "https://doc.rust-lang.org/book/".into(),
            },
            SearchHit {
                title: "Blocked".into(),
                url: "https://example.com/page".into(),
            },
        ];
        let input = SearchInput {
            query: "rust ownership".into(),
            allowed_domains: vec!["doc.rust-lang.org".into()],
            blocked_domains: Vec::new(),
        };

        let filtered = filter_search_hits(hits, &input);
        let formatted = format_search_results(&input, &filtered, 1.25);

        assert_eq!(filtered.len(), 1);
        assert!(formatted.contains("Sources:"));
        assert!(formatted.contains("[Rust Book](https://doc.rust-lang.org/book/)"));
        assert!(formatted.contains("必须在回答中引用这些来源"));
    }
}
