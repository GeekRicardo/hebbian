import {
  filterMessagesDuplicatedInLiveTimeline,
  reorderForWakeupView,
  runningTimelineRenderItems,
} from "./liveTimelineOrder.ts";
import type { Message } from "../types.ts";

type Item = { id: string };

function labels(items: ReturnType<typeof runningTimelineRenderItems<Item>>): string[] {
  return items.map((item) => (item.kind === "streaming" ? "streaming" : item.item.id));
}

function expectOrder(name: string, actual: string[], expected: string[]) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${name}: expected ${e}, got ${a}`);
  }
}

expectOrder(
  "shows assistant placeholder before model text arrives",
  labels(runningTimelineRenderItems([], 0, true)),
  ["streaming"]
);

expectOrder(
  "keeps assistant bubble under avatar while waiting before injected user",
  labels(runningTimelineRenderItems([{ id: "user-1" }], 0, true)),
  ["streaming", "user-1"]
);

expectOrder(
  "puts injected user after current streaming bubble",
  labels(runningTimelineRenderItems([{ id: "user-1" }], 0, true)),
  ["streaming", "user-1"]
);

expectOrder(
  "keeps next turn streaming after prior injection but before new injection",
  labels(
    runningTimelineRenderItems(
      [{ id: "assistant-1" }, { id: "user-1" }, { id: "user-2" }],
      2,
      true
    )
  ),
  ["assistant-1", "user-1", "streaming", "user-2"]
);

expectOrder(
  "does not change frozen timeline when there is no active streaming bubble",
  labels(runningTimelineRenderItems([{ id: "assistant-1" }, { id: "user-1" }], 2, false)),
  ["assistant-1", "user-1"]
);

expectOrder(
  "hides injected messages from persisted history while active run owns them",
  filterMessagesDuplicatedInLiveTimeline(
    [{ id: "initial-user" }, { id: "injected-user" }],
    [{ id: "injected-user" }]
  ).map((m) => m.id),
  ["initial-user"]
);

// 不传 projector 时保持基础行为：注入项追加到 liveTimeline 末尾。
const notificationDuringTool = {
  liveTimeline: [{ id: "older-user" }, { id: "notification" }] as Item[],
  assistantInsertPos: 0,
  streamingText: "current assistant",
  streamingParts: ["current assistant"],
};

expectOrder(
  "system notification waits below current streaming assistant while the tool is still running",
  labels(
    runningTimelineRenderItems(
      notificationDuringTool.liveTimeline,
      notificationDuringTool.assistantInsertPos,
      true
    )
  ),
  ["streaming", "older-user", "notification"]
);

if (notificationDuringTool.streamingText !== "current assistant") {
  throw new Error("system notification should not clear current streaming text");
}

expectOrder(
  "system notification settles below frozen assistant after turn finishes",
  labels(
    runningTimelineRenderItems(
      [{ id: "assistant-current" }, ...notificationDuringTool.liveTimeline],
      3,
      false
    )
  ),
  ["assistant-current", "older-user", "notification"]
);

const notificationWithoutStreaming = {
  liveTimeline: [{ id: "older-user" }, { id: "notification" }] as Item[],
  assistantInsertPos: 0,
  streamingText: "",
  streamingParts: [] as string[],
};

expectOrder(
  "system notification also stays below an empty assistant placeholder",
  labels(
    runningTimelineRenderItems(
      notificationWithoutStreaming.liveTimeline,
      notificationWithoutStreaming.assistantInsertPos,
      true
    )
  ),
  ["streaming", "older-user", "notification"]
);

// ── reorderForWakeupView：reload 后把 system_notification 推迟到目标 assistant 之后 ──

function notificationMsg(id: string, toolUseId: string): Message {
  return {
    id,
    role: "user",
    content: id,
    created_at: 0,
    meta: { type: "system_notification", kind: "bg_task_finished", tool_use_id: toolUseId },
  };
}
function assistantWithToolCall(id: string, callId: string): Message {
  return {
    id,
    role: "assistant",
    content: id,
    created_at: 0,
    tool_calls: [{ id: callId, name: "Bash", input: {} }],
  };
}
function assistantText(id: string): Message {
  return { id, role: "assistant", content: id, created_at: 0 };
}

function reorderedIds(messages: Message[]): string[] {
  return reorderForWakeupView(messages).map((i) => messages[i].id);
}

expectOrder(
  "reorderForWakeupView keeps a notification that is already after its assistant in place",
  reorderedIds([
    assistantWithToolCall("a0", "call_bg"),
    notificationMsg("n0", "call_bg"),
  ]),
  ["a0", "n0"]
);

expectOrder(
  "reorderForWakeupView defers a notification physically before its assistant",
  reorderedIds([
    notificationMsg("n0", "call_bg"),
    assistantWithToolCall("a0", "call_bg"),
  ]),
  ["a0", "n0"]
);

// ── 复现根因：实时渲染顺序 == reload 后顺序 ──
//
// 场景：模型起后台 Bash（call_bg）→ 输出 assistant-A（含该 tool_call）→ 后台任务
// 完成注入 system_notification（tool_use_id=call_bg）→ 模型继续输出 assistant-B。
//
// reload 后物理序 [assistant-A, notification, assistant-B]，reorderForWakeupView
// 把 notification 钉在 assistant-A（含 call_bg）之后：[assistant-A, notification, assistant-B]。
// 实时渲染必须给出同样的相对顺序，否则 run 结束 reload 会跳变。
const reloadOrder = reorderedIds([
  assistantWithToolCall("assistant-A", "call_bg"),
  notificationMsg("notification", "call_bg"),
  assistantText("assistant-B"),
]);
expectOrder(
  "reload order pins the notification right after the assistant that owns its tool call",
  reloadOrder,
  ["assistant-A", "notification", "assistant-B"]
);

// 实时路径：assistant-A 已冻结（assistantInsertPos=1）后注入 notification，
// 此时 assistant-B 仍在 streaming。期望相对序与 reload 一致：
//   assistant-A → notification → (streaming = assistant-B)
//
// 真同源：实时渲染传入 projector，notification（关联 call_bg）被钉到持有 call_bg 的
// assistant-A 之后，而不是被甩到 streaming(assistant-B) 之后。
const liveAfterNotification = {
  liveTimeline: [{ id: "assistant-A" }, { id: "notification" }] as Item[],
  assistantInsertPos: 1,
};
// 模拟语义：assistant-A 段持有 tool_call call_bg；notification 关联 call_bg；
// streaming(assistant-B) 不持有任何相关 tool_call。
const itemToolCalls: Record<string, string[]> = { "assistant-A": ["call_bg"] };
const itemNotification: Record<string, string> = { notification: "call_bg" };
const liveOrder = labels(
  runningTimelineRenderItems(liveAfterNotification.liveTimeline, liveAfterNotification.assistantInsertPos, true, {
    notificationToolUseId: (it) => itemNotification[it.id] ?? null,
    assistantToolCallIds: (it) => itemToolCalls[it.id] ?? null,
    streamingToolCallIds: () => null,
  })
).map((l) => (l === "streaming" ? "assistant-B" : l));
expectOrder(
  "live timeline order matches reload order for a background-task notification",
  liveOrder,
  ["assistant-A", "notification", "assistant-B"]
);


