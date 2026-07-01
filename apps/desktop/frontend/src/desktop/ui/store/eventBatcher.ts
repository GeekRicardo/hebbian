import type { EngineEvent } from "@/desktop/ui/types";

type Flush = () => void;
type Schedule = (flush: Flush) => unknown;

export type EventBatcher = {
  push(event: EngineEvent): void;
  flushNow(): void;
  dispose(): void;
};

function defaultSchedule(flush: Flush): unknown {
  if (typeof requestAnimationFrame === "function") {
    return requestAnimationFrame(flush);
  }
  return setTimeout(flush, 16);
}

export function createEventBatcher(options: {
  dispatch: (event: EngineEvent) => void;
  schedule?: Schedule;
}): EventBatcher {
  const schedule = options.schedule ?? defaultSchedule;
  let text = "";
  let scheduled = false;
  let disposed = false;

  const flushNow = () => {
    if (disposed) return;
    scheduled = false;
    if (!text) return;
    const merged = text;
    text = "";
    options.dispatch({ type: "text_delta", text: merged } as EngineEvent);
  };

  return {
    push(event) {
      if (disposed) return;
      if (event.type !== "text_delta") {
        flushNow();
        options.dispatch(event);
        return;
      }
      if (!event.text) return;
      text += event.text;
      if (!scheduled) {
        scheduled = true;
        schedule(flushNow);
      }
    },
    flushNow,
    dispose() {
      flushNow();
      disposed = true;
      text = "";
      scheduled = false;
    },
  };
}
