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
import { WebglAddon } from "@xterm/addon-webgl";
import { Plus, X, SquareTerminal, PanelRightClose } from "lucide-react";
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
  webgl: WebglAddon | null;
  resizeObserver: ResizeObserver;
}

/** PTY 输出是 raw 字节（base64），可能含半个 UTF-8 字符——交 xterm 按字节解码。 */
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

const DARK_THEME = {
  background: "#0b0c0a",
  foreground: "#d6dac9",
  cursor: "#cdf24b",
  selectionBackground: "#3a3f2e",
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

  // xterm 实例 / host DOM，按 termId 索引（不进 React state，避免重渲染）。
  const viewsRef = useRef<Map<string, TermView>>(new Map());
  const hostsRef = useRef<Map<string, HTMLDivElement>>(new Map());
  const terminalsRef = useRef<TermMeta[]>([]);
  terminalsRef.current = terminals;

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
        lineHeight: 1.2,
        cursorBlink: true,
        scrollback: 5000,
        allowProposedApi: true,
        macOptionIsMeta: true,
        minimumContrastRatio: 4.5,
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
      let webgl: WebglAddon | null = null;
      try {
        webgl = new WebglAddon();
        webgl.onContextLoss(() => {
          webgl?.dispose();
          webgl = null;
        });
        term.loadAddon(webgl);
      } catch {
        webgl = null; // 回退 DOM renderer
      }

      term.open(host);
      installKeyHandler(term, termId);

      // 用户输入 → PTY（普通键 xterm 编码后走这里；Alt/Cmd 特例已在 key handler 直写）
      term.onData((data) => {
        void api.terminalWrite(termId, data);
      });
      // xterm 尺寸变化（fit 触发）→ 同步 PTY
      term.onResize(({ cols, rows }) => {
        void api.terminalResize(termId, cols, rows);
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
      void api.terminalResize(termId, term.cols, term.rows);

      const resizeObserver = new ResizeObserver(() => {
        try {
          fit.fit();
        } catch {
          /* host 尺寸为 0 时 fit 抛错，忽略 */
        }
      });
      resizeObserver.observe(host);

      viewsRef.current.set(termId, { term, fit, webgl, resizeObserver });
    },
    [installKeyHandler],
  );

  const unmountTerm = useCallback((termId: string) => {
    const view = viewsRef.current.get(termId);
    if (!view) return;
    view.resizeObserver.disconnect();
    view.webgl?.dispose();
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
    void refreshList();

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

  // 内嵌视图首次成为活跃且无终端时，自动开一个
  useEffect(() => {
    if (isCeded) return;
    if (active && terminals.length === 0) void openTerminal();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isCeded, active, terminals.length]);

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
      <div className="flex h-full flex-col items-center justify-center gap-3 bg-muted/30 p-6 text-center">
        <SquareTerminal className="h-8 w-8 text-muted-foreground" />
        <p className="text-sm text-muted-foreground">终端已在独立窗口打开</p>
        <button
          type="button"
          onClick={() => void api.terminalClosePopout()}
          className="rounded border border-border px-3 py-1 text-xs hover:bg-accent"
        >
          收回到这里
        </button>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col bg-[#0b0c0a]">
      {/* 子终端 tab 条 */}
      <div className="flex h-8 shrink-0 items-stretch border-b border-border/40 bg-background/40">
        <div className="flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto px-1 [scrollbar-width:thin]">
          {terminals.map((t) => (
            <button
              key={t.id}
              type="button"
              onClick={() => setActiveTermId(t.id)}
              title={t.cwd}
              className={cn(
                "group inline-flex h-6 shrink-0 items-center gap-1 rounded px-2 text-[12px] transition-colors",
                t.id === activeTermId
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
                !t.alive && "opacity-50",
              )}
            >
              <SquareTerminal className="h-3 w-3" />
              <span className="max-w-[120px] truncate">{baseName(t.cwd)}</span>
              {!t.alive && <span className="text-[10px]">·已退出</span>}
              <span
                role="button"
                tabIndex={-1}
                onClick={(e) => {
                  e.stopPropagation();
                  void closeTerminal(t.id);
                }}
                className="ml-0.5 hidden rounded p-0.5 hover:bg-accent group-hover:inline-flex"
              >
                <X className="h-2.5 w-2.5" />
              </span>
            </button>
          ))}
        </div>
        <div className="flex shrink-0 items-center gap-0.5 border-l border-border/40 px-1">
          <button
            type="button"
            onClick={() => void openTerminal()}
            title="新终端"
            className="grid h-6 w-6 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <Plus className="h-3.5 w-3.5" />
          </button>
          {variant === "embedded" && (
            <button
              type="button"
              onClick={() => void api.terminalPopout()}
              title="在独立窗口打开"
              className="grid h-6 w-6 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
            >
              <PanelRightClose className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
      </div>

      {/* 终端渲染区：所有子终端 host 共存于 DOM，非激活的隐藏 */}
      <div className="relative min-h-0 flex-1">
        {terminals.map((t) => (
          <div
            key={t.id}
            data-terminal-root
            ref={(el) => {
              if (el) hostsRef.current.set(t.id, el);
              else hostsRef.current.delete(t.id);
            }}
            className={cn("absolute inset-0 p-1", t.id !== activeTermId && "hidden")}
          />
        ))}
        {terminals.length === 0 && (
          <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
            点右上角 + 新建终端
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
