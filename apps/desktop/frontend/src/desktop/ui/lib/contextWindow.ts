/**
 * 各 provider 主力模型的 context window 兜底查表，与 Rust 侧
 * `crates/model-gateway/src/context_window.rs` 保持同步。
 *
 * 这里只用来在 UI 提示里展示模型上下文窗口；和 Rust 那张表偶尔
 * 漂移不会影响功能正确性（`/compact` 进度条仍然以后端值为准）。
 */
export function contextWindowFor(providerKind: string, model: string): number {
  const m = model.toLowerCase();
  switch (providerKind) {
    case "anthropic":
      if (
        m.includes("opus-4-7") ||
        m.includes("opus-4-6") ||
        m.includes("sonnet-4-6") ||
        m.includes("mythos")
      ) {
        return 1_000_000;
      }
      return 200_000;

    case "openai":
      if (
        m.includes("gpt-5.5") ||
        m.includes("gpt-5-5") ||
        m.includes("gpt-5.4") ||
        m.includes("gpt-5-4")
      ) {
        return 1_000_000;
      }
      if (m.startsWith("gpt-5")) {
        return 400_000;
      }
      return 128_000;

    case "deepseek":
      return 1_000_000;

    case "gemini":
      if (m.includes("flash") && m.includes("3")) {
        return 200_000;
      }
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
