use model_gateway::types::{AssistantEntry, TranscriptEntry};

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

/// 计算整个 transcript 的估算 token 数
pub fn estimate_transcript_tokens(system: Option<&str>, entries: &[TranscriptEntry]) -> usize {
    let mut total = 0;

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
                    platform::attachments::MessageAttachment::TextFile { content, .. } => {
                        estimate_tokens(content) + 16
                    }
                    platform::attachments::MessageAttachment::Image { data, .. } => {
                        (data.len() / 1024) * 85 + 128
                    }
                })
                .sum();
            estimate_tokens(&user.text) + attachment_tokens + 4
        }
        TranscriptEntry::Assistant(AssistantEntry { text, tool_calls }) => {
            let mut n = estimate_tokens(text) + 4;
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
