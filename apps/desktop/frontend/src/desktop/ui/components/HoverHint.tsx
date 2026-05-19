import { useEffect, useRef, useState, type ReactNode } from "react";
import { cn } from "@/desktop/ui/lib/utils";

interface Props {
  hint: ReactNode;
  side?: "top" | "bottom";
  align?: "start" | "center" | "end";
  className?: string;
  keepOpenDelayMs?: number;
  children: ReactNode;
}

/**
 * 鼠标移入即显示的提示气泡，替代 HTML title 的浏览器原生 tooltip
 * （原生 title 有 1~2 秒延迟且无法配置，体感"等很久才出来"）。
 * hint 节点可选中文本，鼠标移出后延迟关闭，方便复制长路径。
 */
export function HoverHint({
  hint,
  side = "top",
  align = "center",
  className,
  keepOpenDelayMs = 50,
  children,
}: Props) {
  const [open, setOpen] = useState(false);
  const closeTimerRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (closeTimerRef.current != null) {
        window.clearTimeout(closeTimerRef.current);
      }
    };
  }, []);

  function openHint() {
    if (closeTimerRef.current != null) {
      window.clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
    setOpen(true);
  }

  function closeHint() {
    if (closeTimerRef.current != null) {
      window.clearTimeout(closeTimerRef.current);
    }
    closeTimerRef.current = window.setTimeout(() => {
      setOpen(false);
      closeTimerRef.current = null;
    }, keepOpenDelayMs);
  }

  if (hint == null || hint === "") return <>{children}</>;

  return (
    <span
      className={cn("relative inline-flex", className)}
      onMouseEnter={openHint}
      onMouseLeave={closeHint}
    >
      {children}
      {open && (
        <span
          role="tooltip"
          className={cn(
            "absolute z-50 px-2 py-1 rounded-md border border-border bg-card text-foreground text-[11px] leading-relaxed shadow-md whitespace-pre-wrap break-words max-w-[320px] w-max select-text pointer-events-auto",
            side === "top" ? "bottom-full mb-1" : "top-full mt-1",
            align === "start" && "left-0",
            align === "center" && "left-1/2 -translate-x-1/2",
            align === "end" && "right-0"
          )}
          onMouseEnter={openHint}
          onMouseLeave={closeHint}
        >
          {hint}
        </span>
      )}
    </span>
  );
}
