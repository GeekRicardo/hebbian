//! Session 目录布局（架构 §4.9.1 / §6.1）。
//!
//! 每段对话一个目录：
//!
//! ```text
//! ~/.hebbian/sessions/<session_id>/
//! ├── session.jsonl
//! ├── meta.json
//! ├── tool_results/
//! ├── compactions/
//! ├── plans/
//! └── partial/
//!     └── <msg_id>.partial.jsonl
//! ```
//!
//! 当前阶段 `session.jsonl` 主体写入仍由 [`common::storage::sessions`] 处理；本模块负责
//! 提供新布局的路径计算 + 目录初始化 + meta.json + partial sidecar，配合 Recorder
//! 的流式中间态落盘 + 中断恢复（架构 §4.9.3 / §10.8）。

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use common::AppResult;

use super::lock;

/// session 根目录：`~/.hebbian/sessions/<id>/`。
pub fn session_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join("sessions").join(session_id)
}

pub fn session_jsonl_path(data_dir: &Path, session_id: &str) -> PathBuf {
    session_dir(data_dir, session_id).join("session.jsonl")
}

pub fn meta_path(data_dir: &Path, session_id: &str) -> PathBuf {
    session_dir(data_dir, session_id).join("meta.json")
}

pub fn partial_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    session_dir(data_dir, session_id).join("partial")
}

pub fn partial_path(data_dir: &Path, session_id: &str, msg_id: &str) -> PathBuf {
    partial_dir(data_dir, session_id).join(format!("{msg_id}.partial.jsonl"))
}

/// 确保 session 主体目录与所有子目录都存在。
pub fn ensure_session_dirs(data_dir: &Path, session_id: &str) -> AppResult<()> {
    let root = session_dir(data_dir, session_id);
    for sub in [
        root.clone(),
        root.join("tool_results"),
        root.join("compactions"),
        root.join("plans"),
        root.join("partial"),
    ] {
        std::fs::create_dir_all(&sub)?;
    }
    Ok(())
}

/// 架构 §4.9.3：`{yyyymmddHHmm}-{shortUuid}`。
///
/// 新 session 推荐使用本函数生成 id；老 session 走 uuid 的 v4 仍然可被识别——
/// 列表与加载按目录名当 id，不解析格式。
pub fn new_session_id() -> String {
    let now = Utc::now().format("%Y%m%d%H%M");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{now}-{}", &suffix[..8])
}

// ──────────────────────────────────────────────────────────────────────────
// meta.json
// ──────────────────────────────────────────────────────────────────────────

/// 写入 session/meta.json 的最小字段集（架构 §4.9.1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDirMeta {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub agent: String,
    pub workdir: Option<PathBuf>,
    pub provider: String,
    pub model: String,
    /// 流式中断时间戳；首次落 partial 时不写，恢复时由
    /// [`recover_interrupted_partials`] 填上。
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastInterruptedAt")]
    pub last_interrupted_at: Option<i64>,
}

pub fn save_meta(data_dir: &Path, meta: &SessionDirMeta) -> AppResult<()> {
    ensure_session_dirs(data_dir, &meta.session_id)?;
    let path = meta_path(data_dir, &meta.session_id);
    let bytes = serde_json::to_vec_pretty(meta)?;
    lock::write_atomic(&path, &bytes)
}

pub fn load_meta(data_dir: &Path, session_id: &str) -> AppResult<Option<SessionDirMeta>> {
    let p = meta_path(data_dir, session_id);
    if !p.exists() {
        return Ok(None);
    }
    let bytes = lock::read_locked(&p)?;
    Ok(serde_json::from_slice(&bytes).ok())
}

// ──────────────────────────────────────────────────────────────────────────
// partial sidecar
// ──────────────────────────────────────────────────────────────────────────

/// partial 文件单行格式。`text` / `reasoning` / `tool_call` 三类增量。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PartialFragment {
    Text { text: String },
    Reasoning { text: String },
    ToolCall {
        index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        arguments_chunk: String,
    },
}

pub fn append_partial(
    data_dir: &Path,
    session_id: &str,
    msg_id: &str,
    fragment: &PartialFragment,
) -> AppResult<()> {
    let dir = partial_dir(data_dir, session_id);
    std::fs::create_dir_all(&dir)?;
    let path = partial_path(data_dir, session_id, msg_id);
    let line = serde_json::to_string(fragment)?;
    lock::append_jsonl(&path, &line)
}

pub fn delete_partial(data_dir: &Path, session_id: &str, msg_id: &str) -> AppResult<()> {
    let path = partial_path(data_dir, session_id, msg_id);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// 中断恢复结果：每个 msg_id 累出文本 + reasoning + tool_call 串。
#[derive(Debug, Default, Clone)]
pub struct RecoveredPartial {
    pub msg_id: String,
    pub text: String,
    pub reasoning: String,
    /// 按 index 聚合的 tool_call arguments 累积字符串。
    pub tool_calls: std::collections::BTreeMap<u32, (Option<String>, String)>,
}

/// 扫描 partial 目录，把每个残留文件聚合并返回。返回后调用方负责把内容写到
/// `session.jsonl` 并删除 partial 文件（架构 §10.8 / §4.9.3）。
pub fn recover_interrupted_partials(
    data_dir: &Path,
    session_id: &str,
) -> AppResult<Vec<RecoveredPartial>> {
    let dir = partial_dir(data_dir, session_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // <msg_id>.partial.jsonl
        let Some(msg_id) = name.strip_suffix(".partial.jsonl") else {
            continue;
        };
        let bytes = match lock::read_locked(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "读 partial 失败");
                continue;
            }
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut recovered = RecoveredPartial {
            msg_id: msg_id.to_string(),
            ..Default::default()
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<PartialFragment>(line) {
                Ok(PartialFragment::Text { text }) => recovered.text.push_str(&text),
                Ok(PartialFragment::Reasoning { text }) => recovered.reasoning.push_str(&text),
                Ok(PartialFragment::ToolCall {
                    index,
                    name,
                    arguments_chunk,
                }) => {
                    let entry = recovered.tool_calls.entry(index).or_insert((None, String::new()));
                    if entry.0.is_none() {
                        entry.0 = name;
                    }
                    entry.1.push_str(&arguments_chunk);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "解析 partial 行失败");
                }
            }
        }
        out.push(recovered);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hebbian-sd-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn new_session_id_has_expected_shape() {
        let id = new_session_id();
        // yyyymmddHHmm = 12 字符；- 1 字符；short uuid 8 字符
        assert_eq!(id.len(), 12 + 1 + 8, "id = {id}");
        assert!(id.chars().nth(12) == Some('-'));
    }

    #[test]
    fn partial_roundtrip_and_recovery() {
        let dir = tmp("partial");
        let sid = "20260511-abc12345";
        ensure_session_dirs(&dir, sid).unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::Text {
                text: "hello".into(),
            },
        )
        .unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::Text {
                text: " world".into(),
            },
        )
        .unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::ToolCall {
                index: 0,
                name: Some("Bash".into()),
                arguments_chunk: r#"{"command""#.into(),
            },
        )
        .unwrap();
        append_partial(
            &dir,
            sid,
            "msg1",
            &PartialFragment::ToolCall {
                index: 0,
                name: None,
                arguments_chunk: r#":"ls"}"#.into(),
            },
        )
        .unwrap();

        let recovered = recover_interrupted_partials(&dir, sid).unwrap();
        assert_eq!(recovered.len(), 1);
        let r = &recovered[0];
        assert_eq!(r.msg_id, "msg1");
        assert_eq!(r.text, "hello world");
        let tc = r.tool_calls.get(&0).unwrap();
        assert_eq!(tc.0.as_deref(), Some("Bash"));
        assert_eq!(tc.1, r#"{"command":"ls"}"#);
    }
}
