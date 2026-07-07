// 内置终端 UI 主体（架构 §8 内置终端）。内嵌（sidebar）与 popout（独立窗口）
// 共用同一份；唯一差别是「自己是不是当前活跃视图」——由 Rust 的 active_view
// 决定（terminal://view 事件 + terminal_list 初值）。让位视图卸载 xterm、显示占位，
// 避免两个 xterm 各自 fit() 来回 resize 同一 PTY。
//
// PTY 在 Rust 端是单一真理源；本组件的 xterm 只是视图，靠 terminal_attach 回放
// scrollback 重建画面，切视图 / reload 都不丢数据。
import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { ChevronRight, Circle, Folder, Maximize2, Plus, SquareTerminal, X } from "lucide-react";
import "@xterm/xterm/css/xterm.css";
import { api } from "@/desktop/bridge/tauri";
import { listen } from "@/desktop/bridge/transport";
import { cn } from "@/desktop/ui/lib/utils";

type ViewOwner = "embedded" | "popout";

interface TermMeta {
  id: string;
  cwd: string;
  alive: boolean;
}

interface TermView {
  term: Terminal;
  fit: FitAddon;
  resizeObserver: ResizeObserver;
}

/** PTY 输出是 raw 字节（base64），可能含半个 UTF-8 字符——交 xterm 按字节解码。 */
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

/**
 * 对正在消失的 PTY 的 fire-and-forget 调用（write / resize）：xterm 的 onData/onResize
 * 回调存活会跨越终端 close 那一刻，对已 remove 的 id 调用时 Rust 返回「终端不存在」。
 * 这是 UI 回调寿命长于 PTY 的固有竞态，静默忽略即可——不该冒泡成未处理的 rejection。
 */
function fireForget(p: Promise<unknown>) {
  void p.catch(() => {});
}

const DARK_THEME = {
  background: "#090b10",
  foreground: "#d7dde8",
  cursor: "#7dd3fc",
  cursorAccent: "#090b10",
  selectionBackground: "#25415f",
  black: "#111827",
  red: "#f87171",
  green: "#86efac",
  yellow: "#fde68a",
  blue: "#93c5fd",
  magenta: "#d8b4fe",
  cyan: "#67e8f9",
  white: "#e5e7eb",
  brightBlack: "#4b5563",
  brightRed: "#fca5a5",
  brightGreen: "#bbf7d0",
  brightYellow: "#fef3c7",
  brightBlue: "#bfdbfe",
  brightMagenta: "#e9d5ff",
  brightCyan: "#a5f3fc",
  brightWhite: "#ffffff",
};

const TERMINAL_CHROME = {
  red: "#ff5f57",
  yellow: "#febc2e",
  green: "#28c840",
};

interface TerminalSurfaceProps {
  /** "embedded" = sidebar 内嵌；"popout" = 独立窗口。决定本视图归谁活跃。 */
  variant: ViewOwner;
  /** 内嵌视图：当前 sidebar 终端 tab 是否可见（用于切回时 fit）。popout 恒 true。 */
  active?: boolean;
  /** 新建子终端的默认 cwd（内嵌传当前会话 workdir；popout 无会话传 null → $HOME）。 */
  defaultCwd?: string | null;
}

export function TerminalSurface({ variant, active = true, defaultCwd = null }: TerminalSurfaceProps) {
  const [terminals, setTerminals] = useState<TermMeta[]>([]);
  const [activeTermId, setActiveTermId] = useState<string | null>(null);
  const [activeView, setActiveView] = useState<ViewOwner>(variant);

  const isCeded = activeView !== variant;
  const activeTerm = terminals.find((t) => t.id === activeTermId) ?? null;
  const liveCount = terminals.filter((t) => t.alive).length;

  // xterm 实例 / host DOM，按 termId 索引（不进 React state，避免重渲染）。
  const viewsRef = useRef<Map<string, TermView>>(new Map());
  const hostsRef = useRef<Map<string, HTMLDivElement>>(new Map());
  const terminalsRef = useRef<TermMeta[]>([]);
  terminalsRef.current = terminals;
  // 初始化 effect（[] deps）里读最新 active，避免把它写进依赖触发重订阅。
  const activeRef = useRef(active);
  activeRef.current = active;

  // ── 键盘策略（架构 §5.2）：终端聚焦时除 Cmd 白名单外按键全透传 PTY ──
  const installKeyHandler = useCallback((term: Terminal, termId: string) => {
    term.attachCustomKeyEventHandler((e: KeyboardEvent) => {
      if (e.type !== "keydown") return true;
      if (e.isComposing) return true; // IME 组合输入交 xterm 原生处理

      // Alt + 方向 / 退格 → readline 词级操作（iTerm 默认 profile 同款）
      if (e.altKey && !e.metaKey && !e.ctrlKey) {
        const seq =
          e.key === "ArrowLeft"
            ? "\x1bb"
            : e.key === "ArrowRight"
              ? "\x1bf"
              : e.key === "Backspace"
                ? "\x1b\x7f"
                : null;
        if (seq) {
          e.preventDefault();
          void api.terminalWrite(termId, seq);
          return false;
        }
      }

      if (e.metaKey) {
        // Cmd 白名单由终端自己处理；其余 Cmd 组合放行给应用（菜单 / 全局快捷键）
        const k = e.key.toLowerCase();
        if (k === "c") {
          const sel = term.getSelection();
          if (sel) {
            e.preventDefault();
            void navigator.clipboard?.writeText(sel);
          }
          return false; // 无选区也不发 SIGINT（那是 Ctrl+C 的事）
        }
        if (k === "v") {
          e.preventDefault();
          void navigator.clipboard?.readText().then((text) => {
            if (text) void api.terminalWrite(termId, `\x1b[200~${text}\x1b[201~`);
          });
          return false;
        }
        if (k === "k") {
          e.preventDefault();
          term.clear();
          return false;
        }
        if (e.key === "ArrowLeft") {
          e.preventDefault();
          void api.terminalWrite(termId, "\x01"); // 行首
          return false;
        }
        if (e.key === "ArrowRight") {
          e.preventDefault();
          void api.terminalWrite(termId, "\x05"); // 行尾
          return false;
        }
        if (e.key === "Backspace") {
          e.preventDefault();
          void api.terminalWrite(termId, "\x15"); // 删整行
          return false;
        }
        return false; // 其余 Cmd 组合：xterm 不吃，冒泡给应用
      }

      return true; // 不带 Cmd 的一切（Ctrl-* / Tab / Esc / 方向键）→ 透传 PTY
    });
  }, []);

  // ── 挂载单个子终端的 xterm + attach 回放 ──
  const mountTerm = useCallback(
    async (termId: string) => {
      if (viewsRef.current.has(termId)) return;
      const host = hostsRef.current.get(termId);
      if (!host) return;

      const term = new Terminal({
        fontFamily:
          getComputedStyle(document.documentElement).getPropertyValue("--font-mono").trim() ||
          "monospace",
        fontSize: 13,
        lineHeight: 1.24,
        cursorBlink: true,
        cursorStyle: "block",
        cursorWidth: 1,
        scrollback: 10000,
        allowProposedApi: true,
        macOptionIsMeta: true,
        minimumContrastRatio: 4.5,
        rightClickSelectsWord: true,
        smoothScrollDuration: 80,
        theme: DARK_THEME,
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      try {
        const u11 = new Unicode11Addon();
        term.loadAddon(u11);
        term.unicode.activeVersion = "11";
      } catch {
        /* unicode11 不可用时回退默认宽度 */
      }
      // 渲染器：用 xterm 6 内建的 DOM renderer（不 load 任何 renderer addon）。
      // 不用 WebGL：WKWebView 的 WebGL2 在全屏重绘（vim / htop）时会抛逃逸出
      // mountTerm try 的渲染帧异常，整屏崩成 uncaught ReferenceError。DOM renderer
      // 不依赖 GPU、绝对稳定，性能对终端日常使用足够。canvas renderer 已被 xterm 6 移除。

      term.open(host);
      installKeyHandler(term, termId);

      // 用户输入 → PTY（普通键 xterm 编码后走这里；Alt/Cmd 特例已在 key handler 直写）
      term.onData((data) => {
        fireForget(api.terminalWrite(termId, data));
      });
      // xterm 尺寸变化（fit 触发）→ 同步 PTY
      term.onResize(({ cols, rows }) => {
        fireForget(api.terminalResize(termId, cols, rows));
      });

      // 选中自动复制（copy-on-select），100ms debounce
      let selTimer: number | undefined;
      term.onSelectionChange(() => {
        window.clearTimeout(selTimer);
        selTimer = window.setTimeout(() => {
          const sel = term.getSelection();
          if (sel) void navigator.clipboard?.writeText(sel);
        }, 100);
      });

      // attach 回放：拉 scrollback 重建画面
      try {
        const { scrollback_b64 } = await api.terminalAttach(termId);
        if (scrollback_b64) term.write(b64ToBytes(scrollback_b64));
      } catch {
        /* 终端可能已被关闭 */
      }

      fit.fit();
      fireForget(api.terminalResize(termId, term.cols, term.rows));

      const resizeObserver = new ResizeObserver(() => {
        try {
          fit.fit();
        } catch {
          /* host 尺寸为 0 时 fit 抛错，忽略 */
        }
      });
      resizeObserver.observe(host);

      viewsRef.current.set(termId, { term, fit, resizeObserver });
    },
    [installKeyHandler],
  );

  const unmountTerm = useCallback((termId: string) => {
    const view = viewsRef.current.get(termId);
    if (!view) return;
    view.resizeObserver.disconnect();
    view.term.dispose();
    viewsRef.current.delete(termId);
  }, []);

  const unmountAll = useCallback(() => {
    for (const id of [...viewsRef.current.keys()]) unmountTerm(id);
  }, [unmountTerm]);

  const refreshList = useCallback(async () => {
    const res = await api.terminalList();
    setTerminals(res.terminals);
    setActiveView(res.active_view);
    setActiveTermId((prev) => {
      if (prev && res.terminals.some((t) => t.id === prev)) return prev;
      return res.terminals[0]?.id ?? null;
    });
    return res;
  }, []);

  const openTerminal = useCallback(async () => {
    const id = await api.terminalOpen(defaultCwd, 80, 24);
    await refreshList();
    setActiveTermId(id);
  }, [defaultCwd, refreshList]);

  const closeTerminal = useCallback(
    async (termId: string) => {
      unmountTerm(termId);
      await api.terminalClose(termId);
      await refreshList();
    },
    [refreshList, unmountTerm],
  );

  // ── 初始化：拉列表 + 订阅事件 ──
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const res = await refreshList();
      if (cancelled) return;
      // 自动开终端只看 Rust 端真实列表（全局单例常驻），且仅当本视图正显示终端时。
      // 基于真实列表而非 state 初值 []，所以折叠→展开重挂时若已有终端不会再新建。
      if (res.active_view === variant && activeRef.current && res.terminals.length === 0) {
        void openTerminal();
      }
    })();

    const unlistens: Array<Promise<() => void>> = [];
    unlistens.push(
      listen<{ id: string; data_b64: string }>("terminal://output", (e) => {
        const view = viewsRef.current.get(e.payload.id);
        if (view) view.term.write(b64ToBytes(e.payload.data_b64));
      }),
    );
    unlistens.push(
      listen<{ id: string }>("terminal://exit", (e) => {
        setTerminals((prev) =>
          prev.map((t) => (t.id === e.payload.id ? { ...t, alive: false } : t)),
        );
      }),
    );
    unlistens.push(
      listen<{ owner: ViewOwner }>("terminal://view", (e) => {
        setActiveView(e.payload.owner);
      }),
    );

    return () => {
      cancelled = true;
      unmountAll();
      for (const p of unlistens) void p.then((un) => un());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── 让位 / 接管：活跃视图才挂 xterm；让位时全卸载显示占位 ──
  useEffect(() => {
    if (isCeded) {
      unmountAll();
      return;
    }
    // 挂载所有应在的子终端，卸载已不在列表的
    for (const t of terminals) void mountTerm(t.id);
    for (const id of [...viewsRef.current.keys()]) {
      if (!terminals.some((t) => t.id === id)) unmountTerm(id);
    }
  }, [isCeded, terminals, mountTerm, unmountTerm, unmountAll]);

  // 切回可见 / 切换子 tab 时：fit + 聚焦（xterm 6 已修复隐藏期滚动区 bug，无需私有 hack）
  useEffect(() => {
    if (isCeded || !active || !activeTermId) return;
    const view = viewsRef.current.get(activeTermId);
    if (!view) return;
    const raf = requestAnimationFrame(() => {
      try {
        view.fit.fit();
        view.term.scrollToBottom();
        view.term.focus();
      } catch {
        /* ignore */
      }
    });
    return () => cancelAnimationFrame(raf);
  }, [isCeded, active, activeTermId, terminals]);

  if (isCeded) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-4 bg-[#090b10] p-6 text-center text-slate-300">
        <div className="rounded-2xl border border-white/10 bg-white/[0.04] p-4 shadow-2xl shadow-black/40">
          <SquareTerminal className="h-8 w-8 text-sky-300" />
        </div>
        <div className="space-y-1">
          <p className="text-sm font-medium text-slate-100">终端已在独立窗口打开</p>
          <p className="text-xs text-slate-500">收回后会继续显示同一个 shell</p>
        </div>
        <button
          type="button"
          onClick={() => void api.terminalClosePopout()}
          className="rounded-lg border border-white/10 bg-white/[0.06] px-3 py-1.5 text-xs text-slate-200 shadow-sm hover:bg-white/[0.1]"
        >
          收回到这里
        </button>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col overflow-hidden bg-[#05070b] text-slate-200">
      <div className="flex h-10 shrink-0 items-center gap-3 border-b border-white/10 bg-gradient-to-b from-[#191d27] to-[#10131b] px-3 shadow-[0_1px_0_rgba(255,255,255,0.05)_inset]">
        <div className="flex shrink-0 items-center gap-1.5" aria-hidden="true">
          <span className="h-3 w-3 rounded-full shadow-inner" style={{ backgroundColor: TERMINAL_CHROME.red }} />
          <span className="h-3 w-3 rounded-full shadow-inner" style={{ backgroundColor: TERMINAL_CHROME.yellow }} />
          <span className="h-3 w-3 rounded-full shadow-inner" style={{ backgroundColor: TERMINAL_CHROME.green }} />
        </div>
        <div className="flex min-w-0 flex-1 items-center gap-2 text-xs">
          <SquareTerminal className="h-3.5 w-3.5 shrink-0 text-sky-300" />
          <span className="truncate font-medium text-slate-100">{activeTerm ? baseName(activeTerm.cwd) : "Terminal"}</span>
          {activeTerm && (
            <span className="hidden min-w-0 items-center gap-1 truncate text-slate-500 sm:flex" title={activeTerm.cwd}>
              <ChevronRight className="h-3 w-3 shrink-0" />
              <span className="truncate">{compactPath(activeTerm.cwd)}</span>
            </span>
          )}
        </div>
        <div className="hidden shrink-0 items-center gap-2 text-[11px] text-slate-500 md:flex">
          {activeTerm && (
            <span className="rounded-full border border-white/10 bg-white/[0.04] px-2 py-0.5 tabular-nums">
              {activeTerm.alive ? "运行中" : "已退出"}
            </span>
          )}
          <span className="rounded-full border border-white/10 bg-white/[0.04] px-2 py-0.5 tabular-nums">
            {liveCount}/{terminals.length || 0}
          </span>
        </div>
        {variant === "embedded" && (
          <button
            type="button"
            onClick={() => void api.terminalPopout()}
            title="在独立窗口打开"
            className="grid h-7 w-7 shrink-0 place-items-center rounded-md text-slate-400 transition-colors hover:bg-white/10 hover:text-slate-100"
          >
            <Maximize2 className="h-3.5 w-3.5" />
          </button>
        )}
      </div>

      {/* 子终端 tab 条 */}
      <div className="flex h-9 shrink-0 items-stretch border-b border-white/10 bg-[#0b0f17]">
        <div className="flex min-w-0 flex-1 items-end gap-1 overflow-x-auto px-2 pt-1 [scrollbar-width:thin]">
          {terminals.map((t) => (
            <button
              key={t.id}
              type="button"
              onClick={() => setActiveTermId(t.id)}
              title={t.cwd}
              className={cn(
                "group relative inline-flex h-7 max-w-[180px] shrink-0 items-center gap-1.5 rounded-t-lg border px-2.5 pr-7 text-[12px] transition-colors",
                t.id === activeTermId
                  ? "border-white/10 border-b-[#090b10] bg-[#090b10] text-slate-100 shadow-[0_-1px_0_rgba(255,255,255,0.06)_inset]"
                  : "border-transparent bg-white/[0.03] text-slate-500 hover:bg-white/[0.07] hover:text-slate-200",
                !t.alive && "opacity-60",
              )}
            >
              <Circle
                className={cn("h-2 w-2 shrink-0", t.alive ? "fill-emerald-400 text-emerald-400" : "fill-slate-600 text-slate-600")}
              />
              <span className="truncate">{baseName(t.cwd)}</span>
              {!t.alive && <span className="shrink-0 text-[10px] text-slate-500">已退出</span>}
              <span
                role="button"
                tabIndex={-1}
                onClick={(e) => {
                  e.stopPropagation();
                  void closeTerminal(t.id);
                }}
                title="关闭"
                className="absolute right-1.5 top-1/2 grid h-4 w-4 -translate-y-1/2 place-items-center rounded text-slate-500 opacity-0 transition-opacity hover:bg-white/10 hover:text-slate-100 group-hover:opacity-100"
              >
                <X className="h-2.5 w-2.5" />
              </span>
            </button>
          ))}
        </div>
        <div className="flex shrink-0 items-center border-l border-white/10 px-1.5">
          <button
            type="button"
            onClick={() => void openTerminal()}
            title="新终端"
            className="grid h-7 w-7 place-items-center rounded-md text-slate-400 transition-colors hover:bg-white/10 hover:text-slate-100"
          >
            <Plus className="h-4 w-4" />
          </button>
        </div>
      </div>

      {/* 终端渲染区：所有子终端 host 共存于 DOM，非激活的隐藏 */}
      <div className="relative min-h-0 flex-1 bg-[#090b10]">
        <div className="pointer-events-none absolute inset-x-0 top-0 z-10 h-4 bg-gradient-to-b from-black/25 to-transparent" />
        {terminals.map((t) => (
          <div
            key={t.id}
            data-terminal-root
            ref={(el) => {
              if (el) hostsRef.current.set(t.id, el);
              else hostsRef.current.delete(t.id);
            }}
            className={cn(
              "absolute inset-0 p-3 [&_.xterm]:h-full [&_.xterm-screen]:rounded-md [&_.xterm-viewport]:!bg-transparent [&_.xterm-viewport::-webkit-scrollbar]:w-2 [&_.xterm-viewport::-webkit-scrollbar-thumb]:rounded-full [&_.xterm-viewport::-webkit-scrollbar-thumb]:bg-white/20 [&_.xterm-viewport::-webkit-scrollbar-track]:bg-transparent",
              t.id !== activeTermId && "hidden",
            )}
          />
        ))}
        {terminals.length === 0 && (
          <div className="flex h-full flex-col items-center justify-center gap-3 text-center text-slate-500">
            <div className="rounded-2xl border border-white/10 bg-white/[0.04] p-4">
              <Folder className="h-7 w-7 text-slate-400" />
            </div>
            <div className="space-y-1">
              <p className="text-sm text-slate-300">还没有终端</p>
              <p className="text-xs">点右上角 + 新建一个 shell</p>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function baseName(path: string): string {
  if (!path) return "shell";
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || "/";
}

function compactPath(path: string): string {
  if (!path) return "";
  const normalized = path.replace(/\/+$/, "");
  const parts = normalized.split("/").filter(Boolean);
  if (parts.length <= 3) return normalized || "/";
  const prefix = normalized.startsWith("/") ? "/" : "";
  return `${prefix}…/${parts.slice(-2).join("/")}`;
}
