import { useEffect, useRef, useState } from "react";
import { Slash } from "lucide-react";

import { cn } from "@/desktop/ui/lib/utils";
import {
  slashCommandCatalog,
  type SlashCommandMeta,
} from "@/desktop/ui/lib/slashCommands";

interface Props {
  disabled?: boolean;
  /** 用户点中一条命令时回调；调用方把 `//<name> ` 写入输入框并 focus 末尾。 */
  onPick: (cmd: SlashCommandMeta) => void;
}

/**
 * 工具栏按钮：点击弹出注册的 `//` 命令清单。
 *
 * 按 §8 的"前端拦截"原则，本组件只负责"把命令模板写到输入框"——真正派发由
 * `dispatchSlashCommand` 在 ChatInput 的 submit 路径完成。
 */
export function SlashCommandButton({ disabled, onPick }: Props) {
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

  return (
    <div className="relative" ref={wrapperRef}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={disabled}
        className={cn(
          "h-8 w-8 rounded-md inline-flex items-center justify-center bg-transparent text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:pointer-events-none",
          open && "bg-muted text-foreground"
        )}
        title="插入命令"
      >
        <Slash className="w-4 h-4" />
      </button>
      {open && (
        <div
          onClick={(e) => e.stopPropagation()}
          className="absolute bottom-full left-0 mb-1 min-w-[280px] rounded-lg border border-border bg-card shadow-lg z-[90] overflow-hidden animate-slide-up"
        >
          {slashCommandCatalog.map((cmd) => (
            <button
              key={cmd.name}
              type="button"
              onClick={() => {
                setOpen(false);
                onPick(cmd);
              }}
              className="w-full flex items-center justify-between gap-3 px-3 py-2 text-sm hover:bg-accent text-left"
            >
              <div className="flex flex-col">
                <span className="font-mono text-foreground">
                  //{cmd.name}{" "}
                  <span className="text-muted-foreground">{cmd.args}</span>
                </span>
                <span className="text-xs text-muted-foreground">
                  {cmd.desc}
                </span>
              </div>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
