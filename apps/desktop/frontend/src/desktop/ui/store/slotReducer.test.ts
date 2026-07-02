import { applyEventToSlot } from "./slotReducer.ts";
import { createEventBatcher } from "./eventBatcher.ts";
import type { SessionStream } from "./useStore.ts";
import type { EngineEvent, StreamingAssistantPart } from "../types.ts";

function check(name: string, got: unknown, exp: unknown) {
  const a = JSON.stringify(got), e = JSON.stringify(exp);
  if (a !== e) throw new Error(`FAIL ${name}: expected ${e}, got ${a}`);
  console.log(`ok ${name}`);
}

function makeSlot(over: Partial<SessionStream> = {}): SessionStream {
  return {
    requestId: "req1",
    streamingMessageId: "streaming",
    streamingText: "",
    streamingParts: [],
    liveTimeline: [],
    assistantInsertPos: 0,
    pendingApproval: null,
    pendingApprovalQueue: [],
    pendingQuestion: null,
    pendingQuestionQueue: [],
    autoJudgedNotes: [],
    currentRunMode: null,
    judgingRequests: {},
    suspended: null,
    todos: [],
    activePlan: null,
    planComments: {},
    modelRetry: null,
    ...over,
  } as SessionStream;
}

const ev = (e: unknown) => e as EngineEvent;
const toolCalls = (p: StreamingAssistantPart[]) => p.filter((x) => x.type === "tool_call");

// ── 复现：多 Turn 无插队共用同一个 streaming bubble，turn2 的 ModelStep 请求失败重试，
// 新 attempt 的首个 delta 不应清空 turn1 已经输出（且工具已执行）的内容。
// 修前：text_delta 的 hadRetry 分支用 e.text / applyTextDelta([], …) 直接清空整个 slot
// 累积 → turn1 的文本和工具卡片全消失，run 完 reload 才恢复（用户看到的「消息消失→
// 渲染后面→完成后重渲染」）。
{
  let slot = makeSlot();
  slot = applyEventToSlot(slot, ev({ type: "step_started", step_kind: "model", step_index: 1 }));
  slot = applyEventToSlot(slot, ev({ type: "text_delta", text: "Turn1分析" }));
  slot = applyEventToSlot(slot, ev({ type: "tool_start", index: 0, id: "t1", name: "Bash", input: {} }));
  slot = applyEventToSlot(slot, ev({ type: "tool_done", index: 0, id: "t1", result: "ok", duration_ms: 1 }));
  slot = applyEventToSlot(slot, ev({ type: "turn_finished", stop_reason: "end_turn" }));

  slot = applyEventToSlot(slot, ev({ type: "step_started", step_kind: "model", step_index: 2 }));
  slot = applyEventToSlot(slot, ev({ type: "model_retry", attempt: 1, max: 5, delay_ms: 100, reason: "503" }));
  slot = applyEventToSlot(slot, ev({ type: "text_delta", text: "Turn2结果" }));

  check("retry 不丢前 turn 文本（续写而非覆盖）", slot.streamingText, "Turn1分析Turn2结果");
  check("retry 后前 turn 工具卡仍在", toolCalls(slot.streamingParts).length, 1);
}

// ── 对照：单 Turn 内流到一半失败重试，应丢弃本次失败 attempt 的残片、从本 step 起点
// 重来——retry 的本意（清残片、避免叠加）必须保住。
{
  let slot = makeSlot();
  slot = applyEventToSlot(slot, ev({ type: "step_started", step_kind: "model", step_index: 1 }));
  slot = applyEventToSlot(slot, ev({ type: "text_delta", text: "半句" }));
  slot = applyEventToSlot(slot, ev({ type: "model_retry", attempt: 1, max: 5, delay_ms: 100, reason: "503" }));
  slot = applyEventToSlot(slot, ev({ type: "text_delta", text: "完整答案" }));

  check("单 turn retry 丢弃失败残片、不叠加", slot.streamingText, "完整答案");
}

// ── 高频 text_delta 必须被批处理成每帧一次 store 写入。否则 Desktop 端模型流式输出
// 会按 token 触发 Zustand set + ChatView 重渲染，长回答时主线程明显卡顿。
{
  const handled: EngineEvent[] = [];
  let scheduled = 0;
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => {
      scheduled += 1;
      return flush;
    },
  });

  batcher.push(ev({ type: "text_delta", text: "你" }));
  batcher.push(ev({ type: "text_delta", text: "好" }));

  check("连续 text_delta 入队前不立刻 dispatch", handled.length, 0);
  check("连续 text_delta 只调度一次 flush", scheduled, 1);
  batcher.flushNow();
  check("连续 text_delta 合并成一条", handled, [ev({ type: "text_delta", text: "你好" })]);
}

// ── 非文本事件是边界：先 flush 已累积文本，再按原顺序派发边界事件。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "text_delta", text: "A" }));
  batcher.push(ev({ type: "tool_start", id: "t1", index: 0, name: "Read", input: {} }));

  check("边界事件前刷新文本", handled, [
    ev({ type: "text_delta", text: "A" }),
    ev({ type: "tool_start", id: "t1", index: 0, name: "Read", input: {} }),
  ]);
}

// ── tool_output_delta 批处理：PTY 输出高频 chunk 必须合并到 rAF 帧统一 dispatch，
// 避免每次 chunk 都触发一次 setState（tool_start / tool_done 正常即时派发）。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "line1\n" }));
  batcher.push(ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "line2\n" }));

  check("tool_output_delta 入队前不立刻 dispatch", handled.length, 0);
  batcher.flushNow();
  check("同 tool call 连续 chunk 合并", handled, [
    ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "line1\nline2\n" }),
  ]);
}

// ── 不同 tool call 的 tool_output_delta 各管各合并，互不串。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "a" }));
  batcher.push(ev({ type: "tool_output_delta", index: 1, id: "t2", chunk: "x" }));
  batcher.push(ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "b" }));

  // 按 push 顺序 flush 两条
  batcher.flushNow();
  // Map 迭代顺序 = 插入顺序 → t1 先 t2 后（t2 在 t1 两次之间插入，key 不同仍保持各自累积）
  check("不同 tool call 独立合并", handled.length, 2);
  const t1 = handled.find((e): e is Extract<EngineEvent, { type: "tool_output_delta" }> => e.type === "tool_output_delta" && e.id === "t1");
  const t2 = handled.find((e): e is Extract<EngineEvent, { type: "tool_output_delta" }> => e.type === "tool_output_delta" && e.id === "t2");
  check("t1 chunk 合并", t1?.chunk, "ab");
  check("t2 chunk 不变", t2?.chunk, "x");
}

// ── 非 output 事件前先 flush 累积的输出（与 text_delta 一致语义）。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "log\n" }));
  batcher.push(ev({ type: "tool_done", index: 0, id: "t1", result: "ok", duration_ms: 3 }));

  check("tool_done 前 flush tool_output_delta + 即时派发 tool_done",
    handled.length >= 2 &&
    handled[0].type === "tool_output_delta" &&
    handled[handled.length - 1].type === "tool_done",
    true
  );
}

// ── text_delta 与 tool_output_delta 共享同一个 schedule 槽，flush 时先 text 再 output。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "text_delta", text: "Hello" }));
  batcher.push(ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "out" }));
  check("text + output 都不立即 dispatch", handled.length, 0);
  batcher.flushNow();
  check("flush 后 text_delta 在前、tool_output_delta 在后", handled, [
    ev({ type: "text_delta", text: "Hello" }),
    ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "out" }),
  ]);
}

// ── reasoning 批处理：与 text_delta 同语义，按 rAF 帧合并。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "reasoning", text: "嗯" }));
  batcher.push(ev({ type: "reasoning", text: "……" }));

  check("reasoning 入队前不立刻 dispatch", handled.length, 0);
  batcher.flushNow();
  check("reasoning 合并成一条", handled, [
    ev({ type: "reasoning", text: "嗯……" }),
  ]);
}

// ── tool_call_delta 批处理：按 tool call 累积 arguments_delta。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "tool_call_delta", index: 0, id: "t1", name: "Bash", arguments_delta: '{"c' }));
  batcher.push(ev({ type: "tool_call_delta", index: 0, id: "t1", name: "Bash", arguments_delta: 'md":"ls"' }));

  check("tool_call_delta 入队前不立刻 dispatch", handled.length, 0);
  batcher.flushNow();
  check("同 tool call 的 arguments_delta 合并", handled, [
    ev({ type: "tool_call_delta", index: 0, id: "t1", name: "Bash", arguments_delta: '{"cmd":"ls"' }),
  ]);
}

// ── 不同 tool call 的 tool_call_delta 各管各合并。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "tool_call_delta", index: 0, id: "t1", arguments_delta: "a" }));
  batcher.push(ev({ type: "tool_call_delta", index: 1, id: "t2", arguments_delta: "x" }));
  batcher.push(ev({ type: "tool_call_delta", index: 0, id: "t1", arguments_delta: "b" }));

  batcher.flushNow();
  check("不同 tool call delta 独立合并", handled.length, 2);
  const d1 = handled.find((e): e is Extract<EngineEvent, { type: "tool_call_delta" }> => e.type === "tool_call_delta" && e.id === "t1");
  const d2 = handled.find((e): e is Extract<EngineEvent, { type: "tool_call_delta" }> => e.type === "tool_call_delta" && e.id === "t2");
  check("t1 arguments 合并", d1?.arguments_delta, "ab");
  check("t2 arguments 不变", d2?.arguments_delta, "x");
}

// ── 四种高频事件共享 schedule 槽，flush 顺序：text → reasoning → call_delta → output。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "text_delta", text: "Hello" }));
  batcher.push(ev({ type: "reasoning", text: "think" }));
  batcher.push(ev({ type: "tool_call_delta", index: 0, id: "t1", arguments_delta: "arg" }));
  batcher.push(ev({ type: "tool_output_delta", index: 0, id: "t1", chunk: "out" }));

  check("四种都不立即 dispatch", handled.length, 0);
  batcher.flushNow();
  check("flush 顺序: text → reasoning → call_delta → output", handled.map((e) => e.type), [
    "text_delta",
    "reasoning",
    "tool_call_delta",
    "tool_output_delta",
  ]);
}

// ── reasoning + text 交替到达时必须保序（不能塌成全 text 先、全 reasoning 后）。
{
  const handled: EngineEvent[] = [];
  const batcher = createEventBatcher({
    dispatch: (event) => handled.push(event),
    schedule: (flush) => flush,
  });

  batcher.push(ev({ type: "reasoning", text: "先想" }));
  batcher.push(ev({ type: "text_delta", text: "输出" }));
  batcher.push(ev({ type: "reasoning", text: "再想" }));

  batcher.flushNow();
  check("reasoning→text→reasoning 保持交替顺序", handled.map((e) => e.type), [
    "reasoning",
    "text_delta",
    "reasoning",
  ]);
}

// ── cron 唤醒必须触发 assistant 分段冻结（问题 3 回归）。
// cron_fired 是新对话轮次（后端也落成独立 assistant message）；若不冻结，每轮唤醒的
// 输出全叠进同一个 streaming bubble，无限堆叠 + tool 卡片糊成一团。
// 反例（修前）：system_notification 一律不算插队 → 永不冻结 → 复现堆叠。
{
  const cronMsg = {
    id: "u-cron",
    role: "user",
    content: "<wakeup kind=cron_fired>",
    created_at: 1,
    meta: { type: "system_notification", kind: "cron_fired" },
  };
  let slot = makeSlot({
    streamingText: "第一轮检查结果",
    streamingParts: [{ type: "text", text: "第一轮检查结果" } as StreamingAssistantPart],
    liveTimeline: [{ kind: "user_injected", message: cronMsg } as never],
    assistantInsertPos: 0,
  });
  slot = applyEventToSlot(slot, ev({ type: "turn_finished", stop_reason: "end_turn" }));
  check("cron 唤醒后当前段被冻结进 liveTimeline", slot.liveTimeline.filter((i) => i.kind === "assistant_frozen").length, 1);
  check("cron 唤醒后 streaming 段清空（不再堆叠）", slot.streamingText, "");
}

// ── bg_task_finished 不触发分段：它是某 tool_call 的异步回应、不是新对话轮次，
// 由 wakeup 排序钉到对应 assistant 段之后，继续累积进当前 bubble。
{
  const bgMsg = {
    id: "u-bg",
    role: "user",
    content: "<wakeup kind=bg_task_finished>",
    created_at: 1,
    meta: { type: "system_notification", kind: "bg_task_finished", task_id: "bash_001" },
  };
  let slot = makeSlot({
    streamingText: "正在等后台",
    streamingParts: [{ type: "text", text: "正在等后台" } as StreamingAssistantPart],
    liveTimeline: [{ kind: "user_injected", message: bgMsg } as never],
    assistantInsertPos: 0,
  });
  slot = applyEventToSlot(slot, ev({ type: "turn_finished", stop_reason: "end_turn" }));
  check("bg 任务完成不冻结（不分段）", slot.liveTimeline.filter((i) => i.kind === "assistant_frozen").length, 0);
  check("bg 任务完成保持累积", slot.streamingText, "正在等后台");
}

// ── 普通用户插队仍触发分段（不被误伤）。
{
  const userMsg = { id: "u1", role: "user", content: "顺便看下这个", created_at: 1 };
  let slot = makeSlot({
    streamingText: "回答中",
    streamingParts: [{ type: "text", text: "回答中" } as StreamingAssistantPart],
    liveTimeline: [{ kind: "user_injected", message: userMsg } as never],
    assistantInsertPos: 0,
  });
  slot = applyEventToSlot(slot, ev({ type: "turn_finished", stop_reason: "end_turn" }));
  check("普通用户插队仍冻结分段", slot.liveTimeline.filter((i) => i.kind === "assistant_frozen").length, 1);
}

console.log("ALL PASS");