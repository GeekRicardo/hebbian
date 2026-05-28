import {
  appendInjectedMessageAfterCurrentAssistant,
  filterMessagesDuplicatedInLiveTimeline,
  runningTimelineRenderItems,
} from "./liveTimelineOrder.ts";

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

const notificationDuringTool = appendInjectedMessageAfterCurrentAssistant<Item, string>(
  {
    liveTimeline: [{ id: "older-user" }],
    assistantInsertPos: 0,
    streamingText: "current assistant",
    streamingParts: ["current assistant"],
  },
  { id: "notification" }
);

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

const notificationWithoutStreaming = appendInjectedMessageAfterCurrentAssistant<Item>(
  {
    liveTimeline: [{ id: "older-user" }],
    assistantInsertPos: 0,
    streamingText: "",
    streamingParts: [],
  },
  { id: "notification" }
);

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
