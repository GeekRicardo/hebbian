export type RunningTimelineRenderItem<T> =
  | { kind: "timeline"; item: T }
  | { kind: "streaming" };

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
  hasStreaming: boolean
): Array<RunningTimelineRenderItem<T>> {
  if (!hasStreaming) return items.map((item) => ({ kind: "timeline", item }));
  const insertAt = Math.max(0, Math.min(streamingInsertPos, items.length));
  return [
    ...items.slice(0, insertAt).map((item) => ({ kind: "timeline" as const, item })),
    { kind: "streaming" as const },
    ...items.slice(insertAt).map((item) => ({ kind: "timeline" as const, item })),
  ];
}
