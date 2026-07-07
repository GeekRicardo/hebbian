//! compact 前的 transcript 归档（架构 §4.7 / §6.1）。
//!
//! 路径形如 `~/.hebbian/sessions/<sid>/compactions/compact-<archive_id>.md/jsonl/meta.json`。
//! `session.jsonl` 仍是唯一历史账本；这里的文件只用于审计、恢复和按需检索。

use std::path::{Path, PathBuf};

use common::AppResult;
use model_gateway::types::TranscriptEntry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::lock;

pub fn dir_for_session(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("sessions")
        .join(session_id)
        .join("compactions")
}

pub fn save_compaction(
    data_dir: &Path,
    session_id: &str,
    timestamp_label: &str,
    markdown: &str,
) -> AppResult<PathBuf> {
    let dir = dir_for_session(data_dir, session_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("compact-{timestamp_label}.md"));
    lock::write_atomic(&path, markdown.as_bytes())?;
    Ok(path)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionArchiveMeta {
    pub archive_id: String,
    pub start_entry_id: String,
    pub end_entry_id: String,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub summary_hash: String,
    pub checkpoint_hash: String,
    pub artifacts: Vec<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct CompactionArchive {
    pub archive_id: String,
    pub markdown_path: PathBuf,
    pub jsonl_path: PathBuf,
    pub meta_path: PathBuf,
    pub meta: CompactionArchiveMeta,
}

pub fn save_compaction_archive(
    data_dir: &Path,
    session_id: &str,
    entries: &[TranscriptEntry],
    before_tokens: usize,
    after_tokens: usize,
    summary: &str,
) -> AppResult<CompactionArchive> {
    let dir = dir_for_session(data_dir, session_id);
    std::fs::create_dir_all(&dir)?;

    let archive_id = archive_id();
    let base = dir.join(format!("compact-{archive_id}"));
    let markdown_path = base.with_extension("md");
    let jsonl_path = base.with_extension("jsonl");
    let meta_path = base.with_extension("meta.json");

    let markdown = render_markdown(entries);
    let jsonl = render_jsonl(entries)?;
    let checkpoint = render_checkpoint(summary);
    let meta = CompactionArchiveMeta {
        archive_id: archive_id.clone(),
        start_entry_id: "entry-0".to_string(),
        end_entry_id: format!("entry-{}", entries.len().saturating_sub(1)),
        before_tokens,
        after_tokens,
        summary_hash: sha256_hex(summary.as_bytes()),
        checkpoint_hash: sha256_hex(checkpoint.as_bytes()),
        artifacts: collect_artifacts(entries),
        created_at: chrono::Utc::now().timestamp_millis(),
    };
    let meta_json = serde_json::to_vec_pretty(&meta)?;

    lock::write_atomic(&markdown_path, markdown.as_bytes())?;
    lock::write_atomic(&jsonl_path, jsonl.as_bytes())?;
    lock::write_atomic(&meta_path, &meta_json)?;

    Ok(CompactionArchive {
        archive_id,
        markdown_path,
        jsonl_path,
        meta_path,
        meta,
    })
}

fn archive_id() -> String {
    chrono::Utc::now().format("%Y%m%d%H%M%S%.3f").to_string()
}

fn render_markdown(entries: &[TranscriptEntry]) -> String {
    let mut out = String::new();
    out.push_str("# Compacted transcript archive\n\n");
    for (idx, entry) in entries.iter().enumerate() {
        match entry {
            TranscriptEntry::User(user) => {
                out.push_str(&format!("## entry-{idx} user\n\n{}\n\n", user.text));
            }
            TranscriptEntry::Assistant(assistant) => {
                out.push_str(&format!("## entry-{idx} assistant\n\n{}\n\n", assistant.text));
                if !assistant.tool_calls.is_empty() {
                    out.push_str("Tool calls:\n");
                    for call in &assistant.tool_calls {
                        out.push_str(&format!("- {} `{}`\n", call.name, call.id));
                    }
                    out.push('\n');
                }
            }
            TranscriptEntry::ToolResults(results) => {
                out.push_str(&format!("## entry-{idx} tool results\n\n"));
                for result in results {
                    out.push_str(&format!(
                        "### {} `{}`\n\n{}\n\n",
                        result.name, result.call_id, result.content
                    ));
                }
            }
        }
    }
    out
}

fn render_jsonl(entries: &[TranscriptEntry]) -> AppResult<String> {
    let mut out = String::new();
    for (idx, entry) in entries.iter().enumerate() {
        let line = match entry {
            TranscriptEntry::User(user) => serde_json::json!({
                "entry_id": format!("entry-{idx}"),
                "role": "user",
                "text": user.text,
                "attachments_count": user.attachments.len(),
            }),
            TranscriptEntry::Assistant(assistant) => serde_json::json!({
                "entry_id": format!("entry-{idx}"),
                "role": "assistant",
                "text": assistant.text,
                "reasoning": assistant.reasoning,
                "tool_calls": assistant.tool_calls.iter().map(|call| serde_json::json!({
                    "id": call.id,
                    "name": call.name,
                    "input": call.input,
                })).collect::<Vec<_>>(),
            }),
            TranscriptEntry::ToolResults(results) => serde_json::json!({
                "entry_id": format!("entry-{idx}"),
                "role": "tool_results",
                "results": results.iter().map(|result| serde_json::json!({
                    "call_id": result.call_id,
                    "name": result.name,
                    "content": result.content,
                })).collect::<Vec<_>>(),
            }),
        };
        out.push_str(&serde_json::to_string(&line)?);
        out.push('\n');
    }
    Ok(out)
}

fn render_checkpoint(summary: &str) -> String {
    format!("[前情概要]\n{}", summary.trim())
}

fn collect_artifacts(entries: &[TranscriptEntry]) -> Vec<String> {
    let mut artifacts = Vec::new();
    for entry in entries {
        let TranscriptEntry::ToolResults(results) = entry else {
            continue;
        };
        for result in results {
            if let Some(path) = result
                .content
                .lines()
                .find_map(|line| line.strip_prefix("Full output: "))
            {
                artifacts.push(path.trim().to_string());
            }
            let needle = "Full output: ";
            if let Some(pos) = result.content.find(needle) {
                let rest = &result.content[pos + needle.len()..];
                if let Some(path) = rest.split_whitespace().next() {
                    let path = path.trim_matches(|c| c == ']' || c == ';');
                    if !path.is_empty() {
                        artifacts.push(path.to_string());
                    }
                }
            }
        }
    }
    artifacts.sort();
    artifacts.dedup();
    artifacts
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_gateway::types::{ToolResult, UserEntry};

    #[test]
    fn save_archive_writes_markdown_jsonl_and_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            TranscriptEntry::User(UserEntry::text("hello")),
            TranscriptEntry::ToolResults(vec![ToolResult {
                call_id: "call_1".to_string(),
                name: "Bash".to_string(),
                content: "[工具输出过长]\nFull output: tool_results/call_1.txt".to_string(),
                artifact: None,
                attachments: Vec::new(),
            }]),
        ];

        let archive = save_compaction_archive(tmp.path(), "sid", &entries, 100, 20, "summary")
            .expect("archive should save");

        assert!(archive.markdown_path.exists());
        assert!(archive.jsonl_path.exists());
        assert!(archive.meta_path.exists());
        let markdown = std::fs::read_to_string(&archive.markdown_path).unwrap();
        assert!(markdown.contains("entry-0 user"));
        let jsonl = std::fs::read_to_string(&archive.jsonl_path).unwrap();
        assert_eq!(jsonl.lines().count(), 2);
        let meta: CompactionArchiveMeta =
            serde_json::from_str(&std::fs::read_to_string(&archive.meta_path).unwrap()).unwrap();
        assert_eq!(meta.before_tokens, 100);
        assert_eq!(meta.after_tokens, 20);
        assert_eq!(meta.artifacts, vec!["tool_results/call_1.txt"]);
        assert_eq!(meta.summary_hash, sha256_hex(b"summary"));
    }
}
