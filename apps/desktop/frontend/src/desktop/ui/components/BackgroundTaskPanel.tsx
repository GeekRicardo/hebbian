import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  AlertCircle,
  Bot,
  ChevronDown,
  ChevronRight,
  Clock,
  Square,
  Terminal,
} from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { focusToolCall } from "@/desktop/ui/lib/focusToolCall";
import { ansiToHtml } from "@/desktop/ui/lib/ansiToHtml";
import {
  deriveBackgroundTasks,
  type TaskItem,
} from "@/desktop/ui/lib/backgroundTasks";
import type { Message, SessionBackgroundReport } from "@/desktop/ui/types";
import { SuspendedBanner } from "./SuspendedBanner";

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
      {suspended && <SuspendedBanner suspended={suspended} variant="sidebar" />}
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

      {/* 任务列表（Bash 后台 + ScheduleWakeup 定时唤醒，按时序混排） */}
      {items.length === 0 ? (
        <EmptyHint icon={<Terminal />}>
          还没有后台任务。
          <br />
        </EmptyHint>
      ) : (
        <div className="flex flex-col space-y-2 px-2 py-2">
          {items.map((item) => (
            <TaskCard
              key={item.task_id ?? item.tool_call_id}
              item={item}
              sessionId={sessionId}
              now={now}
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
              onKill={item.kind === "bash" && item.task_id ? () => killShell(item.task_id!) : undefined}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function TaskCard({
  item,
  sessionId,
  now,
  expanded,
  onToggle,
  onKill,
}: {
  item: TaskItem;
  sessionId: string;
  now: number;
  expanded: boolean;
  onToggle: () => void;
  onKill?: () => void;
}) {
  const isCron = item.kind === "cron";
  const isSubagent = item.kind === "subagent";
  const isRunning = item.status === "running";
  const [liveOutput, setLiveOutput] = useState<string>("");
  const cursorRef = useRef<number>(0);

  // 卡片展开 + 任务运行中：polling 实时输出（~600ms 一次）。cron / subagent 无输出可拉，跳过。
  useEffect(() => {
    if (isCron || isSubagent || !expanded || !isRunning || !item.task_id) return;
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
  }, [expanded, isRunning, isCron, isSubagent, sessionId, item.task_id]);

  // 跳到 chat 区域里对应的 Bash 工具卡片，并展开 + 边框闪烁——比之前只滚到
  // message bubble 更精确，跟 EditTree 用同一套 focusToolCall 机制。
  const jumpToToolCall = () => {
    if (item.tool_call_id.startsWith("pending-")) return; // 还没在 messages 里就放弃
    focusToolCall(item.tool_call_id);
  };

  return (
    <div
      className={cn(
        "overflow-hidden rounded-md border border-border/60 bg-background transition-all",
        // 默认：与大卡片同款的模糊散开阴影；hover 才切成 neobrutalism 错位
        "shadow-[-3px_2px_8px_-2px_rgba(0,0,0,0.10),-1px_1px_2px_-1px_rgba(0,0,0,0.06)]",
        "dark:shadow-[-3px_2px_8px_-2px_rgba(0,0,0,0.45),-1px_1px_2px_-1px_rgba(0,0,0,0.3)]",
        "hover:shadow-[-6px_8px_4px_1px_rgba(0,0,0,0.50),-18px_21px_14px_-1px_rgba(0,0,0,0.16)] dark:hover:shadow-[-6px_8px_4px_1px_rgba(0,0,0,0.75),-18px_21px_14px_-1px_rgba(0,0,0,0.40)]",
        "hover:translate-x-px hover:-translate-y-px hover:border-border",
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
          {isCron ? (
            <>
              <Clock className="h-3 w-3 shrink-0 text-muted-foreground" />
              <span className="shrink-0 text-[10px] text-muted-foreground">
                {item.cron!.pending
                  ? `${cronCountdown(item.cron!.fireAtMs, now)} 后唤醒`
                  : `已于 ${formatClock(item.cron!.fireAtMs)} 唤醒`}
              </span>
            </>
          ) : isSubagent ? (
            <>
              <Bot className="h-3 w-3 shrink-0 text-muted-foreground" />
              <code className="shrink-0 font-mono text-[10px] text-muted-foreground">
                {item.task_id ?? "subagent"}
              </code>
            </>
          ) : (
            <>
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
            </>
          )}
          {!isRunning && (
            <span className="ml-1 rounded bg-muted px-1 text-[9px] uppercase text-muted-foreground">
              {isCron ? "已唤醒" : item.status}
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
          {isCron ? `⏰ ${item.command}` : isSubagent ? `🤖 ${item.command}` : `$ ${item.command}`}
        </div>
      </button>
      {expanded && (
        <div className="border-t border-border/60 bg-background/40 px-3 py-2">
          {isCron ? (
            <div className="space-y-1 text-[11px] leading-relaxed text-foreground/85">
              <div>
                <span className="text-muted-foreground">原因：</span>
                {item.cron!.reason}
              </div>
              <div>
                <span className="text-muted-foreground">唤醒时刻：</span>
                {formatClock(item.cron!.fireAtMs)}
                {item.cron!.pending && (
                  <span className="ml-1 text-muted-foreground">
                    （{cronCountdown(item.cron!.fireAtMs, now)} 后）
                  </span>
                )}
              </div>
            </div>
          ) : isSubagent ? (
            <div className="space-y-1 text-[11px] leading-relaxed text-foreground/85">
              <div>
                <span className="text-muted-foreground">子代理：</span>
                {item.command}
              </div>
              <div className="text-muted-foreground">
                {isRunning
                  ? "正在后台运行。完成后会自动唤醒这个会话，结果会出现在对话里。"
                  : "已完成，并已唤醒这个会话。"}
              </div>
            </div>
          ) : (
            <>
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
              <pre
                className="max-h-[240px] overflow-auto whitespace-pre-wrap rounded border border-border bg-zinc-900 px-2 py-1.5 font-mono text-[10px] leading-[1.45] text-zinc-200"
                dangerouslySetInnerHTML={{
                  __html: ansiToHtml(
                    isRunning
                      ? liveOutput || "等待输出…"
                      : item.result || "(无输出)"
                  ),
                }}
              />
            </>
          )}
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

// 倒计时：fireAt - now，紧凑格式（天/时/分/秒逐级）。
function cronCountdown(fireAtMs: number, now: number): string {
  const secs = Math.max(0, Math.round((fireAtMs - now) / 1000));
  const days = Math.floor(secs / 86400);
  const hours = Math.floor((secs % 86400) / 3600);
  const mins = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  const parts: string[] = [];
  if (days > 0) parts.push(`${days}d`);
  if (hours > 0) parts.push(`${hours}h`);
  if (mins > 0) parts.push(`${mins}m`);
  if (s > 0 || parts.length === 0) parts.push(`${s}s`);
  return parts.join("");
}

// 唤醒时刻：HH:MM（同一天）/ MM-DD HH:MM（跨天）。
function formatClock(ms: number): string {
  const d = new Date(ms);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  const sameDay = new Date().toDateString() === d.toDateString();
  if (sameDay) return `${hh}:${mm}`;
  const MM = String(d.getMonth() + 1).padStart(2, "0");
  const DD = String(d.getDate()).padStart(2, "0");
  return `${MM}-${DD} ${hh}:${mm}`;
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
