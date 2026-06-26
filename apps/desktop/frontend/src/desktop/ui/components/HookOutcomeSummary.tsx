import { useState } from "react";
import {
  CircleCheck,
  CircleAlert,
  CircleSlash,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";

/**
 * 一次 Stop hook（cargo check / tsc 等后置 verify）执行结果的一行低调摘要。
 * passed=绿勾「检查通过」；injected=橙「检查未过，已让 AI 修复」可展开看详情；
 * blocked=红「被拦下」。区别于 goal 块，视觉更轻，不抢戏。
 */
export function HookOutcomeSummary({
  event,
  status,
  detail,
}: {
  event: string;
  status: "passed" | "injected" | "blocked";
  detail: string;
}) {
  const [open, setOpen] = useState(false);
  const style = {
    passed: {
      icon: <CircleCheck className="h-3.5 w-3.5 text-emerald-500" />,
      label: `${event} 检查通过`,
      color: "text-emerald-600/90 dark:text-emerald-400/90",
    },
    injected: {
      icon: <CircleAlert className="h-3.5 w-3.5 text-amber-500" />,
      label: `${event} 检查未过，已让 AI 修复`,
      color: "text-amber-600/90 dark:text-amber-400/90",
    },
    blocked: {
      icon: <CircleSlash className="h-3.5 w-3.5 text-red-500" />,
      label: `${event} 被拦下`,
      color: "text-red-600/90 dark:text-red-400/90",
    },
  }[status];
  const expandable = !!detail.trim();

  return (
    <div className="w-fit max-w-full text-[11px]">
      <button
        type="button"
        disabled={!expandable}
        onClick={() => setOpen((v) => !v)}
        className={cn(
          "flex items-center gap-1.5",
          style.color,
          expandable ? "cursor-pointer" : "cursor-default"
        )}
      >
        {style.icon}
        <span>{style.label}</span>
        {expandable &&
          (open ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronRight className="h-3 w-3" />
          ))}
      </button>
      {open && expandable && (
        <pre className="mt-1 max-h-[200px] overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-background/60 p-1.5 text-[11px] text-foreground/80">
          {detail}
        </pre>
      )}
    </div>
  );
}