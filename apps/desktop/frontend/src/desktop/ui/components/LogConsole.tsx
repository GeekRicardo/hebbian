import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Search, X, Regex, ChevronUp, ChevronDown, ArrowDownToLine, Trash2 } from "lucide-react";
import { api } from "@/desktop/bridge/tauri";
import type { LogLine } from "@/desktop/ui/types";

// ─── 日志控制台（DOM 渲染，替代终端）─────────────────────────────────
// 为什么不用终端：终端无法在已渲染内容里做原地搜索/按等级过滤，只能清屏重写，
// 实时流一进来就把过滤视图冲乱。DOM 列表里搜索高亮、等级过滤、跳转都是原生能力。

type Level = "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE";
const LEVELS: Level[] = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];

/** 每个等级一套深色背景下的配色：文字 / 行首色条 / 徽章底。 */
const LEVEL_STYLE: Record<Level, { text: string; bar: string; badgeBg: string }> = {
  ERROR: { text: "text-red-400", bar: "#ef4444", badgeBg: "rgba(239,68,68,0.14)" },
  WARN: { text: "text-amber-400", bar: "#f59e0b", badgeBg: "rgba(245,158,11,0.14)" },
  INFO: { text: "text-emerald-400", bar: "#10b981", badgeBg: "rgba(16,185,129,0.12)" },
  DEBUG: { text: "text-sky-400", bar: "#0ea5e9", badgeBg: "rgba(14,165,233,0.12)" },
  TRACE: { text: "text-zinc-500", bar: "#52525b", badgeBg: "rgba(82,82,91,0.18)" },
};

interface Row {
  ts: string; // "HH:MM:SS.mmm"，无则空串
  level: Level;
  body: string; // 已剥离 ANSI 的纯文本（target: message + 字段）
}

/** 剥离 ANSI 转义序列，留纯文本用于解析、搜索、渲染。 */
function stripAnsi(s: string): string {
  // eslint-disable-next-line no-control-regex
  return s.replace(/\x1b\[[0-9;]*m/g, "");
}

const ISO_TS = /^(\d{4}-\d\d-\d\dT)(\d\d:\d\d:\d\d\.\d{3})\d*Z/;
const LEAD_LEVEL = /^\s*(ERROR|WARN|INFO|DEBUG|TRACE)\b\s*/;

/**
 * 解析历史文件的一行纯文本为 Row。无等级的续行（多行 message / panic 栈）
 * 继承上一行等级，这样按等级过滤时续行跟着主行一起显隐。
 */
function parseHistoryLine(plain: string, lastLevel: Level): Row {
  let rest = plain;
  let ts = "";
  const tsM = ISO_TS.exec(rest);
  if (tsM) {
    ts = tsM[2];
    rest = rest.slice(tsM[0].length);
  }
  const lvM = LEAD_LEVEL.exec(rest);
  const level = (lvM?.[1] as Level | undefined) ?? lastLevel;
  if (lvM) rest = rest.slice(lvM[0].length);
  return { ts, level, body: rest };
}

// ─── body 富文本：把 key= 字段名压暗，让数值更突出 ──────────────────
const FIELD_KEY = /([A-Za-z_][\w.]*)(=)/g;

function renderBody(body: string, query: string, caseSensitive: boolean): React.ReactNode {
  // 先按搜索词切片高亮，再在非高亮片段里压暗 key= 字段名。
  if (!query) return dimFieldKeys(body, 0);
  const hay = caseSensitive ? body : body.toLowerCase();
  const needle = caseSensitive ? query : query.toLowerCase();
  const out: React.ReactNode[] = [];
  let from = 0;
  let hit = hay.indexOf(needle, from);
  let key = 0;
  while (hit !== -1 && needle.length > 0) {
    if (hit > from) out.push(dimFieldKeys(body.slice(from, hit), key++));
    out.push(
      <mark key={`m${key++}`} className="rounded-sm bg-yellow-400/30 text-yellow-200">
        {body.slice(hit, hit + needle.length)}
      </mark>,
    );
    from = hit + needle.length;
    hit = hay.indexOf(needle, from);
  }
  if (from < body.length) out.push(dimFieldKeys(body.slice(from), key++));
  return out;
}

function dimFieldKeys(text: string, baseKey: number): React.ReactNode {
  const parts: React.ReactNode[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  FIELD_KEY.lastIndex = 0;
  let i = 0;
  while ((m = FIELD_KEY.exec(text)) !== null) {
    if (m.index > last) parts.push(text.slice(last, m.index));
    parts.push(
      <span key={`k${baseKey}-${i++}`} className="text-white/35">
        {m[1]}
        {m[2]}
      </span>,
    );
    last = m.index + m[0].length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts.length ? <>{parts}</> : text;
}

// ─── 虚拟滚动参数 ─────────────────────────────────────────────────
const OVERSCAN = 12;

interface LogConsoleProps {
  /** 行高与字号；独立窗口给更大的值。 */
  fontSize?: number;
  rowHeight?: number;
  className?: string;
}

export default function LogConsole({ fontSize = 12.5, rowHeight = 22, className }: LogConsoleProps) {
  // rows 用 ref 持有全量（实时流高频 append），tick 触发批量重渲染。
  const rowsRef = useRef<Row[]>([]);
  const lastLevelRef = useRef<Level>("INFO");
  const [, setTick] = useState(0);
  const pendingFlush = useRef<number | null>(null);

  const scrollRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(0);

  const stickRef = useRef(true); // 是否粘底（用户没往上翻时新日志自动到底）
  const [autoScroll, setAutoScroll] = useState(true);

  // 搜索 / 过滤状态
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [enabled, setEnabled] = useState<Set<Level>>(() => new Set(LEVELS));
  const searchInputRef = useRef<HTMLInputElement>(null);

  const scheduleFlush = useCallback(() => {
    if (pendingFlush.current != null) return;
    pendingFlush.current = requestAnimationFrame(() => {
      pendingFlush.current = null;
      setTick((t) => t + 1);
    });
  }, []);

  // ── 加载历史 + 订阅实时流 ──
  useEffect(() => {
    let active = true;
    api
      .readLogFile()
      .then((text) => {
        if (!active || !text.trim()) return;
        for (const raw of text.split("\n")) {
          if (!raw) continue;
          const row = parseHistoryLine(stripAnsi(raw), lastLevelRef.current);
          lastLevelRef.current = row.level;
          rowsRef.current.push(row);
        }
        scheduleFlush();
      })
      .catch(() => {});

    const cancel = api.subscribeLogStream((line: LogLine) => {
      if (!active) return;
      lastLevelRef.current = line.level;
      rowsRef.current.push({ ts: line.ts, level: line.level, body: `${line.target}: ${line.message}` });
      scheduleFlush();
    });
    return () => {
      active = false;
      cancel();
    };
  }, [scheduleFlush]);

  // ── 过滤后的可见行（等级过滤；搜索是高亮+跳转，不删行）──
  const visibleRows = useMemo(() => {
    const all = rowsRef.current;
    if (enabled.size === LEVELS.length) return all;
    return all.filter((r) => enabled.has(r.level));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, rowsRef.current.length]);

  // ── 搜索匹配（visibleRows 里的下标）──
  const matches = useMemo(() => {
    if (!query) return [];
    const needle = caseSensitive ? query : query.toLowerCase();
    const idx: number[] = [];
    for (let i = 0; i < visibleRows.length; i++) {
      const b = caseSensitive ? visibleRows[i].body : visibleRows[i].body.toLowerCase();
      if (b.includes(needle)) idx.push(i);
    }
    return idx;
  }, [query, caseSensitive, visibleRows]);
  const [matchPos, setMatchPos] = useState(0);
  useEffect(() => setMatchPos(0), [query, caseSensitive]);

  // ── 行数变化：粘底则滚到底；否则夹紧 scrollTop，
  //    避免过滤/清空让内容变短时视口停在空白区 ──
  const totalH = visibleRows.length * rowHeight;
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (stickRef.current && autoScroll) {
      el.scrollTop = el.scrollHeight;
    } else {
      const max = Math.max(0, el.scrollHeight - el.clientHeight);
      if (el.scrollTop > max) el.scrollTop = max;
    }
    setScrollTop(el.scrollTop); // 与虚拟窗口的 scrollTop 状态同步
  }, [visibleRows.length, totalH, autoScroll]);

  // ── 容器尺寸 ──
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewH(el.clientHeight));
    ro.observe(el);
    setViewH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    setScrollTop(el.scrollTop);
    stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < rowHeight * 1.5;
  }, [rowHeight]);

  // ── 跳到某个匹配，滚动使其居中 ──
  const jumpTo = useCallback(
    (pos: number) => {
      if (matches.length === 0) return;
      const wrapped = ((pos % matches.length) + matches.length) % matches.length;
      setMatchPos(wrapped);
      const el = scrollRef.current;
      if (!el) return;
      const target = matches[wrapped] * rowHeight - el.clientHeight / 2;
      stickRef.current = false;
      el.scrollTop = Math.max(0, target);
    },
    [matches, rowHeight],
  );

  // ── ⌘F 打开搜索 ──
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        setSearchOpen(true);
        requestAnimationFrame(() => searchInputRef.current?.focus());
      }
      if (e.key === "Escape" && searchOpen) setSearchOpen(false);
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [searchOpen]);

  const clear = useCallback(() => {
    rowsRef.current = [];
    lastLevelRef.current = "INFO";
    setTick((t) => t + 1);
  }, []);

  const toggleLevel = useCallback((lv: Level) => {
    setEnabled((prev) => {
      const next = new Set(prev);
      if (next.has(lv)) next.delete(lv);
      else next.add(lv);
      // 全空等于全显，避免误点成空白
      return next.size === 0 ? new Set(LEVELS) : next;
    });
  }, []);

  // 计数（toolbar 右侧显示各等级条数）
  const counts = useMemo(() => {
    const c: Record<Level, number> = { ERROR: 0, WARN: 0, INFO: 0, DEBUG: 0, TRACE: 0 };
    for (const r of rowsRef.current) c[r.level]++;
    return c;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rowsRef.current.length, visibleRows.length]);

  // ── 虚拟窗口 ──
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - OVERSCAN);
  const end = Math.min(visibleRows.length, Math.ceil((scrollTop + viewH) / rowHeight) + OVERSCAN);
  const slice: { row: Row; i: number; matched: boolean; current: boolean }[] = [];
  const matchSet = useMemo(() => new Set(matches), [matches]);
  const currentMatchRow = matches[matchPos];
  for (let i = start; i < end; i++) {
    slice.push({
      row: visibleRows[i],
      i,
      matched: matchSet.has(i),
      current: i === currentMatchRow,
    });
  }

  return (
    <div
      className={`flex h-full flex-col overflow-hidden rounded-lg border border-white/10 bg-[#0b0b0c] ${className ?? ""}`}
    >
      {/* 工具栏 */}
      <div className="flex shrink-0 items-center gap-1.5 border-b border-white/10 bg-[#121214] px-2.5 py-1.5">
        {/* 等级过滤 */}
        <div className="flex items-center gap-1">
          {LEVELS.map((lv) => {
            const on = enabled.has(lv);
            const st = LEVEL_STYLE[lv];
            return (
              <button
                key={lv}
                type="button"
                onClick={() => toggleLevel(lv)}
                title={`${lv} · ${counts[lv]} 条`}
                className={`rounded px-1.5 py-0.5 text-[10px] font-semibold tracking-wide tabular-nums transition-colors ${
                  on ? st.text : "text-white/25"
                }`}
                style={{ background: on ? st.badgeBg : "transparent" }}
              >
                {lv}
                <span className="ml-1 opacity-60">{counts[lv]}</span>
              </button>
            );
          })}
        </div>

        <div className="mx-1 h-4 w-px bg-white/10" />

        {/* 搜索框（常驻在工具栏，⌘F 聚焦） */}
        <div className="flex min-w-0 flex-1 items-center gap-1.5">
          <Search className="h-3.5 w-3.5 shrink-0 text-white/35" />
          <input
            ref={searchInputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") jumpTo(e.shiftKey ? matchPos - 1 : matchPos + 1);
            }}
            placeholder="搜索日志…"
            className="min-w-0 flex-1 bg-transparent text-[12px] text-white/85 outline-none placeholder:text-white/25"
          />
          {query && (
            <span className={`shrink-0 text-[11px] tabular-nums ${matches.length ? "text-white/45" : "text-red-400"}`}>
              {matches.length ? `${matchPos + 1}/${matches.length}` : "无匹配"}
            </span>
          )}
          <button
            type="button"
            onClick={() => setCaseSensitive((v) => !v)}
            title="区分大小写"
            className={`rounded px-1 py-0.5 text-[10px] font-bold transition-colors ${
              caseSensitive ? "bg-white/15 text-white/80" : "text-white/35 hover:text-white/70"
            }`}
          >
            Aa
          </button>
          <button
            type="button"
            onClick={() => jumpTo(matchPos - 1)}
            disabled={!matches.length}
            title="上一个 (Shift+Enter)"
            className="rounded p-0.5 text-white/40 hover:text-white/80 disabled:opacity-30"
          >
            <ChevronUp className="h-3.5 w-3.5" />
          </button>
          <button
            type="button"
            onClick={() => jumpTo(matchPos + 1)}
            disabled={!matches.length}
            title="下一个 (Enter)"
            className="rounded p-0.5 text-white/40 hover:text-white/80 disabled:opacity-30"
          >
            <ChevronDown className="h-3.5 w-3.5" />
          </button>
          {query && (
            <button
              type="button"
              onClick={() => setQuery("")}
              className="rounded p-0.5 text-white/40 hover:text-white/80"
              title="清除搜索"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          )}
        </div>

        <div className="mx-1 h-4 w-px bg-white/10" />

        <button
          type="button"
          onClick={() => {
            const next = !autoScroll;
            setAutoScroll(next);
            if (next) {
              stickRef.current = true;
              const el = scrollRef.current;
              if (el) el.scrollTop = el.scrollHeight;
            }
          }}
          title={autoScroll ? "自动滚动：开" : "自动滚动：关"}
          className={`rounded p-1 transition-colors ${
            autoScroll ? "text-sky-400 hover:bg-white/10" : "text-white/35 hover:bg-white/10 hover:text-white/70"
          }`}
        >
          <ArrowDownToLine className="h-3.5 w-3.5" />
        </button>
        <button
          type="button"
          onClick={clear}
          title="清空"
          className="rounded p-1 text-white/35 transition-colors hover:bg-white/10 hover:text-white/70"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </button>
      </div>

      {/* 日志列表（虚拟滚动 + 双向滚动） */}
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="relative flex-1 overflow-auto font-mono leading-none"
        style={{ fontSize }}
      >
        <div style={{ height: totalH, minWidth: "100%", position: "relative" }}>
          {slice.map(({ row, i, matched, current }) => {
            const st = LEVEL_STYLE[row.level];
            return (
              <div
                key={i}
                className={`absolute left-0 flex w-full items-center whitespace-pre pr-4 ${
                  current ? "bg-yellow-400/10" : matched ? "bg-yellow-400/[0.04]" : "hover:bg-white/[0.03]"
                }`}
                style={{ top: i * rowHeight, height: rowHeight }}
              >
                <span className="h-full w-[3px] shrink-0" style={{ background: st.bar }} />
                <span className="w-12 shrink-0 select-none pl-2 text-right text-white/20 tabular-nums">{i + 1}</span>
                <span className="w-[92px] shrink-0 select-none pl-2 text-white/35 tabular-nums">{row.ts}</span>
                <span className={`w-[46px] shrink-0 select-none pl-1 font-semibold ${st.text}`}>{row.level}</span>
                <span className="pl-2 text-white/80">{renderBody(row.body, query, caseSensitive)}</span>
              </div>
            );
          })}
        </div>
        {visibleRows.length === 0 && (
          <div className="absolute inset-0 flex items-center justify-center text-[12px] text-white/30">
            暂无日志
          </div>
        )}
      </div>
    </div>
  );
}
