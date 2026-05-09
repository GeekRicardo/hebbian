// 与 platform/src/reasoning.rs 的检测逻辑保持一致。
// 任意一边新增模型家族时，记得两侧同步。

import type { ReasoningConfig, ReasoningEffort } from "@/desktop/ui/types";

// ── Anthropic：thinking 模式分三档（与 Rust AnthropicThinkingMode 对齐） ──

export type AnthropicThinkingMode =
  | "opus_47_adaptive" // claude-opus-4-7：thinking adaptive + output_config.effort（含 xhigh）
  | "adaptive_46" // claude-opus-4-6 / claude-sonnet-4-6：thinking adaptive + effort（无 xhigh）
  | "legacy_enabled"; // 3-7 / opus-4{,-1,-5} / sonnet-4{,-5} / haiku-4-5：thinking enabled + budget_tokens

export function anthropicThinkingMode(
  model: string
): AnthropicThinkingMode | null {
  const m = model.toLowerCase();
  if (m.includes("opus-4-7")) return "opus_47_adaptive";
  if (m.includes("opus-4-6") || m.includes("sonnet-4-6")) return "adaptive_46";
  if (
    m.includes("claude-3-7") ||
    m.includes("claude-opus-4") ||
    m.includes("claude-sonnet-4") ||
    m.includes("claude-haiku-4")
  ) {
    return "legacy_enabled";
  }
  return null;
}

export function anthropicSupportsThinking(model: string): boolean {
  return anthropicThinkingMode(model) !== null;
}

/**
 * 该 Anthropic 模型是否暴露 1M 上下文开关。
 *
 * 4.6 / 4.7 默认 1M（不暴露开关）；老 Sonnet 4 / Sonnet 4.5 / Opus 4.x 通过
 * `anthropic-beta: context-1m-2025-08-07` 才能开启 1M——这部分模型 UI 上要给开关。
 */
export function anthropicExposesLongContextToggle(model: string): boolean {
  const m = model.toLowerCase();
  if (
    m.includes("opus-4-7") ||
    m.includes("opus-4-6") ||
    m.includes("sonnet-4-6")
  ) {
    return false;
  }
  return m.includes("sonnet-4") || m.includes("opus-4");
}

// ── OpenAI ──

/** o1-mini 等完全不支持 reasoning_effort 的模型。 */
export function openaiSkipsReasoning(model: string): boolean {
  return model.toLowerCase().startsWith("o1-mini");
}

/** 支持 `reasoning_effort=xhigh` 的模型：gpt-5.4 / 5.5 / 5.1-codex-max。 */
export function openaiSupportsXhigh(model: string): boolean {
  const m = model.toLowerCase();
  return (
    m.startsWith("gpt-5.4") ||
    m.startsWith("gpt-5.5") ||
    m.includes("gpt-5.1-codex-max")
  );
}

export function openaiSupportsReasoning(model: string): boolean {
  if (openaiSkipsReasoning(model)) return false;
  const m = model.toLowerCase();
  if (m.startsWith("gpt-5")) return true;
  return (
    m.startsWith("o1") ||
    m.startsWith("o3") ||
    m.startsWith("o4") ||
    m.includes("-reasoning")
  );
}

// ── 跨家族 ──

export function modelSupportsReasoning(
  providerKind: string,
  model: string
): boolean {
  if (providerKind === "anthropic") return anthropicSupportsThinking(model);
  if (providerKind === "openai") return openaiSupportsReasoning(model);
  return false;
}

export function modelExposesLongContextToggle(
  providerKind: string,
  model: string
): boolean {
  if (providerKind === "anthropic") return anthropicExposesLongContextToggle(model);
  return false;
}

export const DEFAULT_REASONING: ReasoningConfig = {
  enabled: true,
  effort: "extra",
};

export const REASONING_EFFORT_ORDER: ReasoningEffort[] = [
  "low",
  "medium",
  "high",
  "extra",
];

export const REASONING_EFFORT_LABEL: Record<ReasoningEffort, string> = {
  low: "低",
  medium: "中",
  high: "高",
  extra: "极高",
};

/**
 * 把项目档位翻译成在该模型上「实际」会发出的字符串。
 * 仅用于 UI tooltip / banner 文案，不参与请求构造。
 */
export function effortDisplay(
  providerKind: string,
  model: string,
  effort: ReasoningEffort
): string {
  if (providerKind === "anthropic") {
    const mode = anthropicThinkingMode(model);
    if (mode === "opus_47_adaptive") {
      return effort === "extra" ? "xhigh" : effort;
    }
    if (mode === "adaptive_46") {
      // 4.6 没有 xhigh，Extra 钳到 high
      return effort === "extra" ? "high" : effort;
    }
    if (mode === "legacy_enabled") {
      const budgets = { low: 1024, medium: 4096, high: 16384, extra: 32000 };
      return `${budgets[effort]} tok`;
    }
    return effort;
  }
  if (providerKind === "openai") {
    if (effort === "extra") {
      return openaiSupportsXhigh(model) ? "xhigh" : "high";
    }
    return effort;
  }
  return effort;
}
