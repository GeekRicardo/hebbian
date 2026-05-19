//! 各家主力模型的 context window 兜底查表。
//!
//! 数据来源：2026-05 各家官方文档 / 发布说明。只覆盖**当前主力模型**，
//! 老型号一律走该 provider 的保守默认值，不再单独建表。
//!
//! 调用点目前只有 `context_usage` / `compact_session`，用来算输入框旁
//! 环形进度条的分母。[`resolve_context_window`] 优先从 `/v1/models` 拉取
//! 真实 metadata，拉不到时回退本表。

use crate::config::{Provider, ProviderKind};

/// 从 /v1/models 获取 context_length，失败时回退到预设查表。
/// 适用于 session 创建 / 首次加载时一次性解析。
pub async fn resolve_context_window(provider: &Provider, model: &str) -> usize {
    // 先尝试从 API 获取
    if let Some(ctx) = crate::discovery::fetch_context_length(provider, model).await {
        return ctx;
    }
    // 回退到预设表
    context_window_for(provider.kind, model)
}

pub fn context_window_for(kind: ProviderKind, model: &str) -> usize {
    let m = model.to_lowercase();
    match kind {
        // Anthropic 4.6 / 4.7 / Sonnet 4.6 / Mythos 默认 1M（无需 beta header）
        // 其他（Sonnet 4.5、Haiku 4.5 等）= 200k
        ProviderKind::Anthropic => {
            if m.contains("opus-4-7")
                || m.contains("opus-4-6")
                || m.contains("sonnet-4-6")
                || m.contains("mythos")
            {
                1_000_000
            } else {
                200_000
            }
        }

        // OpenAI: GPT-5.4 / 5.5 = 1M，其他 GPT-5.x = 400k，更老的兜底 128k
        ProviderKind::Openai => {
            if m.contains("gpt-5.5")
                || m.contains("gpt-5-5")
                || m.contains("gpt-5.4")
                || m.contains("gpt-5-4")
            {
                1_000_000
            } else if m.starts_with("gpt-5") {
                400_000
            } else {
                128_000
            }
        }

        // DeepSeek V4 系列默认 1M；V3.x / R1 等按 openhanako known-models.json 对齐
        ProviderKind::Deepseek => {
            if m.contains("v4") {
                1_000_000
            } else if m.contains("v3.2") {
                163_840
            } else if m.contains("r1") {
                65_536
            } else if m.contains("coder") {
                128_000
            } else if m == "deepseek-chat" || m == "deepseek-reasoner" {
                1_000_000
            } else {
                1_000_000
            }
        }

        // Gemini 3 Flash = 200k，其他 Pro / Deep Think / 2.5 = 1M
        ProviderKind::Gemini => {
            if m.contains("flash") && m.contains("3") {
                200_000
            } else {
                1_000_000
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_1m_models() {
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "claude-opus-4-7-20260416"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "claude-sonnet-4-6"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "claude-haiku-4-5"),
            200_000
        );
    }

    #[test]
    fn openai_tiers() {
        assert_eq!(
            context_window_for(ProviderKind::Openai, "gpt-5.5"),
            1_000_000
        );
        assert_eq!(context_window_for(ProviderKind::Openai, "gpt-5.3"), 400_000);
        assert_eq!(context_window_for(ProviderKind::Openai, "gpt-4o"), 128_000);
    }

    #[test]
    fn deepseek_v4() {
        assert_eq!(
            context_window_for(ProviderKind::Deepseek, "deepseek-v4-pro"),
            1_000_000
        );
    }

    #[test]
    fn deepseek_legacy_models() {
        assert_eq!(
            context_window_for(ProviderKind::Deepseek, "deepseek-v3.2"),
            163_840
        );
        assert_eq!(
            context_window_for(ProviderKind::Deepseek, "deepseek-r1"),
            65_536
        );
        assert_eq!(
            context_window_for(ProviderKind::Deepseek, "deepseek-coder"),
            128_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Deepseek, "deepseek-chat"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Deepseek, "deepseek-reasoner"),
            1_000_000
        );
    }

    #[test]
    fn gemini_flash_vs_pro() {
        assert_eq!(
            context_window_for(ProviderKind::Gemini, "gemini-3-flash"),
            200_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Gemini, "gemini-3.1-pro"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Gemini, "gemini-2.5-pro"),
            1_000_000
        );
    }
}
