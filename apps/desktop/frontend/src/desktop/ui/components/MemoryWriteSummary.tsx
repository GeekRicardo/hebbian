import { useState } from "react";
import { NotebookPen, ChevronDown, ChevronRight } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import type { MemoryWriteItem } from "@/desktop/ui/types";

/**
 * 一个 Run 跑完后，后台记忆抽取写入若干条记忆时，在会话末尾渲染的低调摘要行
 * （架构 §4.14）。一行小字「本轮写入 N 条记忆 ▼」，点开展开明细——最多 5 条高度，
 * 多余在区域内滚动。区别于工具卡片，视觉上不抢戏。
 */
export function MemoryWriteSummary({ items }: { items: MemoryWriteItem[] }) {
  const [open, setOpen] = useState(false);
  if (items.length === 0) return null;

  return (
    <div className="w-fit max-w-full">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1 text-xs leading-4 text-muted-foreground/80 hover:text-muted-foreground"
      >
        <NotebookPen className="h-3.5 w-3.5" />
        <span>本轮写入 {items.length} 条记忆</span>
        {open ? (
          <ChevronDown className="h-3.5 w-3.5" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5" />
        )}
      </button>
      {open && (
        <div className="mt-1 max-h-[150px] overflow-y-auto rounded-md border border-border bg-background/60 p-1.5">
          {items.map((it) => (
            <div
              key={it.id}
              className="flex items-center gap-2 px-1 py-1 text-[12px]"
            >
              <span
                className={cn(
                  "shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium",
                  it.scope === "project"
                    ? "bg-amber-500/15 text-amber-600 dark:text-amber-400"
                    : "bg-sky-500/15 text-sky-600 dark:text-sky-400"
                )}
              >
                {it.scope === "project" ? "项目" : "全局"}
              </span>
              <span className="min-w-0 truncate text-foreground/90">
                {it.summary}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
