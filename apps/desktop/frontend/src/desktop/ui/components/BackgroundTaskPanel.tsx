import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  AlertCircle,
  ChevronDown,
  ChevronRight,
  Clock,
  Pause,
  Square,
  Terminal,
} from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { focusToolCall } from "@/desktop/ui/lib/focusToolCall";
import type {
  BackgroundTaskInfo,
  Message,
  SessionBackgroundReport,
} from "@/desktop/ui/types";

/**
 * 旧版本浮动框——已被 RightSidebar 内的 `BackgroundTaskTab` 替代（架构 §4.12.9 修订）。
 * 保留 export 占位让老的 import 还能通过类型检查；本身永远不渲染。
 */
export function BackgroundTaskPanel() {
  return null;
}

/**
 * 工作台 sidebar 内的「后台任务」tab 内容（架构 §4.12.9 修订）。
 *
 * 数据源单一化（借鉴 Claude Code 派的 transcript-as-source-of-truth，附录 D.4）：
 * - 主源：`session.messages` 里所有 Bash + `run_in_background:true`（或前台超时转后台）
 *   的 tool_call —— 这是历史完整账本，已完成 task **永远不会从这里消失**
 * - 实时状态 / 输出：从注册表 polling（每 3s 拉一次 `listBackgroundTasks`）+
 *   按 task_id join；展开卡片时再 polling 一次 `readBackgroundTaskOutput` 取增量
 *
 * 排序：running 优先 → 其余按 tool_call 在 messages 中的出现顺序（时间序）。
 * 已完成 task：默认折叠态（只显示 task_id + cmd + 状态徽章），点开看完整输出。
 */
// zustand selector 必须返回 stable 引用——`?? []` 每次产生新数组会触发
// "getSnapshot should be cached" 无限循环。selector 只取 raw 引用，
// `??` fallback 放到组件 body 里执行。
const EMPTY_MESSAGES: Message[] = [];

export function BackgroundTaskTab() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const suspended = useStore((s) => s.suspended);
  const messagesRaw = useStore((s) => s.currentSession?.messages);
  const messages = messagesRaw ?? EMPTY_MESSAGES;
  const [report, setReport] = useState<SessionBackgroundReport | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [now, setNow] = useState<number>(Date.now());
  const lastSessionRef = useRef<string | null>(null);

  // 切换 session 时清状态
  useEffect(() => {
    if (lastSessionRef.current !== sessionId) {
      lastSessionRef.current = sessionId;
      setReport(null);
      setExpanded(new Set());
    }
  }, [sessionId]);

  // 注册表轮询（3s 一次足够，task 状态变化粒度本来就是秒级）
  useEffect(() => {
    if (!sessionId) {
      setReport(null);
      return;
    }
    let cancelled = false;
    const refresh = async () => {
      try {
        const r = await api.listBackgroundTasks(sessionId);
        if (!cancelled) setReport(r);
      } catch {
        // 静默——session 已删等场景，下次轮询自动消失
      }
    };
    refresh();
    const t = setInterval(refresh, 3000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, [sessionId]);

  // 1Hz tick：让"挂起 N s"和 cron 倒计时实时更新
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  const items = useMemo(
    () => deriveBackgroundTasks(messages, report),
    [messages, report]
  );

  if (!sessionId) {
    return <EmptyHint icon={<Terminal />}>当前没打开对话</EmptyHint>;
  }

  const pendingCrons = report?.pending_crons ?? [];
  const orphanedCheckpoint =
    !!report?.has_suspended_checkpoint &&
    !suspended &&
    items.every((it) => it.status !== "running") &&
    pendingCrons.length === 0;

  const suspendedElapsedSec = suspended
    ? Math.max(0, Math.round((now - suspended.suspendedAtMs) / 1000))
    : 0;

  async function killShell(taskId: string) {
    if (!sessionId) return;
    try {
      const state = await api.killBackgroundTask(sessionId, taskId);
      toast.success(`已停止 ${taskId}（${state}）`);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  return (
    <div className="flex flex-col text-[12px]">
      {/* 状态横幅（挂起 / 中断 checkpoint） */}
      {suspended && (
        <div className="m-2 flex items-start gap-1.5 rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5">
          <Pause className="mt-0.5 h-3 w-3 shrink-0 text-amber-600 dark:text-amber-400" />
          <div className="min-w-0 text-[11px] leading-tight">
            <div className="font-medium text-amber-700 dark:text-amber-300">
              Run 已挂起 {suspendedElapsedSec}s
            </div>
            <div className="mt-0.5 text-amber-700/80 dark:text-amber-300/80">
              {suspended.reason === "background_task"
                ? `等 ${suspended.waitingForTaskIds.join(", ") || "?"} 完成`
                : suspended.reason === "cron"
                  ? suspended.resumesAtMs != null
                    ? `${Math.max(0, Math.round((suspended.resumesAtMs - now) / 1000))}s 后唤醒`
                    : "定时唤醒"
                  : "等待"}
            </div>
          </div>
        </div>
      )}
      {orphanedCheckpoint && (
        <div className="m-2 flex items-start gap-1.5 rounded-md border border-orange-500/30 bg-orange-500/10 px-2 py-1.5">
          <AlertCircle className="mt-0.5 h-3 w-3 shrink-0 text-orange-600 dark:text-orange-400" />
          <div className="min-w-0 text-[11px] leading-tight">
            <div className="font-medium text-orange-700 dark:text-orange-300">
              上次会话中断
            </div>
            <div className="mt-0.5 text-orange-700/80 dark:text-orange-300/80">
              checkpoint 已落盘但调度器不在等。发新消息会从中断点继续。
            </div>
          </div>
        </div>
      )}

      {/* cron 待唤醒 */}
      {pendingCrons.length > 0 && (
        <div className="mx-2 mt-2">
          <SectionLabel>定时唤醒</SectionLabel>
          {pendingCrons.map((c) => (
            <div
              key={`${c.run_id}-${c.fire_at_ms}`}
              className="mt-1 flex items-start gap-1.5 rounded-md border border-border bg-muted/20 px-2 py-1 text-[11px]"
            >
              <Clock className="mt-0.5 h-3 w-3 shrink-0 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <div className="truncate" title={c.reason}>
                  {c.reason || "(无说明)"}
                </div>
                <div className="mt-0.5 text-[10px] text-muted-foreground">
                  {c.seconds_remaining}s 后唤醒
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* 任务列表 */}
      {items.length === 0 ? (
        <EmptyHint icon={<Terminal />}>
          还没有后台任务。
          <br />
        </EmptyHint>
      ) : (
        <div className="flex flex-col">
          {items.map((item) => (
            <TaskCard
              key={item.task_id ?? item.tool_call_id}
              item={item}
              sessionId={sessionId}
              expanded={expanded.has(item.task_id ?? item.tool_call_id)}
              onToggle={() =>
                setExpanded((prev) => {
                  const key = item.task_id ?? item.tool_call_id;
                  const next = new Set(prev);
                  if (next.has(key)) next.delete(key);
                  else next.add(key);
                  return next;
                })
              }
              onKill={item.task_id ? () => killShell(item.task_id!) : undefined}
            />
          ))}
        </div>
      )}
    </div>
  );
}

interface TaskItem {
  /** 注册表 task_id；某些异常 case 模型 result 还没 parse 出来时为 null */
  task_id: string | null;
  /** 对应 tool_call.id，用于在 chat 区滚动定位 */
  tool_call_id: string;
  /** 对应 message.id，用于 `[data-message-id="..."]` 锚点跳转 */
  message_id: string;
  command: string;
  status: "running" | "exited" | "killed" | "failed" | "unknown";
  /** 注册表里的实时元信息（如果还在） */
  shell?: BackgroundTaskInfo;
  /** 最终 tool result 文本 */
  result?: string | null;
  duration_ms?: number | null;
}

/**
 * 从 session.messages 派生历史 + 用注册表 join 实时状态。
 * messages 是 source of truth：完成的 task 永远在 messages 里，不依赖注册表保留。
 */
function deriveBackgroundTasks(
  messages: Message[],
  report: SessionBackgroundReport | null
): TaskItem[] {
  const shellsByTaskId = new Map<string, BackgroundTaskInfo>();
  for (const s of report?.shells ?? []) {
    shellsByTaskId.set(s.task_id, s);
  }
  const consumed = new Set<string>();
  const items: TaskItem[] = [];

  // 1. 从 messages 找历史 Bash bg task（含前台超时转后台的）
  for (const m of messages) {
    for (const tc of m.tool_calls ?? []) {
      if (tc.name !== "Bash") continue;
      const input = (tc.input as Record<string, any> | undefined) ?? {};
      const explicit = input.run_in_background === true;
      const result = tc.result ?? "";
      const taskId = extractTaskId(result);
      // 仅前台正常结束的 Bash 不该出现（没 task_id 且 explicit=false）
      if (!explicit && !taskId) continue;
      const shell = taskId ? shellsByTaskId.get(taskId) : undefined;
      if (taskId) consumed.add(taskId);
      const status: TaskItem["status"] = shell
        ? (shell.state as TaskItem["status"])
        : tc.result
          ? "exited"
          : "running";
      items.push({
        task_id: taskId,
        tool_call_id: tc.id,
        message_id: m.id,
        command: typeof input.command === "string" ? input.command : "(无命令)",
        status,
        shell,
        result: tc.result,
        duration_ms: tc.duration_ms,
      });
    }
  }
  // 2. 注册表有但 messages 还没记到的（task 刚启动 / tool_result 还没回来 / 上次会话残留）
  for (const s of report?.shells ?? []) {
    if (consumed.has(s.task_id)) continue;
    items.push({
      task_id: s.task_id,
      tool_call_id: `pending-${s.task_id}`,
      message_id: "",
      command: s.command,
      status: s.state as TaskItem["status"],
      shell: s,
    });
  }
  // 3. 排序：running 优先（按 elapsed_secs 升序新的在前）；其他保持 messages 时序
  const runningItems = items.filter((it) => it.status === "running");
  const otherItems = items.filter((it) => it.status !== "running");
  runningItems.sort((a, b) => {
    const ae = a.shell?.elapsed_secs ?? 0;
    const be = b.shell?.elapsed_secs ?? 0;
    return ae - be;
  });
  return [...runningItems, ...otherItems];
}

// 兼容新旧两种格式：
// 新（2026-05-22 精简文案后）：`[bash_001] 已在后台启动` / `[bash_001] 60s 内未结束，已转后台`
// 旧：`task_id=bash_001 cmd=...`
const TASK_ID_RE = /(?:task_id=|\[)(bash_\d+)/;
function extractTaskId(result: string): string | null {
  const m = result.match(TASK_ID_RE);
  return m ? m[1] : null;
}

function TaskCard({
  item,
  sessionId,
  expanded,
  onToggle,
  onKill,
}: {
  item: TaskItem;
  sessionId: string;
  expanded: boolean;
  onToggle: () => void;
  onKill?: () => void;
}) {
  const isRunning = item.status === "running";
  const [liveOutput, setLiveOutput] = useState<string>("");
  const cursorRef = useRef<number>(0);

  // 卡片展开 + 任务运行中：polling 实时输出（~600ms 一次）
  useEffect(() => {
    if (!expanded || !isRunning || !item.task_id) return;
    cursorRef.current = 0;
    setLiveOutput("");
    let cancelled = false;
    const tick = async () => {
      try {
        const out = await api.readBackgroundTaskOutput(
          sessionId,
          item.task_id!,
          cursorRef.current
        );
        if (cancelled) return;
        if (out.chunk) {
          setLiveOutput((prev) => prev + out.chunk);
        }
        cursorRef.current = out.total_bytes;
      } catch {
        // 静默——task 已 GC 时下次取空 chunk
      }
    };
    tick();
    const t = setInterval(tick, 600);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, [expanded, isRunning, sessionId, item.task_id]);

  // 跳到 chat 区域里对应的 Bash 工具卡片，并展开 + 边框闪烁——比之前只滚到
  // message bubble 更精确，跟 EditTree 用同一套 focusToolCall 机制。
  const jumpToToolCall = () => {
    if (item.tool_call_id.startsWith("pending-")) return; // 还没在 messages 里就放弃
    focusToolCall(item.tool_call_id);
  };

  return (
    <div
      className={cn(
        "border-b border-border/60 transition-colors",
        isRunning && "bg-amber-500/5"
      )}
    >
      <button
        type="button"
        onClick={() => {
          // 点击整行：同时切展开态 + 跳到 chat 里对应的工具卡片（折叠态也跳，
          // 因为用户的意图就是"我想看看这次后台任务在对话里哪个位置")
          onToggle();
          jumpToToolCall();
        }}
        className="block w-full px-3 py-2 text-left hover:bg-accent/30"
      >
        <div className="flex items-center gap-1.5">
          <StatusDot status={item.status} />
          <code className="shrink-0 font-mono text-[10px] text-muted-foreground">
            {item.task_id ?? "pending"}
          </code>
          {item.shell ? (
            <span className="shrink-0 text-[10px] text-muted-foreground">
              {item.shell.elapsed_secs}s
            </span>
          ) : item.duration_ms != null ? (
            <span className="shrink-0 text-[10px] text-muted-foreground">
              {Math.round(item.duration_ms / 1000)}s
            </span>
          ) : null}
          {!isRunning && (
            <span className="ml-1 rounded bg-muted px-1 text-[9px] uppercase text-muted-foreground">
              {item.status}
            </span>
          )}
          <span className="ml-auto text-muted-foreground">
            {expanded ? (
              <ChevronDown className="h-3 w-3" />
            ) : (
              <ChevronRight className="h-3 w-3" />
            )}
          </span>
        </div>
        <div
          className="mt-1 truncate font-mono text-[11px] text-foreground/85"
          title={item.command}
        >
          $ {item.command}
        </div>
      </button>
      {expanded && (
        <div className="border-t border-border/60 bg-background/40 px-3 py-2">
          {isRunning && onKill && (
            <div className="mb-1.5 flex items-center gap-2 text-[10px] text-muted-foreground">
              <button
                type="button"
                onClick={onKill}
                className="ml-auto inline-flex items-center gap-1 text-destructive hover:underline"
                title="停止该任务"
              >
                <Square className="h-3 w-3" />
                停止
              </button>
            </div>
          )}
          <pre className="max-h-[240px] overflow-auto whitespace-pre-wrap rounded border border-border bg-zinc-900 px-2 py-1.5 font-mono text-[10px] leading-[1.45] text-zinc-200">
            {isRunning
              ? liveOutput || "等待输出…"
              : item.result || "(无输出)"}
          </pre>
        </div>
      )}
    </div>
  );
}

function StatusDot({ status }: { status: TaskItem["status"] }) {
  const color =
    status === "running"
      ? "bg-amber-500 animate-pulse"
      : status === "exited"
        ? "bg-emerald-500"
        : status === "killed" || status === "failed"
          ? "bg-red-500"
          : "bg-zinc-400";
  return <span className={cn("h-2 w-2 shrink-0 rounded-full", color)} />;
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[10px] uppercase tracking-wide text-muted-foreground">
      {children}
    </div>
  );
}

function EmptyHint({
  icon,
  children,
}: {
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="grid h-full place-items-center px-4 py-8 text-center text-[11px] text-muted-foreground">
      <div>
        <div className="mx-auto mb-2 opacity-40 [&_svg]:h-5 [&_svg]:w-5">
          {icon}
        </div>
        <div className="leading-snug">{children}</div>
      </div>
    </div>
  );
}
