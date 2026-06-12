//! 子 NestedRun（subagent）过程累积（架构 §4.4.11.8）。
//!
//! 三个 surface observer（desktop chat / heb CLI daemon / hebweb）共用：按 `subagent_call_id`
//! 把子事件（子文本 / 子推理 / 子工具调用）累积成 [`MessagePart`] 序列，run 结束由 [`NestedAccumulator::sync_into`]
//! 同步进父 `tool_calls` 里对应 Task call 的 `nested`，随父 message 落**主** session.jsonl。
//! 修复「子过程只活在内存事件流、run 一结束就蒸发」（旧实现 nested 只在前端 streaming 软状态、
//! 不落盘的根因）。

use std::collections::BTreeMap;

use protocol::EventPayload;

use super::sessions::{MessagePart, MessageToolCall};

/// 按 `subagent_call_id` 累积子过程。各 surface observer 持有一个，子事件喂 [`record`]，
/// 落盘前 [`sync_into`] 写进父 tool_calls。
///
/// [`record`]: NestedAccumulator::record
/// [`sync_into`]: NestedAccumulator::sync_into
#[derive(Default)]
pub struct NestedAccumulator {
    by_call: BTreeMap<String, Vec<MessagePart>>,
}

impl NestedAccumulator {
    /// 累积一个子事件（payload 来自带 `subagent_call_id` 的 Event）。与子过程无关的 payload 忽略。
    pub fn record(&mut self, call_id: &str, payload: &EventPayload) {
        let parts = self.by_call.entry(call_id.to_string()).or_default();
        match payload {
            EventPayload::TextDelta { text } => push_text(parts, text),
            EventPayload::Reasoning { text } => push_reasoning(parts, text),
            EventPayload::ToolCallStarted {
                call_id, name, input, ..
            } => parts.push(MessagePart::ToolCall {
                id: call_id.clone(),
                name: name.clone(),
                input: input.clone(),
                arguments: String::new(),
                result: None,
                duration_ms: None,
                is_error: false,
            }),
            EventPayload::ToolCallFinished {
                call_id,
                result,
                duration_ms,
                is_error,
                ..
            } => {
                // 按 call_id 找到对应未完成的子 tool_call part，回填结果。
                for part in parts.iter_mut().rev() {
                    if let MessagePart::ToolCall {
                        id,
                        result: r,
                        duration_ms: d,
                        is_error: e,
                        ..
                    } = part
                    {
                        if id == call_id && r.is_none() {
                            *r = Some(result.clone());
                            *d = Some(*duration_ms);
                            *e = *is_error;
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// 把累积的子过程同步进 `tool_calls` 里对应 Task call（id == subagent_call_id）的 `nested`。
    /// 每次子事件后调用，保证落盘走任意路径时 Task call 都已带最新子过程。
    pub fn sync_into(&self, tool_calls: &mut [MessageToolCall]) {
        for (call_id, parts) in &self.by_call {
            if let Some(call) = tool_calls.iter_mut().find(|c| &c.id == call_id) {
                call.nested.clone_from(parts);
            }
        }
    }
}

fn push_text(parts: &mut Vec<MessagePart>, text: &str) {
    if let Some(MessagePart::Text { text: t }) = parts.last_mut() {
        t.push_str(text);
    } else {
        parts.push(MessagePart::Text {
            text: text.to_string(),
        });
    }
}

fn push_reasoning(parts: &mut Vec<MessagePart>, text: &str) {
    if let Some(MessagePart::Reasoning { text: t }) = parts.last_mut() {
        t.push_str(text);
    } else {
        parts.push(MessagePart::Reasoning {
            text: text.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task_call(id: &str) -> MessageToolCall {
        MessageToolCall {
            id: id.to_string(),
            name: "Task".to_string(),
            input: json!({"subagent_type": "explore"}),
            result: None,
            duration_ms: None,
            is_error: false,
            nested: Vec::new(),
        }
    }

    #[test]
    fn accumulates_child_text_and_tool_then_syncs_into_task_call() {
        let mut acc = NestedAccumulator::default();
        let cid = "T1";
        acc.record(cid, &EventPayload::TextDelta { text: "找".into() });
        acc.record(cid, &EventPayload::TextDelta { text: "到了".into() });
        acc.record(
            cid,
            &EventPayload::ToolCallStarted {
                index: 0,
                call_id: "c1".into(),
                name: "Bash".into(),
                input: json!({"command": "ls"}),
            },
        );
        acc.record(
            cid,
            &EventPayload::ToolCallFinished {
                index: 0,
                call_id: "c1".into(),
                result: "a.txt".into(),
                duration_ms: 5,
                truncated: false,
                artifact_path: None,
                is_error: false,
            },
        );

        let mut calls = vec![task_call("T1"), task_call("T2")];
        acc.sync_into(&mut calls);

        // T1 拿到子过程：相邻文本合并成一段 + 子工具（带结果）。
        assert_eq!(calls[0].nested.len(), 2);
        match &calls[0].nested[0] {
            MessagePart::Text { text } => assert_eq!(text, "找到了"),
            other => panic!("expected Text, got {other:?}"),
        }
        match &calls[0].nested[1] {
            MessagePart::ToolCall { name, result, .. } => {
                assert_eq!(name, "Bash");
                assert_eq!(result.as_deref(), Some("a.txt"));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        // T2 无子事件，nested 保持空。
        assert!(calls[1].nested.is_empty());
    }
}
