//! 模型 IO 落盘（与 model-gateway 同处，任务2）：**请求发起**与**响应到达**分两条 jsonl 行，
//! 用 `call_id` 关联。由 [`crate::instrument::InstrumentedClient`] 在所有模型调用边界**自动**
//! 落盘——不管哪个调用点发起（主 chat / judge / 旁支 / 压缩…），按 [`ModelCallMeta::tag`] 区分。
//!
//! - **默认开启**：`HEBBIAN_DUMP_MODEL_IO=0`/`false`/`off`/`no` 显式禁用。
//! - 文件：`<data_dir>/sessions/<session_id>/model_io.jsonl`（meta 缺 session_id 的调用——如健康
//!   检查——不落盘）。
//! - 请求发起即落 `{phase:"request", call_id, session_id, tag, run_id, turn, message_id, model,
//!   request}`，响应到达落 `{phase:"response", call_id, duration_ms, response}`。崩溃 / 取消也留
//!   请求痕迹（旧的一条式在 complete 返回后才落，崩溃丢请求）。
//! - 每个 session 一个后台 actor（懒建、全局复用），fire-and-forget 写盘不阻塞模型调用热路径。
//! - `tag=main` 的 request 做 messages 前缀去重（actor 内 per-session 状态），避免每轮重发整段
//!   transcript 撑爆文件；读侧顺序累积重建（见 `agent-core storage::model_io`）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use serde_json::{json, Value};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tracing::warn;

use crate::types::{
    AssistantEntry, ModelCallMeta, ModelError, ModelRequest, ModelResponse, ToolCall, ToolResult,
    TranscriptEntry, UserEntry,
};
use common::attachments::MessageAttachment;

/// 控制本功能的环境变量（未设置 = 默认启用）。
pub const ENV_VAR: &str = "HEBBIAN_DUMP_MODEL_IO";

pub fn is_enabled() -> bool {
    match std::env::var(ENV_VAR) {
        Ok(v) => {
            let t = v.trim().to_ascii_lowercase();
            !matches!(t.as_str(), "0" | "false" | "off" | "no")
        }
        Err(_) => true,
    }
}

/// `<data_dir>/sessions/<session_id>/model_io.jsonl`（与 tool_results/ 等同级，§4.9.1）。
fn default_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("sessions")
        .join(session_id)
        .join("model_io.jsonl")
}

pub fn iso_now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ── per-session 落盘 actor + 全局 registry ─────────────────────────────────────

enum DumpCmd {
    Write(Value),
}

/// 一个 session 的落盘 actor：串行消费命令、实时 append jsonl。收 `Value` 而非 String——
/// 在 actor 内对 `tag=main` 的 request 做前缀去重（需 per-session 状态），再序列化写。
struct DumpActor {
    tx: mpsc::UnboundedSender<DumpCmd>,
}

impl DumpActor {
    fn spawn(path: PathBuf) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<DumpCmd>();
        tokio::spawn(async move {
            if let Some(parent) = path.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    warn!(error = %e, path = %path.display(), "model_io 落盘目录创建失败");
                    return;
                }
            }
            let mut file = match OpenOptions::new().create(true).append(true).open(&path).await {
                Ok(f) => f,
                Err(e) => {
                    warn!(error = %e, path = %path.display(), "model_io 打开失败");
                    return;
                }
            };
            // 上一条 main request 的 messages——前缀去重基线。
            let mut prev_main_messages: Vec<Value> = Vec::new();
            while let Some(cmd) = rx.recv().await {
                let DumpCmd::Write(mut entry) = cmd;
                // 仅对 main 的 request 做 messages 前缀去重（judge/aside 等结构不同、全量写）。
                if entry.get("phase").and_then(Value::as_str) == Some("request")
                    && entry.get("tag").and_then(Value::as_str) == Some("main")
                {
                    if let Some(req) = entry.get_mut("request") {
                        dedup_messages(req, &mut prev_main_messages);
                    }
                }
                match serde_json::to_string(&entry) {
                    Ok(line) => {
                        if file.write_all(line.as_bytes()).await.is_err()
                            || file.write_all(b"\n").await.is_err()
                        {
                            warn!(path = %path.display(), "model_io 写入失败");
                        }
                    }
                    Err(e) => warn!(error = %e, "model_io 序列化失败"),
                }
            }
        });
        Self { tx }
    }

    fn write(&self, entry: Value) {
        let _ = self.tx.send(DumpCmd::Write(entry));
    }
}

static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, DumpActor>>> = OnceLock::new();

fn write_entry(path: PathBuf, entry: Value) {
    let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = reg.lock().unwrap();
    map.entry(path.clone())
        .or_insert_with(|| DumpActor::spawn(path))
        .write(entry);
}

// ── 落盘入口（InstrumentedClient 调）──────────────────────────────────────────

/// 模型请求发起即落一条 `request` 行。返回 `call_id`（响应落盘用同一个关联）；未启用 / meta
/// 缺 session_id（如健康检查）时返回 None，调用方据此跳过响应落盘。
pub fn record_request(
    data_dir: &Path,
    meta: &ModelCallMeta,
    model: &str,
    req: &ModelRequest,
) -> Option<String> {
    if !is_enabled() {
        return None;
    }
    let session_id = meta.session_id.as_deref()?;
    let call_id = uuid::Uuid::new_v4().to_string();
    let entry = json!({
        "ts": iso_now(),
        "phase": "request",
        "call_id": call_id,
        "session_id": session_id,
        "tag": meta.tag.as_str(),
        "run_id": meta.run_id,
        "turn": meta.turn,
        "message_id": meta.message_id,
        "model": model,
        "request": request_to_json(req, model),
    });
    write_entry(default_path(data_dir, session_id), entry);
    Some(call_id)
}

/// 模型响应到达落一条 `response` 行（与 `record_request` 返回的 `call_id` 关联）。
pub fn record_response(
    data_dir: &Path,
    session_id: &str,
    call_id: &str,
    resp: &Result<ModelResponse, ModelError>,
    duration_ms: u64,
) {
    if !is_enabled() {
        return;
    }
    let entry = json!({
        "ts": iso_now(),
        "phase": "response",
        "call_id": call_id,
        "duration_ms": duration_ms,
        "response": response_to_json(resp),
    });
    write_entry(default_path(data_dir, session_id), entry);
}

// ── JSON 序列化（从 agent-core model_io_dump 迁移；attachments 只留元数据）────────

/// `tag=main` 的 request.messages 前缀去重：与上一条比较，相同前缀替换为
/// `{messages_carried: N, messages_new: [...]}`。读侧顺序累积可重建完整数组。
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
    *prev = messages;
    if let Some(obj) = request.as_object_mut() {
        obj.remove("messages");
        obj.insert("messages_carried".to_string(), Value::from(carried as u64));
        obj.insert("messages_new".to_string(), Value::Array(new_msgs));
    }
}

/// 把 [`ModelRequest`] 序列化成精简 JSON：保留文本字段，attachments 只留元数据。
pub fn request_to_json(req: &ModelRequest, model_name: &str) -> Value {
    let messages: Vec<Value> = req.entries.iter().map(transcript_entry_to_json).collect();
    json!({
        "model": model_name,
        "system": req.system,
        "messages": messages,
        "tools": req.tools,
        "max_tokens": req.max_tokens,
        "reasoning": req.reasoning,
    })
}

/// 把 [`ModelResponse`] 序列化；`Err` 走 `{type:"Error", error:...}` 分支。
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
        Err(e) => json!({ "type": "Error", "error": e.to_string() }),
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
    json!({ "id": call.id, "name": call.name, "input": call.input })
}

fn tool_result_to_json(result: &ToolResult) -> Value {
    json!({ "id": result.call_id, "name": result.name, "content": result.content })
}

fn attachment_meta(att: &MessageAttachment) -> Value {
    match att {
        MessageAttachment::Image {
            name,
            media_type,
            data,
        } => json!({"kind": "image", "name": name, "media_type": media_type, "size_bytes": data.len()}),
        MessageAttachment::TextFile {
            name,
            media_type,
            content,
        } => json!({"kind": "text_file", "name": name, "media_type": media_type, "size_bytes": content.len()}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelCallTag, ModelError};

    fn unique_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("heb_mio_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dedup_carries_common_prefix() {
        let mut prev = vec![json!({"role": "user", "content": "a"})];
        let mut req = json!({"messages": [
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": "b"}
        ]});
        dedup_messages(&mut req, &mut prev);
        assert_eq!(req["messages_carried"], json!(1));
        assert_eq!(req["messages_new"].as_array().unwrap().len(), 1);
        assert!(req.get("messages").is_none(), "原 messages 被替换成增量");
        assert_eq!(prev.len(), 2, "累积基线更新为本次完整 messages");
    }

    /// 任务2 核心：一次模型调用落「请求 + 响应」两行，同 call_id 关联。
    #[tokio::test]
    async fn request_and_response_fall_into_two_lines_with_same_call_id() {
        let dir = unique_dir();
        let meta = ModelCallMeta {
            session_id: Some("s1".into()),
            tag: ModelCallTag::Main,
            ..Default::default()
        };
        let req = ModelRequest {
            model: "m1".into(),
            ..Default::default()
        };
        let call_id = record_request(&dir, &meta, "m1", &req).expect("启用时返回 call_id");
        let resp: Result<ModelResponse, ModelError> = Err(ModelError::Other("boom".into()));
        record_response(&dir, "s1", &call_id, &resp, 42);

        let path = dir.join("sessions/s1/model_io.jsonl");
        let mut content = String::new();
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            content = std::fs::read_to_string(&path).unwrap_or_default();
            if content.lines().count() >= 2 {
                break;
            }
        }
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "请求 + 响应 = 两行");
        let l0: Value = serde_json::from_str(lines[0]).unwrap();
        let l1: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(l0["phase"], "request");
        assert_eq!(l0["tag"], "main");
        assert_eq!(l0["model"], "m1");
        assert_eq!(l1["phase"], "response");
        assert_eq!(l1["duration_ms"], 42);
        assert_eq!(l1["response"]["type"], "Error");
        assert_eq!(l0["call_id"], l1["call_id"], "两行同 call_id 关联");
        assert!(!l0["call_id"].as_str().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
