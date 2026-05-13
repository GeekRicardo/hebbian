use serde_json::Value;

use model_gateway::types::{AssistantEntry, ToolCall, ToolResult, TranscriptEntry, UserEntry};
use common::attachments::MessageAttachment;
use crate::storage::sessions::{Message, MessageMeta, MessagePart, Role};

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
        let boundary = messages.iter().rposition(|m| {
            matches!(
                m.meta,
                Some(MessageMeta::CompactBoundary { .. })
            )
        });

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
                        tool_calls: Vec::new(),
                    }));
                }
            }
        }

        let start = boundary.map(|i| i + 1).unwrap_or(0);
        for msg in &messages[start..] {
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
        .map(|call| ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
        })
        .collect();
    entries.push(TranscriptEntry::Assistant(AssistantEntry {
        text: msg.content.clone(),
        reasoning: String::new(),
        tool_calls: tool_calls.clone(),
    }));

    let tool_results: Vec<ToolResult> = msg
        .tool_calls
        .iter()
        .filter_map(|call| {
            call.result.as_ref().map(|content| ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: content.clone(),
                artifact: None,
            })
        })
        .collect();
    if !tool_calls.is_empty() && !tool_results.is_empty() {
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
            MessagePart::Reasoning { text: next_reasoning } => {
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
                result,
                ..
            } => {
                tool_calls.push(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                if let Some(content) = result {
                    tool_results.push(ToolResult {
                        call_id: id.clone(),
                        name: name.clone(),
                        content: content.clone(),
                        artifact: None,
                    });
                }
            }
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
        entries.push(TranscriptEntry::Assistant(AssistantEntry {
            text,
            reasoning,
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
        tool_calls: std::mem::take(tool_calls),
    }));
    if !tool_results.is_empty() {
        entries.push(TranscriptEntry::ToolResults(std::mem::take(tool_results)));
    }
}
