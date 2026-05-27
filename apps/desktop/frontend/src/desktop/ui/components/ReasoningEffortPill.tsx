import { Fragment, useEffect, useRef, useState } from "react";
import { ChevronDown, Flame } from "lucide-react";
import { toast } from "sonner";

import { useStore } from "@/desktop/ui/store/useStore";
import {
  DEFAULT_REASONING,
  REASONING_EFFORT_LABEL,
  REASONING_EFFORT_ORDER,
  effortDisplay,
  modelSupportsReasoning,
} from "@/desktop/ui/lib/reasoning";
import { cn } from "@/desktop/ui/lib/utils";
import type { ReasoningEffort } from "@/desktop/ui/types";

/**
 * 抽屉里的思考强度 chip：点击向上弹出菜单选择 low / medium / high / extra。
 *
 * 与 ModelPicker popup 里的 ReasoningControls 共享同一份 store 数据（SSoT 不冲突）。
 * 不支持 reasoning 的模型 / thinking 已关闭时，分别按情况隐藏或 disabled。
 */
export function ReasoningEffortPill() {
  const session = useStore((s) => s.currentSession);
  const providers = useStore((s) => s.providersFile.providers);
  const setReasoning = useStore((s) => s.setReasoning);

  const [open, setOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);

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

  if (!session) return null;
  const provider = providers.find((p) => p.id === session.provider_id);
  if (!provider) return null;
  if (!modelSupportsReasoning(provider.kind, session.model)) return null;

  const reasoning = session.reasoning ?? DEFAULT_REASONING;
  const enabled = reasoning.enabled ?? true;
  const effort = reasoning.effort ?? "extra";

  function pick(next: ReasoningEffort) {
    setOpen(false);
    if (next === effort) return;
    void setReasoning({ ...reasoning, effort: next }).catch((e: unknown) => {
      toast.error(e instanceof Error ? e.message : String(e));
    });
  }

  return (
    <div className="relative" ref={wrapperRef}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={!enabled}
        className={cn(
          "h-8 inline-flex items-center gap-1 rounded-md px-2 text-xs",
          "bg-transparent text-muted-foreground hover:bg-muted hover:text-foreground transition-colors",
          "disabled:opacity-40 disabled:pointer-events-none",
          open && "bg-muted text-foreground"
        )}
        title={
          enabled
            ? "切换思考强度"
            : "thinking 已关闭，可在模型菜单里开启"
        }
      >
        <Flame className="h-3.5 w-3.5" />
        <span className="font-medium">{REASONING_EFFORT_LABEL[effort]}</span>
        <ChevronDown className="w-3 h-3 opacity-60" />
      </button>
      {open && (
        <div
          onClick={(e) => e.stopPropagation()}
          className="absolute bottom-full left-0 mb-1 min-w-[160px] rounded-lg border border-border bg-card shadow-lg z-[90] overflow-hidden animate-slide-up"
        >
          {REASONING_EFFORT_ORDER.map((level, i) => {
            const real = effortDisplay(provider.kind, session.model, level);
            return (
              <Fragment key={level}>
                {i > 0 && <div className="h-px bg-border mx-3" />}
                <button
                  type="button"
                  onClick={() => pick(level)}
                  className={cn(
                    "w-full flex items-center justify-between gap-3 px-3 py-2 text-sm hover:bg-accent text-left",
                    level === effort && "bg-accent/60"
                  )}
                  title={`实际发送：${real}`}
                >
                  <span className="font-medium">
                    {REASONING_EFFORT_LABEL[level]}
                  </span>
                  <span className="text-[10px] text-muted-foreground tabular-nums">
                    {real}
                  </span>
                </button>
              </Fragment>
            );
          })}
        </div>
      )}
    </div>
  );
}
