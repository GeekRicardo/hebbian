import { Fragment, useEffect, useRef, useState } from "react";
import { ChevronDown, Gauge } from "lucide-react";
import { toast } from "sonner";

import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";

/** 与后端 `agent_core::run_mode::RunMode::as_str()` 一一对应。 */
type RunMode = "AskBeforeEdits" | "EditAutomatically" | "PlanMode" | "AutoMode";

const MODE_OPTIONS: { value: RunMode; label: string; desc: string }[] = [
  {
    value: "AskBeforeEdits",
    label: "编辑前询问",
    desc: "修改文件或运行命令前都会询问",
  },
  {
    value: "EditAutomatically",
    label: "编辑自动执行",
    desc: "直接修改文件，运行命令前仍会询问",
  },
  {
    value: "PlanMode",
    label: "计划模式",
    desc: "只读模式，先规划再动手",
  },
  {
    value: "AutoMode",
    label: "自动模式",
    desc: "让 AI 自己判断哪些操作可以放行",
  },
];

/** 把后端 RunMode 字符串映射成展示给用户的中文 label。 */
export function runModeLabel(mode: string): string {
  return MODE_OPTIONS.find((o) => o.value === mode)?.label ?? mode;
}

function labelOf(mode: RunMode): string {
  return runModeLabel(mode);
}

interface Props {
  sessionId: string | null;
  disabled?: boolean;
}

/**
 * 工具栏 chip：显示当前 [`RunMode`]，点击弹出下拉切换。
 *
 * 状态走后端 `RunModeState` 进程级 in-memory 表（架构 §4.4.3 / §8），重启回归
 * `AskBeforeEdits`。本 chip 不订阅 session 变更，每次 sessionId 切换时拉一次最新值。
 */
export function RunModeChip({ sessionId, disabled }: Props) {
  const [mode, setMode] = useState<RunMode>("AskBeforeEdits");
  const [open, setOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    if (!sessionId) {
      setMode("AskBeforeEdits");
      return;
    }
    api
      .getRunMode(sessionId)
      .then((value) => {
        if (!cancelled) setMode(value as RunMode);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  useEffect(() => {
    if (!open) return;
    function onClick(event: MouseEvent) {
      if (
        wrapperRef.current &&
        !wrapperRef.current.contains(event.target as Node)
      ) {
        setOpen(false);
      }
    }
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [open]);

  async function pick(next: RunMode) {
    setOpen(false);
    if (!sessionId) {
      toast.error("当前没有打开的对话");
      return;
    }
    if (next === mode) return;
    try {
      const applied = await api.setRunMode(sessionId, next);
      setMode(applied as RunMode);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  const buttonDisabled = disabled || !sessionId;

  return (
    <div className="relative" ref={wrapperRef}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={buttonDisabled}
        className={cn(
          "h-8 inline-flex items-center gap-1 rounded-md px-2 text-xs bg-transparent text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:pointer-events-none",
          open && "bg-muted text-foreground"
        )}
        title="切换运行模式"
      >
        <Gauge className="w-3.5 h-3.5" />
        <span className="font-medium">{labelOf(mode)}</span>
        <ChevronDown className="w-3 h-3 opacity-60" />
      </button>
      {open && (
        <div
          onClick={(e) => e.stopPropagation()}
          className="absolute bottom-full left-0 mb-1 min-w-[260px] rounded-lg border border-border bg-card shadow-lg z-[90] overflow-hidden animate-slide-up"
        >
          {/* hairline 用 `mx-3` 缩进，两端留 12px；hover bg 全宽 */}
          {MODE_OPTIONS.map((o, i) => (
            <Fragment key={o.value}>
              {i > 0 && <div className="h-px bg-border mx-3" />}
              <button
                type="button"
                onClick={() => pick(o.value)}
                className={cn(
                  "w-full flex flex-col gap-0.5 px-3 py-2 text-sm hover:bg-accent text-left",
                  o.value === mode && "bg-accent/60"
                )}
              >
                <span className="font-medium">{o.label}</span>
                <span className="text-xs text-muted-foreground">{o.desc}</span>
              </button>
            </Fragment>
          ))}
        </div>
      )}
    </div>
  );
}
