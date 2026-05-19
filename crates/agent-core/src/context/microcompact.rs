//! Microcompact：工具结果压缩（学 Claude Code 的 microcompact 思路）。
//!
//! 长 tool_result（`Bash` / `Read` / `Grep` / `Glob` / `web_fetch` / `web_search` / `Edit`）
//! 一旦超过指定轮数仍留在 transcript 里，会浪费大量 token：
//! - 它们多半是中间步骤的环境读入（已经反映在 assistant 的后续动作里）
//! - 模型回看价值不大，回看也读不动整段 5k+ 输出
//!
//! microcompact = **保留最近 K 个可压缩工具结果，更早的那些把 content 替换成短占位符**。
//! 不动 user / assistant 文本、不动 tool_call 本身（保留 id / name / 参数），
//! 也不动非压缩白名单工具（`ask` / `Skill` / TodoWrite 等状态型工具）。
//!
//! 触发由 [`agent_loop`] 在每轮模型请求**之前**调用，对 transcript entries 就地修改。

use model_gateway::types::{ToolResult, TranscriptEntry};

/// 进入压缩白名单的工具名称。这些工具的结果"看过就没用"，token 大头。
const COMPACTABLE_TOOLS: &[&str] = &[
    "Bash",
    "Read",
    "Grep",
    "Glob",
    "Edit",
    "Fetch",
    "WebSearch",
];

/// 占位符内容。够短，模型也能从字面意思理解"这条结果已被压缩"。
pub const SHADOWED_PLACEHOLDER: &str = "[结果已被压缩]";

/// Microcompact 配置。
#[derive(Debug, Clone, Copy)]
pub struct MicrocompactPolicy {
    /// 累积可压缩工具结果数到达这个值后开始压缩。
    pub trigger_threshold: usize,
    /// 保留最近 K 个工具结果不动。
    pub keep_recent: usize,
}

impl Default for MicrocompactPolicy {
    fn default() -> Self {
        // 经验值：累积 12 个之后开始压；保留最近 5 个。
        Self {
            trigger_threshold: 12,
            keep_recent: 5,
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
    /// agent_loop 拿到后用 [`crate::storage::tool_results::save_tool_result`] 落盘
    /// `~/.hebbian/sessions/<sid>/tool_results/<call_id>.txt`（架构 §4.7 / Step 9）。
    pub shadowed_artifacts: Vec<(String, String)>,
}

fn is_compactable(name: &str) -> bool {
    COMPACTABLE_TOOLS.iter().any(|n| *n == name)
}

/// 对 entries 就地做一次 microcompact。
///
/// 算法：
/// 1. 扫描所有 `ToolResults`，把每条可压缩结果按出现顺序收集 `(entry_idx, result_idx)`
/// 2. 数量 < `trigger_threshold` ⇒ 不动，返回
/// 3. 数量 ≥ `trigger_threshold` ⇒ 把"除了最后 K 个之外"的所有可压缩结果 content 替换成占位符
/// 4. 已经是占位符的不重复替换（幂等）
pub fn microcompact(
    entries: &mut [TranscriptEntry],
    policy: &MicrocompactPolicy,
) -> MicrocompactReport {
    let mut positions: Vec<(usize, usize)> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if let TranscriptEntry::ToolResults(results) = entry {
            for (j, r) in results.iter().enumerate() {
                if is_compactable(&r.name) {
                    positions.push((i, j));
                }
            }
        }
    }

    let total = positions.len();
    if total < policy.trigger_threshold {
        return MicrocompactReport {
            shadowed_count: 0,
            kept_count: total,
            total_compactable: total,
            shadowed_artifacts: Vec::new(),
        };
    }

    let keep = policy.keep_recent.min(total);
    let cutoff = total - keep;
    let mut shadowed = 0;
    let mut artifacts: Vec<(String, String)> = Vec::new();
    for (idx_in_list, (entry_idx, result_idx)) in positions.iter().enumerate() {
        if idx_in_list >= cutoff {
            break;
        }
        if let Some(TranscriptEntry::ToolResults(results)) = entries.get_mut(*entry_idx) {
            if let Some(r) = results.get_mut(*result_idx) {
                if r.content != SHADOWED_PLACEHOLDER && !r.content.starts_with("[结果已被压缩")
                {
                    // 保留原文给 caller 落盘成 txt（架构 §4.7 / Step 9）。
                    artifacts.push((r.call_id.clone(), r.content.clone()));
                    let placeholder = format!(
                        "[结果已被压缩。原始内容可通过 Read 工具按 call_id 检索：tool_results/{}.txt]",
                        r.call_id
                    );
                    *r = ToolResult {
                        call_id: r.call_id.clone(),
                        name: r.name.clone(),
                        content: placeholder,
                        artifact: None,
                    };
                    shadowed += 1;
                }
            }
        }
    }

    MicrocompactReport {
        shadowed_count: shadowed,
        kept_count: keep,
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
            call_id: format!("c-{name}-{}", content.len()),
            name: name.to_string(),
            content: content.to_string(),
            artifact: None,
        }
    }

    #[test]
    fn under_threshold_does_nothing() {
        let mut entries = vec![TranscriptEntry::ToolResults(vec![
            tr("Bash", "ls"),
            tr("Read", "cat"),
        ])];
        let report = microcompact(&mut entries, &MicrocompactPolicy::default());
        assert_eq!(report.shadowed_count, 0);
        assert_eq!(report.total_compactable, 2);
    }

    #[test]
    fn shadows_old_keeps_recent() {
        let policy = MicrocompactPolicy {
            trigger_threshold: 4,
            keep_recent: 2,
        };
        let mut entries = vec![TranscriptEntry::ToolResults(vec![
            tr("Bash", "1"),
            tr("Bash", "2"),
            tr("Bash", "3"),
            tr("Bash", "4"),
            tr("Bash", "5"),
        ])];
        let report = microcompact(&mut entries, &policy);
        assert_eq!(report.shadowed_count, 3);
        assert_eq!(report.kept_count, 2);
        if let TranscriptEntry::ToolResults(results) = &entries[0] {
            for i in 0..3 {
                assert!(
                    results[i].content.starts_with("[结果已被压缩"),
                    "result {i} should be shadowed"
                );
            }
            assert_eq!(results[3].content, "4");
            assert_eq!(results[4].content, "5");
        } else {
            panic!("expected ToolResults");
        }
    }

    #[test]
    fn skips_non_compactable_tools() {
        let policy = MicrocompactPolicy {
            trigger_threshold: 1,
            keep_recent: 0,
        };
        let mut entries = vec![TranscriptEntry::ToolResults(vec![
            tr("ask", "需要确认"),
            tr("TodoWrite", "[]"),
        ])];
        let report = microcompact(&mut entries, &policy);
        assert_eq!(report.total_compactable, 0);
        assert_eq!(report.shadowed_count, 0);
    }

    #[test]
    fn idempotent() {
        let policy = MicrocompactPolicy {
            trigger_threshold: 2,
            keep_recent: 1,
        };
        let mut entries = vec![TranscriptEntry::ToolResults(vec![
            tr("Bash", "old"),
            tr("Bash", "newer"),
        ])];
        let r1 = microcompact(&mut entries, &policy);
        let r2 = microcompact(&mut entries, &policy);
        assert_eq!(r1.shadowed_count, 1);
        assert_eq!(r2.shadowed_count, 0); // 第二次不再重复替换
    }
}
