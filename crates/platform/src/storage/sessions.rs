use crate::attachments::MessageAttachment;
use crate::{AppError, AppResult};
use chrono::{TimeZone, Utc};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Marker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageMeta {
    Switch {
        from_provider: String,
        from_model: String,
        to_provider: String,
        to_model: String,
    },
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePart {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        arguments: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<MessageToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<MessagePart>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MessageMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default = "default_stream")]
    pub stream: bool,
    #[serde(default)]
    pub messages: Vec<Message>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn default_stream() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
    pub date: String,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    #[serde(flatten)]
    pub meta: SessionMeta,
    pub snippet: Option<String>,
    pub matched_in: &'static str,
}

fn now() -> i64 {
    Utc::now().timestamp_millis()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn date_string(ts_ms: i64) -> String {
    Utc.timestamp_millis_opt(ts_ms)
        .single()
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".into())
}

fn root_dir(data_dir: &Path) -> PathBuf {
    super::sessions_dir(data_dir)
}

fn all_session_files(data_dir: &Path) -> AppResult<Vec<PathBuf>> {
    let root = root_dir(data_dir);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            for sub in std::fs::read_dir(&path)? {
                let sub = sub?;
                if sub.path().extension().and_then(|s| s.to_str()) == Some("json") {
                    out.push(sub.path());
                }
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(out)
}

fn session_path_for(data_dir: &Path, s: &Session) -> AppResult<PathBuf> {
    let dir = root_dir(data_dir).join(date_string(s.created_at));
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}.json", s.id)))
}

fn find_session_file(data_dir: &Path, id: &str) -> AppResult<Option<PathBuf>> {
    let root = root_dir(data_dir);
    let flat = root.join(format!("{id}.json"));
    if flat.exists() {
        return Ok(Some(flat));
    }
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let candidate = entry.path().join(format!("{id}.json"));
        if candidate.exists() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

pub fn list(data_dir: &Path) -> AppResult<Vec<SessionMeta>> {
    let mut out = Vec::new();
    for file in all_session_files(data_dir)? {
        let s: Session = match super::read_json_required(&file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let count = s
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::Marker))
            .count();
        out.push(SessionMeta {
            id: s.id,
            title: s.title,
            provider_id: s.provider_id,
            model: s.model,
            created_at: s.created_at,
            updated_at: s.updated_at,
            message_count: count,
            date: date_string(s.created_at),
        });
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

pub fn load(data_dir: &Path, id: &str) -> AppResult<Session> {
    let path = find_session_file(data_dir, id)?
        .ok_or_else(|| AppError::msg(format!("session {id} not found")))?;
    super::read_json_required(&path)
}

pub fn save(data_dir: &Path, mut s: Session) -> AppResult<Session> {
    s.updated_at = now();
    let target = session_path_for(data_dir, &s)?;
    if let Some(old) = find_session_file(data_dir, &s.id)? {
        if old != target {
            let _ = std::fs::remove_file(&old);
        }
    }
    super::write_json(&target, &s)?;
    Ok(s)
}

pub fn delete(data_dir: &Path, id: &str) -> AppResult<()> {
    if let Some(path) = find_session_file(data_dir, id)? {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn create(
    data_dir: &Path,
    provider_id: String,
    model: String,
    system_prompt: Option<String>,
    prompt_id: Option<String>,
) -> AppResult<Session> {
    let session = Session {
        id: new_id(),
        title: "新对话".into(),
        provider_id,
        model,
        system_prompt,
        prompt_id,
        stream: true,
        messages: Vec::new(),
        created_at: now(),
        updated_at: now(),
    };
    save(data_dir, session)
}

pub fn append_message(data_dir: &Path, id: &str, msg: Message) -> AppResult<Session> {
    let mut s = load(data_dir, id)?;
    s.messages.push(msg);
    save(data_dir, s)
}

pub fn insert_switch_marker(data_dir: &Path, id: &str, meta: MessageMeta) -> AppResult<Session> {
    let mut s = load(data_dir, id)?;
    s.messages.push(Message {
        id: new_id(),
        role: Role::Marker,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: now(),
        meta: Some(meta),
    });
    save(data_dir, s)
}

pub fn fork(data_dir: &Path, session_id: &str, up_to_message_id: &str) -> AppResult<Session> {
    let src = load(data_dir, session_id)?;
    let mut msgs = Vec::new();
    for m in &src.messages {
        msgs.push(m.clone());
        if m.id == up_to_message_id {
            break;
        }
    }
    let new = Session {
        id: new_id(),
        title: format!("{} (分支)", src.title),
        provider_id: src.provider_id,
        model: src.model,
        system_prompt: src.system_prompt,
        prompt_id: src.prompt_id,
        stream: src.stream,
        messages: msgs,
        created_at: now(),
        updated_at: now(),
    };
    save(data_dir, new)
}

pub fn rename(data_dir: &Path, id: &str, title: String) -> AppResult<Session> {
    let mut s = load(data_dir, id)?;
    s.title = title;
    save(data_dir, s)
}

pub fn truncate_after(data_dir: &Path, id: &str, message_id: &str) -> AppResult<Session> {
    let mut s = load(data_dir, id)?;
    if let Some(idx) = s.messages.iter().position(|m| m.id == message_id) {
        s.messages.truncate(idx + 1);
    }
    save(data_dir, s)
}

pub fn truncate_inclusive(data_dir: &Path, id: &str, message_id: &str) -> AppResult<Session> {
    let mut s = load(data_dir, id)?;
    if let Some(idx) = s.messages.iter().position(|m| m.id == message_id) {
        s.messages.truncate(idx);
    }
    save(data_dir, s)
}

enum SearchMatcher {
    Literal {
        needle: String,
        case_sensitive: bool,
    },
    Regex(Regex),
}

impl SearchMatcher {
    fn new(query: &str, case_sensitive: bool, regex: bool) -> Option<Self> {
        if regex {
            let re = RegexBuilder::new(query)
                .case_insensitive(!case_sensitive)
                .build()
                .ok()?;
            return Some(Self::Regex(re));
        }

        Some(Self::Literal {
            needle: if case_sensitive {
                query.to_string()
            } else {
                query.to_lowercase()
            },
            case_sensitive,
        })
    }

    fn find(&self, text: &str) -> Option<(usize, usize)> {
        match self {
            SearchMatcher::Literal {
                needle,
                case_sensitive,
            } => {
                let haystack = if *case_sensitive {
                    text.to_string()
                } else {
                    text.to_lowercase()
                };
                haystack
                    .find(needle)
                    .map(|start| (start, start + needle.len()))
            }
            SearchMatcher::Regex(re) => re.find(text).map(|m| (m.start(), m.end())),
        }
    }
}

pub fn search(
    data_dir: &Path,
    query: &str,
    case_sensitive: bool,
    regex: bool,
) -> AppResult<Vec<SearchHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(list(data_dir)?
            .into_iter()
            .map(|m| SearchHit {
                meta: m,
                snippet: None,
                matched_in: "",
            })
            .collect());
    }
    let matcher = match SearchMatcher::new(q, case_sensitive, regex) {
        Some(matcher) => matcher,
        None => return Ok(Vec::new()),
    };

    let mut hits = Vec::new();
    for file in all_session_files(data_dir)? {
        let s: Session = match super::read_json_required(&file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let count = s
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::Marker))
            .count();
        let title_hit = matcher.find(&s.title).is_some();
        let content_hit = s.messages.iter().find_map(|m| {
            if matches!(m.role, Role::Marker) {
                return None;
            }
            matcher
                .find(&m.content)
                .map(|(start, end)| (m.content.clone(), start, end))
        });
        if !title_hit && content_hit.is_none() {
            continue;
        }
        let snippet = content_hit
            .as_ref()
            .map(|(content, start, end)| make_snippet_from_range(content, *start, *end, 60));
        hits.push(SearchHit {
            meta: SessionMeta {
                id: s.id,
                title: s.title,
                provider_id: s.provider_id,
                model: s.model,
                created_at: s.created_at,
                updated_at: s.updated_at,
                message_count: count,
                date: date_string(s.created_at),
            },
            snippet,
            matched_in: if title_hit { "title" } else { "content" },
        });
    }
    hits.sort_by(|a, b| b.meta.updated_at.cmp(&a.meta.updated_at));
    Ok(hits)
}

fn make_snippet_from_range(content: &str, start: usize, end: usize, ctx: usize) -> String {
    let chars: Vec<(usize, char)> = content.char_indices().collect();
    let start_pos = chars.iter().position(|(i, _)| *i >= start).unwrap_or(0);
    let end_pos = chars
        .iter()
        .position(|(i, _)| *i >= end)
        .unwrap_or(chars.len());
    make_snippet_from_char_range(content, start_pos, end_pos, ctx)
}

fn make_snippet_from_char_range(
    content: &str,
    start_pos: usize,
    end_pos: usize,
    ctx: usize,
) -> String {
    let chars: Vec<(usize, char)> = content.char_indices().collect();
    let start_char = start_pos.saturating_sub(ctx);
    let end_char = (end_pos + ctx).min(chars.len());
    let start_byte = chars.get(start_char).map(|(i, _)| *i).unwrap_or(0);
    let end_byte = chars
        .get(end_char)
        .map(|(i, _)| *i)
        .unwrap_or(content.len());
    let mut out = String::new();
    if start_byte > 0 {
        out.push('…');
    }
    out.push_str(&content[start_byte..end_byte]);
    if end_byte < content.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hebbian-sessions-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp data dir");
        dir
    }

    fn save_session(data_dir: &Path, title: &str, content: &str) -> Session {
        let session = create(
            data_dir,
            "openai".to_string(),
            "gpt-test".to_string(),
            None,
            None,
        )
        .expect("create session");
        rename(data_dir, &session.id, title.to_string()).expect("rename session");
        append_message(
            data_dir,
            &session.id,
            Message {
                id: new_id(),
                role: Role::User,
                content: content.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                parts: Vec::new(),
                created_at: now(),
                meta: None,
            },
        )
        .expect("append message")
    }

    #[test]
    fn regex_search_matches_titles_and_message_content() {
        let dir = temp_data_dir("regex-global");
        let title_hit = save_session(&dir, "Release 2026 Notes", "nothing here");
        let content_hit = save_session(&dir, "Planning", "error 502 happened");
        save_session(&dir, "Scratch", "error abc happened");

        let hits = search(&dir, r"\d{3}", false, true).expect("regex search");
        let ids: Vec<_> = hits.iter().map(|hit| hit.meta.id.as_str()).collect();

        assert!(ids.contains(&title_hit.id.as_str()));
        assert!(ids.contains(&content_hit.id.as_str()));
        assert_eq!(ids.len(), 2);
        assert_eq!(
            hits.iter()
                .find(|hit| hit.meta.id == title_hit.id)
                .expect("title hit")
                .matched_in,
            "title"
        );
        assert_eq!(
            hits.iter()
                .find(|hit| hit.meta.id == content_hit.id)
                .expect("content hit")
                .matched_in,
            "content"
        );
    }

    #[test]
    fn regex_search_respects_case_sensitivity() {
        let dir = temp_data_dir("regex-case");
        let session = save_session(&dir, "Build", "Error 500 happened");

        let insensitive = search(&dir, "error \\d+", false, true).expect("search insensitive");
        assert_eq!(insensitive.len(), 1);
        assert_eq!(insensitive[0].meta.id, session.id);

        let sensitive = search(&dir, "error \\d+", true, true).expect("search sensitive");
        assert!(sensitive.is_empty());
    }
}
