//! Microcompact：工具结果大输出 shadow。
//!
//! 每轮模型请求**之前**扫描 transcript，把单条 token 超限的工具结果替换成占位符，
//! 原文通过 [`crate::storage::tool_results::save_tool_result`] 落盘
//! `~/.hebbian/sessions/<sid>/tool_results/<call_id>.txt`，agent 按需用 Grep/Read 检索。
//!
//! 触发条件：单条可压缩工具结果 token 数 > `max_tokens_per_result`（默认 10,000）。
//! 不看累积数量——超限即压，小输出永远保留。

use crate::context::budget::estimate_tokens;
use model_gateway::types::{ToolResult, TranscriptEntry};

/// 进入压缩白名单的工具名称。这些工具的大输出"看过即过"，不值得占 context。
const COMPACTABLE_TOOLS: &[&str] = &["Bash", "Read", "Grep", "Glob", "Edit", "Fetch", "WebSearch"];

/// 占位符前缀，用于幂等检测。
pub const SHADOWED_PLACEHOLDER_PREFIX: &str = "[结果已被压缩";

/// Microcompact 配置。
#[derive(Debug, Clone, Copy)]
pub struct MicrocompactPolicy {
    /// 单条可压缩工具结果超过此 token 数时 shadow（架构 §4.7.3）。
    pub max_tokens_per_result: usize,
}

impl Default for MicrocompactPolicy {
    fn default() -> Self {
        Self {
            max_tokens_per_result: 10_000,
        }
    }
}

/// 一次 microcompact 的统计。
#[derive(Debug, Clone, Default)]
pub struct MicrocompactReport {
    pub shadowed_count: usize,
    pub kept_count: usize,
    pub total_compactable: usize,
    /// 被压缩的工具结果原文备份：`(call_id, original_content)`。
    /// agent_loop 拿到后落盘到 `tool_results/<call_id>.txt`。
    pub shadowed_artifacts: Vec<(String, String)>,
}

fn is_compactable(name: &str) -> bool {
    COMPACTABLE_TOOLS.iter().any(|n| *n == name)
}

fn is_already_shadowed(content: &str) -> bool {
    content.starts_with(SHADOWED_PLACEHOLDER_PREFIX)
}

/// 对 entries 就地做一次 microcompact。
///
/// 扫描所有 `ToolResults`，单条可压缩结果 token 数超过 `max_tokens_per_result` 即 shadow。
/// 幂等——已是占位符的不重复替换。
pub fn microcompact(
    entries: &mut [TranscriptEntry],
    policy: &MicrocompactPolicy,
) -> MicrocompactReport {
    let mut shadowed = 0usize;
    let mut kept = 0usize;
    let mut total = 0usize;
    let mut artifacts: Vec<(String, String)> = Vec::new();

    for entry in entries.iter_mut() {
        let TranscriptEntry::ToolResults(results) = entry else {
            continue;
        };
        for r in results.iter_mut() {
            if !is_compactable(&r.name) {
                continue;
            }
            total += 1;
            if is_already_shadowed(&r.content) {
                // 已压缩，算作 shadowed（不重复落 artifacts）
                shadowed += 1;
                continue;
            }
            if estimate_tokens(&r.content) <= policy.max_tokens_per_result {
                kept += 1;
                continue;
            }
            // 超限：shadow
            artifacts.push((r.call_id.clone(), r.content.clone()));
            let placeholder = format!(
                "[结果已被压缩。原始内容可通过 Read 工具按路径检索：tool_results/{}.txt]",
                r.call_id
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
        shadowed_artifacts: artifacts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_gateway::types::ToolResult;

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
}
