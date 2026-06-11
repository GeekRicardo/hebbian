/// Anthropic Claude API 格式转换
use serde_json::{json, Value};

use crate::types::{
    AssistantEntry, FinishReason, ModelError, ModelRequest, ModelResponse, ToolCall,
    ToolDefinition, ToolResult, TranscriptEntry, Usage, UserEntry, IMAGE_GENERATION_TOOL_NAME,
};
use common::attachments::MessageAttachment;

/// 把 Anthropic 的 `stop_reason` 归一成 [`FinishReason`]（架构 §4.11.4）。
/// `tool_use` 由调用方前置分支拦掉，不会走到这里。
pub fn map_anthropic_finish(stop_reason: &str) -> FinishReason {
    match stop_reason {
        "end_turn" | "stop_sequence" | "" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "refusal" => FinishReason::Refusal,
        other => FinishReason::Other(other.to_string()),
    }
}
use common::reasoning::{
    anthropic_supports_fallbacks, anthropic_thinking_mode, AnthropicThinkingMode,
};

// ── 请求构建 ──────────────────────────────────────────────────────────────────

/// Claude Code OAuth token 必须看到这一行 system 才会被服务端识别为合法 CLI 流量。
/// CC 兼容模式把它作为 system 第一个 block；harness 正文（base_system.md，中性身份开头）
/// 作为第二个 block，对应真 CC 的 system 结构。
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
    account_uuid: Option<&str>,
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
        //
        // ReasoningConfig 的 None / enabled=None 都解读为「沿用模型默认」——
        // DeepSeek-V4 的模型默认是 thinking ON（与 web 协议 + 同类项目对齐）。
        // 只有显式 enabled=Some(false) 才视为关闭。
        let enabled = req
            .reasoning
            .as_ref()
            .map_or(true, |c| c.enabled.unwrap_or(true));
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

    // Claude Code 兼容 / OAuth 需要 adaptive thinking。adaptive 要求 max_tokens 远大于
    // 默认 8192：实测太小会触发 sub2api 代理的 unknown_messages_shape 错误。
    // 抬到 64000（与 claude-code 客户端一致），放在 body 构造前生效。
    if claude_code_oauth && max_tokens < 64000 {
        max_tokens = 64000;
    }

    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": stream,
    });

    // Claude Code 兼容模式：代理只接受 adaptive thinking，不接受 enabled + budget_tokens。
    // effort 取用户选的思考强度（按模型量程：4.7/4.8 可达 xhigh，4.6 最高 high），
    // 不再写死 high——否则思考强度选择对所有走 CC 兼容 / OAuth 的 provider 完全失效。
    // reasoning 未设时用 ReasoningEffort 默认（Extra → 4.8 走 xhigh），符合「默认想清楚」。
    //
    // display 字段：不同代理行为不同。
    // - 直连 Anthropic API（OAuth）：4.7/4.8 的 adaptive 默认 display=omitted，思考会计费但
    //   不发 stream thinking_delta，必须加 `"display": "summarized"` 才外显推理摘要。
    // - sub2api 等第三方代理：不接受 display 字段（400 unknown_messages_shape），
    //   且代理服务端可能自行决定是否返回 thinking_delta。
    // 这里统一不加 display，各代理按各自默认行为处理。
    if claude_code_oauth {
        let effort = req
            .reasoning
            .as_ref()
            .map(|c| c.effective_effort())
            .unwrap_or_default()
            .anthropic_adaptive_effort_for_model(&req.model);
        thinking_block = Some(json!({ "type": "adaptive" }));
        output_config = Some(json!({ "effort": effort }));
    }

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

    // Claude Code 客户端特征：metadata.user_id + context_management + diagnostics + fallbacks。
    //
    // diagnostics / fallbacks 是顶层字段，必须配对应的 enabling beta 才被服务端 schema
    // 接受：diagnostics 需 cache-diagnosis-*、fallbacks 需 server-side-fallback-*（两者都在
    // OAuth 请求头 anthropic-beta 里固定带上，见 providers/mod.rs）。字段与 beta 必须成对——
    // 只发字段不带 beta 会被服务端当未知字段直接 400 "Extra inputs are not permitted"。
    //
    // fallbacks 还有 per-model 限制：只有 Fable 系列支持，其它模型带它会 400
    // "does not support the fallbacks parameter"（见 anthropic_supports_fallbacks）。
    // diagnostics 无 per-model 限制，所有模型通用。
    //
    // billing header（x-anthropic-billing-header）不发——等价于 CLAUDE_CODE_ATTRIBUTION_HEADER=0
    // 的合法 CC 行为：真实 cch 由 CC 客户端在网络层运行时注入、无法稳定复现，
    // 发占位值反而坏掉 prompt cache 的稳定前缀。
    if claude_code_oauth {
        body["metadata"] = json!({ "user_id": cc_user_id(req, account_uuid) });
        body["context_management"] = json!({
            "edits": [{ "type": "clear_thinking_20251015", "keep": "all" }]
        });
        body["diagnostics"] = json!({ "previous_message_id": null });
        if anthropic_supports_fallbacks(&req.model) {
            body["fallbacks"] = json!([{ "model": "claude-opus-4-8" }]);
        }
    }

    if !req.tools.is_empty() {
        let mut defs = tool_defs(&req.tools);
        if claude_code_oauth {
            // 真 CC 给每个 tool 带 eager_input_streaming，CC 兼容流量对齐。
            for d in defs.iter_mut() {
                if let Some(o) = d.as_object_mut() {
                    o.insert("eager_input_streaming".into(), json!(true));
                }
            }
        }
        body["tools"] = json!(defs);
    }

    // ── Prompt cache 标记 ─────────────────────────────────────────────────
    // 贴 2 个断点（Anthropic 上限 4 个）：
    //   1. system 末 block —— ttl 1h + scope global，最稳定前缀，跨会话可共享命中
    //   2. 最后一条 message 尾 block —— ttl 1h，把到本轮为止的历史整段写进缓存，
    //      下一轮 0 成本读出（对齐真 CC 的缓存断点位置）
    apply_cache_control(&mut body);

    Ok(body)
}

/// 打两个缓存断点：system 末 block（ttl 1h + scope global，最稳定前缀，跨会话可共享）
/// 与最后一条 message 的尾 block（ttl 1h）。必须在 `system` / `messages` 都写入 body 后调用。
fn apply_cache_control(body: &mut Value) {
    // system 兼容两种形态：纯字符串 or [content blocks]。cache_control 必须挂在 block 上，
    // 纯字符串先升格成 block。
    if let Some(sys) = body.get_mut("system") {
        let cc = json!({ "type": "ephemeral", "ttl": "1h", "scope": "global" });
        match sys {
            Value::String(s) => {
                let text = std::mem::take(s);
                *sys = json!([{ "type": "text", "text": text, "cache_control": cc }]);
            }
            Value::Array(arr) if !arr.is_empty() => {
                if let Some(obj) = arr.last_mut().and_then(|b| b.as_object_mut()) {
                    obj.insert("cache_control".into(), cc);
                }
            }
            _ => {}
        }
    }

    // 贴到最后一条 message 的尾 block：从 system 到这条为止全部缓存，下一轮直接命中。
    // messages 随会话变，只用 ttl，不用 scope（scope 仅适合稳定前缀）。
    if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        if let Some(target) = msgs.last_mut().and_then(|m| m.as_object_mut()) {
            let cc = json!({ "type": "ephemeral", "ttl": "1h" });
            let content = target.entry("content").or_insert(Value::Null);
            match content {
                Value::String(s) => {
                    let text = std::mem::take(s);
                    *content = json!([{ "type": "text", "text": text, "cache_control": cc }]);
                }
                Value::Array(arr) if !arr.is_empty() => {
                    if let Some(block) = arr.last_mut().and_then(|b| b.as_object_mut()) {
                        block.insert("cache_control".into(), cc);
                    }
                }
                _ => {}
            }
        }
    }
}

fn build_system(user_system: Option<&str>, claude_code_oauth: bool) -> Value {
    let user_system = user_system.map(str::trim).filter(|s| !s.is_empty());

    if !claude_code_oauth {
        return user_system.map(|s| json!(s)).unwrap_or(Value::Null);
    }

    // CC 兼容：banner block + harness 正文 block。banner 在前（让服务端识别为合法 CLI 流量），
    // 正文（base_system.md，中性身份开头）对应真 CC 的 system 结构。
    let banner = json!({ "type": "text", "text": CLAUDE_CODE_BANNER });
    match user_system {
        Some(s) => json!([banner, { "type": "text", "text": s }]),
        None => json!([banner]),
    }
}

/// CC 兼容 `metadata.user_id`：CC 客户端把它发成一个 JSON-string。
/// device_id 机器级稳定、account_uuid 取 OAuth 账号、session_id 由首条消息派生
/// （同会话稳定、跨会话不同），贴合真 CC 同会话 session_id 不变的特征。
fn cc_user_id(req: &ModelRequest, account_uuid: Option<&str>) -> String {
    serde_json::to_string(&json!({
        "device_id": machine_device_id(),
        "account_uuid": account_uuid.unwrap_or(""),
        "session_id": stable_session_id(&req.entries),
    }))
    .unwrap_or_default()
}

/// 机器级稳定的 64-hex device 指纹，按 $HOME 派生（同机稳定、跨机不同）。
fn machine_device_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let seed = std::env::var("HOME").unwrap_or_default();
    let mut out = String::with_capacity(64);
    for i in 0u8..4 {
        let mut h = DefaultHasher::new();
        (i, "hebbian-device", seed.as_str()).hash(&mut h);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// 由首条 transcript 条目派生的稳定 session id（v4 形态）。首条在会话内恒定，
/// 故同会话所有请求得到同一个 id；跨会话首条不同则 id 不同。
fn stable_session_id(entries: &[TranscriptEntry]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let seed = entries
        .first()
        .map(|e| format!("{e:?}"))
        .unwrap_or_default();
    let mut bytes = [0u8; 16];
    for (chunk, salt) in [(0usize, "a"), (8usize, "b")] {
        let mut h = DefaultHasher::new();
        (salt, seed.as_str()).hash(&mut h);
        bytes[chunk..chunk + 8].copy_from_slice(&h.finish().to_be_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // uuid v4 version
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn entry_to_message(entry: &TranscriptEntry, inject_deepseek_thinking: bool) -> Option<Value> {
    match entry {
        TranscriptEntry::User(user) => Some(json!({"role": "user", "content": user_content(user)})),
        TranscriptEntry::Assistant(AssistantEntry {
            text,
            reasoning,
            reasoning_signature,
            tool_calls,
        }) => {
            if tool_calls.is_empty() {
                Some(json!({"role": "assistant", "content": text}))
            } else {
                let mut content: Vec<Value> = Vec::new();
                if inject_deepseek_thinking && !reasoning.trim().is_empty() {
                    // DeepSeek v4 Anthropic 端点要求 tool_use 多轮里带回上一轮 thinking（无 signature）。
                    content.push(json!({
                        "type": "thinking",
                        "thinking": reasoning,
                    }));
                } else if !reasoning.trim().is_empty() && !reasoning_signature.is_empty() {
                    // Anthropic 原生路径：thinking block 必须带上 API 颁发的 signature，否则 400。
                    // signature 为空时不回填（比发空 signature 更安全；模型已从 text 段看到结论）。
                    content.push(json!({
                        "type": "thinking",
                        "thinking": reasoning,
                        "signature": reasoning_signature,
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
                         call_id,
                         content,
                         attachments,
                         ..
                     }| {
                        // 无附件：content 是纯字符串（现状）。带图片附件：tool_result.content
                        // 用块数组（Anthropic 原生支持 text + image 块），文本占位 + image 块。
                        let inner = if attachments.is_empty() {
                            json!(content)
                        } else {
                            let mut blocks = vec![json!({"type": "text", "text": content})];
                            blocks.extend(attachments.iter().filter_map(image_block));
                            Value::Array(blocks)
                        };
                        json!({
                            "type": "tool_result",
                            "tool_use_id": call_id,
                            "content": inner
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

/// 把图片附件编码成 Anthropic image 块；非图片附件返回 `None`。
fn image_block(attachment: &MessageAttachment) -> Option<Value> {
    match attachment {
        MessageAttachment::Image {
            media_type, data, ..
        } => Some(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data
            }
        })),
        MessageAttachment::TextFile { .. } => None,
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
            MessageAttachment::TextFile { .. } => {
                if let Some(text) = attachment.as_text_block() {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            MessageAttachment::Image { .. } => {
                if let Some(block) = image_block(attachment) {
                    content.push(block);
                }
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
    let mut reasoning_signature = String::new();
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
                    if let Some(sig) = block["signature"].as_str() {
                        reasoning_signature = sig.to_string();
                    }
                }
                Some("tool_use") => {
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    let name = block["name"].as_str().unwrap_or("").to_string();
                    tracing::info!(
                        block_index = calls.len(),
                        tool_id = %id,
                        tool_name = %name,
                        "parse_response: tool_use block"
                    );
                    calls.push(ToolCall {
                        id,
                        name,
                        input: block["input"].clone(),
                    });
                }
                _ => {}
            }
        }
    }

    if stop_reason == "tool_use" {
        ModelResponse::ToolCalls {
            text,
            reasoning,
            reasoning_signature,
            calls,
            attachments: Vec::new(),
            usage,
        }
    } else {
        ModelResponse::Done {
            text,
            reasoning,
            reasoning_signature,
            attachments: Vec::new(),
            usage,
            finish: map_anthropic_finish(stop_reason),
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
    /// thinking block 的签名，`signature_delta` 帧携带，一次性整体到达（不是增量）。
    Signature {
        index: usize,
        signature: String,
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
                Some("signature_delta") => {
                    v["delta"]["signature"]
                        .as_str()
                        .map(|s| AnthropicStreamEvent::Signature {
                            index,
                            signature: s.to_string(),
                        })
                }
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
    fn tool_result_image_encoded_as_image_block() {
        let entry = TranscriptEntry::ToolResults(vec![ToolResult {
            call_id: "call_1".into(),
            name: "Read".into(),
            content: "已读取图片 a.png".into(),
            artifact: None,
            attachments: vec![MessageAttachment::Image {
                name: "a.png".into(),
                media_type: "image/png".into(),
                data: "BASE64DATA".into(),
            }],
        }]);
        let msg = entry_to_message(&entry, false).unwrap();
        let blocks = msg["content"][0]["content"].as_array().unwrap();
        // tool_result.content 是块数组：text 占位 + image 块。
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], "BASE64DATA");
    }

    #[test]
    fn tool_result_without_attachment_stays_plain_string() {
        let entry = TranscriptEntry::ToolResults(vec![ToolResult {
            call_id: "call_1".into(),
            name: "Bash".into(),
            content: "a.txt".into(),
            artifact: None,
            attachments: Vec::new(),
        }]);
        let msg = entry_to_message(&entry, false).unwrap();
        // 无附件：content 仍是纯字符串，不退化成块数组。
        assert_eq!(msg["content"][0]["content"], "a.txt");
    }

    #[test]
    fn anthropic_finish_maps_all_variants() {
        assert_eq!(map_anthropic_finish("end_turn"), FinishReason::Stop);
        assert_eq!(map_anthropic_finish("stop_sequence"), FinishReason::Stop);
        assert_eq!(map_anthropic_finish("max_tokens"), FinishReason::Length);
        assert_eq!(map_anthropic_finish("refusal"), FinishReason::Refusal);
        assert_eq!(
            map_anthropic_finish("pause_turn"),
            FinishReason::Other("pause_turn".to_string())
        );
    }

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

        let body = build_body(&req, false, false, None).unwrap();
        let content = body["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0], json!({"type": "text", "text": "what changed?"}));
        assert_eq!(
            content[1],
            json!({"type": "text", "text": "<file name=\"diff.txt\" media_type=\"text/plain\">\n+hello\n</file>"})
        );
        // content[2] 是最后一个 block，apply_cache_control 会给它打 cache_control，
        // 故只校验 attachment 本身的结构。
        assert_eq!(content[2]["type"], "image");
        assert_eq!(
            content[2]["source"],
            json!({
                "type": "base64",
                "media_type": "image/webp",
                "data": "webpbytes"
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

        let body = build_body(&req, false, true, None).unwrap();
        let system = body["system"].as_array().expect("system must be an array");

        // CC 兼容：banner block + 用户 system 正文 block（无 billing block）。
        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["text"], CLAUDE_CODE_BANNER);
        assert_eq!(system[1]["text"], "Be terse.");
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

        let body = build_body(&req, false, true, None).unwrap();
        let system = body["system"].as_array().expect("system must be an array");

        // 无用户 system 时只发 banner block。
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

        let body = build_body(&req, false, false, None).unwrap();
        // apply_cache_control 会把字符串 system 升格为带 cache_control 的 block 数组。
        let arr = body["system"]
            .as_array()
            .expect("system 已升格为 block 数组");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["text"], "Be terse.");
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
    }

    use crate::types::ToolCall;
    use common::ReasoningEffort;

    /// Sub2API / kiro 等第三方网关把 Anthropic 模型版本号写成 `claude-opus-4.7` 形式
    /// （dot 而非 dash）。`anthropic_thinking_mode` 必须能识别 dot 变体，让 build_body
    /// 走 Opus47Adaptive 分支（区别于 LegacyEnabled：thinking 块多了 `display:"summarized"`，
    /// 这是 4.7 在 stream 下能发 thinking_delta 的关键），否则错落到 LegacyEnabled 没 display 字段
    /// 4.7 stream 不发 thinking_delta。
    #[test]
    fn dot_versioned_opus_4_7_walks_opus47_branch() {
        let req = ModelRequest {
            model: "claude-opus-4.7".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(common::ReasoningConfig {
                enabled: Some(true),
                effort: Some(ReasoningEffort::Extra),
                long_context: None,
            }),
        };
        let body = build_body(&req, false, false, None).unwrap();
        assert_eq!(
            body["thinking"]["display"], "summarized",
            "dot variant of opus-4-7 must walk Opus47Adaptive branch (has display:summarized), body={body}"
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body["thinking"]["budget_tokens"].is_number());
    }

    /// Legacy（如 sonnet-4.5）不能错走 Opus47 —— 区别就是没有 `display` 字段。
    #[test]
    fn dot_versioned_sonnet_4_5_stays_legacy_branch() {
        let req = ModelRequest {
            model: "claude-sonnet-4.5".into(),
            system: None,
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![],
            max_tokens: 8192,
            reasoning: Some(common::ReasoningConfig {
                enabled: Some(true),
                effort: Some(ReasoningEffort::High),
                long_context: None,
            }),
        };
        let body = build_body(&req, false, false, None).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body["thinking"]["budget_tokens"].is_number());
        // LegacyEnabled 不该带 display
        assert!(
            body["thinking"].get("display").is_none(),
            "sonnet-4.5 must NOT have thinking.display (that's Opus47 marker), body={body}"
        );
    }

    /// Claude Code 兼容 / OAuth 模式：output_config.effort 必须跟随用户思考强度
    /// 并按模型量程取值，不能写死 high。回归历史 bug：sub2api 等 CC 兼容 provider
    /// 上「思考强度」选择完全失效（永远 high）。
    #[test]
    fn cc_compat_effort_follows_user_and_model_scale() {
        let build = |model: &str, effort: Option<ReasoningEffort>| {
            let req = ModelRequest {
                model: model.into(),
                system: None,
                entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
                tools: vec![],
                max_tokens: 8192,
                reasoning: effort.map(|e| common::ReasoningConfig {
                    enabled: Some(true),
                    effort: Some(e),
                    long_context: None,
                }),
            };
            build_body(&req, false, true, None).unwrap()
        };

        // 4.8：Extra → xhigh，Low → low
        assert_eq!(
            build("claude-opus-4-8", Some(ReasoningEffort::Extra))["output_config"]["effort"],
            "xhigh"
        );
        assert_eq!(
            build("claude-opus-4-8", Some(ReasoningEffort::Low))["output_config"]["effort"],
            "low"
        );
        // 4.6 / sonnet-4.6 支持 max 但**不**支持 xhigh（对齐 CC 量程）：
        // Extra(xhigh) 钳到 high、Max 保留 max。这是本次 opus-4-6 xhigh 400 的回归点。
        assert_eq!(
            build("claude-opus-4-6", Some(ReasoningEffort::Extra))["output_config"]["effort"],
            "high"
        );
        assert_eq!(
            build("claude-opus-4-6", Some(ReasoningEffort::Max))["output_config"]["effort"],
            "max"
        );
        assert_eq!(
            build("claude-sonnet-4-6", Some(ReasoningEffort::Extra))["output_config"]["effort"],
            "high"
        );
        // reasoning 未设时用默认 Extra → 4.8 走 xhigh
        assert_eq!(
            build("claude-opus-4-8", None)["output_config"]["effort"],
            "xhigh"
        );
        // thinking 始终 adaptive
        assert_eq!(
            build("claude-opus-4-8", Some(ReasoningEffort::Medium))["thinking"]["type"],
            "adaptive"
        );

        // CC 兼容的 adaptive thinking 一律不带 display（与 c.json 真 CC 实测一致）：
        // 直连默认 display=omitted，sub2api 等第三方代理不接受 display 字段，统一不发。
        for m in ["claude-opus-4-8", "claude-opus-4-7", "claude-opus-4-6"] {
            let b = build(m, None);
            assert!(
                b["thinking"].get("display").is_none(),
                "{m} 不该带 display: {b}"
            );
        }
    }

    /// CC 兼容 body 形态对齐真 CC（c.json 2.1.170 实测）的回归测试：
    /// banner+harness 双 block 且无 billing block、cache ttl/scope、fallbacks/diagnostics、
    /// metadata 稳定 session_id+account、tools eager。任一项回退都会被这里拍醒。
    /// 用 fable-5：c.json 的真实模型，也是唯一会发 fallbacks 的家族。
    #[test]
    fn cc_compat_body_matches_real_cc_shape() {
        let req = ModelRequest {
            model: "claude-fable-5".into(),
            system: Some("Be terse.".into()),
            entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
            tools: vec![ToolDefinition {
                name: "Read".into(),
                description: "read a file".into(),
                parameters: json!({ "type": "object" }),
            }],
            max_tokens: 8192,
            reasoning: None,
        };
        let body = build_body(&req, false, true, Some("acct-123")).unwrap();

        // system：[banner, 用户正文]，绝不含 billing header block。
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["text"], CLAUDE_CODE_BANNER);
        assert_eq!(system[1]["text"], "Be terse.");
        assert!(
            !system.iter().any(|b| b["text"]
                .as_str()
                .unwrap_or("")
                .contains("x-anthropic-billing-header")),
            "CC 兼容不应发 billing header block: {body}"
        );
        // system 末 block：ttl 1h + scope global。
        assert_eq!(
            system[1]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h", "scope": "global" })
        );

        // diagnostics 所有模型都发；fallbacks 仅 Fable 系列发，target = 默认 opus。
        assert_eq!(body["diagnostics"], json!({ "previous_message_id": null }));
        assert_eq!(body["fallbacks"], json!([{ "model": "claude-opus-4-8" }]));

        // metadata.user_id 是 JSON-string，含 account + 36 字符 session_id + 非空 device_id。
        let uid = body["metadata"]["user_id"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(uid).unwrap();
        assert_eq!(parsed["account_uuid"], "acct-123");
        assert_eq!(parsed["session_id"].as_str().unwrap().len(), 36);
        assert!(!parsed["device_id"].as_str().unwrap().is_empty());

        // tools 带 eager_input_streaming。
        assert_eq!(body["tools"][0]["eager_input_streaming"], true);

        // 最后一条 message 尾 block：ttl 1h，无 scope。
        let last_block = body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(
            last_block["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );

        // session_id 同会话稳定：同 entries 再 build 得到同一个 id。
        let body2 = build_body(&req, false, true, Some("acct-123")).unwrap();
        let uid2 = body2["metadata"]["user_id"].as_str().unwrap();
        let parsed2: Value = serde_json::from_str(uid2).unwrap();
        assert_eq!(parsed["session_id"], parsed2["session_id"]);
    }

    /// 非 Fable 模型不发 fallbacks（会 400 "does not support the fallbacks parameter"），
    /// 但 diagnostics 仍然发（所有模型通用）。这是本次 opus-4-8 fallbacks 400 的回归点。
    #[test]
    fn cc_compat_omits_fallbacks_for_non_fable_models() {
        for model in ["claude-opus-4-8", "claude-opus-4-6", "claude-sonnet-4-6"] {
            let req = ModelRequest {
                model: model.into(),
                system: Some("Be terse.".into()),
                entries: vec![TranscriptEntry::User(UserEntry::text("hi"))],
                tools: vec![],
                max_tokens: 8192,
                reasoning: None,
            };
            let body = build_body(&req, false, true, Some("acct-123")).unwrap();
            assert!(
                body.get("fallbacks").is_none(),
                "{model} 不该发 fallbacks: {body}"
            );
            assert_eq!(
                body["diagnostics"],
                json!({ "previous_message_id": null }),
                "{model} 仍应发 diagnostics"
            );
        }
    }

    // ── DeepSeek v4 on Anthropic 端点的方言测试 ──────────────────────────────

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
                reasoning_signature: String::new(),
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
                attachments: Vec::new(),
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
            None,
        )
        .unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["output_config"]["effort"], "max");
        assert_eq!(body["max_tokens"], 131_072);
    }

    /// reasoning=None 时，DeepSeek v4 Anthropic 端点应当走「模型默认 = ON」。
    /// 历史 bug：把 None 视为显式关，导致 heb CLI 默认会话拿不到 thinking。
    /// None 时 effort fallback 是 "high"（保守档），要 max 需 Some({effort: Extra,...})。
    #[test]
    fn deepseek_v4_anthropic_with_none_reasoning_defaults_to_thinking_on() {
        let body = build_body(
            &req_for_deepseek_anthropic(None, "", false, 8192),
            false,
            false,
            None,
        )
        .unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["output_config"]["effort"], "high");
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
            None,
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
        let err = build_body(&req, false, false, None).unwrap_err();
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
        let body = build_body(&req, false, false, None).unwrap();
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
        let body = build_body(&req, false, false, None).unwrap();
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
        let body = build_body(&req, false, false, None).unwrap();
        // claude-sonnet-4-5 走 LegacyEnabled：thinking.type=enabled + budget_tokens
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body["thinking"]["budget_tokens"].is_number());
        // 不应注入 DeepSeek 的 output_config
        assert!(body.get("output_config").is_none());
    }
}
