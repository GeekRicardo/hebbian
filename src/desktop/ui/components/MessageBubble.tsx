import { memo, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  Copy,
  FileText,
  GitBranch,
  RefreshCw,
  User,
  Bot,
  Check,
  MoreHorizontal,
  ArrowRightLeft,
  Ban,
  ChevronDown,
  ChevronRight,
  CheckCircle2,
  Loader2,
  Wrench,
} from "lucide-react";
import type {
  Message,
  MessagePart,
  Prompt,
  StreamingAssistantPart,
  ToolCallStatus,
} from "@/desktop/ui/types";
import { cn } from "@/desktop/ui/lib/utils";
import { toast } from "sonner";
import { animations } from "@/assets/animations";
import { LoopingWebm } from "@/desktop/ui/components/LoopingWebm";
import { AttachmentPreviewStrip } from "@/desktop/ui/components/AttachmentPreviewStrip";
import { AvatarPreview } from "@/desktop/ui/components/AvatarField";
import { findMatches, highlight } from "./FindBar";
import {
  canShowRawMessage,
  getMessageRawText,
} from "@/desktop/ui/lib/messageRawText";

interface Props {
  message: Message;
  streaming?: boolean;
  prompt?: Prompt;
  userAvatar?: string;
  onFork?: (id: string) => void;
  onRegenerate?: (id: string) => void;
  streamingParts?: StreamingAssistantPart[];
  /** 若提供则进入"查找模式"，以纯文本 + 高亮渲染 */
  find?: {
    query: string;
    regex: boolean;
    caseSensitive: boolean;
    activeLocalIdx: number | null;
    matchBaseIdx: number;
  };
}

interface ToolCallItem {
  key: string;
  index: number;
  id?: string | null;
  name?: string | null;
  argumentsText: string;
  result?: string | null;
  durationMs?: number | null;
  status: ToolCallStatus;
}

type AssistantRenderPart =
  | { type: "text"; key: string; text: string }
  | { type: "tool_group"; key: string; calls: ToolCallItem[] };

function formatJsonLike(value: unknown): string {
  if (value === undefined || value === null) return "";
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (!trimmed) return value;
    try {
      return JSON.stringify(JSON.parse(trimmed), null, 2);
    } catch {
      return value;
    }
  }
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function normalizeStreamingToolPart(
  part: Extract<StreamingAssistantPart, { type: "tool_call" }>,
  index: number
): ToolCallItem {
  return {
    key: `streaming-${part.index}`,
    index: part.index ?? index,
    id: part.id,
    name: part.name,
    argumentsText:
      part.input === undefined
        ? formatJsonLike(part.arguments)
        : formatJsonLike(part.input),
    result: part.result,
    durationMs: part.duration_ms,
    status: part.status,
  };
}

function normalizeSavedToolPart(
  part: Extract<MessagePart, { type: "tool_call" }>,
  index: number
): ToolCallItem {
  return {
    key: `saved-part-${index}-${part.id}`,
    index,
    id: part.id,
    name: part.name,
    argumentsText: part.arguments || formatJsonLike(part.input),
    result: part.result,
    durationMs: part.duration_ms,
    status: part.result ? "done" : "running",
  };
}

function normalizeLegacyToolCall(
  call: NonNullable<Message["tool_calls"]>[number],
  index: number
): ToolCallItem {
  return {
    key: `saved-${index}-${call.id}`,
    index,
    id: call.id,
    name: call.name,
    argumentsText: formatJsonLike(call.input),
    result: call.result,
    durationMs: call.duration_ms,
    status: call.result ? "done" : "running",
  };
}

function pushToolGroup(
  out: AssistantRenderPart[],
  pendingTools: ToolCallItem[]
) {
  if (pendingTools.length === 0) return;
  out.push({
    type: "tool_group",
    key: `tool-group-${out.length}-${pendingTools[0].key}`,
    calls: [...pendingTools],
  });
  pendingTools.length = 0;
}

function buildAssistantRenderParts(
  message: Message,
  streamingParts?: StreamingAssistantPart[]
): AssistantRenderPart[] {
  const out: AssistantRenderPart[] = [];
  const pendingTools: ToolCallItem[] = [];

  if (streamingParts?.length) {
    streamingParts.forEach((part, index) => {
      if (part.type === "text") {
        pushToolGroup(out, pendingTools);
        out.push({ type: "text", key: `stream-text-${index}`, text: part.text });
      } else {
        pendingTools.push(normalizeStreamingToolPart(part, index));
      }
    });
    pushToolGroup(out, pendingTools);
    return out;
  }

  if (message.parts?.length) {
    message.parts.forEach((part, index) => {
      if (part.type === "text") {
        pushToolGroup(out, pendingTools);
        out.push({ type: "text", key: `saved-text-${index}`, text: part.text });
      } else {
        pendingTools.push(normalizeSavedToolPart(part, index));
      }
    });
    pushToolGroup(out, pendingTools);
    return out;
  }

  if (message.content) {
    out.push({ type: "text", key: "legacy-text", text: message.content });
  }
  const legacyCalls = (message.tool_calls ?? []).map(normalizeLegacyToolCall);
  if (legacyCalls.length > 0) {
    out.push({ type: "tool_group", key: "legacy-tools", calls: legacyCalls });
  }
  return out;
}

function statusLabel(status: ToolCallItem["status"]) {
  if (status === "done") return "完成";
  if (status === "running") return "执行中";
  return "生成参数";
}

function ToolStatusIcon({ status }: { status: ToolCallItem["status"] }) {
  if (status === "done") {
    return <CheckCircle2 className="h-3.5 w-3.5 text-emerald-500" />;
  }
  if (status === "running") {
    return <Loader2 className="h-3.5 w-3.5 animate-spin text-primary" />;
  }
  return <span className="h-2 w-2 rounded-full bg-primary animate-pulse" />;
}

function ToolCallStrip({
  calls,
  expandedKey,
  onToggle,
}: {
  calls: ToolCallItem[];
  expandedKey: string | null;
  onToggle: (key: string) => void;
}) {
  if (calls.length === 0) return null;

  return (
    <div className="mt-3 space-y-2">
      {calls.map((call) => {
        const active = call.key === expandedKey;
        const label = call.name || "工具调用";
        return (
          <div key={call.key} className="space-y-1.5">
            <button
              type="button"
              onClick={() => onToggle(call.key)}
              className={cn(
                "inline-flex h-7 max-w-full items-center gap-1.5 rounded-md border px-2 text-[11px] transition-colors",
                active
                  ? "border-primary/40 bg-primary/10 text-primary"
                  : "border-border bg-background/80 text-muted-foreground hover:bg-accent"
              )}
              title={label}
            >
              <Wrench className="h-3.5 w-3.5 shrink-0" />
              <span className="max-w-[180px] truncate font-medium">{label}</span>
              <ToolStatusIcon status={call.status} />
              <span className="shrink-0 text-[10px]">
                {statusLabel(call.status)}
              </span>
              {active ? (
                <ChevronDown className="h-3 w-3 shrink-0" />
              ) : (
                <ChevronRight className="h-3 w-3 shrink-0" />
              )}
            </button>

            {active && (
              <div className="rounded-md border border-border bg-background/70 p-3 text-xs shadow-sm">
                <div className="mb-2 flex min-w-0 items-center justify-between gap-2">
                  <div className="flex min-w-0 items-center gap-1.5">
                    <Wrench className="h-3.5 w-3.5 shrink-0 text-primary" />
                    <span className="truncate font-medium">
                      {call.name || "工具调用"}
                    </span>
                    {call.id && (
                      <span className="truncate text-[10px] text-muted-foreground">
                        {call.id}
                      </span>
                    )}
                  </div>
                  {call.durationMs !== undefined && call.durationMs !== null && (
                    <span className="shrink-0 text-[10px] text-muted-foreground">
                      {(call.durationMs / 1000).toFixed(1)}s
                    </span>
                  )}
                </div>
                <div className="grid gap-2 md:grid-cols-2">
                  <div className="min-w-0">
                    <div className="mb-1 text-[10px] font-medium uppercase text-muted-foreground">
                      入参
                    </div>
                    <pre className="max-h-56 overflow-auto rounded-md bg-muted/70 p-2 text-[11px] leading-relaxed text-foreground whitespace-pre-wrap break-words">
                      {call.argumentsText || "等待入参…"}
                    </pre>
                  </div>
                  <div className="min-w-0">
                    <div className="mb-1 text-[10px] font-medium uppercase text-muted-foreground">
                      返回值
                    </div>
                    <pre className="max-h-56 overflow-auto rounded-md bg-muted/70 p-2 text-[11px] leading-relaxed text-foreground whitespace-pre-wrap break-words">
                      {call.result || "等待返回…"}
                    </pre>
                  </div>
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function extractText(node: React.ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (typeof node === "object" && "props" in node) {
    return extractText((node as React.ReactElement<{ children?: React.ReactNode }>).props.children);
  }
  return "";
}

function CodeBlock({ children, ...rest }: React.HTMLAttributes<HTMLPreElement>) {
  const [copied, setCopied] = useState(false);
  const code = extractText(children).replace(/\n$/, "");

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("复制失败");
    }
  }

  return (
    <div className="group/code relative">
      <pre {...rest}>{children}</pre>
      <button
        type="button"
        onClick={handleCopy}
        title="复制代码"
        className="absolute right-2 top-2 inline-flex items-center gap-1 rounded border border-border bg-background/80 px-1.5 py-1 text-xs text-muted-foreground opacity-0 transition-opacity hover:bg-background group-hover/code:opacity-100"
      >
        {copied ? (
          <Check className="h-3.5 w-3.5 text-emerald-500" />
        ) : (
          <Copy className="h-3.5 w-3.5" />
        )}
      </button>
    </div>
  );
}

const markdownComponents = { pre: CodeBlock } satisfies React.ComponentProps<
  typeof ReactMarkdown
>["components"];

function AssistantParts({
  parts,
  streaming,
  expandedKey,
  onToggle,
}: {
  parts: AssistantRenderPart[];
  streaming?: boolean;
  expandedKey: string | null;
  onToggle: (key: string) => void;
}) {
  if (parts.length === 0) {
    return streaming ? <span>▍</span> : null;
  }

  return (
    <div className="space-y-3">
      {parts.map((part) =>
        part.type === "text" ? (
          <div key={part.key} className="markdown-segment">
            <ReactMarkdown
              remarkPlugins={[remarkGfm]}
              components={markdownComponents}
            >
              {part.text || (streaming ? "▍" : "")}
            </ReactMarkdown>
          </div>
        ) : (
          <ToolCallStrip
            key={part.key}
            calls={part.calls}
            expandedKey={expandedKey}
            onToggle={onToggle}
          />
        )
      )}
    </div>
  );
}

export const MessageBubble = memo(function MessageBubble({
  message,
  streaming,
  prompt,
  userAvatar,
  onFork,
  onRegenerate,
  streamingParts,
  find,
}: Props) {
  const [copied, setCopied] = useState(false);
  const [expandedToolCall, setExpandedToolCall] = useState<string | null>(null);
  const [showRawText, setShowRawText] = useState(false);
  const [actionMenuOpen, setActionMenuOpen] = useState(false);
  const actionMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!actionMenuOpen) return;

    const onMouseDown = (event: MouseEvent) => {
      if (actionMenuRef.current?.contains(event.target as Node)) return;
      setActionMenuOpen(false);
    };

    window.addEventListener("mousedown", onMouseDown);
    return () => window.removeEventListener("mousedown", onMouseDown);
  }, [actionMenuOpen]);

  if (message.role === "marker" && message.meta?.type === "switch") {
    const { from_provider, from_model, to_provider, to_model } = message.meta;
    return (
      <div className="px-6 py-3 flex items-center gap-3 text-[11px] text-muted-foreground select-none">
        <div className="flex-1 h-px bg-border" />
        <div className="inline-flex items-center gap-1.5 rounded-full border border-border bg-background px-2.5 py-1">
          <ArrowRightLeft className="w-3 h-3" />
          <span className="font-medium text-foreground/70">
            {from_provider} · {from_model}
          </span>
          <span>→</span>
          <span className="font-medium text-primary">
            {to_provider} · {to_model}
          </span>
        </div>
        <div className="flex-1 h-px bg-border" />
      </div>
    );
  }

  if (message.role === "marker" && message.meta?.type === "interrupted") {
    return (
      <div className="px-6 py-3 flex items-center gap-3 text-[11px] text-muted-foreground select-none">
        <div className="flex-1 h-px bg-border" />
        <div className="inline-flex items-center gap-1.5 rounded-full border border-border bg-background px-2.5 py-1">
          <Ban className="w-3 h-3 text-destructive" />
          <span className="font-medium text-foreground/70">
            当前对话已打断
          </span>
        </div>
        <div className="flex-1 h-px bg-border" />
      </div>
    );
  }

  const isUser = message.role === "user";
  const assistantParts = buildAssistantRenderParts(message, streamingParts);
  const rawText = getMessageRawText(message);
  const canToggleRawText = !streaming && canShowRawMessage(message);

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(message.content);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("复制失败");
    }
  }

  // 查找模式：以纯文本 + 高亮渲染（Markdown 让路）
  const findActive = !!find && find.query.length > 0;
  let body: React.ReactNode;
  if (findActive) {
    const matches = findMatches(
      message.content,
      find!.query,
      find!.regex,
      find!.caseSensitive
    );
    body = (
      <div className="whitespace-pre-wrap font-sans">
        {highlight(message.content, matches, find!.activeLocalIdx, message.id)}
      </div>
    );
  } else if (showRawText && canToggleRawText) {
    body = (
      <div className="whitespace-pre-wrap break-words rounded-md border border-border bg-muted/40 px-3 py-2 font-mono text-[13px] leading-relaxed text-foreground">
        {rawText}
      </div>
    );
  } else if (isUser) {
    body = message.content ? (
      message.content.includes("```") ? (
        <div className="markdown-segment">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            components={markdownComponents}
          >
            {message.content}
          </ReactMarkdown>
        </div>
      ) : (
        <div className="whitespace-pre-wrap">{message.content}</div>
      )
    ) : null;
  } else {
    body = (
      <AssistantParts
        parts={assistantParts}
        streaming={streaming}
        expandedKey={expandedToolCall}
        onToggle={(key) =>
          setExpandedToolCall((current) => (current === key ? null : key))
        }
      />
    );
  }

  return (
    <div
      className={cn(
        "group relative flex gap-3 px-6 py-4",
        isUser ? "bg-background" : "bg-accent/30"
      )}
    >
      {canToggleRawText && (
        <div
          ref={actionMenuRef}
          className={cn(
            "absolute right-4 top-3 z-20 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100",
            actionMenuOpen && "opacity-100"
          )}
        >
          <button
            type="button"
            onClick={(event) => {
              event.stopPropagation();
              setActionMenuOpen((open) => !open);
            }}
            className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            aria-label="消息操作"
            title="消息操作"
          >
            <MoreHorizontal className="h-4 w-4" />
          </button>
          {actionMenuOpen && (
            <div
              onClick={(event) => event.stopPropagation()}
              className="absolute right-0 mt-1 w-32 rounded-md border border-border bg-card py-1 text-xs shadow-lg"
            >
              <button
                type="button"
                onClick={() => {
                  setShowRawText((show) => !show);
                  setActionMenuOpen(false);
                }}
                className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                <FileText className="h-3.5 w-3.5" />
                <span>{showRawText ? "显示渲染" : "显示原文"}</span>
              </button>
            </div>
          )}
        </div>
      )}
      <div className="shrink-0 flex w-7 flex-col items-center gap-1.5">
        <AvatarPreview
          value={isUser ? userAvatar : prompt?.avatar}
          fallback={
            isUser ? (
              <User className="w-4 h-4" />
            ) : (
              <Bot className="w-4 h-4" />
            )
          }
          className={cn(
            "h-7 w-7 text-sm",
            isUser
              ? "bg-gradient-to-br from-sky-500 to-blue-600 text-white"
              : "bg-transparent text-muted-foreground"
          )}
          title={!isUser && prompt ? prompt.name : undefined}
        />
        {!isUser && streaming && (
          <LoopingWebm
            src={animations.assistantThinking}
            className="h-8 w-8 rounded-full"
          />
        )}
      </div>
      <div className={cn("flex-1 min-w-0", canToggleRawText && "pr-8")}>
        <div className="flex items-center gap-2 mb-1.5 text-xs">
          <span className="font-medium">
            {isUser ? "你" : prompt?.name ?? "助手"}
          </span>
          {streaming && (
            <span className="inline-flex items-center gap-1 text-primary">
              <span className="h-1.5 w-1.5 bg-primary rounded-full animate-pulse" />
              生成中…
            </span>
          )}
        </div>
        <div className="markdown text-[14px] leading-relaxed break-words">
          {body}
        </div>
        <AttachmentPreviewStrip
          attachments={message.attachments}
          variant={isUser ? "compact" : "gallery"}
          className="mt-2"
        />
        {!streaming && (
          <div className="opacity-0 group-hover:opacity-100 transition-opacity flex items-center gap-1 mt-2 -ml-1.5">
            <button
              onClick={handleCopy}
              className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-xs"
              title="复制"
            >
              {copied ? (
                <Check className="w-3.5 h-3.5 text-emerald-500" />
              ) : (
                <Copy className="w-3.5 h-3.5" />
              )}
            </button>
            {onFork && (
              <button
                onClick={() => onFork(message.id)}
                className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-xs"
                title="从此处分叉新对话"
              >
                <GitBranch className="w-3.5 h-3.5" />
                <span>分叉</span>
              </button>
            )}
            {!isUser && onRegenerate && (
              <button
                onClick={() => onRegenerate(message.id)}
                className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-xs"
                title="重新生成"
              >
                <RefreshCw className="w-3.5 h-3.5" />
                <span>重新生成</span>
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
});
