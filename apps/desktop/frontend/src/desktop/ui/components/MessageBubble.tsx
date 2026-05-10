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
  Brain,
  Table as TableIcon,
  Braces,
  FileJson,
  Pencil,
  X,
} from "lucide-react";
import type {
  Message,
  MessagePart,
  Prompt,
  Session,
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
import { buildModelMessages } from "@/desktop/ui/lib/buildModelMessages";
import { api } from "@/desktop/bridge/tauri";

interface Props {
  message: Message;
  streaming?: boolean;
  prompt?: Prompt;
  userAvatar?: string;
  /** 当前会话:用于在「显示原始 JSON」时拼出截至该消息的 messages 载荷。 */
  session?: Session;
  onFork?: (id: string) => void;
  /**
   * 重新生成。对 assistant 消息：以前一条 user 消息为锚重跑。
   * 对 user 消息：用同样内容 + 附件重跑（被中断 / 失败时可见）。
   */
  onRegenerate?: (id: string) => void;
  /**
   * 编辑当前 user 消息的文本后重跑。仅在最近一条 user 消息上提供。
   * 附件复用原消息的附件（编辑只动文本）。
   */
  onEdit?: (id: string, nextContent: string) => void | Promise<void>;
  streamingParts?: StreamingAssistantPart[];
  /** 若提供则进入"查找模式"，以纯文本 + 高亮渲染 */
  find?: {
    query: string;
    regex: boolean;
    caseSensitive: boolean;
    activeLocalIdx: number | null;
    matchBaseIdx: number;
  };
  /** 处于最近一次 /compact 之前的消息：模型已看不到，UI 上淡化。 */
  archived?: boolean;
  /** 仅 compact_boundary：摘要是否展开（点击主体切换） */
  summaryExpanded?: boolean;
  /** 仅 compact_boundary：点击分隔条主体切换摘要展示 */
  onToggleSummary?: () => void;
  /** 仅 compact_boundary：原始历史是否展开（点击「历史对话」按钮切换） */
  historyExpanded?: boolean;
  /** 仅 compact_boundary：点击「历史对话」按钮切换 */
  onToggleHistory?: () => void;
  /** 仅 compact_boundary：该 boundary 折叠了多少条原始历史消息 */
  archivedCount?: number;
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
  | { type: "reasoning"; key: string; text: string; streaming: boolean }
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
  streamingParts?: StreamingAssistantPart[],
  streaming?: boolean
): AssistantRenderPart[] {
  const out: AssistantRenderPart[] = [];
  const pendingTools: ToolCallItem[] = [];

  if (streamingParts?.length) {
    streamingParts.forEach((part, index) => {
      if (part.type === "text") {
        pushToolGroup(out, pendingTools);
        out.push({ type: "text", key: `stream-text-${index}`, text: part.text });
      } else if (part.type === "reasoning") {
        pushToolGroup(out, pendingTools);
        // 流式时如果末尾就是这一段 reasoning，认为还在写入；
        // 一旦后面有 text/tool 段，就视为已完成、默认折叠。
        const isLast = index === streamingParts.length - 1;
        out.push({
          type: "reasoning",
          key: `stream-reasoning-${index}`,
          text: part.text,
          streaming: !!streaming && isLast,
        });
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
      } else if (part.type === "reasoning") {
        pushToolGroup(out, pendingTools);
        out.push({
          type: "reasoning",
          key: `saved-reasoning-${index}`,
          text: part.text,
          streaming: false,
        });
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

function formatReasoningLabel(
  cfg: import("@/desktop/ui/types").ReasoningConfig | null | undefined
): string {
  if (!cfg) return "默认";
  const enabled = cfg.enabled ?? false;
  const effortText = cfg.effort ?? "extra";
  const long = cfg.long_context ? " · 1M" : "";
  if (!enabled) return `thinking off${long}`;
  return `thinking · ${effortText}${long}`;
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

function previewArgValue(v: unknown): string {
  if (v === null || v === undefined) return "";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

// 切多段连续 JSON：兼容 stream 拼接出 `{"a":1}{"b":2}` 这种形态。
// 用栈做花括号 / 中括号匹配，遇到未闭合就停下，已切出的段照样返回。
function splitJsonSegments(s: string): string[] {
  const out: string[] = [];
  const n = s.length;
  let i = 0;
  while (i < n) {
    while (i < n && /\s/.test(s[i])) i++;
    if (i >= n) break;
    if (s[i] !== "{" && s[i] !== "[") return out;
    const start = i;
    let depth = 0;
    let inStr = false;
    let escape = false;
    for (; i < n; i++) {
      const ch = s[i];
      if (inStr) {
        if (escape) escape = false;
        else if (ch === "\\") escape = true;
        else if (ch === '"') inStr = false;
        continue;
      }
      if (ch === '"') inStr = true;
      else if (ch === "{" || ch === "[") depth++;
      else if (ch === "}" || ch === "]") {
        depth--;
        if (depth === 0) {
          i++;
          out.push(s.slice(start, i));
          break;
        }
      }
    }
    if (depth !== 0) break;
  }
  return out;
}

function buildArgsPreview(
  argumentsText: string
): Array<{ key: string; value: string }> {
  const trimmed = argumentsText.trim();
  if (!trimmed) return [];

  const collect = (
    value: unknown,
    out: Array<{ key: string; value: string }>
  ) => {
    if (value && typeof value === "object" && !Array.isArray(value)) {
      for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
        out.push({ key: k, value: previewArgValue(v) });
      }
    }
  };

  // 1) 直接整段 parse —— 大多数完整 tool_call 走这里
  try {
    const parsed = JSON.parse(trimmed);
    const entries: Array<{ key: string; value: string }> = [];
    collect(parsed, entries);
    if (entries.length > 0) return entries;
  } catch {
    // fallthrough
  }

  // 2) 拆多段连续 JSON —— 兼容 stream / 后端拼接异常
  const segments = splitJsonSegments(trimmed);
  if (segments.length > 0) {
    const entries: Array<{ key: string; value: string }> = [];
    for (const seg of segments) {
      try {
        collect(JSON.parse(seg), entries);
      } catch {
        // 单段坏掉就跳过，剩下的还能展示
      }
    }
    if (entries.length > 0) return entries;
  }

  return [{ key: "", value: trimmed.replace(/\s+/g, " ") }];
}

/** 仅匹配 data:...;base64,... 形式的内嵌资源(主要是图片) */
const DATA_URI_BASE64_RE = /^data:[\w./+-]+;base64,/i;

function Base64DataUriValue({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  async function copy(e: React.MouseEvent) {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("复制失败");
    }
  }
  // 占位用合法 JSON 字符串字面量形式:保留 mime 头,base64 主体折成 …,
  // 全选拷贝拿到的仍是合法 JSON;想取原值用旁边的复制按钮。
  const head = value.slice(0, value.indexOf(",") + 1);
  const placeholder = `"${head}…(${value.length} chars)"`;
  return (
    <span className="inline-flex items-baseline gap-1 break-words">
      <span className="text-emerald-700/80 dark:text-emerald-400/80 italic">
        {placeholder}
      </span>
      <button
        type="button"
        onClick={copy}
        title="复制完整 base64"
        aria-label="复制完整 base64"
        className="select-none inline-flex h-4 w-4 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
      >
        {copied ? (
          <Check className="h-3 w-3 text-emerald-500" />
        ) : (
          <Copy className="h-3 w-3" />
        )}
      </button>
    </span>
  );
}

function JsonPrimitive({ value }: { value: unknown }) {
  if (value === null)
    return <span className="text-muted-foreground">null</span>;
  if (value === undefined)
    return <span className="text-muted-foreground">undefined</span>;
  if (typeof value === "boolean")
    return <span className="text-foreground/90">{String(value)}</span>;
  if (typeof value === "number")
    return <span className="text-foreground/90">{value}</span>;
  if (typeof value === "string") {
    if (DATA_URI_BASE64_RE.test(value)) {
      return <Base64DataUriValue value={value} />;
    }
    return (
      <span className="break-words text-emerald-700 dark:text-emerald-400">
        {JSON.stringify(value)}
      </span>
    );
  }
  return <span>{String(value)}</span>;
}

function JsonKeyLabel({ keyLabel }: { keyLabel: string | number | null }) {
  if (keyLabel === null) return null;
  // 数组索引只是视觉辅助,不属于 JSON 内容,选区里跳过它
  if (typeof keyLabel === "number") {
    return (
      <span className="select-none text-muted-foreground/70">
        <span>{keyLabel}</span>
        <span className="mr-1">:</span>
      </span>
    );
  }
  return (
    <>
      <span className="text-foreground/85">&quot;{keyLabel}&quot;</span>
      <span className="mr-1 text-muted-foreground/70">:</span>
    </>
  );
}

function JsonNode({
  value,
  level,
  keyLabel,
}: {
  value: unknown;
  level: number;
  keyLabel: string | number | null;
}) {
  const [open, setOpen] = useState(true);
  const indent = { paddingLeft: level * 14 } as const;
  const composite =
    value !== null && typeof value === "object" && !(value instanceof Date);

  if (!composite) {
    return (
      <div style={indent} className="flex items-baseline">
        <span className="inline-block w-3.5 shrink-0 select-none" />
        <JsonKeyLabel keyLabel={keyLabel} />
        <JsonPrimitive value={value} />
      </div>
    );
  }

  const isArray = Array.isArray(value);
  const entries: Array<[string | number, unknown]> = isArray
    ? (value as unknown[]).map((v, i) => [i, v])
    : Object.entries(value as Record<string, unknown>);
  const [openBracket, closeBracket] = isArray ? ["[", "]"] : ["{", "}"];

  return (
    <div>
      <div style={indent} className="flex items-baseline">
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          aria-label={open ? "折叠" : "展开"}
          className="inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded hover:bg-accent select-none"
        >
          {open ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronRight className="h-3 w-3" />
          )}
        </button>
        <JsonKeyLabel keyLabel={keyLabel} />
        <span>{openBracket}</span>
        {!open && (
          <>
            {entries.length > 0 && (
              <span className="mx-1 italic text-muted-foreground select-none">
                {entries.length} {isArray ? "项" : "键"}
              </span>
            )}
            <span>{closeBracket}</span>
          </>
        )}
      </div>
      {open && (
        <>
          {entries.map(([k, v]) => (
            <JsonNode
              key={String(k)}
              value={v}
              level={level + 1}
              keyLabel={k}
            />
          ))}
          <div style={indent} className="flex items-baseline">
            <span className="inline-block w-3.5 shrink-0 select-none" />
            <span>{closeBracket}</span>
          </div>
        </>
      )}
    </div>
  );
}

function JsonView({ value }: { value: unknown }) {
  const ref = useRef<HTMLDivElement>(null);

  function handleKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if ((e.metaKey || e.ctrlKey) && (e.key === "a" || e.key === "A")) {
      const node = ref.current;
      if (!node) return;
      e.preventDefault();
      const range = document.createRange();
      range.selectNodeContents(node);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(range);
    }
  }

  return (
    <div
      ref={ref}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      className="max-h-[60vh] overflow-auto rounded-md border border-border bg-muted/40 px-2 py-2 font-mono text-[12px] leading-relaxed text-foreground outline-none focus:ring-1 focus:ring-primary/40"
    >
      <JsonNode value={value} level={0} keyLabel={null} />
    </div>
  );
}

function RawJsonPanel({
  loading,
  error,
  payload,
  fallback,
}: {
  loading: boolean;
  error: string | null;
  payload: unknown;
  fallback: unknown;
}) {
  if (loading && payload == null) {
    return (
      <div className="flex items-center gap-2 rounded-md border border-border bg-muted/40 px-3 py-2 text-[12px] text-muted-foreground">
        <Loader2 className="h-3.5 w-3.5 animate-spin" />
        载入发送给模型的 payload…
      </div>
    );
  }
  if (payload != null) {
    return <JsonView value={payload} />;
  }
  return (
    <div className="space-y-2">
      {error && (
        <div className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-[11px] text-amber-700 dark:text-amber-300">
          {error}（已回退到本地推断的 messages,不含 workspace XML / tools 定义）
        </div>
      )}
      <JsonView value={fallback} />
    </div>
  );
}

function ArgsTable({
  entries,
}: {
  entries: Array<{ key: string; value: string }>;
}) {
  return (
    <div className="max-h-56 overflow-auto rounded-md border border-border bg-muted/40">
      <table className="w-full table-fixed text-[11px]">
        <tbody>
          {entries.map((row, i) => (
            <tr
              key={i}
              className="border-b border-border/40 last:border-b-0 align-top"
            >
              <td className="w-1/4 min-w-[64px] break-all px-2 py-1 font-medium text-foreground/85">
                {row.key || "—"}
              </td>
              <td className="px-2 py-1 text-muted-foreground whitespace-pre-wrap break-words">
                {row.value}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ToolCallDetail({ call }: { call: ToolCallItem }) {
  const entries = buildArgsPreview(call.argumentsText);
  const tableable = entries.length > 0 && entries.some((e) => e.key !== "");
  const [argsRaw, setArgsRaw] = useState(false);
  const showRaw = argsRaw || !tableable;

  return (
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
          <div className="mb-1 flex items-center justify-between gap-2">
            <div className="text-[10px] font-medium uppercase text-muted-foreground">
              入参
            </div>
            {tableable && (
              <button
                type="button"
                onClick={() => setArgsRaw((v) => !v)}
                title={showRaw ? "切换为表格" : "切换为原文"}
                aria-label={showRaw ? "切换为表格" : "切换为原文"}
                className="inline-flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                {showRaw ? (
                  <TableIcon className="h-3 w-3" />
                ) : (
                  <Braces className="h-3 w-3" />
                )}
              </button>
            )}
          </div>
          {showRaw ? (
            <pre className="max-h-56 overflow-auto rounded-md bg-muted/70 p-2 text-[11px] leading-relaxed text-foreground whitespace-pre-wrap break-words">
              {call.argumentsText || "等待入参…"}
            </pre>
          ) : (
            <ArgsTable entries={entries} />
          )}
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
  );
}

function ToolCallStrip({
  calls,
  expandedKeys,
  onToggle,
}: {
  calls: ToolCallItem[];
  expandedKeys: Set<string>;
  onToggle: (key: string) => void;
}) {
  if (calls.length === 0) return null;

  return (
    <div className="mt-3 space-y-2">
      {calls.map((call) => {
        const active = expandedKeys.has(call.key);
        const label = call.name || "工具调用";
        // 按钮右侧只展示 prompt 字段（约定：tool_call 普遍带 `prompt`），
        // 其它参数留给用户点开后自己查看。
        const promptEntry = buildArgsPreview(call.argumentsText).find(
          (e) => e.key === "prompt"
        );
        return (
          <div key={call.key} className="space-y-1.5">
            <div className="flex w-full items-center gap-2">
              <button
                type="button"
                onClick={() => onToggle(call.key)}
                className={cn(
                  "inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md border px-2 text-[11px] transition-colors",
                  active
                    ? "border-primary/40 bg-primary/10 text-primary"
                    : "border-border bg-background/80 text-muted-foreground hover:bg-accent"
                )}
                title={label}
              >
                <Wrench className="h-3.5 w-3.5 shrink-0" />
                <span className="max-w-[180px] truncate font-medium">
                  {label}
                </span>
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
              {promptEntry && (
                <span className="min-w-0 flex-1 truncate text-[11px] font-medium text-foreground">
                  {promptEntry.value}
                </span>
              )}
            </div>

            {active && <ToolCallDetail call={call} />}
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

function ReasoningBlock({
  text,
  streaming,
}: {
  text: string;
  streaming: boolean;
}) {
  // 流式时默认展开、写完默认折叠
  const [open, setOpen] = useState(streaming);
  useEffect(() => {
    if (streaming) setOpen(true);
  }, [streaming]);

  const trimmed = text.trim();
  if (!trimmed && !streaming) return null;

  return (
    <div className="space-y-1">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="inline-flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground"
      >
        <Brain className="h-3.5 w-3.5 shrink-0" />
        <span className="font-medium">
          {streaming ? "思考中…" : "思考过程"}
        </span>
        {streaming && (
          <span className="ml-1 inline-flex h-1.5 w-1.5 rounded-full bg-primary animate-pulse" />
        )}
        {open ? (
          <ChevronDown className="h-3 w-3" />
        ) : (
          <ChevronRight className="h-3 w-3" />
        )}
      </button>
      {open && (
        <div className="border-l border-border/50 pl-3 text-[12px] leading-relaxed text-muted-foreground break-words">
          {text ? (
            <div className="markdown-segment">
              <ReactMarkdown
                remarkPlugins={[remarkGfm]}
                components={markdownComponents}
              >
                {text}
              </ReactMarkdown>
            </div>
          ) : streaming ? (
            "▍"
          ) : null}
        </div>
      )}
    </div>
  );
}

function AssistantParts({
  parts,
  streaming,
  expandedKeys,
  onToggle,
}: {
  parts: AssistantRenderPart[];
  streaming?: boolean;
  expandedKeys: Set<string>;
  onToggle: (key: string) => void;
}) {
  if (parts.length === 0) {
    return streaming ? <span>▍</span> : null;
  }

  return (
    <div className="space-y-3">
      {parts.map((part) => {
        if (part.type === "text") {
          return (
            <div key={part.key} className="markdown-segment">
              <ReactMarkdown
                remarkPlugins={[remarkGfm]}
                components={markdownComponents}
              >
                {part.text || (streaming ? "▍" : "")}
              </ReactMarkdown>
            </div>
          );
        }
        if (part.type === "reasoning") {
          return (
            <ReasoningBlock
              key={part.key}
              text={part.text}
              streaming={part.streaming}
            />
          );
        }
        return (
          <ToolCallStrip
            key={part.key}
            calls={part.calls}
            expandedKeys={expandedKeys}
            onToggle={onToggle}
          />
        );
      })}
    </div>
  );
}

export const MessageBubble = memo(function MessageBubble({
  message,
  streaming,
  prompt,
  userAvatar,
  session,
  onFork,
  onRegenerate,
  onEdit,
  streamingParts,
  find,
  archived,
  summaryExpanded,
  onToggleSummary,
  historyExpanded,
  onToggleHistory,
  archivedCount,
}: Props) {
  const [copied, setCopied] = useState(false);
  const [expandedToolCalls, setExpandedToolCalls] = useState<Set<string>>(
    () => new Set()
  );
  const [showRawText, setShowRawText] = useState(false);
  const [showRawJson, setShowRawJson] = useState(false);
  // 后端 preview payload(含 workspace XML / tools / skills)。
  // 打开 JSON 视图时按需拉取,失败时回退到前端 buildModelMessages。
  const [rawJsonPayload, setRawJsonPayload] = useState<unknown>(null);
  const [rawJsonLoading, setRawJsonLoading] = useState(false);
  const [rawJsonError, setRawJsonError] = useState<string | null>(null);
  const [actionMenuOpen, setActionMenuOpen] = useState(false);
  const actionMenuRef = useRef<HTMLDivElement>(null);
  const [editing, setEditing] = useState(false);
  const [editDraft, setEditDraft] = useState("");
  const [submittingEdit, setSubmittingEdit] = useState(false);
  const editTextareaRef = useRef<HTMLTextAreaElement>(null);

  // 编辑态打开时自动 focus + 末尾光标
  useEffect(() => {
    if (!editing) return;
    const ta = editTextareaRef.current;
    if (!ta) return;
    ta.focus();
    const len = ta.value.length;
    ta.setSelectionRange(len, len);
  }, [editing]);

  // 打开 JSON 视图时按需拉取后端 preview。
  // streaming 消息或没有 session(理论上不会发生)走前端 fallback。
  useEffect(() => {
    if (!showRawJson) return;
    const sessionId = session?.id;
    if (!sessionId || streaming) return;
    let cancelled = false;
    setRawJsonLoading(true);
    setRawJsonError(null);
    api
      .previewSessionPayload(sessionId, message.id)
      .then((value) => {
        if (cancelled) return;
        setRawJsonPayload(value);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setRawJsonError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setRawJsonLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [showRawJson, session?.id, message.id, streaming]);

  function startEdit() {
    setEditDraft(message.content);
    setEditing(true);
  }

  function cancelEdit() {
    setEditing(false);
    setEditDraft("");
  }

  async function commitEdit() {
    if (!onEdit) return;
    const next = editDraft.trim();
    if (!next || submittingEdit) return;
    if (next === message.content.trim()) {
      cancelEdit();
      return;
    }
    setSubmittingEdit(true);
    try {
      await onEdit(message.id, next);
      // 提交成功后该消息会被 truncate 并重发，组件会被卸载，无需手动收尾
    } catch {
      // 失败时让用户继续编辑：保留草稿
      setSubmittingEdit(false);
    }
  }

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
            用户中断对话
          </span>
        </div>
        <div className="flex-1 h-px bg-border" />
      </div>
    );
  }

  if (message.role === "marker" && message.meta?.type === "reasoning_switch") {
    const { from, to } = message.meta;
    return (
      <div className="px-6 py-3 flex items-center gap-3 text-[11px] text-muted-foreground select-none">
        <div className="flex-1 h-px bg-border" />
        <div className="inline-flex items-center gap-1.5 rounded-full border border-border bg-background px-2.5 py-1">
          <ArrowRightLeft className="w-3 h-3" />
          <span className="font-medium text-foreground/70">
            {formatReasoningLabel(from)}
          </span>
          <span>→</span>
          <span className="font-medium text-primary">
            {formatReasoningLabel(to)}
          </span>
        </div>
        <div className="flex-1 h-px bg-border" />
      </div>
    );
  }

  if (message.role === "marker" && message.meta?.type === "compact_boundary") {
    const { before_tokens, after_tokens, summary } = message.meta;
    const summaryOn = !!summaryExpanded;
    const historyOn = !!historyExpanded;
    const count = archivedCount ?? 0;
    return (
      <div className="px-6 py-3 flex flex-col items-stretch gap-2 text-[11px] text-muted-foreground select-none">
        <div className="flex items-center gap-3">
          <div className="flex-1 h-px bg-border" />
          <button
            type="button"
            onClick={onToggleSummary}
            title={summaryOn ? "点击折叠压缩摘要" : "点击查看压缩摘要"}
            className="inline-flex items-center gap-1.5 rounded-full border border-border bg-background px-2.5 py-1 transition-colors hover:bg-muted hover:text-foreground cursor-pointer"
          >
            {summaryOn ? (
              <ChevronDown className="w-3 h-3" />
            ) : (
              <ChevronRight className="w-3 h-3" />
            )}
            <span className="font-medium text-foreground/70">上下文已压缩</span>
            <span className="tabular-nums">
              {before_tokens} → {after_tokens} tokens
            </span>
          </button>
          {count > 0 && (
            <button
              type="button"
              onClick={onToggleHistory}
              title={
                historyOn
                  ? "点击折叠原始历史对话"
                  : `点击展开 ${count} 条原始历史对话`
              }
              className="inline-flex items-center gap-1 rounded-full border border-border bg-background px-2 py-1 transition-colors hover:bg-muted hover:text-foreground cursor-pointer"
            >
              {historyOn ? (
                <ChevronDown className="w-3 h-3" />
              ) : (
                <ChevronRight className="w-3 h-3" />
              )}
              <span>历史对话 · {count}</span>
            </button>
          )}
          <div className="flex-1 h-px bg-border" />
        </div>
        {summaryOn && summary && (
          <div className="mx-auto max-w-3xl w-full rounded-md border border-border bg-muted/30 px-3 py-2 text-[12px] leading-relaxed text-foreground whitespace-pre-wrap">
            {summary}
          </div>
        )}
      </div>
    );
  }

  const isUser = message.role === "user";
  const assistantParts = buildAssistantRenderParts(
    message,
    streamingParts,
    streaming
  );
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
  } else if (showRawJson) {
    const fallback = session ? buildModelMessages(session, message.id) : message;
    body = (
      <RawJsonPanel
        loading={rawJsonLoading}
        error={rawJsonError}
        payload={rawJsonPayload}
        fallback={fallback}
      />
    );
  } else if (showRawText && canToggleRawText) {
    body = (
      <div className="whitespace-pre-wrap break-words rounded-md border border-border bg-muted/40 px-3 py-2 font-mono text-[13px] leading-relaxed text-foreground">
        {rawText}
      </div>
    );
  } else if (isUser && editing) {
    body = (
      <div className="space-y-2">
        <textarea
          ref={editTextareaRef}
          value={editDraft}
          onChange={(e) => setEditDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              cancelEdit();
            } else if (
              (e.metaKey || e.ctrlKey) &&
              e.key === "Enter" &&
              !submittingEdit
            ) {
              e.preventDefault();
              void commitEdit();
            }
          }}
          rows={Math.min(12, Math.max(2, editDraft.split("\n").length))}
          disabled={submittingEdit}
          className="w-full resize-y rounded-md border border-border bg-background px-3 py-2 text-[14px] leading-relaxed text-foreground outline-none focus:border-primary disabled:opacity-60"
          placeholder="编辑消息…"
        />
        <div className="flex items-center justify-end gap-2 text-xs">
          <span className="mr-auto text-[11px] text-muted-foreground">
            ⌘/Ctrl + Enter 保存并重跑 · Esc 取消
          </span>
          <button
            type="button"
            onClick={cancelEdit}
            disabled={submittingEdit}
            className="inline-flex h-7 items-center gap-1 rounded-md border border-border px-2 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-60"
          >
            <X className="h-3.5 w-3.5" />
            取消
          </button>
          <button
            type="button"
            onClick={() => void commitEdit()}
            disabled={
              submittingEdit ||
              !editDraft.trim() ||
              editDraft.trim() === message.content.trim()
            }
            className="inline-flex h-7 items-center gap-1 rounded-md bg-primary px-2 text-primary-foreground hover:bg-primary/90 disabled:opacity-60"
          >
            {submittingEdit ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Check className="h-3.5 w-3.5" />
            )}
            保存并重跑
          </button>
        </div>
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
        expandedKeys={expandedToolCalls}
        onToggle={(key) =>
          setExpandedToolCalls((current) => {
            const next = new Set(current);
            if (next.has(key)) next.delete(key);
            else next.add(key);
            return next;
          })
        }
      />
    );
  }

  return (
    <div
      title={archived ? "已被压缩，模型不再读取此消息（点击右上角圆环可再次压缩）" : undefined}
      className={cn(
        "group relative flex gap-3 px-6 py-4",
        isUser ? "bg-background" : "bg-accent/30",
        archived && "opacity-50 hover:opacity-100 transition-opacity"
      )}
    >
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
            className="absolute right-0 mt-1 w-40 rounded-md border border-border bg-card py-1 text-xs shadow-lg"
          >
            {canToggleRawText && (
              <button
                type="button"
                onClick={() => {
                  setShowRawText((show) => !show);
                  setShowRawJson(false);
                  setActionMenuOpen(false);
                }}
                className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                <FileText className="h-3.5 w-3.5" />
                <span>{showRawText ? "显示渲染" : "显示原文"}</span>
              </button>
            )}
            <button
              type="button"
              onClick={() => {
                setShowRawJson((show) => !show);
                setShowRawText(false);
                setActionMenuOpen(false);
              }}
              className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-muted-foreground hover:bg-accent hover:text-foreground"
            >
              <FileJson className="h-3.5 w-3.5" />
              <span>{showRawJson ? "显示渲染" : "显示原始 JSON"}</span>
            </button>
          </div>
        )}
      </div>
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
        {!streaming && !editing && (
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
            {isUser && onEdit && (
              <button
                onClick={startEdit}
                className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-xs"
                title="编辑后重跑"
              >
                <Pencil className="w-3.5 h-3.5" />
                <span>编辑</span>
              </button>
            )}
            {onRegenerate && (
              <button
                onClick={() => onRegenerate(message.id)}
                className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-xs"
                title={isUser ? "用同样内容重跑" : "重新生成"}
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
