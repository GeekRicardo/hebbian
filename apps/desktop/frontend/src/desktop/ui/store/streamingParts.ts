import type { EngineEvent, StreamingAssistantPart } from "@/desktop/ui/types";

/**
 * 流式 assistant 输出的累积纯函数（主对话 useStore 与旁支 useBranchStore 同源复用）。
 *
 * 把后端 EngineEvent 事件流（text_delta / reasoning / tool_call_delta / tool_start /
 * tool_done / tool_output_delta）逐条折叠进一个 `StreamingAssistantPart[]`，
 * MessageBubble 直接吃这个结构渲染——文本段、推理折叠块、工具卡片（含实时输出）按真实
 * 发生顺序穿插。全部是无副作用纯函数，返回新数组，便于 store 直接替换 state。
 *
 * 不含主对话专属的 nested（Task 子事件路由）/ judge（AutoMode 黄色呼吸）/ reasoning
 * duration 回填——那些只主对话需要，留在 useStore 里。
 */

export function cloneStreamingParts(
  parts: StreamingAssistantPart[]
): StreamingAssistantPart[] {
  return parts.map((part) => ({ ...part }));
}

/** 末尾若是未结束的 reasoning 段，给它补上 duration（推理结束、后续段开始前调用）。 */
export function finalizeOpenReasoning(
  parts: StreamingAssistantPart[],
  now = Date.now()
): StreamingAssistantPart[] {
  const last = parts[parts.length - 1];
  if (last?.type !== "reasoning" || last.duration_ms != null) return parts;
  const next = cloneStreamingParts(parts);
  const reasoning = next[next.length - 1];
  if (reasoning.type === "reasoning") {
    reasoning.duration_ms = Math.max(0, now - (reasoning.started_at_ms ?? now));
  }
  return next;
}

/** 正文增量：贴到末尾 text 段；末尾非 text 则开新段（先收尾未结束的 reasoning）。 */
export function applyTextDelta(
  parts: StreamingAssistantPart[],
  text: string
): StreamingAssistantPart[] {
  if (!text) return parts;
  const base = finalizeOpenReasoning(parts);
  const next = cloneStreamingParts(base);
  const last = next[next.length - 1];
  if (last?.type === "text") {
    last.text += text;
  } else {
    next.push({ type: "text", text });
  }
  return next;
}

/**
 * 一轮 ModelStep 文本结束（TextDone）。`fullText` 是本轮的完整正文。
 *
 * 两种到达方式：
 * - 流式路径：本轮文本已由 text_delta 逐段累积，streamingText 尾部即 fullText
 *   → 不重复，原样返回。
 * - 非流式 end_turn 路径：Done 分支只 emit TextDone、没发过 TextDelta，本轮文本只在
 *   fullText 里 → 追加到累积末尾。
 *
 * 判定用 `streamingText.endsWith(fullText)`（累积串结尾是否已是本轮全文）。多轮 run 里
 * streamingText 是全 run 累积、远长于单轮 fullText，**不能**反过来用
 * `fullText.endsWith(streamingText)`——那会恒 false，把多轮累积覆盖成单轮（前面输出消失）。
 */
export function applyTextDone(
  streamingText: string,
  parts: StreamingAssistantPart[],
  fullText: string
): { streamingText: string; streamingParts: StreamingAssistantPart[] } {
  if (!fullText || streamingText.endsWith(fullText)) {
    return { streamingText, streamingParts: parts };
  }
  return {
    streamingText: streamingText + fullText,
    streamingParts: applyTextDelta(parts, fullText),
  };
}

/** 推理增量：贴到最近一段未结束 reasoning；末尾非 reasoning 则开新段。 */
export function applyReasoningDelta(
  parts: StreamingAssistantPart[],
  text: string
): StreamingAssistantPart[] {
  if (!text) return parts;
  const next = cloneStreamingParts(parts);
  const last = next[next.length - 1];
  if (last?.type === "reasoning" && last.duration_ms == null) {
    last.text += text;
  } else {
    next.push({ type: "reasoning", text, started_at_ms: Date.now() });
  }
  return next;
}

function toolPartIndex(
  parts: StreamingAssistantPart[],
  index: number | null | undefined,
  id?: string | null
) {
  // 有 id：优先按 id 匹配（唯一标识）。命中直接返回。
  if (id) {
    const byId = parts.findIndex(
      (part) => part.type === "tool_call" && part.id === id
    );
    if (byId >= 0) return byId;
  }
  // 按 index 回退匹配——仅当 index 是有效数字时。
  // 非流式 provider（如 anthropic 带工具）每轮 tool_start 的 index 都是 undefined，
  // 若用 `part.index === undefined` 匹配会命中上一轮工具 part，多轮工具全塌进同一卡片
  // 互相覆盖。index 缺失时无法按位置定位，当作新 part（返回 -1）。
  // 反之，流式 delta 场景下同一工具首个 chunk 可能 id 未到、只有有效 index，后续 chunk
  // 带 id——此时仍需按 index 认领之前建的 part（id 后到不分裂）。
  if (index == null) return -1;
  return parts.findIndex(
    (part) => part.type === "tool_call" && part.index === index
  );
}

/** 取/建对应 tool_call part（有 id 按 id、否则按 index 定位），返回新数组 + 该 part 下标。 */
export function ensureToolPart(
  parts: StreamingAssistantPart[],
  index: number | null | undefined,
  id?: string | null,
  name?: string | null
): [StreamingAssistantPart[], number] {
  const next = cloneStreamingParts(finalizeOpenReasoning(parts));
  const existing = toolPartIndex(next, index, id);
  if (existing >= 0) return [next, existing];

  next.push({
    type: "tool_call",
    index: index ?? next.length,
    id,
    name,
    arguments: "",
    status: "streaming",
  });
  return [next, next.length - 1];
}

export function applyToolCallDelta(
  parts: StreamingAssistantPart[],
  event: Extract<EngineEvent, { type: "tool_call_delta" }>
): StreamingAssistantPart[] {
  const [next, pos] = ensureToolPart(parts, event.index, event.id, event.name);
  const call = next[pos];
  if (call.type !== "tool_call") return next;
  next[pos] = {
    ...call,
    id: event.id ?? call.id,
    name: event.name ?? call.name,
    arguments: call.arguments + (event.arguments_delta ?? ""),
    status: call.status === "done" ? "done" : "streaming",
  };
  return next;
}

export function applyToolStart(
  parts: StreamingAssistantPart[],
  event: Extract<EngineEvent, { type: "tool_start" }>
): StreamingAssistantPart[] {
  const [next, pos] = ensureToolPart(parts, event.index, event.id, event.name);
  const call = next[pos];
  if (call.type !== "tool_call") return next;
  next[pos] = {
    ...call,
    id: event.id,
    name: event.name,
    input: event.input,
    status: "running",
  };
  return next;
}

export function applyToolDone(
  parts: StreamingAssistantPart[],
  event: Extract<EngineEvent, { type: "tool_done" }>
): StreamingAssistantPart[] {
  const [next, pos] = ensureToolPart(parts, event.index, event.id);
  const call = next[pos];
  if (call.type !== "tool_call") return next;
  next[pos] = {
    ...call,
    id: event.id,
    result: event.result,
    duration_ms: event.duration_ms,
    status: "done",
    is_error: event.is_error ?? false,
    artifact_path: event.artifact_path ?? null,
  };
  return next;
}

/**
 * 工具执行期间的流式输出片段——把 chunk 累加到对应 tool_call part 的 live_output。
 * 顺序保证：dispatcher 先 emit tool_start，本事件之后；finished 前都可能来多次。
 */
export function applyToolOutputDelta(
  parts: StreamingAssistantPart[],
  event: Extract<EngineEvent, { type: "tool_output_delta" }>
): StreamingAssistantPart[] {
  if (!event.chunk) return parts;
  const [next, pos] = ensureToolPart(parts, event.index, event.id);
  const call = next[pos];
  if (call.type !== "tool_call") return next;
  next[pos] = {
    ...call,
    live_output: (call.live_output ?? "") + event.chunk,
  };
  return next;
}
