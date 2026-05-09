/// OpenAI / OpenAI-compatible API 格式转换
use std::collections::HashMap;

use serde_json::{json, Value};

use crate::types::{
    AssistantEntry, ModelRequest, ModelResponse, ToolCall, ToolDefinition, ToolResult,
    TranscriptEntry, Usage, UserEntry, IMAGE_GENERATION_TOOL_NAME,
};
use platform::attachments::MessageAttachment;
use platform::reasoning::openai_supports_reasoning;

// ── 请求构建 ──────────────────────────────────────────────────────────────────

pub fn build_body(req: &ModelRequest, stream: bool) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(sys) = &req.system {
        if !sys.trim().is_empty() {
            messages.push(json!({"role": "system", "content": sys}));
        }
    }

    for entry in &req.entries {
        match entry {
            TranscriptEntry::User(user) => {
                messages.push(json!({"role": "user", "content": chat_user_content(user)}));
            }
            TranscriptEntry::Assistant(AssistantEntry {
                text,
                reasoning,
                tool_calls,
            }) => {
                let mut msg = if tool_calls.is_empty() {
                    json!({"role": "assistant", "content": text})
                } else {
                    let calls: Vec<Value> = tool_calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": {
                                    "name": c.name,
                                    "arguments": c.input.to_string()
                                }
                            })
                        })
                        .collect();
                    json!({
                        "role": "assistant",
                        "content": if text.is_empty() { Value::Null } else { json!(text) },
                        "tool_calls": calls
                    })
                };
                // DeepSeek (api.deepseek.com/beta) 等 thinking-aware 后端要求把
                // 上一轮的 reasoning_content 回填，否则带 tool_calls 的多轮场景会
                // 报错 / 丢推理链。其它 provider 见到这个字段会无视。
                if !reasoning.is_empty() {
                    msg["reasoning_content"] = Value::String(reasoning.clone());
                }
                messages.push(msg);
            }
            TranscriptEntry::ToolResults(results) => {
                for ToolResult {
                    call_id, content, ..
                } in results
                {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": content
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": stream,
    });

    if !req.tools.is_empty() {
        body["tools"] = json!(tool_defs(&req.tools));
    }

    // gpt-5 / o-series 支持 reasoning_effort（顶层字段，chat completions 形态）。
    // 5.4 / 5.5 / codex-max 走 xhigh，其它（含 o-series）钳到 high。o1-mini 直接跳过整个字段。
    if let Some(cfg) = req.reasoning.as_ref() {
        if cfg.is_enabled() && openai_supports_reasoning(&req.model) {
            body["reasoning_effort"] = json!(cfg
                .effective_effort()
                .openai_effort_for_model(&req.model));
        }
    }

    body
}

pub fn build_responses_body(req: &ModelRequest, stream: bool, codex_oauth: bool) -> Value {
    let mut input: Vec<Value> = Vec::new();
    let mut tool_call_names: HashMap<String, String> = HashMap::new();

    for entry in &req.entries {
        match entry {
            TranscriptEntry::User(user) => {
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": responses_user_content(user)
                }));
            }
            TranscriptEntry::Assistant(AssistantEntry {
                text, tool_calls, ..
            }) => {
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }]
                    }));
                }
                for call in tool_calls {
                    if !call.name.trim().is_empty() {
                        tool_call_names.insert(call.id.clone(), call.name.clone());
                    }
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.input.to_string()
                    }));
                }
            }
            TranscriptEntry::ToolResults(results) => {
                for ToolResult {
                    call_id,
                    name,
                    content,
                } in results
                {
                    let mut item = json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": content
                    });
                    if codex_oauth {
                        item["name"] = Value::String(resolve_tool_result_name(
                            call_id,
                            name,
                            &tool_call_names,
                        ));
                    }
                    input.push(item);
                }
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "input": input,
        "instructions": req.system.as_deref().unwrap_or(""),
        "stream": stream,
    });

    if req.max_tokens > 0 {
        body["max_output_tokens"] = json!(req.max_tokens);
    }

    let response_tools = responses_tool_defs(&req.tools);
    if !response_tools.is_empty() {
        body["tools"] = Value::Array(response_tools);
    }
    if should_force_image_generation(req) {
        body["tool_choice"] = json!({"type": "image_generation"});
    }

    if codex_oauth {
        body["store"] = json!(false);
        body["include"] = json!(["reasoning.encrypted_content"]);
        body["stream"] = json!(true);

        if let Some(obj) = body.as_object_mut() {
            obj.remove("max_output_tokens");
            obj.entry("tools".to_string()).or_insert(json!([]));
            obj.entry("parallel_tool_calls".to_string())
                .or_insert(json!(false));
        }
    }

    // Responses API 用嵌套对象 reasoning.effort（gpt-5 / o-series）。
    if let Some(cfg) = req.reasoning.as_ref() {
        if cfg.is_enabled() && openai_supports_reasoning(&req.model) {
            body["reasoning"] = json!({
                "effort": cfg.effective_effort().openai_effort_for_model(&req.model),
            });
        }
    }

    body
}

fn should_force_image_generation(req: &ModelRequest) -> bool {
    if !req
        .tools
        .iter()
        .any(|t| t.name == IMAGE_GENERATION_TOOL_NAME)
    {
        return false;
    }
    let Some(text) = req.entries.iter().rev().find_map(|entry| match entry {
        TranscriptEntry::User(user) => Some(user.text.as_str()),
        _ => None,
    }) else {
        return false;
    };

    image_generation_intent(text)
}

fn image_generation_intent(text: &str) -> bool {
    let lower = text.to_lowercase();
    if [
        "不要生成",
        "不生成",
        "别生成",
        "不要画",
        "别画",
        "为什么",
        "为何",
        "如何",
        "怎么",
        "提示词",
        "do not generate",
        "don't generate",
        "how to",
        "why",
        "prompt",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return false;
    }

    let has_image_noun = [
        "图",
        "图片",
        "图像",
        "照片",
        "海报",
        "插画",
        "image",
        "picture",
        "photo",
        "poster",
        "illustration",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let has_generation_verb = [
        "生成", "生图", "出图", "画", "绘制", "创建", "制作", "generate", "create", "draw", "make",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    has_image_noun && has_generation_verb
}

fn resolve_tool_result_name(
    call_id: &str,
    name: &str,
    tool_call_names: &HashMap<String, String>,
) -> String {
    if !name.trim().is_empty() {
        return name.to_string();
    }
    tool_call_names
        .get(call_id)
        .cloned()
        .unwrap_or_else(|| "unknown_tool".to_string())
}

pub fn tool_defs(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .filter(|t| t.name != IMAGE_GENERATION_TOOL_NAME)
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                }
            })
        })
        .collect()
}

pub fn responses_tool_defs(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            if t.name == IMAGE_GENERATION_TOOL_NAME {
                return json!({"type": "image_generation"});
            }
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters
            })
        })
        .collect()
}

fn chat_user_content(user: &UserEntry) -> Value {
    if user.attachments.is_empty() {
        return json!(user.text);
    }

    let mut content = Vec::new();
    if !user.text.trim().is_empty() {
        content.push(json!({"type": "text", "text": user.text}));
    }
    for attachment in &user.attachments {
        if let Some(text) = attachment.as_text_block() {
            content.push(json!({"type": "text", "text": text}));
        } else if let Some(url) = attachment.image_data_url() {
            content.push(json!({"type": "image_url", "image_url": {"url": url}}));
        }
    }
    Value::Array(content)
}

fn responses_user_content(user: &UserEntry) -> Vec<Value> {
    let mut content = Vec::new();
    if !user.text.trim().is_empty() {
        content.push(json!({"type": "input_text", "text": user.text}));
    }
    for attachment in &user.attachments {
        if let Some(text) = attachment.as_text_block() {
            content.push(json!({"type": "input_text", "text": text}));
        } else if let Some(url) = attachment.image_data_url() {
            content.push(json!({"type": "input_image", "image_url": url}));
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "input_text", "text": ""}));
    }
    content
}

// ── 响应解析 ──────────────────────────────────────────────────────────────────

pub fn parse_response(v: &Value) -> ModelResponse {
    let msg = &v["choices"][0]["message"];
    let finish = v["choices"][0]["finish_reason"].as_str().unwrap_or("");
    let usage = parse_usage(v);

    // 非流式响应里 DeepSeek 把推理放在 message.reasoning_content
    let reasoning = msg
        .get("reasoning_content")
        .or_else(|| msg.get("reasoning"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if finish == "tool_calls" {
        let calls = parse_tool_calls(&msg["tool_calls"]);
        let text = msg["content"].as_str().unwrap_or("").to_string();
        ModelResponse::ToolCalls {
            text,
            reasoning,
            calls,
            attachments: Vec::new(),
            usage,
        }
    } else {
        let text = msg["content"].as_str().unwrap_or("").to_string();
        ModelResponse::Done {
            text,
            reasoning,
            attachments: Vec::new(),
            usage,
        }
    }
}

pub fn parse_responses_response(v: &Value) -> ModelResponse {
    let usage = parse_responses_usage(v);
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut attachments = Vec::new();

    if let Some(output) = v["output"].as_array() {
        for item in output {
            match item["type"].as_str().unwrap_or("") {
                "message" => {
                    if let Some(content) = item["content"].as_array() {
                        for block in content {
                            match block["type"].as_str().unwrap_or("") {
                                "output_text" => {
                                    text.push_str(block["text"].as_str().unwrap_or(""));
                                }
                                "refusal" => {
                                    text.push_str(block["refusal"].as_str().unwrap_or(""));
                                }
                                "output_image" => {
                                    push_output_image_attachment(&mut attachments, block);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "image_generation_call" => {
                    push_generated_image_attachment(&mut attachments, item);
                }
                "function_call" => {
                    let id = item["call_id"]
                        .as_str()
                        .or_else(|| item["id"].as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item["name"].as_str().unwrap_or("").to_string();
                    if id.is_empty() || name.is_empty() {
                        continue;
                    }
                    let args = item["arguments"].as_str().unwrap_or("{}");
                    let input = serde_json::from_str(args).unwrap_or(Value::Null);
                    calls.push(ToolCall { id, name, input });
                }
                _ => {}
            }
        }
    } else if let Some(s) = v["output_text"].as_str() {
        text.push_str(s);
    }

    if calls.is_empty() {
        ModelResponse::Done {
            text,
            reasoning: String::new(),
            attachments,
            usage,
        }
    } else {
        ModelResponse::ToolCalls {
            text,
            reasoning: String::new(),
            calls,
            attachments,
            usage,
        }
    }
}

fn push_generated_image_attachment(attachments: &mut Vec<MessageAttachment>, item: &Value) {
    let Some(result) = item["result"].as_str().filter(|s| !s.trim().is_empty()) else {
        return;
    };
    push_image_attachment(attachments, item, result);
}

fn push_output_image_attachment(attachments: &mut Vec<MessageAttachment>, item: &Value) {
    let data = item["image_url"]
        .as_str()
        .or_else(|| item["b64_json"].as_str())
        .or_else(|| item["data"].as_str());
    let Some(data) = data.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    push_image_attachment(attachments, item, data);
}

fn push_image_attachment(attachments: &mut Vec<MessageAttachment>, item: &Value, data: &str) {
    let (media_type, data) =
        split_data_url(data).unwrap_or_else(|| (image_media_type(item), data.to_string()));
    let name = item["filename"]
        .as_str()
        .or_else(|| item["name"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| generated_image_name(attachments.len() + 1, &media_type));

    attachments.push(MessageAttachment::Image {
        name,
        media_type,
        data,
    });
}

fn split_data_url(value: &str) -> Option<(String, String)> {
    let rest = value.strip_prefix("data:")?;
    let (media_type, data) = rest.split_once(";base64,")?;
    Some((media_type.to_string(), data.to_string()))
}

fn image_media_type(item: &Value) -> String {
    let format = item["output_format"]
        .as_str()
        .or_else(|| item["format"].as_str())
        .unwrap_or("png")
        .trim_start_matches('.');
    match format {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "png" => "image/png",
        other if other.starts_with("image/") => other,
        _ => "image/png",
    }
    .to_string()
}

fn generated_image_name(index: usize, media_type: &str) -> String {
    let ext = match media_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    };
    format!("generated-image-{index}.{ext}")
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ChatStreamFrame {
    pub text_delta: Option<String>,
    /// DeepSeek / Qwen / GLM-thinking 等：`delta.reasoning_content`（部分实现叫 `reasoning`）。
    pub reasoning_delta: Option<String>,
    pub tool_calls: Vec<ChatStreamToolCallDelta>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ChatStreamToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

pub fn parse_chat_stream_frame(data: &str) -> Option<ChatStreamFrame> {
    let v: Value = serde_json::from_str(data).ok()?;
    let delta = v["choices"].as_array()?.first()?.get("delta")?;
    let text_delta = delta["content"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let reasoning_delta = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let tool_calls: Vec<ChatStreamToolCallDelta> = delta["tool_calls"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .enumerate()
                .map(|(fallback_index, item)| ChatStreamToolCallDelta {
                    index: item["index"]
                        .as_u64()
                        .map(|index| index as usize)
                        .unwrap_or(fallback_index),
                    id: item["id"].as_str().map(|s| s.to_string()),
                    name: item["function"]["name"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.to_string()),
                    arguments: item["function"]["arguments"]
                        .as_str()
                        .map(|s| s.to_string()),
                })
                .collect()
        })
        .unwrap_or_default();

    if text_delta.is_none() && reasoning_delta.is_none() && tool_calls.is_empty() {
        None
    } else {
        Some(ChatStreamFrame {
            text_delta,
            reasoning_delta,
            tool_calls,
        })
    }
}

pub enum ResponsesSseEvent {
    TextDelta(String),
    ReasoningDelta(String),
    OutputItemAdded {
        output_index: Option<usize>,
        item: Value,
    },
    OutputItemDone {
        output_index: Option<usize>,
        item: Value,
    },
    FunctionCallArgumentsDelta {
        output_index: Option<usize>,
        item_id: Option<String>,
        delta: String,
    },
    FunctionCallArgumentsDone {
        output_index: Option<usize>,
        item_id: Option<String>,
        call_id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Completed(Value),
    Failed(String),
    Ignore,
}

pub fn parse_responses_sse_event(event_name: &str, data: &str) -> ResponsesSseEvent {
    if data.trim().is_empty() || data.trim() == "[DONE]" {
        return ResponsesSseEvent::Ignore;
    }

    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return ResponsesSseEvent::Ignore;
    };

    let event_name = if event_name.is_empty() {
        v["type"].as_str().unwrap_or("")
    } else {
        event_name
    };

    match event_name {
        "response.output_text.delta" => v["delta"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| ResponsesSseEvent::TextDelta(s.to_string()))
            .unwrap_or(ResponsesSseEvent::Ignore),
        "response.reasoning_text.delta"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning.delta" => v["delta"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| ResponsesSseEvent::ReasoningDelta(s.to_string()))
            .unwrap_or(ResponsesSseEvent::Ignore),
        "response.output_item.added" => match v.get("item").cloned() {
            Some(mut item) => {
                merge_responses_event_call_fields(&mut item, &v);
                ResponsesSseEvent::OutputItemAdded {
                    output_index: responses_output_index(&v),
                    item,
                }
            }
            None => ResponsesSseEvent::Ignore,
        },
        "response.output_item.done" => match v.get("item").cloned() {
            Some(mut item) => {
                merge_responses_event_call_fields(&mut item, &v);
                ResponsesSseEvent::OutputItemDone {
                    output_index: responses_output_index(&v),
                    item,
                }
            }
            None => ResponsesSseEvent::Ignore,
        },
        "response.function_call_arguments.delta" => v["delta"]
            .as_str()
            .map(|delta| ResponsesSseEvent::FunctionCallArgumentsDelta {
                output_index: responses_output_index(&v),
                item_id: responses_string(&v, "item_id"),
                delta: delta.to_string(),
            })
            .unwrap_or(ResponsesSseEvent::Ignore),
        "response.function_call_arguments.done" => ResponsesSseEvent::FunctionCallArgumentsDone {
            output_index: responses_output_index(&v),
            item_id: responses_string(&v, "item_id"),
            call_id: responses_string(&v, "call_id"),
            name: responses_string(&v, "name"),
            arguments: v["arguments"].as_str().unwrap_or("").to_string(),
        },
        "response.completed" => {
            ResponsesSseEvent::Completed(v.get("response").cloned().unwrap_or(v))
        }
        "response.failed" => {
            let msg = v
                .pointer("/response/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or("response.failed event received")
                .to_string();
            ResponsesSseEvent::Failed(msg)
        }
        _ => ResponsesSseEvent::Ignore,
    }
}

fn responses_output_index(v: &Value) -> Option<usize> {
    v["output_index"].as_u64().map(|index| index as usize)
}

fn responses_string(v: &Value, key: &str) -> Option<String> {
    v[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn merge_responses_event_call_fields(item: &mut Value, event: &Value) {
    for key in ["call_id", "name", "arguments"] {
        if item[key]
            .as_str()
            .filter(|value| !value.is_empty())
            .is_some()
        {
            continue;
        }
        if let Some(value) = event[key].as_str().filter(|value| !value.is_empty()) {
            item[key] = Value::String(value.to_string());
        }
    }
}

fn parse_tool_calls(tool_calls: &Value) -> Vec<ToolCall> {
    tool_calls
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|tc| {
            let id = tc["id"].as_str()?.to_string();
            let name = tc["function"]["name"]
                .as_str()
                .filter(|s| !s.trim().is_empty())?
                .to_string();
            let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
            let input = serde_json::from_str(args_str).unwrap_or(Value::Null);
            Some(ToolCall { id, name, input })
        })
        .collect()
}

fn parse_usage(v: &Value) -> Usage {
    let usage = &v["usage"];
    // OpenAI Chat Completions：`prompt_tokens_details.cached_tokens` 是命中缓存读出的部分；
    // 已经计入 `prompt_tokens`，所以不用再加。
    // DeepSeek（OpenAI 兼容路径）：用 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`，
    // 这两个字段加起来 = `prompt_tokens`，hit 部分对应缓存命中。
    let cached = usage["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_cache_hit_tokens"].as_u64())
        .unwrap_or(0);
    Usage {
        input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: cached,
        cache_creation_tokens: 0,
    }
}

fn parse_responses_usage(v: &Value) -> Usage {
    let usage = &v["usage"];
    let cached = usage["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    Usage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: cached,
        cache_creation_tokens: 0,
    }
}

#[cfg(test)]
mod responses_tests {
    use super::*;
    use crate::types::{TranscriptEntry, UserEntry};
    use platform::attachments::MessageAttachment;

    #[test]
    fn codex_oauth_responses_body_matches_chatgpt_backend_contract() {
        let req = ModelRequest {
            model: "gpt-5.4".into(),
            system: Some("You are concise.".into()),
            entries: vec![TranscriptEntry::User(UserEntry::text("hello"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, false, true);

        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["instructions"], "You are concise.");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert!(body.get("max_output_tokens").is_none());
        assert!(body["tools"].as_array().unwrap().is_empty());
        assert_eq!(body["parallel_tool_calls"], false);
    }

    #[test]
    fn codex_oauth_responses_body_advertises_image_generation_tool() {
        let req = ModelRequest {
            model: "gpt-5.4".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("生成图片"))],
            tools: vec![ToolDefinition {
                name: "image_generation".into(),
                description: "生成图片".into(),
                parameters: json!({"type": "object"}),
            }],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, true, true);

        assert_eq!(body["tools"], json!([{"type": "image_generation"}]));
    }

    #[test]
    fn responses_body_forces_image_generation_for_image_generation_intent() {
        let req = ModelRequest {
            model: "gpt-5.5".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text(
                "一只可爱的猴子坐在热带雨林的树枝上，生成图片",
            ))],
            tools: vec![ToolDefinition {
                name: IMAGE_GENERATION_TOOL_NAME.into(),
                description: "生成图片".into(),
                parameters: json!({"type": "object"}),
            }],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, true, true);

        assert_eq!(body["tool_choice"], json!({"type": "image_generation"}));
    }

    #[test]
    fn responses_body_does_not_force_image_generation_without_image_intent() {
        let req = ModelRequest {
            model: "gpt-5.5".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![ToolDefinition {
                name: IMAGE_GENERATION_TOOL_NAME.into(),
                description: "生成图片".into(),
                parameters: json!({"type": "object"}),
            }],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, true, true);

        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn responses_body_sends_user_text_files_and_images_as_content_parts() {
        let req = ModelRequest {
            model: "gpt-5.4".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry {
                text: "describe this".into(),
                attachments: vec![
                    MessageAttachment::TextFile {
                        name: "notes.md".into(),
                        media_type: "text/markdown".into(),
                        content: "# Notes".into(),
                    },
                    MessageAttachment::Image {
                        name: "screen.png".into(),
                        media_type: "image/png".into(),
                        data: "abc123".into(),
                    },
                ],
            })],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, false, false);
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][0]["role"], "user");
        let content = body["input"][0]["content"].as_array().unwrap();

        assert_eq!(
            content[0],
            json!({"type": "input_text", "text": "describe this"})
        );
        assert_eq!(
            content[1],
            json!({"type": "input_text", "text": "<file name=\"notes.md\" media_type=\"text/markdown\">\n# Notes\n</file>"})
        );
        assert_eq!(
            content[2],
            json!({"type": "input_image", "image_url": "data:image/png;base64,abc123"})
        );
    }

    #[test]
    fn responses_body_marks_replayed_plain_messages_as_message_items() {
        let entries = (0..11)
            .map(|i| {
                if i % 2 == 0 {
                    TranscriptEntry::User(UserEntry::text(format!("user {i}")))
                } else {
                    TranscriptEntry::Assistant(AssistantEntry {
                        text: format!("assistant {i}"),
                        ..Default::default()
                    })
                }
            })
            .collect();
        let req = ModelRequest {
            model: "gpt-5.4".into(),
            system: None,
            entries,
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, true, true);
        let input = body["input"].as_array().unwrap();

        assert_eq!(input.len(), 11);
        assert_eq!(input[10]["type"], "message");
        for item in input {
            assert_eq!(item["type"], "message");
            assert!(item.get("name").is_none());
        }
    }

    #[test]
    fn responses_function_calls_keep_name_on_call_item() {
        let req = ModelRequest {
            model: "gpt-5.4".into(),
            system: None,
            entries: vec![
                TranscriptEntry::User(UserEntry::text("search rust docs")),
                TranscriptEntry::Assistant(AssistantEntry {
                    text: String::new(),
                    reasoning: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "web_search".into(),
                        input: json!({"query": "rust docs"}),
                    }],
                }),
                TranscriptEntry::ToolResults(vec![ToolResult {
                    call_id: "call_1".into(),
                    name: "web_search".into(),
                    content: "Sources:\n- [Rust](https://www.rust-lang.org/)".into(),
                }]),
            ],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, true, true);

        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][1]["call_id"], "call_1");
        assert_eq!(body["input"][1]["name"], "web_search");
    }

    #[test]
    fn responses_function_call_outputs_match_codex_shape_without_name() {
        let req = ModelRequest {
            model: "gpt-5.4".into(),
            system: None,
            entries: vec![
                TranscriptEntry::User(UserEntry::text("first")),
                TranscriptEntry::Assistant(AssistantEntry {
                    text: String::new(),
                    reasoning: String::new(),
                    tool_calls: vec![
                        ToolCall {
                            id: "call_1".into(),
                            name: "web_search".into(),
                            input: json!({"query": "one"}),
                        },
                        ToolCall {
                            id: "call_2".into(),
                            name: "web_fetch".into(),
                            input: json!({"url": "https://example.com", "prompt": "read"}),
                        },
                    ],
                }),
                TranscriptEntry::ToolResults(vec![
                    ToolResult {
                        call_id: "call_1".into(),
                        name: "web_search".into(),
                        content: "one".into(),
                    },
                    ToolResult {
                        call_id: "call_2".into(),
                        name: "web_fetch".into(),
                        content: "two".into(),
                    },
                ]),
                TranscriptEntry::Assistant(AssistantEntry {
                    text: String::new(),
                    reasoning: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_3".into(),
                        name: "web_search".into(),
                        input: json!({"query": "three"}),
                    }],
                }),
                TranscriptEntry::ToolResults(vec![ToolResult {
                    call_id: "call_3".into(),
                    name: "web_search".into(),
                    content: "three".into(),
                }]),
            ],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, true, false);

        assert_eq!(body["input"][3]["type"], "function_call_output");
        assert_eq!(body["input"][3]["call_id"], "call_1");
        assert_eq!(body["input"][3]["output"], "one");
        assert!(body["input"][3].get("name").is_none());
        assert_eq!(body["input"][4]["type"], "function_call_output");
        assert_eq!(body["input"][4]["call_id"], "call_2");
        assert_eq!(body["input"][4]["output"], "two");
        assert!(body["input"][4].get("name").is_none());
        assert_eq!(body["input"][6]["type"], "function_call_output");
        assert_eq!(body["input"][6]["call_id"], "call_3");
        assert_eq!(body["input"][6]["output"], "three");
        assert!(body["input"][6].get("name").is_none());
    }

    #[test]
    fn codex_oauth_responses_function_call_outputs_include_name() {
        let req = ModelRequest {
            model: "gpt-5.4".into(),
            system: None,
            entries: vec![
                TranscriptEntry::User(UserEntry::text("first")),
                TranscriptEntry::Assistant(AssistantEntry {
                    text: String::new(),
                    reasoning: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "web_search".into(),
                        input: json!({"query": "one"}),
                    }],
                }),
                TranscriptEntry::ToolResults(vec![ToolResult {
                    call_id: "call_1".into(),
                    name: String::new(),
                    content: "one".into(),
                }]),
            ],
            tools: vec![],
            max_tokens: 8192,
            reasoning: None,
        };

        let body = build_responses_body(&req, true, true);

        assert_eq!(body["input"][2]["type"], "function_call_output");
        assert_eq!(body["input"][2]["call_id"], "call_1");
        assert_eq!(body["input"][2]["name"], "web_search");
    }

    #[test]
    fn chat_stream_frame_parses_text_delta() {
        let frame = parse_chat_stream_frame(
            r#"{"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}"#,
        )
        .unwrap();

        assert_eq!(frame.text_delta.as_deref(), Some("hello"));
        assert!(frame.tool_calls.is_empty());
    }

    #[test]
    fn chat_stream_frame_parses_tool_call_delta() {
        let frame = parse_chat_stream_frame(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"web_search","arguments":"{\"q\""}}]}}]}"#,
        )
        .unwrap();

        assert_eq!(frame.text_delta, None);
        assert_eq!(
            frame.tool_calls,
            vec![ChatStreamToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("web_search".into()),
                arguments: Some(r#"{"q""#.into()),
            }]
        );
    }

    #[test]
    fn chat_stream_frame_ignores_empty_tool_call_name_delta() {
        let frame = parse_chat_stream_frame(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"type":"function","function":{"name":"","arguments":"ue\"}"}}]}}]}"#,
        )
        .unwrap();

        assert_eq!(
            frame.tool_calls,
            vec![ChatStreamToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments: Some(r#"ue"}"#.into()),
            }]
        );
    }

    #[test]
    fn responses_body_extracts_multiple_generated_images() {
        let response = json!({
            "output": [
                {
                    "id": "ig_1",
                    "type": "image_generation_call",
                    "status": "completed",
                    "result": "base64_png_1"
                },
                {
                    "id": "ig_2",
                    "type": "image_generation_call",
                    "status": "completed",
                    "result": "base64_png_2"
                }
            ],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 2
            }
        });

        let parsed = parse_responses_response(&response);

        match parsed {
            ModelResponse::Done { attachments, .. } => {
                assert_eq!(
                    attachments,
                    vec![
                        MessageAttachment::Image {
                            name: "generated-image-1.png".into(),
                            media_type: "image/png".into(),
                            data: "base64_png_1".into(),
                        },
                        MessageAttachment::Image {
                            name: "generated-image-2.png".into(),
                            media_type: "image/png".into(),
                            data: "base64_png_2".into(),
                        },
                    ]
                );
            }
            other => panic!("expected Done response, got {other:?}"),
        }
    }
}
