import { CornerDownLeft, X } from "lucide-react";
import { toast } from "sonner";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";

/**
 * 运行时输入队列面板：streaming 期间用户排进的下一条 / 下几条 user message。
 *
 * - 列表从上到下展示，最早入队的在最上面。
 * - 当前 turn 跑完后会按 FIFO 自动消费第一条。
 * - 每条右侧的「立即发送」图标只对队首启用——「立即发送只允许从上到下」：
 *   点击后立刻把该条注入当前 run 的 pending 队列（agent_loop 在下一次
 *   model.request 之前 drain 出来加入 transcript），同时立即把它显示到 chat
 *   区域作为 user message——不打断当前 agent loop，下个 iteration 立刻可见。
 * - X 按钮可移除任意排队项（撤回）。
 *
 * 布局参考 UserQuestionPopup：外层 `px-4 pb-2`、内层 `max-w-3xl mx-auto pr-[50px]`，
 * 跟 ChatInput 内的 textarea 完全对齐。
 */
export function InputQueuePanel() {
  const queue = useStore((s) => s.currentInputQueue);
  const removeQueuedInput = useStore((s) => s.removeQueuedInput);
  const flushQueuedHead = useStore((s) => s.flushQueuedHead);

  if (queue.length === 0) return null;

  async function flushHead() {
    try {
      await flushQueuedHead();
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
                  disabled={!isHead}
                  onClick={isHead ? flushHead : undefined}
                  className={cn(
                    "shrink-0 h-6 w-6 rounded inline-flex items-center justify-center transition",
                    isHead
                      ? "text-primary hover:bg-primary/15"
                      : "text-muted-foreground/40 cursor-not-allowed"
                  )}
                  title={
                    isHead
                      ? "立即发送：下一个 model 请求前注入到对话"
                      : "仅允许从上到下立即发送"
                  }
                >
                  <CornerDownLeft className="w-3.5 h-3.5" />
                </button>
                <button
                  type="button"
                  onClick={() => removeQueuedInput(item.id)}
                  className="shrink-0 h-6 w-6 rounded inline-flex items-center justify-center text-muted-foreground hover:bg-muted hover:text-foreground"
                  title="移除"
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
