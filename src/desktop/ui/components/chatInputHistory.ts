export interface ChatInputHistoryState {
  index: number | null;
}

export interface ChatInputHistoryRequest {
  direction: "older" | "newer";
  currentValue: string;
  history: string[];
  state: ChatInputHistoryState;
}

export interface ChatInputHistoryResult {
  handled: boolean;
  value: string;
  state: ChatInputHistoryState;
}

export function getHistoryDraft({
  direction,
  currentValue,
  history,
  state,
}: ChatInputHistoryRequest): ChatInputHistoryResult {
  if (history.length === 0) {
    return { handled: false, value: currentValue, state };
  }

  if (direction === "older") {
    if (state.index === null && currentValue.length > 0) {
      return { handled: false, value: currentValue, state };
    }

    const nextIndex =
      state.index === null
        ? history.length - 1
        : Math.max(0, Math.min(state.index - 1, history.length - 1));

    return {
      handled: true,
      value: history[nextIndex],
      state: { index: nextIndex },
    };
  }

  if (state.index === null) {
    return { handled: false, value: currentValue, state };
  }

  if (state.index >= history.length - 1) {
    return {
      handled: true,
      value: "",
      state: { index: null },
    };
  }

  const nextIndex = Math.max(0, state.index + 1);
  return {
    handled: true,
    value: history[nextIndex],
    state: { index: nextIndex },
  };
}
