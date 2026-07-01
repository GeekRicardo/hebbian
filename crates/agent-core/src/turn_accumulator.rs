//! 把一个 Run 的 [`Event`] 流累积成一条 assistant [`Message`]（架构 §4.3 / §4.9）。
//!
//! 历史上 desktop / hebweb / heb CLI 各写了一份"从事件流重建 assistant message"的逻辑
//! 且几乎逐字节雷同（处理 Reasoning / TextDelta / TextDone / ToolCall* 同一套规则）。
//! 这里收敛成唯一一份 [`AssistantAccumulator`]，三 surface 的 `TurnObserver` 内部复用它，
//! 各自只保留"事件投递差异"（Channel / WS broadcast / NDJSON）。
//!
//! 累积分两路（与 desktop 历史实现一致，刻意冗余以保证中断也能落盘）：
//! - `parts`：流式 `MessagePart` 序列（Text / Reasoning / ToolCall），支持 `ToolCallDelta`
//!   按 `index` + `id` 去重定位增量拼接——前端 detail 视图 / diff 依赖它的有序性。
//! - `tool_calls`：扁平 [`MessageToolCall`] 列表，按 `index` resize 填充，保证流式中断时
//!   工具调用仍能落盘（不依赖 parts 的完整性）。
//!
//! `build` 时若已有 `TextDone` 的最终全文，用它收尾补齐尾部 Text。

use std::collections::HashMap;

use protocol::{Event, EventPayload};
use serde_json::Value;

use crate::storage::sessions::{Message, MessagePart, MessageToolCall, Role};

/// 从 Run 事件流累积出一条 assistant [`Message`]。
///
/// 用法：每个事件 `acc.on_event(&event)`，Run 结束 `acc.build()` 取 message（无内容返回 `None`）。
#[derive(Default)]
pub struct AssistantAccumulator {
    parts: Vec<MessagePart>,
    /// parts 中 ToolCall 的定位索引：按事件里的 `index` / `id` 找回已建的那条。
    by_index: HashMap<usize, usize>,
    by_id: HashMap<String, usize>,
    /// 扁平 tool_calls（中断兜底，与 parts 并行维护）。
    tool_calls: Vec<MessageToolCall>,
    /// `TextDone` 给的最终全文，build 时用于补齐尾部 Text。
    final_text: String,
    /// 首个内容事件到达时刻（Unix ms），用于 run 耗时统计。
    started_at: Option<i64>,
}

impl AssistantAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 消费一个事件，更新累积状态。非内容事件（生命周期 / 审批等）忽略。
    pub fn on_event(&mut self, event: &Event) {
        self.mark_started(&event.payload);
        self.record_part(&event.payload);
        self.record_tool(&event.payload);
    }

    /// 当前已累积的纯文本快照（仅 Text part 拼接）。
    pub fn text_snapshot(&self) -> String {
        text_from_parts(&self.parts)
    }

    /// 首个内容事件时刻（Unix ms），无内容则 `None`。
    pub fn started_at(&self) -> Option<i64> {
        self.started_at
    }

    /// 收尾产出 assistant message。无任何文本 / 工具调用时返回 `None`（不该落一条空消息）。
    pub fn build(mut self) -> Option<Message> {
        self.append_final_text_if_missing();
        let content = text_from_parts(&self.parts);
        if content.is_empty() && self.tool_calls.is_empty() && self.parts.is_empty() {
            return None;
        }
        // created_at = 该 message 内容「真实产生时刻」（首个内容事件到达时），
        // 不是落盘时刻（架构 §4.9.5 消息顺序契约）。否则一段早就流式输出的 assistant，
        // 落盘时刻晚于 turn 末尾的 goal marker / 流式途中插队的 user，sort 后倒挂。
        let created_at = self
            .started_at
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        Some(Message {
            id: crate::storage::sessions::new_id(),
            role: Role::Assistant,
            content,
            attachments: Vec::new(),
            tool_calls: self.tool_calls,
            parts: self.parts,
            created_at,
            meta: None,
            subagent_call_id: None,
            run_duration_ms: None,
        })
    }

    /// 解构取出 parts + tool_calls，供调用方自行组装 message（如需要保留特殊 meta）。
    pub fn into_parts(mut self) -> (Vec<MessagePart>, Vec<MessageToolCall>) {
        self.append_final_text_if_missing();
        (self.parts, self.tool_calls)
    }

    // ── 内部 ──────────────────────────────────────────────────────────────

    fn mark_started(&mut self, payload: &EventPayload) {
        if self.started_at.is_none()
            && matches!(
                payload,
                EventPayload::Reasoning { .. }
                    | EventPayload::ReasoningDuration { .. }
                    | EventPayload::TextDelta { .. }
                    | EventPayload::TextDone { .. }
                    | EventPayload::ToolCallStarted { .. }
                    | EventPayload::ToolCallFinished { .. }
            )
        {
            self.started_at = Some(chrono::Utc::now().timestamp_millis());
        }
    }

    fn record_part(&mut self, payload: &EventPayload) {
        match payload {
            EventPayload::TextDelta { text } => self.append_text(text),
            EventPayload::Reasoning { text } => self.append_reasoning(text),
            EventPayload::ReasoningDuration { ms } => self.set_reasoning_duration(*ms),
            EventPayload::TextDone { full_text } => self.final_text = full_text.clone(),
            EventPayload::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => self.apply_tool_delta(
                *index,
                id.as_deref(),
                name.as_deref(),
                arguments_delta.as_deref(),
            ),
            EventPayload::ToolCallStarted {
                index,
                call_id,
                name,
                input,
            } => self.start_tool(*index, call_id, name, input.clone()),
            EventPayload::ToolCallFinished {
                index,
                call_id,
                result,
                duration_ms,
                is_error,
                ..
            } => self.finish_tool(*index, call_id, result, *duration_ms, *is_error),
            _ => {}
        }
    }

    fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.parts.last_mut() {
            Some(MessagePart::Text { text: existing }) => existing.push_str(text),
            _ => self.parts.push(MessagePart::Text {
                text: text.to_string(),
            }),
        }
    }

    fn append_reasoning(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.parts.last_mut() {
            Some(MessagePart::Reasoning { text: existing, .. }) => existing.push_str(text),
            _ => self.parts.push(MessagePart::Reasoning {
                text: text.to_string(),
                duration_ms: None,
            }),
        }
    }

    /// 把思考时长写进当前的 Reasoning part。若最后一段不是 Reasoning（OAuth 直连官方时
    /// thinking 文本被清空、没产生任何 Reasoning delta），就补一个空文本 part 承载时长——
    /// UI 据此显示「思考用时 N 秒」，哪怕没有可展开的思考正文。
    fn set_reasoning_duration(&mut self, ms: u64) {
        match self.parts.last_mut() {
            Some(MessagePart::Reasoning { duration_ms, .. }) => *duration_ms = Some(ms),
            _ => self.parts.push(MessagePart::Reasoning {
                text: String::new(),
                duration_ms: Some(ms),
            }),
        }
    }

    fn append_final_text_if_missing(&mut self) {
        if self.final_text.is_empty() {
            return;
        }
        let current = text_from_parts(&self.parts);
        if !current.ends_with(&self.final_text) {
            let final_text = std::mem::take(&mut self.final_text);
            self.append_text(&final_text);
        }
    }

    fn apply_tool_delta(
        &mut self,
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        arguments_delta: Option<&str>,
    ) {
        let pos = self.tool_position(index, id, name);
        if let MessagePart::ToolCall {
            id: existing_id,
            name: existing_name,
            arguments,
            ..
        } = &mut self.parts[pos]
        {
            if let Some(next_id) = id.filter(|v| !v.trim().is_empty()) {
                *existing_id = next_id.to_string();
                self.by_id.insert(next_id.to_string(), pos);
            }
            if let Some(next_name) = name.filter(|v| !v.trim().is_empty()) {
                *existing_name = next_name.to_string();
            }
            if let Some(delta) = arguments_delta.filter(|v| !v.is_empty()) {
                arguments.push_str(delta);
            }
        }
    }

    fn start_tool(&mut self, index: usize, call_id: &str, name: &str, input: Value) {
        let pos = self.tool_position(index, Some(call_id), Some(name));
        if let MessagePart::ToolCall {
            id,
            name: existing_name,
            input: existing_input,
            arguments,
            ..
        } = &mut self.parts[pos]
        {
            *id = call_id.to_string();
            *existing_name = name.to_string();
            *existing_input = input.clone();
            if arguments.is_empty() {
                *arguments = input.to_string();
            }
            self.by_id.insert(call_id.to_string(), pos);
        }
        // 扁平 tool_calls 兜底。
        upsert_tool_call(&mut self.tool_calls, index, call_id, name, input);
    }

    fn finish_tool(
        &mut self,
        index: usize,
        call_id: &str,
        result: &str,
        duration_ms: u64,
        is_error: bool,
    ) {
        let pos = self.tool_position(index, Some(call_id), None);
        if let MessagePart::ToolCall {
            id,
            result: existing_result,
            duration_ms: existing_duration_ms,
            is_error: existing_is_error,
            ..
        } = &mut self.parts[pos]
        {
            *id = call_id.to_string();
            *existing_result = Some(result.to_string());
            *existing_duration_ms = Some(duration_ms);
            *existing_is_error = is_error;
            self.by_id.insert(call_id.to_string(), pos);
        }
        // 扁平 tool_calls 兜底。
        if self.tool_calls.len() <= index {
            self.tool_calls.resize_with(index + 1, empty_tool_call);
        }
        let call = &mut self.tool_calls[index];
        call.id = call_id.to_string();
        call.result = Some(result.to_string());
        call.duration_ms = Some(duration_ms);
        call.is_error = is_error;
    }

    /// 找回（或新建）`index` / `id` 对应的 ToolCall part 位置。
    fn tool_position(&mut self, index: usize, id: Option<&str>, name: Option<&str>) -> usize {
        let incoming_id = id.filter(|v| !v.trim().is_empty());

        // call_id 是 tool_call 的稳定全局身份：见过同 id 直接复用那条 part。
        if let Some(pos) = incoming_id.and_then(|v| self.by_id.get(v).copied()) {
            self.by_index.entry(index).or_insert(pos);
            return pos;
        }

        // index 只是 id 到达前的 streaming fallback。命中的 part 若已绑定了另一个
        // call_id，说明上游把不同 tool_call 的 index 撞到了一起，此时绝不能复用——
        // 否则新 tool_call 会把旧的覆盖、旧 part 被静默丢失。
        if let Some(pos) = self.by_index.get(&index).copied() {
            let collides_with_other = incoming_id.is_some_and(|incoming| {
                matches!(&self.parts[pos],
                    MessagePart::ToolCall { id, .. } if !id.trim().is_empty() && id != incoming)
            });
            if !collides_with_other {
                return pos;
            }
        }

        let pos = self.parts.len();
        let clean_id = incoming_id.unwrap_or_default().to_string();
        self.parts.push(MessagePart::ToolCall {
            id: clean_id.clone(),
            name: name
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_default()
                .to_string(),
            input: serde_json::json!({}),
            arguments: String::new(),
            result: None,
            duration_ms: None,
            is_error: false,
        });
        self.by_index.insert(index, pos);
        if !clean_id.is_empty() {
            self.by_id.insert(clean_id, pos);
        }
        pos
    }

    /// 扁平 tool_calls 的 delta 兜底（id / name 增量）——与 parts 并行维护。
    fn record_tool(&mut self, payload: &EventPayload) {
        if let EventPayload::ToolCallDelta {
            index, id, name, ..
        } = payload
        {
            if self.tool_calls.len() <= *index {
                self.tool_calls.resize_with(*index + 1, empty_tool_call);
            }
            let call = &mut self.tool_calls[*index];
            if let Some(id) = id {
                call.id.clone_from(id);
            }
            if let Some(name) = name {
                call.name.clone_from(name);
            }
        }
    }
}

fn text_from_parts(parts: &[MessagePart]) -> String {
    let mut out = String::new();
    for part in parts {
        if let MessagePart::Text { text } = part {
            out.push_str(text);
        }
    }
    out
}

fn upsert_tool_call(
    calls: &mut Vec<MessageToolCall>,
    index: usize,
    call_id: &str,
    name: &str,
    input: Value,
) {
    if calls.len() <= index {
        calls.resize_with(index + 1, empty_tool_call);
    }
    let call = &mut calls[index];
    call.id = call_id.to_string();
    call.name = name.to_string();
    call.input = input;
}

fn empty_tool_call() -> MessageToolCall {
    MessageToolCall {
        id: String::new(),
        name: String::new(),
        input: serde_json::json!({}),
        result: None,
        duration_ms: None,
        is_error: false,
        nested: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::RunId;

    fn ev(payload: EventPayload) -> Event {
        Event::now(RunId::new(), 0, payload)
    }

    #[test]
    fn accumulates_text_and_reasoning_in_order() {
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::Reasoning {
            text: "想一下".into(),
        }));
        acc.on_event(&ev(EventPayload::TextDelta {
            text: "你好".into(),
        }));
        acc.on_event(&ev(EventPayload::TextDelta {
            text: "世界".into(),
        }));
        let msg = acc.build().unwrap();
        assert_eq!(msg.content, "你好世界");
        assert_eq!(msg.parts.len(), 2);
        assert!(matches!(msg.parts[0], MessagePart::Reasoning { .. }));
        assert!(matches!(msg.parts[1], MessagePart::Text { .. }));
    }

    #[test]
    fn reasoning_duration_attaches_to_thinking_part() {
        // 有 thinking 文本时：时长写进同一个 Reasoning part，不另起一段。
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::Reasoning {
            text: "推理过程".into(),
        }));
        acc.on_event(&ev(EventPayload::ReasoningDuration { ms: 4500 }));
        acc.on_event(&ev(EventPayload::TextDelta {
            text: "答案".into(),
        }));
        let msg = acc.build().unwrap();
        assert_eq!(msg.parts.len(), 2);
        match &msg.parts[0] {
            MessagePart::Reasoning { text, duration_ms } => {
                assert_eq!(text, "推理过程");
                assert_eq!(*duration_ms, Some(4500));
            }
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_duration_without_text_creates_empty_part() {
        // OAuth 直连官方：thinking 文本被清空，全程没有 Reasoning delta，只有时长。
        // 必须凭空补一个空文本 Reasoning part 承载时长，UI 才能显示「思考用时 N 秒」。
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::ReasoningDuration { ms: 4500 }));
        acc.on_event(&ev(EventPayload::TextDelta {
            text: "答案".into(),
        }));
        let msg = acc.build().unwrap();
        assert_eq!(msg.parts.len(), 2);
        match &msg.parts[0] {
            MessagePart::Reasoning { text, duration_ms } => {
                assert!(text.is_empty(), "OAuth 场景 thinking 文本应为空");
                assert_eq!(*duration_ms, Some(4500));
            }
            other => panic!("expected empty Reasoning carrying duration, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_started_then_finished_merges() {
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::ToolCallStarted {
            index: 0,
            call_id: "c1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
        }));
        acc.on_event(&ev(EventPayload::ToolCallFinished {
            index: 0,
            call_id: "c1".into(),
            result: "out".into(),
            duration_ms: 7,
            truncated: false,
            artifact_path: None,
            is_error: false,
        }));
        let msg = acc.build().unwrap();
        assert_eq!(msg.tool_calls.len(), 1);
        let tc = &msg.tool_calls[0];
        assert_eq!(tc.id, "c1");
        assert_eq!(tc.name, "Bash");
        assert_eq!(tc.result.as_deref(), Some("out"));
        assert_eq!(tc.duration_ms, Some(7));
        // parts 里也有同一条 ToolCall。
        assert!(msg
            .parts
            .iter()
            .any(|p| matches!(p, MessagePart::ToolCall { id, .. } if id == "c1")));
    }

    #[test]
    fn tool_delta_then_started_dedups_by_index_id() {
        let mut acc = AssistantAccumulator::new();
        // 流式：先 delta 拼参数，再 started 落定，不该产生两条。
        acc.on_event(&ev(EventPayload::ToolCallDelta {
            index: 0,
            id: Some("c1".into()),
            name: Some("Edit".into()),
            arguments_delta: Some("{\"file".into()),
        }));
        acc.on_event(&ev(EventPayload::ToolCallStarted {
            index: 0,
            call_id: "c1".into(),
            name: "Edit".into(),
            input: serde_json::json!({"file_path": "/x"}),
        }));
        let msg = acc.build().unwrap();
        let tool_parts = msg
            .parts
            .iter()
            .filter(|p| matches!(p, MessagePart::ToolCall { .. }))
            .count();
        assert_eq!(tool_parts, 1, "同一 index+id 不应产生多条 ToolCall part");
    }

    #[test]
    fn colliding_index_with_distinct_ids_keeps_both_tool_parts() {
        // 上游（如 adapter 失误透传浮动 block index）把两个不同 call_id 的 tool_call
        // 撞到同一个 index：by_index 命中旧 part 但 id 不同，必须新建独立 part，
        // 不能复用旧 part 把它的 id 覆盖掉，否则旧 tool_call 被静默丢失。
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::ToolCallDelta {
            index: 1,
            id: Some("call_a".into()),
            name: Some("tool_a".into()),
            arguments_delta: Some("{}".into()),
        }));
        acc.on_event(&ev(EventPayload::ToolCallDelta {
            index: 1,
            id: Some("call_b".into()),
            name: Some("tool_b".into()),
            arguments_delta: Some("{}".into()),
        }));
        let msg = acc.build().unwrap();
        let tool_names: Vec<&str> = msg
            .parts
            .iter()
            .filter_map(|p| match p {
                MessagePart::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            tool_names,
            vec!["tool_a", "tool_b"],
            "撞 index 的不同 call_id 应各自落一条 part"
        );
    }

    #[test]
    fn text_done_finalizes_when_stream_incomplete() {
        // 流式没吐任何 TextDelta，只有 TextDone 给全文：build 时用 final_text 补齐。
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::TextDone {
            full_text: "完整回复".into(),
        }));
        let msg = acc.build().unwrap();
        assert_eq!(msg.content, "完整回复");
    }

    #[test]
    fn text_done_no_double_append_when_stream_complete() {
        // 常态：TextDelta 已累积完整全文，TextDone 全文与之一致，不重复追加。
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::TextDelta {
            text: "完整回复".into(),
        }));
        acc.on_event(&ev(EventPayload::TextDone {
            full_text: "完整回复".into(),
        }));
        let msg = acc.build().unwrap();
        assert_eq!(msg.content, "完整回复");
    }

    #[test]
    fn empty_run_yields_none() {
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::RunStarted {
            agent: protocol::AgentRef("default".into()),
            parent: None,
        }));
        assert!(acc.build().is_none());
    }

    #[test]
    fn started_at_set_on_first_content() {
        let mut acc = AssistantAccumulator::new();
        assert!(acc.started_at().is_none());
        acc.on_event(&ev(EventPayload::TextDelta { text: "x".into() }));
        assert!(acc.started_at().is_some());
    }

    /// 回归（架构 §4.9.5 消息顺序契约）：build 出的 message.created_at 必须是「首个内容
    /// 事件到达时刻」，不是「落盘/build 时刻」。否则一段早就流式输出的 assistant 会因
    /// created_at 虚高，sort 后倒挂到 turn 末尾的 goal marker / 系统通知之后。
    ///
    /// A/B 翻转：build() 用 `started_at` 时本测试 pass；改回 `Utc::now()` 必 fail
    /// （created_at 会等于 build 时刻，>= after_first_event）。
    #[test]
    fn created_at_is_first_content_time_not_build_time() {
        let mut acc = AssistantAccumulator::new();
        acc.on_event(&ev(EventPayload::TextDelta { text: "首".into() }));
        let after_first_event = chrono::Utc::now().timestamp_millis();
        // 拉开首事件与 build 的时间差：用了落盘时刻会显著大于 after_first_event。
        std::thread::sleep(std::time::Duration::from_millis(15));
        acc.on_event(&ev(EventPayload::TextDelta { text: "尾".into() }));
        let msg = acc.build().unwrap();
        assert!(
            msg.created_at <= after_first_event,
            "created_at({}) 应锁定在首事件时刻(<= {})，而非 build 落盘时刻",
            msg.created_at,
            after_first_event
        );
    }
}
