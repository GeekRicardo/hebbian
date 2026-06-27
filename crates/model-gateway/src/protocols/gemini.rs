/// Google Gemini API 格式转换
use serde_json::{json, Value};

use crate::types::{
    AssistantEntry, FinishReason, ModelRequest, ModelResponse, ToolCall, ToolDefinition,
    ToolResult, TranscriptEntry, Usage, UserEntry, IMAGE_GENERATION_TOOL_NAME,
};
use common::attachments::MessageAttachment;

/// 把 Gemini 的 `candidates[0].finishReason` 归一成 [`FinishReason`]（架构 §4.11.4）。
pub fn map_gemini_finish(finish: &str) -> FinishReason {
    match finish {
        "STOP" | "" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_string()),
    }
}

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
        TranscriptEntry::Assistant(AssistantEntry {
            text, tool_calls, ..
        }) => {
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
            let mut parts: Vec<Value> = Vec::new();
            for ToolResult {
                name,
                content,
                attachments,
                ..
            } in results
            {
                parts.push(json!({
                    "functionResponse": {
                        "name": name,
                        "response": {"result": content}
                    }
                }));
                // functionResponse 不带图片——图片附件在同一 user role 里追加
                // inlineData part（架构 §4.4.1）。
                for attachment in attachments {
                    if let MessageAttachment::Image {
                        media_type, data, ..
                    } = attachment
                    {
                        parts.push(json!({
                            "inlineData": {"mimeType": media_type, "data": data}
                        }));
                    }
                }
            }
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
            MessageAttachment::TextFile { .. } => {
                if let Some(text) = attachment.as_text_block() {
                    parts.push(json!({"text": text}));
                }
            }
            MessageAttachment::Image {
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
    let finish = map_gemini_finish(v["candidates"][0]["finishReason"].as_str().unwrap_or(""));

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
                reasoning: String::new(),
                reasoning_signature: String::new(),
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
            reasoning: String::new(),
            reasoning_signature: String::new(),
            attachments: Vec::new(),
            usage,
            finish,
        }
    } else {
        ModelResponse::Done {
            text: String::new(),
            reasoning: String::new(),
            reasoning_signature: String::new(),
            attachments: Vec::new(),
            usage,
            finish,
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

/// Gemini 流式响应中 `usageMetadata` 会随多帧增量更新，最后一帧给完整终态；
/// 调用侧每帧都跑一次、保留最新的非空结果即可。
pub fn parse_stream_usage(data: &str) -> Option<Usage> {
    let v: Value = serde_json::from_str(data).ok()?;
    if v.get("usageMetadata").is_none() {
        return None;
    }
    Some(parse_usage(&v))
}

fn parse_usage(v: &Value) -> Usage {
    let meta = &v["usageMetadata"];
    // Gemini implicit caching：`cachedContentTokenCount` 是命中显式缓存的部分；
    // 已计入 `promptTokenCount`。implicit cache 命中也会通过这个字段返回。
    Usage {
        input_tokens: meta["promptTokenCount"].as_u64().unwrap_or(0),
        output_tokens: meta["candidatesTokenCount"].as_u64().unwrap_or(0),
        cache_read_tokens: meta["cachedContentTokenCount"].as_u64().unwrap_or(0),
        cache_creation_tokens: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TranscriptEntry, UserEntry};
    use common::attachments::MessageAttachment;

    #[test]
    fn gemini_finish_maps_all_variants() {
        assert_eq!(map_gemini_finish("STOP"), FinishReason::Stop);
        assert_eq!(map_gemini_finish("MAX_TOKENS"), FinishReason::Length);
        assert_eq!(map_gemini_finish("SAFETY"), FinishReason::ContentFilter);
        assert_eq!(map_gemini_finish("RECITATION"), FinishReason::ContentFilter);
        assert_eq!(
            map_gemini_finish("MALFORMED_FUNCTION_CALL"),
            FinishReason::Other("MALFORMED_FUNCTION_CALL".to_string())
        );
    }

    #[test]
    fn tool_result_image_becomes_inline_data_part() {
        let req = ModelRequest {
            model: "gemini-3-pro".into(),
            system: None,
            entries: vec![TranscriptEntry::ToolResults(vec![ToolResult {
                call_id: "call_1".into(),
                name: "Read".into(),
                content: "已读取图片 a.png".into(),
                artifact: None,
                attachments: vec![MessageAttachment::Image {
                    name: "a.png".into(),
                    media_type: "image/png".into(),
                    data: "BASE64DATA".into(),
                }],
            }])],
            tools: vec![],
            max_tokens: 4096,
            reasoning: None,
                    meta: Default::default(),
        };
        let body = build_body(&req);
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        // functionResponse part + inlineData part 在同一 user role
        assert!(parts[0].get("functionResponse").is_some());
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], "BASE64DATA");
    }

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
            reasoning: None,
                    meta: Default::default(),
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
