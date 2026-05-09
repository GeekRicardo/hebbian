//! DeepSeek `chat.deepseek.com` web 协议适配。
//!
//! 对齐 ds2api `internal/promptcompat/standard_request.go` + `internal/sse/parser.go`：
//!
//! - 入：把 `Transcript` 折叠成单段 `prompt`（带 `<system>` / 多轮对话头），
//!   按需注入 tool 提示，组成 `chat/completion` 请求 body。
//! - 出：解析路径式 JSON SSE，把 `response/fragments/-N/content` /
//!   `response/thinking_content` / `response/content` 拆成 Text/Thinking 段，
//!   `response/status = FINISHED` 视为完成；最终把模型输出文本里
//!   `<tool_calls>` / `<|DSML|tool_calls>` 块再解析回 tool_calls。
//!
//! tool_call 格式取 ds2api 的「兼容形态」：
//! ```text
//! <tool_calls>
//!   <invoke name="ToolName">
//!     <parameter name="arg1"><![CDATA[value]]></parameter>
//!   </invoke>
//! </tool_calls>
//! ```
//! `<|DSML|...>` 也认（runtime 端按 ds2api 设计兼容这两套）。

use serde_json::{json, Value};

use crate::types::{
    AssistantEntry, ModelRequest, ToolCall, ToolDefinition, ToolResult, TranscriptEntry, UserEntry,
};

// ── 请求构建 ───────────────────────────────────────────────────────────────

/// 把 model 名映射到 chat.deepseek.com 的 model_type 字段。
pub fn model_type_for(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.contains("vision") {
        "vision"
    } else if m.contains("pro") {
        "expert"
    } else {
        "default"
    }
}

/// 模型名是否带 `-search` 后缀（启用联网搜索）。
pub fn search_enabled_for(model: &str) -> bool {
    model.to_lowercase().contains("search")
}

/// 模型名是否带 `-nothinking` 后缀（关闭思维链）。
pub fn thinking_enabled_for(model: &str) -> bool {
    !model.to_lowercase().contains("nothinking")
}

/// 把 transcript 折成单 prompt。规则：
/// 1) 系统消息独立放在最前并以「### System」起头；
/// 2) 多轮 user / assistant 用「### User」「### Assistant」分隔；
/// 3) tool_results 跟在最近一条 assistant 后面，用 `<tool_result>` 包裹；
/// 4) 末尾留一个空 Assistant 头让模型续写。
pub fn build_prompt(req: &ModelRequest, tools: &[ToolDefinition]) -> String {
    let mut out = String::new();

    let mut system_text = req
        .system
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    if !tools.is_empty() {
        let tool_prompt = build_tool_prompt(tools);
        if system_text.is_empty() {
            system_text = tool_prompt;
        } else {
            system_text.push_str("\n\n");
            system_text.push_str(&tool_prompt);
        }
    }
    if !system_text.is_empty() {
        out.push_str("### System\n");
        out.push_str(&system_text);
        out.push_str("\n\n");
    }

    for entry in &req.entries {
        match entry {
            TranscriptEntry::User(UserEntry { text, attachments }) => {
                out.push_str("### User\n");
                out.push_str(text.trim());
                for att in attachments {
                    if let Some(block) = att.as_text_block() {
                        out.push('\n');
                        out.push_str(&block);
                    }
                }
                out.push_str("\n\n");
            }
            TranscriptEntry::Assistant(AssistantEntry {
                text,
                reasoning,
                tool_calls,
            }) => {
                out.push_str("### Assistant\n");
                if !reasoning.is_empty() {
                    // 用 <think>...</think> 把上一轮思维链回喂给模型，
                    // 避免在工具调用多轮场景里丢失推理链。这是 DeepSeek-tui /
                    // ds2api 默认的 thinking 续传形态。
                    out.push_str("<think>\n");
                    out.push_str(reasoning);
                    if !reasoning.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("</think>\n");
                }
                out.push_str(text);
                if !tool_calls.is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&render_tool_calls_xml(tool_calls));
                }
                out.push_str("\n\n");
            }
            TranscriptEntry::ToolResults(results) => {
                // chat.deepseek.com 模型只在 ### User / Assistant / System
                // 这三种角色头上训练过；曾经用过的 ### Tool Result 是新角色头，
                // 模型会把它当成"脚本继续模拟"——开始伪造 ### Tool Result /
                // ### Assistant / ### Tool Output 等头部续写整段历史，导致后续
                // tool_call 永远生成不到 <tool_calls> wrapper 里。
                // 把 tool_result 全部包进一段 ### User 里规避。
                out.push_str("### User\n");
                for ToolResult {
                    call_id,
                    name,
                    content,
                } in results
                {
                    out.push_str(&format!(
                        "<tool_result tool=\"{}\" call_id=\"{}\">\n",
                        xml_escape_attr(name),
                        xml_escape_attr(call_id),
                    ));
                    out.push_str(content);
                    out.push_str("\n</tool_result>\n");
                }
                out.push('\n');
            }
        }
    }

    out.push_str("### Assistant\n");
    out
}

fn build_tool_prompt(tools: &[ToolDefinition]) -> String {
    let mut s = String::from("You have access to these tools:\n\n");
    for t in tools {
        let params = serde_json::to_string(&t.parameters).unwrap_or_else(|_| "{}".to_string());
        s.push_str(&format!(
            "Tool: {}\nDescription: {}\nParameters: {}\n\n",
            t.name, t.description, params
        ));
    }
    s.push_str(TOOL_CALL_FORMAT);
    s
}

const TOOL_CALL_FORMAT: &str = r#"TOOL CALL FORMAT — FOLLOW EXACTLY:

<tool_calls>
  <invoke name="TOOL_NAME">
    <parameter name="ARG_NAME"><![CDATA[VALUE]]></parameter>
  </invoke>
</tool_calls>

RULES:
1) Wrap all tool calls in a single <tool_calls>...</tool_calls> block.
2) Each <invoke name="..."> contains one or more <parameter name="...">...</parameter>.
3) String values MUST use <![CDATA[...]]>. Numbers / booleans / null may be plain text.
4) Object values use nested XML elements; arrays repeat <item>.
5) Do NOT wrap the block in markdown fences. Do NOT add explanations after the block.
6) If you call any tool, the first non-whitespace characters of that section must be exactly <tool_calls>.
"#;

fn render_tool_calls_xml(calls: &[ToolCall]) -> String {
    let mut s = String::from("<tool_calls>\n");
    for call in calls {
        s.push_str(&format!(
            "  <invoke name=\"{}\">\n",
            xml_escape_attr(&call.name)
        ));
        if let Some(obj) = call.input.as_object() {
            for (k, v) in obj {
                s.push_str(&render_param(k, v, "    "));
            }
        }
        s.push_str("  </invoke>\n");
    }
    s.push_str("</tool_calls>");
    s
}

fn render_param(name: &str, value: &Value, indent: &str) -> String {
    let attr = xml_escape_attr(name);
    let inner = match value {
        Value::String(s) => cdata(s),
        Value::Null => String::from("null"),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| format!("<item>{}</item>", value_to_inner_xml(item)))
            .collect::<String>(),
        Value::Object(obj) => obj
            .iter()
            .map(|(k, v)| {
                format!(
                    "<{name}>{value}</{name}>",
                    name = xml_escape_attr(k),
                    value = value_to_inner_xml(v)
                )
            })
            .collect(),
    };
    format!("{indent}<parameter name=\"{attr}\">{inner}</parameter>\n")
}

fn value_to_inner_xml(v: &Value) -> String {
    match v {
        Value::String(s) => cdata(s),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| format!("<item>{}</item>", value_to_inner_xml(item)))
            .collect(),
        Value::Object(obj) => obj
            .iter()
            .map(|(k, v)| {
                format!(
                    "<{k}>{v}</{k}>",
                    k = xml_escape_attr(k),
                    v = value_to_inner_xml(v)
                )
            })
            .collect(),
    }
}

fn cdata(s: &str) -> String {
    if s.contains("]]>") {
        format!("<![CDATA[{}]]>", s.replace("]]>", "]]]]><![CDATA[>"))
    } else {
        format!("<![CDATA[{s}]]>")
    }
}

fn xml_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 完整 chat/completion 请求 body。
pub fn build_completion_body(
    chat_session_id: &str,
    parent_message_id: Option<&str>,
    prompt: &str,
    model: &str,
) -> Value {
    let empty_refs: Vec<Value> = Vec::new();
    json!({
        "chat_session_id": chat_session_id,
        "parent_message_id": parent_message_id,
        "model_type": model_type_for(model),
        "prompt": prompt,
        "ref_file_ids": empty_refs,
        "thinking_enabled": thinking_enabled_for(model),
        "search_enabled": search_enabled_for(model),
    })
}

// ── SSE 解析 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepseekChunkPart {
    Text(String),
    Thinking(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeepseekChunkResult {
    pub parts: Vec<DeepseekChunkPart>,
    pub finished: bool,
}

const SKIP_CONTAINS: &[&str] = &[
    "quasi_status",
    "elapsed_secs",
    "token_usage",
    "pending_fragment",
    "conversation_mode",
    "fragments/-1/status",
    "fragments/-2/status",
    "fragments/-3/status",
];
const SKIP_EXACT: &[&str] = &["response/search_status"];

fn should_skip_path(path: &str) -> bool {
    if SKIP_EXACT.iter().any(|p| *p == path) {
        return true;
    }
    if SKIP_CONTAINS.iter().any(|p| path.contains(p)) {
        return true;
    }
    false
}

/// 跨 chunk 的解析状态。DeepSeek 用「先来 `response/fragments` 写明
/// 下一段是 THINK 还是 RESPONSE，再用 `response/fragments/-1/content`
/// 持续 APPEND」的玩法，所以 content 增量必须查这里得到的 fragment 类型。
///
/// 同时实现了 sticky path：抓包发现 DeepSeek 在同一条 path 上连续 APPEND 时
/// 会省略 `p` 字段，只发 `{"v":"..."}`。这种 chunk 必须沿用上一条的 path 解析。
#[derive(Debug, Default, Clone)]
pub struct DeepseekStreamState {
    /// 当前最后一条 fragment 的类型；None = 尚未声明（按 text 走）。
    pub current_kind: Option<Kind>,
    /// 检测到 `</think>` 后强制把后续 thinking 段切成 text（绕开 DeepSeek 上游 bug）。
    pub thinking_done: bool,
    /// 上一条 chunk 的 `p` 字段；用于「sticky path」省略写法。
    pub last_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Thinking,
}

/// 解析单条 `data: {...}` 行。需要传入跨调用的状态。
pub fn parse_sse_line(
    data: &str,
    thinking_enabled: bool,
    state: &mut DeepseekStreamState,
) -> Option<DeepseekChunkResult> {
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return None;
    }
    let v: Value = serde_json::from_str(trimmed).ok()?;
    Some(parse_chunk(&v, thinking_enabled, state))
}

fn parse_chunk(
    chunk: &Value,
    thinking_enabled: bool,
    state: &mut DeepseekStreamState,
) -> DeepseekChunkResult {
    let mut out = DeepseekChunkResult::default();
    // sticky path：path 字段缺失时沿用上一条 chunk 的 path
    let raw_path = chunk.get("p").and_then(Value::as_str);
    let path: &str = match raw_path {
        Some(p) => {
            // 不更新 state.last_path 这里，等真正消费完再更新（见末尾）
            p
        }
        None => &state.last_path,
    };

    // status FINISHED
    if path == "response/status" || path == "status" {
        if let Some(s) = chunk["v"].as_str() {
            if s.trim().eq_ignore_ascii_case("FINISHED") {
                out.finished = true;
                return out;
            }
        }
    }
    if should_skip_path(path) {
        return out;
    }

    let v = match chunk.get("v") {
        Some(v) => v,
        None => return out,
    };

    // response/fragments APPEND/SET -> [{type, content}, ...]：声明下一段类型。
    if path == "response/fragments" {
        if let Some(items) = v.as_array() {
            for frag in items {
                let typ = frag
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_uppercase();
                let content = frag.get("content").and_then(Value::as_str).unwrap_or("");
                let kind = classify_fragment_type(&typ, state);
                state.current_kind = Some(kind);
                if !content.is_empty() {
                    push_with_split(state, &mut out.parts, kind, content);
                }
            }
        }
        return finalize(out, state, thinking_enabled);
    }

    // response/thinking_content / response/content：直接定型并落字。
    let path_kind = explicit_path_kind(path);

    match v {
        Value::String(s) => {
            if s.is_empty() || s == "FINISHED" {
                return finalize(out, state, thinking_enabled);
            }
            let kind = path_kind
                .or(state.current_kind)
                .unwrap_or(Kind::Text);
            // 显式路径会更新 state.current_kind
            if path_kind.is_some() {
                state.current_kind = path_kind;
            }
            push_with_split(state, &mut out.parts, kind, s);
        }
        Value::Array(items) => {
            // 嵌套 BATCH：v 是数组，每项有自己的 p/o/v
            for item in items {
                let inner_path = item["p"].as_str().unwrap_or("");
                if inner_path == "response/status" || inner_path == "status" {
                    if let Some(s) = item["v"].as_str() {
                        if s.trim().eq_ignore_ascii_case("FINISHED") {
                            out.finished = true;
                            return finalize(out, state, thinking_enabled);
                        }
                    }
                    continue;
                }
                if should_skip_path(inner_path) {
                    continue;
                }
                if inner_path == "response/fragments" {
                    if let Some(frags) = item.get("v").and_then(Value::as_array) {
                        for frag in frags {
                            let typ = frag
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_uppercase();
                            let content = frag.get("content").and_then(Value::as_str).unwrap_or("");
                            let kind = classify_fragment_type(&typ, state);
                            state.current_kind = Some(kind);
                            if !content.is_empty() {
                                push_with_split(state, &mut out.parts, kind, content);
                            }
                        }
                    }
                    continue;
                }
                let inner_kind = explicit_path_kind(inner_path)
                    .or(state.current_kind)
                    .unwrap_or(Kind::Text);
                if let Some(explicit) = explicit_path_kind(inner_path) {
                    state.current_kind = Some(explicit);
                }
                if let Some(s) = item["v"].as_str() {
                    if !s.is_empty() && s != "FINISHED" {
                        push_with_split(state, &mut out.parts, inner_kind, s);
                    }
                } else if let Some(t) = item.get("type").and_then(Value::as_str) {
                    if let Some(c) = item.get("content").and_then(Value::as_str) {
                        if !c.is_empty() {
                            let kind = classify_fragment_type(&t.to_uppercase(), state);
                            state.current_kind = Some(kind);
                            push_with_split(state, &mut out.parts, kind, c);
                        }
                    }
                }
            }
        }
        Value::Object(obj) => {
            // 形态 1：初始整 response 对象 `{"v":{"response":{...,"fragments":[...]}}}`。
            // 形态 2：直接的 `{"text":...}` / `{"content":...}` 简单值对象。
            // 形态 3：fragment dict `{"type":"THINK","content":"..."}`。
            if let Some(resp) = obj.get("response").and_then(Value::as_object) {
                consume_response_fragments(resp, state, &mut out.parts);
            } else if obj.contains_key("fragments") {
                consume_response_fragments(obj, state, &mut out.parts);
            } else if let Some(typ) = obj.get("type").and_then(Value::as_str) {
                let kind = classify_fragment_type(&typ.to_uppercase(), state);
                state.current_kind = Some(kind);
                if let Some(c) = obj.get("content").and_then(Value::as_str) {
                    if !c.is_empty() {
                        push_with_split(state, &mut out.parts, kind, c);
                    }
                }
            } else if let Some(text) = obj.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    let kind = path_kind.or(state.current_kind).unwrap_or(Kind::Text);
                    push_with_split(state, &mut out.parts, kind, text);
                }
            } else if let Some(text) = obj.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    let kind = path_kind.or(state.current_kind).unwrap_or(Kind::Text);
                    push_with_split(state, &mut out.parts, kind, text);
                }
            }
        }
        _ => {}
    }

    // 更新 sticky path（仅当本 chunk 显式带了 p）
    if let Some(p) = raw_path {
        state.last_path = p.to_string();
    }

    finalize(out, state, thinking_enabled)
}

/// 处理整个 response 对象里的 fragments 数组：识别每个 fragment 的 type，
/// 写入 state 并把 content 推进 parts。
fn consume_response_fragments(
    response_obj: &serde_json::Map<String, Value>,
    state: &mut DeepseekStreamState,
    parts: &mut Vec<DeepseekChunkPart>,
) {
    let Some(frags) = response_obj.get("fragments").and_then(Value::as_array) else {
        return;
    };
    for frag in frags {
        let typ = frag
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase();
        let content = frag.get("content").and_then(Value::as_str).unwrap_or("");
        let kind = classify_fragment_type(&typ, state);
        state.current_kind = Some(kind);
        if !content.is_empty() {
            push_with_split(state, parts, kind, content);
        }
    }
}

/// DeepSeek fragment type → Kind 分类。
///
/// 已知正文类型只有 `RESPONSE`；其余（`THINK` / `THINKING` / `READ_LINK` /
/// 未来新增的元阶段）一律折进 thinking 通道，避免内部工作内容污染正文。
/// 空字符串保留 `state.current_kind`（应对 sticky path 的延续 chunk）。
fn classify_fragment_type(typ: &str, state: &DeepseekStreamState) -> Kind {
    match typ {
        "RESPONSE" => Kind::Text,
        "" => state.current_kind.unwrap_or(Kind::Text),
        _ => Kind::Thinking,
    }
}

fn finalize(
    mut out: DeepseekChunkResult,
    _state: &DeepseekStreamState,
    thinking_enabled: bool,
) -> DeepseekChunkResult {
    if !thinking_enabled {
        out.parts.retain(|p| !matches!(p, DeepseekChunkPart::Thinking(_)));
    }
    out
}

fn explicit_path_kind(path: &str) -> Option<Kind> {
    match path {
        "response/thinking_content" | "response/reasoning_content" => Some(Kind::Thinking),
        "response/content" => Some(Kind::Text),
        // response/fragments/-N/content：用 state.current_kind
        other => {
            // 兜底：路径里含 "thinking" / "reasoning" 当 thinking
            let lower = other.to_lowercase();
            if lower.contains("thinking") || lower.contains("reasoning") {
                Some(Kind::Thinking)
            } else {
                None
            }
        }
    }
}

/// 把一段文本按当前类型推入。两种额外的 split：
///
/// 1. 如果是 thinking 且里面包含 `</think>`：之前算 thinking、之后翻成 text，
///    并把状态机标记为 thinking_done。
/// 2. 如果是 text 但里面夹了 `<think>...</think>` 块（部分模型把思维链直接
///    inline 进正文里），把内层提成 thinking、外层保留为 text。
fn push_with_split(
    state: &mut DeepseekStreamState,
    parts: &mut Vec<DeepseekChunkPart>,
    kind: Kind,
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    // 已经检测到 </think>，后续到达的 thinking 段一律当 text
    let kind = if state.thinking_done && matches!(kind, Kind::Thinking) {
        Kind::Text
    } else {
        kind
    };

    match kind {
        Kind::Thinking => push_thinking_with_close(state, parts, text),
        Kind::Text => push_text_with_inline_think(state, parts, text),
    }
}

/// thinking 段里若出现 `</think>` —— 前面继续 thinking，后面切回 text。
fn push_thinking_with_close(
    state: &mut DeepseekStreamState,
    parts: &mut Vec<DeepseekChunkPart>,
    text: &str,
) {
    if let Some(idx) = find_close_think(text) {
        let before = &text[..idx.start];
        let after = &text[idx.end..];
        if !before.is_empty() {
            parts.push(DeepseekChunkPart::Thinking(before.to_string()));
        }
        state.thinking_done = true;
        state.current_kind = Some(Kind::Text);
        if !after.is_empty() {
            push_text_with_inline_think(state, parts, after);
        }
    } else {
        parts.push(DeepseekChunkPart::Thinking(text.to_string()));
    }
}

/// 一段 text，可能内嵌了 `<think>...</think>` 块；逐块拆。
/// 也处理半截的 `<think>` 没闭合的情况：进入 thinking 模式，等下一段 chunk。
fn push_text_with_inline_think(
    state: &mut DeepseekStreamState,
    parts: &mut Vec<DeepseekChunkPart>,
    text: &str,
) {
    let lower = text.to_lowercase();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let rel_open = lower[cursor..].find("<think>");
        match rel_open {
            None => {
                // 剩下的全是 text；如果之前进入了 thinking 没闭合，则当前在 thinking
                let rest = &text[cursor..];
                let rest = strip_think_tags(rest);
                if !rest.is_empty() {
                    parts.push(DeepseekChunkPart::Text(rest));
                }
                return;
            }
            Some(rel) => {
                let abs_open = cursor + rel;
                let before = &text[cursor..abs_open];
                let before = strip_think_tags(before);
                if !before.is_empty() {
                    parts.push(DeepseekChunkPart::Text(before));
                }
                let after_open = abs_open + "<think>".len();
                if let Some(close_rel) = lower[after_open..].find("</think>") {
                    let abs_close = after_open + close_rel;
                    let inner = &text[after_open..abs_close];
                    if !inner.is_empty() {
                        parts.push(DeepseekChunkPart::Thinking(inner.to_string()));
                    }
                    state.thinking_done = true;
                    cursor = abs_close + "</think>".len();
                } else {
                    // <think> 但本 chunk 内没有 </think>：把剩余视作 thinking，
                    // state 切到 Thinking 等下一 chunk 接着判断。
                    let inner = &text[after_open..];
                    if !inner.is_empty() {
                        parts.push(DeepseekChunkPart::Thinking(inner.to_string()));
                    }
                    state.current_kind = Some(Kind::Thinking);
                    return;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Range {
    start: usize,
    end: usize,
}

/// 匹配 reasoning 段的闭合标签。模型有时会写成 `</think>` / `</thinking>` /
/// `</thought>` / `</think_block>` 等变体——我们都当成有效闭合，否则状态机
/// 会卡在 thinking，后续的 assistant 正文（含 `<tool_calls>` wrapper）会被错
/// 当成 reasoning 渲染、且永远进不到 tool_call 解析。
///
/// 匹配规则：`</think` 开头（大小写不敏感），后面跟 ≤16 字节的任意内容，
/// 直到第一个 `>`。16 字节足够覆盖常见变体且不会把后文整段吞掉。
fn find_close_think(s: &str) -> Option<Range> {
    const TAG_PREFIX: &str = "</think";
    const MAX_TAIL: usize = 16;
    let lower = s.to_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(TAG_PREFIX) {
        let start = search_from + rel;
        let after = start + TAG_PREFIX.len();
        let tail = &s[after..];
        let limit = tail.len().min(MAX_TAIL);
        if let Some(gt) = tail[..limit].find('>') {
            return Some(Range {
                start,
                end: after + gt + 1,
            });
        }
        search_from = start + TAG_PREFIX.len();
    }
    None
}

fn strip_think_tags(s: &str) -> String {
    let lower = s.to_lowercase();
    if !lower.contains("<think") && !lower.contains("</think") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i..];
            if rest.starts_with("<think>") {
                i += "<think>".len();
                continue;
            }
            if rest.starts_with("</think>") {
                i += "</think>".len();
                continue;
            }
        }
        // 直接拷贝当前字节
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}


// ── 流式 tool_call sieve ──────────────────────────────────────────────────

/// 边流边筛掉 `<tool_calls>` / `<|DSML|tool_calls>` 块，只把剩余正文吐给上层。
///
/// 协议层的 [`extract_tool_calls`] 会在最后从 *full text* 里再解析一次拿到
/// `ToolCall` 列表；这个 sieve 仅决定 UI 上看得到哪些 chunk。
#[derive(Debug, Default)]
pub struct ToolCallSieve {
    pending: String,
    swallowing: bool,
}

const OPEN_MARKERS: &[&str] = &["<tool_calls>", "<|DSML|tool_calls>"];
const CLOSE_MARKERS: &[&str] = &["</tool_calls>", "</|DSML|tool_calls>"];
/// 最长 open marker 长度 + 余量，作为 streaming 时的 hold 窗口。
const HOLD_WINDOW: usize = 24;

/// 模型自爆时常见的伪角色头——遇到任何一个就当本轮结束，截断 stream。
/// 这些是 prompt 模板用过 / 模型容易脑补出来的形态：`### User` / `### Assistant`
/// 是 prompt 的真实角色；`### Tool Result` / `### Tool Output` 是模型在续写
/// "对话脚本"时常见的捏造。
const FAKE_ROLE_HEADERS: &[&str] = &[
    "\n### User",
    "\n### Assistant",
    "\n### System",
    "\n### Tool",
];

/// 在 `text` 里找最早的伪角色头位置，返回应该保留的字节数。
/// 没找到时返回 `None`。
pub(crate) fn find_fake_role_header_cut(text: &str) -> Option<usize> {
    FAKE_ROLE_HEADERS
        .iter()
        .filter_map(|m| text.find(m))
        .min()
}

impl ToolCallSieve {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入新一段 text，返回可以安全发给 surface 的文本。
    pub fn push(&mut self, delta: &str) -> String {
        if delta.is_empty() {
            return String::new();
        }
        self.pending.push_str(delta);
        self.flush_safe()
    }

    /// 流结束时清空 buffer：swallow 中则丢掉、否则原样吐出。
    pub fn finalize(&mut self) -> String {
        if self.swallowing {
            // 没遇到闭合 tag，整段视作 tool_call 内部，丢弃。
            self.pending.clear();
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }

    fn flush_safe(&mut self) -> String {
        let mut out = String::new();
        loop {
            if self.swallowing {
                if let Some((close_pos, close_len)) = find_first_marker(&self.pending, CLOSE_MARKERS)
                {
                    // 把 tool_calls 块整段（含闭合标签）丢掉，保留 close 之后的部分
                    let tail = self.pending[close_pos + close_len..].to_string();
                    self.pending = tail;
                    self.swallowing = false;
                    continue;
                }
                // 闭合还没到：继续吞，pending 里东西都不能吐
                return out;
            }

            if let Some((open_pos, open_len)) = find_first_marker(&self.pending, OPEN_MARKERS) {
                // 把 open 之前的安全部分吐出
                if open_pos > 0 {
                    out.push_str(&self.pending[..open_pos]);
                }
                // 丢掉 open 标签本身，进入 swallow
                self.pending = self.pending[open_pos + open_len..].to_string();
                self.swallowing = true;
                continue;
            }

            // 没找到 open，但 buffer 末尾可能正在拼一个 open（如 `<tool_`、`<|DSM`），
            // 留 HOLD_WINDOW 的尾巴等下一段。
            if self.pending.len() <= HOLD_WINDOW {
                return out;
            }
            // 找一个 char-boundary 安全的 split
            let mut split = self.pending.len() - HOLD_WINDOW;
            while split > 0 && !self.pending.is_char_boundary(split) {
                split -= 1;
            }
            // 但要避免把 `<...` 的开头切走：从 split 往前找最近的 `<`。
            // 如果 `<` 距 split 很近（< HOLD_WINDOW），就把 split 提前到那个 `<`。
            if let Some(lt) = self.pending[..split].rfind('<') {
                if split - lt < HOLD_WINDOW {
                    split = lt;
                }
            }
            if split == 0 {
                return out;
            }
            out.push_str(&self.pending[..split]);
            self.pending = self.pending[split..].to_string();
            return out;
        }
    }
}

fn find_first_marker(haystack: &str, markers: &[&str]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for m in markers {
        if let Some(pos) = haystack.find(m) {
            match best {
                Some((bp, _)) if bp <= pos => {}
                _ => best = Some((pos, m.len())),
            }
        }
    }
    best
}

// ── tool_calls 解析 ───────────────────────────────────────────────────────

/// 从模型最终输出文本中解析所有 `<tool_calls>...</tool_calls>` 或
/// `<|DSML|tool_calls>...</|DSML|tool_calls>` 块。模型可能拼多个 wrapper（甚至
/// 一个 wrapper 多 invoke），逐块扫直到找完。返回（剩余文本，识别到的 tool_calls）。
pub fn extract_tool_calls(full_text: &str) -> (String, Vec<ToolCall>) {
    let mut text = full_text
        .replace("<|DSML|tool_calls>", "<tool_calls>")
        .replace("</|DSML|tool_calls>", "</tool_calls>")
        .replace("<|DSML|invoke", "<invoke")
        .replace("</|DSML|invoke>", "</invoke>")
        .replace("<|DSML|parameter", "<parameter")
        .replace("</|DSML|parameter>", "</parameter>");

    let mut all_calls = Vec::new();
    loop {
        let Some(start) = text.find("<tool_calls>") else {
            break;
        };
        let Some(end_rel) = text[start..].find("</tool_calls>") else {
            // 有 open 没 close：跳出，剩余 text 作为正文
            break;
        };
        let end = start + end_rel + "</tool_calls>".len();
        let block_calls = parse_tool_calls_block(&text[start..end]);
        all_calls.extend(block_calls);
        // 扣掉这一段（包含开闭 wrapper），继续找下一段
        text = format!("{}{}", &text[..start], &text[end..]);
    }

    if all_calls.is_empty() {
        return (full_text.to_string(), Vec::new());
    }
    // 重新分配 call_id 让序号连续（parse_tool_calls_block 的 idx 从每块的 1 开始）
    for (i, call) in all_calls.iter_mut().enumerate() {
        call.id = format!("call_{}", i + 1);
    }
    (text.trim().to_string(), all_calls)
}

fn parse_tool_calls_block(block: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut idx = 0;
    while let Some(invoke_start) = find_after(block, "<invoke", idx) {
        // 找 name="..."
        let after_open = &block[invoke_start..];
        let Some(name_pos) = after_open.find("name=") else {
            break;
        };
        let attr = &after_open[name_pos + "name=".len()..];
        let quote_char = attr.chars().next().unwrap_or('"');
        let attr_after = &attr[1..];
        let Some(quote_end) = attr_after.find(quote_char) else {
            break;
        };
        let tool_name = attr_after[..quote_end].trim().to_string();

        // 找对应的 </invoke>
        let after_name = &attr_after[quote_end + 1..];
        let absolute_after = (invoke_start + name_pos + "name=".len() + 1 + quote_end + 1) as usize;
        let Some(invoke_end_rel) = block[absolute_after..].find("</invoke>") else {
            break;
        };
        let invoke_body_end = absolute_after + invoke_end_rel;
        let body = &block[absolute_after..invoke_body_end];
        let _ = after_name; // silence

        let input = parse_invoke_params(body);
        calls.push(ToolCall {
            id: format!("call_{}", calls.len() + 1),
            name: tool_name,
            input,
        });
        idx = invoke_body_end + "</invoke>".len();
    }
    calls
}

fn find_after(s: &str, needle: &str, from: usize) -> Option<usize> {
    s[from..].find(needle).map(|i| from + i)
}

/// 在 `body[start..]` 内按深度找与已打开标签匹配的闭合位置（绝对 index）。
/// 调用方已经消费了一个 open，所以初始 depth = 1；CDATA 内文本会被跳过，避免误匹配。
fn find_matching_close(
    body: &str,
    start: usize,
    open_tag: &str,
    close_tag: &str,
) -> Option<usize> {
    let mut depth = 1usize;
    let mut i = start;
    while i < body.len() {
        let next_cdata = find_after(body, "<![CDATA[", i);
        let next_open = find_after(body, open_tag, i);
        let next_close = find_after(body, close_tag, i);
        let min = [next_cdata, next_open, next_close]
            .iter()
            .filter_map(|x| *x)
            .min()?;
        if Some(min) == next_cdata {
            let body_start = min + "<![CDATA[".len();
            let end = find_after(body, "]]>", body_start)?;
            i = end + "]]>".len();
        } else if Some(min) == next_open {
            depth += 1;
            i = min + open_tag.len();
        } else {
            depth -= 1;
            if depth == 0 {
                return Some(min);
            }
            i = min + close_tag.len();
        }
    }
    None
}

fn parse_invoke_params(body: &str) -> Value {
    let mut obj = serde_json::Map::new();
    let mut i = 0usize;
    while let Some(p_start) = find_after(body, "<parameter", i) {
        let after = &body[p_start..];
        let Some(name_pos) = after.find("name=") else {
            break;
        };
        let attr = &after[name_pos + "name=".len()..];
        let quote_char = attr.chars().next().unwrap_or('"');
        let attr_inner = &attr[1..];
        let Some(quote_end) = attr_inner.find(quote_char) else {
            break;
        };
        let key = attr_inner[..quote_end].trim().to_string();
        // 跳过 ">" 之前的剩余属性
        let after_name_close = &attr_inner[quote_end + 1..];
        let Some(open_close) = after_name_close.find('>') else {
            break;
        };
        let abs_value_start = p_start
            + name_pos
            + "name=".len()
            + 1
            + quote_end
            + 1
            + open_close
            + 1;
        // 关键：模型在 array<object> 时常会嵌套 <parameter name="...">，必须按深度找匹配 </parameter>
        let Some(close_abs) = find_matching_close(body, abs_value_start, "<parameter", "</parameter>")
        else {
            break;
        };
        let value_str = &body[abs_value_start..close_abs];
        let value = parse_param_value(value_str);
        obj.insert(key, value);
        i = close_abs + "</parameter>".len();
    }
    Value::Object(obj)
}

fn parse_param_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    // CDATA?
    if let Some(inner) = strip_cdata(trimmed) {
        return Value::String(inner);
    }
    // 多个 <item>... </item> → array（兼容 <item attr="..."> 写法）
    if trimmed.starts_with("<item>") || trimmed.starts_with("<item ") {
        let mut items = Vec::new();
        let mut idx = 0;
        while let Some(start) = find_after(trimmed, "<item", idx) {
            // 容忍 <item> 上的属性：跳到首个 '>' 作为 open 标签结束
            let Some(open_end_rel) = trimmed[start..].find('>') else {
                break;
            };
            let body_start = start + open_end_rel + 1;
            let Some(close_abs) =
                find_matching_close(trimmed, body_start, "<item", "</item>")
            else {
                break;
            };
            let inner = &trimmed[body_start..close_abs];
            items.push(parse_param_value(inner));
            idx = close_abs + "</item>".len();
        }
        if !items.is_empty() {
            return Value::Array(items);
        }
    }
    // 模型也常用 <parameter name="..."> 写嵌套对象（与外层一致），优先按 invoke 参数风格解析
    if trimmed.contains("<parameter") {
        if let Value::Object(map) = parse_invoke_params(trimmed) {
            if !map.is_empty() {
                return Value::Object(map);
            }
        }
    }
    // 嵌套对象 <field>...</field>
    if trimmed.starts_with('<') && trimmed.ends_with('>') {
        let mut obj = serde_json::Map::new();
        let mut i = 0usize;
        let bytes = trimmed.as_bytes();
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }
            let Some(name_end) = trimmed[i + 1..].find('>') else {
                break;
            };
            let tag_open_end = i + 1 + name_end;
            let tag_name = trimmed[i + 1..tag_open_end].trim();
            if tag_name.starts_with('/') {
                i = tag_open_end + 1;
                continue;
            }
            let close = format!("</{tag_name}>");
            let Some(close_pos) = trimmed[tag_open_end + 1..].find(&close) else {
                break;
            };
            let body = &trimmed[tag_open_end + 1..tag_open_end + 1 + close_pos];
            obj.insert(tag_name.to_string(), parse_param_value(body));
            i = tag_open_end + 1 + close_pos + close.len();
        }
        if !obj.is_empty() {
            return Value::Object(obj);
        }
    }
    // 标量
    if trimmed == "null" {
        return Value::Null;
    }
    if let Ok(b) = trimmed.parse::<bool>() {
        return Value::Bool(b);
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return json!(n);
    }
    if let Ok(n) = trimmed.parse::<f64>() {
        return json!(n);
    }
    Value::String(trimmed.to_string())
}

fn strip_cdata(s: &str) -> Option<String> {
    let s = s.trim();
    if !s.starts_with("<![CDATA[") || !s.ends_with("]]>") {
        return None;
    }
    let body = &s["<![CDATA[".len()..s.len() - "]]>".len()];
    Some(body.replace("]]]]><![CDATA[>", "]]>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str, state: &mut DeepseekStreamState) -> DeepseekChunkResult {
        parse_sse_line(line, true, state).unwrap()
    }

    #[test]
    fn parse_sse_text_delta() {
        // 没有先声明类型时，content 默认按 text 走
        let mut state = DeepseekStreamState::default();
        let r = parse(
            r#"{"p":"response/fragments/-1/content","o":"APPEND","v":"hello"}"#,
            &mut state,
        );
        assert_eq!(r.parts.len(), 1);
        assert!(matches!(&r.parts[0], DeepseekChunkPart::Text(s) if s == "hello"));
    }

    #[test]
    fn parse_sse_thinking_path() {
        let mut state = DeepseekStreamState::default();
        let r = parse(r#"{"p":"response/thinking_content","v":"思考"}"#, &mut state);
        assert!(matches!(&r.parts[0], DeepseekChunkPart::Thinking(s) if s == "思考"));
    }

    #[test]
    fn parse_sse_finished() {
        let mut state = DeepseekStreamState::default();
        let r = parse(r#"{"p":"response/status","v":"FINISHED"}"#, &mut state);
        assert!(r.finished);
    }

    #[test]
    fn parse_sse_fragments_array() {
        let mut state = DeepseekStreamState::default();
        let data = r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"THINK","content":"a"},{"type":"RESPONSE","content":"b"}]}"#;
        let r = parse(data, &mut state);
        assert_eq!(r.parts.len(), 2);
        assert!(matches!(&r.parts[0], DeepseekChunkPart::Thinking(s) if s == "a"));
        assert!(matches!(&r.parts[1], DeepseekChunkPart::Text(s) if s == "b"));
    }

    #[test]
    fn fragment_type_carries_into_subsequent_content_appends() {
        // ds2api 真实抓包：先来一条 fragments 声明 THINK，
        // 接下来一串 fragments/-1/content APPEND 的字符串增量都属于这一段 thinking
        let mut state = DeepseekStreamState::default();
        let _ = parse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"THINK","content":""}]}"#,
            &mut state,
        );
        let r1 = parse(
            r#"{"p":"response/fragments/-1/content","o":"APPEND","v":"思考"}"#,
            &mut state,
        );
        let r2 = parse(
            r#"{"p":"response/fragments/-1/content","o":"APPEND","v":"过程"}"#,
            &mut state,
        );
        assert!(matches!(&r1.parts[0], DeepseekChunkPart::Thinking(s) if s == "思考"));
        assert!(matches!(&r2.parts[0], DeepseekChunkPart::Thinking(s) if s == "过程"));

        // 之后 RESPONSE 类型 fragment 切到 text
        let _ = parse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":""}]}"#,
            &mut state,
        );
        let r3 = parse(
            r#"{"p":"response/fragments/-1/content","o":"APPEND","v":"答案"}"#,
            &mut state,
        );
        assert!(matches!(&r3.parts[0], DeepseekChunkPart::Text(s) if s == "答案"));
    }

    #[test]
    fn initial_response_object_sets_thinking_kind_and_sticky_path_carries_appends() {
        // 抓自实测：第一条 chunk 是整个 response 对象，里面 fragments 已带 type=THINK；
        // 之后省略 path 的 {"v":"..."} chunk 必须沿用 sticky path 走 thinking。
        let mut state = DeepseekStreamState::default();
        let r0 = parse(
            r#"{"v":{"response":{"fragments":[{"type":"THINK","content":"The"}]}}}"#,
            &mut state,
        );
        assert!(matches!(&r0.parts[0], DeepseekChunkPart::Thinking(s) if s == "The"));

        let r1 = parse(
            r#"{"p":"response/fragments/-1/content","o":"APPEND","v":" user"}"#,
            &mut state,
        );
        assert!(matches!(&r1.parts[0], DeepseekChunkPart::Thinking(s) if s == " user"));

        // sticky：没有 p 字段，应当沿用 response/fragments/-1/content
        let r2 = parse(r#"{"v":" is"}"#, &mut state);
        assert!(matches!(&r2.parts[0], DeepseekChunkPart::Thinking(s) if s == " is"));

        // 切到 RESPONSE fragment 后，sticky 同 path 的 chunk 应当当 text
        let _ = parse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":"1"}]}"#,
            &mut state,
        );
        let r3 = parse(
            r#"{"p":"response/fragments/-1/content","v":"+"}"#,
            &mut state,
        );
        assert!(matches!(&r3.parts[0], DeepseekChunkPart::Text(s) if s == "+"));
        let r4 = parse(r#"{"v":"1"}"#, &mut state);
        assert!(matches!(&r4.parts[0], DeepseekChunkPart::Text(s) if s == "1"));
    }

    #[test]
    fn read_link_fragment_is_classified_as_thinking() {
        // 抓自第二轮 SSE：DeepSeek 在 web_search 后用 READ_LINK 开 head fragment
        // 「边读链接边总结」属于内部工作，应该折进 thinking 不能泄到正文
        let mut state = DeepseekStreamState::default();
        let r0 = parse(
            r#"{"v":{"response":{"fragments":[{"type":"READ_LINK","status":"WIP"}]}}}"#,
            &mut state,
        );
        // 没 content 不会立即吐字，但 state.current_kind 应已切到 Thinking
        assert!(r0.parts.is_empty());

        let r1 = parse(r#"{"v":" about today's weather"}"#, &mut state);
        assert!(matches!(&r1.parts[0], DeepseekChunkPart::Thinking(s) if s == " about today's weather"));

        // 切到 RESPONSE 后才回 text
        let _ = parse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":"根据"}]}"#,
            &mut state,
        );
        let r3 = parse(r#"{"v":"搜索"}"#, &mut state);
        assert!(matches!(&r3.parts[0], DeepseekChunkPart::Text(s) if s == "搜索"));
    }

    #[test]
    fn close_think_tag_inside_thinking_switches_to_text() {
        let mut state = DeepseekStreamState::default();
        let _ = parse(
            r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"THINK","content":""}]}"#,
            &mut state,
        );
        // 上游 bug：thinking 段里夹了 </think>，之后内容应当算 text
        let r = parse(
            r#"{"p":"response/fragments/-1/content","o":"APPEND","v":"思考完了</think>正式答案"}"#,
            &mut state,
        );
        assert_eq!(r.parts.len(), 2);
        assert!(matches!(&r.parts[0], DeepseekChunkPart::Thinking(s) if s == "思考完了"));
        assert!(matches!(&r.parts[1], DeepseekChunkPart::Text(s) if s == "正式答案"));
    }

    #[test]
    fn sieve_swallows_legacy_tool_calls_across_chunks() {
        let mut sieve = ToolCallSieve::new();
        // 模型先吐了一段普通 text，然后 `<tool_calls>` 跨 chunk
        let parts = [
            "你好",
            "<tool",
            "_calls>\n  <invoke name=\"web",
            "_search\"><parameter name=\"q\"><![CDATA[",
            "深圳天气]]></parameter></invoke></tool",
            "_calls>",
            "尾巴正文",
        ];
        let mut emitted = String::new();
        for p in parts {
            emitted.push_str(&sieve.push(p));
        }
        emitted.push_str(&sieve.finalize());
        assert_eq!(emitted, "你好尾巴正文");
    }

    #[test]
    fn sieve_swallows_dsml_tool_calls() {
        let mut sieve = ToolCallSieve::new();
        let mut emitted = String::new();
        emitted.push_str(&sieve.push("前缀<|DSML|tool_calls><|DSML|invoke name=\"x\"><|DSML|parameter name=\"a\"><![CDATA[1]]></|DSML|parameter></|DSML|invoke></|DSML|tool_calls>后缀"));
        emitted.push_str(&sieve.finalize());
        assert_eq!(emitted, "前缀后缀");
    }

    #[test]
    fn sieve_holds_potential_open_tag_until_safe() {
        let mut sieve = ToolCallSieve::new();
        // 末尾正在生成 `<` 不能立刻吐，等下一段确认是 < 标签还是普通 text
        let a = sieve.push("xxxxxxxxxxxxxxxxxxxxxxxxxxxx<");
        // 应该至少吐出前面的 x，但保留尾部含 `<`
        assert!(a.contains('x'));
        assert!(!a.contains('<'));
        // 接着发现是普通 text，不是 tool_calls，剩下的 < 加 a 该吐出
        let b = sieve.push("a普通字符");
        let c = sieve.finalize();
        let total = format!("{a}{b}{c}");
        assert!(total.ends_with("<a普通字符") || total.ends_with("a普通字符"));
        assert!(total.contains("<a普通字符") || total.contains("a普通字符"));
    }

    #[test]
    fn extract_legacy_tool_call() {
        let s = r#"some text<tool_calls>
  <invoke name="Bash">
    <parameter name="command"><![CDATA[ls -la]]></parameter>
  </invoke>
</tool_calls>"#;
        let (rest, calls) = extract_tool_calls(s);
        assert_eq!(rest, "some text");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].input["command"], "ls -la");
    }

    #[test]
    fn extract_multiple_tool_call_wrappers() {
        // 模型可能生成多个独立的 <tool_calls> 块（每个含一个 invoke）；都要捕获。
        let s = r#"先这样<tool_calls>
  <invoke name="web_search">
    <parameter name="query"><![CDATA[A]]></parameter>
  </invoke>
</tool_calls>
然后再这样
<tool_calls>
  <invoke name="web_search">
    <parameter name="query"><![CDATA[B]]></parameter>
  </invoke>
</tool_calls>"#;
        let (rest, calls) = extract_tool_calls(s);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].input["query"], "A");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[1].name, "web_search");
        assert_eq!(calls[1].input["query"], "B");
        assert_eq!(calls[1].id, "call_2");
        // 中间的「然后再这样」保留为正文
        assert!(rest.contains("先这样"));
        assert!(rest.contains("然后再这样"));
    }

    #[test]
    fn extract_multiple_invokes_in_single_wrapper() {
        // 一个 wrapper 内并列两个 invoke（DSML 推荐的并行调用形态）
        let s = r#"<tool_calls>
  <invoke name="web_search">
    <parameter name="query"><![CDATA[A]]></parameter>
  </invoke>
  <invoke name="web_search">
    <parameter name="query"><![CDATA[B]]></parameter>
  </invoke>
</tool_calls>"#;
        let (_rest, calls) = extract_tool_calls(s);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].input["query"], "A");
        assert_eq!(calls[1].input["query"], "B");
    }

    #[test]
    fn extract_dsml_tool_call() {
        let s = r#"<|DSML|tool_calls><|DSML|invoke name="Read"><|DSML|parameter name="file_path"><![CDATA[a.txt]]></|DSML|parameter></|DSML|invoke></|DSML|tool_calls>"#;
        let (_rest, calls) = extract_tool_calls(s);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Read");
        assert_eq!(calls[0].input["file_path"], "a.txt");
    }

    #[test]
    fn model_type_mapping() {
        assert_eq!(model_type_for("deepseek-v4-flash"), "default");
        assert_eq!(model_type_for("deepseek-v4-pro"), "expert");
        assert_eq!(model_type_for("deepseek-v4-vision"), "vision");
    }

    /// 真实复现：ask 工具 options 是 array<object>，模型按外层一致风格在 <item> 里继续
    /// 写 <parameter name="label">...</parameter>。早期 parser 用 `find("</parameter>")`
    /// 找闭合，会被内层 label 的 </parameter> 截断 → options 退化成 String → 反序列化失败。
    #[test]
    fn extract_ask_tool_with_nested_array_of_objects() {
        let s = r#"<tool_calls>
  <invoke name="ask">
    <parameter name="question"><![CDATA[你想做什么？]]></parameter>
    <parameter name="options" type="array">
      <item>
        <parameter name="label" type="string"><![CDATA[选项 A]]></parameter>
        <parameter name="description" type="string"><![CDATA[A 的说明]]></parameter>
      </item>
      <item>
        <parameter name="label" type="string"><![CDATA[选项 B]]></parameter>
        <parameter name="description" type="string"><![CDATA[某个已有应用或网站，我想找回之前的生成版本]]></parameter>
      </item>
    </parameter>
    <parameter name="multi">false</parameter>
  </invoke>
</tool_calls>"#;
        let (_rest, calls) = extract_tool_calls(s);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ask");
        assert_eq!(calls[0].input["question"], "你想做什么？");
        assert_eq!(calls[0].input["multi"], false);
        let options = calls[0].input["options"]
            .as_array()
            .expect("options should be array");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["label"], "选项 A");
        assert_eq!(options[0]["description"], "A 的说明");
        assert_eq!(options[1]["label"], "选项 B");
        assert_eq!(
            options[1]["description"],
            "某个已有应用或网站，我想找回之前的生成版本"
        );
    }

    /// 兼容路径：模型若用「tag-as-key」风格写嵌套对象（<label>...</label>），仍能解析。
    #[test]
    fn extract_ask_tool_with_tag_as_key_objects() {
        let s = r#"<tool_calls>
  <invoke name="ask">
    <parameter name="question"><![CDATA[Q]]></parameter>
    <parameter name="options">
      <item><label><![CDATA[A]]></label><description><![CDATA[da]]></description></item>
      <item><label><![CDATA[B]]></label><description><![CDATA[db]]></description></item>
    </parameter>
  </invoke>
</tool_calls>"#;
        let (_rest, calls) = extract_tool_calls(s);
        assert_eq!(calls.len(), 1);
        let options = calls[0].input["options"].as_array().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["label"], "A");
        assert_eq!(options[1]["description"], "db");
    }

    #[test]
    fn tool_results_render_under_user_role_header() {
        // 防回归：tool_results 一定要包在 ### User 段里，绝不能再用 ### Tool Result。
        // 之前用 ### Tool Result 会让 deepseek 模型在第二轮开始伪造对话脚本，
        // 后续 tool_call 永远进不到 <tool_calls> wrapper 里。
        use crate::types::{
            AssistantEntry, ModelRequest, ToolCall, ToolResult, TranscriptEntry, UserEntry,
        };
        let req = ModelRequest {
            system: Some("sys".into()),
            entries: vec![
                TranscriptEntry::User(UserEntry {
                    text: "查一下".into(),
                    attachments: Vec::new(),
                }),
                TranscriptEntry::Assistant(AssistantEntry {
                    text: String::new(),
                    reasoning: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "ls"}),
                    }],
                }),
                TranscriptEntry::ToolResults(vec![ToolResult {
                    call_id: "call_1".into(),
                    name: "Bash".into(),
                    content: "a.txt\nb.txt".into(),
                }]),
            ],
            tools: Vec::new(),
            model: "deepseek-v4".into(),
            max_tokens: 1024,
            reasoning: None,
        };
        let prompt = build_prompt(&req, &[]);
        assert!(
            !prompt.contains("### Tool Result"),
            "tool_result 必须包在 ### User 里，不能再造 ### Tool Result 角色头\n实际:\n{prompt}",
        );
        assert!(prompt.contains("### User\n<tool_result"));
    }

    #[test]
    fn close_think_tolerates_common_misspellings() {
        // 模型常见把闭合写成 </thinking> / </thought> / </think_block>，
        // 都应当被识别为 reasoning 段闭合，否则后续正文会被错渲染成「思考过程」。
        for (s, expected_after) in [
            ("abc</think>def", "def"),
            ("abc</thinking>def", "def"),
            ("abc</think_block>def", "def"),
            ("abc</Think>def", "def"),
            ("abc</think >def", "def"),
        ] {
            let r = find_close_think(s).unwrap_or_else(|| panic!("未识别: {s}"));
            assert_eq!(&s[r.end..], expected_after, "case: {s}");
        }
        assert!(find_close_think("没有闭合标签").is_none());
        // 不要把整段后续吞掉：tag 内文超过 MAX_TAIL 字节就不当作闭合
        assert!(find_close_think("abc</think 我要写一段超长的描述继续等等等等等等>def").is_none());
    }

    #[test]
    fn fake_role_header_cut_detects_common_hallucinations() {
        // 模型自爆开始续写对话脚本时常见的几种伪角色头都要能命中。
        let cases = [
            "正文\n### User\n伪造的下一轮",
            "正文\n### Assistant\n伪造续写",
            "正文\n### Tool Result\nfake",
            "正文\n### Tool Output\nfake",
            "正文\n### System\nfake",
        ];
        for s in cases {
            let cut = find_fake_role_header_cut(s).expect(s);
            assert_eq!(&s[..cut], "正文", "{s}");
        }
        assert!(find_fake_role_header_cut("没有伪头部，只是普通正文").is_none());
        // 不要误伤句中提到的 "### " —— 必须以 \n 起始
        assert!(find_fake_role_header_cut("行内提到 ### User 不是新行").is_none());
    }
}
