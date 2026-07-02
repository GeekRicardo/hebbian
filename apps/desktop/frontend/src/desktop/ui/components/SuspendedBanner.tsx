import { useEffect, useState } from "react";
import { Pause } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import type { SuspendedInfo } from "@/desktop/ui/store/useStore";

/**
 * Run 挂起横幅（架构 §4.12）：显示「已暂停 X 分 Y 秒」+ 下一步（cron 倒计时 / 等后台任务）。
 * 自带每秒 tick 刷新倒计时。侧栏 BackgroundTaskTab 与主时间线 ChatView 共用同一份，
 * 避免两处各写一份倒计时逻辑漂移。`variant` 只切边距：主时间线用居中窄卡，
 * 侧栏用贴边横幅。
 */
export function SuspendedBanner({
  suspended,
  variant = "sidebar",
}: {
  suspended: SuspendedInfo;
  variant?: "sidebar" | "timeline";
}) {
  const [now, setNow] = useState<number>(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  const elapsedSec = Math.max(0, Math.round((now - suspended.suspendedAtMs) / 1000));
  const remainingSec =
    suspended.resumesAtMs != null
      ? Math.max(0, Math.round((suspended.resumesAtMs - now) / 1000))
      : null;
  const nextLine =
    suspended.reason === "background_task"
      ? `等 ${suspended.waitingForTaskIds.join("、") || "后台任务"}完成`
      : suspended.reason === "cron"
        ? remainingSec != null
          ? `${formatDuration(remainingSec)}后自动继续`
          : "定时到点后自动继续"
        : "等待唤醒";

  return (
    <div
      className={cn(
        "flex items-center gap-1.5 rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5",
        variant === "timeline" ? "mx-auto my-3 max-w-2xl" : "m-2"
      )}
    >
      <Pause className="h-3 w-3 shrink-0 text-amber-600 dark:text-amber-400" />
      <div className="min-w-0 text-[11px] leading-tight">
        <div className="font-medium text-amber-700 dark:text-amber-300">
          已暂停 {formatDuration(elapsedSec)}
        </div>
        <div className="mt-0.5 text-amber-700/80 dark:text-amber-300/80">{nextLine}</div>
      </div>
    </div>
  );
}

function formatDuration(totalSeconds: number): string {
  if (totalSeconds <= 0) return "0 秒";
  const days = Math.floor(totalSeconds / 86400);
  const hours = Math.floor((totalSeconds % 86400) / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const parts: string[] = [];
  if (days > 0) parts.push(`${days} 天`);
  if (hours > 0) parts.push(`${hours} 时`);
  if (minutes > 0) parts.push(`${minutes} 分`);
  if (seconds > 0 || parts.length === 0) parts.push(`${seconds} 秒`);
  return parts.join(" ");
}
