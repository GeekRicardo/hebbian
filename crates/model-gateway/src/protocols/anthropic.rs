/// Anthropic Claude API 格式转换
use serde_json::{json, Value};

use crate::types::{
    AssistantEntry, ModelRequest, ModelResponse, ToolCall, ToolDefinition, ToolResult,
    TranscriptEntry, Usage, UserEntry, IMAGE_GENERATION_TOOL_NAME,
};

// ── 请求构建 ──────────────────────────────────────────────────────────────────

/// Claude Code OAuth token 必须看到这一行 system 才会被服务端识别为合法 CLI 流量。
const CLAUDE_CODE_BANNER: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

pub fn build_body(req: &ModelRequest, stream: bool, claude_code_oauth: bool) -> Value {
    let messages: Vec<Value> = req.entries.iter().filter_map(entry_to_message).collect();

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": messages,
        "stream": stream,
    });

    body["system"] = build_system(req.system.as_deref(), claude_code_oauth);
    if body["system"].is_null() {
        body.as_object_mut().unwrap().remove("system");
    }

    if !req.tools.is_empty() {
        body["tools"] = json!(tool_defs(&req.tools));
    }

    body
}

fn build_system(user_system: Option<&str>, claude_code_oauth: bool) -> Value {
    let user_system = user_system.map(str::trim).filter(|s| !s.is_empty());

    if !claude_code_oauth {
        return user_system.map(|s| json!(s)).unwrap_or(Value::Null);
    }

    let banner = json!({ "type": "text", "text": CLAUDE_CODE_BANNER });
    match user_system {
        None => json!([banner]),
        Some(s) if s == CLAUDE_CODE_BANNER => json!([banner]),
        Some(s) => json!([
            banner,
            { "type": "text", "text": format!("{CLAUDE_CODE_BANNER}\n\n{s}") }
        ]),
    }
}

fn entry_to_message(entry: &TranscriptEntry) -> Option<Value> {
    match entry {
        TranscriptEntry::User(user) => Some(json!({"role": "user", "content": user_content(user)})),
        TranscriptEntry::Assistant(AssistantEntry { text, tool_calls }) => {
            if tool_calls.is_empty() {
                Some(json!({"role": "assistant", "content": text}))
            } else {
                let mut content: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                for c in tool_calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": c.id,
                        "name": c.name,
                        "input": c.input
                    }));
                }
                Some(json!({"role": "assistant", "content": content}))
            }
        }
        TranscriptEntry::ToolResults(results) => {
            let content: Vec<Value> = results
                .iter()
                .map(
                    |ToolResult {
                         call_id, content, ..
                     }| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": call_id,
                            "content": content
                        })
                    },
                )
                .collect();
            if content.is_empty() {
                None
            } else {
                Some(json!({"role": "user", "content": content}))
            }
        }
    }
}

fn user_content(user: &UserEntry) -> Value {
    if user.attachments.is_empty() {
        return json!(user.text);
    }

    let mut content = Vec::new();
    if !user.text.trim().is_empty() {
        content.push(json!({"type": "text", "text": user.text}));
    }
    for attachment in &user.attachments {
        match attachment {
            platform::attachments::MessageAttachment::TextFile { .. } => {
                if let Some(text) = attachment.as_text_block() {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            platform::attachments::MessageAttachment::Image {
                media_type, data, ..
            } => {
                content.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": data
                    }
                }));
            }
        }
    }
    Value::Array(content)
}

pub fn tool_defs(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .filter(|t| t.name != IMAGE_GENERATION_TOOL_NAME)
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters
            })
        })
        .collect()
}

// ── 响应解析 ──────────────────────────────────────────────────────────────────

pub fn parse_response(v: &Value) -> ModelResponse {
    let stop_reason = v["stop_reason"].as_str().unwrap_or("");
    let usage = parse_usage(v);

    if stop_reason == "tool_use" {
        let mut text = String::new();
        let mut calls = Vec::new();
        if let Some(content) = v["content"].as_array() {
            for block in content {
                match block["type"].as_str() {
                    Some("text") => text.push_str(block["text"].as_str().unwrap_or("")),
                    Some("tool_use") => calls.push(ToolCall {
                        id: block["id"].as_str().unwrap_or("").to_string(),
                        name: block["name"].as_str().unwrap_or("").to_string(),
                        input: block["input"].clone(),
                    }),
                    _ => {}
                }
            }
        }
        ModelResponse::ToolCalls {
            text,
            calls,
            attachments: Vec::new(),
            usage,
        }
    } else {
        let mut text = String::new();
        if let Some(arr) = v["content"].as_array() {
            for block in arr {
                if block["type"] == "text" {
                    text.push_str(block["text"].as_str().unwrap_or(""));
                }
            }
        }
        ModelResponse::Done {
            text,
            attachments: Vec::new(),
            usage,
        }
    }
}

/// 从 Anthropic SSE 事件中提取文本增量
pub fn parse_stream_delta(event_type: &str, data: &str) -> Option<String> {
    if event_type != "content_block_delta" {
        return None;
    }
    let v: Value = serde_json::from_str(data).ok()?;
    if v["type"] == "content_block_delta" {
        let delta = v["delta"]["text"].as_str()?;
        if delta.is_empty() {
            None
        } else {
            Some(delta.to_string())
        }
    } else {
        None
    }
}

fn parse_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TranscriptEntry, UserEntry};
    use platform::attachments::MessageAttachment;

    #[test]
    fn user_attachments_become_claude_content_blocks() {
        let req = ModelRequest {
            model: "claude-sonnet-4-5".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry {
                text: "what changed?".into(),
                attachments: vec![
                    MessageAttachment::TextFile {
                        name: "diff.txt".into(),
                        media_type: "text/plain".into(),
                        content: "+hello".into(),
                    },
                    MessageAttachment::Image {
                        name: "shot.webp".into(),
                        media_type: "image/webp".into(),
                        data: "webpbytes".into(),
                    },
                ],
            })],
            tools: vec![],
            max_tokens: 4096,
        };

        let body = build_body(&req, false, false);
        let content = body["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0], json!({"type": "text", "text": "what changed?"}));
        assert_eq!(
            content[1],
            json!({"type": "text", "text": "<file name=\"diff.txt\" media_type=\"text/plain\">\n+hello\n</file>"})
        );
        assert_eq!(
            content[2],
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/webp",
                    "data": "webpbytes"
                }
            })
        );
    }

    #[test]
    fn claude_code_oauth_prepends_banner_to_system() {
        let req = ModelRequest {
            model: "claude-sonnet-4-5".into(),
            system: Some("Be terse.".into()),
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 1024,
        };

        let body = build_body(&req, false, true);
        let system = body["system"].as_array().expect("system must be an array");

        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["text"], CLAUDE_CODE_BANNER);
        assert_eq!(
            system[1]["text"],
            format!("{CLAUDE_CODE_BANNER}\n\nBe terse.")
        );
    }

    #[test]
    fn claude_code_oauth_without_user_system_emits_banner_only() {
        let req = ModelRequest {
            model: "claude-sonnet-4-5".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 1024,
        };

        let body = build_body(&req, false, true);
        let system = body["system"].as_array().expect("system must be an array");

        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["text"], CLAUDE_CODE_BANNER);
    }

    #[test]
    fn non_oauth_keeps_plain_string_system() {
        let req = ModelRequest {
            model: "claude-sonnet-4-5".into(),
            system: Some("Be terse.".into()),
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 1024,
        };

        let body = build_body(&req, false, false);
        assert_eq!(body["system"], json!("Be terse."));
    }
}
