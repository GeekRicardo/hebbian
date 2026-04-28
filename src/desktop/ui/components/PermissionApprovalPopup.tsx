import { useState } from "react";
import { Check, MessageSquareWarning, Shield, X } from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";

const RISK_STYLE: Record<
  "low" | "medium" | "high" | "critical",
  { label: string; className: string }
> = {
  low: {
    label: "低风险",
    className: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
  },
  medium: {
    label: "中风险",
    className: "bg-amber-500/10 text-amber-600 dark:text-amber-400",
  },
  high: {
    label: "高风险",
    className: "bg-orange-500/15 text-orange-600 dark:text-orange-400",
  },
  critical: {
    label: "高危",
    className: "bg-red-500/15 text-red-600 dark:text-red-400",
  },
};

/**
 * HITL 审批弹窗。挂在 ChatInput 上方。
 *
 * 设计：
 * - 没有 pendingApproval 时返回 null
 * - 默认显示工具名 + 输入预览 + 4 个按钮
 * - 点 "拒绝并反馈" 时 inline 展开 textarea
 */
export function PermissionApprovalPopup() {
  const pending = useStore((s) => s.pendingApproval);
  const resolveApproval = useStore((s) => s.resolveApproval);
  const [feedbackOpen, setFeedbackOpen] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [submitting, setSubmitting] = useState(false);

  if (!pending) return null;

  async function send(decision: Parameters<typeof resolveApproval>[0]) {
    setSubmitting(true);
    try {
      await resolveApproval(decision);
      setFeedbackOpen(false);
      setFeedback("");
    } catch (e: any) {
      toast.error(e?.message ?? "审批回应失败");
    } finally {
      setSubmitting(false);
    }
  }

  const risk = RISK_STYLE[pending.risk] ?? RISK_STYLE.medium;
  const inputPreview =
    typeof pending.input === "string"
      ? pending.input
      : JSON.stringify(pending.input, null, 2);

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
          <Shield className="w-4 h-4 text-primary shrink-0" />
          <span className="text-sm font-medium flex-1 truncate">
            AI 请求执行 <code className="font-mono">{pending.toolName}</code>
          </span>
          <span
            className={cn(
              "text-[11px] px-1.5 py-0.5 rounded font-medium",
              risk.className
            )}
          >
            {risk.label}
          </span>
        </div>

        {/* 输入参数预览 */}
        {inputPreview && inputPreview !== "null" && (
          <pre className="text-[11px] text-muted-foreground/90 px-3 py-2 max-h-32 overflow-auto bg-background/50 font-mono whitespace-pre-wrap break-all">
            {inputPreview.slice(0, 800)}
            {inputPreview.length > 800 ? "…" : ""}
          </pre>
        )}

        {/* 反馈输入框（按需展开） */}
        {feedbackOpen && (
          <div className="px-3 py-2 border-t border-border">
            <textarea
              value={feedback}
              onChange={(e) => setFeedback(e.target.value)}
              placeholder="告诉模型为什么拒绝（会作为 user message 注入下一轮）"
              rows={2}
              className="w-full resize-none rounded-md border border-input bg-background px-2 py-1.5 text-sm outline-none focus:ring-2 focus:ring-ring"
              autoFocus
            />
          </div>
        )}

        {/* 按钮组 */}
        <div className="flex items-center gap-1.5 px-2 py-2 border-t border-border bg-background/60">
          {!feedbackOpen ? (
            <>
              <button
                type="button"
                onClick={() => send({ kind: "allow_once" })}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm font-medium inline-flex items-center gap-1.5 transition-colors",
                  "bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
                )}
              >
                <Check className="w-3.5 h-3.5" />
                允许此次
              </button>
              <button
                type="button"
                onClick={() => send({ kind: "allow_and_remember" })}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                  "bg-muted hover:bg-muted/80 disabled:opacity-50"
                )}
                title="本会话内不再询问此工具"
              >
                总是允许
              </button>
              <div className="flex-1" />
              <button
                type="button"
                onClick={() => setFeedbackOpen(true)}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                  "text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"
                )}
              >
                <MessageSquareWarning className="w-3.5 h-3.5" />
                拒绝并反馈
              </button>
              <button
                type="button"
                onClick={() => send({ kind: "deny" })}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm font-medium inline-flex items-center gap-1.5 transition-colors",
                  "bg-destructive/10 text-destructive hover:bg-destructive/20 disabled:opacity-50"
                )}
              >
                <X className="w-3.5 h-3.5" />
                拒绝
              </button>
            </>
          ) : (
            <>
              <button
                type="button"
                onClick={() => {
                  setFeedbackOpen(false);
                  setFeedback("");
                }}
                disabled={submitting}
                className="h-8 px-3 rounded-md text-sm text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"
              >
                取消
              </button>
              <div className="flex-1" />
              <button
                type="button"
                onClick={() =>
                  send({ kind: "deny_with_feedback", feedback: feedback.trim() })
                }
                disabled={submitting || !feedback.trim()}
                className="h-8 px-3 rounded-md text-sm font-medium bg-destructive text-destructive-foreground hover:bg-destructive/90 disabled:opacity-50"
              >
                提交反馈
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
