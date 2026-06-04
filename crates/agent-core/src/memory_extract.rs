//! 后台记忆抽取（架构 §4.14）。
//!
//! 每个 Run 的 agent_loop 跑完（`RunFinished`）后，由 [`crate::harness`] spawn 一个独立
//! task 调 [`extract_for_session`]：把游标之后的新对话喂给一个便宜模型，让它产出
//! 「值得跨会话长期记住」的候选记忆，去重后写入 `storage::memory`。
//!
//! 设计要点：
//! - **fallback 链**：按 [`MemorySettings::models`] 顺序尝试，每个模型最多重试
//!   [`MAX_RETRIES_PER_MODEL`] 次仍失败 → 下一个；全链耗尽 → 整轮失败。
//! - **补抽游标**：成功才推进 [`storage::memory::write_cursor`]；失败不动游标，下次
//!   Run 结束自动把这段连同新增对话一起重抽。
//! - **与主 run 解耦**：失败只 warn + emit 一个事件（surface 弹 toast），绝不影响对话。
//! - 抽取本身**不进 transcript、不进 agent loop**——纯派生，和 session_titler 同一性质。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use common::{CancelFlag, ReasoningConfig};
use model_gateway::types::{ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry};
use serde::Deserialize;

use crate::storage::memory::{self, mem_log, mem_warn, MemoryL0, MemoryScope};
use crate::storage::sessions::{self, Role, Session};
use crate::storage::settings;
use crate::tools::memory_project_workdir;

/// 每个模型最多重试次数，超过则 fallback 到链上下一个。
pub const MAX_RETRIES_PER_MODEL: u32 = 5;

const EXTRACT_MAX_TOKENS: u32 = 2048;

/// 一次抽取的结果摘要，供事件 / 日志使用。
#[derive(Debug, Clone)]
pub struct ExtractionResult {
    /// 本轮新写入 / 更新的记忆 L0。
    pub written: Vec<MemoryWrite>,
    /// 实际命中的模型（fallback 链上成功的那个）。
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct MemoryWrite {
    pub id: String,
    pub summary: String,
    pub scope: MemoryScope,
}

/// 模型返回的单条候选记忆（JSON 数组里的一项）。
#[derive(Debug, Deserialize)]
struct Candidate {
    /// "project" | "global"
    scope: String,
    category: String,
    /// 稳定标识；同 key 覆盖更新。
    key: String,
    summary: String,
    content: String,
}

/// 后台入口：抽取 `session_id` 游标之后的新对话。
///
/// 返回 `Ok(Some(result))` 表示抽取成功（可能写了 0 条——没有可记的事实也算成功，
/// 推进游标）；`Ok(None)` 表示无需抽取（功能关 / 没新消息）；`Err` 表示 fallback 链
/// 耗尽，游标不推进，留待下次补抽。
pub async fn extract_for_session(
    data_dir: &Path,
    session_id: &str,
) -> Result<Option<ExtractionResult>, ExtractError> {
    let app_settings = settings::load(data_dir);
    if !app_settings.memory.enabled || app_settings.memory.models.is_empty() {
        return Ok(None);
    }

    let session = sessions::load(data_dir, session_id).map_err(ExtractError::other)?;

    // 游标之后的新消息——这是本轮要抽取的增量。
    let cursor = memory::read_cursor(data_dir, session_id);
    let new_messages = messages_after_cursor(&session, cursor.as_deref());
    if new_messages.is_empty() {
        mem_log!("Extract", "跳过：游标之后无新消息 session={session_id}");
        return Ok(None);
    }
    let last_msg_id = new_messages.last().map(|m| m.id.clone());
    mem_log!(
        "Extract",
        "开始 session={session_id} 新消息 {} 条 游标={cursor:?}",
        new_messages.len()
    );

    let project_workdir = session.workdir.as_deref().and_then(memory_project_workdir);

    // 现有 L0 清单（global + project）作为去重上下文喂给模型。
    let existing = collect_existing_l0(data_dir, project_workdir.as_deref());

    let transcript_text = render_transcript(&new_messages);
    let prompt = build_extract_prompt(&existing, &transcript_text, project_workdir.is_some());

    // fallback 链：逐个模型尝试，每个最多重试 MAX_RETRIES_PER_MODEL 次。
    // 全链耗尽时也写一条 "failed" 审计——成功 / 失败都在 .memory_log.jsonl 留痕，
    // 便于事后排查「这段为什么没抽出来」（游标已保留，下个 Run 会补抽）。
    let raw = match run_fallback_chain(data_dir, &app_settings.memory.models, &prompt).await {
        Ok(raw) => raw,
        Err(e) => {
            mem_warn!("Extract", "失败 session={session_id}：{e}");
            log_outcome(
                data_dir,
                project_workdir.as_deref(),
                "failed",
                None,
                &e.to_string(),
            );
            return Err(e);
        }
    };

    let candidates = parse_candidates(&raw.text);
    let written = persist_candidates(data_dir, project_workdir.as_deref(), candidates);

    // 成功（哪怕 0 条）→ 推进游标。
    if let Some(mid) = last_msg_id {
        if let Err(e) = memory::write_cursor(data_dir, session_id, &mid) {
            mem_warn!("Cursor", "推进失败 session={session_id}：{e}");
        }
    }

    mem_log!(
        "Extract",
        "完成 session={session_id} 写入 {} 条 模型={}",
        written.len(),
        raw.model
    );
    log_outcome(
        data_dir,
        project_workdir.as_deref(),
        "extracted",
        Some(&raw.model),
        &format!("写入 {} 条", written.len()),
    );

    Ok(Some(ExtractionResult {
        written,
        model: raw.model,
    }))
}

/// 截取游标之后的消息。游标为 `None`（从未抽过）时返回全部；游标 message 不在
/// session 里（被压缩/裁剪）时保守返回全部，靠去重兜底避免漏抽。
fn messages_after_cursor<'a>(
    session: &'a Session,
    cursor: Option<&str>,
) -> Vec<&'a sessions::Message> {
    let msgs: Vec<&sessions::Message> = session
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant) && !m.content.trim().is_empty())
        .collect();
    let Some(cursor) = cursor else {
        return msgs;
    };
    match msgs.iter().position(|m| m.id == cursor) {
        Some(idx) => msgs[idx + 1..].to_vec(),
        None => msgs,
    }
}

fn render_transcript(messages: &[&sessions::Message]) -> String {
    let mut s = String::new();
    for m in messages {
        let role = match m.role {
            Role::User => "用户",
            Role::Assistant => "助手",
            _ => continue,
        };
        s.push_str(&format!("【{role}】{}\n\n", m.content.trim()));
    }
    s
}

fn collect_existing_l0(data_dir: &Path, project_workdir: Option<&Path>) -> Vec<MemoryL0> {
    let mut out = memory::list_l0(data_dir, None, MemoryScope::Global).unwrap_or_default();
    if let Some(wd) = project_workdir {
        if let Ok(mut v) = memory::list_l0(data_dir, Some(wd), MemoryScope::Project) {
            out.append(&mut v);
        }
    }
    out
}

/// 把候选记忆写入 storage::memory。scope=project 但当前无项目时降级 global。
fn persist_candidates(
    data_dir: &Path,
    project_workdir: Option<&Path>,
    candidates: Vec<Candidate>,
) -> Vec<MemoryWrite> {
    let mut written = Vec::new();
    for c in candidates {
        let want_project = c.scope.eq_ignore_ascii_case("project");
        let (scope, workdir) = if want_project && project_workdir.is_some() {
            (MemoryScope::Project, project_workdir)
        } else {
            (MemoryScope::Global, None)
        };
        match memory::write(
            data_dir,
            workdir,
            scope,
            &c.key,
            &c.category,
            &c.summary,
            &c.content,
        ) {
            Ok(l0) => written.push(MemoryWrite {
                id: l0.id,
                summary: l0.summary,
                scope,
            }),
            Err(e) => mem_warn!("Write", "候选写入失败 key={}：{e}", c.key),
        }
    }
    written
}

fn log_outcome(
    data_dir: &Path,
    project_workdir: Option<&Path>,
    outcome: &str,
    model: Option<&str>,
    detail: &str,
) {
    let scope = if project_workdir.is_some() {
        MemoryScope::Project
    } else {
        MemoryScope::Global
    };
    let mut entry = memory::MemoryLogEntry::new(outcome, detail);
    entry.model = model.map(|s| s.to_string());
    let _ = memory::append_log(data_dir, project_workdir, scope, &entry);
}

// ── fallback 链 ──────────────────────────────────────────────────────────────

struct RawExtraction {
    text: String,
    model: String,
}

/// 按 `models` 顺序尝试；每个模型最多重试 [`MAX_RETRIES_PER_MODEL`] 次。
/// 全部失败 → `Err(ExtractError::AllModelsFailed)`，调用方据此不推进游标。
async fn run_fallback_chain(
    data_dir: &Path,
    models: &[settings::MemoryModelRef],
    prompt: &str,
) -> Result<RawExtraction, ExtractError> {
    let mut last_err = String::from("无可用模型");
    for m in models {
        for attempt in 1..=MAX_RETRIES_PER_MODEL {
            match call_model(data_dir, &m.provider_id, &m.model, prompt).await {
                Ok(text) => {
                    return Ok(RawExtraction {
                        text,
                        model: format!("{}/{}", m.provider_id, m.model),
                    });
                }
                Err(e) => {
                    last_err = format!("{}/{} 第{attempt}次: {e}", m.provider_id, m.model);
                    mem_warn!(
                        "Extract",
                        "模型调用失败 {}/{} 第{attempt}次：{e}",
                        m.provider_id,
                        m.model
                    );
                }
            }
        }
    }
    Err(ExtractError::AllModelsFailed(last_err))
}

/// 一次模型调用：取 provider → 刷新 token → build_client → complete。
async fn call_model(
    data_dir: &Path,
    provider_id: &str,
    model: &str,
    prompt: &str,
) -> Result<String, ModelError> {
    let provider = model_gateway::config::get(data_dir, provider_id)
        .map_err(|e| ModelError::Other(format!("provider 不存在: {e}")))?;
    let provider = model_gateway::auth::refresh::ensure_fresh_provider_token(data_dir, provider)
        .await
        .map_err(|e| ModelError::Other(format!("token 刷新失败: {e}")))?;
    let client = model_gateway::build_client(provider)?;

    let req = ModelRequest {
        model: model.into(),
        system: Some(EXTRACT_SYSTEM.into()),
        entries: vec![TranscriptEntry::User(UserEntry::text(prompt))],
        tools: vec![],
        max_tokens: EXTRACT_MAX_TOKENS,
        reasoning: Some(ReasoningConfig {
            enabled: Some(false),
            effort: None,
            long_context: None,
        }),
    };
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    match client.complete(req, cancel).await? {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => Ok(text),
    }
}

// ── prompt ───────────────────────────────────────────────────────────────────

const EXTRACT_SYSTEM: &str = "你是一个记忆抽取器。从对话里提炼「值得跨会话长期记住」的事实，\
    只输出一个 JSON 数组，不要任何解释 / markdown 代码围栏。每项形如：\
    {\"scope\":\"project|global\",\"category\":\"\",\"key\":\"\",\"summary\":\"\",\"content\":\"\"}。\
    没有值得记的就输出 []。";

fn build_extract_prompt(existing: &[MemoryL0], transcript: &str, has_project: bool) -> String {
    let mut s = String::new();
    s.push_str(
        "从下面这段对话里抽取值得长期记住的事实。判定标准：跨会话仍成立的项目结构 / \
         架构 / 命名约定 / 踩过的坑 / 用户长期偏好。**不要**记当前 session 的临时状态、\
         正在调试的中间结论、一次性的具体数值。\n\n",
    );
    if has_project {
        s.push_str(
            "scope 规则：项目特定的事实用 \"project\"；跨项目通用的用户偏好用 \"global\"。\n\n",
        );
    } else {
        s.push_str("当前对话未绑定项目，所有记忆一律用 scope=\"global\"。\n\n");
    }
    if !existing.is_empty() {
        s.push_str("已有记忆（key 相同视为更新，避免重复创建语义相同的新条目）：\n");
        for m in existing {
            s.push_str(&format!("- [{}] {}\n", m.id, m.summary));
        }
        s.push('\n');
    }
    s.push_str("对话内容：\n");
    s.push_str(transcript);
    s.push_str("\n只输出 JSON 数组。");
    s
}

/// 容错解析：模型可能裹 markdown 围栏 / 前后有杂字，截取第一个 `[` 到最后一个 `]`。
fn parse_candidates(raw: &str) -> Vec<Candidate> {
    let start = raw.find('[');
    let end = raw.rfind(']');
    let json = match (start, end) {
        (Some(s), Some(e)) if e > s => &raw[s..=e],
        _ => return Vec::new(),
    };
    match serde_json::from_str::<Vec<Candidate>>(json) {
        Ok(v) => v
            .into_iter()
            .filter(|c| !c.key.trim().is_empty() && !c.summary.trim().is_empty())
            .collect(),
        Err(e) => {
            mem_warn!("Extract", "候选 JSON 解析失败：{e}");
            Vec::new()
        }
    }
}

// ── 错误类型 ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ExtractError {
    /// fallback 链全部耗尽（每个模型都重试满了仍失败）。
    AllModelsFailed(String),
    /// 其他（session 读取失败等）。
    Other(String),
}

impl ExtractError {
    fn other(e: impl std::fmt::Display) -> Self {
        ExtractError::Other(e.to_string())
    }
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::AllModelsFailed(s) => write!(f, "所有记忆抽取模型都失败了：{s}"),
            ExtractError::Other(s) => write!(f, "{s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_candidates_handles_code_fence() {
        let raw = "```json\n[{\"scope\":\"global\",\"category\":\"pref\",\"key\":\"lang\",\"summary\":\"中文\",\"content\":\"用中文\"}]\n```";
        let c = parse_candidates(raw);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].key, "lang");
    }

    #[test]
    fn parse_candidates_empty_array() {
        assert!(parse_candidates("[]").is_empty());
        assert!(parse_candidates("没有可记的内容").is_empty());
    }

    #[test]
    fn parse_candidates_filters_blank_key() {
        let raw = r#"[{"scope":"global","category":"c","key":"","summary":"s","content":"x"}]"#;
        assert!(parse_candidates(raw).is_empty());
    }

    #[test]
    fn build_prompt_global_only_when_no_project() {
        let p = build_extract_prompt(&[], "【用户】hi", false);
        assert!(p.contains("一律用 scope=\"global\""));
    }
}
