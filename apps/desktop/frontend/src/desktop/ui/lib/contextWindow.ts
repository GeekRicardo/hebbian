/**
 * 各 provider 主力模型的 context window 兜底查表，与 Rust 侧
 * `crates/model-gateway/src/context_window.rs` 保持同步。
 *
 * 调度策略：**先按 model 名识别家族，再按 provider kind 兜底**。
 * 用户经常用 anthropic-kind / openai-kind 的第三方网关代理 deepseek-v4-pro
 * 这种跨家族模型；按 kind 分发会落到错的表。模型名命中关键字时优先用模型表。
 *
 * 这里只用来在 UI 提示里展示上下文窗口；和 Rust 那张表偶尔漂移不会影响功能
 * 正确性（`/compact` 进度条仍然以后端 resolve_context_window 为准）。
 */
export function contextWindowFor(providerKind: string, model: string): number {
  const m = normalizeModelId(model);
  const byModel = lookupByModelName(m);
  if (byModel !== null) return byModel;
  return fallbackByKind(providerKind);
}

/**
 * 与 Rust `common::reasoning::normalize_model_id` 一致：lowercase + dot→dash。
 * 同一款模型在不同上游网关里命名风格不一致（`opus-4-7` vs `opus-4.7`、
 * `gpt-5-5` vs `gpt-5.5`），归一化后只用 dash 形式比对。
 */
export function normalizeModelId(model: string): string {
  return model.toLowerCase().replace(/\./g, "-");
}

function lookupByModelName(m: string): number | null {
  // DeepSeek 家族（与 openhanako known-models.json deepseek 分区对齐）
  if (m.includes("deepseek")) {
    if (m.includes("v4")) return 1_000_000;
    // v3.2 在网关里写成 `deepseek-v3.2` 或缺 v 的 `deepseek-3.2`（kiro），归一化
    // 后是 `v3-2` / `-3-2`，两种都要命中，否则缺 v 的会掉到末尾兜底 1M。
    if (m.includes("v3-2") || m.includes("-3-2")) return 163_840;
    if (m.includes("r1")) return 65_536;
    if (m.includes("coder")) return 128_000;
    if (m.endsWith("deepseek-chat") || m.endsWith("deepseek-reasoner")) {
      return 1_000_000;
    }
    return 1_000_000;
  }
  // Claude 家族
  if (m.includes("claude") || m.includes("mythos")) {
    if (
      m.includes("opus-4-8") ||
      m.includes("opus-4-7") ||
      m.includes("opus-4-6") ||
      m.includes("sonnet-4-6") ||
      m.includes("mythos")
    ) {
      return 1_000_000;
    }
    return 200_000;
  }
  // 小米 MiMo v2+：1M 上下文。/v1/models 不返回 context_length，只能预设兜底。
  if (m.startsWith("mimo-v2")) return 1_000_000;
  // OpenAI GPT 家族
  if (
    m.startsWith("gpt-") ||
    m.startsWith("o1-") ||
    m.startsWith("o3-") ||
    m.startsWith("o4-")
  ) {
    if (m.startsWith("gpt-5-5") || m.startsWith("gpt-5-4")) {
      return 1_000_000;
    }
    if (m.startsWith("gpt-5")) return 400_000;
    return 128_000;
  }
  // Gemini 家族
  if (m.startsWith("gemini-")) {
    if (m.includes("flash") && m.includes("3")) return 200_000;
    return 1_000_000;
  }
  return null;
}

function fallbackByKind(providerKind: string): number {
  switch (providerKind) {
    case "anthropic":
      return 200_000;
    case "openai":
      return 128_000;
    case "deepseek":
      return 1_000_000;
    case "gemini":
      return 1_000_000;
    default:
      return 0;
  }
}

export function formatContextWindow(tokens: number): string {
  if (tokens <= 0) return "未知";
  if (tokens >= 1_000_000) {
    const m = tokens / 1_000_000;
    return Number.isInteger(m) ? `${m}M` : `${m.toFixed(1)}M`;
  }
  return `${Math.round(tokens / 1000)}k`;
}
