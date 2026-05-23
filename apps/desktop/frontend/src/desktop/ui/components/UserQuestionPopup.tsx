import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { CircleHelp, Send, X } from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";

const OTHER_KEY = "__other__";

/**
 * agent 主动提问弹窗（ask 工具）。挂在 ChatInput 上方。
 *
 * 单选：点击切换选中，最后一项 "其他" 选中时露出 textarea；ESC 取消。
 * 多选（`pending.multi=true`）：点击勾选/取消勾选，可多个；不提供 "其他" 自由输入。
 * 选项均按 `1./2./3.` 编号显示，跟终端模式风格一致。
 */
export function UserQuestionPopup() {
  const pending = useStore((s) => s.pendingQuestion);
  const resolveQuestion = useStore((s) => s.resolveQuestion);

  // 单选状态：label 或 OTHER_KEY
  const [selected, setSelected] = useState<string | null>(null);
  // 多选状态：勾选的 label 集合（按勾选顺序保存）
  const [multiSelected, setMultiSelected] = useState<string[]>([]);
  const [otherText, setOtherText] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const otherInputRef = useRef<HTMLTextAreaElement>(null);

  const isMulti = !!pending?.multi;

  // 切换 question 时重置
  useEffect(() => {
    setSelected(null);
    setMultiSelected([]);
    setOtherText("");
  }, [pending?.requestId]);

  // 单选 + "其他" 时自动聚焦输入框
  useEffect(() => {
    if (!isMulti && selected === OTHER_KEY) {
      otherInputRef.current?.focus();
    }
  }, [isMulti, selected]);

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

  const canSubmit = useMemo(() => {
    if (submitting) return false;
    if (isMulti) return multiSelected.length > 0;
    if (!selected) return false;
    if (selected === OTHER_KEY) return otherText.trim().length > 0;
    return true;
  }, [isMulti, multiSelected, selected, otherText, submitting]);

  if (!pending) return null;

  function toggleMulti(label: string) {
    setMultiSelected((prev) =>
      prev.includes(label) ? prev.filter((l) => l !== label) : [...prev, label]
    );
  }

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
    if (!canSubmit) return;
    let payload: Parameters<typeof resolveQuestion>[0];
    if (isMulti) {
      payload = { kind: "selected_multi", labels: multiSelected };
    } else if (selected === OTHER_KEY) {
      payload = { kind: "custom", text: otherText.trim() };
    } else if (selected) {
      payload = { kind: "selected", label: selected };
    } else {
      return;
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

  return (
    <div className="px-4 pb-2">
      <div className="max-w-3xl mx-auto pr-[50px]">
        <div
          className={cn(
            "w-full rounded-lg border border-border bg-card text-card-foreground shadow-lg overflow-hidden pointer-events-auto",
            "animate-in fade-in slide-in-from-bottom-2 duration-150"
          )}
        >
          {/* 头部 */}
          <div className="flex items-start gap-2 px-3 py-1.5 border-b border-border bg-muted/40">
            <CircleHelp className="w-3.5 h-3.5 text-primary shrink-0 mt-1" />
            <span className="text-sm font-medium flex-1 whitespace-pre-wrap break-words leading-5">
              {pending.question}
            </span>
            {isMulti && (
              <span className="text-[11px] px-1.5 py-0.5 rounded bg-primary/15 text-primary font-medium shrink-0 mt-0.5">
                多选
              </span>
            )}
            <span className="text-[11px] text-muted-foreground/80 shrink-0 mt-1">
              ESC 取消
            </span>
          </div>

          {/* 选项列表：选项 button 贴 popup 两端 hover bg 全宽，hairline mx-3 缩进留 12px */}
          <div>
            {pending.options.map((opt, idx) => {
              const checked = isMulti
                ? multiSelected.includes(opt.label)
                : selected === opt.label;
              return (
                <Fragment key={`${idx}-${opt.label}`}>
                  {idx > 0 && <div className="h-px bg-border mx-3" />}
                <button
                  type="button"
                  onClick={() =>
                    isMulti ? toggleMulti(opt.label) : setSelected(opt.label)
                  }
                  disabled={submitting}
                  aria-pressed={checked}
                  className={cn(
                    "w-full text-left px-3 py-1.5 transition-colors text-sm flex items-start gap-2",
                    checked
                      ? "bg-primary/10 text-primary"
                      : "hover:bg-muted"
                  )}
                >
                  {isMulti ? (
                    <span
                      className={cn(
                        "mt-[3px] inline-flex items-center justify-center w-3.5 h-3.5 rounded border text-[10px] leading-none shrink-0",
                        checked
                          ? "bg-primary border-primary text-primary-foreground"
                          : "border-muted-foreground/40"
                      )}
                      aria-hidden
                    >
                      {checked ? "✓" : ""}
                    </span>
                  ) : null}
                  <span
                    className={cn(
                      "shrink-0 font-mono text-[12px] tabular-nums select-none leading-5",
                      checked ? "text-primary" : "text-muted-foreground"
                    )}
                  >
                    {idx + 1}.
                  </span>
                  <span className="flex-1 min-w-0">
                    <div className="font-medium leading-5">{opt.label}</div>
                    {opt.description && (
                      <div className="text-[12px] text-muted-foreground leading-4">
                        {opt.description}
                      </div>
                    )}
                  </span>
                </button>
                </Fragment>
              );
            })}

            {/* "其他"：仅单选模式提供 */}
            {!isMulti && (
              <>
              {pending.options.length > 0 && <div className="h-px bg-border mx-3" />}
              <button
                type="button"
                onClick={() => setSelected(OTHER_KEY)}
                disabled={submitting}
                className={cn(
                  "w-full text-left px-3 py-1.5 transition-colors text-sm",
                  selected === OTHER_KEY
                    ? "bg-primary/10 text-primary"
                    : "hover:bg-muted text-muted-foreground"
                )}
              >
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
                    placeholder="其他回答…（Cmd/Ctrl+Enter 提交）"
                    rows={2}
                    className="w-full resize-none rounded-md border border-input bg-background px-2 py-1 text-sm outline-none focus:ring-2 focus:ring-ring"
                  />
                ) : (
                  <div className="text-muted-foreground text-[13px] leading-5">
                    其他回答…
                  </div>
                )}
              </button>
              </>
            )}
          </div>

          {/* 底部按钮 */}
          <div className="flex items-center gap-1.5 px-2 py-1.5 border-t border-border bg-background/60">
            {isMulti && multiSelected.length > 0 && (
              <span className="text-[11px] text-muted-foreground pl-1">
                已选 {multiSelected.length} 项
              </span>
            )}
            <div className="flex-1" />
            <button
              type="button"
              onClick={cancel}
              disabled={submitting}
              className={cn(
                "h-7 px-2.5 rounded-md text-[13px] inline-flex items-center gap-1 transition-colors",
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
                "h-7 px-2.5 rounded-md text-[13px] font-medium inline-flex items-center gap-1 transition-colors",
                "bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-40"
              )}
            >
              <Send className="w-3.5 h-3.5" />
              提交
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
