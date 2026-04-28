use serde_json::Value;

use model_gateway::types::{AssistantEntry, ToolCall, ToolResult, TranscriptEntry, UserEntry};
use platform::attachments::MessageAttachment;
use platform::storage::sessions::{Message, MessagePart, Role};

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
        let mut t = Self::new(system);
        for msg in messages {
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
        self.entries
            .push(TranscriptEntry::Assistant(AssistantEntry {
                text,
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
            })
        })
        .collect();
    if !tool_calls.is_empty() && !tool_results.is_empty() {
        entries.push(TranscriptEntry::ToolResults(tool_results));
    }
}

fn push_assistant_parts(entries: &mut Vec<TranscriptEntry>, parts: &[MessagePart]) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut tool_results = Vec::new();

    for part in parts {
        match part {
            MessagePart::Text { text: next_text } => {
                if !tool_calls.is_empty() {
                    flush_assistant_turn(entries, &mut text, &mut tool_calls, &mut tool_results);
                }
                text.push_str(next_text);
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
                    });
                }
            }
        }
    }

    if !tool_calls.is_empty() {
        flush_assistant_turn(entries, &mut text, &mut tool_calls, &mut tool_results);
    } else if !text.is_empty() {
        entries.push(TranscriptEntry::Assistant(AssistantEntry {
            text,
            tool_calls: Vec::new(),
        }));
    }
}

fn flush_assistant_turn(
    entries: &mut Vec<TranscriptEntry>,
    text: &mut String,
    tool_calls: &mut Vec<ToolCall>,
    tool_results: &mut Vec<ToolResult>,
) {
    entries.push(TranscriptEntry::Assistant(AssistantEntry {
        text: std::mem::take(text),
        tool_calls: std::mem::take(tool_calls),
    }));
    if !tool_results.is_empty() {
        entries.push(TranscriptEntry::ToolResults(std::mem::take(tool_results)));
    }
}
