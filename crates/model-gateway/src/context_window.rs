//! 各家主力模型的 context window 兜底查表。
//!
//! 数据来源：2026-05 各家官方文档 / 发布说明 + openhanako known-models.json
//! 的 anthropic / openai / deepseek / google 四个分区。
//!
//! 调度策略：**先按 model 名识别家族，再按 provider kind 兜底**。
//! 这条原则很关键——用户经常用一个 anthropic-kind 的第三方网关（如 Sub2api）
//! 代理 deepseek-v4-pro 或者 claude-opus；按 kind 分发会落到错的家族表。
//! 模型名包含 `deepseek`/`claude`/`gpt-`/`gemini` 等关键字时优先用模型表查询。
//!
//! 调用点目前只有 `context_usage` / `compact_session`，用来算输入框旁
//! 环形进度条的分母。[`resolve_context_window`] 优先从 `/v1/models` 拉取
//! 真实 metadata，拉不到时回退本表。

use crate::config::{Provider, ProviderKind};

/// 解析模型上下文窗口：用户手动设置 > /v1/models metadata > 预设查表。
/// 适用于 session 创建 / 首次加载时一次性解析。
pub async fn resolve_context_window(provider: &Provider, model: &str) -> usize {
    if let Some(ctx) = configured_context_window(provider, model) {
        return ctx;
    }
    if let Some(ctx) = crate::discovery::fetch_context_length(provider, model).await {
        return ctx;
    }
    context_window_for(provider.kind, model)
}

/// 解析本地配置里的模型上下文窗口。
pub fn configured_context_window(provider: &Provider, model: &str) -> Option<usize> {
    provider
        .model_context_windows
        .get(model)
        .copied()
        .filter(|n| *n > 0)
}

/// 同步路径使用的上下文窗口解析：用户手动设置 > 预设查表。
pub fn effective_context_window_for(provider: &Provider, model: &str) -> usize {
    configured_context_window(provider, model)
        .unwrap_or_else(|| context_window_for(provider.kind, model))
}

/// 按 model 名优先 + provider kind 兜底解析 context window（tokens）。
///
/// 优先策略保证「用 anthropic 网关代理 deepseek-v4-pro」「用 openai 兼容端点
/// 代理 claude-opus-4-7」这类常见跨 kind 网关都能给出对的窗口。
///
/// model id 在不同上游网关里的命名风格不一致（`opus-4-7` vs `opus-4.7`、
/// `gpt-5-5` vs `gpt-5.5`）——这里用 [`common::reasoning::normalize_model_id`]
/// 统一翻成 dash 形式后再匹配。
pub fn context_window_for(kind: ProviderKind, model: &str) -> usize {
    let m = common::reasoning::normalize_model_id(model);
    if let Some(n) = lookup_by_model_name(&m) {
        return n;
    }
    fallback_by_kind(kind, &m)
}

/// 通过模型名识别家族并返回 context window。
/// 命中返回 Some；模型名无足够特征时返回 None 让上层走 kind 兜底。
///
/// 入参 `m` 已经过 [`common::reasoning::normalize_model_id`] 归一化（小写 + dot→dash）。
fn lookup_by_model_name(m: &str) -> Option<usize> {
    // DeepSeek 家族（与 openhanako known-models.json 的 deepseek 分区对齐）
    if m.contains("deepseek") {
        if m.contains("v4") {
            return Some(1_000_000);
        }
        // v3.2 在不同网关里写成 `deepseek-v3.2` 或缺 v 的 `deepseek-3.2`（kiro），
        // 归一化后分别是 `v3-2` / `-3-2`，两种都要命中——否则缺 v 的会掉到末尾
        // 兜底 1M，把 164k 的模型当 1M 用、超长不压缩直接 400。
        if m.contains("v3-2") || m.contains("-3-2") {
            return Some(163_840);
        }
        if m.contains("r1") {
            return Some(65_536);
        }
        if m.contains("coder") {
            return Some(128_000);
        }
        if m.ends_with("deepseek-chat") || m.ends_with("deepseek-reasoner") {
            return Some(1_000_000);
        }
        return Some(1_000_000);
    }
    // Claude 家族
    if m.contains("claude") || m.contains("mythos") {
        if m.contains("opus-4-8")
            || m.contains("opus-4-7")
            || m.contains("opus-4-6")
            || m.contains("sonnet-4-6")
            || m.contains("mythos")
        {
            return Some(1_000_000);
        }
        return Some(200_000);
    }
    // 小米 MiMo v2+：1M 上下文。其 /v1/models 不返回 context_length 字段，
    // discovery 拉不到，只能在此预设兜底。
    if m.starts_with("mimo-v2") {
        return Some(1_000_000);
    }
    // OpenAI GPT 家族
    if m.starts_with("gpt-") || m.starts_with("o1-") || m.starts_with("o3-") || m.starts_with("o4-")
    {
        if m.starts_with("gpt-5-5") || m.starts_with("gpt-5-4") {
            return Some(1_000_000);
        }
        if m.starts_with("gpt-5") {
            return Some(400_000);
        }
        return Some(128_000);
    }
    // Gemini 家族
    if m.starts_with("gemini-") {
        if m.contains("flash") && m.contains("3") {
            return Some(200_000);
        }
        return Some(1_000_000);
    }
    None
}

/// 模型名无法识别时按 provider kind 给一个保守默认值。
fn fallback_by_kind(kind: ProviderKind, _m: &str) -> usize {
    match kind {
        ProviderKind::Anthropic => 200_000,
        ProviderKind::Openai => 128_000,
        ProviderKind::Deepseek => 1_000_000,
        ProviderKind::Gemini => 1_000_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_1m_models() {
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "claude-opus-4-8"),
            1_000_000
        );
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
        // kiro 把 v3.2 写成缺 v 的 deepseek-3.2，同样要识别成 164k（不能掉 1M 兜底）
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "deepseek-3.2"),
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

    /// 用 anthropic-kind 网关代理 deepseek-v4-pro：必须按 model 名取 1M，
    /// 而不是按 anthropic kind 默认的 200k。
    #[test]
    fn deepseek_via_anthropic_gateway() {
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "deepseek-v4-pro"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "deepseek-v3.2"),
            163_840
        );
    }

    /// 用 openai-kind（兼容 endpoint）代理 deepseek-v4-pro：同样要按 model 名取 1M。
    #[test]
    fn deepseek_via_openai_gateway() {
        assert_eq!(
            context_window_for(ProviderKind::Openai, "deepseek-v4-pro"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Openai, "deepseek-r1"),
            65_536
        );
    }

    /// 用 openai-kind 网关代理 claude-opus-4-7：按 model 名取 1M，而不是 GPT 兜底的 128k。
    #[test]
    fn claude_via_openai_gateway() {
        assert_eq!(
            context_window_for(ProviderKind::Openai, "claude-opus-4-7"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Openai, "claude-sonnet-4-5"),
            200_000
        );
    }

    /// Sub2API kind=anthropic 网关里挂着 `gpt-5.5` 等 GPT 模型；model-first 必须
    /// 把它们路由到 OpenAI 表（1M），而不是按 kind 落到 anthropic 兜底 200k。
    #[test]
    fn anthropic_gateway_serving_gpt_models_uses_openai_table() {
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "gpt-5.5"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "gpt-5.4"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "gpt-5"),
            400_000
        );
    }

    /// Sub2API / kiro 等网关常把版本号写成 dot：`claude-opus-4.7`、`gpt-5.5`、
    /// `deepseek-v3.2`。归一化后这些 id 都要匹配到对的 context window。
    #[test]
    fn dot_versioned_model_ids_resolved_after_normalize() {
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "claude-opus-4.7"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "claude-opus-4.6"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "claude-sonnet-4.6"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "claude-sonnet-4.5"),
            200_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Openai, "gpt-5.5"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Deepseek, "deepseek-v3.2"),
            163_840
        );
    }

    #[test]
    fn configured_context_window_wins() {
        let mut provider = Provider {
            id: "custom".into(),
            name: "Custom".into(),
            kind: ProviderKind::Openai,
            enabled: true,
            auth_mode: crate::config::AuthMode::ApiKey,
            base_url: "https://example.test/v1".into(),
            api_key: "test".into(),
            refresh_token: None,
            token_expires_at: None,
            account_id: None,
            extra_headers: std::collections::BTreeMap::new(),
            models: vec!["gpt-4o".into()],
            fetched_models: None,
            model_context_windows: std::collections::BTreeMap::new(),
            default_model: None,
            title_gen_enabled: false,
            title_gen_model: None,
            claude_code_compat: false,
        };
        provider
            .model_context_windows
            .insert("gpt-4o".into(), 256_000);

        assert_eq!(
            configured_context_window(&provider, "gpt-4o"),
            Some(256_000)
        );
        assert_eq!(effective_context_window_for(&provider, "gpt-4o"), 256_000);
    }

    /// MiMo v2+ 是 1M 上下文，但其 /v1/models 不返回 context_length，
    /// 只能靠模型名预设兜底（openai-kind 默认 128k 不对）。
    #[test]
    fn mimo_v2_is_1m() {
        assert_eq!(
            context_window_for(ProviderKind::Openai, "mimo-v2.5-pro"),
            1_000_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Openai, "mimo-v2.5"),
            1_000_000
        );
    }

    /// 模型名完全没特征时落到 kind 兜底。
    #[test]
    fn unknown_model_falls_back_to_kind() {
        assert_eq!(
            context_window_for(ProviderKind::Anthropic, "unknown-model"),
            200_000
        );
        assert_eq!(
            context_window_for(ProviderKind::Openai, "some-internal-model"),
            128_000
        );
    }
}
