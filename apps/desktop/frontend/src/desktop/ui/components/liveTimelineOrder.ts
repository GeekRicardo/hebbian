export type RunningTimelineRenderItem<T> =
  | { kind: "timeline"; item: T }
  | { kind: "streaming" };

export type TimelineSplitState<T, Part = unknown> = {
  liveTimeline: T[];
  assistantInsertPos: number;
  streamingText: string;
  streamingParts: Part[];
};

export function filterMessagesDuplicatedInLiveTimeline<T extends { id: string }>(
  messages: T[],
  liveTimelineMessages: Array<{ id: string }>
): T[] {
  if (liveTimelineMessages.length === 0) return messages;
  const liveIds = new Set(liveTimelineMessages.map((message) => message.id));
  return messages.filter((message) => !liveIds.has(message.id));
}

export function runningTimelineRenderItems<T>(
  items: T[],
  streamingInsertPos: number,
  shouldShowStreaming: boolean
): Array<RunningTimelineRenderItem<T>> {
  if (!shouldShowStreaming) return items.map((item) => ({ kind: "timeline", item }));
  const insertAt = Math.max(0, Math.min(streamingInsertPos, items.length));
  return [
    ...items.slice(0, insertAt).map((item) => ({ kind: "timeline" as const, item })),
    { kind: "streaming" as const },
    ...items.slice(insertAt).map((item) => ({ kind: "timeline" as const, item })),
  ];
}

export function appendInjectedMessageAfterCurrentAssistant<T, Part = unknown>(
  state: TimelineSplitState<T, Part>,
  injected: T
): TimelineSplitState<T, Part> {
  const next = [...state.liveTimeline, injected];

  return {
    liveTimeline: next,
    assistantInsertPos: Math.max(
      0,
      Math.min(state.assistantInsertPos, state.liveTimeline.length)
    ),
    streamingText: state.streamingText,
    streamingParts: state.streamingParts,
  };
}
