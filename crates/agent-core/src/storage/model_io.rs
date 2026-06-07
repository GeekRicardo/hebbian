//! 读 `<sid>/model_io.jsonl`：把每行 [`DumpEntry`] 反序列化成 surface 友好的列表。
//!
//! 写盘走 [`crate::model_io_dump`]（actor 模式异步落盘）；读这边只走只读 IO，
//! 给前端 Model I/O 调试器、heb CLI `model-io` 子命令提供数据源。
//!
//! 容错策略：jsonl 中坏行（解析失败）只记 warn 并跳过，不让一个孤行毁了整次读取——
//! dump 是 best-effort 写入，可能因进程被 SIGKILL 留半截行；调试器要照常打开。
//!
//! ## 增量 messages 存储
//!
//! 写盘侧对 `kind == "main"` 的 entry 做前缀去重：
//! - `messages_carried: N` — 前 N 条与上一条 main entry 相同
//! - `messages_new: [...]` — 第 N+1 条起的新增 messages
//!
//! 读取侧顺序扫描时维护 `accumulated_messages` 累积数组，遇到增量格式时
//! 取前 N 条 + 拼接 new 即可重建完整 messages。兼容老格式（有 `messages` 字段的）。

use std::path::Path;

use serde_json::Value;

use crate::model_io_dump::default_path;

/// 从增量格式重建完整 messages 数组，同时更新累积状态。
///
/// 兼容三种格式：
/// 1. 增量格式：`{messages_carried: N, messages_new: [...]}`
/// 2. 老格式：`{messages: [...]}`
/// 3. 非 main（judge/compaction）：无 messages 相关字段，不更新累积状态
fn rebuild_messages(request: &mut Value, accumulated: &mut Vec<Value>) {
    let obj = match request.as_object_mut() {
        Some(o) => o,
        None => return,
    };

    if let Some(carried_val) = obj.remove("messages_carried") {
        let carried = carried_val.as_u64().unwrap_or(0) as usize;
        let new_msgs = obj
            .remove("messages_new")
            .and_then(|v| match v {
                Value::Array(a) => Some(a),
                _ => None,
            })
            .unwrap_or_default();

        // 从累积状态取前 carried 条 + 拼接 new
        let mut full: Vec<Value> = accumulated.iter().take(carried).cloned().collect();
        full.extend(new_msgs);

        *accumulated = full.clone();
        obj.insert("messages".to_string(), Value::Array(full));
    } else if let Some(messages) = obj.get("messages").and_then(|m| m.as_array()) {
        // 老格式或无去重的条目——直接用 messages 更新累积状态
        *accumulated = messages.clone();
    }
    // judge/compaction 没有 messages 字段，不动累积状态
}

/// 读 session 的所有 model_io 条目（完整重建 messages）。
///
/// ⚠️ 大 session 下会把所有 entry 全部驻留在内存中，仅 CLI 使用。
/// 前端应走 `read_session_summaries` + `read_session_entry` 两级加载。
pub fn read_session(data_dir: &Path, session_id: &str) -> std::io::Result<Vec<Value>> {
    use std::io::BufRead;

    let path = default_path(data_dir, session_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(&path)?;
    let reader = std::io::BufReader::new(file);
    let mut out = Vec::new();
    let mut accumulated: Vec<Value> = Vec::new();
    for (idx, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(mut v) => {
                if let Some(req) = v.get_mut("request") {
                    rebuild_messages(req, &mut accumulated);
                }
                out.push(v);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    line_idx = idx,
                    "model_io jsonl 行解析失败，跳过"
                );
            }
        }
    }
    Ok(out)
}

/// 只返回每条 entry 的摘要（不含 request.messages / response 正文），
/// 给前端侧边栏列表用。即使 jsonl 有几百 MB，摘要总量也只有几十 KB。
///
/// 逐行读取 + 解析 + 提取摘要 + 立刻丢弃完整 Value —— 峰值内存 ≈ 单条 entry 大小，
/// 远低于 `read_session` 的全量驻留。
pub fn read_session_summaries(data_dir: &Path, session_id: &str) -> std::io::Result<Vec<Value>> {
    use std::io::BufRead;

    let path = default_path(data_dir, session_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(&path)?;
    let reader = std::io::BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => {
                let resp = v.get("response");
                let req = v.get("request");

                // 计算 message_count：兼容增量格式和老格式
                let msg_count = if let Some(carried) =
                    req.and_then(|r| r.get("messages_carried")).and_then(|c| c.as_u64())
                {
                    let new_len = req
                        .and_then(|r| r.get("messages_new"))
                        .and_then(|m| m.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    (carried as usize + new_len) as u64
                } else if let Some(msgs) =
                    req.and_then(|r| r.get("messages")).and_then(|m| m.as_array())
                {
                    msgs.len() as u64
                } else {
                    0
                };

                out.push(serde_json::json!({
                    "ts": v.get("ts").cloned(),
                    "run_id": v.get("run_id").cloned(),
                    "turn": v.get("turn").cloned(),
                    "model": v.get("model").cloned(),
                    "kind": v.get("kind").cloned(),
                    "duration_ms": v.get("duration_ms").cloned(),
                    "response": {
                        "type": resp.and_then(|r| r.get("type")).cloned(),
                        "usage": resp.and_then(|r| r.get("usage")).cloned()
                    },
                    "message_count": msg_count,
                }));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    line_idx = idx,
                    "model_io jsonl 行解析失败，跳过"
                );
            }
        }
    }
    Ok(out)
}

/// 按索引返回单条完整 entry（增量 messages 已重建）。
///
/// 顺序扫描至目标行，沿途累积 messages 状态以支持增量重建。
/// 未到目标行的行只解析 request 中的 messages 相关字段来维护累积状态，
/// 不保留完整 Value——峰值内存 ≈ 累积 messages 数组大小。
pub fn read_session_entry(
    data_dir: &Path,
    session_id: &str,
    index: usize,
) -> std::io::Result<Option<Value>> {
    use std::io::BufRead;

    let path = default_path(data_dir, session_id);
    if !path.exists() {
        return Ok(None);
    }
    let file = std::fs::File::open(&path)?;
    let reader = std::io::BufReader::new(file);
    let mut accumulated: Vec<Value> = Vec::new();
    let mut valid_idx = 0usize;
    for (line_no, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if valid_idx == index {
            // 目标行：完整解析 + 重建 messages
            return match serde_json::from_str::<Value>(trimmed) {
                Ok(mut v) => {
                    if let Some(req) = v.get_mut("request") {
                        rebuild_messages(req, &mut accumulated);
                    }
                    Ok(Some(v))
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        line_idx = line_no,
                        "model_io entry 解析失败"
                    );
                    Ok(None)
                }
            };
        }
        // 非目标行：只更新累积状态
        if let Ok(mut v) = serde_json::from_str::<Value>(trimmed) {
            if let Some(req) = v.get_mut("request") {
                rebuild_messages(req, &mut accumulated);
            }
        }
        valid_idx += 1;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_returns_empty_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = read_session(tmp.path(), "missing-sid").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn read_parses_well_formed_lines_and_skips_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        let path = default_path(tmp.path(), "sid1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = format!(
            "{}\n{}\nnot json here\n{}\n",
            serde_json::to_string(&json!({"turn": 1, "model": "m1"})).unwrap(),
            serde_json::to_string(&json!({"turn": 2, "model": "m1"})).unwrap(),
            serde_json::to_string(&json!({"turn": 3, "model": "m1"})).unwrap(),
        );
        std::fs::write(&path, body).unwrap();
        let entries = read_session(tmp.path(), "sid1").unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["turn"], 1);
        assert_eq!(entries[2]["turn"], 3);
    }

    #[test]
    fn incremental_messages_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        let path = default_path(tmp.path(), "sid2");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // 第一条：全量 messages（老格式或第一次写入）
        let entry1 = json!({
            "ts": "2026-01-01T00:00:00Z",
            "run_id": "r1",
            "turn": 0,
            "model": "m1",
            "kind": "main",
            "duration_ms": 100,
            "request": {
                "messages": [
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "content": "hi"}
                ]
            },
            "response": {"type": "Done"}
        });
        // 第二条：增量格式，carried=2，new=[新消息]
        let entry2 = json!({
            "ts": "2026-01-01T00:01:00Z",
            "run_id": "r1",
            "turn": 1,
            "model": "m1",
            "kind": "main",
            "duration_ms": 200,
            "request": {
                "messages_carried": 2,
                "messages_new": [
                    {"role": "user", "content": "how are you"},
                    {"role": "assistant", "content": "good"}
                ]
            },
            "response": {"type": "Done"}
        });
        // 第三条：judge（不影响累积状态）
        let entry3 = json!({
            "ts": "2026-01-01T00:01:30Z",
            "run_id": "r1",
            "turn": 0,
            "model": "m1",
            "kind": "judge",
            "duration_ms": 50,
            "request": {"tool": "Bash", "input": {"cmd": "ls"}},
            "response": {"raw": "allow", "final": "allow"}
        });
        // 第四条：基于第二条的增量
        let entry4 = json!({
            "ts": "2026-01-01T00:02:00Z",
            "run_id": "r1",
            "turn": 2,
            "model": "m1",
            "kind": "main",
            "duration_ms": 300,
            "request": {
                "messages_carried": 4,
                "messages_new": [
                    {"role": "user", "content": "bye"}
                ]
            },
            "response": {"type": "Done"}
        });

        let body = format!(
            "{}\n{}\n{}\n{}\n",
            serde_json::to_string(&entry1).unwrap(),
            serde_json::to_string(&entry2).unwrap(),
            serde_json::to_string(&entry3).unwrap(),
            serde_json::to_string(&entry4).unwrap(),
        );
        std::fs::write(&path, body).unwrap();

        // read_session 全量读
        let entries = read_session(tmp.path(), "sid2").unwrap();
        assert_eq!(entries.len(), 4);
        // 第一条：原样
        let msgs0 = entries[0]["request"]["messages"].as_array().unwrap();
        assert_eq!(msgs0.len(), 2);
        assert_eq!(msgs0[0]["content"], "hello");
        // 第二条：重建完整 4 条
        let msgs1 = entries[1]["request"]["messages"].as_array().unwrap();
        assert_eq!(msgs1.len(), 4);
        assert_eq!(msgs1[0]["content"], "hello");
        assert_eq!(msgs1[2]["content"], "how are you");
        // 第三条 judge：没有 messages
        assert!(entries[2]["request"]["messages"].is_null() || entries[2]["request"].get("messages").is_none());
        // 第四条：重建完整 5 条
        let msgs3 = entries[3]["request"]["messages"].as_array().unwrap();
        assert_eq!(msgs3.len(), 5);
        assert_eq!(msgs3[4]["content"], "bye");

        // read_session_entry 单条读
        let single = read_session_entry(tmp.path(), "sid2", 3).unwrap().unwrap();
        let single_msgs = single["request"]["messages"].as_array().unwrap();
        assert_eq!(single_msgs.len(), 5);
        assert_eq!(single_msgs[0]["content"], "hello");
        assert_eq!(single_msgs[4]["content"], "bye");

        // read_session_summaries message_count
        let summaries = read_session_summaries(tmp.path(), "sid2").unwrap();
        assert_eq!(summaries[0]["message_count"], 2);
        assert_eq!(summaries[1]["message_count"], 4);
        assert_eq!(summaries[2]["message_count"], 0); // judge
        assert_eq!(summaries[3]["message_count"], 5);
    }
}
