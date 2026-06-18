use serde_json::Value;

use crate::context::tool_xml_leak::sanitize_tool_xml_leak;
use crate::storage::sessions::{Message, MessageMeta, MessagePart, Role};
use common::attachments::MessageAttachment;
use model_gateway::types::{AssistantEntry, ToolCall, ToolResult, TranscriptEntry, UserEntry};

#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone)]
pub struct ToolCallResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Transcript {
    pub system: Option<String>,
    pub entries: Vec<TranscriptEntry>,
}

impl Transcript {
    pub fn new(system: Option<String>) -> Self {
        Self {
            system,
            entries: Vec::new(),
        }
    }

    pub fn from_session(system: Option<String>, messages: &[Message]) -> Self {
        // 找到最近一次 CompactBoundary：之前的消息全部跳过，summary 作为前情提要注入。
        let boundary = messages
            .iter()
            .rposition(|m| matches!(m.meta, Some(MessageMeta::CompactBoundary { .. })));

        let mut t = Self::new(system);
        if let Some(idx) = boundary {
            if let Some(MessageMeta::CompactBoundary { summary, .. }) = &messages[idx].meta {
                if !summary.trim().is_empty() {
                    t.entries.push(TranscriptEntry::User(UserEntry {
                        text: format!("[前情概要]\n{summary}"),
                        attachments: Vec::new(),
                    }));
                    t.entries.push(TranscriptEntry::Assistant(AssistantEntry {
                        text: "已收到前情概要，将基于此继续。".to_string(),
                        reasoning: String::new(),
                        reasoning_signature: String::new(),
                        tool_calls: Vec::new(),
                    }));
                }
            }
        }

        let start = boundary.map(|i| i + 1).unwrap_or(0);
        for msg in &messages[start..] {
            // Subagent 子 NestedRun 的消息已经在子 session.jsonl 自成一份（架构 §4.4.11.8），
            // 父 transcript 重建时跳过——父只关心 Task 工具调用的 ToolResult（子终态文本），
            // 那一条 ToolResult 由父侧 dispatcher 在 spawn_task 完成时写入父 Message。
            if msg.subagent_call_id.is_some() {
                continue;
            }
            match msg.role {
                Role::User => t.entries.push(TranscriptEntry::User(UserEntry {
                    text: msg.content.clone(),
                    attachments: msg.attachments.clone(),
                })),
                Role::Assistant => push_assistant_message(&mut t.entries, msg),
                _ => {}
            }
        }

        // 所有已支持的 API（Anthropic / OpenAI 兼容）都要求 messages 最后一条必须是
        // user message；assistant prefill 路径只有极少数特殊场景用到且需要明确 opt-in。
        // from_session 重建历史后末尾可能是 assistant（截断续跑、boundary 占位等），
        // 统一在这里注入"继续"兜底，让所有重用 from_session 的路径都是合法状态。
        // continue_run 逻辑不再特判——transcript 末尾已经保证是 user。
        if matches!(t.entries.last(), Some(TranscriptEntry::Assistant(_)) | Some(TranscriptEntry::ToolResults(_))) {
            t.entries.push(TranscriptEntry::User(UserEntry {
                text: "继续".to_string(),
                attachments: Vec::new(),
            }));
        }

        t
    }

    pub fn push_user(&mut self, text: String, attachments: Vec<MessageAttachment>) {
        self.entries
            .push(TranscriptEntry::User(UserEntry { text, attachments }));
    }

    pub fn push_assistant(&mut self, text: String, tool_calls: Vec<ToolCall>) {
        self.push_assistant_with_reasoning(text, String::new(), tool_calls);
    }

    /// 带 reasoning 的 push（DeepSeek 等需要回喂思维链的 provider）。
    /// 普通 push_assistant 是 reasoning="" 的快捷方式。
    pub fn push_assistant_with_reasoning(
        &mut self,
        text: String,
        reasoning: String,
        tool_calls: Vec<ToolCall>,
    ) {
        self.entries
            .push(TranscriptEntry::Assistant(AssistantEntry {
                text,
                reasoning,
                reasoning_signature: String::new(),
                tool_calls,
            }));
    }

    pub fn push_tool_results(&mut self, results: Vec<ToolResult>) {
        self.entries.push(TranscriptEntry::ToolResults(results));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 把 tool_call 的 input 归一成 object：
/// - 已经是 object → 原样返回
/// - 字符串 → 先尝试 JSON re-parse（双重编码场景还原），仍非 object → 空 object
/// - 其他（null / array / number …）→ 空 object
///
/// 从磁盘重建 transcript 时保底：历史里偶尔会出现生成时解析失败而退化成字符串的
/// input（如工具参数生成到一半被截断），原样发给 API 会 400。
fn normalize_tool_input(input: &Value) -> Value {
    match input {
        Value::Object(_) => input.clone(),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Default::default())),
        _ => Value::Object(Default::default()),
    }
}

fn push_assistant_message(entries: &mut Vec<TranscriptEntry>, msg: &Message) {
    if !msg.parts.is_empty() {
        push_assistant_parts(entries, &msg.parts);
        return;
    }

    let tool_calls: Vec<ToolCall> = msg
        .tool_calls
        .iter()
        .filter(|call| call.result.is_some())
        .map(|call| ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            input: normalize_tool_input(&call.input),
        })
        .collect();

    let tool_results: Vec<ToolResult> = msg
        .tool_calls
        .iter()
        .filter_map(|call| {
            call.result.as_ref().map(|content| ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: content.clone(),
                artifact: None,
                attachments: Vec::new(),
            })
        })
        .collect();
    if !tool_calls.is_empty() || !msg.content.is_empty() {
        // 加载兜底（架构 §4.3.3）：观察者按既定取舍把脏正文原样落盘，重启续聊读回时
        // 在此清洗，杜绝残骸经历史再次喂给模型。无 tool_call 的纯文本才可能是残骸。
        let text = if tool_calls.is_empty() {
            sanitize_tool_xml_leak(&msg.content).text
        } else {
            msg.content.clone()
        };
        entries.push(TranscriptEntry::Assistant(AssistantEntry {
            text,
            reasoning: String::new(),
            reasoning_signature: String::new(),
            tool_calls: tool_calls.clone(),
        }));
    }
    if !tool_results.is_empty() {
        entries.push(TranscriptEntry::ToolResults(tool_results));
    }
}

fn push_assistant_parts(entries: &mut Vec<TranscriptEntry>, parts: &[MessagePart]) {
    let mut text = String::new();
    // 把同一轮内的 reasoning 段拼起来回填，让 DeepSeek 等 thinking-aware provider
    // 在多轮 + tool_call 场景里看到自己上一轮的推理链，避免重复思考或丢上下文。
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();

    for part in parts {
        match part {
            MessagePart::Text { text: next_text } => {
                if !tool_calls.is_empty() {
                    flush_assistant_turn(
                        entries,
                        &mut text,
                        &mut reasoning,
                        &mut tool_calls,
                        &mut tool_results,
                    );
                }
                text.push_str(next_text);
            }
            MessagePart::Reasoning {
                text: next_reasoning,
            } => {
                if !tool_calls.is_empty() {
                    // reasoning 出现在工具调用之后罕见；如果发生，先把当前 turn 落桶。
                    flush_assistant_turn(
                        entries,
                        &mut text,
                        &mut reasoning,
                        &mut tool_calls,
                        &mut tool_results,
                    );
                }
                reasoning.push_str(next_reasoning);
            }
            MessagePart::ToolCall {
                id,
                name,
                input,
                result: Some(content),
                ..
            } => {
                tool_calls.push(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: normalize_tool_input(input),
                });
                tool_results.push(ToolResult {
                    call_id: id.clone(),
                    name: name.clone(),
                    content: content.clone(),
                    artifact: None,
                    attachments: Vec::new(),
                });
            }
            MessagePart::ToolCall { result: None, .. } => {}
        }
    }

    if !tool_calls.is_empty() {
        flush_assistant_turn(
            entries,
            &mut text,
            &mut reasoning,
            &mut tool_calls,
            &mut tool_results,
        );
    } else if !text.is_empty() || !reasoning.is_empty() {
        // 加载兜底（架构 §4.3.3）：纯文本段（无 tool_call）才可能是漏出的残骸，清洗后入桶。
        entries.push(TranscriptEntry::Assistant(AssistantEntry {
            text: sanitize_tool_xml_leak(&text).text,
            reasoning,
            reasoning_signature: String::new(),
            tool_calls: Vec::new(),
        }));
    }
}

fn flush_assistant_turn(
    entries: &mut Vec<TranscriptEntry>,
    text: &mut String,
    reasoning: &mut String,
    tool_calls: &mut Vec<ToolCall>,
    tool_results: &mut Vec<ToolResult>,
) {
    entries.push(TranscriptEntry::Assistant(AssistantEntry {
        text: std::mem::take(text),
        reasoning: std::mem::take(reasoning),
        reasoning_signature: String::new(),
        tool_calls: std::mem::take(tool_calls),
    }));
    if !tool_results.is_empty() {
        entries.push(TranscriptEntry::ToolResults(std::mem::take(tool_results)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sessions::{MessageMeta, MessagePart, MessageToolCall};
    use serde_json::json;

    fn user_msg(id: &str, text: &str) -> Message {
        Message {
            id: id.to_string(),
            role: Role::User,
            content: text.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        }
    }

    /// 回归：历史以 CompactBoundary 结尾时，`from_session` 注入「已收到前情概要」
    /// 占位 assistant 后，必须再补一条"继续" user，否则 API 400。
    /// 本测试锁定「from_session 末尾永远是 user」这个不变式。
    #[test]
    fn from_session_always_ends_with_user() {
        // case 1: boundary 在末尾——会产生占位 assistant，必须被补 user
        let mut boundary = user_msg("b", "");
        boundary.meta = Some(MessageMeta::CompactBoundary {
            summary: "前情提要内容".to_string(),
            before_tokens: 100,
            after_tokens: 10,
        });
        let history = vec![user_msg("u1", "hi"), boundary];
        let t = Transcript::from_session(Some("sys".to_string()), &history);
        assert!(
            matches!(t.entries.last(), Some(TranscriptEntry::User(_))),
            "boundary 尾部场景：末尾必须是 user，实际是 {:?}",
            t.entries.last()
        );

        // case 2: 最后一条是正常 user message——不应额外插入多余 user
        let history2 = vec![user_msg("u1", "hi"), user_msg("u2", "继续")];
        let t2 = Transcript::from_session(None, &history2);
        assert!(matches!(t2.entries.last(), Some(TranscriptEntry::User(_))));
        assert_eq!(
            t2.entries
                .iter()
                .filter(|e| matches!(e, TranscriptEntry::User(_)))
                .count(),
            2,
            "不应重复插入 user"
        );
    }

    fn assistant(parts: Vec<MessagePart>, tool_calls: Vec<MessageToolCall>) -> Message {
        Message {
            id: "assistant-1".to_string(),
            role: Role::Assistant,
            content: String::new(),
            attachments: Vec::new(),
            tool_calls,
            parts,
            created_at: 0,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        }
    }

    #[test]
    fn from_session_skips_messages_tagged_with_subagent_call_id() {
        let parent_user = Message {
            id: "u1".to_string(),
            role: Role::User,
            content: "parent question".to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };
        let child_user = Message {
            id: "u2".to_string(),
            role: Role::User,
            content: "child internal user msg".to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
            subagent_call_id: Some("task-call-1".to_string()),
            run_duration_ms: None,
        };
        let child_assistant = Message {
            id: "a1".to_string(),
            role: Role::Assistant,
            content: "child reply".to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
            subagent_call_id: Some("task-call-1".to_string()),
            run_duration_ms: None,
        };
        let parent_assistant = Message {
            id: "a2".to_string(),
            role: Role::Assistant,
            content: "parent reply".to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            parts: Vec::new(),
            created_at: 0,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };

        let t = Transcript::from_session(
            None,
            &[parent_user, child_user, child_assistant, parent_assistant],
        );

        // 子事件被过滤：剩 parent user + parent assistant。
        // parent assistant 是末尾，from_session 自动补"继续" user → 共 3 条。
        assert_eq!(t.entries.len(), 3, "entries={:?}", t.entries);
        match &t.entries[0] {
            TranscriptEntry::User(u) => assert_eq!(u.text, "parent question"),
            other => panic!("expected parent User first, got {other:?}"),
        }
        match &t.entries[1] {
            TranscriptEntry::Assistant(a) => assert_eq!(a.text, "parent reply"),
            other => panic!("expected parent Assistant second, got {other:?}"),
        }
        match &t.entries[2] {
            TranscriptEntry::User(u) => assert_eq!(u.text, "继续"),
            other => panic!("expected injected '继续' user third, got {other:?}"),
        }
    }

    #[test]
    fn skips_unfinished_part_tool_calls_when_rebuilding_transcript() {
        let msg = assistant(
            vec![
                MessagePart::Text {
                    text: "before".to_string(),
                },
                MessagePart::ToolCall {
                    id: "call_done".to_string(),
                    name: "Read".to_string(),
                    input: json!({"file_path": "a.txt"}),
                    arguments: "{\"file_path\":\"a.txt\"}".to_string(),
                    result: Some("file contents".to_string()),
                    duration_ms: None,
                    is_error: false,
                },
                MessagePart::Text {
                    text: "after".to_string(),
                },
                MessagePart::ToolCall {
                    id: "call_orphan".to_string(),
                    name: "Edit".to_string(),
                    input: json!({"file_path": "a.txt"}),
                    arguments: "{\"file_path\":\"a.txt\"}".to_string(),
                    result: None,
                    duration_ms: None,
                    is_error: false,
                },
            ],
            Vec::new(),
        );

        let transcript = Transcript::from_session(None, &[msg]);
        let has_orphan = transcript.entries.iter().any(|entry| match entry {
            TranscriptEntry::Assistant(a) => {
                a.tool_calls.iter().any(|call| call.id == "call_orphan")
            }
            TranscriptEntry::ToolResults(results) => {
                results.iter().any(|result| result.call_id == "call_orphan")
            }
            TranscriptEntry::User(_) => false,
        });

        assert!(!has_orphan);
        assert!(transcript.entries.iter().any(|entry| match entry {
            TranscriptEntry::Assistant(a) => {
                a.tool_calls.iter().any(|call| call.id == "call_done")
            }
            _ => false,
        }));
        assert!(transcript.entries.iter().any(|entry| match entry {
            TranscriptEntry::ToolResults(results) => {
                results.iter().any(|result| result.call_id == "call_done")
            }
            _ => false,
        }));
    }

    /// 加载兜底回归：还原真实 session 202606160757-eeb33d38 的脏 message 形态——
    /// 「干净 Text + 已完成 ToolCall + 末尾 Text 含 court+<invoke> 残骸」。
    /// `push_assistant_parts` 末尾走「else if !text.is_empty()」分支，必须经 sanitize。
    /// 修前会留 <invoke> 残骸继续喂模型；修后该 assistant 文本被截到「现在调度器：...」。
    #[test]
    fn from_session_sanitizes_trailing_dirty_text_in_part_stream() {
        let msg = assistant(
            vec![
                MessagePart::Text {
                    text: "现在让某某工具看一下。".to_string(),
                },
                MessagePart::ToolCall {
                    id: "call_1".to_string(),
                    name: "Read".to_string(),
                    input: json!({"file_path": "a.ts"}),
                    arguments: "{}".to_string(),
                    result: Some("ok".to_string()),
                    duration_ms: None,
                    is_error: false,
                },
                MessagePart::Text {
                    text: "现在调度器：周期性运行优化。\n\ncourt\n<invoke name=\"Edit\">\n<parameter name=\"file_path\">/tmp/a.ts</parameter>\n</invoke>".to_string(),
                },
            ],
            Vec::new(),
        );

        let transcript = Transcript::from_session(None, &[msg]);
        // 任何 assistant 文本都不得残留 <invoke>——自我强化的燃料被掐断。
        for entry in &transcript.entries {
            if let TranscriptEntry::Assistant(a) = entry {
                assert!(
                    !a.text.contains("<invoke") && !a.text.contains("court"),
                    "脏文本未被清洗，仍含残骸: {:?}",
                    a.text
                );
            }
        }
        // 干净前导文本要保留。
        let saw_clean_tail = transcript.entries.iter().any(|entry| matches!(
            entry,
            TranscriptEntry::Assistant(a) if a.text == "现在调度器：周期性运行优化。"
        ));
        assert!(saw_clean_tail, "末尾 Text 应被清洗为「现在调度器：周期性运行优化。」");
    }

    /// input 归一：字符串 / null / 非法字符串都必须在 transcript 层归一成 object，
    /// 不依赖协议层兜底——两层都有防线，最早的层最先命中。
    #[test]
    fn normalize_tool_input_handles_non_object() {
        use serde_json::json;
        // 合法 object → 原样
        assert_eq!(normalize_tool_input(&json!({"a": 1})), json!({"a": 1}));
        // null → {}
        assert_eq!(normalize_tool_input(&Value::Null), json!({}));
        // 数组 → {}
        assert_eq!(normalize_tool_input(&json!([1, 2])), json!({}));
        // 字符串但是合法 JSON object → 还原
        assert_eq!(
            normalize_tool_input(&json!("{\"command\":\"ls\"}")),
            json!({"command": "ls"})
        );
        // 字符串但不是 JSON → {}
        assert_eq!(
            normalize_tool_input(&json!("<parameter name=\"label\">拆 6")),
            json!({})
        );
        // 字符串但 JSON 不是 object（是 array）→ {}
        assert_eq!(normalize_tool_input(&json!("[1,2,3]")), json!({}));
    }

    /// 从 session 重建时 tool_call input 为非 object 的情况下，transcript 里的 ToolCall.input
    /// 必须被归一成 object（不带非法值进入 API 请求）。
    #[test]
    fn from_session_normalizes_bad_tool_input_in_parts() {
        use crate::storage::sessions::{MessagePart, MessageToolCall};
        use serde_json::json;

        let part_with_string_input = MessagePart::ToolCall {
            id: "c1".to_string(),
            name: "Bash".to_string(),
            input: json!("{\"command\":\"ls\"}"), // 字符串，需要 re-parse
            arguments: String::new(),
            result: Some("ok".to_string()),
            duration_ms: Some(10),
            is_error: false,
        };
        let null_input_part = MessagePart::ToolCall {
            id: "c2".to_string(),
            name: "Ask".to_string(),
            input: Value::Null,
            arguments: String::new(),
            result: Some("ans".to_string()),
            duration_ms: None,
            is_error: false,
        };
        let msg = Message {
            id: "a1".to_string(),
            role: Role::Assistant,
            content: String::new(),
            attachments: Vec::new(),
            tool_calls: vec![
                MessageToolCall {
                    id: "c1".to_string(),
                    name: "Bash".to_string(),
                    input: json!("{\"command\":\"ls\"}"),
                    result: Some("ok".to_string()),
                    duration_ms: Some(10),
                    is_error: false,
                    nested: Vec::new(),
                },
                MessageToolCall {
                    id: "c2".to_string(),
                    name: "Ask".to_string(),
                    input: Value::Null,
                    result: Some("ans".to_string()),
                    duration_ms: None,
                    is_error: false,
                    nested: Vec::new(),
                },
            ],
            parts: vec![part_with_string_input, null_input_part],
            created_at: 0,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        };
        let history = vec![user_msg("u1", "do stuff"), msg];
        let t = Transcript::from_session(None, &history);
        // 找到 ToolCall entries 里的 input
        for entry in &t.entries {
            if let TranscriptEntry::Assistant(a) = entry {
                for call in &a.tool_calls {
                    assert!(
                        call.input.is_object(),
                        "tool {} input 应为 object，实际={:?}",
                        call.name,
                        call.input
                    );
                }
            }
        }
    }
}
