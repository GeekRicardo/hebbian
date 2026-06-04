import { useEffect, useRef, useState } from "react";
import { AlertTriangle, Info, X, XCircle } from "lucide-react";

import { useToastStore, type ModelToast } from "@/desktop/ui/store/useToastStore";

/** 无 hover 时单条 toast 的自动关闭时长（ms）。 */
const AUTO_DISMISS_MS = 8000;

const LEVEL_STYLE: Record<ModelToast["level"], { icon: typeof Info; cls: string }> = {
  info: { icon: Info, cls: "border-border bg-background/95 text-foreground" },
  warn: {
    icon: AlertTriangle,
    cls: "border-amber-500/40 bg-amber-50/95 text-amber-900 dark:bg-amber-950/80 dark:text-amber-100",
  },
  error: {
    icon: XCircle,
    cls: "border-red-500/40 bg-red-50/95 text-red-900 dark:bg-red-950/80 dark:text-red-100",
  },
};

function ToastCard({ toast }: { toast: ModelToast }) {
  const dismiss = useToastStore((s) => s.dismiss);
  // 入场动画：先挂载在屏幕右侧外，下一帧滑入。
  const [shown, setShown] = useState(false);
  const hovering = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const armTimer = () => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      if (!hovering.current) dismiss(toast.id);
    }, AUTO_DISMISS_MS);
  };

  useEffect(() => {
    const raf = requestAnimationFrame(() => setShown(true));
    armTimer();
    return () => {
      cancelAnimationFrame(raf);
      if (timer.current) clearTimeout(timer.current);
    };
    // toast.id 稳定，仅挂载时跑一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const { icon: Icon, cls } = LEVEL_STYLE[toast.level];

  return (
    <div
      onMouseEnter={() => {
        hovering.current = true;
        if (timer.current) clearTimeout(timer.current);
      }}
      onMouseLeave={() => {
        hovering.current = false;
        armTimer();
      }}
      className={`pointer-events-auto flex w-[min(28rem,calc(100vw-3rem))] items-start gap-2 rounded-lg border px-3 py-2 text-sm shadow-lg backdrop-blur transition-all duration-300 ease-out ${cls} ${
        shown ? "translate-x-0 opacity-100" : "translate-x-full opacity-0"
      }`}
    >
      <Icon className="mt-0.5 h-4 w-4 shrink-0" />
      <span className="min-w-0 flex-1 break-words leading-snug">{toast.message}</span>
      <button
        type="button"
        onClick={() => dismiss(toast.id)}
        className="mt-0.5 shrink-0 rounded p-0.5 opacity-60 transition-opacity hover:opacity-100"
        aria-label="关闭"
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}

/**
 * 输入框上方右侧的模型异常退出 toast 区（架构 §7.3）。
 * 容器底对齐 + 右对齐：新消息追加在底部，旧的被往上挤；每条右→左滑入；hover 暂停关闭。
 */
export function ToastRegion() {
  const toasts = useToastStore((s) => s.toasts);
  if (toasts.length === 0) return null;
  return (
    <div className="pointer-events-none flex flex-col items-end gap-2 px-3 pb-2">
      {toasts.map((t) => (
        <ToastCard key={t.id} toast={t} />
      ))}
    </div>
  );
}
