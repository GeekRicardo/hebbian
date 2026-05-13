import { CornerDownLeft, Trash2, X } from "lucide-react";
import { toast } from "sonner";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";

/**
 * 运行时输入队列面板：streaming 期间用户排队的下一条 / 下几条 user message。
 *
 * 队列语义（架构.md §4.2.3）：
 * - 默认 Enter 入队 → 等本 Run 跑完后 drainNext 顺次作为新 Run 发出。
 * - Shift+Enter / 行内 ↩「引导」按钮 → 走 PendingInputs，agent_loop
 *   在下一次 ModelStep 之前 drain，等价于"当前 model_call+tool_call
 *   完成后立即插队"，不开新 Run。
 *
 * 行内三按钮：
 * - ↩ 引导：把这条注入 PendingInputs（任意位置可点）
 * - 🗑 删除：从队列移除
 * - ✕ 放回输入框：移除并把内容追加回 ChatInput 草稿
 */
export function InputQueuePanel() {
  const queue = useStore((s) => s.currentInputQueue);
  const removeQueuedInput = useStore((s) => s.removeQueuedInput);
  const flushQueuedItem = useStore((s) => s.flushQueuedItem);
  const returnQueuedToComposer = useStore((s) => s.returnQueuedToComposer);

  if (queue.length === 0) return null;

  async function flush(id: string) {
    try {
      await flushQueuedItem(id);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  return (
    <div className="px-4 pb-2">
      <div className="max-w-3xl mx-auto pr-[50px]">
        <ul className="flex flex-col gap-1">
          {queue.map((item, idx) => {
            const isHead = idx === 0;
            const preview =
              item.content.trim() ||
              (item.attachments.length > 0
                ? `（${item.attachments.length} 个附件）`
                : "（空）");
            return (
              <li
                key={item.id}
                className={cn(
                  "flex items-center gap-2 rounded-md border px-2 py-1.5 text-sm",
                  isHead
                    ? "border-primary/40 bg-primary/5"
                    : "border-border bg-muted/40"
                )}
              >
                <span
                  className={cn(
                    "shrink-0 inline-flex items-center justify-center w-5 h-5 rounded-full text-[10px] font-mono",
                    isHead
                      ? "bg-primary text-primary-foreground"
                      : "bg-muted text-muted-foreground"
                  )}
                  title={isHead ? "下一个发送" : `第 ${idx + 1} 条`}
                >
                  {idx + 1}
                </span>
                <span
                  className="flex-1 min-w-0 truncate text-foreground/90"
                  title={item.content}
                >
                  {preview}
                </span>
                {item.attachments.length > 0 && (
                  <span className="shrink-0 text-[10px] text-muted-foreground">
                    +{item.attachments.length} 附件
                  </span>
                )}
                <button
                  type="button"
                  onClick={() => flush(item.id)}
                  className="shrink-0 h-6 w-6 rounded inline-flex items-center justify-center text-primary hover:bg-primary/15 transition"
                  title="引导：当前模型调用完成后立即插队"
                  aria-label="引导"
                >
                  <CornerDownLeft className="w-3.5 h-3.5" />
                </button>
                <button
                  type="button"
                  onClick={() => removeQueuedInput(item.id)}
                  className="shrink-0 h-6 w-6 rounded inline-flex items-center justify-center text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                  title="删除"
                  aria-label="删除"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
                <button
                  type="button"
                  onClick={() => returnQueuedToComposer(item.id)}
                  className="shrink-0 h-6 w-6 rounded inline-flex items-center justify-center text-muted-foreground hover:bg-muted hover:text-foreground"
                  title="放回输入框"
                  aria-label="放回输入框"
                >
                  <X className="w-3.5 h-3.5" />
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}
