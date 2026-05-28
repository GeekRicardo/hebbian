export type RunningTimelineRenderItem<T> =
  | { kind: "timeline"; item: T }
  | { kind: "streaming" };

export type TimelineSplitState<T, Part = unknown> = {
  liveTimeline: T[];
  assistantInsertPos: number;
  streamingText: string;
  streamingParts: Part[];
};

export type TimelineSplitOptions<T, Part = unknown> = {
  makeFrozenAssistant: (input: {
    text: string;
    parts: Part[];
    insertPos: number;
  }) => T;
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

export function insertInjectedMessageBeforeNextAssistant<T, Part = unknown>(
  state: TimelineSplitState<T, Part>,
  injected: T,
  options: TimelineSplitOptions<T, Part>
): TimelineSplitState<T, Part> {
  const next = [...state.liveTimeline];
  const insertAt = Math.max(0, Math.min(state.assistantInsertPos, next.length));
  const hasStreaming =
    state.streamingText.length > 0 || state.streamingParts.length > 0;

  if (hasStreaming) {
    next.splice(
      insertAt,
      0,
      options.makeFrozenAssistant({
        text: state.streamingText,
        parts: state.streamingParts,
        insertPos: insertAt,
      })
    );
  }
  next.push(injected);

  return {
    liveTimeline: next,
    assistantInsertPos: next.length,
    streamingText: "",
    streamingParts: [],
  };
}
