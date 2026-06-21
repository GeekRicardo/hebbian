import { Fragment, useEffect, useRef, useState, type ComponentType, type ReactNode } from "react";
import { Check, ChevronDown, Gauge, Map, Sparkles, Zap } from "lucide-react";
import { toast } from "sonner";

import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { HoverHint } from "@/desktop/ui/components/HoverHint";
import { COMPACT_TOOLBAR_BUTTON_CLASS } from "@/desktop/ui/lib/toolbarStyles";

/** 与后端 `agent_core::run_mode::RunMode::as_str()` 一一对应。 */
type RunMode = "Default" | "PlanMode" | "AutoMode" | "Yolo";

const MODE_OPTIONS: {
  value: RunMode;
  label: string;
  desc: string;
  icon: ComponentType<{ className?: string }>;
}[] = [
  {
    value: "Default",
    label: "默认",
    desc: "工作区内改文件直接执行，运行命令前会询问",
    icon: Gauge,
  },
  {
    value: "PlanMode",
    label: "计划模式",
    desc: "只读模式，先规划再动手",
    icon: Map,
  },
  {
    value: "AutoMode",
    label: "自动模式",
    desc: "让 AI 自己判断哪些操作可以放行",
    icon: Sparkles,
  },
  {
    value: "Yolo",
    label: "全速模式",
    desc: "全部自动执行、不打断，只拦最危险的不可逆操作",
    icon: Zap,
  },
];

/** 把后端 RunMode 字符串映射成展示给用户的中文 label。 */
export function runModeLabel(mode: string): string {
  return MODE_OPTIONS.find((o) => o.value === mode)?.label ?? mode;
}

function optionOf(mode: RunMode) {
  return MODE_OPTIONS.find((o) => o.value === mode) ?? MODE_OPTIONS[0];
}

interface Props {
  sessionId: string | null;
  /** 跑 run（streaming）时收成纯图标，hover 显示当前模式名；切换仍即时生效。 */
  compact?: boolean;
}

/**
 * 工具栏 chip：显示当前 [`RunMode`]，点击弹出下拉切换。
 *
 * 状态走后端 `RunModeState` 进程级 in-memory 表（架构 §4.4.3 / §8），重启回归
 * `Default`。本 chip 不订阅 session 变更，每次 sessionId 切换时拉一次最新值。
 *
 * 跑 run 中也允许切换：后端派发器实时读 `run_mode`，下一次模型请求即生效，所以
 * compact 态只缩小视觉占位，不禁用交互。
 */
export function RunModeChip({ sessionId, compact }: Props) {
  const [mode, setMode] = useState<RunMode>("Default");
  const [handsOff, setHandsOff] = useState(false);
  const [open, setOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    if (!sessionId) {
      setMode("Default");
      setHandsOff(false);
      return;
    }
    api
      .getRunMode(sessionId)
      .then((value) => {
        if (!cancelled) setMode(value as RunMode);
      })
      .catch(() => {});
    api
      .getForceAutomode(sessionId)
      .then((value) => {
        if (!cancelled) setHandsOff(value);
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
    if (!sessionId) {
      toast.error("当前没有打开的对话");
      return;
    }
    // 切到 AutoMode 不关闭面板——让用户能接着调右侧「全自动」开关。
    if (next !== "AutoMode") setOpen(false);
    if (next === mode) return;
    try {
      const applied = await api.setRunMode(sessionId, next);
      setMode(applied as RunMode);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  /** 切换 AutoMode 的「全自动」(hands-off) 子开关——run 中途也实时生效。 */
  async function toggleHandsOff(next: boolean) {
    if (!sessionId) {
      toast.error("当前没有打开的对话");
      return;
    }
    try {
      const applied = await api.setForceAutomode(sessionId, next);
      setHandsOff(applied);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  const current = optionOf(mode);
  const CurrentIcon = current.icon;
  const buttonDisabled = !sessionId;

  const trigger = (
    <button
      type="button"
      onClick={() => setOpen((v) => !v)}
      disabled={buttonDisabled}
      className={cn(
        compact
          ? COMPACT_TOOLBAR_BUTTON_CLASS
          : "h-8 rounded-md inline-flex items-center justify-center bg-transparent hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:pointer-events-none transition-colors gap-1 px-2 text-[11px] leading-none text-muted-foreground",
        open && "bg-muted text-foreground"
      )}
      title={compact ? undefined : "切换运行模式"}
    >
      <CurrentIcon className="h-4 w-4 shrink-0" />
      {!compact && (
        <>
          <span className="font-medium leading-none">{current.label}</span>
          <ChevronDown className="h-3 w-3 shrink-0 opacity-60" />
        </>
      )}
    </button>
  );

  return (
    <div className="relative" ref={wrapperRef}>
      {compact ? (
        <HoverHint hint={`运行模式：${current.label}`} side="top" align="start">
          {trigger}
        </HoverHint>
      ) : (
        trigger
      )}
      {open && (
        <div
          onClick={(e) => e.stopPropagation()}
          className="absolute bottom-full left-0 mb-1 min-w-[280px] rounded-lg border border-border bg-card shadow-lg z-[90] overflow-hidden animate-slide-up"
        >
          {MODE_OPTIONS.map((o, i) => {
            const Icon = o.icon;
            const selected = o.value === mode;
            const isAuto = o.value === "AutoMode";
            return (
              <Fragment key={o.value}>
                {i > 0 && <div className="h-px bg-border mx-3" />}
                <button
                  type="button"
                  onClick={() => pick(o.value)}
                  className={cn(
                    "w-full flex items-start gap-2.5 px-3 py-2 text-left transition-colors",
                    selected ? "bg-accent" : "hover:bg-accent/40"
                  )}
                >
                  <Icon
                    className={cn(
                      "h-4 w-4 shrink-0 mt-0.5",
                      selected ? "text-foreground" : "text-muted-foreground"
                    )}
                  />
                  <div className="flex flex-col gap-0.5 min-w-0 flex-1">
                    <span className="text-sm font-medium leading-none">
                      {o.label}
                    </span>
                    <span className="text-xs text-muted-foreground leading-snug">
                      {o.desc}
                    </span>
                    {/* AutoMode 行内「问我 ↔ 全自动」分段开关：仅选中 AutoMode 时展开，
                        实时切换 hands-off（run 中途也生效）。 */}
                    {isAuto && selected && (
                      <div
                        className="mt-1.5 inline-flex items-center gap-0.5 rounded-md bg-muted/60 p-0.5 self-start"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <SegBtn
                          active={!handsOff}
                          onClick={() => toggleHandsOff(false)}
                        >
                          问我
                        </SegBtn>
                        <SegBtn
                          active={handsOff}
                          onClick={() => toggleHandsOff(true)}
                        >
                          全自动
                        </SegBtn>
                      </div>
                    )}
                  </div>
                  {selected && (
                    <Check className="h-3.5 w-3.5 shrink-0 mt-0.5 text-foreground" />
                  )}
                </button>
              </Fragment>
            );
          })}
        </div>
      )}
    </div>
  );
}

/** AutoMode 行内分段开关的单个按钮。 */
function SegBtn({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "px-2 py-0.5 rounded text-[11px] font-medium leading-none transition-colors",
        active
          ? "bg-background text-foreground shadow-sm"
          : "text-muted-foreground hover:text-foreground"
      )}
    >
      {children}
    </button>
  );
}
