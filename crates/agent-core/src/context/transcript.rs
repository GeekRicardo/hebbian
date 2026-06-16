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
            input: call.input.clone(),
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
                    input: input.clone(),
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
        }
    }

    /// 回归：历史以 CompactBoundary 结尾时，`from_session` 会注入「已收到前情概要」
    /// 占位 assistant，使 transcript 以 assistant 结尾。Claude Opus/Sonnet 4.6+ 拒绝
    /// assistant prefill（400 "conversation must end with a user message"）。旁支引擎
    /// `run_aside` 据此改为 `from_session(历史) + push_user`，保证末尾永远是 user。
    /// 本测试锁定「末尾 boundary → assistant 结尾」这个危险属性，提醒任何复用 from_session
    /// 重建后直接发请求的路径必须自己补 user。
    #[test]
    fn from_session_with_trailing_compact_boundary_ends_with_assistant() {
        let mut boundary = user_msg("b", "");
        boundary.meta = Some(MessageMeta::CompactBoundary {
            summary: "前情提要内容".to_string(),
            before_tokens: 100,
            after_tokens: 10,
        });
        let history = vec![user_msg("u1", "hi"), boundary];

        let t = Transcript::from_session(Some("sys".to_string()), &history);
        // 末尾是占位 assistant —— 直接发请求会 400
        assert!(matches!(
            t.entries.last(),
            Some(TranscriptEntry::Assistant(_))
        ));

        // 修复手法：再 push 一条 user，末尾恢复为 user
        let mut fixed = t;
        fixed.push_user("新问题".to_string(), Vec::new());
        assert!(matches!(fixed.entries.last(), Some(TranscriptEntry::User(_))));
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
        };

        let t = Transcript::from_session(
            None,
            &[parent_user, child_user, child_assistant, parent_assistant],
        );

        // 子事件被过滤：只剩 parent user + parent assistant
        assert_eq!(t.entries.len(), 2);
        match &t.entries[0] {
            TranscriptEntry::User(u) => assert_eq!(u.text, "parent question"),
            other => panic!("expected parent User first, got {other:?}"),
        }
        match &t.entries[1] {
            TranscriptEntry::Assistant(a) => assert_eq!(a.text, "parent reply"),
            other => panic!("expected parent Assistant second, got {other:?}"),
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
}
