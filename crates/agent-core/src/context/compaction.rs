use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::context::budget;
use crate::definition::CompactionPolicy;
use model_gateway::client::ModelClient;
use model_gateway::types::{
    AssistantEntry, ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry,
};

/// 默认的中文压缩 prompt：参考 codex / claude-code 的 summarization 模板，
/// 但调成项目内统一的中文风格。
pub const COMPACT_PROMPT: &str = "你正在执行【上下文压缩】。请把当前对话历史浓缩成一份简明、结构化的接力摘要，让另一个 LLM 能在不读原对话的情况下无缝继续工作。\n\n请覆盖：\n- 用户的核心目标 / 约束 / 偏好\n- 已完成的关键工作和重要决策（含影响后续判断的细节）\n- 仍未完成的事项 / 下一步\n- 关键数据：文件路径、命令、代码片段、错误信息、外部链接\n- 任何模型不读上下文就会丢的隐含上下文\n\n输出要求：\n- 直接给摘要正文，不要寒暄、不要 “以下是摘要” 之类的引导语\n- 紧凑但不丢关键信息；优先 bullet list\n- 保持中文";

/// 压缩结果：用于 surface 端展示前后对比。
pub struct CompactionResult {
    pub entries: Vec<TranscriptEntry>,
    pub before_tokens: usize,
    pub after_tokens: usize,
    /// LLM 摘要时填入；结构化裁剪时为空。
    pub summary: String,
}

/// 结构化裁剪：保留 system + 最近 keep_recent_turns 轮
pub fn compact_structural(
    system: Option<&str>,
    entries: Vec<TranscriptEntry>,
    policy: &CompactionPolicy,
) -> CompactionResult {
    let before_tokens = budget::estimate_transcript_tokens(system, &entries);

    if before_tokens <= policy.token_budget {
        let after_tokens = before_tokens;
        return CompactionResult {
            entries,
            before_tokens,
            after_tokens,
            summary: String::new(),
        };
    }

    let keep = policy.keep_recent_turns * 3;
    let total = entries.len();
    let start = if total > keep { total - keep } else { 0 };

    let mut start = start;
    while start < total {
        if matches!(entries[start], TranscriptEntry::User(_)) {
            break;
        }
        start += 1;
    }

    let compacted: Vec<TranscriptEntry> = entries.into_iter().skip(start).collect();
    let after_tokens = budget::estimate_transcript_tokens(system, &compacted);

    CompactionResult {
        entries: compacted,
        before_tokens,
        after_tokens,
        summary: String::new(),
    }
}

/// 检查是否需要压缩
pub fn needs_compaction(
    system: Option<&str>,
    entries: &[TranscriptEntry],
    policy: &CompactionPolicy,
) -> bool {
    budget::estimate_transcript_tokens(system, entries) > policy.token_budget
}

/// LLM 摘要式压缩：用一次 `complete()` 调用把整段对话浓缩成一份简明摘要，
/// 然后用 `[前情概要 + assistant 确认]` 这一对消息替换原 entries。
pub async fn compact_with_llm(
    client: &dyn ModelClient,
    system: Option<&str>,
    entries: Vec<TranscriptEntry>,
    custom_instructions: Option<&str>,
) -> Result<CompactionResult, ModelError> {
    let before_tokens = budget::estimate_transcript_tokens(system, &entries);

    let mut summarize_entries = entries;
    let prompt = match custom_instructions.map(str::trim).filter(|s| !s.is_empty()) {
        Some(extra) => format!("{COMPACT_PROMPT}\n\n附加指令：{extra}"),
        None => COMPACT_PROMPT.to_string(),
    };
    summarize_entries.push(TranscriptEntry::User(UserEntry::text(prompt)));

    let req = ModelRequest {
        model: String::new(),
        system: system.map(str::to_string),
        entries: summarize_entries,
        tools: Vec::new(),
        max_tokens: 4096,
        reasoning: None,
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let summary = match client.complete(req, cancel).await? {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text,
    };

    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err(ModelError::Other("压缩失败：模型返回了空摘要".to_string()));
    }

    let new_entries = vec![
        TranscriptEntry::User(UserEntry::text(format!("[前情概要]\n{summary}"))),
        TranscriptEntry::Assistant(AssistantEntry {
            text: "已收到前情概要，将基于此继续。".to_string(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
        }),
    ];
    let after_tokens = budget::estimate_transcript_tokens(system, &new_entries);

    Ok(CompactionResult {
        entries: new_entries,
        before_tokens,
        after_tokens,
        summary,
    })
}
