import { useMemo, useState } from "react";
import { Check, ChevronDown, ChevronRight, Circle, ListTodo } from "lucide-react";
import { useStore, selectCurrentSessionStream } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import {
  isTaskListTool,
  parseTodos,
  type TodoItem,
  type TodoStatus,
} from "./MessageBubble";
import { todoBlocksForDisplay, type TodoDisplayBlock } from "./todoBlocksForDisplay";
import type { Session, StreamingAssistantPart } from "@/desktop/ui/types";

/**
 * 右侧工作台「任务清单」tab。
 *
 * 数据派生：扫 currentSession.messages + 当前 streamingParts 里所有 TodoWrite
 * 调用，每次调用 → 一次"快照"。同一份 list 的多次更新（pending → completed）
 * 通过 **id 重叠 / content 重叠** 归并成同一个块；新一轮独立任务集形成新块。
 *
 * 渲染：blocks 倒序——最新在最上面（默认展开），旧块在下（默认折叠）。
 * 折叠头**只放图标 + 进度条 + N/M**，不写文字标题——窄 sidebar 才不会被挤崩。
 */
export function TodoTab() {
  const currentSession = useStore((s) => s.currentSession);
  const sessionStream = useStore(selectCurrentSessionStream);
  const streamingParts = sessionStream.streamingParts;
  const todos = sessionStream.todos;

  const fallbackBlocks = useMemo(
    () => extractTodoBlocks(currentSession ?? undefined, streamingParts),
    [currentSession, streamingParts],
  );
  const blocks = useMemo(
    () => todoBlocksForDisplay(todos, fallbackBlocks),
    [todos, fallbackBlocks],
  );

  if (!currentSession) {
    return (
      <div className="p-4 text-xs text-muted-foreground">打开对话后会显示任务清单。</div>
    );
  }
  if (blocks.length === 0) {
    return (
      <div className="p-4 text-xs text-muted-foreground">
        Agent 调用 TodoWrite 时自动填入。
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <div className="min-h-0 flex-1 space-y-1 overflow-auto px-2 py-2 [scrollbar-color:#424242_transparent] [scrollbar-width:thin]">
        {blocks.map((block, idx) => (
          <TodoBlock key={block.key} block={block} defaultOpen={idx === 0} />
        ))}
      </div>
    </div>
  );
}

type Block = TodoDisplayBlock<TodoItem>;

function TodoBlock({ block, defaultOpen }: { block: Block; defaultOpen: boolean }) {
  const total = block.todos.length;
  const completed = block.todos.filter((t) => t.status === "completed").length;
  const inProgress = block.todos.filter((t) => t.status === "in_progress").length;
  const allDone = total > 0 && completed === total;
  const ratio = total > 0 ? Math.round((completed / total) * 100) : 0;
  const [open, setOpen] = useState(defaultOpen);

  const headerCls = allDone
    ? "text-[#89d185] hover:bg-[#2a2d2e]"
    : "text-[#cca700] hover:bg-[#2a2d2e]";
  const barTrackCls = "bg-[#3c3c3c]";
  const barFillCls = allDone ? "bg-[#89d185]" : "bg-[#cca700]";

  return (
    <div className="overflow-hidden rounded-sm border border-[#2b2b2b] bg-[#252526]">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className={cn(
          "flex w-full items-center gap-1.5 px-2 py-1.5 text-xs transition-colors",
          headerCls,
        )}
        title={open ? "收起" : "展开"}
      >
        {open ? (
          <ChevronDown className="h-3 w-3 shrink-0" />
        ) : (
          <ChevronRight className="h-3 w-3 shrink-0" />
        )}
        {allDone ? (
          <Check className="h-3.5 w-3.5 shrink-0" strokeWidth={3} />
        ) : (
          <ListTodo className="h-3.5 w-3.5 shrink-0" />
        )}
        <div className={cn("h-1.5 flex-1 overflow-hidden rounded-full", barTrackCls)}>
          <div
            className={cn("h-full transition-all", barFillCls)}
            style={{ width: `${ratio}%` }}
          />
        </div>
        <span className="shrink-0 font-mono text-[11px] tabular-nums">
          {completed}/{total}
        </span>
        {inProgress > 0 && !allDone && (
          <span
            className="h-1.5 w-1.5 shrink-0 rounded-full bg-[#cca700]"
            title={`${inProgress} 进行中`}
          />
        )}
      </button>
      {open && (
        <ul className="space-y-0.5 border-t border-[#2b2b2b] bg-[#1e1e1e] px-1 py-1">
          {block.todos.map((it, i) => (
            <li
              key={it.id ?? `${block.key}-${i}`}
              className={cn(
                "group/item flex items-start gap-2 rounded-sm px-2 py-1.5 text-xs leading-snug transition-colors hover:bg-[#2a2d2e]",
                it.status === "completed"
                  ? "text-[#cccccc]"
                  : "text-[#d4d4d4]",
              )}
            >
              <StatusIndicator status={it.status} />
              <span className="min-w-0 break-words">
                {it.status === "in_progress" && it.activeForm ? it.activeForm : it.content}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function StatusIndicator({ status }: { status: TodoStatus }) {
  if (status === "completed") {
    return (
      <span
        aria-label="已完成"
        className="mt-0.5 inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-sm bg-[#3fb950] text-[10px] text-[#0d1117]"
      >
        ✓
      </span>
    );
  }
  if (status === "in_progress") {
    return (
      <span
        aria-label="进行中"
        className="mt-0.5 inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-sm border border-[#cca700]"
      >
        <span className="h-0.5 w-2 bg-[#cca700]" />
      </span>
    );
  }
  return <Circle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-[#cca700]" />;
}

/**
 * 扫 messages + streamingParts 里所有 TodoWrite 调用，按时间序排好，再归并成 blocks。
 *
 * 归并：从前往后处理每次调用，如果当前调用的 todos 与**上一个块**有 id 重叠或
 * content 重叠（兜底——模型不传稳定 id 时），视为同块更新（覆盖整块）；否则新块。
 *
 * 返回顺序倒序：新块在前。
 */
function extractTodoBlocks(
  session: Session | undefined,
  streamingParts: StreamingAssistantPart[] | null | undefined,
): Block[] {
  const calls: Array<{ key: string; todos: TodoItem[]; ts: number; streaming: boolean }> = [];

  for (const msg of session?.messages ?? []) {
    if (msg.role !== "assistant") continue;
    const parts = msg.parts ?? [];
    for (const p of parts) {
      if (p.type === "tool_call" && isTaskListTool(p.name)) {
        const text =
          p.input === undefined
            ? p.arguments ?? ""
            : typeof p.input === "string"
              ? p.input
              : JSON.stringify(p.input);
        const todos = parseTodos(text);
        if (todos.length > 0) {
          calls.push({
            key: p.id ?? `${msg.id}-${calls.length}`,
            todos,
            ts: msg.created_at,
            streaming: false,
          });
        }
      }
    }
    const legacy = msg.tool_calls ?? [];
    for (const c of legacy) {
      if (!isTaskListTool(c.name)) continue;
      if (calls.some((x) => x.key === c.id)) continue;
      const text = typeof c.input === "string" ? c.input : JSON.stringify(c.input);
      const todos = parseTodos(text);
      if (todos.length > 0) {
        calls.push({ key: c.id, todos, ts: msg.created_at, streaming: false });
      }
    }
  }

  for (const sp of streamingParts ?? []) {
    if (sp.type !== "tool_call" || !isTaskListTool(sp.name)) continue;
    const text =
      sp.input === undefined
        ? sp.arguments ?? ""
        : typeof sp.input === "string"
          ? sp.input
          : JSON.stringify(sp.input);
    const todos = parseTodos(text);
    if (todos.length > 0) {
      const key = sp.id ?? `streaming-${calls.length}`;
      if (!calls.some((x) => x.key === key)) {
        calls.push({ key, todos, ts: Date.now(), streaming: true });
      }
    }
  }

  const blocks: Block[] = [];
  for (const call of calls) {
    const last = blocks[blocks.length - 1];
    if (last && (hasIdOverlap(last.todos, call.todos) || hasContentOverlap(last.todos, call.todos))) {
      last.todos = call.todos;
      last.ts = call.ts;
      last.streaming = call.streaming;
    } else {
      blocks.push({
        key: call.key,
        todos: call.todos,
        ts: call.ts,
        streaming: call.streaming,
      });
    }
  }

  return blocks.reverse();
}

function hasIdOverlap(a: TodoItem[], b: TodoItem[]): boolean {
  const ids = new Set(a.map((t) => t.id).filter((id): id is string => Boolean(id)));
  if (ids.size === 0) return false;
  return b.some((t) => t.id && ids.has(t.id));
}

/** id 不匹配时的兜底——模型不传稳定 id 时，按 content 字符串重叠判同块。 */
function hasContentOverlap(a: TodoItem[], b: TodoItem[]): boolean {
  const set = new Set(a.map((t) => t.content.trim()));
  return b.some((t) => set.has(t.content.trim()));
}
