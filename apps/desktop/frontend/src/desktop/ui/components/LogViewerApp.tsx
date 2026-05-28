import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { init as initGhostty, Terminal, FitAddon } from "ghostty-web";
import { Pin, PinOff, Maximize2, Minimize2, Search, X, Regex, ChevronUp, ChevronDown } from "lucide-react";

// ── ANSI helpers ──────────────────────────────────────────────────────

const LEVEL_ANSI: Record<string, string> = {
  ERROR: "\x1b[31m",
  WARN: "\x1b[33m",
  INFO: "\x1b[32m",
  DEBUG: "\x1b[34m",
  TRACE: "\x1b[90m",
};

function colorizeLogLine(line: string): string {
  return line.replace(/\b(ERROR|WARN|INFO|DEBUG|TRACE)\b/, (level) => `${LEVEL_ANSI[level]}${level}\x1b[0m`);
}

function formatLiveLogLine(line: { ts: string; level: string; target: string; message: string }): string {
  const color = LEVEL_ANSI[line.level] ?? "\x1b[0m";
  return `${line.ts} ${color}[${line.level}]\x1b[0m \x1b[2m${line.target}\x1b[0m ${line.message}`;
}

/** Strip ANSI escape sequences for plain-text search. */
function stripAnsi(s: string): string {
  // eslint-disable-next-line no-control-regex
  return s.replace(/\x1b\[[0-9;]*m/g, "");
}

// ── WASM singleton ────────────────────────────────────────────────────

let _wasmReady: Promise<void> | null = null;
function ensureWasm() {
  if (!_wasmReady) _wasmReady = initGhostty();
  return _wasmReady;
}

// ── Search bar ────────────────────────────────────────────────────────

interface SearchBarProps {
  onClose: () => void;
  onSearch: (query: string, opts: { regex: boolean; caseSensitive: boolean }) => void;
  onPrev: () => void;
  onNext: () => void;
  matchIndex: number;
  matchTotal: number;
}

function SearchBar({ onClose, onSearch, onPrev, onNext, matchIndex, matchTotal }: SearchBarProps) {
  const [query, setQuery] = useState("");
  const [regex, setRegex] = useState(false);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    onSearch(query, { regex, caseSensitive });
  }, [query, regex, caseSensitive]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (e.key === "Enter" && e.shiftKey) onPrev();
      else if (e.key === "Enter") onNext();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose, onPrev, onNext]);

  return (
    <div className="flex items-center gap-1.5 rounded-md border border-border bg-background/95 px-2 py-1.5 shadow-lg backdrop-blur">
      <Search className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
      <input
        ref={inputRef}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder="搜索日志…"
        className="w-56 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground"
      />
      {query && matchTotal > 0 && (
        <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
          {matchIndex + 1}/{matchTotal}
        </span>
      )}
      {query && matchTotal === 0 && (
        <span className="shrink-0 text-[11px] text-destructive">无匹配</span>
      )}
      <div className="mx-1 h-4 w-px bg-border" />
      <button
        type="button"
        onClick={() => setRegex(!regex)}
        className={`rounded p-0.5 transition-colors ${regex ? "bg-accent text-accent-foreground" : "text-muted-foreground hover:text-foreground"}`}
        title="正则表达式"
      >
        <Regex className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={() => setCaseSensitive(!caseSensitive)}
        className={`rounded px-1 py-0.5 text-[10px] font-bold transition-colors ${caseSensitive ? "bg-accent text-accent-foreground" : "text-muted-foreground hover:text-foreground"}`}
        title="区分大小写"
      >
        Aa
      </button>
      <div className="mx-1 h-4 w-px bg-border" />
      <button
        type="button"
        onClick={onPrev}
        className="rounded p-0.5 text-muted-foreground hover:text-foreground"
        title="上一个 (Shift+Enter)"
      >
        <ChevronUp className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={onNext}
        className="rounded p-0.5 text-muted-foreground hover:text-foreground"
        title="下一个 (Enter)"
      >
        <ChevronDown className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={onClose}
        className="rounded p-0.5 text-muted-foreground hover:text-foreground"
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}

// ── Main LogViewerApp ─────────────────────────────────────────────────

export default function LogViewerApp() {
  const [alwaysOnTop, setAlwaysOnTop] = useState(true);
  const [searchOpen, setSearchOpen] = useState(false);
  const [matchIndex, setMatchIndex] = useState(0);
  const [matchTotal, setMatchTotal] = useState(0);

  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  // Raw log lines (with ANSI) for search & re-render
  const rawLinesRef = useRef<string[]>([]);
  // Parsed search matches: indices into rawLinesRef
  const matchesRef = useRef<number[]>([]);

  // ── Terminal init ──
  useEffect(() => {
    let active = true;
    let cancelStream: (() => void) | null = null;

    ensureWasm().then(async () => {
      if (!active || !containerRef.current) return;

      const term = new Terminal({
        fontSize: 12,
        fontFamily: "ui-monospace, 'Cascadia Code', Menlo, Consolas, monospace",
        theme: {
          background: "#0a0a0a",
          foreground: "#cccccc",
          black: "#1e1e1e", brightBlack: "#808080",
          red: "#f44747", brightRed: "#f44747",
          green: "#6a9955", brightGreen: "#b5cea8",
          yellow: "#dcdcaa", brightYellow: "#dcdcaa",
          blue: "#569cd6", brightBlue: "#9cdcfe",
          magenta: "#c586c0", brightMagenta: "#c586c0",
          cyan: "#4ec9b0", brightCyan: "#4ec9b0",
          white: "#d4d4d4", brightWhite: "#ffffff",
        },
        scrollback: 100000,
        disableStdin: true,
        convertEol: true,
      });

      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(containerRef.current);
      fit.fit();
      fit.observeResize();
      termRef.current = term;

      // Load historical log file
      try {
        const text = await invoke<string>("read_log_file");
        if (active && text.trim()) {
          const lines = text.split("\n");
          const formattedLines = lines.map(colorizeLogLine);
          rawLinesRef.current.push(...formattedLines);
          term.write(formattedLines.join("\r\n"));
          term.scrollToBottom();
        }
      } catch {}

      // Subscribe to real-time log stream
      const { Channel } = await import("@tauri-apps/api/core");
      const channel = new Channel<{
        level: string; target: string; message: string; ts: string;
      }>();
      channel.onmessage = (line) => {
        if (!active) return;
        const formatted = formatLiveLogLine(line);
        rawLinesRef.current.push(formatted);
        term.write(formatted + "\r\n");
        term.scrollToBottom();
      };
      invoke("subscribe_log_stream", { onLog: channel }).catch(() => {});
      cancelStream = () => { active = false; };
    }).catch(() => {});

    return () => {
      active = false;
      cancelStream?.();
      termRef.current?.dispose();
      termRef.current = null;
    };
  }, []);

  // ── Always-on-top toggle ──
  const toggleAlwaysOnTop = useCallback(async () => {
    const next = !alwaysOnTop;
    setAlwaysOnTop(next);
    try {
      await invoke("set_log_viewer_always_on_top", { alwaysOnTop: next });
    } catch {}
  }, [alwaysOnTop]);

  // ── Search logic ──
  const handleSearch = useCallback((query: string, opts: { regex: boolean; caseSensitive: boolean }) => {
    if (!query) {
      matchesRef.current = [];
      setMatchTotal(0);
      setMatchIndex(0);
      // Re-render original content
      const term = termRef.current;
      if (term) {
        term.clear();
        for (const line of rawLinesRef.current) {
          term.write(line + "\r\n");
        }
      }
      return;
    }

    const lines = rawLinesRef.current;
    const indices: number[] = [];

    let matcher: (s: string) => boolean;
    if (opts.regex) {
      try {
        const flags = opts.caseSensitive ? "" : "i";
        const re = new RegExp(query, flags);
        matcher = (s) => re.test(stripAnsi(s));
      } catch {
        // Invalid regex — no matches
        matchesRef.current = [];
        setMatchTotal(0);
        setMatchIndex(0);
        return;
      }
    } else {
      const q = opts.caseSensitive ? query : query.toLowerCase();
      matcher = (s) => {
        const plain = stripAnsi(s);
        return opts.caseSensitive ? plain.includes(q) : plain.toLowerCase().includes(q);
      };
    }

    for (let i = 0; i < lines.length; i++) {
      if (matcher(lines[i])) indices.push(i);
    }

    matchesRef.current = indices;
    setMatchTotal(indices.length);
    setMatchIndex(indices.length > 0 ? 0 : -1);

    // Re-render: show only matching lines (with context)
    const term = termRef.current;
    if (term) {
      term.clear();
      if (indices.length === 0) {
        term.write("\x1b[90m没有匹配的日志行\x1b[0m\r\n");
      } else {
        // Show matching lines with ±2 context lines
        const showSet = new Set<number>();
        for (const idx of indices) {
          for (let d = -2; d <= 2; d++) showSet.add(idx + d);
        }
        const sorted = [...showSet].filter(i => i >= 0 && i < lines.length).sort((a, b) => a - b);
        let prev = -1;
        for (const i of sorted) {
          if (prev >= 0 && i > prev + 1) {
            term.write("\x1b[90m  ···\x1b[0m\r\n");
          }
          const marker = indices.includes(i) ? "\x1b[43;30m►\x1b[0m " : "  ";
          term.write(`${marker}${lines[i]}\r\n`);
          prev = i;
        }
      }
    }
  }, []);

  const jumpToMatch = useCallback((idx: number) => {
    const matches = matchesRef.current;
    if (matches.length === 0) return;
    const wrapped = ((idx % matches.length) + matches.length) % matches.length;
    setMatchIndex(wrapped);
    // Re-render with the target match highlighted
    // For simplicity, just scroll context around the match
    const term = termRef.current;
    if (!term) return;
    const targetLine = matches[wrapped];
    const lines = rawLinesRef.current;
    const start = Math.max(0, targetLine - 10);
    const end = Math.min(lines.length, targetLine + 20);
    term.clear();
    for (let i = start; i < end; i++) {
      const marker = i === targetLine ? "\x1b[43;30m►\x1b[0m " : "  ";
      term.write(`${marker}${lines[i]}\r\n`);
    }
  }, []);

  // ── Keyboard shortcuts ──
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  return (
    <div className="flex h-screen flex-col bg-[#0a0a0a] text-foreground">
      {/* Title bar */}
      <div className="flex h-9 shrink-0 items-center justify-between border-b border-white/10 bg-[#0a0a0a] px-3"
        style={{ WebkitAppRegion: "drag" } as React.CSSProperties}
      >
        <span className="text-xs font-medium text-white/60">日志查看器</span>
        <div className="flex items-center gap-1" style={{ WebkitAppRegion: "no-drag" } as React.CSSProperties}>
          <button
            type="button"
            onClick={() => setSearchOpen(!searchOpen)}
            className="rounded p-1 text-white/50 hover:bg-white/10 hover:text-white/80"
            title="搜索 (⌘F)"
          >
            <Search className="h-3.5 w-3.5" />
          </button>
          <button
            type="button"
            onClick={toggleAlwaysOnTop}
            className={`rounded p-1 transition-colors ${alwaysOnTop ? "text-blue-400 hover:bg-white/10" : "text-white/50 hover:bg-white/10 hover:text-white/80"}`}
            title={alwaysOnTop ? "取消置顶" : "永远置顶"}
          >
            {alwaysOnTop ? <Pin className="h-3.5 w-3.5" /> : <PinOff className="h-3.5 w-3.5" />}
          </button>
        </div>
      </div>

      {/* Search bar overlay */}
      {searchOpen && (
        <div className="absolute right-3 top-10 z-50">
          <SearchBar
            onClose={() => setSearchOpen(false)}
            onSearch={handleSearch}
            onPrev={() => jumpToMatch(matchIndex - 1)}
            onNext={() => jumpToMatch(matchIndex + 1)}
            matchIndex={matchIndex}
            matchTotal={matchTotal}
          />
        </div>
      )}

      {/* Terminal */}
      <div ref={containerRef} className="flex-1 overflow-hidden" />
    </div>
  );
}
