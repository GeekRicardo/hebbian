import { useEffect, useMemo, useRef, useState } from "react";
import { X, ChevronUp, ChevronDown, Regex, CaseSensitive } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";

export interface FindState {
  query: string;
  regex: boolean;
  caseSensitive: boolean;
  current: number; // 1-based, 0 = 无命中
  total: number;
}

interface Props {
  open: boolean;
  onClose: () => void;
  state: FindState;
  onChange: (patch: Partial<FindState>) => void;
  onPrev: () => void;
  onNext: () => void;
}

export function FindBar({ open, onClose, state, onChange, onPrev, onNext }: Props) {
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      const id = window.setTimeout(() => inputRef.current?.select(), 10);
      return () => window.clearTimeout(id);
    }
  }, [open]);

  if (!open) return null;

  const invalid = state.query.length > 0 && state.total === 0;

  return (
    <div className="absolute top-3 right-4 z-40 flex items-center gap-1 rounded-lg border border-border bg-background shadow-lg px-2 py-1.5 animate-slide-up">
      <input
        ref={inputRef}
        value={state.query}
        onChange={(e) => onChange({ query: e.target.value })}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            if (e.shiftKey) onPrev();
            else onNext();
          } else if (e.key === "Escape") {
            e.preventDefault();
            onClose();
          }
        }}
        spellCheck={false}
        autoCorrect="off"
        placeholder="在当前对话中查找"
        className={cn(
          "h-7 w-56 rounded-md border border-input bg-background px-2 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring",
          invalid && "border-destructive/60"
        )}
      />
      <span className="text-[11px] text-muted-foreground w-14 text-center tabular-nums">
        {state.total === 0
          ? state.query
            ? "0 / 0"
            : "—"
          : `${state.current} / ${state.total}`}
      </span>
      <div className="h-4 w-px bg-border mx-0.5" />
      <button
        onClick={() => onChange({ caseSensitive: !state.caseSensitive })}
        className={cn(
          "h-7 w-7 inline-flex items-center justify-center rounded-md text-muted-foreground hover:bg-accent",
          state.caseSensitive && "bg-primary/15 text-primary"
        )}
        title="区分大小写 (Alt+C)"
      >
        <CaseSensitive className="w-4 h-4" />
      </button>
      <button
        onClick={() => onChange({ regex: !state.regex })}
        className={cn(
          "h-7 w-7 inline-flex items-center justify-center rounded-md text-muted-foreground hover:bg-accent",
          state.regex && "bg-primary/15 text-primary"
        )}
        title="正则表达式 (Alt+R)"
      >
        <Regex className="w-4 h-4" />
      </button>
      <div className="h-4 w-px bg-border mx-0.5" />
      <button
        onClick={onPrev}
        disabled={state.total === 0}
        className="h-7 w-7 inline-flex items-center justify-center rounded-md text-muted-foreground hover:bg-accent disabled:opacity-40"
        title="上一个 (Shift+Enter)"
      >
        <ChevronUp className="w-4 h-4" />
      </button>
      <button
        onClick={onNext}
        disabled={state.total === 0}
        className="h-7 w-7 inline-flex items-center justify-center rounded-md text-muted-foreground hover:bg-accent disabled:opacity-40"
        title="下一个 (Enter)"
      >
        <ChevronDown className="w-4 h-4" />
      </button>
      <button
        onClick={onClose}
        className="h-7 w-7 inline-flex items-center justify-center rounded-md text-muted-foreground hover:bg-accent"
        title="关闭 (Esc)"
      >
        <X className="w-4 h-4" />
      </button>
    </div>
  );
}

/**
 * 在给定文本里寻找所有匹配的字节区间
 */
export function findMatches(
  text: string,
  query: string,
  regex: boolean,
  caseSensitive: boolean
): Array<[number, number]> {
  if (!query) return [];
  if (regex) {
    let re: RegExp;
    try {
      re = new RegExp(query, caseSensitive ? "g" : "gi");
    } catch {
      return [];
    }
    const out: Array<[number, number]> = [];
    let m: RegExpExecArray | null;
    while ((m = re.exec(text))) {
      if (m[0].length === 0) {
        re.lastIndex++;
        continue;
      }
      out.push([m.index, m.index + m[0].length]);
    }
    return out;
  }
  const hay = caseSensitive ? text : text.toLowerCase();
  const needle = caseSensitive ? query : query.toLowerCase();
  const out: Array<[number, number]> = [];
  let i = 0;
  while (i <= hay.length - needle.length) {
    const idx = hay.indexOf(needle, i);
    if (idx < 0) break;
    out.push([idx, idx + needle.length]);
    i = idx + needle.length;
  }
  return out;
}

/**
 * 把文本 + 匹配区间渲染成带高亮的 span
 */
export function highlight(
  text: string,
  matches: Array<[number, number]>,
  activeIdx: number | null,
  keyPrefix: string
) {
  if (matches.length === 0) return text;
  const out: React.ReactNode[] = [];
  let cursor = 0;
  matches.forEach(([s, e], i) => {
    if (s > cursor) out.push(text.slice(cursor, s));
    const active = i === activeIdx;
    out.push(
      <mark
        key={`${keyPrefix}-${i}`}
        data-find-match
        data-active={active ? "true" : undefined}
        className={cn(
          "rounded-sm px-0.5",
          active
            ? "bg-amber-400 text-black"
            : "bg-yellow-200 text-black dark:bg-yellow-300/90"
        )}
      >
        {text.slice(s, e)}
      </mark>
    );
    cursor = e;
  });
  if (cursor < text.length) out.push(text.slice(cursor));
  return out;
}

/**
 * 将消息列表展平，返回每条消息的匹配数量（用于计算全局 current/total）
 */
export function useMessageMatches(
  contents: string[],
  query: string,
  regex: boolean,
  caseSensitive: boolean
) {
  return useMemo(() => {
    const per = contents.map((c) =>
      findMatches(c, query, regex, caseSensitive)
    );
    const total = per.reduce((s, a) => s + a.length, 0);
    return { per, total };
  }, [contents, query, regex, caseSensitive]);
}

export function useFindController(totalMatches: number) {
  const [active, setActive] = useState(0);
  useEffect(() => {
    if (active >= totalMatches) setActive(0);
  }, [totalMatches, active]);
  const next = () =>
    setActive((i) => (totalMatches === 0 ? 0 : (i + 1) % totalMatches));
  const prev = () =>
    setActive((i) =>
      totalMatches === 0 ? 0 : (i - 1 + totalMatches) % totalMatches
    );
  return { active, setActive, next, prev };
}
