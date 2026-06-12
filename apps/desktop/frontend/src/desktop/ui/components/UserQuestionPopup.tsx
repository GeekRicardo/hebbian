import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { CircleHelp, Send, X } from "lucide-react";
import { toast } from "sonner";
import type { AskQuestion, QuestionAnswerItem } from "@/desktop/ui/types";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";

const OTHER_KEY = "__other__";

type SingleState = {
  selected: string | null;
  multiSelected: string[];
  otherText: string;
};

const emptySingleState = (): SingleState => ({
  selected: null,
  multiSelected: [],
  otherText: "",
});

export function UserQuestionPopup() {
  const pending = useStore((s) => s.pendingQuestion);
  const resolveQuestion = useStore((s) => s.resolveQuestion);

  const [singleState, setSingleState] = useState<SingleState>(() => emptySingleState());
  const [answers, setAnswers] = useState<Record<number, SingleState>>({});
  const [submitting, setSubmitting] = useState(false);
  const otherInputRef = useRef<HTMLTextAreaElement>(null);

  const questions = useMemo<AskQuestion[]>(() => {
    if (!pending) return [];
    if (pending.questions.length > 0) return pending.questions;
    return [{ title: pending.question, description: "", options: pending.options, multi: pending.multi }];
  }, [pending]);
  const isMultiQuestion = !!pending && pending.questions.length > 0;

  useEffect(() => {
    setSingleState(emptySingleState());
    setAnswers({});
  }, [pending?.requestId]);

  useEffect(() => {
    if (!isMultiQuestion && singleState.selected === OTHER_KEY) otherInputRef.current?.focus();
  }, [isMultiQuestion, singleState.selected]);

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
    if (submitting || !pending) return false;
    return questions.every((q, idx) => isAnswered(q, stateFor(idx)));
  }, [pending, questions, answers, singleState, submitting]);

  if (!pending) return null;

  function stateFor(index: number) {
    return isMultiQuestion ? (answers[index] ?? emptySingleState()) : singleState;
  }

  function updateQuestion(index: number, updater: (prev: SingleState) => SingleState) {
    if (isMultiQuestion) {
      setAnswers((prev) => ({ ...prev, [index]: updater(prev[index] ?? emptySingleState()) }));
    } else {
      setSingleState((prev) => updater(prev));
    }
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
    const payload = isMultiQuestion
      ? { kind: "multi" as const, items: buildMultiItems(questions, answers) }
      : buildSinglePayload(questions[0], singleState);
    if (!payload) return;
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
        <div className="w-[calc(100%+42px)] -mr-[42px] rounded-lg border border-border bg-card text-card-foreground shadow-lg overflow-hidden pointer-events-auto animate-in fade-in slide-in-from-bottom-2 duration-150">
          <div className="flex items-start gap-2 px-3 py-1.5 border-b border-border bg-muted/40">
            <CircleHelp className="w-3.5 h-3.5 text-primary shrink-0 mt-1" />
            <span className="text-sm font-medium flex-1 leading-5">
              {isMultiQuestion ? `需要你回答 ${questions.length} 个问题` : pending.question}
            </span>
            {(isMultiQuestion || pending.multi) && (
              <span className="text-[11px] px-1.5 py-0.5 rounded bg-primary/15 text-primary font-medium shrink-0 mt-0.5">
                {isMultiQuestion ? "多题" : "多选"}
              </span>
            )}
            <span className="text-[11px] text-muted-foreground/80 shrink-0 mt-1">ESC 取消</span>
          </div>

          <div className="max-h-[55vh] overflow-y-auto">
            {questions.map((q, qIdx) => (
              <QuestionBlock
                key={`${qIdx}-${q.title}`}
                index={qIdx}
                question={q}
                state={stateFor(qIdx)}
                submitting={submitting}
                otherInputRef={!isMultiQuestion && qIdx === 0 ? otherInputRef : undefined}
                showQuestionHeader={isMultiQuestion}
                onSelect={(label) => updateQuestion(qIdx, (prev) => ({ ...prev, selected: label }))}
                onToggleMulti={(label) => updateQuestion(qIdx, (prev) => ({
                  ...prev,
                  multiSelected: prev.multiSelected.includes(label)
                    ? prev.multiSelected.filter((l) => l !== label)
                    : [...prev.multiSelected, label],
                }))}
                onOtherText={(text) => updateQuestion(qIdx, (prev) => ({ ...prev, selected: OTHER_KEY, otherText: text }))}
                onSelectOther={() => updateQuestion(qIdx, (prev) => ({ ...prev, selected: OTHER_KEY }))}
                onSubmit={submit}
              />
            ))}
          </div>

          <div className="flex items-center gap-1.5 px-2 py-1.5 border-t border-border bg-background/60">
            {isMultiQuestion && (
              <span className="text-[11px] text-muted-foreground pl-1">
                已回答 {questions.filter((q, idx) => isAnswered(q, stateFor(idx))).length} / {questions.length}
              </span>
            )}
            <div className="flex-1" />
            <button type="button" onClick={cancel} disabled={submitting} className="h-7 px-2.5 rounded-md text-[13px] inline-flex items-center gap-1 transition-colors text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50">
              <X className="w-3.5 h-3.5" />取消
            </button>
            <button type="button" onClick={submit} disabled={!canSubmit} className="h-7 px-2.5 rounded-md text-[13px] font-medium inline-flex items-center gap-1 transition-colors bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-40">
              <Send className="w-3.5 h-3.5" />提交
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function QuestionBlock({ index, question, state, submitting, otherInputRef, showQuestionHeader, onSelect, onToggleMulti, onOtherText, onSelectOther, onSubmit }: {
  index: number;
  question: AskQuestion;
  state: SingleState;
  submitting: boolean;
  otherInputRef?: React.RefObject<HTMLTextAreaElement>;
  showQuestionHeader: boolean;
  onSelect: (label: string) => void;
  onToggleMulti: (label: string) => void;
  onOtherText: (text: string) => void;
  onSelectOther: () => void;
  onSubmit: () => void;
}) {
  return (
    <section className={cn(showQuestionHeader && index > 0 && "border-t border-border")}>
      {showQuestionHeader && (
        <div className="px-3 py-2 bg-muted/20">
          <div className="flex items-center gap-2">
            <span className="text-[11px] font-mono text-muted-foreground">{index + 1}.</span>
            <div className="text-sm font-medium leading-5 flex-1">{question.title}</div>
            {question.multi && <span className="text-[11px] px-1.5 py-0.5 rounded bg-primary/15 text-primary font-medium">多选</span>}
          </div>
          {question.description && <div className="mt-0.5 pl-5 text-[12px] text-muted-foreground leading-4">{question.description}</div>}
        </div>
      )}
      <div>
        {question.options.map((opt, idx) => {
          const checked = question.multi ? state.multiSelected.includes(opt.label) : state.selected === opt.label;
          return (
            <Fragment key={`${idx}-${opt.label}`}>
              {idx > 0 && <div className="h-px bg-border mx-3" />}
              <button type="button" onClick={() => (question.multi ? onToggleMulti(opt.label) : onSelect(opt.label))} disabled={submitting} aria-pressed={checked} className={cn("w-full text-left px-3 py-1.5 transition-colors text-sm flex items-start gap-2", checked ? "bg-primary/10 text-primary" : "hover:bg-muted")}>
                {question.multi && <span className={cn("mt-[3px] inline-flex items-center justify-center w-3.5 h-3.5 rounded border text-[10px] leading-none shrink-0", checked ? "bg-primary border-primary text-primary-foreground" : "border-muted-foreground/40")} aria-hidden>{checked ? "✓" : ""}</span>}
                <span className={cn("shrink-0 font-mono text-[12px] tabular-nums select-none leading-5", checked ? "text-primary" : "text-muted-foreground")}>{idx + 1}.</span>
                <span className="flex-1 min-w-0">
                  <div className="font-medium leading-5">{opt.label}</div>
                  {opt.description && <div className="text-[12px] text-muted-foreground leading-4">{opt.description}</div>}
                </span>
              </button>
            </Fragment>
          );
        })}

        {!question.multi && (
          <>
            {question.options.length > 0 && <div className="h-px bg-border mx-3" />}
            <button type="button" onClick={onSelectOther} disabled={submitting} className={cn("w-full text-left px-3 py-1.5 transition-colors text-sm", state.selected === OTHER_KEY ? "bg-primary/10 text-primary" : "hover:bg-muted text-muted-foreground")}>
              {state.selected === OTHER_KEY ? (
                <textarea ref={otherInputRef} value={state.otherText} onChange={(e) => onOtherText(e.target.value)} onClick={(e) => e.stopPropagation()} onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); onSubmit(); } }} placeholder="其他回答…（Cmd/Ctrl+Enter 提交）" rows={2} className="w-full resize-none rounded-md border border-input bg-background px-2 py-1 text-sm outline-none focus:ring-2 focus:ring-ring" />
              ) : (
                <div className="text-muted-foreground text-[13px] leading-5">其他回答…</div>
              )}
            </button>
          </>
        )}
      </div>
    </section>
  );
}

function isAnswered(question: AskQuestion, state: SingleState) {
  if (question.multi) return state.multiSelected.length > 0;
  if (!state.selected) return false;
  if (state.selected === OTHER_KEY) return state.otherText.trim().length > 0;
  return true;
}

function buildSinglePayload(question: AskQuestion, state: SingleState) {
  if (question.multi) return { kind: "selected_multi" as const, labels: state.multiSelected };
  if (state.selected === OTHER_KEY) return { kind: "custom" as const, text: state.otherText.trim() };
  if (state.selected) return { kind: "selected" as const, label: state.selected };
  return null;
}

function buildMultiItems(questions: AskQuestion[], answers: Record<number, SingleState>): QuestionAnswerItem[] {
  return questions.map((q, idx) => {
    const payload = buildSinglePayload(q, answers[idx] ?? emptySingleState());
    if (!payload) return { title: q.title, kind: "cancelled" };
    if (payload.kind === "selected") return { title: q.title, kind: "selected", text: payload.label };
    if (payload.kind === "selected_multi") return { title: q.title, kind: "selected_multi", labels: payload.labels };
    return { title: q.title, kind: "custom", text: payload.text };
  });
}
