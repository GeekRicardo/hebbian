import { CircleCheck, CircleSlash, ArrowRightCircle } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";

/**
 * 一次 `//goal` 裁决结果，渲染成带彩色竖线的结果块（类似 markdown blockquote 的 `|`，
 * 只有竖线带色）。goal judge 在 turn 收尾判完成条件是否达成后落一条 marker，由此渲染。
 *
 * - achieved（达成）→ 绿
 * - impossible（判不可达）→ 橙
 * - progress（续跑一轮）→ 蓝
 *
 * 结构：彩色竖线 + 粗体标题 + judge 的理由 + 底部一行小字标明是哪个目标。
 */
export function GoalResultSummary({
  kind,
  condition,
  reason,
  iteration,
}: {
  kind: "achieved" | "impossible" | "progress";
  condition: string;
  reason: string;
  iteration: number;
}) {
  const style = {
    achieved: {
      bar: "bg-emerald-500",
      icon: <CircleCheck className="h-3.5 w-3.5 text-emerald-500" />,
      title: "目标达成",
      titleColor: "text-emerald-600 dark:text-emerald-400",
    },
    impossible: {
      bar: "bg-amber-500",
      icon: <CircleSlash className="h-3.5 w-3.5 text-amber-500" />,
      title: "目标无法达成",
      titleColor: "text-amber-600 dark:text-amber-400",
    },
    progress: {
      bar: "bg-sky-500",
      icon: <ArrowRightCircle className="h-3.5 w-3.5 text-sky-500" />,
      title: `继续推进目标（第 ${iteration} 轮）`,
      titleColor: "text-sky-600 dark:text-sky-400",
    },
  }[kind];

  return (
    <div className="flex w-fit max-w-full gap-2 text-[12px]">
      {/* 彩色竖线：类似 markdown blockquote 的 |，只有它带色 */}
      <div className={cn("w-0.5 shrink-0 rounded-full", style.bar)} />
      <div className="min-w-0 py-0.5">
        <div className={cn("flex items-center gap-1.5 font-medium", style.titleColor)}>
          {style.icon}
          <span>{style.title}</span>
        </div>
        {reason && (
          <div className="mt-0.5 whitespace-pre-wrap break-words text-foreground/80">
            {reason}
          </div>
        )}
        <div className="mt-1 truncate text-[10px] text-muted-foreground/70">
          目标：{condition}
        </div>
      </div>
    </div>
  );
}