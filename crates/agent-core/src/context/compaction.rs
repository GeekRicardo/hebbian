use crate::context::budget;
use crate::definition::CompactionPolicy;
use model_gateway::types::TranscriptEntry;

/// 压缩结果
pub struct CompactionResult {
    pub entries: Vec<TranscriptEntry>,
    pub before_tokens: usize,
    pub after_tokens: usize,
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
