/// Google Gemini API 格式转换
use serde_json::{json, Value};

use crate::types::{
    AssistantEntry, ModelRequest, ModelResponse, ToolCall, ToolDefinition, ToolResult,
    TranscriptEntry, Usage, UserEntry, IMAGE_GENERATION_TOOL_NAME,
};

// ── 请求构建 ──────────────────────────────────────────────────────────────────

pub fn build_body(req: &ModelRequest) -> Value {
    let contents: Vec<Value> = req.entries.iter().filter_map(entry_to_content).collect();

    let mut body = json!({"contents": contents});

    if let Some(sys) = &req.system {
        if !sys.trim().is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": sys}]});
        }
    }

    let tool_defs = tool_defs(&req.tools);
    if !tool_defs.is_empty() {
        body["tools"] = json!([{"functionDeclarations": tool_defs}]);
    }

    body
}

fn entry_to_content(entry: &TranscriptEntry) -> Option<Value> {
    match entry {
        TranscriptEntry::User(user) => Some(json!({
            "role": "user",
            "parts": user_parts(user)
        })),
        TranscriptEntry::Assistant(AssistantEntry { text, tool_calls }) => {
            if tool_calls.is_empty() {
                Some(json!({
                    "role": "model",
                    "parts": [{"text": text}]
                }))
            } else {
                let parts: Vec<Value> = tool_calls
                    .iter()
                    .map(|c| json!({"functionCall": {"name": c.name, "args": c.input}}))
                    .collect();
                Some(json!({"role": "model", "parts": parts}))
            }
        }
        TranscriptEntry::ToolResults(results) => {
            if results.is_empty() {
                return None;
            }
            let parts: Vec<Value> = results
                .iter()
                .map(|ToolResult { name, content, .. }| {
                    json!({
                        "functionResponse": {
                            "name": name,
                            "response": {"result": content}
                        }
                    })
                })
                .collect();
            Some(json!({"role": "user", "parts": parts}))
        }
    }
}

fn user_parts(user: &UserEntry) -> Vec<Value> {
    let mut parts = Vec::new();
    if !user.text.trim().is_empty() {
        parts.push(json!({"text": user.text}));
    }
    for attachment in &user.attachments {
        match attachment {
            platform::attachments::MessageAttachment::TextFile { .. } => {
                if let Some(text) = attachment.as_text_block() {
                    parts.push(json!({"text": text}));
                }
            }
            platform::attachments::MessageAttachment::Image {
                media_type, data, ..
            } => {
                parts.push(json!({
                    "inlineData": {
                        "mimeType": media_type,
                        "data": data
                    }
                }));
            }
        }
    }
    if parts.is_empty() {
        parts.push(json!({"text": ""}));
    }
    parts
}

pub fn tool_defs(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .filter(|t| t.name != IMAGE_GENERATION_TOOL_NAME)
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters
            })
        })
        .collect()
}

// ── 响应解析 ──────────────────────────────────────────────────────────────────

pub fn parse_response(v: &Value) -> ModelResponse {
    let parts = &v["candidates"][0]["content"]["parts"];
    let usage = parse_usage(v);

    if let Some(arr) = parts.as_array() {
        let has_fn_call = arr.iter().any(|p| p.get("functionCall").is_some());
        if has_fn_call {
            let calls: Vec<ToolCall> = arr
                .iter()
                .filter_map(|p| {
                    let fc = p.get("functionCall")?;
                    Some(ToolCall {
                        id: fc["name"].as_str().unwrap_or("").to_string(),
                        name: fc["name"].as_str().unwrap_or("").to_string(),
                        input: fc["args"].clone(),
                    })
                })
                .collect();
            return ModelResponse::ToolCalls {
                text: String::new(),
                calls,
                attachments: Vec::new(),
                usage,
            };
        }
        let text = arr
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        ModelResponse::Done {
            text,
            attachments: Vec::new(),
            usage,
        }
    } else {
        ModelResponse::Done {
            text: String::new(),
            attachments: Vec::new(),
            usage,
        }
    }
}

/// 从 Gemini SSE 数据中提取文本增量
pub fn parse_stream_delta(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    let mut text = String::new();
    if let Some(parts) = v["candidates"][0]["content"]["parts"].as_array() {
        for p in parts {
            text.push_str(p["text"].as_str().unwrap_or(""));
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn parse_usage(v: &Value) -> Usage {
    let meta = &v["usageMetadata"];
    Usage {
        input_tokens: meta["promptTokenCount"].as_u64().unwrap_or(0),
        output_tokens: meta["candidatesTokenCount"].as_u64().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TranscriptEntry, UserEntry};
    use platform::attachments::MessageAttachment;

    #[test]
    fn user_attachments_become_gemini_parts() {
        let req = ModelRequest {
            model: "gemini-3-pro".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry {
                text: "inspect".into(),
                attachments: vec![
                    MessageAttachment::TextFile {
                        name: "app.ts".into(),
                        media_type: "text/typescript".into(),
                        content: "export {}".into(),
                    },
                    MessageAttachment::Image {
                        name: "screen.jpg".into(),
                        media_type: "image/jpeg".into(),
                        data: "jpegbytes".into(),
                    },
                ],
            })],
            tools: vec![],
            max_tokens: 4096,
        };

        let body = build_body(&req);
        let parts = body["contents"][0]["parts"].as_array().unwrap();

        assert_eq!(parts[0], json!({"text": "inspect"}));
        assert_eq!(
            parts[1],
            json!({"text": "<file name=\"app.ts\" media_type=\"text/typescript\">\nexport {}\n</file>"})
        );
        assert_eq!(
            parts[2],
            json!({"inlineData": {"mimeType": "image/jpeg", "data": "jpegbytes"}})
        );
    }
}
