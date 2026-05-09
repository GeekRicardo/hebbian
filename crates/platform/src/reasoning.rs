//! 模型推理 / thinking 配置。
//!
//! 由 `ModelRequest`（model-gateway）与 `Session`（storage）共用，
//! 因此放在 platform 这一层（两处的共同依赖）。
//!
//! 各家 schema 差异巨大，统一抽象在这里：
//!
//! - **Anthropic**：分三种「thinking 模式」按模型家族走不同 schema（详见
//!   [`AnthropicThinkingMode`]）。1M context 在 Opus/Sonnet 4.6+ 默认开启，
//!   老 Sonnet 4 / Opus 4.5 等需要靠 `anthropic-beta: context-1m-2025-08-07`。
//! - **OpenAI**：reasoning_effort 顶层字段，但枚举值随模型变化：
//!   gpt-5.4 / 5.5 / codex-max 多一档 `xhigh`，o-series 只有 low/medium/high，
//!   o1-mini 完全不支持。

use serde::{Deserialize, Serialize};

/// 推理强度。具体含义由各 provider 的翻译函数处理：
/// - Anthropic Opus 4.7（adaptive47）：写到 `output_config.effort`，能用到 `xhigh`。
/// - Anthropic 4.6 (adaptive)：写到 `thinking.effort`，**只支持 low/medium/high**，
///   `Extra` 钳成 `high`。
/// - Anthropic legacy enabled（3.7 / 4.x 旧型号）：用 [`Self::anthropic_legacy_budget_tokens`]。
/// - OpenAI：见 [`Self::openai_effort_for_model`]，按模型决定 high vs xhigh。
///
/// 默认 [`ReasoningEffort::Extra`]：希望默认值「尽量想清楚再回」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    /// 项目内部最高档。具体翻译看上面注释。
    #[default]
    Extra,
}

impl ReasoningEffort {
    /// 用于 Anthropic legacy `thinking: { type: "enabled", budget_tokens: N }`。
    /// 文档要求 `budget_tokens >= 1024`，且 `budget_tokens < max_tokens`。
    pub fn anthropic_legacy_budget_tokens(self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Medium => 4096,
            Self::High => 16_384,
            Self::Extra => 32_000,
        }
    }

    /// 用于 Anthropic 4.6 adaptive `thinking.effort` —— 仅支持 low/medium/high。
    pub fn anthropic_adaptive46_effort(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High | Self::Extra => "high",
        }
    }

    /// 用于 Anthropic 4.7 `output_config.effort` —— 4 档全有，含 `xhigh`。
    pub fn anthropic_adaptive47_effort(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Extra => "xhigh",
        }
    }

    /// 用于 DeepSeek `reasoning_effort`（OpenAI compat 路径，api.deepseek.com）。
    /// DeepSeek 的 effort 命名空间只有 `high` / `max` 两档，所以我们把 Low/Medium/High
    /// 全钳到 `high`，Extra 升到 `max`（与 openhanako provider-compat/deepseek.js 对齐）。
    pub fn deepseek_effort(self) -> &'static str {
        match self {
            Self::Low | Self::Medium | Self::High => "high",
            Self::Extra => "max",
        }
    }

    /// 用于 OpenAI `reasoning_effort`。按模型决定 Extra 是否能用 `xhigh`：
    /// gpt-5.4 / 5.5 / codex-max 支持 xhigh，其它（o-series / gpt-5 / 5.1）
    /// Extra 钳到 `high`。
    pub fn openai_effort_for_model(self, model: &str) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Extra => {
                if openai_supports_xhigh(model) {
                    "xhigh"
                } else {
                    "high"
                }
            }
        }
    }
}

/// 推理 / thinking 行为。`enabled = None` 表示「沿用模型默认」（多数模型默认关闭）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningConfig {
    /// 是否启用 thinking / reasoning。`None` = 用模型默认；
    /// 对支持 thinking 的模型，UI 默认填 `Some(true)`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 推理强度。`None` = 用 [`ReasoningEffort::default()`]（Extra）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Anthropic 1M 上下文开关。仅对 Sonnet/Opus 系列旧模型有意义；
    /// 4.6+ 默认就是 1M，开关被服务端忽略。`None` = 不传 header。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context: Option<bool>,
}

impl ReasoningConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    pub fn effective_effort(&self) -> ReasoningEffort {
        self.effort.unwrap_or_default()
    }

    pub fn wants_long_context(&self) -> bool {
        self.long_context.unwrap_or(false)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Anthropic 模型家族判定
// ──────────────────────────────────────────────────────────────────────────────

/// Anthropic 家族的 thinking schema 模式。
///
/// schema 三套互不兼容，必须按模型选择：
///
/// | 模式 | 适用模型 | 请求体 |
/// |------|---------|--------|
/// | `Opus47Adaptive` | `claude-opus-4-7*` | `thinking:{type:"adaptive",display:"summarized"}` + `output_config:{effort: low\|medium\|high\|xhigh}`，并且**禁止**带 `temperature/top_p/top_k`。 |
/// | `Adaptive46` | `claude-opus-4-6*` / `claude-sonnet-4-6*` | `thinking:{type:"adaptive",effort: low\|medium\|high}`（无 xhigh）。 |
/// | `LegacyEnabled` | `claude-3-7-*` / `claude-opus-4*`（不含 4.6/4.7）/ `claude-sonnet-4*`（不含 4.6）/ `claude-haiku-4*` | `thinking:{type:"enabled",budget_tokens:N}`，`budget_tokens >= 1024 && budget_tokens < max_tokens`。 |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicThinkingMode {
    Opus47Adaptive,
    Adaptive46,
    LegacyEnabled,
}

/// 判定 Anthropic 模型走哪种 thinking schema。
/// `None` = 不支持 thinking（如 claude-3-5、claude-3-haiku）。
pub fn anthropic_thinking_mode(model: &str) -> Option<AnthropicThinkingMode> {
    let m = model.to_ascii_lowercase();
    if m.contains("opus-4-7") {
        return Some(AnthropicThinkingMode::Opus47Adaptive);
    }
    if m.contains("opus-4-6") || m.contains("sonnet-4-6") {
        return Some(AnthropicThinkingMode::Adaptive46);
    }
    // 兜底：claude-3-7 / claude-opus-4{,-1,-5} / claude-sonnet-4{,-5} / claude-haiku-4-5
    if m.contains("claude-3-7")
        || m.contains("claude-opus-4")
        || m.contains("claude-sonnet-4")
        || m.contains("claude-haiku-4")
    {
        return Some(AnthropicThinkingMode::LegacyEnabled);
    }
    None
}

/// 兼容老 API：是否支持 thinking。
pub fn anthropic_supports_thinking(model: &str) -> bool {
    anthropic_thinking_mode(model).is_some()
}

/// 该 Anthropic 模型是否需要靠 `anthropic-beta: context-1m-2025-08-07` 才能开 1M。
///
/// - 4.6 / 4.7 默认 1M，无需 header（即使带也无效）。
/// - Sonnet 4 / Sonnet 4.5 / Opus 4 / 4.1 / 4.5 老 Sonnet 系列：需要 header。
/// - Haiku 4.5 / 3.7 等：无 1M 支持，UI 不暴露这个开关。
pub fn anthropic_long_context_uses_beta(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if m.contains("opus-4-7") || m.contains("opus-4-6") || m.contains("sonnet-4-6") {
        return false;
    }
    // 仅 Sonnet 4 系列 + Opus 4.x 老型号支持 1M（通过 beta header）
    m.contains("sonnet-4") || m.contains("opus-4")
}

/// 该 Anthropic 模型是否暴露 1M context 开关给 UI。
/// 4.6+ 默认 1M、不该让用户瞎切；老型号 + 不支持的型号都返回 false / true 对应。
pub fn anthropic_exposes_long_context_toggle(model: &str) -> bool {
    anthropic_long_context_uses_beta(model)
}

/// 1M context 的 anthropic-beta header 值。
pub const ANTHROPIC_LONG_CONTEXT_BETA: &str = "context-1m-2025-08-07";

// ──────────────────────────────────────────────────────────────────────────────
// OpenAI 模型家族判定
// ──────────────────────────────────────────────────────────────────────────────

/// 该 OpenAI 模型是否完全不支持 reasoning_effort（典型：`o1-mini`）。
pub fn openai_skips_reasoning(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    // o1-mini 历史上不支持 reasoning_effort，不传字段免得 400
    m.starts_with("o1-mini")
}

/// 该 OpenAI 模型是否支持 `reasoning_effort=xhigh`。
/// 当前已知支持 xhigh 的：gpt-5.4 系列、gpt-5.5 系列、gpt-5.1-codex-max。
pub fn openai_supports_xhigh(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("gpt-5.4")
        || m.starts_with("gpt-5.5")
        || m.contains("gpt-5.1-codex-max")
}

/// 该 OpenAI 模型是否支持 reasoning_effort 控制（即配 reasoning UI 是否可见）。
///
/// 覆盖：gpt-5* / o1（不含 mini）/ o3 / o4。
pub fn openai_supports_reasoning(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if openai_skips_reasoning(&m) {
        return false;
    }
    if m.starts_with("gpt-5") {
        return true;
    }
    m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.contains("-reasoning")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_mode_detection() {
        use AnthropicThinkingMode::*;
        assert_eq!(
            anthropic_thinking_mode("claude-opus-4-7-20260101"),
            Some(Opus47Adaptive)
        );
        assert_eq!(
            anthropic_thinking_mode("claude-opus-4-6"),
            Some(Adaptive46)
        );
        assert_eq!(
            anthropic_thinking_mode("claude-sonnet-4-6"),
            Some(Adaptive46)
        );
        assert_eq!(
            anthropic_thinking_mode("claude-opus-4-5-20251015"),
            Some(LegacyEnabled)
        );
        assert_eq!(
            anthropic_thinking_mode("claude-sonnet-4-5"),
            Some(LegacyEnabled)
        );
        assert_eq!(
            anthropic_thinking_mode("claude-haiku-4-5"),
            Some(LegacyEnabled)
        );
        assert_eq!(
            anthropic_thinking_mode("claude-3-7-sonnet-latest"),
            Some(LegacyEnabled)
        );
        assert_eq!(anthropic_thinking_mode("claude-3-5-sonnet"), None);
        assert_eq!(anthropic_thinking_mode("claude-3-haiku"), None);
    }

    #[test]
    fn long_context_toggle_only_for_legacy_4_family() {
        assert!(!anthropic_exposes_long_context_toggle("claude-opus-4-7"));
        assert!(!anthropic_exposes_long_context_toggle("claude-opus-4-6"));
        assert!(!anthropic_exposes_long_context_toggle("claude-sonnet-4-6"));
        assert!(anthropic_exposes_long_context_toggle("claude-sonnet-4-5"));
        assert!(anthropic_exposes_long_context_toggle("claude-sonnet-4"));
        assert!(anthropic_exposes_long_context_toggle("claude-opus-4-1"));
        assert!(!anthropic_exposes_long_context_toggle("claude-haiku-4-5"));
        assert!(!anthropic_exposes_long_context_toggle("claude-3-7-sonnet"));
    }

    #[test]
    fn openai_xhigh_detection() {
        assert!(openai_supports_xhigh("gpt-5.4"));
        assert!(openai_supports_xhigh("gpt-5.4-mini"));
        assert!(openai_supports_xhigh("gpt-5.5"));
        assert!(openai_supports_xhigh("gpt-5.1-codex-max"));
        assert!(!openai_supports_xhigh("gpt-5"));
        assert!(!openai_supports_xhigh("gpt-5.1"));
        assert!(!openai_supports_xhigh("o3"));
    }

    #[test]
    fn openai_o1_mini_skipped() {
        assert!(openai_skips_reasoning("o1-mini"));
        assert!(!openai_supports_reasoning("o1-mini"));
        assert!(openai_supports_reasoning("o1"));
        assert!(openai_supports_reasoning("o3"));
        assert!(openai_supports_reasoning("gpt-5.5"));
    }

    #[test]
    fn extra_maps_to_xhigh_only_when_supported() {
        assert_eq!(ReasoningEffort::Extra.openai_effort_for_model("gpt-5.4"), "xhigh");
        assert_eq!(ReasoningEffort::Extra.openai_effort_for_model("gpt-5.5"), "xhigh");
        assert_eq!(ReasoningEffort::Extra.openai_effort_for_model("gpt-5"), "high");
        assert_eq!(ReasoningEffort::Extra.openai_effort_for_model("o3"), "high");
        assert_eq!(ReasoningEffort::Medium.openai_effort_for_model("gpt-5.4"), "medium");
    }

    #[test]
    fn budget_monotonic() {
        let levels = [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Extra,
        ];
        let budgets: Vec<u32> = levels
            .iter()
            .map(|e| e.anthropic_legacy_budget_tokens())
            .collect();
        for w in budgets.windows(2) {
            assert!(w[0] < w[1]);
        }
    }

    #[test]
    fn reasoning_config_serde_roundtrip() {
        let cfg = ReasoningConfig {
            enabled: Some(true),
            effort: Some(ReasoningEffort::Extra),
            long_context: Some(true),
        };
        let s = serde_json::to_string(&cfg).unwrap();
        let back: ReasoningConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }
}
