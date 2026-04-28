import { useEffect, useRef, useState } from "react";
import { CircleHelp, Send, X } from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";

const OTHER_KEY = "__other__";

/**
 * agent 主动提问弹窗（ask 工具）。挂在 ChatInput 上方。
 *
 * 设计：
 * - 选项可点击，最后一项是 "其他"，选中时露出 textarea
 * - 右下角：取消 / 提交
 * - ESC = 取消
 */
export function UserQuestionPopup() {
  const pending = useStore((s) => s.pendingQuestion);
  const resolveQuestion = useStore((s) => s.resolveQuestion);

  const [selected, setSelected] = useState<string | null>(null);
  const [otherText, setOtherText] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const otherInputRef = useRef<HTMLTextAreaElement>(null);

  // 切换 question 时重置选择
  useEffect(() => {
    setSelected(null);
    setOtherText("");
  }, [pending?.requestId]);

  // 选中 "其他" 时自动聚焦输入框
  useEffect(() => {
    if (selected === OTHER_KEY) {
      otherInputRef.current?.focus();
    }
  }, [selected]);

  // ESC 取消
  useEffect(() => {
    if (!pending) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        cancel();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending?.requestId]);

  if (!pending) return null;

  async function cancel() {
    setSubmitting(true);
    try {
      await resolveQuestion({ kind: "cancelled" });
    } catch (e: any) {
      toast.error(e?.message ?? "提交失败");
    } finally {
      setSubmitting(false);
    }
  }

  async function submit() {
    if (!selected) return;
    let payload: Parameters<typeof resolveQuestion>[0];
    if (selected === OTHER_KEY) {
      const text = otherText.trim();
      if (!text) {
        otherInputRef.current?.focus();
        return;
      }
      payload = { kind: "custom", text };
    } else {
      payload = { kind: "selected", label: selected };
    }
    setSubmitting(true);
    try {
      await resolveQuestion(payload);
    } catch (e: any) {
      toast.error(e?.message ?? "提交失败");
    } finally {
      setSubmitting(false);
    }
  }

  const canSubmit =
    !!selected &&
    (selected !== OTHER_KEY || otherText.trim().length > 0) &&
    !submitting;

  return (
    <div className="max-w-3xl mx-auto px-4 pb-2">
      <div
        className={cn(
          "rounded-lg border border-border bg-popover shadow-lg overflow-hidden",
          "animate-in fade-in slide-in-from-bottom-2 duration-150"
        )}
      >
        {/* 头部 */}
        <div className="flex items-center gap-2 px-3 py-2 border-b border-border bg-muted/40">
          <CircleHelp className="w-4 h-4 text-primary shrink-0" />
          <span className="text-sm font-medium flex-1 truncate">
            {pending.question}
          </span>
          <span className="text-[11px] text-muted-foreground/80">
            ESC 取消
          </span>
        </div>

        {/* 选项列表 */}
        <div className="px-2 py-2 flex flex-col gap-1">
          {pending.options.map((opt) => {
            const isSelected = selected === opt.label;
            return (
              <button
                key={opt.label}
                type="button"
                onClick={() => setSelected(opt.label)}
                disabled={submitting}
                className={cn(
                  "w-full text-left px-3 py-2 rounded-md transition-colors text-sm",
                  "border",
                  isSelected
                    ? "border-primary bg-primary/10"
                    : "border-transparent hover:bg-muted"
                )}
              >
                <div className="font-medium">{opt.label}</div>
                {opt.description && (
                  <div className="text-[12px] text-muted-foreground mt-0.5">
                    {opt.description}
                  </div>
                )}
              </button>
            );
          })}

          {/* 其他选项：选中时露 textarea */}
          <button
            type="button"
            onClick={() => setSelected(OTHER_KEY)}
            disabled={submitting}
            className={cn(
              "w-full text-left px-3 py-2 rounded-md transition-colors text-sm border",
              selected === OTHER_KEY
                ? "border-primary bg-primary/10"
                : "border-dashed border-muted-foreground/30 hover:bg-muted"
            )}
          >
            <div className="text-muted-foreground text-[12px] mb-1">
              其他
            </div>
            {selected === OTHER_KEY ? (
              <textarea
                ref={otherInputRef}
                value={otherText}
                onChange={(e) => setOtherText(e.target.value)}
                onClick={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    submit();
                  }
                }}
                placeholder="输入你的回答…（Cmd/Ctrl+Enter 提交）"
                rows={2}
                className="w-full resize-none rounded-md border border-input bg-background px-2 py-1.5 text-sm outline-none focus:ring-2 focus:ring-ring"
              />
            ) : (
              <div className="text-muted-foreground text-[13px]">
                自由输入回答…
              </div>
            )}
          </button>
        </div>

        {/* 底部按钮 */}
        <div className="flex items-center gap-2 px-2 py-2 border-t border-border bg-background/60">
          <div className="flex-1" />
          <button
            type="button"
            onClick={cancel}
            disabled={submitting}
            className={cn(
              "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
              "text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"
            )}
          >
            <X className="w-3.5 h-3.5" />
            取消
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={!canSubmit}
            className={cn(
              "h-8 px-3 rounded-md text-sm font-medium inline-flex items-center gap-1.5 transition-colors",
              "bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-40"
            )}
          >
            <Send className="w-3.5 h-3.5" />
            提交
          </button>
        </div>
      </div>
    </div>
  );
}
