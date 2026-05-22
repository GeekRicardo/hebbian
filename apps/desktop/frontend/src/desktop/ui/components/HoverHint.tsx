import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
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
 *
 * 浮层 portal 到 body 顶层 + position fixed：避免被任何祖先 overflow:hidden
 * 裁掉（典型场景：工具调用卡片 ToolCallTimeline 的 rounded 卡片需要 overflow-hidden
 * 让圆角生效，但 hint 又必须能溢出卡片边界）。
 *
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
  const [rect, setRect] = useState<DOMRect | null>(null);
  const anchorRef = useRef<HTMLSpanElement>(null);
  const closeTimerRef = useRef<number | null>(null);

  const updateRect = useCallback(() => {
    if (anchorRef.current) {
      setRect(anchorRef.current.getBoundingClientRect());
    }
  }, []);

  useEffect(() => {
    return () => {
      if (closeTimerRef.current != null) {
        window.clearTimeout(closeTimerRef.current);
      }
    };
  }, []);

  // 打开时锁定一次位置；滚动 / resize 期间跟随更新
  useLayoutEffect(() => {
    if (!open) return;
    updateRect();
    window.addEventListener("scroll", updateRect, true);
    window.addEventListener("resize", updateRect);
    return () => {
      window.removeEventListener("scroll", updateRect, true);
      window.removeEventListener("resize", updateRect);
    };
  }, [open, updateRect]);

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

  // fixed 定位坐标 + transform：side 控制上下，align 控制左右锚点对齐
  const top = rect ? (side === "top" ? rect.top - 4 : rect.bottom + 4) : 0;
  const left = rect
    ? align === "start"
      ? rect.left
      : align === "center"
        ? rect.left + rect.width / 2
        : rect.right
    : 0;
  const translateX =
    align === "start" ? "0" : align === "center" ? "-50%" : "-100%";
  const translateY = side === "top" ? "-100%" : "0";

  return (
    <>
      <span
        ref={anchorRef}
        className={cn("relative inline-flex", className)}
        onMouseEnter={openHint}
        onMouseLeave={closeHint}
      >
        {children}
      </span>
      {open && rect && createPortal(
        <span
          role="tooltip"
          style={{
            position: "fixed",
            top,
            left,
            transform: `translate(${translateX}, ${translateY})`,
          }}
          className="z-[100] px-2 py-1 rounded-md border border-border bg-card text-foreground text-[11px] leading-relaxed shadow-md whitespace-pre-wrap break-words max-w-[320px] w-max select-text pointer-events-auto"
          onMouseEnter={openHint}
          onMouseLeave={closeHint}
        >
          {hint}
        </span>,
        document.body,
      )}
    </>
  );
}
