import { applyEventToSlot } from "./slotReducer.ts";
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
    contextCompacted: null,
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

console.log("ALL PASS");
