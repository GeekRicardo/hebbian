//! 模型 IO 调试 dump：把每次模型请求的完整入参 + 响应按 jsonl 写到磁盘。
//!
//! **默认开启**：每个 session 都会落盘 model_io.jsonl，为前端"Model I/O 调试器"
//! 提供数据源。开销极小（每个 turn 一行 jsonl，attachments 不写正文）。
//! 用 `HEBBIAN_DUMP_MODEL_IO=0` / `=false` 显式禁用。
//!
//! 文件位置：`<data_dir>/sessions/<session_id>/model_io.jsonl`，与 session 的其它工件
//! `tool_results/` `compactions/` `plans/` `partial/` 同级，遵循架构 §4.9.1：
//! 一段对话所有文件落在 `<sid>/` 目录内。
//!
//! 设计与 [`Recorder`] 同构：actor 模式，clone 廉价（只复 `Sender`），
//! 后台 writer task 异步落盘，主 loop 不被 IO 阻塞。
//!
//! 序列化策略：
//! - `system` / `text` / `tool input/output` 这类文本字段原样保留。
//! - `attachments` 不写 base64 数据，只写元数据（kind + media_type + size），
//!   避免 dump 文件爆炸。
//! - 调用失败时 response 字段写 `{"type": "Error", "error": "..."}`，
//!   不丢弃 request 上下文，便于排查 provider 错误。
//!
//! [`Recorder`]: crate::recorder::Recorder

use std::path::{Path, PathBuf};

use chrono::Utc;
use common::attachments::MessageAttachment;
use model_gateway::types::{
    AssistantEntry, ModelError, ModelRequest, ModelResponse, ToolCall, ToolResult, TranscriptEntry,
    UserEntry,
};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

/// 控制本功能的环境变量。
///
/// - 未设置：**默认启用**，dump 写到 `<data_dir>/sessions/<session_id>/model_io.jsonl`。
/// - `0` / `false` / `off` / `no`（大小写无关）：显式禁用。
/// - 其它非空值：启用。
pub const ENV_VAR: &str = "HEBBIAN_DUMP_MODEL_IO";

/// 是否启用 dump。surface 用它决定要不要构造 [`ModelIoDump`]。
///
/// 调试器的可用性比微小开销更重要——bug 出现时再去开环境变量已经晚了，
/// 所以默认开。
pub fn is_enabled() -> bool {
    match std::env::var(ENV_VAR) {
        Ok(v) => {
            let trimmed = v.trim().to_ascii_lowercase();
            !matches!(trimmed.as_str(), "0" | "false" | "off" | "no")
        }
        Err(_) => true,
    }
}

/// 默认路径：`<data_dir>/sessions/<session_id>/model_io.jsonl`。
///
/// 落在 session 目录内（与 `tool_results/` 等同级），避免污染 `sessions/` 根目录、
/// 也避开 legacy migration 把平铺 `.jsonl` 误当作老 session 迁移的坑。
pub fn default_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("sessions")
        .join(session_id)
        .join("model_io.jsonl")
}

/// 检查 [`ENV_VAR`]：开启则按 [`default_path`] 打开一份 dump，失败仅记 trace 不传播。
/// CLI / desktop 启动时调用。
pub async fn open_for_session_if_enabled(data_dir: &Path, session_id: &str) -> Option<ModelIoDump> {
    if !is_enabled() {
        return None;
    }
    let path = default_path(data_dir, session_id);
    match ModelIoDump::open(&path).await {
        Ok(dump) => {
            tracing::info!(path = %path.display(), "model IO dump enabled");
            Some(dump)
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "model IO dump open failed");
            None
        }
    }
}

/// 一对模型请求 / 响应记录。jsonl 里每行一条这样的对象。
#[derive(Debug, Clone, Serialize)]
pub struct DumpEntry {
    /// ISO-8601 时间戳（UTC）。
    pub ts: String,
    pub run_id: String,
    pub turn: u32,
    /// 实际发往 provider 的模型名（`ModelClient::set_model` 决定）。
    pub model: String,
    pub request: Value,
    pub response: Value,
    /// 调用耗时（毫秒）。
    pub duration_ms: u64,
    /// 调用类别：`"main"` 主模型调用 / `"judge"` AutoMode 判官 / `"compaction"` 自动压缩摘要。
    /// 前端 ModelIoInspector 据此渲染区分标签。
    pub kind: String,
}

enum DumpCmd {
    Write(DumpEntry),
    Flush(oneshot::Sender<std::io::Result<()>>),
}

/// jsonl 模型 IO 持久化的句柄。Clone 是廉价的（只复制 `Sender`）。
#[derive(Clone)]
pub struct ModelIoDump {
    tx: mpsc::Sender<DumpCmd>,
    path: PathBuf,
}

impl ModelIoDump {
    /// 打开（创建或追加）一份 jsonl 文件。父目录会自动创建。
    pub async fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        let (tx, mut rx) = mpsc::channel::<DumpCmd>(1024);
        let writer_path = path.clone();
        tokio::spawn(async move {
            // 上一条 main entry 的 messages 数组——用于前缀去重。
            // judge / compaction 不参与（它们的 request 结构不同）。
            let mut prev_main_messages: Vec<Value> = Vec::new();

            while let Some(cmd) = rx.recv().await {
                match cmd {
                    DumpCmd::Write(mut entry) => {
                        // 对 main entry 做 messages 前缀去重
                        if entry.kind == "main" {
                            Self::dedup_messages(&mut entry.request, &mut prev_main_messages);
                        }
                        match serde_json::to_string(&entry) {
                            Ok(line) => {
                                if let Err(e) = file.write_all(line.as_bytes()).await {
                                    warn!(error = %e, path = %writer_path.display(), "model_io_dump write");
                                    continue;
                                }
                                if let Err(e) = file.write_all(b"\n").await {
                                    warn!(error = %e, path = %writer_path.display(), "model_io_dump newline");
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "model_io_dump serialize");
                            }
                        }
                    }
                    DumpCmd::Flush(reply) => {
                        let _ = reply.send(file.flush().await);
                    }
                }
            }
        });

        Ok(Self { tx, path })
    }

    /// fire-and-forget 写一条记录。失败仅记 trace，不向调用方传播。
    /// 通道满时丢弃并打 warn——dump 是调试用，best-effort。
    pub fn record(&self, entry: DumpEntry) {
        if let Err(e) = self.tx.try_send(DumpCmd::Write(entry)) {
            warn!(error = %e, "model_io_dump queue full, dropping entry");
        }
    }

    /// 等待写队列排空到磁盘。run 结束时可选调用。
    pub async fn flush(&self) -> std::io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DumpCmd::Flush(tx))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "dump closed"))?;
        rx.await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "dump dropped"))?
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 对 main entry 的 `request.messages` 做前缀去重：
    /// 与上一条 main entry 的 messages 比较，相同前缀替换为
    /// `{"messages_carried": N, "messages_new": [新增部分]}`。
    ///
    /// 读取侧顺序扫描时累积 messages 即可重建完整数组。
    fn dedup_messages(request: &mut Value, prev: &mut Vec<Value>) {
        let messages = match request.get("messages").and_then(|m| m.as_array()) {
            Some(arr) => arr.clone(),
            None => {
                *prev = Vec::new();
                return;
            }
        };

        let mut carried = 0usize;
        let max = std::cmp::min(prev.len(), messages.len());
        while carried < max && prev[carried] == messages[carried] {
            carried += 1;
        }

        let new_msgs: Vec<Value> = messages[carried..].to_vec();

        // 更新 prev 为当前完整 messages
        *prev = messages;

        // 替换 request 中的 messages 字段
        if let Some(obj) = request.as_object_mut() {
            obj.remove("messages");
            obj.insert("messages_carried".to_string(), Value::from(carried as u64));
            obj.insert("messages_new".to_string(), Value::Array(new_msgs));
        }
    }
}

/// 把 [`ModelRequest`] 序列化成精简 JSON：保留所有文本字段，attachments 只留元数据。
pub fn request_to_json(req: &ModelRequest, model_name: &str) -> Value {
    let mut messages = Vec::with_capacity(req.entries.len());
    for entry in &req.entries {
        messages.push(transcript_entry_to_json(entry));
    }
    json!({
        "model": model_name,
        "system": req.system,
        "messages": messages,
        "tools": req.tools,
        "max_tokens": req.max_tokens,
        "reasoning": req.reasoning,
    })
}

/// 把 [`ModelResponse`] 序列化。`Err` 走 `{"type": "Error", "error": ...}` 分支。
pub fn response_to_json(resp: &Result<ModelResponse, ModelError>) -> Value {
    match resp {
        Ok(ModelResponse::Done {
            text,
            reasoning,
            attachments,
            usage,
            finish,
            reasoning_signature: _,
        }) => json!({
            "type": "Done",
            "text": text,
            "reasoning": reasoning,
            "attachments": attachments.iter().map(attachment_meta).collect::<Vec<_>>(),
            "usage": usage,
            "finish": format!("{finish:?}"),
        }),
        Ok(ModelResponse::ToolCalls {
            text,
            reasoning,
            calls,
            attachments,
            usage,
            reasoning_signature: _,
        }) => json!({
            "type": "ToolCalls",
            "text": text,
            "reasoning": reasoning,
            "calls": calls.iter().map(tool_call_to_json).collect::<Vec<_>>(),
            "attachments": attachments.iter().map(attachment_meta).collect::<Vec<_>>(),
            "usage": usage,
        }),
        Err(e) => json!({
            "type": "Error",
            "error": e.to_string(),
        }),
    }
}

fn transcript_entry_to_json(entry: &TranscriptEntry) -> Value {
    match entry {
        TranscriptEntry::User(UserEntry { text, attachments }) => json!({
            "role": "user",
            "content": text,
            "attachments": attachments.iter().map(attachment_meta).collect::<Vec<_>>(),
        }),
        TranscriptEntry::Assistant(AssistantEntry {
            text,
            reasoning,
            tool_calls,
            ..
        }) => json!({
            "role": "assistant",
            "content": text,
            "reasoning": reasoning,
            "tool_calls": tool_calls.iter().map(tool_call_to_json).collect::<Vec<_>>(),
        }),
        TranscriptEntry::ToolResults(results) => json!({
            "role": "tool",
            "results": results.iter().map(tool_result_to_json).collect::<Vec<_>>(),
        }),
    }
}

fn tool_call_to_json(call: &ToolCall) -> Value {
    json!({
        "id": call.id,
        "name": call.name,
        "input": call.input,
    })
}

fn tool_result_to_json(result: &ToolResult) -> Value {
    json!({
        "id": result.call_id,
        "name": result.name,
        "content": result.content,
    })
}

/// 只输出 attachment 元数据，base64 / 大文本正文不写盘。
fn attachment_meta(att: &MessageAttachment) -> Value {
    match att {
        MessageAttachment::Image {
            name,
            media_type,
            data,
        } => json!({
            "kind": "image",
            "name": name,
            "media_type": media_type,
            "size_bytes": data.len(),
        }),
        MessageAttachment::TextFile {
            name,
            media_type,
            content,
        } => json!({
            "kind": "text_file",
            "name": name,
            "media_type": media_type,
            "size_bytes": content.len(),
        }),
    }
}

pub fn iso_now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_gateway::types::{ToolDefinition, Usage};

    fn sample_request() -> ModelRequest {
        ModelRequest {
            model: String::new(),
            system: Some("you are hebbian".into()),
            entries: vec![
                TranscriptEntry::User(UserEntry::text("hello")),
                TranscriptEntry::Assistant(AssistantEntry {
                    text: "hi".into(),
                    reasoning: "deliberation".into(),
                    reasoning_signature: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "t1".into(),
                        name: "Read".into(),
                        input: json!({"path": "a.txt"}),
                    }],
                }),
                TranscriptEntry::ToolResults(vec![ToolResult {
                    call_id: "t1".into(),
                    name: "Read".into(),
                    content: "file body".into(),
                    artifact: None,
                }]),
            ],
            tools: vec![ToolDefinition {
                name: "Read".into(),
                description: "read".into(),
                parameters: json!({"type": "object"}),
            }],
            max_tokens: 4096,
            reasoning: None,
        }
    }

    #[test]
    fn request_round_trips_text_fields() {
        let req = sample_request();
        let v = request_to_json(&req, "claude-opus-4-7");
        assert_eq!(v["model"], "claude-opus-4-7");
        assert_eq!(v["system"], "you are hebbian");
        assert_eq!(v["max_tokens"], 4096);
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], "hello");
        assert_eq!(v["messages"][1]["role"], "assistant");
        assert_eq!(v["messages"][1]["tool_calls"][0]["name"], "Read");
        assert_eq!(v["messages"][2]["role"], "tool");
        assert_eq!(v["messages"][2]["results"][0]["content"], "file body");
        assert_eq!(v["tools"][0]["name"], "Read");
    }

    #[test]
    fn response_done_serializes_with_usage() {
        let resp = Ok(ModelResponse::Done {
            finish: model_gateway::types::FinishReason::Stop,
            text: "done".into(),
            reasoning: "thought".into(),
            reasoning_signature: String::new(),
            attachments: Vec::new(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        });
        let v = response_to_json(&resp);
        assert_eq!(v["type"], "Done");
        assert_eq!(v["text"], "done");
        assert_eq!(v["usage"]["input_tokens"], 100);
        assert_eq!(v["usage"]["output_tokens"], 50);
    }

    #[test]
    fn response_tool_calls_serializes_calls() {
        let resp = Ok(ModelResponse::ToolCalls {
            text: String::new(),
            reasoning: String::new(),
            reasoning_signature: String::new(),
            calls: vec![ToolCall {
                id: "id-1".into(),
                name: "Bash".into(),
                input: json!({"cmd": "ls"}),
            }],
            attachments: Vec::new(),
            usage: Usage::default(),
        });
        let v = response_to_json(&resp);
        assert_eq!(v["type"], "ToolCalls");
        assert_eq!(v["calls"][0]["name"], "Bash");
        assert_eq!(v["calls"][0]["input"]["cmd"], "ls");
    }

    #[test]
    fn response_error_branch() {
        let err: Result<ModelResponse, ModelError> = Err(ModelError::Other("provider 502".into()));
        let v = response_to_json(&err);
        assert_eq!(v["type"], "Error");
        assert_eq!(v["error"], "provider 502");
    }

    #[test]
    fn attachments_only_dump_metadata() {
        let att = MessageAttachment::Image {
            name: "shot.png".into(),
            media_type: "image/png".into(),
            data: "AAAAAAAA".repeat(1000),
        };
        let v = attachment_meta(&att);
        assert_eq!(v["kind"], "image");
        assert_eq!(v["media_type"], "image/png");
        assert_eq!(v["size_bytes"], 8000);
        // 没有 data 字段
        assert!(v.get("data").is_none());
    }

    #[test]
    fn is_enabled_defaults_on_when_env_unset() {
        // 直接断言 default 行为：环境变量未设置时启用。
        // 用一个绝对不可能被别处占用的临时 var 名做隔离没有意义，因为 is_enabled
        // 用的是常量；这里只断当前进程未配置该 var 时的行为。
        let saved = std::env::var(ENV_VAR).ok();
        // 删除一下以模拟 missing
        std::env::remove_var(ENV_VAR);
        assert!(is_enabled());
        // 恢复
        match saved {
            Some(v) => std::env::set_var(ENV_VAR, v),
            None => std::env::remove_var(ENV_VAR),
        }
    }

    #[test]
    fn is_enabled_disabled_by_explicit_false_values() {
        let saved = std::env::var(ENV_VAR).ok();
        for v in ["0", "false", "FALSE", "off", "No"] {
            std::env::set_var(ENV_VAR, v);
            assert!(!is_enabled(), "expected disabled for {v:?}");
        }
        match saved {
            Some(v) => std::env::set_var(ENV_VAR, v),
            None => std::env::remove_var(ENV_VAR),
        }
    }

    #[tokio::test]
    async fn dump_writes_jsonl_lines_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dump.jsonl");
        let dump = ModelIoDump::open(&path).await.unwrap();
        let entry = DumpEntry {
            ts: iso_now(),
            run_id: "r1".into(),
            turn: 1,
            model: "test".into(),
            request: json!({"x": 1}),
            response: json!({"type": "Done"}),
            duration_ms: 12,
            kind: "main".into(),
        };
        dump.record(entry.clone());
        dump.record(DumpEntry { turn: 2, ..entry });
        dump.flush().await.unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["run_id"], "r1");
        assert_eq!(parsed["turn"], 1);
        let parsed2: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(parsed2["turn"], 2);
    }

    #[test]
    fn dedup_messages_replaces_shared_prefix() {
        let mut prev = Vec::new();
        let m1 = json!({"role": "user", "content": "hello"});
        let m2 = json!({"role": "assistant", "content": "hi"});
        let m3 = json!({"role": "user", "content": "bye"});

        // 第一次：无前缀，所有 messages 都是 new
        let mut req1 = json!({"messages": [m1.clone(), m2.clone()], "system": "sys"});
        ModelIoDump::dedup_messages(&mut req1, &mut prev);
        assert_eq!(prev.len(), 2);
        assert_eq!(req1["messages_carried"], 0);
        assert_eq!(req1["messages_new"].as_array().unwrap().len(), 2);
        assert!(req1.get("messages").is_none());

        // 第二次：前 2 条相同，新增 1 条
        let mut req2 = json!({"messages": [m1.clone(), m2.clone(), m3.clone()]});
        ModelIoDump::dedup_messages(&mut req2, &mut prev);
        assert_eq!(prev.len(), 3);
        assert_eq!(req2["messages_carried"], 2);
        let new_msgs = req2["messages_new"].as_array().unwrap();
        assert_eq!(new_msgs.len(), 1);
        assert_eq!(new_msgs[0]["content"], "bye");
    }
}
