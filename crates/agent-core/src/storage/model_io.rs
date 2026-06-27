//! 读 `<sid>/model_io.jsonl`：给前端 Model I/O 调试器、heb CLI `model-io` 子命令提供数据源。
//!
//! ## 两条式格式（任务2）
//!
//! 落盘下沉 model-gateway 后，每次模型调用落**两行**：
//! - `{phase:"request", call_id, tag, run_id, turn, message_id, model, request}`（发起即落）
//! - `{phase:"response", call_id, duration_ms, response}`（响应到达）
//!
//! 读取侧按 `call_id` 把两行**合并**成一条 surface 友好的 entry：保持 request 的顺序、用
//! response 回填 `response` / `duration_ms`；崩溃 / 取消导致 response 缺失时，该条只有 request
//! （前端显示「无响应」），不丢痕迹。`tag` 同时映射到 `kind`，前端按 `kind` 渲染标签不必改。
//!
//! ## 兼容旧一条式
//!
//! 旧的 `{kind, request, response}` 一条式（无 `phase` 字段）原样直接收。
//!
//! ## 增量 messages
//!
//! `tag/kind == "main"` 的 request 做 messages 前缀去重（`messages_carried` / `messages_new`）：
//! 读侧顺序扫描维护 `accumulated` 累积数组重建完整 messages。judge / compaction / aside 等
//! 非主调用不参与累积链（旁支 messages 与主对话无关，混入会带偏 main 的重建基线）。

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::model_io_dump::default_path;

/// 是否参与 main 增量累积链：新格式看 `tag`，旧格式看 `kind`，都缺则当老 main。
fn is_main_entry(entry: &Value) -> bool {
    match entry
        .get("tag")
        .or_else(|| entry.get("kind"))
        .and_then(Value::as_str)
    {
        Some(k) => k == "main",
        None => true,
    }
}

/// 从增量格式重建一条 entry 的完整 `request.messages`，并按需更新累积状态（仅 main 参与）。
fn rebuild_messages(entry: &mut Value, accumulated: &mut Vec<Value>) {
    let is_main = is_main_entry(entry);
    let request = match entry.get_mut("request").and_then(|r| r.as_object_mut()) {
        Some(o) => o,
        None => return,
    };
    if !is_main {
        return;
    }
    if let Some(carried_val) = request.remove("messages_carried") {
        let carried = carried_val.as_u64().unwrap_or(0) as usize;
        let new_msgs = request
            .remove("messages_new")
            .and_then(|v| match v {
                Value::Array(a) => Some(a),
                _ => None,
            })
            .unwrap_or_default();
        let mut full: Vec<Value> = accumulated.iter().take(carried).cloned().collect();
        full.extend(new_msgs);
        *accumulated = full.clone();
        request.insert("messages".to_string(), Value::Array(full));
    } else if let Some(messages) = request.get("messages").and_then(|m| m.as_array()) {
        *accumulated = messages.clone();
    }
}

/// 一行的相位。无 `phase` 字段 = 旧一条式。
fn phase_of(v: &Value) -> Option<&str> {
    v.get("phase").and_then(Value::as_str)
}

/// 把 `tag` 映射成 `kind`（前端按 kind 渲染标签，不必感知新字段名）。
fn tag_to_kind(entry: &mut Value) {
    if entry.get("kind").is_none() {
        if let Some(tag) = entry.get("tag").cloned() {
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("kind".to_string(), tag);
            }
        }
    }
}

/// 读 session 的所有 model_io 条目（两条式合并 + 增量重建）。仅 CLI 全量使用。
pub fn read_session(data_dir: &Path, session_id: &str) -> std::io::Result<Vec<Value>> {
    use std::io::BufRead;
    let path = default_path(data_dir, session_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let reader = std::io::BufReader::new(std::fs::File::open(&path)?);
    let mut out: Vec<Value> = Vec::new();
    let mut call_idx: HashMap<String, usize> = HashMap::new();
    let mut accumulated: Vec<Value> = Vec::new();
    for (idx, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut v: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), line_idx = idx, "model_io 行解析失败，跳过");
                continue;
            }
        };
        match phase_of(&v) {
            Some("request") => {
                rebuild_messages(&mut v, &mut accumulated);
                tag_to_kind(&mut v);
                if let Some(call_id) = v.get("call_id").and_then(Value::as_str) {
                    call_idx.insert(call_id.to_string(), out.len());
                }
                out.push(v);
            }
            Some("response") => {
                let call_id = v.get("call_id").and_then(Value::as_str).map(str::to_owned);
                let target = call_id.as_deref().and_then(|c| call_idx.get(c)).copied();
                if let Some(i) = target {
                    if let Some(obj) = out[i].as_object_mut() {
                        if let Some(resp) = v.get("response") {
                            obj.insert("response".to_string(), resp.clone());
                        }
                        if let Some(d) = v.get("duration_ms") {
                            obj.insert("duration_ms".to_string(), d.clone());
                        }
                    }
                } else {
                    out.push(v); // 孤 response（request 丢失）
                }
            }
            _ => {
                // 旧一条式
                rebuild_messages(&mut v, &mut accumulated);
                out.push(v);
            }
        }
    }
    Ok(out)
}

/// 提取一条 entry 的摘要（不含 messages / response 正文），给前端侧边栏列表。
fn summary_of(v: &Value) -> Value {
    let resp = v.get("response");
    let req = v.get("request");
    let msg_count = if let Some(carried) = req
        .and_then(|r| r.get("messages_carried"))
        .and_then(Value::as_u64)
    {
        let new_len = req
            .and_then(|r| r.get("messages_new"))
            .and_then(|m| m.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        carried as usize + new_len
    } else {
        req.and_then(|r| r.get("messages"))
            .and_then(|m| m.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    } as u64;
    serde_json::json!({
        "ts": v.get("ts").cloned(),
        "run_id": v.get("run_id").cloned(),
        "turn": v.get("turn").cloned(),
        "model": v.get("model").cloned(),
        "kind": v.get("kind").or_else(|| v.get("tag")).cloned(),
        "call_id": v.get("call_id").cloned(),
        "duration_ms": v.get("duration_ms").cloned(),
        "response": {
            "type": resp.and_then(|r| r.get("type")).cloned(),
            "usage": resp.and_then(|r| r.get("usage")).cloned()
        },
        "message_count": msg_count,
    })
}

/// 只返回每条 entry 的摘要（两条式合并后）。
pub fn read_session_summaries(data_dir: &Path, session_id: &str) -> std::io::Result<Vec<Value>> {
    // 摘要不含正文，先合并再提取最简单（峰值仍受控：摘要只在 request 行暂存的 meta + response
    // 的 type/usage，messages 正文在 summary_of 里只数长度不保留）。
    let merged = read_session(data_dir, session_id)?;
    Ok(merged.iter().map(summary_of).collect())
}

/// 按索引返回单条完整 entry（两条式合并 + 增量重建）。
pub fn read_session_entry(
    data_dir: &Path,
    session_id: &str,
    index: usize,
) -> std::io::Result<Option<Value>> {
    let merged = read_session(data_dir, session_id)?;
    Ok(merged.into_iter().nth(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_returns_empty_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_session(tmp.path(), "missing-sid").unwrap().is_empty());
    }

    #[test]
    fn two_phase_request_response_merge_by_call_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = default_path(tmp.path(), "sid-2p");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let req = json!({"phase":"request","call_id":"c1","tag":"main","run_id":"r1","turn":0,
            "model":"m1","request":{"messages":[{"role":"user","content":"hi"}]}});
        let resp = json!({"phase":"response","call_id":"c1","duration_ms":120,
            "response":{"type":"Done","usage":{"input_tokens":10}}});
        // 第二次调用只落了 request（崩溃，无 response）
        let req2 = json!({"phase":"request","call_id":"c2","tag":"judge","run_id":"r1","turn":1,
            "model":"m1","request":{"tool":"Bash"}});
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                serde_json::to_string(&req).unwrap(),
                serde_json::to_string(&resp).unwrap(),
                serde_json::to_string(&req2).unwrap(),
            ),
        )
        .unwrap();
        let entries = read_session(tmp.path(), "sid-2p").unwrap();
        assert_eq!(entries.len(), 2, "两行 c1 合并成 1 条 + c2 一条 = 2");
        // c1：request + response 合并，tag 映射成 kind
        assert_eq!(entries[0]["call_id"], "c1");
        assert_eq!(entries[0]["kind"], "main");
        assert_eq!(entries[0]["duration_ms"], 120);
        assert_eq!(entries[0]["response"]["type"], "Done");
        assert_eq!(entries[0]["request"]["messages"][0]["content"], "hi");
        // c2：崩溃只有 request，无 response（保留痕迹）
        assert_eq!(entries[1]["call_id"], "c2");
        assert!(entries[1].get("response").is_none());
    }

    #[test]
    fn two_phase_main_increment_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        let path = default_path(tmp.path(), "sid-inc");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let r1 = json!({"phase":"request","call_id":"c1","tag":"main","request":{"messages":[
            {"role":"user","content":"a"},{"role":"assistant","content":"b"}]}});
        let p1 = json!({"phase":"response","call_id":"c1","response":{"type":"Done"}});
        let r2 = json!({"phase":"request","call_id":"c2","tag":"main",
            "request":{"messages_carried":2,"messages_new":[{"role":"user","content":"c"}]}});
        let p2 = json!({"phase":"response","call_id":"c2","response":{"type":"Done"}});
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n{}\n",
                serde_json::to_string(&r1).unwrap(),
                serde_json::to_string(&p1).unwrap(),
                serde_json::to_string(&r2).unwrap(),
                serde_json::to_string(&p2).unwrap(),
            ),
        )
        .unwrap();
        let entries = read_session(tmp.path(), "sid-inc").unwrap();
        assert_eq!(entries.len(), 2);
        let m2 = entries[1]["request"]["messages"].as_array().unwrap();
        assert_eq!(m2.len(), 3, "carried=2 + new=1 重建出 3 条");
        assert_eq!(m2[0]["content"], "a");
        assert_eq!(m2[2]["content"], "c");
        let summaries = read_session_summaries(tmp.path(), "sid-inc").unwrap();
        assert_eq!(summaries[1]["message_count"], 3);
    }

    #[test]
    fn legacy_one_line_format_still_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = default_path(tmp.path(), "sid-legacy");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 旧一条式（无 phase）：kind + request + response 同行
        let old = json!({"kind":"main","turn":0,"model":"m1",
            "request":{"messages":[{"role":"user","content":"hi"}]},"response":{"type":"Done"}});
        std::fs::write(&path, format!("{}\n", serde_json::to_string(&old).unwrap())).unwrap();
        let entries = read_session(tmp.path(), "sid-legacy").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["kind"], "main");
        assert_eq!(entries[0]["response"]["type"], "Done");
        assert_eq!(entries[0]["request"]["messages"][0]["content"], "hi");
    }
}
