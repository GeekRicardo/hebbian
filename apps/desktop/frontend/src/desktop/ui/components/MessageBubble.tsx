import { createContext, memo, useCallback, useContext, useEffect, useRef, useState } from "react";
import { isTauri } from "@/desktop/bridge/transport";
import { openUrl as openExternalUrl } from "@tauri-apps/plugin-opener";
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
  Brain,
  Pencil,
  Undo2,
  X,
  Trash2,
  Terminal,
  SquareTerminal,
  CircleStop,
  ScrollText,
  Edit3,
  Search,
  Sparkles,
  MessageSquare,
  Globe2,
  Image as ImageIcon,
  Boxes,
  Maximize2,
  Minimize2,
  ClipboardCheck,
  Paperclip,
  BookOpen,
  NotebookPen,
  Square,
} from "lucide-react";
import type {
  Message,
  MessagePart,
  Prompt,
  Session,
  StreamingAssistantPart,
  ToolCallStatus,
  AppSettings,
  QuestionOption,
  AskQuestion,
  MemoryWriteItem,
} from "@/desktop/ui/types";

// 稳定空数组引用：zustand selector 用浅比较，每次返回新 `[]` 会触发无限重渲染。
const EMPTY_STR_ARR: string[] = [];
import { cn, formatTime } from "@/desktop/ui/lib/utils";
import { ansiToHtml } from "@/desktop/ui/lib/ansiToHtml";
import { extractBgTaskId } from "@/desktop/ui/lib/bgTaskId";
import { FOCUS_TOOL_CALL_EVENT } from "@/desktop/ui/lib/focusToolCall";
import { toast } from "sonner";
import { animations } from "@/assets/animations";
import { LoopingWebm } from "@/desktop/ui/components/LoopingWebm";
import { CodeBlock } from "@/desktop/ui/components/CodeBlock";
import { AttachmentPreviewStrip } from "@/desktop/ui/components/AttachmentPreviewStrip";
import { MemoryWriteSummary } from "@/desktop/ui/components/MemoryWriteSummary";
import { AvatarPreview } from "@/desktop/ui/components/AvatarField";
import {
  DiffViewer,
  FullscreenPortal,
  type DiffMode,
} from "@/desktop/ui/components/DiffPanel";
import {
  diffSidesFromArgs,
  inferDiffAction,
  parsePartialEditArgs,
} from "@/desktop/ui/lib/parsePartialEditArgs";
import {
  lineOfOldString,
  useOriginalFileText,
} from "@/desktop/ui/lib/useDiffBaseLine";
import { findMatches, highlight } from "./FindBar";
import {
  canShowRawMessage,
  getMessageRawText,
} from "@/desktop/ui/lib/messageRawText";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";

interface Props {
  message: Message;
  streaming?: boolean;
  prompt?: Prompt;
  appSettings?: AppSettings;
  userAvatar?: string;
  reserveBottomForQuestionPopup?: boolean;
  /** 当前 session id，用于后台任务 kill 操作 */
  sessionId?: string;
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
  /**
   * 删除尾部消息（点三点菜单「删除」）。assistant → 删本轮回复全部输出；
   * user → 仅尾部无回复时删这一条。父层只在可删的消息上传入。
   */
  onDelete?: (id: string, role: string) => void;
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
  /** 仅 compact_boundary：点击分隔条主体切换摘要展示。参数为本 boundary 消息 id。 */
  onToggleSummary?: (messageId: string) => void;
  /** 仅 compact_boundary：原始历史是否展开（点击「历史对话」按钮切换） */
  historyExpanded?: boolean;
  /** 仅 compact_boundary：点击「历史对话」按钮切换。参数为本 boundary 消息 id。 */
  onToggleHistory?: (messageId: string) => void;
  /** 仅 compact_boundary：该 boundary 折叠了多少条原始历史消息 */
  archivedCount?: number;
  /** 仅 compact_boundary：是否可撤销（这条 marker 是最后一条消息，压缩后还没产生新对话）。 */
  canUndoCompaction?: boolean;
  /** 仅 compact_boundary：点击「撤销压缩」。参数为本 boundary 消息 id。 */
  onUndoCompaction?: (messageId: string) => void;
  /** 本轮后台抽取写入的记忆（架构 §4.14）。由 MessageList 把紧跟其后的 memory_writes
   *  marker"提"进所属 assistant 气泡，渲染在正文下方、操作行上方。 */
  memoryWrites?: MemoryWriteItem[];
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
  /** 这次调用以失败收场（执行错误 / 入参解析失败 / 被拒 / Bash 退出码非 0）：状态点标红。 */
  isError?: boolean;
  /** 工具输出超阈值时落盘的工件路径（架构 §4.4.9） */
  artifactPath?: string | null;
  /**
   * 工具执行中的流式输出累积（架构 §4.4.1）。Bash 前台等待期间的
   * stdout/stderr 增量；status=running 时渲染为实时 console，status=done
   * 后由 result 取代显示。
   */
  liveOutput?: string | null;
  /**
   * Task 工具的嵌套子事件（架构 §4.4.11.8 / P7）。
   * 子 agent 事件经 store 路由后存在 StreamingAssistantPart.nested_parts，
   * 这里透传给渲染层，在 Task 卡片内嵌套显示子工具调用 / 子文本 / 子推理。
   */
  nestedParts?: StreamingAssistantPart[];
  /** Task 工具调用的子 agent 名（subagent_type）；卡片标题展示它而非 "Task"（架构 §4.4.11.8）。 */
  subagentType?: string;
  /** AutoMode judge 正在评估这次调用（架构 §4.4.4）：卡片渲染黄色呼吸。 */
  isJudging?: boolean;
}

type AssistantRenderPart =
  | { type: "text"; key: string; text: string }
  | {
      type: "reasoning";
      key: string;
      text: string;
      streaming: boolean;
      durationMs?: number | null;
    }
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

/** 持久化的 nested（MessagePart[]）转成渲染用的 StreamingAssistantPart[]（架构 §4.4.11.8）。 */
function savedNestedToStreaming(
  nested?: MessagePart[] | null
): StreamingAssistantPart[] | undefined {
  if (!nested || nested.length === 0) return undefined;
  return nested.map((p, i): StreamingAssistantPart => {
    if (p.type === "text") return { type: "text", text: p.text };
    if (p.type === "reasoning")
      return { type: "reasoning", text: p.text, duration_ms: p.duration_ms };
    return {
      type: "tool_call",
      index: i,
      id: p.id,
      name: p.name,
      arguments: p.arguments ?? "",
      input: p.input,
      result: p.result,
      duration_ms: p.duration_ms,
      status: p.result ? "done" : "running",
      is_error: p.is_error,
      artifact_path: p.artifact_path,
    };
  });
}

/** Task 工具调用的子 agent 名（subagent_type）；卡片标题展示它而非通用 "Task"。 */
function extractSubagentType(input: unknown): string | undefined {
  if (input && typeof input === "object" && "subagent_type" in input) {
    const v = (input as { subagent_type?: unknown }).subagent_type;
    if (typeof v === "string" && v) return v;
  }
  return undefined;
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
    isError: part.is_error,
    artifactPath: part.artifact_path,
    liveOutput: part.live_output,
    nestedParts: part.nested_parts,
    subagentType: extractSubagentType(part.input),
    isJudging: part.isJudging,
  };
}

function normalizeSavedToolPart(
  part: Extract<MessagePart, { type: "tool_call" }>,
  index: number,
  nestedByCallId?: Map<string, StreamingAssistantPart[]>
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
    isError: part.is_error,
    artifactPath: part.artifact_path,
    // 子过程在 MessageToolCall.nested（落 message.tool_calls），按 id 关联回这条 part。
    nestedParts: part.id ? nestedByCallId?.get(part.id) : undefined,
    subagentType: extractSubagentType(part.input),
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
    isError: call.is_error,
    nestedParts: savedNestedToStreaming(call.nested),
    subagentType: extractSubagentType(call.input),
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
          durationMs: part.duration_ms,
        });
      } else {
        pendingTools.push(normalizeStreamingToolPart(part, index));
      }
    });
    pushToolGroup(out, pendingTools);
    return out;
  }

  if (message.parts?.length) {
    // 子过程持久化在 message.tool_calls[].nested（架构 §4.4.11.8）；按 call id 关联回 parts 里的 tool_call。
    const nestedByCallId = new Map<string, StreamingAssistantPart[]>();
    for (const c of message.tool_calls ?? []) {
      const ns = savedNestedToStreaming(c.nested);
      if (c.id && ns) nestedByCallId.set(c.id, ns);
    }
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
          durationMs: part.duration_ms,
        });
      } else {
        pendingTools.push(normalizeSavedToolPart(part, index, nestedByCallId));
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

export function formatCompactDuration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds % 60);
  return rest > 0 ? `${minutes}m ${rest}s` : `${minutes}m`;
}

function statusLabel(status: ToolCallItem["status"]) {
  if (status === "done") return "完成";
  if (status === "running") return "执行中";
  return "生成参数";
}

async function openSystemBrowser(url: string) {
  try {
    await openExternalUrl(url);
    return;
  } catch (error) {
    if (isTauri()) {
      throw error;
    }
  }
  window.open(url, "_blank", "noopener,noreferrer");
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


function parseArgsObject(argumentsText: string): Record<string, unknown> {
  const trimmed = argumentsText.trim();
  if (!trimmed) return {};
  try {
    const parsed = JSON.parse(trimmed);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
  } catch {
    // fallthrough
  }
  const entries = buildArgsPreview(argumentsText);
  return Object.fromEntries(entries.filter((e) => e.key).map((e) => [e.key, e.value]));
}

function argString(args: Record<string, unknown>, key: string): string {
  const value = args[key];
  if (value === undefined || value === null) return "";
  return previewArgValue(value);
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}

function callArgs(call: ToolCallItem): Record<string, unknown> {
  return parseArgsObject(call.argumentsText);
}

function callSummary(call: ToolCallItem): string {
  const args = callArgs(call);
  const name = call.name || "工具调用";
  if (name === "Bash") {
    return argString(args, "command") || "运行命令";
  }
  if (name === "PowerShell") {
    return argString(args, "command") || "运行命令";
  }
  if (name === "BashOutput") {
    return argString(args, "task_id") || "读取后台命令输出";
  }
  if (name === "KillShell") {
    return argString(args, "task_id") || "停止后台命令";
  }
  if (name === "Read") {
    const file = argString(args, "file_path");
    const offset = argString(args, "offset");
    return file ? `${basename(file)}${offset ? `:${offset}` : ""}` : "读取文件";
  }
  if (name === "Write" || name === "Edit") {
    const file = argString(args, "file_path");
    return file ? basename(file) : name === "Edit" ? "编辑文件" : "写入文件";
  }
  if (name === "ReadMemory") {
    return argString(args, "id") || "读取记忆";
  }
  if (name === "WriteMemory") {
    return argString(args, "summary") || argString(args, "key") || "记下一条";
  }
  if (name === "Grep") {
    return argString(args, "pattern") || "搜索代码";
  }
  if (name === "Glob") {
    return argString(args, "pattern") || "匹配文件";
  }
  if (name === "Skill") {
    return argString(args, "name") || argString(args, "skill") || "读取技能";
  }
  if (name === "Ask") {
    return argString(args, "question") || "用户提问记录";
  }
  if (name === "WebSearch") {
    return argString(args, "query") || "网络搜索";
  }
  if (name === "Fetch") {
    return argString(args, "url") || "抓取网页内容";
  }
  if (name === "image_generation") {
    return argString(args, "prompt") || "生成图片";
  }
  if (isTaskListTool(name)) {
    const todos = parseTodos(call.argumentsText);
    if (todos.length === 0) return "任务列表";
    const done = todos.filter((t) => t.status === "completed").length;
    const active = todos.filter((t) => t.status === "in_progress").length;
    const segs = [`${todos.length} 项`, `${done} 完成`];
    if (active > 0) segs.push(`${active} 进行中`);
    return segs.join(" · ");
  }
  if (name === "ExitPlanMode") {
    return argString(args, "plan_markdown") || "提交计划";
  }
  return (
    argString(args, "prompt") ||
    argString(args, "query") ||
    argString(args, "file_path") ||
    "自定义工具调用"
  );
}

function defaultActionLabel(name: string): string {
  if (name === "Bash") return "运行命令";
  if (name === "PowerShell") return "运行命令";
  if (name === "BashOutput") return "读取后台命令输出";
  if (name === "KillShell") return "停止后台命令";
  if (name === "Read") return "读取文件";
  if (name === "ReadMemory") return "读取记忆";
  if (name === "WriteMemory") return "记下";
  if (name === "Write") return "写入文件";
  if (name === "Edit") return "编辑文件";
  if (name === "Grep") return "搜索代码";
  if (name === "Glob") return "匹配文件";
  if (name === "Skill") return "读取技能说明";
  if (name === "Ask") return "用户提问记录";
  if (name === "WebSearch") return "网络搜索";
  if (name === "Fetch") return "抓取网页内容";
  if (name === "image_generation") return "生成图片";
  if (isTaskListTool(name)) return "任务列表";
  if (name === "ExitPlanMode") return "提交计划";
  return "自定义工具 fallback";
}

function callDescription(call: ToolCallItem): string {
  const name = call.name || "工具调用";
  // 模型若在入参里写了 description（如 Bash 推荐的简短意图说明），优先展示，
  // 它通常比通用动词更具体。fallback 才回到 "运行命令" 这一档。
  const args = callArgs(call);
  const userDesc = argString(args, "description");
  if (userDesc) return userDesc;
  return defaultActionLabel(name);
}

/**
 * Read 工具显示路径的相对化：
 * - 在 workdir 内 → 相对 workdir 的路径（去掉公共前缀）
 * - 在某个 allowed_path 内 → `{该 allowed_path 的 basename}/相对路径`
 *   例：allowed = `/Users/ricardo/code/xxx1`、绝对 = `/Users/ricardo/code/xxx1/a/b.ts`
 *       → `xxx1/a/b.ts`
 * - 否则 → 原始绝对路径
 *
 * 用 `startsWith(p + "/")` 而不是 `startsWith(p)`，避免 `/foo/bar` 被 `/foo/ba` 命中。
 */
function relativizeReadPath(
  absolute: string,
  workdir: string | null | undefined,
  allowedPaths: string[],
): string {
  if (!absolute) return absolute;
  const trim = (s: string) => s.replace(/\/+$/, "");
  if (workdir) {
    const w = trim(workdir);
    if (absolute === w) return ".";
    if (absolute.startsWith(w + "/")) return absolute.slice(w.length + 1);
  }
  for (const raw of allowedPaths) {
    const p = trim(raw);
    if (!p) continue;
    const base = p.slice(p.lastIndexOf("/") + 1) || p;
    if (absolute === p) return base;
    if (absolute.startsWith(p + "/")) {
      return `${base}/${absolute.slice(p.length + 1)}`;
    }
  }
  return absolute;
}

function ToolIcon({ name }: { name?: string | null }) {
  const cls = "h-3.5 w-3.5";
  if (name === "Bash" || name === "PowerShell") return <Terminal className={cls} />;
  if (name === "BashOutput") return <SquareTerminal className={cls} />;
  if (name === "KillShell") return <CircleStop className={cls} />;
  if (name === "Read") return <ScrollText className={cls} />;
  if (name === "ReadMemory") return <BookOpen className={cls} />;
  if (name === "WriteMemory") return <NotebookPen className={cls} />;
  if (name === "Write" || name === "Edit") return <Edit3 className={cls} />;
  if (name === "Grep" || name === "Glob") return <Search className={cls} />;
  if (name === "Skill") return <Sparkles className={cls} />;
  if (name === "Ask") return <MessageSquare className={cls} />;
  if (name === "WebSearch" || name === "Fetch") return <Globe2 className={cls} />;
  if (name === "image_generation") return <ImageIcon className={cls} />;
  if (isTaskListTool(name)) {
    return <span className="h-3 w-3 rounded-[2px] border border-current" />;
  }
  if (name === "ExitPlanMode") return <ClipboardCheck className={cls} />;
  return <Boxes className={cls} />;
}

export function isTaskListTool(name?: string | null): boolean {
  return name === "TaskList" || name === "Task" || name === "TodoWrite";
}

export type TodoStatus = "pending" | "in_progress" | "completed";
export interface TodoItem {
  /** 稳定 id。Rust 端 normalize 时根据 content+activeForm 算 FNV hash，模型可覆盖。
   *  sidebar 用 id 重叠判定"是不是同一份 todo 列表的更新（同 block）"。 */
  id?: string;
  content: string;
  status: TodoStatus;
  activeForm?: string;
}

export function parseTodos(argumentsText: string): TodoItem[] {
  const trimmed = (argumentsText || "").trim();
  if (!trimmed) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return [];
  }
  const arr = Array.isArray((parsed as { todos?: unknown })?.todos)
    ? (parsed as { todos: unknown[] }).todos
    : Array.isArray(parsed)
      ? (parsed as unknown[])
      : [];
  return arr
    .map((raw): TodoItem | null => {
      if (typeof raw === "string") {
        return { content: raw, status: "pending" };
      }
      if (raw && typeof raw === "object") {
        const t = raw as Record<string, unknown>;
        const content = String(t.content ?? t.text ?? t.title ?? "");
        if (!content) return null;
        const rawStatus = String(t.status ?? "pending");
        const status: TodoStatus =
          rawStatus === "completed" || rawStatus === "in_progress" || rawStatus === "pending"
            ? rawStatus
            : "pending";
        const activeForm = typeof t.activeForm === "string" ? t.activeForm : undefined;
        const id = typeof t.id === "string" ? t.id : undefined;
        return { id, content, status, activeForm };
      }
      return null;
    })
    .filter((t): t is TodoItem => t !== null);
}

function TodoChecklist({ todos }: { todos: TodoItem[] }) {
  if (todos.length === 0) {
    return (
      <div className="px-2 py-3 text-center text-[11px] text-muted-foreground">
        空任务列表
      </div>
    );
  }
  return (
    <div className="grid gap-1 p-1.5">
      {todos.map((todo, i) => {
        const done = todo.status === "completed";
        const active = todo.status === "in_progress";
        return (
          <div
            key={i}
            className={cn(
              "grid grid-cols-[16px_minmax(0,1fr)] items-start gap-2 rounded-md px-1.5 py-1 text-[13px]",
              active && "bg-muted/60"
            )}
          >
            <span
              className={cn(
                "mt-[3px] grid h-3.5 w-3.5 place-items-center rounded-[3px] border",
                done
                  ? "border-muted-foreground/70 bg-muted-foreground/70 text-background"
                  : active
                    ? "border-foreground/60"
                    : "border-border bg-background"
              )}
            >
              {done ? (
                <Check className="h-2.5 w-2.5" strokeWidth={3} />
              ) : active ? (
                <span className="h-1.5 w-1.5 rounded-[1px] bg-foreground/60" />
              ) : null}
            </span>
            <span className="min-w-0">
              <span
                className={cn(
                  "block break-words text-foreground",
                  done && "line-through text-muted-foreground"
                )}
              >
                {active && todo.activeForm ? todo.activeForm : todo.content}
              </span>
            </span>
          </div>
        );
      })}
    </div>
  );
}

// FloatingTaskPanel / extractLatestTodoSnapshot 已下线（2026-05-26）：
// todo 列表只在右侧工作台「任务清单」tab 展示，避免与 sidebar 重复 + 浮在 chat 上挡正文。
// 工具卡片 body 里的 TodoChecklist + parseTodos 保留——chat 流里那张 TodoWrite 工具卡片
// 还要展示该次调用的入参快照。

/**
 * 工件徽标（架构 §4.4.9）：工具输出超阈值时落盘后，让用户一眼看到
 * 「完整输出在 path」并提供原生「复制路径」操作。模型已经从 result 文本
 * 拿到指针；这里只是给人看的。
 */
function ArtifactBadge({ path }: { path: string }) {
  return (
    <div className="mt-1 flex items-center gap-1.5 rounded-md border border-dashed border-border bg-muted/30 px-2 py-1 text-[11px] text-muted-foreground">
      <Paperclip className="h-3 w-3 shrink-0" />
      <span className="shrink-0">完整输出落盘到</span>
      <code
        className="min-w-0 flex-1 truncate font-mono text-foreground/80"
        title={path}
      >
        {path}
      </code>
      <button
        type="button"
        onClick={() => {
          void navigator.clipboard?.writeText(path);
        }}
        className="shrink-0 rounded px-1 py-0.5 text-[10px] text-muted-foreground hover:bg-muted hover:text-foreground"
        title="复制路径"
      >
        复制
      </button>
    </div>
  );
}

function WriteHeader({ call, label = "write" }: { call: ToolCallItem; label?: string }) {
  const args = callArgs(call);
  const path = argString(args, "file_path") || "file";
  const content = argString(args, "content");
  const lineCount = content ? content.split(/\r?\n/).length : 0;
  return (
    <div className="flex min-h-8 items-center gap-2 border-b border-border bg-muted/30 px-2 text-[13px] text-muted-foreground">
      <Edit3 className="h-3.5 w-3.5 shrink-0" />
      <span className="min-w-0 truncate font-mono">
        {path}:#{lineCount ? `${lineCount} lines` : label}
      </span>
    </div>
  );
}

function SkillHeader({ call }: { call: ToolCallItem }) {
  const args = callArgs(call);
  const name = argString(args, "skill") || argString(args, "name") || "skill";
  return (
    <div className="flex min-h-8 items-center gap-2 border-b border-border bg-muted/30 px-2 text-[13px] text-muted-foreground">
      <Sparkles className="h-3.5 w-3.5 shrink-0" />
      <span className="min-w-0 truncate font-mono">{name}</span>
    </div>
  );
}

function FetchHeader({ call }: { call: ToolCallItem }) {
  const args = callArgs(call);
  const url = argString(args, "url") || "url";
  const prompt = argString(args, "prompt");
  const open = (event: React.MouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();
    event.stopPropagation();
    void openSystemBrowser(url).catch(() => toast.error("打开链接失败"));
  };
  return (
    <div className="flex min-h-8 items-center gap-2 border-b border-border bg-muted/30 px-2 text-[13px] text-muted-foreground">
      <Globe2 className="h-3.5 w-3.5 shrink-0" />
      <a
        className="min-w-0 truncate text-primary hover:underline"
        href={url}
        onClick={open}
        target="_blank"
        rel="noreferrer"
      >
        {url}
      </a>
      {prompt && <span className="shrink-0 truncate">prompt: {prompt}</span>}
    </div>
  );
}

/** 嵌套在 ExpandButton modal 内的渲染组件用它判断是否解除自身高度限制。 */
const ToolDetailExpandedContext = createContext(false);

function ExpandButton({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open]);

  return (
    <>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen(true);
        }}
        title="放大查看完整内容"
        aria-label="放大查看完整内容"
        className="absolute right-2 top-2 z-10 inline-flex h-6 w-6 items-center justify-center rounded-md border border-border bg-background/90 text-muted-foreground shadow-sm hover:bg-background hover:text-foreground"
      >
        <Maximize2 className="h-3.5 w-3.5" />
      </button>
      {open && (
        <FullscreenPortal>
          {/* 半透明 backdrop：点击关闭、视觉提示 modal 性质 */}
          <div
            className="pointer-events-auto absolute inset-0 bg-foreground/30"
            onClick={() => setOpen(false)}
          />
          <div
            className="pointer-events-auto absolute inset-3 grid grid-rows-[auto_minmax(0,1fr)] overflow-hidden rounded-xl bg-background shadow-2xl"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex min-h-10 items-center justify-between gap-3 border-b border-border bg-muted/30 px-3">
              <strong className="min-w-0 truncate text-[13px]">{title}</strong>
              <button
                type="button"
                onClick={() => setOpen(false)}
                className="inline-flex h-7 w-7 items-center justify-center rounded-md border border-border bg-background text-muted-foreground hover:text-foreground"
                aria-label="关闭"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
            <div className="flex min-h-0 min-w-0 flex-col overflow-auto">
              <ToolDetailExpandedContext.Provider value={true}>
                {children}
              </ToolDetailExpandedContext.Provider>
            </div>
          </div>
        </FullscreenPortal>
      )}
    </>
  );
}

function ToolPre({ children, dark = false }: { children: string; dark?: boolean }) {
  const expanded = useContext(ToolDetailExpandedContext);
  return (
    <pre
      className={cn(
        "tool-pre m-0 overflow-auto rounded-none p-2 text-[13px] leading-relaxed whitespace-pre-wrap break-words",
        // 全屏放大态：铺满内容区（纵向 flex 撑满、左右对称内边距）；
        // 普通态：限高 max-h-48，右侧 pr-10 给右上角折叠按钮让位
        expanded ? "min-h-0 min-w-0 flex-1" : "max-h-48 pr-10",
        dark
          ? "bg-slate-950 text-slate-100 font-['JetBrains_Mono',ui-monospace,SFMono-Regular,Menlo,monospace]"
          : "text-foreground font-mono"
      )}
      dangerouslySetInnerHTML={{ __html: ansiToHtml(children) }}
    />
  );
}

function RenderedMarkdown({ text }: { text: string }) {
  const expanded = useContext(ToolDetailExpandedContext);
  return (
    <div
      className={cn(
        "overflow-auto px-3 py-2 text-[14px] leading-relaxed",
        !expanded && "max-h-48"
      )}
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
        {text}
      </ReactMarkdown>
    </div>
  );
}

function SearchResults({
  call,
  web,
  showPath = true,
}: {
  call: ToolCallItem;
  web?: boolean;
  showPath?: boolean;
}) {
  const expanded = useContext(ToolDetailExpandedContext);
  const rows = (call.result || "")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .slice(0, expanded ? 500 : 20);
  const args = callArgs(call);
  const query = argString(args, "query") || argString(args, "pattern");
  const path = !web && showPath ? argString(args, "path") : "";
  const fallback = rows.length ? rows : ["等待返回…"];
  return (
    <div className={cn("overflow-auto", !expanded && "max-h-48")}>
      {(query || path) && (
        <div className="border-b border-border/40 px-2 py-1 text-[13px] text-muted-foreground">
          {query && (
            <div>
              query: <span className="font-medium text-foreground">{query}</span>
            </div>
          )}
          {path && (
            <div>
              path: <span className="font-medium text-foreground">{path}</span>
            </div>
          )}
        </div>
      )}
      {fallback.map((line, i) => {
        const maybeUrl = line.match(/https?:\/\/\S+/)?.[0];
        const open = (event: React.MouseEvent<HTMLAnchorElement>) => {
          if (!maybeUrl) return;
          event.preventDefault();
          event.stopPropagation();
          void openSystemBrowser(maybeUrl).catch(() =>
            toast.error("打开链接失败")
          );
        };
        return (
          <div key={i} className="border-b border-border/40 bg-background px-2 py-1.5 text-[13px] last:border-b-0">
            {web && maybeUrl ? (
              <a
                href={maybeUrl}
                onClick={open}
                target="_blank"
                rel="noreferrer"
                className="text-primary hover:underline"
              >
                {line}
              </a>
            ) : (
              <span className="whitespace-pre-wrap break-words">{line}</span>
            )}
          </div>
        );
      })}
    </div>
  );
}

function DefaultToolDetail({ call }: { call: ToolCallItem }) {
  const expanded = useContext(ToolDetailExpandedContext);
  return (
    <div className="grid gap-2 md:grid-cols-2">
      <div className="min-w-0">
        <div className="mb-1 text-[12px] font-semibold uppercase tracking-wide text-muted-foreground">
          Input
        </div>
        <div
          className={cn(
            "overflow-auto bg-muted/30",
            !expanded && "max-h-48"
          )}
        >
          <table className="w-full table-fixed text-[13px]">
            <tbody>
              {buildArgsPreview(call.argumentsText).map((row, i) => (
                <tr key={i} className="border-b border-border/40 last:border-b-0 align-top">
                  <td className="w-1/3 break-all px-2 py-1 font-medium text-foreground/85">
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
      </div>
      <div className="min-w-0">
        <div className="mb-1 text-[12px] font-semibold uppercase tracking-wide text-muted-foreground">
          Output
        </div>
        <ToolPre>{call.result || "等待返回…"}</ToolPre>
      </div>
    </div>
  );
}

/**
 * Edit / Write 工具卡片的差异视图（架构 §4.13.9）。
 *
 * 数据源策略：
 * - **默认（非放大）**：永远只渲 args 的 old_string / new_string，不读 worktree——
 *   零网络等待，每次 ToolCallDelta 立刻刷新画面。
 * - **放大（GitHub review 风格）**：per-turn 完整净变化在右侧「修改文件」栏展示；
 *   消息内放大仍渲染本次 tool args 的局部 diff。
 *
 * 布局（inline / split）和"是否放大"两个状态独立——切换 inline↔split 不会关掉放大框。
 */
function EditDiffDetail({ call }: { call: ToolCallItem }) {
  const workdir = useStore((s) => s.currentSession?.workdir ?? null);
  const [viewMode, setViewMode] = useState<DiffMode>("split");
  const [expanded, setExpanded] = useState(false);

  const partial = parsePartialEditArgs(call.argumentsText);
  const argSides = diffSidesFromArgs(call.name, partial);
  const action = inferDiffAction(call.name, partial);
  const actionLabel =
    action === "create" ? "创建文件" : action === "overwrite" ? "覆盖文件" : "修改文件";

  const beforeText = argSides.beforeText;
  const afterText = argSides.afterText;
  const filePath = partial.file_path ?? "";

  // args 局部 diff 渲染时，读盘原文件给出 old_string 真实起始行号。
  const isCreateAction = action === "create" || call.name === "Write";
  const enableBaseLookup = !isCreateAction && !!partial.old_string;
  const originalText = useOriginalFileText(
    partial.file_path,
    workdir,
    enableBaseLookup,
  );
  const baseLine = enableBaseLookup && originalText
    ? lineOfOldString(originalText, partial.old_string ?? "")
    : 1;

  const streamingFlag = call.status === "streaming";
  const badge = call.status === "streaming" ? "实时预览" : undefined;

  const cycleMode = () => setViewMode((prev) => (prev === "split" ? "inline" : "split"));
  const toggleExpanded = () => setExpanded((e) => !e);

  // Esc 在放大态退出而不是关掉父展开
  useEffect(() => {
    if (!expanded) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setExpanded(false);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [expanded]);

  if (expanded) {
    return (
      <FullscreenPortal>
        <div
          className="pointer-events-auto absolute inset-0 bg-foreground/30"
          onClick={() => setExpanded(false)}
        />
        <div
          className="pointer-events-auto absolute inset-3 flex flex-col overflow-hidden border border-border bg-background shadow-2xl"
          onClick={(e) => e.stopPropagation()}
        >
          <DiffViewer
            beforeText={beforeText}
            afterText={afterText}
            filePath={filePath}
            actionLabel={actionLabel}
            badge={badge}
            mode={viewMode}
            onCycleMode={cycleMode}
            expanded={expanded}
            onToggleExpanded={toggleExpanded}
            streaming={streamingFlag}
            onClose={() => setExpanded(false)}
            className="min-h-0 flex-1"
            collapseContext={undefined}
            baseLineBefore={baseLine}
            baseLineAfter={baseLine}
          />
        </div>
      </FullscreenPortal>
    );
  }

  return (
    <DiffViewer
      beforeText={beforeText}
      afterText={afterText}
      filePath={filePath}
      actionLabel={actionLabel}
      badge={badge}
      mode={viewMode}
      onCycleMode={cycleMode}
      expanded={expanded}
      onToggleExpanded={toggleExpanded}
      streaming={streamingFlag}
      maxRows={14}
      baseLineBefore={baseLine}
      baseLineAfter={baseLine}
    />
  );
}

/**
 * Ask 工具回放：展示模型当时给出的候选选项（含 description），并把用户
 * 实际选中的选项高亮。选项数据来自 args（单题 options / 多题 questions[].options），
 * 选中态从 result 文本解析——result 形如「用户选择：X」「用户选择（多选）：A、B」
 * 「用户输入：X」，多题则每行「- 标题: 选择：X」。把所有出现的 label 收进 Set
 * 做按文本匹配的高亮；匹配不上不打勾，不影响展示。自由输入单独列出。
 */
function AskDetail({ call }: { call: ToolCallItem }) {
  const args = callArgs(call);
  const result = call.result || "";

  const sections: { title: string; description: string; options: QuestionOption[] }[] = [];
  const rawQuestions = args.questions;
  if (Array.isArray(rawQuestions) && rawQuestions.length > 0) {
    for (const q of rawQuestions as AskQuestion[]) {
      sections.push({
        title: q.title ?? "",
        description: q.description ?? "",
        options: Array.isArray(q.options) ? q.options : [],
      });
    }
  } else {
    sections.push({
      title: argString(args, "question") || "用户提问",
      description: "",
      options: Array.isArray(args.options) ? (args.options as QuestionOption[]) : [],
    });
  }

  // result 里所有「选择：/多选：/输入：」后面的文本都算用户的回答；多选用「、」再拆。
  const chosen = new Set<string>();
  for (const m of result.matchAll(/(?:选择|多选|输入)[：:]\s*(.+)/g)) {
    for (const part of m[1].split("、")) {
      const t = part.trim();
      if (t) chosen.add(t);
    }
  }

  return (
    <div className="space-y-3 bg-muted/30 p-2 text-[14px] text-muted-foreground">
      {sections.map((section, si) => (
        <div key={si} className="space-y-1.5">
          <div className="font-medium text-foreground">{section.title}</div>
          {section.description && (
            <div className="text-[13px] text-muted-foreground">{section.description}</div>
          )}
          <div className="space-y-1">
            {section.options.map((opt, oi) => {
              const picked = chosen.has(opt.label.trim());
              return (
                <div
                  key={oi}
                  className={cn(
                    "flex items-start gap-2 rounded-[5px] border px-2 py-1 text-[13px]",
                    picked
                      ? "border-primary/40 bg-primary/10 text-foreground"
                      : "border-border bg-background/40"
                  )}
                >
                  <span className="mt-[2px] grid h-3.5 w-3.5 shrink-0 place-items-center">
                    {picked && <Check className="h-3.5 w-3.5 text-primary" strokeWidth={3} />}
                  </span>
                  <span className="min-w-0">
                    <span className={cn("font-medium", picked && "text-foreground")}>
                      {opt.label}
                    </span>
                    {opt.description && (
                      <span className="ml-1.5 text-muted-foreground">{opt.description}</span>
                    )}
                  </span>
                </div>
              );
            })}
          </div>
        </div>
      ))}
      <ToolPre>{result}</ToolPre>
    </div>
  );
}

function ToolCallDetail({
  call,
  appSettings,
  sessionId,
}: {
  call: ToolCallItem;
  appSettings?: AppSettings;
  sessionId?: string;
}) {
  const name = call.name || "工具调用";
  const result = call.result || "等待返回…";
  const title = `${name} · ${callSummary(call)}`;

  // 提取 Bash 后台任务的 task_id（与 sidebar 共用一份正则，兼容当前 / 旧版文案）
  const taskIdFromResult = extractBgTaskId(call.result);

  // 对于前台 Bash，需要从注册表匹配 task_id
  const cmd = name === "Bash" || name === "PowerShell" ? argString(callArgs(call), "command") : "";
  const [matchedTaskId, setMatchedTaskId] = useState<string | null>(null);
  const [bgTaskState, setBgTaskState] = useState<string | null>(null);
  const [bgOutput, setBgOutput] = useState<string>("");
  const [killedLocally, setKilledLocally] = useState(false);

  // 前台 Bash 运行中时，轮询 listBackgroundTasks 按 command 匹配 task_id
  useEffect(() => {
    // 仅对运行中的前台 Bash 生效（真后台从 result 已能提取 task_id）
    if (!sessionId || killedLocally || taskIdFromResult || name !== "Bash" || call.status === "done" || !cmd) {
      return;
    }

    let cancelled = false;
    const poll = async () => {
      try {
        const report = await api.listBackgroundTasks(sessionId);
        if (cancelled) return;
        // 按 command 精确匹配找前台运行中的任务
        const match = report.shells.find(
          (s) => s.command === cmd && s.state === "running" && !s.is_background
        );
        if (match) {
          setMatchedTaskId(match.task_id);
          setBgTaskState(match.state);
        }
      } catch {
        // 静默失败
      }
    };

    poll();
    const interval = setInterval(poll, 2000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [sessionId, killedLocally, taskIdFromResult, name, call.status, cmd]);

  // 真后台任务：轮询状态 + 增量输出（与 sidebar TaskCard 同款带 cursor 累加）。
  // 后台 Bash 的 tool_call 一启动就返回 `[bash_NNN] 已在后台启动`、status 立刻变 done，
  // 真实输出只进 tail buffer 不写回 result——所以 chat 卡片必须自己 polling 才看得到。
  const bgCursorRef = useRef<number>(0);
  useEffect(() => {
    if (!taskIdFromResult || !sessionId || killedLocally) return;

    let cancelled = false;
    bgCursorRef.current = 0;
    setBgOutput("");
    const poll = async () => {
      try {
        const out = await api.readBackgroundTaskOutput(
          sessionId,
          taskIdFromResult,
          bgCursorRef.current
        );
        if (cancelled) return;
        setBgTaskState(out.state);
        if (out.chunk) setBgOutput((prev) => prev + out.chunk);
        bgCursorRef.current = out.total_bytes;
      } catch {
        // 静默失败
      }
    };
    poll();
    // 运行中 600ms 刷新（同 sidebar）；终态后由下方判断停轮询
    const interval = setInterval(poll, 600);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [taskIdFromResult, sessionId, killedLocally]);

  // 最终用于 kill 的 task_id
  const effectiveTaskId = taskIdFromResult || matchedTaskId;

  // Kill 处理函数
  const handleKill = async () => {
    if (!effectiveTaskId || !sessionId) return;
    try {
      await api.killBackgroundTask(sessionId, effectiveTaskId);
      setKilledLocally(true);
      setBgTaskState("killed");
      toast.success(`已终止任务 ${effectiveTaskId}`);
    } catch (err) {
      toast.error(`终止失败: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const isBgTaskRunning = bgTaskState === "running";
  const isRunning = call.status === "running";
  const canKill = effectiveTaskId && sessionId && !killedLocally && (isBgTaskRunning || (isRunning && !call.result));

  if (
    name === "Bash" ||
    name === "PowerShell" ||
    name === "BashOutput" ||
    name === "KillShell"
  ) {
    // status=running 且收到过 ToolOutputDelta：实时控制台展示，命令仍在跑。
    // status=done 后 result 已是聚合后的完整文本，覆盖掉 liveOutput。
    const live = call.liveOutput ?? "";
    // 真后台任务：tool_call 早已 done（result 只是启动提示），真实输出靠 bgOutput
    // polling 累加；终态后注册表 GC 会让增量取空，此时回落到 result（聚合文本）。
    const isBgTask = !!taskIdFromResult;
    const bgRunning = isBgTask && bgTaskState !== null && bgTaskState !== "exited";
    let stream: string;
    if (isBgTask) {
      stream = bgOutput || (bgRunning ? "等待输出…" : result);
    } else {
      stream = isRunning ? live || "等待输出…" : result;
    }

    // 如果用户本地 kill 了，追加提示
    if (killedLocally) {
      stream = stream + "\n\n[用户已结束进程]";
    }

    const showCursor =
      ((isRunning && live) || (bgRunning && bgOutput)) && !killedLocally;
    const body = cmd
      ? `$ ${cmd}\n\n${stream}${showCursor ? "\n▍" : ""}`
      : stream;

    return (
      <div className="relative">
        <ExpandButton title={title}>
          <ToolPre dark>{body}</ToolPre>
        </ExpandButton>
        <ToolPre dark>{body}</ToolPre>
        {/* Kill 按钮：仅当有 taskId 且任务正在运行时显示 */}
        {canKill && (
          <button
            onClick={handleKill}
            className="absolute top-2 right-2 flex items-center gap-1 rounded-md bg-red-500/10 px-2 py-1 text-xs text-red-500 hover:bg-red-500/20 transition-colors"
            title="终止任务"
          >
            <Square className="w-3 h-3 fill-current" />
            终止
          </button>
        )}
      </div>
    );
  }
  if (name === "Read") {
    return (
      <div className="relative">
        <ExpandButton title={title}>
          <ToolPre>{result}</ToolPre>
        </ExpandButton>
        <ToolPre>{result}</ToolPre>
      </div>
    );
  }
  if (name === "Skill") {
    return (
      <div className="relative">
        <ExpandButton title={title}>
          <ToolPre>{result}</ToolPre>
        </ExpandButton>
        <SkillHeader call={call} />
        <ToolPre>{result}</ToolPre>
      </div>
    );
  }
  if (name === "Write" || name === "Edit") {
    return <EditDiffDetail call={call} />;
  }
  if (name === "Grep" || name === "Glob") {
    const showSearchPath =
      name === "Grep" ? appSettings?.general.show_grep_search_path ?? true : true;
    return (
      <div className="relative">
        <ExpandButton title={title}>
          <SearchResults call={call} showPath={showSearchPath} />
        </ExpandButton>
        <SearchResults call={call} showPath={showSearchPath} />
      </div>
    );
  }
  if (name === "WebSearch") {
    return (
      <div className="relative">
        <ExpandButton title={title}>
          <SearchResults call={call} web />
        </ExpandButton>
        <SearchResults call={call} web />
      </div>
    );
  }
  if (name === "Fetch") {
    return (
      <div className="relative">
        <ExpandButton title={title}>
          <div className="space-y-2">
            <RenderedMarkdown text={result} />
            <ToolPre>{result}</ToolPre>
          </div>
        </ExpandButton>
        <FetchHeader call={call} />
        <div className="space-y-2 p-2">
          <RenderedMarkdown text={result} />
          <ToolPre>{result}</ToolPre>
        </div>
      </div>
    );
  }
  if (name === "Ask") {
    return <AskDetail call={call} />;
  }
  if (name === "ExitPlanMode") {
    return (
      <div className="relative">
        <ExpandButton title={title}>
          <RenderedMarkdown text={result} />
        </ExpandButton>
        <RenderedMarkdown text={result} />
      </div>
    );
  }
  if (name === "image_generation") {
    return (
      <div className="grid overflow-hidden bg-background md:grid-cols-[160px_minmax(0,1fr)]">
        <div className="min-h-32 bg-[radial-gradient(circle_at_35%_35%,rgba(22,119,255,0.28),transparent_28%),radial-gradient(circle_at_70%_65%,rgba(18,166,111,0.24),transparent_30%),linear-gradient(135deg,#f8fafc,#edf1f7)]" />
        <div className="space-y-2 p-2 text-[14px]">
          <div className="rounded-md border border-border bg-muted/40 px-2 py-1 text-muted-foreground">
            Hosted image_generation 由 provider 端执行。
          </div>
          <ToolPre>{result}</ToolPre>
        </div>
      </div>
    );
  }
  if (isTaskListTool(name)) {
    // 浮动 TaskPanel 显示最新状态，这里只回放本次调用提交的快照
    return (
      <div className="overflow-hidden bg-background">
        <TodoChecklist todos={parseTodos(call.argumentsText)} />
      </div>
    );
  }
  return <DefaultToolDetail call={call} />;
}

// ─── 嵌套子 agent 渲染（架构 §4.4.11.8 / P7）────────────────────────────────

function buildNestedRenderParts(parts: StreamingAssistantPart[]): AssistantRenderPart[] {
  const out: AssistantRenderPart[] = [];
  const pendingTools: ToolCallItem[] = [];
  parts.forEach((part, index) => {
    if (part.type === "text") {
      pushToolGroup(out, pendingTools);
      out.push({ type: "text", key: `nested-text-${index}`, text: part.text });
    } else if (part.type === "reasoning") {
      pushToolGroup(out, pendingTools);
      out.push({ type: "reasoning", key: `nested-reasoning-${index}`, text: part.text, streaming: false });
    } else {
      pendingTools.push(normalizeStreamingToolPart(part, index));
    }
  });
  pushToolGroup(out, pendingTools);
  return out;
}

function NestedTaskContent({
  nestedParts,
  appSettings,
  sessionId,
}: {
  nestedParts: StreamingAssistantPart[];
  appSettings?: AppSettings;
  sessionId?: string;
}) {
  const [expandedKeys, setExpandedKeys] = useState<Set<string>>(new Set());
  const onToggle = useCallback((key: string) => {
    setExpandedKeys((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const renderParts = buildNestedRenderParts(nestedParts);
  if (renderParts.length === 0) return null;

  return (
    <div className="ml-3 border-l-2 border-primary/20 pl-3 py-1 space-y-1 max-h-96 overflow-y-auto">
      {renderParts.map((part) => {
        if (part.type === "text") {
          return (
            <div
              key={part.key}
              className="markdown-segment text-[13px] leading-relaxed text-muted-foreground"
            >
              <ReactMarkdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
                {part.text}
              </ReactMarkdown>
            </div>
          );
        }
        if (part.type === "reasoning") {
          return <ReasoningBlock key={part.key} text={part.text} streaming={part.streaming} />;
        }
        if (part.type === "tool_group") {
          return (
            <ToolCallTimeline
              key={part.key}
              calls={part.calls}
              expandedKeys={expandedKeys}
              onToggle={onToggle}
              appSettings={appSettings}
              sessionId={sessionId}
            />
          );
        }
        return null;
      })}
    </div>
  );
}

function ToolCallTimeline({
  calls,
  expandedKeys,
  onToggle,
  appSettings,
  sessionId,
}: {
  calls: ToolCallItem[];
  expandedKeys: Set<string>;
  onToggle: (key: string) => void;
  appSettings?: AppSettings;
  sessionId?: string;
}) {
  // Read 工具显示路径用：在 workdir 内显示相对路径、在 allowed_paths 之一内显示
  // `<basename>/...`、否则显示完整绝对路径。沿用当前 session 的实际 workdir / allowed_paths。
  const workdir = useStore((s) => s.currentSession?.workdir ?? null);
  const allowedPaths = useStore(
    (s) => s.currentSession?.allowed_paths ?? EMPTY_STR_ARR,
  );

  // 监听 sidebar / 任何外部组件派发的「跳到这个 call」事件：
  // 如果是本 timeline 持有的某条 call，就把它展开（已展开则不动）。
  // 滚动 + 闪烁由 focusToolCall util 自己负责，不需要在这里做。
  useEffect(() => {
    function handler(e: Event) {
      const callId = (e as CustomEvent<string>).detail;
      const match = calls.find((c) => c.id === callId);
      if (!match) return;
      if (match.status !== "done") return; // 未 done 默认展开，不需要触发
      if (expandedKeys.has(match.key)) return;
      onToggle(match.key);
    }
    window.addEventListener(FOCUS_TOOL_CALL_EVENT, handler);
    return () => window.removeEventListener(FOCUS_TOOL_CALL_EVENT, handler);
  }, [calls, expandedKeys, onToggle]);

  if (calls.length === 0) return null;
  return (
    <div className="relative mt-1.5 space-y-1 rounded-md bg-muted/70 py-0.5 pl-6 pr-2">
      {calls.map((call, index) => {
        // 未 done 时（streaming / running / failed）默认展开，让运行中的 tool
        // 边输出边看；done 后折叠（带退场动画，见 ToolCallRow），靠用户手动 toggle 展开看 detail。
        // 这跟 ReasoningBlock 的"流完折叠"是一致语义。
        //
        // 例外：Read / Grep / Glob / Ask 等「查询类」工具运行中默认不展开——
        // 输出量大但用户多半不关心实时进度（Read 输出文件全文滚屏、Grep 输出大堆
        // 匹配行），自动展开反而把消息流挤到底。继续靠用户手动 toggle。
        const READ_LIKE = new Set(["Read", "Grep", "Glob", "Ask"]);
        const autoExpand = !READ_LIKE.has(call.name ?? "");
        // `expandedKeys` 语义 = 「相对默认值的显式翻转」。默认展开的 running tool
        // 点击 → 命中 expandedKeys → 折叠成功；再点击 → 移出 expandedKeys → 回到默认。
        const defaultExpanded = autoExpand && call.status !== "done";
        const active = expandedKeys.has(call.key) ? !defaultExpanded : defaultExpanded;
        return (
          <ToolCallRow
            key={call.key}
            call={call}
            index={index}
            total={calls.length}
            active={active}
            onToggle={onToggle}
            workdir={workdir}
            allowedPaths={allowedPaths}
            appSettings={appSettings}
            sessionId={sessionId}
          />
        );
      })}
    </div>
  );
}

function ToolCallRow({
  call,
  index,
  total,
  active,
  onToggle,
  workdir,
  allowedPaths,
  appSettings,
  sessionId,
}: {
  call: ToolCallItem;
  index: number;
  total: number;
  active: boolean;
  onToggle: (key: string) => void;
  workdir: string | null;
  allowedPaths: string[];
  appSettings?: AppSettings;
  sessionId?: string;
}) {
  // detail 的退场时序：active 翻 false 时不立即卸载内容，先播完高度收起动画再卸载。
  // 否则 status→done 当帧 active 变 false、detail 直接 unmount，就是"执行完啪一下消失"。
  // mounted 跟随 active 置真；active 转 false 后等过渡结束（onTransitionEnd / 兜底 timer）再置假。
  const [mounted, setMounted] = useState(active);
  useEffect(() => {
    if (active) {
      setMounted(true);
      return;
    }
    // grid-rows 过渡时长 300ms，留 50ms 余量兜底——onTransitionEnd 正常会先触发，
    // 这个 timer 只在过渡事件因元素被遮挡 / 浏览器抖动漏发时收尾。
    const t = window.setTimeout(() => setMounted(false), 350);
    return () => window.clearTimeout(t);
  }, [active]);

  // 左侧时间轴上的"状态点"取代原 ChevronRight：颜色编码状态——
  // done=绿 / running=蓝呼吸 / streaming(生成参数中)=灰 / failed=红。
  // ToolCallStatus 目前只有 streaming|running|done 三态，failed 分支预留。
  const statusDot = call.isJudging
    ? "animate-breathe bg-amber-400"
    : call.status === "done"
      ? call.isError
        ? "bg-rose-400"
        : "bg-green-400"
      : call.status === "running"
        ? "animate-breathe bg-primary"
        : "bg-muted-foreground/40";

  const titleClass = cn(
    "grid min-h-8 w-full cursor-pointer grid-cols-[18px_minmax(0,1fr)] items-center gap-2 px-1 py-1 text-left",
    // 分隔线用 inset 阴影而非 border-b：border 会参与盒模型，在 min-h-8
    // 锁死外高 32px 时把内容盒压成 31px，items-center 重新居中导致文本上跳；
    // inset 阴影零布局影响，展开/折叠文本不再抖动。
    active && "bg-muted/30 shadow-[inset_0_-1px_0_0_hsl(var(--border))]",
  );

  return (
    <div
      data-tool-call-id={call.id}
      // rounded-md：跟外层 tool_group 容器保持一致——focus-flash 闪烁时 box-shadow
      // 沿这个 wrapper 自身的 border-radius 绘制，视觉上贴着卡片本身边缘的圆角。
      // 平时 wrapper 无背景 / 边框，圆角不可见——仅 focus-flash 期间 box-shadow 才显示。
      className={cn(
        "relative rounded-[5px]",
        index === total - 1 && "pb-0",
        // AutoMode judge 评估中：黄色边框呼吸（架构 §4.4.4）。
        call.isJudging && "judge-breathe",
      )}
    >
      {/* 竖线从本行点中心(16px)向下连到下一行点中心：行高 32 + space-y-1 4 + 16 = 52，
          相对本 wrapper 即 top-[16px] 到 bottom-[-20px]（32-52）。展开时 wrapper 变高也成立——
          两端都相对各自 wrapper 定位，间距恒为 space-y-1。最后一行不画线。 */}
      {index !== total - 1 && (
        <div className="absolute -left-[15px] top-[16px] bottom-[-20px] w-px bg-border" />
      )}
      <button
        type="button"
        onClick={() => onToggle(call.key)}
        aria-label={active ? "折叠工具调用" : "展开工具调用"}
        // 竖线在 -left-[15px] w-px，中心 -14.5；让 button 本身就是圆点，
        // 中心 = -17.5 + 3 = -14.5，精确对齐竖线。top-[13px]+半径3 = 16，
        // 让圆点中心落在内容行(min-h-8=32px)的垂直中心。
        className={cn(
          "absolute -left-[17.5px] top-[13px] h-1.5 w-1.5 cursor-pointer rounded-full",
          statusDot,
        )}
      />
      <div
        className={cn(
          // translateZ(0)：把容器提成独立合成层，让 overflow 裁剪圆角的抗锯齿在
          // 层内一次完成。否则底部异色子背景（如 DefaultToolDetail 的 bg-muted/30
          // Input 列）直角顶到圆弧，会和父圆角抗锯齿叠加，在圆弧竖直段露出 1px 台阶。
          "overflow-hidden rounded-b-md [transform:translateZ(0)]",
          active && "bg-background",
        )}
      >
        {call.name === "Read" ? (
          <button type="button" onClick={() => onToggle(call.key)} className={titleClass}>
            {(() => {
              const args = callArgs(call);
              const path = argString(args, "file_path") || "读取文件";
              const offset = argString(args, "offset");
              const limit = argString(args, "limit");
              const range = offset
                ? limit
                  ? `${offset}-${Number(offset) + Number(limit) - 1}`
                  : `${offset}+`
                : "";
              const display = relativizeReadPath(path, workdir, allowedPaths);
              // 行号合并到路径后面，`:#xx-xx` 形式——选中复制粘到外部工具
              // 是「path:#100-150」一体的 vscode 风格 anchor，不再拆成两列
              const displayWithRange = range ? `${display}:#${range}` : display;
              return (
                <>
                  <span className="grid h-[18px] w-[18px] place-items-center text-muted-foreground">
                    <ScrollText className="h-3.5 w-3.5" />
                  </span>
                  <span className="flex min-w-0 items-center text-[12px] text-muted-foreground">
                    <span className="mr-[2ch] min-w-0 shrink-0 whitespace-nowrap font-semibold text-foreground">
                      Read
                    </span>
                    <span className="mr-[2ch] shrink-0">读取文件</span>
                    <code className="min-w-0 truncate font-mono text-[11px] text-foreground">
                      {displayWithRange}
                    </code>
                  </span>
                </>
              );
            })()}
          </button>
        ) : (
          <button type="button" onClick={() => onToggle(call.key)} className={titleClass}>
            <span className="grid h-[18px] w-[18px] place-items-center text-muted-foreground">
              <ToolIcon name={call.name} />
            </span>
            <span className="flex min-w-0 items-center text-[12px] text-muted-foreground">
              <span className="mr-[2ch] min-w-0 shrink-0 whitespace-nowrap font-semibold text-foreground">
                {call.name === "Task" && call.subagentType
                  ? call.subagentType
                  : call.name || "工具调用"}
              </span>
              <span className="mr-[2ch] shrink-0">{callDescription(call)}</span>
              <code className="min-w-0 truncate font-mono text-[11px] text-foreground">
                {callSummary(call)}
              </code>
            </span>
          </button>
        )}
        {/* 退场动画：grid-rows fr 单位插值（复用 ChatInput chips 同款模式，WKWebView 实测可插值）。
            active 控制目标高度 0fr↔1fr，过渡 300ms；mounted 决定 detail 是否还在树上——
            收起动画播完才卸载，避免 done 当帧内容直接消失。内层 overflow-hidden 裁掉收起中的溢出。 */}
        <div
          className={cn(
            "grid transition-[grid-template-rows] duration-300 ease-out",
            active ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
          )}
          onTransitionEnd={() => {
            if (!active) setMounted(false);
          }}
        >
          <div className="overflow-hidden">
            {mounted && (
              <>
                <ToolCallDetail call={call} appSettings={appSettings} sessionId={sessionId} />
                {call.name === "Task" &&
                  call.nestedParts &&
                  call.nestedParts.length > 0 && (
                    <div className="border-t border-border">
                      <NestedTaskContent
                        nestedParts={call.nestedParts}
                        appSettings={appSettings}
                        sessionId={sessionId}
                      />
                    </div>
                  )}
                {call.artifactPath && (
                  <div className="border-t border-border p-2">
                    <ArtifactBadge path={call.artifactPath} />
                  </div>
                )}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// CodeBlock / extractText 已抽到 ./CodeBlock 共享给 PlanTab / MarkdownRenderer 等。
// markdownComponents 也复用一份配置，避免本文件再单独维护。
const markdownComponents = { pre: CodeBlock } satisfies React.ComponentProps<
  typeof ReactMarkdown
>["components"];

function isRequestFailureText(text: string) {
  return text.trimStart().startsWith("[请求失败：");
}

const requestFailureMarkdownClass =
  "block w-full max-w-full box-border whitespace-pre-wrap break-words [overflow-wrap:anywhere] text-[13px] leading-[1.45]";

function ReasoningBlock({
  text,
  streaming,
  durationMs,
}: {
  text: string;
  streaming: boolean;
  durationMs?: number | null;
}) {
  const [open, setOpen] = useState(streaming);
  const prevStreamingRef = useRef(streaming);
  useEffect(() => {
    if (prevStreamingRef.current !== streaming) {
      setOpen(streaming);
    }
    prevStreamingRef.current = streaming;
  }, [streaming]);

  const [elapsedMs, setElapsedMs] = useState(durationMs ?? 0);
  const startedAtRef = useRef(Date.now() - (durationMs ?? 0));

  useEffect(() => {
    if (!streaming) {
      setElapsedMs(durationMs ?? elapsedMs);
      return;
    }
    const tick = () => setElapsedMs(Date.now() - startedAtRef.current);
    tick();
    const timer = window.setInterval(tick, 250);
    return () => window.clearInterval(timer);
  }, [streaming, durationMs]);

  const shownDuration = durationMs ?? elapsedMs;
  const trimmed = text.trim();
  if (!trimmed && !streaming) return null;

  return (
    <div className="space-y-px">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="inline-flex items-center gap-0.5 text-[7px] leading-[9px] text-muted-foreground hover:text-foreground"
      >
        <Brain className="h-3 w-3 shrink-0" />
        <span className="font-medium">
          {streaming ? "思考中…" : "思考过程"}
        </span>
        <span className="tabular-nums text-muted-foreground/80">
          {formatCompactDuration(shownDuration)}
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
        <ReasoningScrollArea text={text} streaming={streaming} />
      )}
    </div>
  );
}

function ReasoningScrollArea({ text, streaming }: { text: string; streaming: boolean }) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);

  useEffect(() => {
    if (!stickRef.current) return;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text]);

  const handleScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickRef.current = distFromBottom < 30;
  };

  return (
    <div
      ref={scrollRef}
      onScroll={handleScroll}
      className="border-l border-border/50 pl-2 text-[12px] leading-relaxed text-muted-foreground break-words overflow-y-auto"
      style={{ maxHeight: "10lh" }}
    >
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
  );
}

function AssistantParts({
  parts,
  streaming,
  expandedKeys,
  onToggle,
  appSettings,
  sessionId,
}: {
  parts: AssistantRenderPart[];
  streaming?: boolean;
  expandedKeys: Set<string>;
  onToggle: (key: string) => void;
  appSettings?: AppSettings;
  sessionId?: string;
}) {
  if (parts.length === 0) {
    return streaming ? <span>▍</span> : null;
  }

  return (
    <div className="space-y-2">
      {parts.map((part) => {
        if (part.type === "text") {
          return (
            <div
              key={part.key}
              className={cn(
                "markdown-segment",
                isRequestFailureText(part.text) && requestFailureMarkdownClass
              )}
            >
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
              durationMs={part.durationMs}
            />
          );
        }
        return (
          <ToolCallTimeline
            key={part.key}
            calls={part.calls}
            expandedKeys={expandedKeys}
            onToggle={onToggle}
            appSettings={appSettings}
            sessionId={sessionId}
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
  reserveBottomForQuestionPopup,
  sessionId,
  onFork,
  onRegenerate,
  onEdit,
  onDelete,
  streamingParts,
  find,
  archived,
  summaryExpanded,
  onToggleSummary,
  historyExpanded,
  onToggleHistory,
  archivedCount,
  canUndoCompaction,
  onUndoCompaction,
  appSettings,
  memoryWrites,
}: Props) {
  const [copied, setCopied] = useState(false);
  const [expandedToolCalls, setExpandedToolCalls] = useState<Set<string>>(
    () => new Set()
  );
  const [showRawText, setShowRawText] = useState(false);
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

  if (message.role === "marker" && message.meta?.type === "memory_writes") {
    return (
      <div className="px-6 py-1 select-none">
        <MemoryWriteSummary items={message.meta.items} />
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
            onClick={onToggleSummary ? () => onToggleSummary(message.id) : undefined}
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
              onClick={onToggleHistory ? () => onToggleHistory(message.id) : undefined}
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
          {canUndoCompaction && (
            <button
              type="button"
              onClick={
                onUndoCompaction ? () => onUndoCompaction(message.id) : undefined
              }
              title="撤销这次压缩，回到压缩前（可换模型重新压缩）"
              className="inline-flex items-center gap-1 rounded-full border border-border bg-background px-2 py-1 transition-colors hover:bg-muted hover:text-foreground cursor-pointer"
            >
              <Undo2 className="w-3 h-3" />
              <span>撤销压缩</span>
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
  const shouldReserveBottomForQuestionPopup =
    !!reserveBottomForQuestionPopup &&
    assistantParts.some(
      (part) => part.type === "tool_group" && part.calls.some((call) => call.name === "Ask")
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
        appSettings={appSettings}
        sessionId={sessionId}
      />
    );
  }

  return (
    <div
      data-message-role={message.role}
      data-message-id={message.id}
      title={archived ? "已被压缩，模型不再读取此消息（点击右上角圆环可再次压缩）" : undefined}
      className={cn(
        "group relative flex gap-3 px-6 py-4",
        shouldReserveBottomForQuestionPopup && "mb-[320px]",
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
                  setActionMenuOpen(false);
                }}
                className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                <FileText className="h-3.5 w-3.5" />
                <span>{showRawText ? "显示渲染" : "显示原文"}</span>
              </button>
            )}
            {onDelete && (
              <button
                type="button"
                onClick={() => {
                  setActionMenuOpen(false);
                  onDelete(message.id, message.role);
                }}
                className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-destructive hover:bg-destructive/10"
              >
                <Trash2 className="h-3.5 w-3.5" />
                <span>删除</span>
              </button>
            )}
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
      </div>
      <div className={cn("flex-1 min-w-0", canToggleRawText && "pr-8")}>
        <div className="flex items-center gap-2 mb-1.5 text-xs message-role-label">
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
        {!isUser && streaming && (
          <LoopingWebm
            src={animations.sendInterrupt}
            className="mt-2 h-8 w-8 rounded-full"
          />
        )}
        <AttachmentPreviewStrip
          attachments={message.attachments}
          variant={isUser ? "compact" : "gallery"}
          className="mt-2"
        />
        {!streaming && !editing && memoryWrites && memoryWrites.length > 0 && (
          <div className="mt-2">
            <MemoryWriteSummary items={memoryWrites} />
          </div>
        )}
        {!streaming && !editing && (
          <div className="opacity-0 group-hover:opacity-100 transition-opacity flex items-center gap-1 mt-2 -ml-1.5 text-[10px]">
            <span className="px-1.5 py-1 text-muted-foreground/70">
              {formatTime(message.created_at)}
            </span>
            {message.run_duration_ms != null && (
              <span className="px-1 py-1 text-muted-foreground/60 tabular-nums">
                · {formatCompactDuration(message.run_duration_ms)}
              </span>
            )}
            <button
              onClick={handleCopy}
              className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-[10px]"
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
                className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-[10px]"
                title="从此处分叉新对话"
              >
                <GitBranch className="w-3.5 h-3.5" />
                <span>分叉</span>
              </button>
            )}
            {isUser && onEdit && (
              <button
                onClick={startEdit}
                className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-[10px]"
                title="编辑后重跑"
              >
                <Pencil className="w-3.5 h-3.5" />
                <span>编辑</span>
              </button>
            )}
            {onRegenerate && (
              <button
                onClick={() => onRegenerate(message.id)}
                className="px-1.5 py-1 rounded hover:bg-accent text-muted-foreground inline-flex items-center gap-1 text-[10px]"
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
