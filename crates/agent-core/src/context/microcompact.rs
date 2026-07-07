//! Pressure Microcompact：上下文有压力时把老工具结果 shadow。
//!
//! 每轮模型请求**之前**估算当前上下文压力。低于 50% 不改历史；达到压力线后，
//! 只把最近 turns 之外、可再生的大工具结果替换成占位符，原文落到
//! `~/.hebbian/sessions/<sid>/tool_results/<call_id>.txt`，agent 按需用 Grep/Read 检索。

use crate::context::{
    budget::{calibrated_transcript_tokens, estimate_tokens},
    tool_output::{head_tail_preview, sanitize_tool_output, PreviewPolicy},
};
use crate::definition::CompactionPolicy;
use model_gateway::types::{ToolResult, TranscriptEntry};

/// 进入压缩白名单的工具名称。这些工具的大输出“看过即过”，不值得长期占 context。
const COMPACTABLE_TOOLS: &[&str] = &["Bash", "Read", "Grep", "Glob", "Edit", "Fetch", "WebSearch"];

/// L2 压力线：低于 50% 不改 transcript。
const DEFAULT_MIN_PRESSURE: f64 = 0.50;
/// L2 单条候选的下限：避免把很多小结果压成噪声 marker。
const DEFAULT_MIN_RESULT_TOKENS: usize = 2_000;
/// 占位符预览上限。L2 marker 保留少量 head/tail，避免完全失去局部线索。
const SHADOW_PREVIEW_CHARS: usize = 800;

/// 占位符前缀，用于幂等检测。
pub const SHADOWED_PLACEHOLDER_PREFIX: &str = "[结果已被压缩";

/// Microcompact 配置。
#[derive(Debug, Clone, Copy)]
pub struct MicrocompactPolicy {
    /// 上下文压力低于此比例时不做 microcompact。
    pub min_pressure: f64,
    /// 单条可压缩工具结果至少超过此 token 数才成为候选。
    pub min_tokens_per_result: usize,
    /// 保护最近 N 个完整 user-started turns。
    pub recent_turns_to_keep: usize,
}

impl Default for MicrocompactPolicy {
    fn default() -> Self {
        Self {
            min_pressure: DEFAULT_MIN_PRESSURE,
            min_tokens_per_result: DEFAULT_MIN_RESULT_TOKENS,
            recent_turns_to_keep: 3,
        }
    }
}

/// 一次 microcompact 的统计。
#[derive(Debug, Clone, Default)]
pub struct MicrocompactReport {
    pub shadowed_count: usize,
    pub kept_count: usize,
    pub total_compactable: usize,
    pub pressure: f64,
    /// 被压缩的工具结果备份：`(call_id, sanitized_content)`。
    /// agent_loop 拿到后落盘到 `tool_results/<call_id>.txt`。
    pub shadowed_artifacts: Vec<(String, String)>,
}

fn is_compactable(name: &str) -> bool {
    COMPACTABLE_TOOLS.iter().any(|n| *n == name)
}

fn is_already_shadowed(content: &str) -> bool {
    content.starts_with(SHADOWED_PLACEHOLDER_PREFIX)
}

/// 兼容单测与旧调用：只做单条阈值 microcompact，不按压力判断，也不保护 suffix。
pub fn microcompact(
    entries: &mut [TranscriptEntry],
    policy: &MicrocompactPolicy,
) -> MicrocompactReport {
    let mut policy = *policy;
    policy.recent_turns_to_keep = 0;
    microcompact_entries(entries, 1.0, &policy)
}

/// 按当前上下文压力做 L2 microcompact。
pub fn microcompact_with_pressure(
    system: Option<&str>,
    entries: &mut [TranscriptEntry],
    compaction_policy: &CompactionPolicy,
    last_real: u64,
    last_estimated: u64,
    policy: &MicrocompactPolicy,
) -> MicrocompactReport {
    let estimated = calibrated_transcript_tokens(system, entries, last_real, last_estimated);
    let pressure = if compaction_policy.token_budget == 0 {
        1.0
    } else {
        estimated as f64 / compaction_policy.token_budget as f64
    };
    if pressure < policy.min_pressure {
        return MicrocompactReport {
            pressure,
            ..Default::default()
        };
    }
    microcompact_entries(entries, pressure, policy)
}

fn microcompact_entries(
    entries: &mut [TranscriptEntry],
    pressure: f64,
    policy: &MicrocompactPolicy,
) -> MicrocompactReport {
    let protected_from = protected_suffix_start(entries, policy.recent_turns_to_keep);
    let mut shadowed = 0usize;
    let mut kept = 0usize;
    let mut total = 0usize;
    let mut artifacts: Vec<(String, String)> = Vec::new();

    for (entry_index, entry) in entries.iter_mut().enumerate() {
        let TranscriptEntry::ToolResults(results) = entry else {
            continue;
        };
        for r in results.iter_mut() {
            if !is_compactable(&r.name) {
                continue;
            }
            total += 1;
            if is_already_shadowed(&r.content) {
                shadowed += 1;
                continue;
            }
            if entry_index >= protected_from {
                kept += 1;
                continue;
            }
            let token_estimate = estimate_tokens(&r.content);
            if token_estimate < policy.min_tokens_per_result {
                kept += 1;
                continue;
            }

            let sanitized = sanitize_tool_output(&r.content);
            let artifact_content = sanitized.text;
            let original_bytes = r.content.len();
            let sanitized_bytes = artifact_content.len();
            let line_count = artifact_content.lines().count();
            let preview = head_tail_preview(
                &artifact_content,
                PreviewPolicy::new(SHADOW_PREVIEW_CHARS, false, &artifact_content),
            );
            artifacts.push((r.call_id.clone(), artifact_content));
            let placeholder = format!(
                "[结果已被压缩。Tool: {tool}; Call ID: {call_id}; Original: {original_bytes} bytes; Sanitized: {sanitized_bytes} bytes / {line_count} lines; Full output: tool_results/{call_id}.txt]\n\n{preview}\n\nNeed details? Use Grep on the artifact first, then Read with offset/limit.",
                tool = r.name,
                call_id = r.call_id,
                preview = preview.text,
            );
            *r = ToolResult {
                call_id: r.call_id.clone(),
                name: r.name.clone(),
                content: placeholder,
                artifact: None,
                attachments: Vec::new(),
            };
            shadowed += 1;
        }
    }

    MicrocompactReport {
        shadowed_count: shadowed,
        kept_count: kept,
        total_compactable: total,
        pressure,
        shadowed_artifacts: artifacts,
    }
}

fn protected_suffix_start(entries: &[TranscriptEntry], recent_turns_to_keep: usize) -> usize {
    if recent_turns_to_keep == 0 {
        return entries.len();
    }
    let mut seen_turns = 0usize;
    for (idx, entry) in entries.iter().enumerate().rev() {
        if matches!(entry, TranscriptEntry::User(_)) {
            seen_turns += 1;
            if seen_turns == recent_turns_to_keep {
                return idx;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{CompactionPolicy, CompactionStrategy};
    use model_gateway::types::{ToolResult, UserEntry};

    fn tr(name: &str, content: &str) -> ToolResult {
        ToolResult {
            call_id: format!("c-{name}"),
            name: name.to_string(),
            content: content.to_string(),
            artifact: None,
            attachments: Vec::new(),
        }
    }

    fn large_content(tokens: usize) -> String {
        // estimate_tokens: bytes/4 for ASCII，所以 tokens*4 个字符 ≈ tokens token
        "x".repeat(tokens * 4)
    }

    #[test]
    fn small_result_not_shadowed() {
        let mut entries = vec![TranscriptEntry::ToolResults(vec![
            tr("Bash", "hello"),
            tr("Read", "short content"),
        ])];
        let report = microcompact(&mut entries, &MicrocompactPolicy::default());
        assert_eq!(report.shadowed_count, 0);
        assert_eq!(report.kept_count, 2);
        assert_eq!(report.total_compactable, 2);
    }

    #[test]
    fn large_result_immediately_shadowed() {
        let big = large_content(11_000); // > 10k token
        let mut entries = vec![TranscriptEntry::ToolResults(vec![tr("Bash", &big)])];
        let report = microcompact(&mut entries, &MicrocompactPolicy::default());
        assert_eq!(report.shadowed_count, 1);
        assert_eq!(report.kept_count, 0);
        assert_eq!(report.shadowed_artifacts.len(), 1);
        if let TranscriptEntry::ToolResults(results) = &entries[0] {
            assert!(results[0].content.starts_with(SHADOWED_PLACEHOLDER_PREFIX));
        }
    }

    #[test]
    fn small_results_always_kept_regardless_of_count() {
        // 20 个小结果，全保留（不因数量多就压）
        let items: Vec<ToolResult> = (0..20)
            .map(|i| tr("Bash", &format!("output {i}")))
            .collect();
        let mut entries = vec![TranscriptEntry::ToolResults(items)];
        let report = microcompact(&mut entries, &MicrocompactPolicy::default());
        assert_eq!(report.shadowed_count, 0);
        assert_eq!(report.kept_count, 20);
    }

    #[test]
    fn mix_large_and_small() {
        let big = large_content(11_000);
        let mut entries = vec![TranscriptEntry::ToolResults(vec![
            tr("Bash", "small"),
            tr("Read", &big),
            tr("Grep", "small grep"),
        ])];
        let report = microcompact(&mut entries, &MicrocompactPolicy::default());
        assert_eq!(report.shadowed_count, 1); // only Read
        assert_eq!(report.kept_count, 2);
        if let TranscriptEntry::ToolResults(results) = &entries[0] {
            assert_eq!(results[0].content, "small");
            assert!(results[1].content.starts_with(SHADOWED_PLACEHOLDER_PREFIX));
            assert_eq!(results[2].content, "small grep");
        }
    }

    #[test]
    fn non_compactable_tools_skipped() {
        let big = large_content(11_000);
        let mut entries = vec![TranscriptEntry::ToolResults(vec![
            tr("ask", &big),
            tr("TodoWrite", &big),
        ])];
        let report = microcompact(&mut entries, &MicrocompactPolicy::default());
        assert_eq!(report.total_compactable, 0);
        assert_eq!(report.shadowed_count, 0);
    }

    #[test]
    fn idempotent() {
        let big = large_content(11_000);
        let mut entries = vec![TranscriptEntry::ToolResults(vec![tr("Bash", &big)])];
        let r1 = microcompact(&mut entries, &MicrocompactPolicy::default());
        let r2 = microcompact(&mut entries, &MicrocompactPolicy::default());
        assert_eq!(r1.shadowed_artifacts.len(), 1);
        assert_eq!(r2.shadowed_artifacts.len(), 0); // 第二次不再重复落 artifacts
    }

    #[test]
    fn pressure_below_threshold_does_not_touch_history() {
        let big = large_content(11_000);
        let mut entries = vec![TranscriptEntry::ToolResults(vec![tr("Bash", &big)])];
        let policy = CompactionPolicy {
            token_budget: 1_000_000,
            keep_recent_turns: 8,
            strategy: CompactionStrategy::Structural,
        };

        let report = microcompact_with_pressure(
            None,
            &mut entries,
            &policy,
            0,
            0,
            &MicrocompactPolicy::default(),
        );

        assert_eq!(report.shadowed_count, 0);
        assert_eq!(report.total_compactable, 0);
        if let TranscriptEntry::ToolResults(results) = &entries[0] {
            assert_eq!(results[0].content, big);
        }
    }

    #[test]
    fn pressure_microcompact_protects_recent_turns() {
        let old_big = "old line\n".repeat(4_000);
        let recent_big = "recent line\n".repeat(4_000);
        let mut entries = vec![
            TranscriptEntry::User(UserEntry::text("old turn")),
            TranscriptEntry::ToolResults(vec![tr("Bash", &old_big)]),
            TranscriptEntry::User(UserEntry::text("recent turn")),
            TranscriptEntry::ToolResults(vec![tr("Bash", &recent_big)]),
        ];
        let policy = CompactionPolicy {
            token_budget: 20_000,
            keep_recent_turns: 8,
            strategy: CompactionStrategy::Structural,
        };
        let micro_policy = MicrocompactPolicy {
            min_pressure: 0.50,
            min_tokens_per_result: 2_000,
            recent_turns_to_keep: 1,
        };

        let report = microcompact_with_pressure(None, &mut entries, &policy, 0, 0, &micro_policy);

        assert_eq!(report.shadowed_count, 1);
        assert_eq!(report.kept_count, 1);
        if let TranscriptEntry::ToolResults(results) = &entries[1] {
            assert!(results[0].content.starts_with(SHADOWED_PLACEHOLDER_PREFIX));
            assert!(results[0].content.contains("BEGIN HEAD"));
            assert!(results[0].content.contains("Full output: tool_results/c-Bash.txt"));
        } else {
            panic!("old result should remain a tool result entry");
        }
        if let TranscriptEntry::ToolResults(results) = &entries[3] {
            assert_eq!(results[0].content, recent_big);
        } else {
            panic!("recent result should remain a tool result entry");
        }
    }

    #[test]
    fn shadowed_artifact_is_sanitized() {
        let raw = format!("\u{1b}[31mHEAD\u{1b}[0m token={}\n{}", "a".repeat(900), large_content(3_000));
        let mut entries = vec![TranscriptEntry::ToolResults(vec![tr("Bash", &raw)])];
        let report = microcompact(&mut entries, &MicrocompactPolicy::default());

        assert_eq!(report.shadowed_artifacts.len(), 1);
        let artifact = &report.shadowed_artifacts[0].1;
        assert!(artifact.contains("HEAD"));
        assert!(artifact.contains("[REDACTED:secret_assignment]"));
        assert!(!artifact.contains("\u{1b}[31m"));
        assert!(!artifact.contains(&"a".repeat(100)));
    }
}
