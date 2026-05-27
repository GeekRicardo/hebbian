import { filterMessagesDuplicatedInLiveTimeline, runningTimelineRenderItems } from "./liveTimelineOrder";

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
