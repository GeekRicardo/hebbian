use async_trait::async_trait;
use std::collections::BTreeMap;

use serde_json::Value;

use crate::config::{AuthMode, Provider};
use crate::{
    client::ModelClient,
    protocols::openai as proto,
    providers::apply_auth,
    types::{
        has_image_generation_tool, ModelError, ModelRequest, ModelResponse, ModelStreamEvent,
        ToolCall, ToolCallStreamDelta, Usage,
    },
};
use platform::CancelFlag;

pub struct OpenAiClient {
    provider: Provider,
    http: reqwest::Client,
}

impl OpenAiClient {
    pub fn new(provider: Provider) -> Result<Self, ModelError> {
        let http = super::build_http_client()?;
        Ok(Self { provider, http })
    }

    fn chat_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.provider.base_url.trim_end_matches('/')
        )
    }

    fn responses_url(&self) -> String {
        let base = if matches!(self.provider.auth_mode, AuthMode::OauthCodex)
            && self.provider.base_url.trim_end_matches('/') == "https://api.openai.com/v1"
        {
            "https://chatgpt.com/backend-api/codex"
        } else {
            self.provider.base_url.trim_end_matches('/')
        };
        format!("{base}/responses")
    }

    fn uses_codex_oauth(&self) -> bool {
        matches!(self.provider.auth_mode, AuthMode::OauthCodex)
    }

    fn should_use_responses(&self, req: &ModelRequest) -> bool {
        self.uses_codex_oauth()
            || req
                .tools
                .iter()
                .any(|tool| tool.name == crate::types::IMAGE_GENERATION_TOOL_NAME)
    }
}

#[async_trait]
impl ModelClient for OpenAiClient {
    fn provider_id(&self) -> &str {
        &self.provider.id
    }

    fn supports_streaming_tools(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
    ) -> Result<ModelResponse, ModelError> {
        let use_responses = self.should_use_responses(&req);
        if use_responses {
            return self
                .complete_responses(req, cancel, self.uses_codex_oauth())
                .await;
        }

        let body = proto::build_body(&req, false);

        super::retry_request(cancel, || {
            let body = body.clone();
            async move {
                let resp = apply_auth(self.http.post(self.chat_url()), &self.provider)
                    .json(&body)
                    .send()
                    .await?;
                let status = resp.status().as_u16();
                let text = resp.text().await?;
                if status >= 400 {
                    return Err(ModelError::Http { status, body: text });
                }
                let v: Value = serde_json::from_str(&text)?;
                Ok(proto::parse_response(&v))
            }
        })
        .await
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        let use_responses = self.should_use_responses(&req);
        if use_responses {
            return self
                .stream_responses(req, cancel, on_event, self.uses_codex_oauth())
                .await;
        }

        let body = proto::build_body(&req, true);

        let resp = super::retry_request(cancel.clone(), || {
            let body = body.clone();
            async move {
                let r = apply_auth(self.http.post(self.chat_url()), &self.provider)
                    .json(&body)
                    .send()
                    .await?;
                let status = r.status().as_u16();
                if status >= 400 {
                    let body = r.text().await?;
                    return Err(ModelError::Http { status, body });
                }
                Ok(r)
            }
        })
        .await?;

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full = String::new();
        let mut full_reasoning = String::new();
        let mut tool_call_parts = Vec::new();

        while let Some(chunk) = super::next_stream_chunk_or_cancel(&mut stream, &cancel).await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buf.find("\n\n").or_else(|| buf.find("\r\n\r\n")) {
                let skip = if buf[pos..].starts_with("\r\n\r\n") {
                    4
                } else {
                    2
                };
                let frame = buf[..pos].to_string();
                buf = buf[pos + skip..].to_string();

                for line in frame.lines() {
                    let line = line.trim_end_matches('\r');
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data == "[DONE]" || data.is_empty() {
                            continue;
                        }
                        if let Some(parsed) = proto::parse_chat_stream_frame(data) {
                            if let Some(delta) = parsed.reasoning_delta {
                                full_reasoning.push_str(&delta);
                                on_event(ModelStreamEvent::ReasoningDelta {
                                    text: delta,
                                });
                            }
                            if let Some(delta) = parsed.text_delta {
                                on_event(ModelStreamEvent::TextDelta {
                                    text: delta.clone(),
                                });
                                full.push_str(&delta);
                            }
                            for delta in parsed.tool_calls {
                                emit_tool_call_delta(
                                    on_event,
                                    delta.index,
                                    clean_optional(delta.id.as_deref()),
                                    clean_optional(delta.name.as_deref()),
                                    nonempty_optional(delta.arguments.as_deref()),
                                );
                                apply_tool_call_delta(&mut tool_call_parts, delta);
                            }
                        }
                    }
                }
            }
        }

        let calls = finish_tool_calls(tool_call_parts);
        if calls.is_empty() {
            Ok(ModelResponse::Done {
                text: full,
                reasoning: full_reasoning,
                attachments: Vec::new(),
                usage: Usage::default(),
            })
        } else {
            Ok(ModelResponse::ToolCalls {
                text: full,
                reasoning: full_reasoning,
                calls,
                attachments: Vec::new(),
                usage: Usage::default(),
            })
        }
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn apply_tool_call_delta(
    tool_call_parts: &mut Vec<PartialToolCall>,
    delta: proto::ChatStreamToolCallDelta,
) {
    while tool_call_parts.len() <= delta.index {
        tool_call_parts.push(PartialToolCall::default());
    }

    let part = &mut tool_call_parts[delta.index];
    if let Some(id) = delta.id.filter(|id| !id.trim().is_empty()) {
        part.id = Some(id);
    }
    if let Some(name) = delta.name.filter(|name| !name.trim().is_empty()) {
        part.name = Some(name);
    }
    if let Some(arguments) = delta.arguments {
        part.arguments.push_str(&arguments);
    }
}

fn finish_tool_calls(tool_call_parts: Vec<PartialToolCall>) -> Vec<ToolCall> {
    tool_call_parts
        .into_iter()
        .filter_map(|part| {
            let id = part.id?;
            let name = part.name?;
            let arguments = if part.arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&part.arguments).unwrap_or(Value::Null)
            };
            Some(ToolCall {
                id,
                name,
                input: arguments,
            })
        })
        .collect()
}

fn emit_tool_call_delta(
    on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    index: usize,
    id: Option<String>,
    name: Option<String>,
    arguments_delta: Option<String>,
) {
    if id.is_none() && name.is_none() && arguments_delta.is_none() {
        return;
    }

    on_event(ModelStreamEvent::ToolCallDelta(ToolCallStreamDelta {
        index,
        id,
        name,
        arguments_delta,
    }));
}

fn emit_responses_tool_call_item(
    on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
    output_index: Option<usize>,
    item: &Value,
) {
    if item["type"].as_str() != Some("function_call") {
        return;
    }

    emit_tool_call_delta(
        on_event,
        response_tool_index(output_index),
        clean_optional(item["call_id"].as_str()).or_else(|| clean_optional(item["id"].as_str())),
        clean_optional(item["name"].as_str()),
        None,
    );
}

fn response_tool_index(output_index: Option<usize>) -> usize {
    output_index.unwrap_or(0)
}

fn clean_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn nonempty_optional(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(str::to_string)
}

fn responses_http_error(
    status: u16,
    body: String,
    image_generation_enabled: bool,
    model: &str,
) -> ModelError {
    if !image_generation_enabled {
        return ModelError::Http { status, body };
    }

    let body_lower = body.to_lowercase();
    if status == 403 && body_lower.contains("codex official clients") {
        return ModelError::Other(
            "图片生成请求已切到 OpenAI Responses，但当前代理拒绝了该客户端：只允许 Codex 官方客户端。请切换到支持 image_generation 的 OpenAI provider，或在代理端关闭/放行 codex_cli_only。"
                .to_string(),
        );
    }

    if status == 400 && model.to_lowercase().starts_with("gpt-image-") {
        return ModelError::Other(format!(
            "不要把 {model} 作为对话模型来跑生图；请选择 gpt-5.5 / gpt-5.4 这类对话模型，并启用 image_generation 工具。原始错误：{body}"
        ));
    }

    ModelError::Http { status, body }
}

impl OpenAiClient {
    async fn complete_responses(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        codex_oauth: bool,
    ) -> Result<ModelResponse, ModelError> {
        let body = proto::build_responses_body(&req, false, codex_oauth);
        let has_image_generation_tool = has_image_generation_tool(&req.tools);
        let model = req.model.clone();

        super::retry_request(cancel, || {
            let body = body.clone();
            let model = model.clone();
            async move {
                let resp = apply_auth(self.http.post(self.responses_url()), &self.provider)
                    .json(&body)
                    .send()
                    .await?;
                let status = resp.status().as_u16();
                let text = resp.text().await?;
                if status >= 400 {
                    return Err(responses_http_error(
                        status,
                        text,
                        has_image_generation_tool,
                        &model,
                    ));
                }
                parse_responses_body_or_sse(&text)
            }
        })
        .await
    }

    async fn stream_responses(
        &self,
        req: ModelRequest,
        cancel: CancelFlag,
        on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync),
        codex_oauth: bool,
    ) -> Result<ModelResponse, ModelError> {
        let body = proto::build_responses_body(&req, true, codex_oauth);
        let has_image_generation_tool = has_image_generation_tool(&req.tools);
        let model = req.model.clone();

        let resp = super::retry_request(cancel.clone(), || {
            let body = body.clone();
            let model = model.clone();
            async move {
                let r = apply_auth(self.http.post(self.responses_url()), &self.provider)
                    .json(&body)
                    .send()
                    .await?;
                let status = r.status().as_u16();
                if status >= 400 {
                    let body = r.text().await?;
                    return Err(responses_http_error(
                        status,
                        body,
                        has_image_generation_tool,
                        &model,
                    ));
                }
                Ok(r)
            }
        })
        .await?;

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full = String::new();
        let mut full_reasoning = String::new();
        let mut output_items = ResponsesOutputAccumulator::default();
        let mut completed: Option<Value> = None;

        while let Some(chunk) = super::next_stream_chunk_or_cancel(&mut stream, &cancel).await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(frame) = take_sse_frame(&mut buf) {
                match parse_responses_frame(&frame)? {
                    ParsedResponsesFrame::Delta(delta) => {
                        on_event(ModelStreamEvent::TextDelta {
                            text: delta.clone(),
                        });
                        full.push_str(&delta);
                    }
                    ParsedResponsesFrame::ReasoningDelta(delta) => {
                        full_reasoning.push_str(&delta);
                        on_event(ModelStreamEvent::ReasoningDelta { text: delta });
                    }
                    ParsedResponsesFrame::OutputItemAdded { output_index, item }
                    | ParsedResponsesFrame::OutputItemDone { output_index, item } => {
                        emit_responses_tool_call_item(on_event, output_index, &item);
                        output_items.put_output_item(output_index, item);
                    }
                    ParsedResponsesFrame::FunctionCallArgumentsDelta {
                        output_index,
                        item_id,
                        delta,
                    } => {
                        emit_tool_call_delta(
                            on_event,
                            response_tool_index(output_index),
                            clean_optional(item_id.as_deref()),
                            None,
                            nonempty_optional(Some(&delta)),
                        );
                        output_items.append_function_call_arguments(
                            output_index,
                            item_id.as_deref(),
                            &delta,
                        );
                    }
                    ParsedResponsesFrame::FunctionCallArgumentsDone {
                        output_index,
                        item_id,
                        call_id,
                        name,
                        arguments,
                    } => {
                        emit_tool_call_delta(
                            on_event,
                            response_tool_index(output_index),
                            clean_optional(call_id.as_deref())
                                .or_else(|| clean_optional(item_id.as_deref())),
                            clean_optional(name.as_deref()),
                            None,
                        );
                        output_items.finish_function_call_arguments(
                            output_index,
                            item_id.as_deref(),
                            call_id.as_deref(),
                            name.as_deref(),
                            &arguments,
                        );
                    }
                    ParsedResponsesFrame::Completed(response) => completed = Some(response),
                    ParsedResponsesFrame::Ignore => {}
                }
            }
        }

        let mut response = completed.unwrap_or_else(|| serde_json::json!({}));
        let output_items = merge_response_output(response.get("output"), output_items);
        if !output_items.is_empty() {
            response["output"] = Value::Array(output_items);
        }
        let parsed = proto::parse_responses_response(&response);

        match parsed {
            ModelResponse::Done {
                text,
                reasoning,
                attachments,
                usage,
            } => {
                // 流式累积的 reasoning 优先级最高
                let final_reasoning = if !full_reasoning.is_empty() {
                    full_reasoning
                } else {
                    reasoning
                };
                if full.is_empty() {
                    Ok(ModelResponse::Done {
                        text,
                        reasoning: final_reasoning,
                        attachments,
                        usage,
                    })
                } else {
                    Ok(ModelResponse::Done {
                        text: full,
                        reasoning: final_reasoning,
                        attachments,
                        usage,
                    })
                }
            }
            other => Ok(other),
        }
    }
}

enum ParsedResponsesFrame {
    Delta(String),
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
    Ignore,
}

#[derive(Default)]
struct ResponsesOutputAccumulator {
    indexed_items: BTreeMap<usize, Value>,
    unindexed_items: Vec<Value>,
}

impl ResponsesOutputAccumulator {
    fn put_output_item(&mut self, output_index: Option<usize>, item: Value) {
        if let Some(index) = output_index.or_else(|| self.find_index_for_item(&item)) {
            let entry = self
                .indexed_items
                .entry(index)
                .or_insert(serde_json::json!({}));
            merge_response_output_item(entry, item);
            return;
        }

        if let Some(pos) = self.find_unindexed_pos_for_item(&item) {
            merge_response_output_item(&mut self.unindexed_items[pos], item);
        } else {
            self.unindexed_items.push(item);
        }
    }

    fn append_function_call_arguments(
        &mut self,
        output_index: Option<usize>,
        item_id: Option<&str>,
        delta: &str,
    ) {
        if delta.is_empty() {
            return;
        }

        let item = self.function_call_item_mut(output_index, item_id);
        let current = item["arguments"].as_str().unwrap_or("").to_string();
        item["arguments"] = Value::String(format!("{current}{delta}"));
    }

    fn finish_function_call_arguments(
        &mut self,
        output_index: Option<usize>,
        item_id: Option<&str>,
        call_id: Option<&str>,
        name: Option<&str>,
        arguments: &str,
    ) {
        let item = self.function_call_item_mut(output_index, item_id);
        if let Some(call_id) = call_id {
            item["call_id"] = Value::String(call_id.to_string());
        }
        if let Some(name) = name {
            item["name"] = Value::String(name.to_string());
        }
        if !arguments.is_empty() || item["arguments"].as_str().unwrap_or("").is_empty() {
            item["arguments"] = Value::String(arguments.to_string());
        }
        item["status"] = Value::String("completed".to_string());
    }

    fn into_output_items(self) -> Vec<Value> {
        self.indexed_items
            .into_values()
            .chain(self.unindexed_items)
            .collect()
    }

    fn merge_from(&mut self, other: ResponsesOutputAccumulator) {
        for (index, item) in other.indexed_items {
            self.put_output_item(Some(index), item);
        }
        for item in other.unindexed_items {
            self.put_output_item(None, item);
        }
    }

    fn function_call_item_mut(
        &mut self,
        output_index: Option<usize>,
        item_id: Option<&str>,
    ) -> &mut Value {
        if let Some(index) = output_index.or_else(|| self.find_index_for_id(item_id)) {
            let item = self
                .indexed_items
                .entry(index)
                .or_insert_with(|| minimal_function_call_item(item_id));
            ensure_function_call_item(item, item_id);
            return item;
        }

        if let Some(pos) = self.find_unindexed_pos_for_id(item_id) {
            let item = &mut self.unindexed_items[pos];
            ensure_function_call_item(item, item_id);
            return item;
        }

        self.unindexed_items
            .push(minimal_function_call_item(item_id));
        self.unindexed_items.last_mut().unwrap()
    }

    fn find_index_for_item(&self, item: &Value) -> Option<usize> {
        output_item_id(item).and_then(|id| self.find_index_for_id(Some(id)))
    }

    fn find_index_for_id(&self, item_id: Option<&str>) -> Option<usize> {
        let expected_id = item_id?;
        self.indexed_items
            .iter()
            .find_map(|(index, item)| (output_item_id(item) == Some(expected_id)).then_some(*index))
    }

    fn find_unindexed_pos_for_item(&self, item: &Value) -> Option<usize> {
        output_item_id(item).and_then(|id| self.find_unindexed_pos_for_id(Some(id)))
    }

    fn find_unindexed_pos_for_id(&self, item_id: Option<&str>) -> Option<usize> {
        let expected_id = item_id?;
        self.unindexed_items
            .iter()
            .position(|item| output_item_id(item) == Some(expected_id))
    }
}

fn minimal_function_call_item(item_id: Option<&str>) -> Value {
    let mut item = serde_json::json!({
        "type": "function_call",
        "arguments": ""
    });
    if let Some(item_id) = item_id {
        item["id"] = Value::String(item_id.to_string());
    }
    item
}

fn ensure_function_call_item(item: &mut Value, item_id: Option<&str>) {
    if !item.is_object() {
        *item = serde_json::json!({});
    }
    if item["type"].as_str().unwrap_or("").is_empty() {
        item["type"] = Value::String("function_call".to_string());
    }
    if let Some(item_id) = item_id {
        if item["id"].as_str().unwrap_or("").is_empty() {
            item["id"] = Value::String(item_id.to_string());
        }
    }
    if !item["arguments"].is_string() {
        item["arguments"] = Value::String(String::new());
    }
}

fn merge_response_output_item(existing: &mut Value, incoming: Value) {
    let Some(existing_obj) = existing.as_object_mut() else {
        *existing = incoming;
        return;
    };
    let Value::Object(incoming_obj) = incoming else {
        *existing = incoming;
        return;
    };

    for (key, value) in incoming_obj {
        let preserve_existing = match (existing_obj.get(&key), &value) {
            (Some(Value::String(current)), Value::String(next)) => {
                !current.is_empty() && next.is_empty()
            }
            (Some(current), Value::Null) => !current.is_null(),
            _ => false,
        };
        if !preserve_existing {
            existing_obj.insert(key, value);
        }
    }
}

fn output_item_id(item: &Value) -> Option<&str> {
    item["id"].as_str()
}

fn merge_response_output(
    completed_output: Option<&Value>,
    accumulated: ResponsesOutputAccumulator,
) -> Vec<Value> {
    let Some(completed_items) = completed_output.and_then(Value::as_array) else {
        return accumulated.into_output_items();
    };

    let mut merged = ResponsesOutputAccumulator::default();
    for (index, item) in completed_items.iter().cloned().enumerate() {
        merged.put_output_item(Some(index), item);
    }
    merged.merge_from(accumulated);
    merged.into_output_items()
}

fn parse_responses_body_or_sse(body: &str) -> Result<ModelResponse, ModelError> {
    let trimmed = body.trim_start();
    if trimmed.starts_with('{') {
        let v: Value = serde_json::from_str(body)?;
        return Ok(proto::parse_responses_response(&v));
    }

    let mut buf = body.to_string();
    let mut output_items = ResponsesOutputAccumulator::default();
    let mut completed: Option<Value> = None;

    while let Some(frame) = take_sse_frame(&mut buf) {
        match parse_responses_frame(&frame)? {
            ParsedResponsesFrame::OutputItemAdded { output_index, item }
            | ParsedResponsesFrame::OutputItemDone { output_index, item } => {
                output_items.put_output_item(output_index, item);
            }
            ParsedResponsesFrame::FunctionCallArgumentsDelta {
                output_index,
                item_id,
                delta,
            } => output_items.append_function_call_arguments(
                output_index,
                item_id.as_deref(),
                &delta,
            ),
            ParsedResponsesFrame::FunctionCallArgumentsDone {
                output_index,
                item_id,
                call_id,
                name,
                arguments,
            } => output_items.finish_function_call_arguments(
                output_index,
                item_id.as_deref(),
                call_id.as_deref(),
                name.as_deref(),
                &arguments,
            ),
            ParsedResponsesFrame::Completed(response) => completed = Some(response),
            ParsedResponsesFrame::Delta(_)
            | ParsedResponsesFrame::ReasoningDelta(_)
            | ParsedResponsesFrame::Ignore => {}
        }
    }

    let mut response = completed
        .ok_or_else(|| ModelError::Other("Responses SSE 缺少 response.completed".to_string()))?;
    let output_items = merge_response_output(response.get("output"), output_items);
    if !output_items.is_empty() {
        response["output"] = Value::Array(output_items);
    }
    Ok(proto::parse_responses_response(&response))
}

fn take_sse_frame(buf: &mut String) -> Option<String> {
    let (pos, skip) = if let Some(pos) = buf.find("\n\n") {
        (pos, 2)
    } else if let Some(pos) = buf.find("\r\n\r\n") {
        (pos, 4)
    } else {
        return None;
    };
    let frame = buf[..pos].to_string();
    *buf = buf[pos + skip..].to_string();
    Some(frame)
}

fn parse_responses_frame(frame: &str) -> Result<ParsedResponsesFrame, ModelError> {
    let mut event_name = "";
    let mut data_lines = Vec::new();

    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event_name = value.trim();
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start());
        }
    }

    if data_lines.is_empty() {
        return Ok(ParsedResponsesFrame::Ignore);
    }

    let data = data_lines.join("\n");
    match proto::parse_responses_sse_event(event_name, &data) {
        proto::ResponsesSseEvent::TextDelta(delta) => Ok(ParsedResponsesFrame::Delta(delta)),
        proto::ResponsesSseEvent::ReasoningDelta(delta) => {
            Ok(ParsedResponsesFrame::ReasoningDelta(delta))
        }
        proto::ResponsesSseEvent::OutputItemAdded { output_index, item } => {
            Ok(ParsedResponsesFrame::OutputItemAdded { output_index, item })
        }
        proto::ResponsesSseEvent::OutputItemDone { output_index, item } => {
            Ok(ParsedResponsesFrame::OutputItemDone { output_index, item })
        }
        proto::ResponsesSseEvent::FunctionCallArgumentsDelta {
            output_index,
            item_id,
            delta,
        } => Ok(ParsedResponsesFrame::FunctionCallArgumentsDelta {
            output_index,
            item_id,
            delta,
        }),
        proto::ResponsesSseEvent::FunctionCallArgumentsDone {
            output_index,
            item_id,
            call_id,
            name,
            arguments,
        } => Ok(ParsedResponsesFrame::FunctionCallArgumentsDone {
            output_index,
            item_id,
            call_id,
            name,
            arguments,
        }),
        proto::ResponsesSseEvent::Completed(response) => {
            Ok(ParsedResponsesFrame::Completed(response))
        }
        proto::ResponsesSseEvent::Failed(message) => Err(ModelError::Other(message)),
        proto::ResponsesSseEvent::Ignore => Ok(ParsedResponsesFrame::Ignore),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderKind;
    use serde_json::json;

    #[test]
    fn chat_tool_call_delta_keeps_existing_name_when_later_delta_has_no_name() {
        let mut parts = Vec::new();
        apply_tool_call_delta(
            &mut parts,
            proto::ChatStreamToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("web_search".into()),
                arguments: Some(r#"{"query":"#.into()),
            },
        );
        apply_tool_call_delta(
            &mut parts,
            proto::ChatStreamToolCallDelta {
                index: 0,
                id: None,
                name: Some(String::new()),
                arguments: Some(r#""rust"}"#.into()),
            },
        );

        let calls = finish_tool_calls(parts);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].input, json!({"query": "rust"}));
    }

    #[test]
    fn api_key_openai_uses_responses_when_image_generation_tool_is_enabled() {
        let client = OpenAiClient::new(Provider {
            id: "openai".into(),
            name: "OpenAI".into(),
            kind: ProviderKind::Openai,
            enabled: true,
            auth_mode: AuthMode::ApiKey,
            base_url: "https://api.openai.com/v1".into(),
            api_key: "test".into(),
            refresh_token: None,
            token_expires_at: None,
            account_id: None,
            extra_headers: Default::default(),
            models: Vec::new(),
            default_model: None,
        })
        .unwrap();
        let req = ModelRequest {
            model: "gpt-5.5".into(),
            system: None,
            entries: Vec::new(),
            tools: vec![crate::types::ToolDefinition {
                name: crate::types::IMAGE_GENERATION_TOOL_NAME.into(),
                description: "生成图片".into(),
                parameters: json!({"type": "object"}),
            }],
            max_tokens: 8192,
        };

        assert!(client.should_use_responses(&req));
    }

    #[test]
    fn image_generation_codex_client_gate_error_is_actionable() {
        let err = responses_http_error(
            403,
            r#"{"error":{"message":"This account only allows Codex official clients"}}"#
                .to_string(),
            true,
            "gpt-5.5",
        );

        assert!(err.to_string().contains("当前代理拒绝"));
        assert!(err.to_string().contains("codex_cli_only"));
    }

    #[test]
    fn image_generation_direct_image_model_error_is_actionable() {
        let err = responses_http_error(
            400,
            "The 'gpt-image-2' model is not supported when using Codex with a ChatGPT account."
                .to_string(),
            true,
            "gpt-image-2",
        );

        assert!(err.to_string().contains("不要把 gpt-image-2 作为对话模型"));
        assert!(err.to_string().contains("启用 image_generation 工具"));
    }

    #[test]
    fn responses_sse_accumulates_function_call_arguments_delta_and_done() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"in_progress\",\"call_id\":\"call_1\",\"name\":\"web_search\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"{\\\"query\\\"\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\":\\\"rust docs\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"output_index\":0,\"item_id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"web_search\",\"arguments\":\"{\\\"query\\\":\\\"rust docs\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"id\":\"resp_1\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":5}}}\n\n",
        );

        let parsed = parse_responses_body_or_sse(body).unwrap();

        match parsed {
            ModelResponse::ToolCalls { calls, usage, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "web_search");
                assert_eq!(calls[0].input, json!({"query": "rust docs"}));
                assert_eq!(usage.input_tokens, 3);
                assert_eq!(usage.output_tokens, 5);
            }
            other => panic!("expected ToolCalls response, got {other:?}"),
        }
    }

    #[test]
    fn responses_sse_preserves_accumulated_arguments_when_done_item_is_empty() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"in_progress\",\"call_id\":\"call_1\",\"name\":\"web_fetch\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"{\\\"url\\\":\\\"https://example.com\\\"\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\",\\\"prompt\\\":\\\"summarize\\\"}\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"call_1\",\"name\":\"web_fetch\",\"arguments\":\"\"}}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"id\":\"resp_1\",\"output\":[],\"usage\":{\"input_tokens\":4,\"output_tokens\":6}}}\n\n",
        );

        let parsed = parse_responses_body_or_sse(body).unwrap();

        match parsed {
            ModelResponse::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "web_fetch");
                assert_eq!(
                    calls[0].input,
                    json!({"url": "https://example.com", "prompt": "summarize"})
                );
            }
            other => panic!("expected ToolCalls response, got {other:?}"),
        }
    }

    #[test]
    fn responses_sse_uses_top_level_name_for_function_call_item() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"output_index\":0,\"item_id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"web_search\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"in_progress\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"{\\\"query\\\":\\\"weather\\\"}\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"output_index\":0,\"item_id\":\"fc_1\",\"call_id\":\"call_1\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"completed\",\"arguments\":\"\"}}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"id\":\"resp_1\",\"output\":[],\"usage\":{\"input_tokens\":4,\"output_tokens\":6}}}\n\n",
        );

        let parsed = parse_responses_body_or_sse(body).unwrap();

        match parsed {
            ModelResponse::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "web_search");
                assert_eq!(calls[0].input, json!({"query": "weather"}));
            }
            other => panic!("expected ToolCalls response, got {other:?}"),
        }
    }
}
