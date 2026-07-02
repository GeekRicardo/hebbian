import type { EngineEvent } from "@/desktop/ui/types";

type Flush = () => void;
type Schedule = (flush: Flush) => unknown;

export type EventBatcher = {
  push(event: EngineEvent): void;
  flushNow(): void;
  dispose(): void;
};

function defaultSchedule(flush: Flush): unknown {
  if (typeof requestAnimationFrame === "function") {
    return requestAnimationFrame(flush);
  }
  return setTimeout(flush, 16);
}

function toolKey(
  e: Extract<EngineEvent, { type: "tool_output_delta" | "tool_call_delta" }>,
): string {
  const ns = ("subagent_call_id" in e ? e.subagent_call_id : null) ?? "";
  const id = e.id ?? String(e.index);
  return `${ns}:${id}`;
}

/**
 * 文本 / 推理段：保留这两种事件交替出现的顺序。
 * 模型输出典型是 reasoning → text → reasoning → text … 交替。
 * 若各归各累加然后固定顺序 flush，交替会塌成全 text 先、全 reasoning 后 → 渲染错乱。
 */
type TextSegment =
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string };

function dispatchTextSegment(
  seg: TextSegment,
  dispatch: (event: EngineEvent) => void,
): void {
  if (seg.kind === "text") {
    dispatch({ type: "text_delta", text: seg.text } as EngineEvent);
  } else {
    dispatch({ type: "reasoning", text: seg.text } as EngineEvent);
  }
}

export function createEventBatcher(options: {
  dispatch: (event: EngineEvent) => void;
  schedule?: Schedule;
}): EventBatcher {
  const schedule = options.schedule ?? defaultSchedule;
  let disposed = false;
  let scheduled = false;

  // text / reasoning 按到达顺序排列的段列表
  let textSegments: TextSegment[] = [];

  // tool 事件：按 tool call key 独立累加（不同 tool call 各自去不同卡片，不需要互相保序）
  const outputChunks = new Map<string, string>();
  const argChunks = new Map<string, { event: EngineEvent; args: string }>();

  const ensureScheduled = () => {
    if (!scheduled) {
      scheduled = true;
      schedule(flushNow);
    }
  };

  const flushNow = () => {
    if (disposed) return;
    scheduled = false;

    const hadText = textSegments.length > 0;
    const hadOutput = outputChunks.size > 0;
    const hadArgs = argChunks.size > 0;

    if (!hadText && !hadOutput && !hadArgs) return;

    // 1) 文本 / 推理：按原始交替顺序 emit
    if (hadText) {
      for (const seg of textSegments) dispatchTextSegment(seg, options.dispatch);
      textSegments = [];
    }

    // 2) tool_call_delta
    if (hadArgs) {
      for (const [, entry] of argChunks) {
        options.dispatch({
          ...entry.event,
          arguments_delta: entry.args,
        } as EngineEvent);
      }
      argChunks.clear();
    }

    // 3) tool_output_delta
    if (hadOutput) {
      for (const [key, chunk] of outputChunks) {
        const parts = key.split(":");
        const subagentPart = parts[0] || null;
        const idPart = parts.slice(1).join(":");
        options.dispatch({
          type: "tool_output_delta",
          index: 0,
          id: idPart || undefined,
          chunk,
          ...(subagentPart ? { subagent_call_id: subagentPart } : {}),
        } as EngineEvent);
      }
      outputChunks.clear();
    }
  };

  return {
    push(event) {
      if (disposed) return;

      // ── text_delta：追加到尾部 text 段，类型不同则开新段 ──
      if (event.type === "text_delta") {
        if (!event.text) return;
        const last = textSegments[textSegments.length - 1];
        if (last?.kind === "text") {
          last.text += event.text;
        } else {
          textSegments.push({ kind: "text", text: event.text });
        }
        ensureScheduled();
        return;
      }

      // ── reasoning：追加到尾部 reasoning 段，类型不同则开新段 ──
      if (event.type === "reasoning") {
        if (!event.text) return;
        const last = textSegments[textSegments.length - 1];
        if (last?.kind === "reasoning") {
          last.text += event.text;
        } else {
          textSegments.push({ kind: "reasoning", text: event.text });
        }
        ensureScheduled();
        return;
      }

      // ── tool_call_delta：按 tool call key 独立累加 ──
      if (event.type === "tool_call_delta") {
        const delta = event.arguments_delta;
        if (!delta) return;
        const key = toolKey(event);
        const prev = argChunks.get(key);
        argChunks.set(key, {
          event: prev ? prev.event : event,
          args: (prev?.args ?? "") + delta,
        });
        ensureScheduled();
        return;
      }

      // ── tool_output_delta：按 tool call key 独立累加 ──
      if (event.type === "tool_output_delta") {
        if (!event.chunk) return;
        const key = toolKey(event);
        const prev = outputChunks.get(key) ?? "";
        outputChunks.set(key, prev + event.chunk);
        ensureScheduled();
        return;
      }

      // ── 边界事件：先 flush 全部累加，再立即 dispatch ──
      flushNow();
      options.dispatch(event);
    },
    flushNow,
    dispose() {
      flushNow();
      disposed = true;
      textSegments = [];
      outputChunks.clear();
      argChunks.clear();
      scheduled = false;
    },
  };
}
