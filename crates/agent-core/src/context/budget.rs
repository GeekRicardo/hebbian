use model_gateway::types::{AssistantEntry, TranscriptEntry};

/// system prompt（BASE + persona + rules）+ 全部 tool 定义 schema 的恒定开销近似。
/// 这块每轮请求都在、与对话长度无关，服务端真值约一万 token；本地估算若漏掉它，
/// 在对话很短时估值趋近 0，会让 `calibrated` 的「真值/估值」比值爆炸成巨大乘数。
const BASE_OVERHEAD_TOKENS: usize = 10_000;

/// 单张图片的 token 估算。**与 base64 字节数无关**：目标模型原生支持图片时按分辨率
/// 编码（量级数百~两千 token），不支持时经 VisionBridge 转成 ≤900 token 的文字描述
/// 再入上下文。取一个覆盖两条路径的保守上界，避免按 base64 长度估出十几万的虚高值。
const IMAGE_TOKENS_ESTIMATE: usize = 2_000;

/// 校准比值（真值/估值）的上界。比值本意是吸收 tokenizer 偏差与恒定开销，正常在 1~2；
/// 一旦因估值异常偏小被放大成巨大乘数，会把后续估算整体抬爆。钳到此上界兜底。
const MAX_CALIBRATION_RATIO: f64 = 3.0;

/// 粗略 token 估算（约 4 字符/token）
pub fn estimate_tokens(text: &str) -> usize {
    // 中文约 1.5 字符/token，英文约 4 字符/token，取平均
    let chars = text.chars().count();
    let bytes = text.len();
    // 如果字节数远超字符数，说明含大量 CJK，调整估算
    if bytes > chars * 2 {
        chars // CJK: ~1 token per char
    } else {
        bytes / 4
    }
}

/// 用最近一次请求的服务端真值校准本地估算。
///
/// 本地 [`estimate_transcript_tokens`] 是 ~4 字符/token 的启发式，对代码 / JSON
/// 工具结果系统性低估约 30%，且没把 base prompt + tool 定义这类恒定开销算进去。
/// 而每次模型请求返回的 `usage.input_tokens` 是服务端 tokenizer 的精确值。
///
/// 关键：**采样时与应用时的估算口径必须一致**（两处都用 `estimate_transcript_tokens`
/// 同样的 `system` / `entries` 口径）。这样 `真值 / 估值` 这个比值就同时吸收了
/// tokenizer 偏差和恒定开销，乘回当前估算即得逼近真值的校准结果。
///
/// 压缩后当前估算立刻下降、比值保持稳定 → 校准值实时跟随，不会像直接用
/// `last_real` 那样滞后一拍。`last_real` / `last_estimated` 任一为 0（新会话还没
/// 采到样本）时退化为裸估算，与历史行为一致。
pub fn calibrated_transcript_tokens(
    system: Option<&str>,
    entries: &[TranscriptEntry],
    last_real: u64,
    last_estimated: u64,
) -> usize {
    let raw = estimate_transcript_tokens(system, entries);
    if last_real == 0 || last_estimated == 0 {
        return raw;
    }
    let ratio = (last_real as f64 / last_estimated as f64).min(MAX_CALIBRATION_RATIO);
    ((raw as f64) * ratio).round() as usize
}

/// 计算整个 transcript 的估算 token 数。含 system + tool 定义 schema 的恒定开销近似
/// （[`BASE_OVERHEAD_TOKENS`]），让短对话的估值不至于趋近 0、把校准比值带爆。
pub fn estimate_transcript_tokens(system: Option<&str>, entries: &[TranscriptEntry]) -> usize {
    let mut total = BASE_OVERHEAD_TOKENS;

    if let Some(s) = system {
        total += estimate_tokens(s) + 4; // role overhead
    }

    for entry in entries {
        total += entry_tokens(entry);
    }

    total
}

fn entry_tokens(entry: &TranscriptEntry) -> usize {
    match entry {
        TranscriptEntry::User(user) => {
            let attachment_tokens: usize = user
                .attachments
                .iter()
                .map(|a| match a {
                    common::attachments::MessageAttachment::TextFile { content, .. } => {
                        estimate_tokens(content) + 16
                    }
                    common::attachments::MessageAttachment::Image { .. } => {
                        IMAGE_TOKENS_ESTIMATE
                    }
                })
                .sum();
            estimate_tokens(&user.text) + attachment_tokens + 4
        }
        TranscriptEntry::Assistant(AssistantEntry {
            text,
            reasoning,
            tool_calls,
            ..
        }) => {
            let mut n = estimate_tokens(text) + estimate_tokens(reasoning) + 4;
            for c in tool_calls {
                n += estimate_tokens(&c.name) + estimate_tokens(&c.input.to_string()) + 8;
            }
            n
        }
        TranscriptEntry::ToolResults(results) => results
            .iter()
            .map(|r| estimate_tokens(&r.content) + 8)
            .sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_gateway::types::UserEntry;

    fn sample_entries() -> Vec<TranscriptEntry> {
        vec![TranscriptEntry::User(UserEntry::text(&"x".repeat(4000)))]
    }

    #[test]
    fn calibration_falls_back_to_raw_without_sample() {
        let entries = sample_entries();
        let raw = estimate_transcript_tokens(None, &entries);
        // 任一样本为 0（新会话还没采样）→ 退化为裸估算。
        assert_eq!(calibrated_transcript_tokens(None, &entries, 0, 0), raw);
        assert_eq!(calibrated_transcript_tokens(None, &entries, 1000, 0), raw);
        assert_eq!(calibrated_transcript_tokens(None, &entries, 0, 1000), raw);
    }

    #[test]
    fn calibration_scales_by_real_over_estimated_ratio() {
        let entries = sample_entries();
        let raw = estimate_transcript_tokens(None, &entries) as f64;
        // 真值是上次估值的 1.5 倍 → 当前估算同比放大。
        let got = calibrated_transcript_tokens(None, &entries, 1500, 1000);
        assert_eq!(got, (raw * 1.5).round() as usize);
    }

    #[test]
    fn calibration_follows_transcript_shrink_after_compaction() {
        // 压缩后 transcript 变短：当前估算下降，比值保持稳定 → 校准值随之下降，
        // 不像直接用 last_real 那样滞后一拍仍显示满格。
        let before = sample_entries();
        let after = vec![TranscriptEntry::User(UserEntry::text("compacted summary"))];
        let cal_before = calibrated_transcript_tokens(None, &before, 1500, 1000);
        let cal_after = calibrated_transcript_tokens(None, &after, 1500, 1000);
        assert!(cal_after < cal_before);
    }
}
