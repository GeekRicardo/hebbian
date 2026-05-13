import { useState, type ReactNode } from "react";
import { cn } from "@/desktop/ui/lib/utils";

interface Props {
  hint: ReactNode;
  side?: "top" | "bottom";
  align?: "start" | "center" | "end";
  className?: string;
  children: ReactNode;
}

/**
 * 鼠标移入即显示的提示气泡，替代 HTML title 的浏览器原生 tooltip
 * （原生 title 有 1~2 秒延迟且无法配置，体感"等很久才出来"）。
 * hint 节点 pointer-events:none，不会拦截子元素的点击。
 */
export function HoverHint({
  hint,
  side = "top",
  align = "center",
  className,
  children,
}: Props) {
  const [open, setOpen] = useState(false);
  if (hint == null || hint === "") return <>{children}</>;
  return (
    <span
      className={cn("relative inline-flex", className)}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      {children}
      {open && (
        <span
          role="tooltip"
          className={cn(
            "pointer-events-none absolute z-50 px-2 py-1 rounded-md border border-border bg-card text-foreground text-[11px] leading-relaxed shadow-md whitespace-pre-wrap break-words max-w-[320px] w-max",
            side === "top" ? "bottom-full mb-1" : "top-full mt-1",
            align === "start" && "left-0",
            align === "center" && "left-1/2 -translate-x-1/2",
            align === "end" && "right-0"
          )}
        >
          {hint}
        </span>
      )}
    </span>
  );
}
