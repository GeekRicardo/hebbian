/// Anthropic Claude API 格式转换
use serde_json::{json, Value};

use crate::types::{
    AssistantEntry, ModelError, ModelRequest, ModelResponse, ToolCall, ToolDefinition, ToolResult,
    TranscriptEntry, Usage, UserEntry, IMAGE_GENERATION_TOOL_NAME,
};
use common::reasoning::{anthropic_thinking_mode, AnthropicThinkingMode};

// ── 请求构建 ──────────────────────────────────────────────────────────────────

/// Claude Code OAuth token 必须看到这一行 system 才会被服务端识别为合法 CLI 流量。
const CLAUDE_CODE_BANNER: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

// DeepSeek v4 走 Anthropic Messages 端点时的 thinking 预算下限（与 protocols/openai.rs
// 同源；server 拒掉「thinking 启用 & max_tokens 不足」的请求）。
const DEEPSEEK_HIGH_THINKING_BUDGET: u32 = 32_768;
const DEEPSEEK_HIGH_SAFE_MAX_TOKENS: u32 = 65_536;
const DEEPSEEK_MAX_SAFE_MAX_TOKENS: u32 = 131_072;

const DEEPSEEK_ANTHROPIC_TOOL_THINKING_MISSING: &str =
    "DeepSeek thinking (Anthropic 端点) 模式下，历史里带 tool_use 的 assistant 消息缺失 \
     非空 thinking block。请压缩当前会话或开新会话后再继续使用 DeepSeek thinking。";

/// 是否为「走 Anthropic Messages 端点的 DeepSeek v4」模型。
///
/// 触发条件：模型名以 `deepseek-v4` 开头且不含 `nothinking`。具体走哪个 base_url
/// 由 provider 配置决定——只要 provider 是 anthropic-kind 而 model 命中此名段，
/// 就按 DeepSeek 方言重写 thinking/output_config 字段。
fn is_deepseek_v4_anthropic_dialect(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if m.contains("nothinking") {
        return false;
    }
    m.starts_with("deepseek-v4")
}

pub fn build_body(
    req: &ModelRequest,
    stream: bool,
    claude_code_oauth: bool,
) -> Result<Value, ModelError> {
    let dialect_deepseek = is_deepseek_v4_anthropic_dialect(&req.model);
    let messages: Vec<Value> = req
        .entries
        .iter()
        .filter_map(|e| entry_to_message(e, dialect_deepseek))
        .collect();

    // 三种 thinking schema 互不兼容，按模型家族走不同分支。
    // 见 common::reasoning::AnthropicThinkingMode。
    let mut max_tokens = req.max_tokens;
    let mut thinking_block: Option<Value> = None;
    let mut output_config: Option<Value> = None;
    let suppress_sampling = false;

    if dialect_deepseek {
        // DeepSeek v4 在 Anthropic 端点上的方言：
        //   - enabled: `thinking: { type: "enabled" }` + `output_config: { effort: high|max }`
        //   - disabled: `thinking: { type: "disabled" }`，剥掉 output_config
        //   - max_tokens 与 OpenAI 兼容路径同步抬升到 65536 / 131072
        //   - tool_use 多轮 fail-closed：历史里带 tool_use 的 assistant 消息必须有非空 thinking
        let enabled = req.reasoning.as_ref().is_some_and(|c| c.is_enabled());
        if enabled {
            for msg in &messages {
                ensure_deepseek_anthropic_tool_thinking(msg)?;
            }
            let effort = req
                .reasoning
                .as_ref()
                .map(|c| c.effective_effort().deepseek_effort())
                .unwrap_or("high");
            thinking_block = Some(json!({ "type": "enabled" }));
            output_config = Some(json!({ "effort": effort }));
            let desired = if effort == "max" {
                DEEPSEEK_MAX_SAFE_MAX_TOKENS
            } else {
                DEEPSEEK_HIGH_SAFE_MAX_TOKENS
            };
            if max_tokens <= DEEPSEEK_HIGH_THINKING_BUDGET {
                max_tokens = desired;
            }
        } else {
            thinking_block = Some(json!({ "type": "disabled" }));
        }
    } else if let (Some(cfg), Some(mode)) =
        (req.reasoning.as_ref(), anthropic_thinking_mode(&req.model))
    {
        if cfg.is_enabled() {
            let effort = cfg.effective_effort();
            match mode {
                AnthropicThinkingMode::Opus47Adaptive => {
                    // Opus 4.7：
                    // - adaptive + display:summarized 在 stream 模式下不发 thinking_delta（实测）
                    // - 退化成 enabled + budget_tokens
                    // - **必须**显式 display:"summarized"，否则 4.7 默认 display=omitted，
                    //   stream 同样不发 thinking_delta。
                    let budget = effort.anthropic_legacy_budget_tokens();
                    if max_tokens <= budget {
                        max_tokens = budget.saturating_add(1024);
                    }
                    thinking_block = Some(json!({
                        "type": "enabled",
                        "budget_tokens": budget,
                        "display": "summarized",
                    }));
                }
                AnthropicThinkingMode::Adaptive46 => {
                    // Opus/Sonnet 4.6：实测 API 不接受 thinking.adaptive.effort 字段
                    // （400 invalid_request_error: "Extra inputs are not permitted"），
                    // 没找到公开的 effort 控制点，退化成 legacy enabled + budget_tokens
                    // —— 这条路 4.6 仍然支持，且可控 budget。
                    let budget = effort.anthropic_legacy_budget_tokens();
                    if max_tokens <= budget {
                        max_tokens = budget.saturating_add(1024);
                    }
                    thinking_block = Some(json!({
                        "type": "enabled",
                        "budget_tokens": budget,
                    }));
                }
                AnthropicThinkingMode::LegacyEnabled => {
                    let budget = effort.anthropic_legacy_budget_tokens();
                    // budget_tokens 必须 < max_tokens；给输出留至少 1024 余量。
                    if max_tokens <= budget {
                        max_tokens = budget.saturating_add(1024);
                    }
                    thinking_block = Some(json!({
                        "type": "enabled",
                        "budget_tokens": budget,
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": stream,
    });

    if let Some(t) = thinking_block {
        body["thinking"] = t;
    }
    if let Some(oc) = output_config {
        body["output_config"] = oc;
    }
    if suppress_sampling {
        // Opus 4.7 在 adaptive 模式下显式拒绝 temperature/top_p/top_k。
        // 我们当前不主动注入这些字段，但万一上游 wrapper 透传了，统一抹掉。
        if let Some(obj) = body.as_object_mut() {
            obj.remove("temperature");
            obj.remove("top_p");
            obj.remove("top_k");
        }
    }

    body["system"] = build_system(req.system.as_deref(), claude_code_oauth);
    if body["system"].is_null() {
        body.as_object_mut().unwrap().remove("system");
    }

    if !req.tools.is_empty() {
        body["tools"] = json!(tool_defs(&req.tools));
    }

    // ── Prompt cache 标记 ─────────────────────────────────────────────────
    // Anthropic 支持最多 4 个 `cache_control: { type: "ephemeral" }` 标记，
    // 落到任意 content block 上后，从开头到该 block 的所有内容都会被缓存
    // （5 分钟 TTL；Claude 4 系列上 ~10% 折扣命中读、写入加价）。
    //
    // 我们贴 2 个标记：
    //   1. system 末尾 —— 最稳定的前缀，几乎所有轮都能命中
    //   2. 倒数第二条 messages 里最后一个 block —— 把"上一轮已经发过的历史"
    //      整段标缓存，让本轮第一次发就把它写进缓存，下一轮 0 成本读出来
    apply_cache_control(&mut body);

    Ok(body)
}

/// 把 `cache_control: ephemeral` 打到 system 末尾 + 倒数第二条 message 的尾 block。
/// 必须发生在 `system` / `messages` 都已写入 body 之后。
fn apply_cache_control(body: &mut Value) {
    // system 兼容两种形态：纯字符串 or [content blocks]。
    // Anthropic cache_control 必须挂在 block object 上，所以纯字符串得升格成 block。
    if let Some(sys) = body.get_mut("system") {
        match sys {
            Value::String(s) => {
                let text = std::mem::take(s);
                *sys = json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": { "type": "ephemeral" }
                }]);
            }
            Value::Array(arr) if !arr.is_empty() => {
                if let Some(last) = arr.last_mut() {
                    if let Some(obj) = last.as_object_mut() {
                        obj.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
                    }
                }
            }
            _ => {}
        }
    }

    // 把 cache_control 贴到「倒数第二条 message」的最后一个 content block 上。
    // 这样从 system 到这条消息为止的所有内容都会缓存；下一轮第一条新 user 消息就
    // 能命中这一段。如果消息只有 1 条，没法贴第二个标记，跳过。
    if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        if msgs.len() >= 2 {
            let idx = msgs.len() - 2;
            let target = &mut msgs[idx];
            // content 可能是 string，也可能是 [block]；统一升格成 [block] 后挂标记
            if let Some(obj) = target.as_object_mut() {
                let content = obj.entry("content").or_insert(Value::Null);
                match content {
                    Value::String(s) => {
                        let text = std::mem::take(s);
                        *content = json!([{
                            "type": "text",
                            "text": text,
                            "cache_control": { "type": "ephemeral" }
                        }]);
                    }
                    Value::Array(arr) if !arr.is_empty() => {
                        if let Some(last) = arr.last_mut() {
                            if let Some(block) = last.as_object_mut() {
                                block.insert(
                                    "cache_control".to_string(),
                                    json!({ "type": "ephemeral" }),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
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

fn entry_to_message(entry: &TranscriptEntry, inject_deepseek_thinking: bool) -> Option<Value> {
    match entry {
        TranscriptEntry::User(user) => Some(json!({"role": "user", "content": user_content(user)})),
        TranscriptEntry::Assistant(AssistantEntry {
            text,
            reasoning,
            tool_calls,
        }) => {
            if tool_calls.is_empty() {
                Some(json!({"role": "assistant", "content": text}))
            } else {
                let mut content: Vec<Value> = Vec::new();
                // DeepSeek v4 Anthropic 端点要求 tool_use 多轮里带回上一轮 thinking。
                // 仅 dialect_deepseek 时注入，避免影响 Anthropic 原生路径
                //（Anthropic 自家 thinking 回填另有 signature 字段要求，单独处理）。
                if inject_deepseek_thinking && !reasoning.trim().is_empty() {
                    content.push(json!({
                        "type": "thinking",
                        "thinking": reasoning,
                    }));
                }
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
            common::attachments::MessageAttachment::TextFile { .. } => {
                if let Some(text) = attachment.as_text_block() {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            common::attachments::MessageAttachment::Image {
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

/// DeepSeek v4 Anthropic 端点的 fail-closed 守卫：tool_use 多轮必须带非空 thinking。
fn ensure_deepseek_anthropic_tool_thinking(msg: &Value) -> Result<(), ModelError> {
    if msg.get("role").and_then(Value::as_str) != Some("assistant") {
        return Ok(());
    }
    let Some(content) = msg.get("content").and_then(Value::as_array) else {
        return Ok(());
    };
    let has_tool_use = content
        .iter()
        .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"));
    if !has_tool_use {
        return Ok(());
    }
    let has_nonempty_thinking = content.iter().any(|b| {
        b.get("type").and_then(Value::as_str) == Some("thinking")
            && b.get("thinking")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
    });
    if !has_nonempty_thinking {
        return Err(ModelError::Other(
            DEEPSEEK_ANTHROPIC_TOOL_THINKING_MISSING.into(),
        ));
    }
    Ok(())
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

    // 解析所有 content block —— thinking / text / tool_use 都要捕获。
    // 之前漏了 thinking block，开了 extended thinking 拿不到推理文本。
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    if let Some(arr) = v["content"].as_array() {
        for block in arr {
            match block["type"].as_str() {
                Some("text") => text.push_str(block["text"].as_str().unwrap_or("")),
                Some("thinking") => {
                    // adaptive 模式下可能是 summary，legacy enabled 模式下是完整推理；
                    // 字段名都是 `thinking`。
                    if let Some(s) = block["thinking"].as_str() {
                        reasoning.push_str(s);
                    } else if let Some(s) = block["summary"].as_str() {
                        reasoning.push_str(s);
                    }
                }
                Some("tool_use") => calls.push(ToolCall {
                    id: block["id"].as_str().unwrap_or("").to_string(),
                    name: block["name"].as_str().unwrap_or("").to_string(),
                    input: block["input"].clone(),
                }),
                _ => {}
            }
        }
    }

    if stop_reason == "tool_use" {
        ModelResponse::ToolCalls {
            text,
            reasoning,
            calls,
            attachments: Vec::new(),
            usage,
        }
    } else {
        ModelResponse::Done {
            text,
            reasoning,
            attachments: Vec::new(),
            usage,
        }
    }
}

/// Anthropic SSE 流事件解析结果。所有 `content_block_*` / `message_delta` 都打到这里。
///
/// - `Text` / `Thinking`：`content_block_delta` 里的文本增量。
/// - `ToolUseStart`：`content_block_start` 里 type=tool_use，给上层 ID/name/index。
/// - `ToolInputJsonDelta`：`content_block_delta.delta.type=input_json_delta`，
///   是 tool_use 入参 JSON 的字符串增量（**不是**结构化的，要拼起来 parse）。
/// - `MessageStart` / `MessageDelta`：`message_start` 给初始 input/cache token 和占位
///   output_tokens；`message_delta` 给最终 output_tokens 等终态字段。两者合并才是
///   完整 usage。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicStreamEvent {
    Text {
        index: usize,
        delta: String,
    },
    Thinking {
        index: usize,
        delta: String,
    },
    ToolUseStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolInputJsonDelta {
        index: usize,
        partial_json: String,
    },
    MessageStart {
        usage: Usage,
    },
    MessageDelta {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
}

pub fn parse_stream_event(event_type: &str, data: &str) -> Option<AnthropicStreamEvent> {
    let v: Value = serde_json::from_str(data).ok()?;
    match event_type {
        "content_block_start" => {
            let index = v["index"].as_u64()? as usize;
            let block = &v["content_block"];
            match block["type"].as_str()? {
                "tool_use" => {
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let name = block["name"].as_str().unwrap_or("").to_string();
                    Some(AnthropicStreamEvent::ToolUseStart { index, id, name })
                }
                // text / thinking 的 start 没文本载荷，跳过；后续靠 *_delta 补内容
                _ => None,
            }
        }
        "content_block_delta" => {
            let index = v["index"].as_u64()? as usize;
            match v["delta"]["type"].as_str() {
                Some("text_delta") | None => v["delta"]["text"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| AnthropicStreamEvent::Text {
                        index,
                        delta: s.to_string(),
                    }),
                Some("thinking_delta") => v["delta"]["thinking"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| AnthropicStreamEvent::Thinking {
                        index,
                        delta: s.to_string(),
                    }),
                Some("input_json_delta") => v["delta"]["partial_json"].as_str().map(|s| {
                    AnthropicStreamEvent::ToolInputJsonDelta {
                        index,
                        partial_json: s.to_string(),
                    }
                }),
                // signature_delta 等暂时不上抛
                _ => None,
            }
        }
        "message_start" => {
            // message_start.message.usage：入参 + 缓存命中/写入；output_tokens 是占位，
            // 等 message_delta 再覆盖。
            let message = v.get("message")?;
            Some(AnthropicStreamEvent::MessageStart {
                usage: parse_usage(message),
            })
        }
        "message_delta" => Some(AnthropicStreamEvent::MessageDelta {
            stop_reason: v["delta"]["stop_reason"].as_str().map(String::from),
            // message_delta.usage 只带 output_tokens 终态；缺省时该 SSE 没有 usage。
            usage: v.get("usage").map(|_| parse_usage(&v)),
        }),
        _ => None,
    }
}

fn parse_usage(v: &Value) -> Usage {
    let raw_input = v["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let cache_read = v["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let cache_creation = v["usage"]["cache_creation_input_tokens"]
        .as_u64()
        .unwrap_or(0);
    // Anthropic 把 cache_read / cache_creation 单列、不计入 input_tokens；
    // 我们对齐 OpenAI / DeepSeek 的口径，把三者相加暴露成 input_tokens 总数。
    Usage {
        input_tokens: raw_input + cache_read + cache_creation,
        output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_creation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TranscriptEntry, UserEntry};
    use common::attachments::MessageAttachment;

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
            reasoning: None,
        };

        let body = build_body(&req, false, false).unwrap();
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
            reasoning: None,
        };

        let body = build_body(&req, false, true).unwrap();
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
            reasoning: None,
        };

        let body = build_body(&req, false, true).unwrap();
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
            reasoning: None,
        };

        let body = build_body(&req, false, false).unwrap();
        // apply_cache_control 会把字符串 system 升格为带 cache_control 的 block 数组。
        let arr = body["system"]
            .as_array()
            .expect("system 已升格为 block 数组");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["text"], "Be terse.");
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
    }

    // ── DeepSeek v4 on Anthropic 端点的方言测试 ──────────────────────────────
    use crate::types::ToolCall;
    use common::ReasoningEffort;

    fn req_for_deepseek_anthropic(
        reasoning: Option<common::ReasoningConfig>,
        assistant_reasoning: &str,
        with_tool_call: bool,
        max_tokens: u32,
    ) -> ModelRequest {
        let mut entries: Vec<TranscriptEntry> =
            vec![TranscriptEntry::User(UserEntry::text("查一下"))];
        if with_tool_call {
            entries.push(TranscriptEntry::Assistant(AssistantEntry {
                text: String::new(),
                reasoning: assistant_reasoning.into(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "Bash".into(),
                    input: json!({ "command": "ls" }),
                }],
            }));
            entries.push(TranscriptEntry::ToolResults(vec![ToolResult {
                call_id: "call_1".into(),
                name: "Bash".into(),
                content: "a.txt".into(),
                artifact: None,
            }]));
        }
        ModelRequest {
            model: "deepseek-v4-pro".into(),
            system: None,
            entries,
            tools: vec![],
            max_tokens,
            reasoning,
        }
    }

    #[test]
    fn deepseek_v4_anthropic_enabled_emits_output_config_and_lifts_max_tokens() {
        let cfg = common::ReasoningConfig {
            enabled: Some(true),
            effort: Some(ReasoningEffort::Extra),
            long_context: None,
        };
        let body = build_body(
            &req_for_deepseek_anthropic(Some(cfg), "", false, 8192),
            false,
            false,
        )
        .unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["output_config"]["effort"], "max");
        assert_eq!(body["max_tokens"], 131_072);
    }

    #[test]
    fn deepseek_v4_anthropic_disabled_emits_explicit_off() {
        let cfg = common::ReasoningConfig {
            enabled: Some(false),
            effort: None,
            long_context: None,
        };
        let body = build_body(
            &req_for_deepseek_anthropic(Some(cfg), "", false, 8192),
            false,
            false,
        )
        .unwrap();
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn deepseek_v4_anthropic_tool_use_history_missing_thinking_fails_closed() {
        let cfg = common::ReasoningConfig {
            enabled: Some(true),
            effort: Some(ReasoningEffort::Extra),
            long_context: None,
        };
        let req = req_for_deepseek_anthropic(Some(cfg), "", true, 8192);
        let err = build_body(&req, false, false).unwrap_err();
        let crate::types::ModelError::Other(msg) = err else {
            panic!("expected ModelError::Other");
        };
        assert!(msg.contains("thinking"), "msg = {msg}");
    }

    #[test]
    fn deepseek_v4_anthropic_tool_use_history_with_reasoning_injects_thinking_block() {
        let cfg = common::ReasoningConfig {
            enabled: Some(true),
            effort: Some(ReasoningEffort::Extra),
            long_context: None,
        };
        let req = req_for_deepseek_anthropic(Some(cfg), "之前的思考", true, 8192);
        let body = build_body(&req, false, false).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
        let content = assistant["content"].as_array().unwrap();
        // 第一块必须是 thinking block，且非空
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "之前的思考");
        // 后面跟着 tool_use
        assert!(content.iter().any(|b| b["type"] == "tool_use"));
    }

    #[test]
    fn deepseek_nothinking_variant_does_not_trigger_dialect() {
        // -nothinking 后缀的模型不参与 thinking 方言；body 里不应有 thinking 字段
        let cfg = common::ReasoningConfig {
            enabled: Some(true),
            effort: Some(ReasoningEffort::Extra),
            long_context: None,
        };
        let req = ModelRequest {
            model: "deepseek-v4-pro-nothinking".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(cfg),
        };
        let body = build_body(&req, false, false).unwrap();
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn anthropic_native_model_unaffected_by_deepseek_dialect() {
        // claude 系列依然走原有的三态 thinking schema，不该走 DeepSeek 分支
        let cfg = common::ReasoningConfig {
            enabled: Some(true),
            effort: Some(ReasoningEffort::High),
            long_context: None,
        };
        let req = ModelRequest {
            model: "claude-sonnet-4-5".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(cfg),
        };
        let body = build_body(&req, false, false).unwrap();
        // claude-sonnet-4-5 走 LegacyEnabled：thinking.type=enabled + budget_tokens
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body["thinking"]["budget_tokens"].is_number());
        // 不应注入 DeepSeek 的 output_config
        assert!(body.get("output_config").is_none());
    }
}
