import { Quote } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import type { MemoryWriteItem } from "@/desktop/ui/types";

/**
 * 本轮 Run 起步时激活扩散引用了若干条已有记忆（架构 §4.14.5）。渲染一个低调的引用图标
 * 「⟨引用 N 条记忆⟩」，**hover** 弹出引用详情——列出引用了哪几条记忆（项目/全局徽章 +
 * 摘要）。与 MemoryWriteSummary（本轮写了什么）对称：这是本轮引用了什么。
 *
 * 用 hover 而非点击：引用是「顺带看一眼上下文」的轻量信息，不值得占一次点击；hover 即现、
 * 移开即隐，不留展开态污染消息流。
 */
export function MemoryRecallSummary({ items }: { items: MemoryWriteItem[] }) {
  if (items.length === 0) return null;

  return (
    <div className="group relative w-fit">
      <span className="flex cursor-default items-center gap-1 text-xs leading-4 text-muted-foreground/70 hover:text-muted-foreground">
        <Quote className="h-3.5 w-3.5" />
        <span>引用了 {items.length} 条记忆</span>
      </span>
      {/* hover 弹层：绝对定位在图标下方，group-hover 显现。max-h 限高，多条内滚。 */}
      <div className="pointer-events-none absolute left-0 top-full z-30 mt-1 hidden w-max max-w-[420px] group-hover:block">
        <div className="max-h-[220px] overflow-y-auto rounded-md border border-border bg-popover p-1.5 shadow-md">
          {items.map((it) => (
            <div
              key={it.id}
              className="flex items-start gap-2 px-1 py-1 text-[12px]"
            >
              <span
                className={cn(
                  "mt-0.5 shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium",
                  it.scope === "project"
                    ? "bg-amber-500/15 text-amber-600 dark:text-amber-400"
                    : "bg-sky-500/15 text-sky-600 dark:text-sky-400"
                )}
              >
                {it.scope === "project" ? "项目" : "全局"}
              </span>
              <span className="min-w-0 text-foreground/90">{it.summary}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
