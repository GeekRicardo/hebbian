export const COMPOSITION_END_ENTER_GRACE_MS = 80;

export interface ChatInputKeyEvent {
  key: string;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
  timeStamp: number;
}

export interface ChatInputCompositionState {
  isComposing: boolean;
  lastCompositionEndAt: number;
}

export function shouldSubmitChatInput(
  event: ChatInputKeyEvent,
  composition: ChatInputCompositionState
) {
  if (event.key !== "Enter" || event.shiftKey) return false;

  const justEndedComposition =
    composition.lastCompositionEndAt > 0 &&
    event.timeStamp >= composition.lastCompositionEndAt &&
    event.timeStamp - composition.lastCompositionEndAt <
      COMPOSITION_END_ENTER_GRACE_MS;

  return !(
    composition.isComposing ||
    event.isComposing ||
    event.keyCode === 229 ||
    justEndedComposition
  );
}
