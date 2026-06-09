//! 把一段对话转成 Claude Code 会话格式，供 `claude --resume <uuid>` 直接恢复。
//!
//! 纯转换：只读源 session，产出「目标目录名 + 新 session uuid + 逐行 JSON 字符串」，
//! **不落盘**。写文件交给 surface 层——目标在本应用数据目录之外（用户的 claude 目录），
//! 不该由这里的存储层越界写。
//!
//! 目标格式：每行一条记录，靠 `parentUuid → uuid` 串成一条链；assistant 的工具调用
//! 拆成 `assistant` 行的 `tool_use` 块 + 紧跟一条 `user` 行的 `tool_result` 块（与本侧
//! 把结果内联在调用里的存法相反，这里负责拆开配对，否则恢复后首个请求会因 tool_use
//! 缺配对被 API 拒）。

use std::path::Path;

use chrono::{DateTime, SecondsFormat};
use common::AppResult;
use serde_json::{json, Value};
use uuid::Uuid;

use super::sessions::{self, Message, MessagePart, Role};

/// 写进每行的 `version` 字段。恢复方会用自己的版本覆盖，这里只需是个合理值。
const CLAUDE_VERSION: &str = "2.1.156";

/// 转换结果。`lines` 已是逐行 JSON 文本，surface 直接 `join("\n")` 落盘即可。
pub struct ClaudeResumeExport {
    /// 目标项目目录名（cwd 编码后），落在 `<claude>/projects/<dir_name>/`。
    pub dir_name: String,
    /// 解析后的 cwd（绝对路径）。恢复方按当前目录定位会话文件，故恢复命令需先 `cd` 到这里。
    pub cwd: String,
    /// 新会话 uuid，既是文件名也是 `claude --resume <uuid>` 的参数。
    pub session_uuid: String,
    pub lines: Vec<String>,
}

/// 读取本侧某段对话，转成 Claude 会话的逐行记录。
///
/// `include_thinking`：是否把思维链转成 `thinking` 块。本侧不持有恢复方所需的 `signature`，
/// 因此带上思维链时恢复后的首个请求可能被签名校验拒绝——届时关掉本开关重新导出即可。
///
/// `fallback_cwd`：会话无 workdir 时用它兜底。**不能为空**——恢复方按 cwd 编码的目录定位
/// 会话文件，空目录名会让它直接「Failed to resume」。surface 注入一个真实目录（如用户 home）。
pub fn build_claude_resume(
    data_dir: &Path,
    session_id: &str,
    include_thinking: bool,
    fallback_cwd: &Path,
) -> AppResult<ClaudeResumeExport> {
    let session = sessions::load(data_dir, session_id)?;

    let cwd = session
        .workdir
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback_cwd.to_string_lossy().to_string());
    let dir_name = claude_project_dir(&cwd);
    let session_uuid = Uuid::new_v4().to_string();

    let lines = convert_messages(&session.messages, &cwd, &session_uuid, include_thinking)
        .iter()
        .map(Value::to_string)
        .collect();

    Ok(ClaudeResumeExport {
        dir_name,
        cwd,
        session_uuid,
        lines,
    })
}

/// 把消息序列转成 Claude 会话的逐行记录（每行一个 JSON 对象）。纯函数，便于直接测试。
fn convert_messages(
    messages: &[Message],
    cwd: &str,
    session_uuid: &str,
    include_thinking: bool,
) -> Vec<Value> {
    let mut lines: Vec<Value> = Vec::new();
    let mut parent: Option<String> = None;

    let mut push = |typ: &str, message: Value, ts_ms: i64| {
        let uuid = Uuid::new_v4().to_string();
        lines.push(json!({
            "parentUuid": parent,
            "isSidechain": false,
            "userType": "external",
            "cwd": cwd,
            "sessionId": session_uuid,
            "version": CLAUDE_VERSION,
            "gitBranch": "",
            "type": typ,
            "message": message,
            "uuid": uuid,
            "timestamp": iso_millis(ts_ms),
        }));
        parent = Some(uuid);
    };

    for msg in messages {
        match msg.role {
            // 分隔标记 / system_prompt 不进恢复上下文：恢复方用它自己的 system prompt。
            Role::Marker | Role::System => continue,
            Role::User => {
                if msg.content.trim().is_empty() {
                    continue;
                }
                push(
                    "user",
                    json!({ "role": "user", "content": [{ "type": "text", "text": msg.content }] }),
                    msg.created_at,
                );
            }
            Role::Assistant => {
                let (blocks, tool_results) = assistant_blocks(msg, include_thinking);
                if blocks.is_empty() {
                    continue;
                }
                push(
                    "assistant",
                    json!({
                        "id": format!("msg_{}", Uuid::new_v4().to_string().replace('-', "")),
                        "type": "message",
                        "role": "assistant",
                        "model": "",
                        "content": blocks,
                        "stop_reason": "end_turn",
                        "stop_sequence": null,
                        "stop_details": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 }
                    }),
                    msg.created_at,
                );
                if !tool_results.is_empty() {
                    let content: Vec<Value> = tool_results
                        .into_iter()
                        .map(|(id, result)| {
                            json!({ "type": "tool_result", "tool_use_id": id, "content": result })
                        })
                        .collect();
                    push(
                        "user",
                        json!({ "role": "user", "content": content }),
                        msg.created_at,
                    );
                }
            }
        }
    }

    // claude --resume 读取最后一个 last-prompt 行的 leafUuid 定位对话末端，
    // 没有这行会直接报 "Failed to resume"。
    if let Some(leaf_uuid) = lines
        .last()
        .and_then(|l| l["uuid"].as_str())
        .map(String::from)
    {
        lines.push(json!({
            "type": "last-prompt",
            "leafUuid": leaf_uuid,
            "sessionId": session_uuid,
        }));
    }

    lines
}

/// 把 assistant 消息拆成 (content 块数组, 待配对的 tool_result)。
///
/// 优先用 `parts`（有序，能保住思维链/正文/工具调用的先后），`parts` 为空的老消息
/// 回退到 `content` + `tool_calls`。`thinking` 块统一提到最前——API 要求思维链紧跟 turn
/// 开头，不能夹在正文之间。
fn assistant_blocks(msg: &Message, include_thinking: bool) -> (Vec<Value>, Vec<(String, String)>) {
    let mut thinking: Vec<Value> = Vec::new();
    let mut body: Vec<Value> = Vec::new();
    let mut tool_results: Vec<(String, String)> = Vec::new();

    if !msg.parts.is_empty() {
        for part in &msg.parts {
            match part {
                MessagePart::Reasoning { text } => {
                    if include_thinking && !text.trim().is_empty() {
                        // thinking block 需要 API 颁发的 signature，本侧未存储无法伪造；
                        // 包成 thinking 标签的 text block，续聊时上下文仍可见，且不触发签名校验。
                        thinking.push(json!({
                            "type": "text",
                            "text": format!("<thinking>\n{text}\n</thinking>")
                        }));
                    }
                }
                MessagePart::Text { text } => {
                    if !text.trim().is_empty() {
                        body.push(json!({ "type": "text", "text": text }));
                    }
                }
                MessagePart::ToolCall {
                    id,
                    name,
                    input,
                    result,
                    ..
                } => {
                    body.push(
                        json!({ "type": "tool_use", "id": id, "name": name, "input": input }),
                    );
                    tool_results.push((id.clone(), tool_result_text(result)));
                }
            }
        }
    } else {
        if !msg.content.trim().is_empty() {
            body.push(json!({ "type": "text", "text": msg.content }));
        }
        for tc in &msg.tool_calls {
            body.push(
                json!({ "type": "tool_use", "id": tc.id, "name": tc.name, "input": tc.input }),
            );
            tool_results.push((tc.id.clone(), tool_result_text(&tc.result)));
        }
    }

    thinking.extend(body);
    (thinking, tool_results)
}

/// 工具结果文本。结果缺失（运行被中断）也得给个占位，保证每个 `tool_use` 有配对，
/// 否则恢复后的首个请求会被 API 拒。
fn tool_result_text(result: &Option<String>) -> String {
    match result {
        Some(r) => r.clone(),
        None => "[no result: interrupted]".to_string(),
    }
}

/// cwd 编码为 claude 项目目录名：每个非 `[A-Za-z0-9]` 字符替换成 `-`
/// （例 `/Users/x/.app` → `-Users-x--app`）。注意这套规则与本应用自身的 workdir
/// 编码不同（后者保留 `.`），不可互换。
fn claude_project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// 毫秒时间戳 → `2026-05-29T08:04:06.795Z`。非法值回退到 epoch。
fn iso_millis(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| DateTime::from_timestamp_millis(0).unwrap())
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: &str, parts: Vec<MessagePart>) -> Message {
        Message {
            id: Uuid::new_v4().to_string(),
            role,
            content: content.to_string(),
            attachments: vec![],
            tool_calls: vec![],
            parts,
            created_at: 1_780_000_000_000,
            meta: None,
            subagent_call_id: None,
        }
    }

    fn sample() -> Vec<Message> {
        vec![
            msg(Role::Marker, "", vec![]),
            msg(Role::User, "帮我读一下文件", vec![]),
            msg(
                Role::Assistant,
                "先读文件",
                vec![
                    MessagePart::Reasoning {
                        text: "我应该先 Read".into(),
                    },
                    MessagePart::Text {
                        text: "先读文件".into(),
                    },
                    MessagePart::ToolCall {
                        id: "toolu_1".into(),
                        name: "Read".into(),
                        input: json!({ "path": "a.txt" }),
                        arguments: String::new(),
                        result: Some("file body".into()),
                        duration_ms: Some(10),
                    },
                ],
            ),
        ]
    }

    fn convert(include_thinking: bool) -> Vec<Value> {
        convert_messages(&sample(), "/tmp/x", "sess-uuid", include_thinking)
    }

    #[test]
    fn parent_chain_is_contiguous() {
        let lines = convert(true);
        // 过滤掉 last-prompt 等元数据行，只验证 user/assistant 的链式结构
        let msg_lines: Vec<&Value> = lines
            .iter()
            .filter(|l| matches!(l["type"].as_str(), Some("user") | Some("assistant")))
            .collect();
        assert_eq!(msg_lines.first().unwrap()["parentUuid"], Value::Null);
        for w in msg_lines.windows(2) {
            assert_eq!(w[1]["parentUuid"], w[0]["uuid"], "链断裂");
        }
        // marker 被跳过：user + assistant + tool_result = 3 行，另有 1 行 last-prompt
        assert_eq!(msg_lines.len(), 3);
        assert_eq!(lines.len(), 4);

        // last-prompt 指向最后一条消息
        let last_prompt = lines
            .iter()
            .rev()
            .find(|l| l["type"] == "last-prompt")
            .unwrap();
        assert_eq!(last_prompt["leafUuid"], msg_lines.last().unwrap()["uuid"]);
    }

    #[test]
    fn every_tool_use_has_a_matching_tool_result() {
        let lines = convert(true);
        let mut uses = vec![];
        let mut results = vec![];
        for l in &lines {
            for b in l["message"]["content"].as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("tool_use") => uses.push(b["id"].as_str().unwrap().to_string()),
                    Some("tool_result") => {
                        results.push(b["tool_use_id"].as_str().unwrap().to_string())
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(uses, vec!["toolu_1".to_string()]);
        assert_eq!(results, uses, "每个 tool_use 必须有配对 tool_result");
    }

    #[test]
    fn thinking_toggle_controls_thinking_block() {
        // thinking 内容以 <thinking> text block 形式导出（非 thinking 类型块，避免 API signature 校验）
        let has_thinking_text = |include: bool| {
            convert(include).iter().any(|l| {
                l["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|b| {
                        b["type"] == "text"
                            && b["text"].as_str().unwrap_or("").contains("<thinking>")
                    })
            })
        };
        assert!(has_thinking_text(true), "开启时应有 thinking 内容");
        assert!(!has_thinking_text(false), "关闭时不应有 thinking 内容");
    }

    #[test]
    fn claude_project_dir_replaces_non_alnum() {
        assert_eq!(claude_project_dir("/Users/x/.app"), "-Users-x--app");
        assert_eq!(claude_project_dir("/a/b_c"), "-a-b-c");
    }
}
