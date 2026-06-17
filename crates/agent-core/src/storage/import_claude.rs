//! 反向：读 Claude Code 的会话文件，重建成本侧 Session（导入后可在本侧继续聊）。
//!
//! 与 [`super::export_claude`] 对称。难点同样在工具调用：Claude 把 `tool_use`（assistant）
//! 与 `tool_result`（紧跟的 user）分成两行，本侧则把结果内联在 assistant 的工具调用里——
//! 这里负责按 `tool_use_id` 把 result 回填到对应 assistant 消息，tool_result 行本身不产生
//! 独立消息。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use common::AppResult;
use serde_json::Value;

use super::sessions::{self, Message, MessagePart, MessageToolCall, Role};

/// 标题最大长度（取首条用户消息兜底时截断）。
const TITLE_MAX: usize = 40;

/// 扫描得到的一个可导入会话概要（不解析全部消息，仅够列表展示）。
pub struct ClaudeSessionInfo {
    pub path: PathBuf,
    pub uuid: String,
    pub title: String,
    /// 原会话工作目录；列表按它分组，导入后作为本侧 workdir。
    pub cwd: String,
    pub message_count: usize,
    pub modified_ms: i64,
}

/// 完整解析结果：重建出的会话内容。
pub struct ParsedClaudeSession {
    pub title: String,
    pub workdir: Option<PathBuf>,
    pub model: String,
    pub messages: Vec<Message>,
}

/// 扫描 claude projects 根目录，列出所有可导入会话，按修改时间倒序。
/// 目录不存在 / 读不动时返回空列表（不报错，UI 显示「没有可导入的会话」即可）。
pub fn list_importable(projects_dir: &Path) -> AppResult<Vec<ClaudeSessionInfo>> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(projects_dir) else {
        return Ok(out);
    };
    for proj in projects.flatten() {
        if !proj.path().is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(info) = scan_one(&p) {
                out.push(info);
            }
        }
    }
    out.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    Ok(out)
}

/// 轻量扫描单个文件：只取标题 / cwd / 消息数 / mtime，不重建消息。
fn scan_one(path: &Path) -> Option<ClaudeSessionInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    let uuid = path.file_stem()?.to_string_lossy().to_string();

    let mut custom_title: Option<String> = None;
    let mut first_user_title: Option<String> = None;
    let mut cwd = String::new();
    let mut count = 0usize;

    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                custom_title = v
                    .get("customTitle")
                    .and_then(Value::as_str)
                    .map(String::from);
            }
            Some("user") | Some("assistant") => {
                count += 1;
                if cwd.is_empty() {
                    if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                        cwd = c.to_string();
                    }
                }
                if first_user_title.is_none()
                    && v.get("type").and_then(Value::as_str) == Some("user")
                {
                    if let Some(t) = user_text(&v) {
                        if !t.trim().is_empty() {
                            first_user_title = Some(truncate_title(&t));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if count == 0 {
        return None; // 空会话不列
    }
    let modified_ms = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    Some(ClaudeSessionInfo {
        path: path.to_path_buf(),
        uuid,
        title: custom_title
            .or(first_user_title)
            .unwrap_or_else(|| "（无标题）".into()),
        cwd,
        message_count: count,
        modified_ms,
    })
}

/// 完整解析一个 Claude 会话文件内容，重建本侧消息序列。
pub fn parse_claude_jsonl(content: &str) -> AppResult<ParsedClaudeSession> {
    let mut messages: Vec<Message> = Vec::new();
    let mut custom_title: Option<String> = None;
    let mut first_user_title: Option<String> = None;
    let mut cwd = String::new();
    let mut model = String::new();
    // tool_use_id → 它所属 assistant 消息在 messages 里的下标，用于回填 result。
    let mut owner: HashMap<String, usize> = HashMap::new();
    let mut seq = 0i64; // 无 timestamp 时的单调兜底

    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let typ = v.get("type").and_then(Value::as_str).unwrap_or("");
        if cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                cwd = c.to_string();
            }
        }
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso_millis)
            .unwrap_or_else(|| {
                seq += 1;
                seq
            });

        match typ {
            "custom-title" => {
                custom_title = v
                    .get("customTitle")
                    .and_then(Value::as_str)
                    .map(String::from);
            }
            "user" => {
                // 先看是不是工具结果回传：是则回填到 owner，不产生独立消息。
                if let Some(arr) = v.pointer("/message/content").and_then(Value::as_array) {
                    let results: Vec<(String, String)> = arr
                        .iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                        .filter_map(|b| {
                            let id = b.get("tool_use_id")?.as_str()?.to_string();
                            Some((id, tool_result_text(b.get("content"))))
                        })
                        .collect();
                    if !results.is_empty() {
                        for (id, text) in results {
                            if let Some(&idx) = owner.get(&id) {
                                fill_result(&mut messages[idx], &id, text);
                            }
                        }
                        continue;
                    }
                }
                let text = user_text(&v).unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }
                if first_user_title.is_none() {
                    first_user_title = Some(truncate_title(&text));
                }
                messages.push(plain_message(Role::User, text, ts));
            }
            "assistant" => {
                if model.is_empty() {
                    if let Some(m) = v.pointer("/message/model").and_then(Value::as_str) {
                        model = m.to_string();
                    }
                }
                let (msg, tool_ids) = assistant_message(&v, ts);
                if msg.content.is_empty() && msg.parts.is_empty() && msg.tool_calls.is_empty() {
                    continue;
                }
                let idx = messages.len();
                for id in tool_ids {
                    owner.insert(id, idx);
                }
                messages.push(msg);
            }
            _ => {} // system / attachment / mode / file-history-snapshot 等不进上下文
        }
    }

    let workdir = (!cwd.trim().is_empty()).then(|| PathBuf::from(&cwd));
    Ok(ParsedClaudeSession {
        title: custom_title
            .or(first_user_title)
            .unwrap_or_else(|| "（导入）".into()),
        workdir,
        model: if model.is_empty() {
            "claude".into()
        } else {
            model
        },
        messages,
    })
}

/// 从一行 user 记录里抽出纯文本（content 为 string 或 text 块数组）。
fn user_text(v: &Value) -> Option<String> {
    let c = v.pointer("/message/content")?;
    if let Some(s) = c.as_str() {
        return Some(s.to_string());
    }
    let arr = c.as_array()?;
    let text = arr
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

/// 把一行 assistant 记录拆成本侧 Message + 它发起的 tool_use_id 列表（供回填）。
fn assistant_message(v: &Value, ts: i64) -> (Message, Vec<String>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut parts: Vec<MessagePart> = Vec::new();
    let mut tool_calls: Vec<MessageToolCall> = Vec::new();
    let mut tool_ids: Vec<String> = Vec::new();

    if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                        if !t.trim().is_empty() {
                            parts.push(MessagePart::Reasoning {
                                text: t.to_string(),
                            });
                        }
                    }
                }
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        if !t.trim().is_empty() {
                            text_parts.push(t.to_string());
                            parts.push(MessagePart::Text {
                                text: t.to_string(),
                            });
                        }
                    }
                }
                Some("tool_use") => {
                    let id = b
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = b
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let input = b.get("input").cloned().unwrap_or(Value::Null);
                    parts.push(MessagePart::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                        arguments: String::new(),
                        result: None,
                        duration_ms: None,
                        is_error: false,
                    });
                    tool_calls.push(MessageToolCall {
                        id: id.clone(),
                        name,
                        input,
                        result: None,
                        duration_ms: None,
                        is_error: false,
                        nested: Vec::new(),
                    });
                    tool_ids.push(id);
                }
                _ => {}
            }
        }
    }

    let mut msg = plain_message(Role::Assistant, text_parts.join("\n\n"), ts);
    msg.parts = parts;
    msg.tool_calls = tool_calls;
    (msg, tool_ids)
}

/// 把工具结果回填到 assistant 消息里对应 id 的 tool_call 与 part。
fn fill_result(msg: &mut Message, id: &str, text: String) {
    if let Some(tc) = msg.tool_calls.iter_mut().find(|tc| tc.id == id) {
        tc.result = Some(text.clone());
    }
    for part in &mut msg.parts {
        if let MessagePart::ToolCall {
            id: pid, result, ..
        } = part
        {
            if pid == id {
                *result = Some(text);
                break;
            }
        }
    }
}

/// tool_result 的 content：可能是字符串，或 `[{type:text,text}]` 块数组。
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn plain_message(role: Role, content: String, ts: i64) -> Message {
    Message {
        id: sessions::new_id(),
        role,
        content,
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        parts: Vec::new(),
        created_at: ts,
        meta: None,
        subagent_call_id: None,
        run_duration_ms: None,
    }
}

/// ISO8601（`2026-05-29T08:04:06.795Z`）→ 毫秒。
fn parse_iso_millis(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// 按字符（非字节）截断标题，避免切碎多字节中文。
fn truncate_title(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= TITLE_MAX {
        return t.to_string();
    }
    t.chars().take(TITLE_MAX).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"type":"custom-title","customTitle":"读文件任务"}
{"type":"user","cwd":"/tmp/proj","message":{"role":"user","content":[{"type":"text","text":"帮我读 a.txt"}]},"timestamp":"2026-05-29T08:00:00.000Z"}
{"type":"assistant","message":{"role":"assistant","model":"claude-opus-4-8","content":[{"type":"thinking","thinking":"先 Read"},{"type":"text","text":"好的"},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"path":"a.txt"}}]},"timestamp":"2026-05-29T08:00:01.000Z"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"file body"}]},"timestamp":"2026-05-29T08:00:02.000Z"}
{"type":"summary","summary":"x"}"#;

    #[test]
    fn rebuilds_messages_and_inlines_tool_result() {
        let p = parse_claude_jsonl(SAMPLE).unwrap();
        assert_eq!(p.title, "读文件任务");
        assert_eq!(p.workdir, Some(PathBuf::from("/tmp/proj")));
        assert_eq!(p.model, "claude-opus-4-8");
        // tool_result 不产生独立消息：只有 user + assistant 两条。
        assert_eq!(p.messages.len(), 2);
        assert_eq!(p.messages[0].role, Role::User);

        let a = &p.messages[1];
        assert_eq!(a.role, Role::Assistant);
        assert_eq!(a.content, "好的");
        // 结果已回填到内联工具调用。
        assert_eq!(a.tool_calls.len(), 1);
        assert_eq!(a.tool_calls[0].result.as_deref(), Some("file body"));
        // parts 保住有序：reasoning / text / tool_call。
        assert!(matches!(a.parts[0], MessagePart::Reasoning { .. }));
        assert!(matches!(a.parts[1], MessagePart::Text { .. }));
        match &a.parts[2] {
            MessagePart::ToolCall { result, .. } => {
                assert_eq!(result.as_deref(), Some("file body"))
            }
            _ => panic!("第三块应是 tool_call"),
        }
    }

    #[test]
    fn falls_back_to_first_user_text_for_title() {
        let no_title = SAMPLE.lines().skip(1).collect::<Vec<_>>().join("\n");
        let p = parse_claude_jsonl(&no_title).unwrap();
        assert_eq!(p.title, "帮我读 a.txt");
    }

    #[test]
    fn truncate_title_counts_chars_not_bytes() {
        let long = "中".repeat(50);
        let out = truncate_title(&long);
        assert_eq!(out.chars().count(), TITLE_MAX + 1); // 40 + 省略号
    }
}
