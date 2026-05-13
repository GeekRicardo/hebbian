import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import {
  Pause,
  Play,
  Clock,
  Terminal,
  X,
  Square,
  AlertCircle,
} from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import type { SessionBackgroundReport, BackgroundTaskInfo } from "@/desktop/ui/types";

/**
 * 架构 §4.12.9：BackgroundTask 浮动框。
 *
 * 与 FloatingTaskPanel 并列在右侧（top-[110px]），展示本 session 当前的：
 * - bg shells（running / exited / killed / failed）
 * - 还在等的 cron 唤醒
 * - 挂起态（若 agent_loop 已 emit RunSuspended）
 *
 * **session-scoped**——只看当前 session 的后台任务，跨 session 互不可见
 * （架构 §4.12.2 修订）。
 *
 * 三种状态:
 * - 没 shells + 没 cron + 未挂起 → 整个组件不渲染
 * - 有内容 → 默认展开；点 X 折叠为药丸
 * - 折叠 → 显示「N 后台 / 挂起 12s」药丸；点开恢复
 */
export function BackgroundTaskPanel() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const suspended = useStore((s) => s.suspended);
  const [report, setReport] = useState<SessionBackgroundReport | null>(null);
  const [collapsed, setCollapsed] = useState(false);
  const [now, setNow] = useState<number>(Date.now());
  const lastSessionIdRef = useRef<string | null>(null);

  // 切换 session 时重置面板（包括展开 / 折叠状态）
  useEffect(() => {
    if (lastSessionIdRef.current !== sessionId) {
      lastSessionIdRef.current = sessionId;
      setReport(null);
      setCollapsed(false);
    }
  }, [sessionId]);

  // 轮询当前 session 的后台情况；3s 一次足够（bg 任务变化粒度本来就是秒级）
  useEffect(() => {
    if (!sessionId) {
      setReport(null);
      return;
    }
    let cancelled = false;
    async function refresh() {
      try {
        const r = await api.listBackgroundTasks(sessionId!);
        if (!cancelled) setReport(r);
      } catch {
        // 静默——典型失败是 session 已删；下次轮询会自动消失
      }
    }
    refresh();
    const t = setInterval(refresh, 3000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, [sessionId]);

  // 滴答：让"已挂起 N 秒"和 cron 倒计时实时更新
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  const runningShells = (report?.shells ?? []).filter((s) => s.state === "running");
  const exitedShells = (report?.shells ?? []).filter((s) => s.state !== "running");
  const pendingCrons = report?.pending_crons ?? [];
  // 上次中断：有 checkpoint 但调度器没在等任何事件（典型是进程重启后），
  // 且当前没在 live 挂起态。这种情况下 wakeup 不会自动触发，提示用户发消息继续。
  const orphanedCheckpoint =
    !!report?.has_suspended_checkpoint &&
    !suspended &&
    runningShells.length === 0 &&
    pendingCrons.length === 0;
  const hasAny =
    !!suspended ||
    orphanedCheckpoint ||
    runningShells.length > 0 ||
    pendingCrons.length > 0 ||
    exitedShells.length > 0;

  if (!sessionId || !hasAny) return null;

  async function killShell(taskId: string) {
    if (!sessionId) return;
    try {
      const state = await api.killBackgroundTask(sessionId, taskId);
      toast.success(`已停止 ${taskId}（${state}）`);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  const suspendedElapsedSec = suspended
    ? Math.max(0, Math.round((now - suspended.suspendedAtMs) / 1000))
    : 0;

  const summary = (() => {
    const parts: string[] = [];
    if (runningShells.length) parts.push(`${runningShells.length} 后台`);
    if (pendingCrons.length) parts.push(`${pendingCrons.length} 定时`);
    if (suspended) parts.push(`挂起 ${suspendedElapsedSec}s`);
    if (orphanedCheckpoint) parts.push("上次中断");
    return parts.join(" · ") || "后台任务";
  })();

  if (collapsed) {
    return (
      <button
        type="button"
        onClick={() => setCollapsed(false)}
        className="pointer-events-auto absolute right-4 top-[110px] z-30 inline-flex items-center gap-1.5 rounded-full border border-border bg-background/95 px-2.5 py-1 text-[11px] text-muted-foreground shadow-sm backdrop-blur transition-colors hover:bg-background hover:text-foreground"
        title="展开后台任务面板"
      >
        {suspended ? (
          <Pause className="h-3 w-3" />
        ) : (
          <Terminal className="h-3 w-3" />
        )}
        <span>{summary}</span>
      </button>
    );
  }

  return (
    <div className="pointer-events-auto absolute right-4 top-[110px] z-30 w-[280px] overflow-hidden rounded-lg border border-border bg-background/95 shadow-md backdrop-blur">
      <div className="flex items-center justify-between gap-2 border-b border-border bg-muted/30 px-2.5 py-1.5">
        <div className="min-w-0">
          <div className="text-[11px] font-medium leading-tight">后台任务</div>
          <div className="mt-0.5 text-[10px] text-muted-foreground">{summary}</div>
        </div>
        <button
          type="button"
          onClick={() => setCollapsed(true)}
          className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
          title="收起"
          aria-label="收起后台任务面板"
        >
          <X className="h-3 w-3" />
        </button>
      </div>

      <div className="max-h-[60vh] overflow-auto px-2.5 py-2">
        {suspended && (
          <div className="mb-2 flex items-start gap-1.5 rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5">
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
          <div className="mb-2 flex items-start gap-1.5 rounded-md border border-orange-500/30 bg-orange-500/10 px-2 py-1.5">
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

        {runningShells.length > 0 && (
          <ShellSection
            title="运行中"
            shells={runningShells}
            highlight
            onKill={killShell}
          />
        )}
        {pendingCrons.length > 0 && (
          <div className="mt-1">
            <div className="mb-1 text-[10px] uppercase tracking-wide text-muted-foreground">
              定时唤醒
            </div>
            {pendingCrons.map((c) => (
              <div
                key={`${c.run_id}-${c.fire_at_ms}`}
                className="flex items-start gap-1.5 rounded-md border border-border bg-muted/20 px-2 py-1 text-[11px] mt-1"
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
        {exitedShells.length > 0 && (
          <ShellSection title="已结束" shells={exitedShells} highlight={false} />
        )}
      </div>
    </div>
  );
}

function ShellSection({
  title,
  shells,
  highlight,
  onKill,
}: {
  title: string;
  shells: BackgroundTaskInfo[];
  highlight: boolean;
  onKill?: (taskId: string) => void;
}) {
  return (
    <div className="mt-1">
      <div className="mb-1 text-[10px] uppercase tracking-wide text-muted-foreground">
        {title}
      </div>
      {shells.map((s) => (
        <div
          key={s.task_id}
          className={cn(
            "flex items-start gap-1.5 rounded-md px-2 py-1 text-[11px] mt-1 border",
            highlight
              ? "border-primary/30 bg-primary/5"
              : "border-border bg-muted/20"
          )}
        >
          {highlight ? (
            <Play className="mt-0.5 h-3 w-3 shrink-0 text-primary" />
          ) : (
            <Terminal className="mt-0.5 h-3 w-3 shrink-0 text-muted-foreground" />
          )}
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-1">
              <code className="font-mono text-[10px] text-muted-foreground shrink-0">
                {s.task_id}
              </code>
              {!highlight && (
                <span className="rounded bg-muted px-1 text-[9px] uppercase text-muted-foreground">
                  {s.state}
                </span>
              )}
            </div>
            <div className="mt-0.5 truncate font-mono text-foreground/90" title={s.command}>
              {s.command}
            </div>
            <div className="mt-0.5 text-[10px] text-muted-foreground">
              {s.elapsed_secs}s
            </div>
          </div>
          {onKill && highlight && (
            <button
              type="button"
              onClick={() => onKill(s.task_id)}
              className="shrink-0 inline-flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
              title={`停止 ${s.task_id}`}
              aria-label={`停止 ${s.task_id}`}
            >
              <Square className="h-3 w-3" />
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
