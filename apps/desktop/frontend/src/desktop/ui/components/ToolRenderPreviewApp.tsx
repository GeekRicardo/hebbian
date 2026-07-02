import { useEffect, useMemo, useState } from "react";
import { MessageBubble } from "@/desktop/ui/components/MessageBubble";
import fixture from "@/desktop/ui/fixtures/toolPreviewLatestRun.json";
import type { AppSettings, Message, StreamingAssistantPart } from "@/desktop/ui/types";

const appSettings: AppSettings = {
  general: {
    launch_at_login: false,
    language: "zh-cn",
    show_grep_search_path: true,
    shell: null,
    log_enabled: false,
    edit_backend: "string-replace",
    continue_strategy: "resume_loop",
    link_open_target: "system",
    channel_idle_forward_minutes: 0,
  },
  conversation: {
    workdir: fixture.session.workdir,
    allowed_paths: fixture.session.allowed_paths,
    enabled_tools: [],
    skill_dirs: [],
    global_rules: [],
  },
  agents: {},
  memory: {
    enabled: false,
    models: [],
  },
};

function toStreamingParts(message: Message): StreamingAssistantPart[] {
  return (message.parts ?? []).map((part, index): StreamingAssistantPart => {
    if (part.type === "text") return { type: "text", text: part.text };
    if (part.type === "reasoning") {
      return {
        type: "reasoning",
        text: part.text,
        duration_ms: part.duration_ms,
      };
    }
    return {
      type: "tool_call",
      index,
      id: part.id,
      name: part.name,
      arguments:
        part.arguments ??
        (typeof part.input === "string" ? part.input : JSON.stringify(part.input ?? {}, null, 2)),
      input: part.input,
      result: part.result ?? null,
      duration_ms: part.duration_ms,
      status: "done",
      is_error: part.is_error,
      artifact_path: part.artifact_path ?? null,
      live_output: part.result ?? undefined,
    };
  });
}

function markLatestToolRunning(parts: StreamingAssistantPart[]): StreamingAssistantPart[] {
  const next = parts.map((part) => ({ ...part }));
  for (let i = next.length - 1; i >= 0; i -= 1) {
    const part = next[i];
    if (part.type !== "tool_call") continue;
    part.status = "running";
    part.live_output = part.result ?? part.live_output ?? undefined;
    part.result = null;
    return next;
  }
  return next;
}

export function ToolRenderPreviewApp() {
  const [streaming, setStreaming] = useState(true);
  const [playing, setPlaying] = useState(true);
  const [slice, setSlice] = useState(1);
  const message = fixture.message as Message;
  const streamingParts = useMemo(() => toStreamingParts(message), [message]);
  const effectiveSlice = Math.max(1, Math.min(streamingParts.length, slice));
  const visibleParts = streaming
    ? markLatestToolRunning(streamingParts.slice(0, effectiveSlice))
    : undefined;
  const shownMessage = streaming ? { ...message, content: "", parts: [] } : message;

  useEffect(() => {
    if (!streaming || !playing) return;
    const timer = window.setInterval(() => {
      setSlice((current) => (current >= streamingParts.length ? 1 : current + 1));
    }, 900);
    return () => window.clearInterval(timer);
  }, [playing, streaming, streamingParts.length]);

  return (
    <div className="tool-render-preview min-h-screen bg-background text-foreground">
      <div className="mx-auto max-w-[1180px] px-8 py-6">
        <div className="sticky top-0 z-10 mb-4 flex flex-wrap items-center gap-2 border-b border-border bg-background/95 py-3 text-[12px] text-muted-foreground backdrop-blur">
          <span className="font-medium text-foreground">真实 MessageBubble 预览</span>
          <span>session: {fixture.session.id}</span>
          <span>message: {message.id}</span>
          <span>parts: {streamingParts.length}</span>
          <button
            type="button"
            className="ml-auto rounded-md border border-border px-2 py-1 text-foreground hover:bg-muted"
            onClick={() => setPlaying((v) => !v)}
            disabled={!streaming}
          >
            {playing ? "暂停" : "继续"}
          </button>
          <button
            type="button"
            className="rounded-md border border-border px-2 py-1 text-foreground hover:bg-muted"
            onClick={() => setStreaming((v) => !v)}
          >
            {streaming ? "切到完成态" : "切到运行态"}
          </button>
          <label className="flex items-center gap-2">
            <span>运行态 part 截止</span>
            <input
              type="range"
              min={1}
              max={streamingParts.length}
              value={effectiveSlice}
              onChange={(e) => {
                setPlaying(false);
                setSlice(Number(e.target.value));
              }}
            />
            <span>{effectiveSlice}</span>
          </label>
        </div>
        <div className="tool-preview-chat-canvas rounded-2xl p-5 shadow-sm">
          <MessageBubble
            message={shownMessage}
            streaming={streaming}
            streamingParts={visibleParts}
            sessionId={fixture.session.id}
            appSettings={appSettings}
          />
        </div>
      </div>
    </div>
  );
}
