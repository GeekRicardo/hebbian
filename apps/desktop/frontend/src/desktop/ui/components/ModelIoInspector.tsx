import {
  Fragment,
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import {
  X,
  RefreshCw,
  ChevronRight,
  ChevronDown,
  ChevronsUp,
  ChevronsDown,
  Copy,
  Check,
  PanelLeftClose,
  PanelLeftOpen,
  Maximize2,
  Minimize2,
  Code,
} from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { api } from "@/desktop/bridge/tauri";
import { Button } from "@/desktop/ui/components/ui/button";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";
import { FindBar, findMatches } from "./FindBar";
import { isLocalFindShortcut } from "@/desktop/ui/lib/keyboardShortcuts";

/**
 * Session 级 Model I/O 调试器。
 *
 * 痛点：MessageBubble 上的"查看原始 JSON"每条都从 systemprompt 起头展开 ——
 * 跨请求对比要在多个弹窗间切，重复信息又多。本组件改为：抽屉 + 左右两列：
 * - 左：所有 model 请求的时间线（每条 turn / 时间 / 状态 / tokens）
 * - 右：选中请求的 messages + response（diff 模式：本次新增的 message 默认展开，
 *   carried-over 默认折叠 —— 翻 30 次请求也只看你需要看的那块）
 *
 * 数据源：后端 `~/.hebbian/sessions/<sid>/model_io.jsonl`，由 `model_io_dump`
 * 在每次模型调用结束后异步落盘。**默认开启**（环境变量 `HEBBIAN_DUMP_MODEL_IO=0`
 * 才禁用），所以新 session 通常都能直接看到数据；老 session 无数据时显示空提示。
 */
interface ModelIoEntry {
  ts: string;
  run_id: string;
  turn: number;
  model: string;
  request: ModelIoRequest;
  response: ModelIoResponse;
  duration_ms: number;
  /** "main" = 主模型调用，"judge" = AutoMode 判官调用。老 jsonl 无此字段默认 "main"。 */
  kind?: string;
}

interface ModelIoRequest {
  model?: string;
  system?: string | null;
  messages: ModelIoMessage[];
  tools?: unknown[];
  max_tokens?: number;
  reasoning?: unknown;
}

interface ModelIoMessage {
  role: "user" | "assistant" | "tool" | string;
  content?: string | null;
  reasoning?: string | null;
  tool_calls?: Array<{ id: string; name: string; input: unknown }>;
  results?: Array<{ id: string; name: string; content: string }>;
  attachments?: unknown[];
}

interface ModelIoResponse {
  type: "Done" | "ToolCalls" | "Error" | string;
  text?: string;
  reasoning?: string;
  calls?: Array<{ id: string; name: string; input: unknown }>;
  attachments?: unknown[];
  usage?: {
    input_tokens: number;
    output_tokens: number;
    cache_read_tokens: number;
    cache_creation_tokens: number;
  };
  error?: string;
}

interface Props {
  sessionId: string;
  open: boolean;
  onClose: () => void;
}

export const ModelIoInspector = memo(function ModelIoInspector({
  sessionId,
  open,
  onClose,
}: Props) {
  const [entries, setEntries] = useState<ModelIoEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [selected, setSelected] = useState<number>(0);
  const [sidebarOpen, setSidebarOpen] = useState(true);

  // ─── 压缩分割栏：从 session transcript 中提取 compact_boundary 时间戳 ───
  const currentSession = useStore((s) => s.currentSession);
  const compactBoundaryTimestamps = useMemo(() => {
    if (!currentSession?.messages) return [];
    return currentSession.messages
      .filter(
        (m) => m.role === "marker" && m.meta?.type === "compact_boundary"
      )
      .map((m) => m.created_at)
      .sort((a, b) => a - b);
  }, [currentSession?.messages]);

  // 计算每个 compact_boundary 在 entries 列表中的分割位置：
  // 找到最后一个 ts < boundary 的 entry index
  const dividerAfterIndices = useMemo(() => {
    if (compactBoundaryTimestamps.length === 0) return new Set<number>();
    const result = new Set<number>();
    for (const boundaryTs of compactBoundaryTimestamps) {
      let lastIdx = -1;
      for (let i = 0; i < entries.length; i++) {
        const entryMs = new Date(entries[i].ts).getTime();
        if (entryMs < boundaryTs) {
          lastIdx = i;
        } else {
          break;
        }
      }
      if (lastIdx >= 0) result.add(lastIdx);
    }
    return result;
  }, [entries, compactBoundaryTimestamps]);
  // ─── Cmd+F 全局搜索状态 ─────────────────────────────────────────────────
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findRegex, setFindRegex] = useState(false);
  const [findCase, setFindCase] = useState(false);
  const [findActive, setFindActive] = useState(0);
  const detailRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLOListElement>(null);
  // 抽屉宽度可拖动调整；持久化到 localStorage 让下次打开记住
  const [drawerWidth, setDrawerWidth] = useState<number>(() => {
    if (typeof window === "undefined") return 700;
    const saved = Number(window.localStorage.getItem("modelIoDrawerWidth"));
    return Number.isFinite(saved) && saved >= 400 ? saved : 700;
  });
  useEffect(() => {
    if (typeof window !== "undefined") {
      window.localStorage.setItem("modelIoDrawerWidth", String(drawerWidth));
    }
  }, [drawerWidth]);

  const refresh = useCallback(async () => {
    if (!sessionId) return;
    setLoading(true);
    setErr(null);
    try {
      const data = (await api.listSessionModelIo(sessionId)) as ModelIoEntry[];
      setEntries(data);
      // 默认选最后一条（最新请求）—— 排查 bug 时常常关注最近一次
      setSelected(data.length === 0 ? 0 : data.length - 1);
    } catch (e: unknown) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }, [sessionId]);

  // 打开抽屉时拉一次；后续靠 Refresh 按钮手动刷新（避免轮询打扰 + 跑长任务时数据不变）
  useEffect(() => {
    if (open) refresh();
  }, [open, refresh]);

  const current = entries[selected];

  /**
   * find context 只透传 query/regex/case，每个 PrettyStringInner（包括 PrettyJson
   * 嵌套里的）自己 findMatches + render mark —— 这样 tool_calls / results JSON 里
   * 的 `"id": "call_xxx"` `"command": "ls /tmp"` 等嵌套字符串也都自动参与搜索。
   */
  const findCtxValue = useMemo<FindCtxValue | null>(
    () =>
      findOpen && findQuery
        ? { query: findQuery, regex: findRegex, caseSensitive: findCase }
        : null,
    [findOpen, findQuery, findRegex, findCase]
  );

  /**
   * 左侧请求列表的"每条 entry 包含多少个匹配"——给 RequestRow 显示徽章，
   * 让用户立刻看出"另外哪个请求里也有这个词"。这里**不**依赖 DOM，提前算好
   * 即可；嵌套 JSON 用 JSON.stringify 粗略覆盖（搜 key 名 / value 都能命中）。
   */
  const perEntryMatchCount = useMemo<number[]>(() => {
    if (!findOpen || !findQuery) return entries.map(() => 0);
    const collectTexts = (entry: ModelIoEntry): string[] => {
      const out: string[] = [];
      if (entry.request?.system) out.push(entry.request.system);
      entry.request?.messages?.forEach((m) => {
        if (m.reasoning) out.push(m.reasoning);
        if (m.content) out.push(m.content);
        if (m.tool_calls?.length) out.push(JSON.stringify(m.tool_calls));
        if (m.results?.length) out.push(JSON.stringify(m.results));
        if (m.attachments?.length) out.push(JSON.stringify(m.attachments));
      });
      if (entry.response?.error) out.push(entry.response.error);
      if (entry.response?.reasoning) out.push(entry.response.reasoning);
      if (entry.response?.text) out.push(entry.response.text);
      if (entry.response?.calls?.length)
        out.push(JSON.stringify(entry.response.calls));
      return out;
    };
    return entries.map((entry) => {
      let count = 0;
      for (const t of collectTexts(entry)) {
        count += findMatches(t, findQuery, findRegex, findCase).length;
      }
      return count;
    });
  }, [entries, findOpen, findQuery, findRegex, findCase]);

  /**
   * 顶层 totalMatches 由 DOM 后置数 `<mark data-find-match>` 元素得到 ——
   * 当前 entry 的所有 PrettyStringInner（含 PrettyJson 嵌套）渲染后，
   * 数总数即是。query 或 entry 变化触发 layout effect 重数。
   */
  const [totalMatches, setTotalMatches] = useState(0);
  useLayoutEffect(() => {
    const node = detailRef.current;
    if (!node || !findOpen || !findQuery) {
      setTotalMatches(0);
      return;
    }
    const recount = () => {
      const marks = node.querySelectorAll("mark[data-find-match]");
      setTotalMatches(marks.length);
    };
    recount();
    // 折叠/展开 message、PrettyJson 节点会改变 mark 数量 —— MutationObserver 兜底
    const mo = new MutationObserver(recount);
    mo.observe(node, { childList: true, subtree: true });
    return () => mo.disconnect();
  }, [findOpen, findQuery, findRegex, findCase, selected]);

  // active 超界时回 0（query 变短、匹配减少）
  useEffect(() => {
    if (findActive >= totalMatches) setFindActive(0);
  }, [totalMatches, findActive]);

  // 关抽屉时彻底重置 find；**不**在 selected 变化时关 find ——
  // 之前合并写 `[selected, open]` 触发了一个隐 race：用户开抽屉后立刻按 Cmd+F，
  // 但异步 refresh() 此时才 resolve、setSelected(N-1) 触发本 effect 把 findOpen
  // 又设回 false。FindBar 来不及显示就被关掉，用户看到"Cmd+F 没反应"
  useEffect(() => {
    if (!open) {
      setFindOpen(false);
      setFindQuery("");
      setFindActive(0);
    }
  }, [open]);

  // 切换 entry 时只重置 findActive（保留 query 让搜索词跨请求复用）
  useEffect(() => {
    setFindActive(0);
  }, [selected]);

  // 选中 entry 变化 → 左侧列表把它滚到可视区；详情面板滚到底（默认看响应）。
  // findOpen 时跳过详情自动滚底，让下面那条 active-mark scrollIntoView 接管。
  useLayoutEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>(`[data-row-index="${selected}"]`)
      ?.scrollIntoView({ block: "nearest" });
    if (findOpen) return;
    const detail = detailRef.current;
    if (detail) detail.scrollTop = detail.scrollHeight;
  }, [selected, entries.length, findOpen]);

  /**
   * active 切换：用 DOM querySelectorAll 找第 N 个 mark，加 `data-active="true"`，
   * scrollIntoView。**不通过 React 重渲染** —— 否则 active 每变一次整个 PrettyStringInner
   * 都要重算 findMatches。
   */
  useLayoutEffect(() => {
    const node = detailRef.current;
    if (!node) return;
    const marks = node.querySelectorAll<HTMLElement>("mark[data-find-match]");
    marks.forEach((m) => {
      if (m.dataset.active === "true") delete m.dataset.active;
    });
    if (!findOpen || totalMatches === 0) return;
    const target = marks[findActive];
    if (target) {
      target.dataset.active = "true";
      target.scrollIntoView({ block: "center", behavior: "smooth" });
    }
  }, [findActive, totalMatches, findOpen]);

  // findOpen 通过 ref 暴露给 Esc listener —— 避免 React 18 batched updates 让
  // 闭包内的 findOpen 处于 stale 状态（造成 Esc 误关抽屉而不是关 find）
  const findOpenRef = useRef(findOpen);
  findOpenRef.current = findOpen;

  // ESC 关闭抽屉 / find。capture 阶段拦截 —— 早于 FindBar input 的 React onKeyDown，
  // 也早于 chat 区域的 ChatView listener；保证 modelio 抽屉打开时 Esc 优先关 find
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (findOpenRef.current) {
          setFindOpen(false);
          e.preventDefault();
          e.stopPropagation();
          return;
        }
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  // Cmd/Ctrl+F 拉起搜索（仅抽屉打开时拦截，不挡 chat 的全局 find）
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (isLocalFindShortcut(e)) {
        e.preventDefault();
        e.stopPropagation();
        setFindOpen(true);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  // 计算 diff: 当前请求的 messages 与上一条请求 messages 的"前缀重叠"长度。
  // 重叠的部分是 carried over，剩下的是本次新增。
  //
  // **比较前先剥离动态包装段**（system-reminder / environment / workspace-update）——
  // agent_core 会把这些段注入首条 user message，且每次 turn 内容会有微调（如时间戳、
  // workspace 状态），导致字面 JSON 比较永远判"全新"，前缀重叠假设碎掉。
  // 调试器关心的是用户语义意图（"这条 user / assistant / tool 在上次出现过吗"），
  // 而不是字节级一致。
  const carriedOverCount = useMemo(() => {
    if (!current || selected === 0) return 0;
    const prev = entries[selected - 1];
    if (!prev) return 0;
    const a = prev.request.messages;
    const b = current.request.messages;
    let i = 0;
    const max = Math.min(a.length, b.length);
    while (i < max && fingerprintMessage(a[i]) === fingerprintMessage(b[i])) i++;
    return i;
  }, [entries, selected, current]);

  if (!open) return null;

  // 容器**不**铺满 / 不加遮罩 —— 让左侧 chat 区域保持可见、可滚动、可点
  // 抽屉右侧贴窗口边、上下铺满；左侧靠圆角 + 强阴影制造"浮"感（卡片飘在主窗口之上）。
  // 宽度可拖（drawerWidth 状态 + localStorage 持久化）
  return (
    <div
      id="model-io-drawer-root"
      className="fixed top-0 right-0 bottom-0 z-[100] border-l border-border bg-background rounded-l-3xl flex overflow-hidden shadow-[-11px_11px_10px_0px_rgba(0,0,0,0.40)]"
      style={{ width: drawerWidth }}
      role="complementary"
      data-testid="model-io-drawer"
    >
      {/* 左侧 resize handle —— 拖动改 drawerWidth；hover/active 加深可见 */}
      <ResizeHandle
        onResize={(delta) => {
          setDrawerWidth((w) => {
            // 向左拖（delta<0）加宽；向右拖（delta>0）缩窄
            const next = w - delta;
            const min = 400;
            const max = Math.min(1400, window.innerWidth - 200);
            return Math.max(min, Math.min(max, next));
          });
        }}
      />
      <div className="flex-1 min-w-0 flex flex-col">
        <header className="h-12 shrink-0 px-4 flex items-center justify-between border-b border-border">
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setSidebarOpen((o) => !o)}
              title={sidebarOpen ? "折叠请求列表" : "展开请求列表"}
              data-testid="model-io-sidebar-toggle"
            >
              {sidebarOpen ? (
                <PanelLeftClose className="w-3.5 h-3.5" />
              ) : (
                <PanelLeftOpen className="w-3.5 h-3.5" />
              )}
            </Button>
            <h2 className="text-sm font-medium">Model I/O 调试器</h2>
            <span className="text-xs text-muted-foreground">
              {entries.length} 次请求
            </span>
            {/* 当前对话的 session 目录名（~/.hebbian/sessions/<id>）—— 跟 ChatView header
                debug 显示的 id 一致，方便在 modelio 抽屉里直接对照 jsonl 找文件 */}
            <span
              className="select-text truncate max-w-[280px] font-mono text-[10px] text-muted-foreground/70"
              title={`session ${sessionId}`}
            >
              {sessionId}
            </span>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="sm"
              onClick={refresh}
              disabled={loading}
              title="刷新"
              data-testid="model-io-refresh"
            >
              <RefreshCw className={cn("w-3.5 h-3.5", loading && "animate-spin")} />
            </Button>
            <Button variant="ghost" size="sm" onClick={onClose} title="关闭 (Esc)">
              <X className="w-3.5 h-3.5" />
            </Button>
          </div>
        </header>

        {err && (
          <div className="px-4 py-2 text-xs text-destructive border-b border-border">
            {err}
          </div>
        )}

        {entries.length === 0 && !loading ? (
          <EmptyState />
        ) : (
          <div className="flex-1 min-h-0 flex">
            {/* 左：请求列表。宽度过渡动画 —— 折叠后右侧 detail 视图自然变大 */}
            <aside
              className={cn(
                "shrink-0 border-r border-border overflow-hidden transition-[width] duration-200 ease-out",
                sidebarOpen ? "w-[200px]" : "w-0 border-r-0"
              )}
            >
              {sidebarOpen && (
                <ol ref={listRef} className="h-full overflow-y-auto">
                  {entries.map((e, idx) => (
                    <Fragment key={`${e.run_id}-${e.turn}-${idx}`}>
                      <RequestRow
                        entry={e}
                        index={idx}
                        active={idx === selected}
                        matchCount={perEntryMatchCount[idx] ?? 0}
                        onClick={() => setSelected(idx)}
                      />
                      {dividerAfterIndices.has(idx) && (
                        <li className="flex items-center gap-2 px-2 py-1.5 my-1">
                          <div className="flex-1 h-px bg-border" />
                          <span className="text-[10px] text-muted-foreground whitespace-nowrap select-none">
                            上下文压缩
                          </span>
                          <div className="flex-1 h-px bg-border" />
                        </li>
                      )}
                    </Fragment>
                  ))}
                </ol>
              )}
            </aside>

            {/* 右：详情。外层 wrapper 是 relative 容器持有 FindBar / Minimap，
                section 自己只负责滚动 —— 避免 absolute child 在 overflow:auto 容器内
                被某些浏览器当成 in-flow 内容跟随滚动消失 */}
            <div className="relative flex-1 min-w-0 flex flex-col">
              <FindBar
                open={findOpen}
                onClose={() => setFindOpen(false)}
                state={{
                  query: findQuery,
                  regex: findRegex,
                  caseSensitive: findCase,
                  current: totalMatches === 0 ? 0 : findActive + 1,
                  total: totalMatches,
                }}
                onChange={(patch) => {
                  if (patch.query !== undefined) setFindQuery(patch.query);
                  if (patch.regex !== undefined) setFindRegex(patch.regex);
                  if (patch.caseSensitive !== undefined) setFindCase(patch.caseSensitive);
                  setFindActive(0);
                }}
                onPrev={() =>
                  setFindActive((i) =>
                    totalMatches === 0 ? 0 : (i - 1 + totalMatches) % totalMatches
                  )
                }
                onNext={() =>
                  setFindActive((i) =>
                    totalMatches === 0 ? 0 : (i + 1) % totalMatches
                  )
                }
              />
              {findOpen && totalMatches > 0 ? (
                <MatchMinimap
                  containerRef={detailRef}
                  activeIdx={findActive}
                  onJump={setFindActive}
                  totalMatches={totalMatches}
                />
              ) : null}
              <section
                ref={detailRef}
                className="flex-1 min-w-0 overflow-y-auto"
              >
                {current ? (
                  <FindCtx.Provider value={findCtxValue}>
                    <RequestDetail
                      entry={current}
                      carriedOverCount={carriedOverCount}
                      index={selected}
                    />
                  </FindCtx.Provider>
                ) : null}
              </section>
              {current ? <ScrollEndsButtons containerRef={detailRef} /> : null}
            </div>
          </div>
        )}
      </div>
    </div>
  );
});

/**
 * 抽屉左缘可拖动 handle。鼠标按下进入"拖动模式"——document mousemove 监听
 * 把 dx 累加 emit 给 onResize，mouseup 退出。期间锁 cursor 为 col-resize、
 * 关闭 text selection，否则拖文字会高亮。
 */
function ResizeHandle({ onResize }: { onResize: (deltaX: number) => void }) {
  const dragging = useRef(false);
  const lastX = useRef(0);
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      className="absolute left-0 top-0 bottom-0 w-1 cursor-col-resize hover:bg-primary/40 active:bg-primary/60 z-[1] select-none"
      onMouseDown={(e) => {
        e.preventDefault();
        dragging.current = true;
        lastX.current = e.clientX;
        document.body.style.cursor = "col-resize";
        document.body.style.userSelect = "none";
        const onMove = (ev: MouseEvent) => {
          if (!dragging.current) return;
          const delta = ev.clientX - lastX.current;
          lastX.current = ev.clientX;
          onResize(delta);
        };
        const onUp = () => {
          dragging.current = false;
          document.body.style.cursor = "";
          document.body.style.userSelect = "";
          document.removeEventListener("mousemove", onMove);
          document.removeEventListener("mouseup", onUp);
        };
        document.addEventListener("mousemove", onMove);
        document.addEventListener("mouseup", onUp);
      }}
    />
  );
}

function EmptyState() {
  return (
    <div className="flex-1 flex items-center justify-center px-8">
      <div className="max-w-md text-center text-sm text-muted-foreground space-y-2">
        <p>当前对话还没有 model 请求记录。</p>
        <p className="text-xs">
          先发一条消息试试 —— 之后每次请求都会出现在这里。
        </p>
      </div>
    </div>
  );
}

// ─── 左侧：请求行 ────────────────────────────────────────────────────────────

function RequestRow({
  entry,
  index,
  active,
  matchCount,
  onClick,
}: {
  entry: ModelIoEntry;
  index: number;
  active: boolean;
  matchCount: number;
  onClick: () => void;
}) {
  const usage = entry.response?.usage;
  const ok = entry.response?.type !== "Error";
  const msgCount = entry.request?.messages?.length ?? 0;

  return (
    <li
      onClick={onClick}
      data-row-index={index}
      className={cn(
        "px-3 py-2 border-b border-border cursor-pointer hover:bg-accent/40 transition-colors",
        active && "bg-accent",
        // 搜索激活且本行有命中 —— 左边一道黄色细条，跟搜索框颜色一致
        matchCount > 0 && "border-l-2 border-l-yellow-400"
      )}
      data-testid="model-io-row"
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-1.5">
          <span className="text-xs font-medium">#{index + 1}</span>
          {entry.kind === "judge" && (
            <span className="text-[10px] px-1 py-0.5 rounded bg-orange-500/15 text-orange-600 dark:text-orange-400">
              judge
            </span>
          )}
          {matchCount > 0 ? (
            <span
              className="text-[10px] px-1 py-0.5 rounded bg-yellow-400/30 text-yellow-700 dark:text-yellow-300 tabular-nums"
              title={`本请求包含 ${matchCount} 个匹配`}
            >
              {matchCount}
            </span>
          ) : null}
        </div>
        <span
          className={cn(
            "text-[10px] px-1.5 py-0.5 rounded",
            ok
              ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
              : "bg-destructive/20 text-destructive"
          )}
        >
          {entry.response?.type || "?"}
        </span>
      </div>
      <div className="mt-1 text-[11px] text-muted-foreground truncate" title={entry.ts}>
        {formatTs(entry.ts)} · {entry.duration_ms}ms
      </div>
      <div className="mt-0.5 text-[11px] text-muted-foreground flex items-center gap-2">
        <span>{msgCount} msg</span>
        {usage ? (
          <>
            <span>·</span>
            <span title="input / cache_read / output">
              {usage.input_tokens}/{usage.cache_read_tokens}/{usage.output_tokens}
            </span>
          </>
        ) : null}
      </div>
      <div
        className="mt-0.5 text-[10px] text-muted-foreground/80 truncate"
        title={entry.model}
      >
        {entry.model}
      </div>
    </li>
  );
}

// ─── 右侧：请求详情 ──────────────────────────────────────────────────────────

function RequestDetail({
  entry,
  carriedOverCount,
  index,
}: {
  entry: ModelIoEntry;
  carriedOverCount: number;
  index: number;
}) {
  const [showCarried, setShowCarried] = useState(false);
  const [systemOpen, setSystemOpen] = useState(false);
  const [toolsOpen, setToolsOpen] = useState(false);
  const [systemZoomed, setSystemZoomed] = useZoom();

  // AutoMode 判官请求：简化渲染（tool + input + decision）
  if (entry.kind === "judge") {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const req = entry.request as any;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const resp = entry.response as any;
    return (
      <div className="p-4 space-y-3" data-testid="model-io-detail">
        <div className="flex items-center justify-between text-xs">
          <div className="flex items-center gap-2 text-muted-foreground">
            <span className="px-1.5 py-0.5 rounded bg-orange-500/15 text-orange-600 dark:text-orange-400 text-[10px]">
              AutoMode 判官
            </span>
            <span>请求 #{index + 1} · {entry.model} · {entry.duration_ms}ms</span>
          </div>
        </div>
        <CollapsibleBlock
          open={true}
          onToggle={() => {}}
          label={`工具: ${req.tool ?? "?"}`}
        >
          <pre className="px-3 py-2 text-[11px] whitespace-pre-wrap break-words bg-muted/30 max-h-[400px] overflow-auto rounded font-mono">
            {JSON.stringify(req.input, null, 2)}
          </pre>
        </CollapsibleBlock>
        <CollapsibleBlock
          open={true}
          onToggle={() => {}}
          label={`判官决策: ${resp.final ?? "?"}`}
        >
          <div className="px-3 py-2 text-[11px] space-y-1">
            {resp.raw && resp.raw !== resp.final && (
              <div>原始: <span className="font-medium">{String(resp.raw)}</span></div>
            )}
            {resp.reason && (
              <div className="mt-1 text-muted-foreground whitespace-pre-wrap">{String(resp.reason)}</div>
            )}
          </div>
        </CollapsibleBlock>
      </div>
    );
  }

  const tools = entry.request?.tools ?? [];
  const newCount = (entry.request?.messages?.length ?? 0) - carriedOverCount;

  return (
    <div className="p-4 space-y-3" data-testid="model-io-detail">
      {/* 顶部摘要条 */}
      <div className="flex items-center justify-between text-xs">
        <div className="text-muted-foreground">
          请求 #{index + 1} · turn {entry.turn} · {entry.model}
        </div>
        <CopyButton
          payload={entry.request}
          label="复制 messages"
          testid="copy-messages"
        />
      </div>

      {/* system prompt（默认折叠 + 放大按钮：放大后可切渲染/原文）*/}
      {entry.request?.system ? (
        <>
          <CollapsibleBlock
            open={systemOpen}
            onToggle={() => setSystemOpen((v) => !v)}
            label="system prompt"
            sublabel={`${entry.request.system.length} 字符`}
            rightExtras={<ZoomButton onClick={() => setSystemZoomed(true)} />}
          >
            <pre className="px-3 py-2 text-[11px] whitespace-pre-wrap break-words bg-muted/30 max-h-[400px] overflow-auto rounded font-mono">
              <PrettyStringInner value={entry.request.system} />
            </pre>
          </CollapsibleBlock>
          {systemZoomed && (
            <ZoomedModal
              title="system prompt"
              onClose={() => setSystemZoomed(false)}
            >
              <SystemPromptZoomBody value={entry.request.system} />
            </ZoomedModal>
          )}
        </>
      ) : null}

      {tools.length > 0 ? (
        <CollapsibleBlock
          open={toolsOpen}
          onToggle={() => setToolsOpen((v) => !v)}
          label="tool schema"
          sublabel={`${tools.length} 个工具`}
          rightExtras={<CopyButton payload={tools} label="复制" />}
        >
          <div className="px-3 py-2 bg-muted/30 max-h-[400px] overflow-auto rounded">
            <PrettyJson value={tools} />
          </div>
        </CollapsibleBlock>
      ) : null}

      {/* messages */}
      <div className="space-y-2">
        {carriedOverCount > 0 ? (
          <div
            className="px-3 py-1.5 text-xs text-muted-foreground border border-dashed border-border rounded cursor-pointer hover:bg-accent/30"
            onClick={() => setShowCarried((v) => !v)}
            data-testid="carried-over-toggle"
          >
            {showCarried ? (
              <ChevronDown className="inline w-3 h-3 mr-1" />
            ) : (
              <ChevronRight className="inline w-3 h-3 mr-1" />
            )}
            上次已发送 ({carriedOverCount} 条) ——{" "}
            {showCarried ? "点击折叠" : "点击展开"}
          </div>
        ) : null}

        {entry.request?.messages?.map((m, i) => {
          const isCarried = i < carriedOverCount;
          if (isCarried && !showCarried) return null;
          return (
            <MessageRow
              key={i}
              msg={m}
              index={i}
              isNew={i >= carriedOverCount && carriedOverCount > 0}
              showStarBadge={carriedOverCount > 0}
            />
          );
        })}

        {newCount === 0 && carriedOverCount > 0 ? (
          <div className="px-3 py-2 text-xs text-amber-600 dark:text-amber-400 bg-amber-500/10 rounded">
            这条请求的 messages 与上一条完全相同 —— 通常是连续 tool_call 之间无新 user
            输入。
          </div>
        ) : null}
      </div>

      {/* response */}
      <ResponseBlock response={entry.response} />
    </div>
  );
}

// ─── 单条 message ────────────────────────────────────────────────────────────

function MessageRow({
  msg,
  index,
  isNew,
  showStarBadge,
}: {
  msg: ModelIoMessage;
  index: number;
  isNew: boolean;
  showStarBadge: boolean;
}) {
  const [open, setOpen] = useState(isNew); // 新增的默认展开
  const [zoomed, setZoomed] = useZoom();
  const roleColor =
    msg.role === "user"
      ? "bg-blue-500/15 text-blue-700 dark:text-blue-300"
      : msg.role === "assistant"
        ? "bg-purple-500/15 text-purple-700 dark:text-purple-300"
        : msg.role === "tool"
          ? "bg-orange-500/15 text-orange-700 dark:text-orange-300"
          : "bg-muted text-muted-foreground";

  const body = (
    <div className="px-3 py-2 border-t border-border bg-muted/20 space-y-2">
      {msg.reasoning ? (
        <PayloadField label="reasoning" copyText={msg.reasoning}>
          <PrettyStringInner
            value={msg.reasoning}
          />
        </PayloadField>
      ) : null}
      {msg.content ? (
        <PayloadField label="content" copyText={msg.content}>
          <PrettyStringInner
            value={msg.content}
          />
        </PayloadField>
      ) : null}
      {msg.tool_calls && msg.tool_calls.length > 0 ? (
        <PayloadField label="tool_calls" copyJson={msg.tool_calls}>
          <PrettyJson value={msg.tool_calls} />
        </PayloadField>
      ) : null}
      {msg.results && msg.results.length > 0 ? (
        <PayloadField label="tool results" copyJson={msg.results}>
          <PrettyJson value={msg.results} />
        </PayloadField>
      ) : null}
      {msg.attachments && msg.attachments.length > 0 ? (
        <PayloadField label="attachments" copyJson={msg.attachments}>
          <PrettyJson value={msg.attachments} />
        </PayloadField>
      ) : null}
    </div>
  );

  return (
    <div
      className={cn(
        "group border border-border rounded overflow-hidden",
        isNew && "ring-1 ring-emerald-500/40"
      )}
      data-testid="model-io-message"
      data-role={msg.role}
      data-new={isNew ? "true" : "false"}
    >
      <div className="w-full px-3 py-1.5 flex items-center gap-2 hover:bg-accent/30">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="flex-1 flex items-center gap-2 text-left min-w-0"
        >
          {open ? (
            <ChevronDown className="w-3 h-3 shrink-0" />
          ) : (
            <ChevronRight className="w-3 h-3 shrink-0" />
          )}
          <span className="text-[10px] text-muted-foreground tabular-nums w-6 shrink-0">
            {index + 1}
          </span>
          <span className={cn("text-[10px] px-1.5 py-0.5 rounded shrink-0", roleColor)}>
            {msg.role}
          </span>
          {isNew && showStarBadge ? (
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-emerald-500/15 text-emerald-700 dark:text-emerald-300 shrink-0">
              NEW
            </span>
          ) : null}
          <span className="text-[11px] text-muted-foreground truncate">
            {summarize(msg)}
          </span>
        </button>
        <ZoomButton onClick={() => setZoomed(true)} />
      </div>
      {open ? body : null}
      {zoomed && (
        <ZoomedModal
          title={`#${index + 1} · ${msg.role}`}
          onClose={() => setZoomed(false)}
        >
          {body}
        </ZoomedModal>
      )}
    </div>
  );
}

// ─── response 块 ─────────────────────────────────────────────────────────────

function ResponseBlock({ response }: { response: ModelIoResponse }) {
  const errorMode = response?.type === "Error";
  const [zoomed, setZoomed] = useZoom();

  const body = (
    <div className="p-3 space-y-2">
      {response?.error ? (
        <pre className="text-[11px] whitespace-pre-wrap break-words bg-background/60 p-2 rounded font-mono">
          <PrettyStringInner value={response.error} />
        </pre>
      ) : null}
      {response?.reasoning ? (
        <PayloadField label="reasoning" copyText={response.reasoning}>
          <PrettyStringInner
            value={response.reasoning}
          />
        </PayloadField>
      ) : null}
      {response?.text ? (
        <PayloadField label="text" copyText={response.text}>
          <PrettyStringInner value={response.text} />
        </PayloadField>
      ) : null}
      {response?.calls && response.calls.length > 0 ? (
        <PayloadField label="tool_calls" copyJson={response.calls}>
          <PrettyJson value={response.calls} />
        </PayloadField>
      ) : null}
    </div>
  );

  return (
    <div
      className={cn(
        "group border rounded overflow-hidden",
        errorMode ? "border-destructive/40" : "border-border"
      )}
      data-testid="model-io-response"
    >
      <div
        className={cn(
          "px-3 py-2 flex items-center gap-2 border-b",
          errorMode
            ? "bg-destructive/10 text-destructive border-destructive/40"
            : "bg-muted/40 border-border"
        )}
      >
        <div className="text-xs font-medium flex-1">
          response · {response?.type ?? "?"}
        </div>
        {response?.usage ? (
          <div className="text-[11px] text-muted-foreground">
            in {response.usage.input_tokens} · cache_r {response.usage.cache_read_tokens}{" "}
            · cache_w {response.usage.cache_creation_tokens} · out{" "}
            {response.usage.output_tokens}
          </div>
        ) : null}
        <ZoomButton onClick={() => setZoomed(true)} />
      </div>
      {body}
      {zoomed && (
        <ZoomedModal
          title={`response · ${response?.type ?? "?"}`}
          onClose={() => setZoomed(false)}
        >
          {body}
        </ZoomedModal>
      )}
    </div>
  );
}

/**
 * 右侧滚动条边的"匹配锚点条" —— 类似 VSCode 的 minimap 命中指示。
 * 每个 `<mark data-find-match>` 在容器内的相对垂直位置画一个小方块，
 * 点击 → 跳到该匹配。activeIdx 高亮琥珀色，其他黄色。
 *
 * 实现：useLayoutEffect 收集所有 mark 的 offsetTop 算百分比；用 ResizeObserver
 * 监听容器尺寸变化（折叠 message / 切换 entry 时位置会变）+ scrollHeight
 * 也会变。简化起见：依赖 totalMatches / activeIdx 重算一次足够（用户翻请求时
 * useFindController 会 reset，触发重算）。
 */
function MatchMinimap({
  containerRef,
  activeIdx,
  onJump,
  totalMatches,
}: {
  containerRef: React.RefObject<HTMLDivElement | null>;
  activeIdx: number;
  onJump: (i: number) => void;
  totalMatches: number;
}) {
  const [positions, setPositions] = useState<number[]>([]);

  useLayoutEffect(() => {
    const node = containerRef.current;
    if (!node) return;
    const compute = () => {
      const marks = node.querySelectorAll<HTMLElement>("mark[data-find-match]");
      const total = node.scrollHeight || 1;
      const pos: number[] = [];
      marks.forEach((m) => {
        // offsetTop 是相对最近 positioned ancestor；section 是 relative，
        // mark 内嵌任意层级，offsetTop 仍是相对 section。除以 scrollHeight 得
        // 它在整段内容里的垂直百分比
        pos.push(m.offsetTop / total);
      });
      setPositions(pos);
    };
    compute();
    // 容器大小 / 内容高度变化时重算（如折叠展开 message / 字段）
    const ro = new ResizeObserver(compute);
    ro.observe(node);
    return () => ro.disconnect();
  }, [containerRef, totalMatches, activeIdx]);

  if (positions.length === 0) return null;

  return (
    <div
      className="absolute top-12 right-0.5 bottom-2 w-3 pointer-events-none z-30"
      aria-hidden="true"
    >
      {positions.map((p, i) => (
        <button
          key={i}
          type="button"
          onClick={() => onJump(i)}
          title={`匹配 ${i + 1} / ${positions.length}`}
          style={{ top: `${p * 100}%` }}
          className={cn(
            "absolute right-0 h-1.5 rounded-sm pointer-events-auto transition-colors",
            i === activeIdx
              ? "w-3 bg-amber-500 hover:bg-amber-600"
              : "w-2 bg-yellow-400/70 hover:bg-yellow-500"
          )}
        />
      ))}
    </div>
  );
}

/**
 * 详情面板右下角悬浮的"回到顶/底"按钮。绝对定位在 detailRef 的外层 relative 容器里，
 * 所以滚动 detail section 时按钮位置不变。
 *
 * 显示策略：只在内容真比视口高时才出现；scrollTop 决定哪一个高亮可点（已在顶/底
 * 的那个置灰但仍保留位置，避免按钮反复出现导致跳动）。
 */
function ScrollEndsButtons({
  containerRef,
}: {
  containerRef: React.RefObject<HTMLDivElement | null>;
}) {
  const [atTop, setAtTop] = useState(true);
  const [atBottom, setAtBottom] = useState(true);
  const [scrollable, setScrollable] = useState(false);

  useLayoutEffect(() => {
    const node = containerRef.current;
    if (!node) return;
    const update = () => {
      const can = node.scrollHeight - node.clientHeight > 4;
      setScrollable(can);
      setAtTop(node.scrollTop <= 2);
      setAtBottom(node.scrollHeight - node.scrollTop - node.clientHeight <= 2);
    };
    update();
    node.addEventListener("scroll", update, { passive: true });
    const ro = new ResizeObserver(update);
    ro.observe(node);
    return () => {
      node.removeEventListener("scroll", update);
      ro.disconnect();
    };
  }, [containerRef]);

  if (!scrollable) return null;

  const scrollTo = (top: number) => {
    containerRef.current?.scrollTo({ top, behavior: "smooth" });
  };

  return (
    <div className="absolute bottom-3 right-4 z-[2] flex flex-col gap-1.5">
      <button
        type="button"
        onClick={() => scrollTo(0)}
        disabled={atTop}
        title="回到顶部"
        className={cn(
          "w-7 h-7 rounded-full border border-border shadow-md flex items-center justify-center bg-background/90 backdrop-blur transition-opacity",
          atTop ? "opacity-40 cursor-default" : "hover:bg-accent cursor-pointer"
        )}
      >
        <ChevronsUp className="w-3.5 h-3.5" />
      </button>
      <button
        type="button"
        onClick={() =>
          scrollTo(containerRef.current?.scrollHeight ?? 0)
        }
        disabled={atBottom}
        title="回到底部"
        className={cn(
          "w-7 h-7 rounded-full border border-border shadow-md flex items-center justify-center bg-background/90 backdrop-blur transition-opacity",
          atBottom ? "opacity-40 cursor-default" : "hover:bg-accent cursor-pointer"
        )}
      >
        <ChevronsDown className="w-3.5 h-3.5" />
      </button>
    </div>
  );
}

// ─── helpers ────────────────────────────────────────────────────────────────

function CollapsibleBlock({
  open,
  onToggle,
  label,
  sublabel,
  children,
  rightExtras,
}: {
  open: boolean;
  onToggle: () => void;
  label: string;
  sublabel?: string;
  children: React.ReactNode;
  /** header 右侧额外按钮（如 ZoomButton），共享 header 的 group hover */
  rightExtras?: ReactNode;
}) {
  return (
    <div className="group border border-border rounded overflow-hidden">
      <div className="w-full px-3 py-1.5 flex items-center gap-2 hover:bg-accent/30 bg-muted/30">
        <button
          onClick={onToggle}
          className="flex-1 flex items-center gap-2 text-left min-w-0"
        >
          {open ? (
            <ChevronDown className="w-3 h-3 shrink-0" />
          ) : (
            <ChevronRight className="w-3 h-3 shrink-0" />
          )}
          <span className="text-xs">{label}</span>
          {sublabel ? (
            <span className="text-[10px] text-muted-foreground">{sublabel}</span>
          ) : null}
        </button>
        {rightExtras}
      </div>
      {open ? <div>{children}</div> : null}
    </div>
  );
}

/**
 * 放大态下的 system prompt 渲染器：默认走 markdown（system prompt 通常是有标题 /
 * 列表 / 代码块的结构化文本，渲染后能秒看结构），用户可以点「原文」切到纯 monospace
 * 模式查看真实换行 / 缩进。state 保留在组件内，开关放大态时重置为默认（markdown）。
 */
function SystemPromptZoomBody({ value }: { value: string }) {
  const [mode, setMode] = useState<"rendered" | "raw">("rendered");
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-1">
        <ViewModeChip active={mode === "rendered"} onClick={() => setMode("rendered")}>
          渲染
        </ViewModeChip>
        <ViewModeChip active={mode === "raw"} onClick={() => setMode("raw")}>
          原文
        </ViewModeChip>
      </div>
      {mode === "rendered" ? (
        <div className="markdown text-[13px] leading-relaxed break-words">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{value}</ReactMarkdown>
        </div>
      ) : (
        <pre className="text-[12px] whitespace-pre-wrap break-words bg-muted/30 p-3 rounded font-mono">
          <PrettyStringInner value={value} />
        </pre>
      )}
    </div>
  );
}

function ViewModeChip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "px-2.5 py-1 text-xs rounded border transition-colors",
        active
          ? "border-primary text-primary bg-primary/10"
          : "border-border text-muted-foreground hover:bg-accent/40",
      )}
    >
      {children}
    </button>
  );
}

function CopyButton({
  payload,
  label,
  testid,
}: {
  payload: unknown;
  label: string;
  testid?: string;
}) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        } catch {
          // navigator.clipboard 在某些 webview 下可能未授权 —— 静默
        }
      }}
      className="text-[11px] px-2 py-1 rounded hover:bg-accent flex items-center gap-1"
      data-testid={testid}
    >
      {copied ? (
        <Check className="w-3 h-3 text-emerald-500" />
      ) : (
        <Copy className="w-3 h-3" />
      )}
      {label}
    </button>
  );
}

/**
 * 给 diff 用的 message 指纹。剥离会随 turn 微调的动态包装段，让"同一条用户意图
 * 输入"在跨 turn 比较时仍能命中前缀重叠。结构字段（tool_call id / tool result id）
 * 全保留——它们是真实的关联点。
 */
function fingerprintMessage(m: ModelIoMessage): string {
  const stripped = (m.content ?? "")
    .replace(/<system-reminder>[\s\S]*?<\/system-reminder>\s*/g, "")
    .replace(/<environment>[\s\S]*?<\/environment>\s*/g, "")
    .replace(/<workspace-update>[\s\S]*?<\/workspace-update>\s*/g, "");
  return JSON.stringify({
    role: m.role,
    content: stripped,
    reasoning: m.reasoning ?? null,
    tool_calls: m.tool_calls ?? null,
    results: m.results ?? null,
  });
}

function summarize(msg: ModelIoMessage): string {
  if (msg.content) {
    return msg.content.length > 80
      ? `${msg.content.slice(0, 80)}…`
      : msg.content;
  }
  if (msg.tool_calls && msg.tool_calls.length > 0) {
    return `tool_calls: ${msg.tool_calls.map((c) => c.name).join(", ")}`;
  }
  if (msg.results && msg.results.length > 0) {
    return `tool results: ${msg.results.map((r) => r.name).join(", ")}`;
  }
  if (msg.reasoning) return `reasoning (${msg.reasoning.length} 字符)`;
  return "(empty)";
}

function formatTs(ts: string): string {
  // ISO-8601 → "HH:mm:ss"。失败就原样返回。
  try {
    const d = new Date(ts);
    if (Number.isNaN(d.getTime())) return ts;
    return d.toLocaleTimeString("zh-CN", { hour12: false });
  } catch {
    return ts;
  }
}

// ─── JSON 渲染器：div-per-row 缩进 + hover 折叠 + 控制字符可视 marker ─────────

/**
 * 递归渲染任意 JSON。三个关键点：
 *
 * 1. **div-per-row 缩进**：每个 key-value / bracket 单独一个 `<div>`，缩进靠
 *    `paddingLeft = level * 14px`，**不**靠 `whitespace-pre-wrap` 的文本流空格。
 *    这样长字符串（如长文件路径）wrap 时由当前 div 自己换行，下一行仍在 div 的
 *    缩进位置——避免上一版 `<pre whitespace-pre-wrap>` 模式下长值 wrap 跑到屏幕最左。
 *
 * 2. **hover 折叠**：对象 / 数组左侧有 chevron 按钮。展开状态下默认隐藏，
 *    hover 这一行才显示（避免视觉杂乱）；折叠状态下一直显示（提示"能展开"）。
 *
 * 3. **字符串控制字符**：标量字符串走 [`PrettyStringInner`]，把 `\n` `\t` `\r`
 *    展开为真字符 + 可视 marker（`↵` `→` `⏎` `\xNN`），marker `select-none` 不参与复制。
 */
/**
 * Cmd/Ctrl+C 选中复制时拦截，遍历 selection 的 fragment 自己生成 plain text——
 * 浏览器对当前 DOM（多层 flex + 每个 token 独立 span）默认 serialization 在
 * 几乎每个边界塞 \n，复制出来全是零散单词。这里按节点 tagName 来判：
 *   - div / pre / p / li 视作"一行"边界 → 末尾 \n
 *   - span / button / inline 元素 → 同行连续
 *   - 含 `select-none` className 的节点跳过（chevron 占位 / marker / 复制按钮）
 * 复制的是用户**实际选中的部分**，不是整片 JSON——避免"选了一行结果复制了全文"。
 */
function serializeSelectionFragment(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) {
    return node.textContent ?? "";
  }
  if (node.nodeType === Node.DOCUMENT_FRAGMENT_NODE) {
    let out = "";
    node.childNodes.forEach((child) => {
      out += serializeSelectionFragment(child);
    });
    return out;
  }
  if (node.nodeType !== Node.ELEMENT_NODE) return "";

  const el = node as Element;
  const className = typeof el.className === "string" ? el.className : "";
  if (className.includes("select-none")) return "";

  let inner = "";
  el.childNodes.forEach((child) => {
    inner += serializeSelectionFragment(child);
  });

  const tag = el.tagName.toLowerCase();
  if (tag === "br") return "\n";
  if (tag === "div" || tag === "pre" || tag === "p" || tag === "li") {
    // 行级元素：末尾保证一个 \n（inner 已带 \n 时不重复加）
    return inner.replace(/\n+$/, "") + "\n";
  }
  return inner;
}

function PrettyJson({ value }: { value: unknown }) {
  const rootRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const handler = (e: ClipboardEvent) => {
      const sel = window.getSelection();
      if (!sel || sel.rangeCount === 0) return;
      const range = sel.getRangeAt(0);
      if (!el.contains(range.commonAncestorContainer)) return;
      const fragment = range.cloneContents();
      const text = serializeSelectionFragment(fragment);
      if (!text) return;
      e.preventDefault();
      e.clipboardData?.setData("text/plain", text);
    };
    el.addEventListener("copy", handler);
    return () => el.removeEventListener("copy", handler);
  }, []);

  return (
    <div ref={rootRef}>
      <PrettyJsonNode value={value} keyLabel={null} isLast />
    </div>
  );
}

/**
 * 双按钮复制（content / reasoning 等长字符串字段）：
 * - 主按钮：复制渲染后内容（真换行 / 真制表符，跟视觉一致），方便粘到 markdown / 编辑器
 * - 副按钮：复制原格式（`\n` `\t` 还原为字面转义序列），方便粘到代码 / JSON 文件
 * 副按钮平时 opacity-0 偏左 1px，hover 主按钮（或 wrapper 任意位置）时滑出。
 */
function StringCopyButton({ value }: { value: string }) {
  const [copied, setCopied] = useState<"rendered" | "raw" | null>(null);

  async function copy(mode: "rendered" | "raw") {
    const text =
      mode === "rendered" ? value : JSON.stringify(value).slice(1, -1);
    try {
      await navigator.clipboard.writeText(text);
      setCopied(mode);
      setTimeout(() => setCopied(null), 1200);
    } catch {
      // 静默：clipboard 权限被拒下次 hover 重试即可
    }
  }

  return (
    <span
      className="group/strcopy relative inline-flex items-center select-none"
      onClick={(e) => e.stopPropagation()}
    >
      <button
        type="button"
        onClick={() => copy("rendered")}
        title="复制（渲染后内容，跟视觉一致）"
        aria-label="复制渲染后内容"
        className="inline-flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
      >
        {copied === "rendered" ? (
          <Check className="h-3 w-3 text-emerald-500" />
        ) : (
          <Copy className="h-3 w-3" />
        )}
      </button>
      <button
        type="button"
        onClick={() => copy("raw")}
        title="复制原格式（\n \t 保留为字面转义）"
        aria-label="复制原格式"
        className="ml-0.5 inline-flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground opacity-0 -translate-x-1 transition-all duration-150 group-hover/strcopy:opacity-100 group-hover/strcopy:translate-x-0"
      >
        {copied === "raw" ? (
          <Check className="h-3 w-3 text-emerald-500" />
        ) : (
          <Code className="h-3 w-3" />
        )}
      </button>
    </span>
  );
}

/**
 * 双按钮复制：
 * - 主按钮：复制格式化 JSON（`null, 2` 缩进，跟视觉一致）
 * - 副按钮：复制压缩 JSON（一行），平时 opacity-0 + 偏左 1px，hover wrapper 时滑出
 * 按钮自己 stopPropagation，避免点击触发外层 chevron 展开/折叠等行为。
 */
function JsonCopyButton({ value }: { value: unknown }) {
  const [copied, setCopied] = useState<"pretty" | "compact" | null>(null);

  async function copy(mode: "pretty" | "compact") {
    const text =
      mode === "pretty"
        ? JSON.stringify(value, null, 2)
        : JSON.stringify(value);
    try {
      await navigator.clipboard.writeText(text);
      setCopied(mode);
      setTimeout(() => setCopied(null), 1200);
    } catch {
      // swallow：clipboard 权限被拒不需要打扰用户，下次 hover 重试即可
    }
  }

  return (
    <span
      className="group/copy relative inline-flex items-center select-none"
      onClick={(e) => e.stopPropagation()}
    >
      <button
        type="button"
        onClick={() => copy("pretty")}
        title="复制格式化 JSON"
        aria-label="复制格式化 JSON"
        className="inline-flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
      >
        {copied === "pretty" ? (
          <Check className="h-3 w-3 text-emerald-500" />
        ) : (
          <Copy className="h-3 w-3" />
        )}
      </button>
      <button
        type="button"
        onClick={() => copy("compact")}
        title="复制压缩 JSON（一行）"
        aria-label="复制压缩 JSON"
        className="ml-0.5 inline-flex h-5 w-5 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground opacity-0 -translate-x-1 transition-all duration-150 group-hover/copy:opacity-100 group-hover/copy:translate-x-0"
      >
        {copied === "compact" ? (
          <Check className="h-3 w-3 text-emerald-500" />
        ) : (
          <Minimize2 className="h-3 w-3" />
        )}
      </button>
    </span>
  );
}

/**
 * 缩进通过"每个嵌套容器的 children 套一个 `border-l + padding-left`"实现 ——
 * 这条左边框就是 indent guide 竖线（IDE 风格）。比单纯 paddingLeft 更直观：
 * 视觉上能"沿着竖线一路读到底"，跨长块也不会丢层级。
 */
function PrettyJsonNode({
  value,
  keyLabel,
  isLast,
}: {
  value: unknown;
  keyLabel: string | null;
  isLast: boolean;
}) {
  const composite = value !== null && typeof value === "object";

  if (!composite) {
    return <ScalarRow keyLabel={keyLabel} value={value} isLast={isLast} />;
  }

  const isArray = Array.isArray(value);
  const entries: Array<[string | number, unknown]> = isArray
    ? (value as unknown[]).map((v, i) => [i, v])
    : Object.entries(value as Record<string, unknown>);
  const [openBracket, closeBracket] = isArray ? ["[", "]"] : ["{", "}"];

  if (entries.length === 0) {
    return (
      <div className="flex items-baseline gap-1 leading-relaxed">
        <span className="inline-block w-3.5 shrink-0 select-none" />
        <JsonKey label={keyLabel} />
        <span>
          {openBracket}
          {closeBracket}
        </span>
        {!isLast && <span className="text-muted-foreground">,</span>}
      </div>
    );
  }

  return (
    <CollapsibleJsonNode
      keyLabel={keyLabel}
      isArray={isArray}
      openBracket={openBracket}
      closeBracket={closeBracket}
      entries={entries}
      isLast={isLast}
      rawValue={value}
    />
  );
}

function CollapsibleJsonNode({
  keyLabel,
  isArray,
  openBracket,
  closeBracket,
  entries,
  isLast,
  rawValue,
}: {
  keyLabel: string | null;
  isArray: boolean;
  openBracket: string;
  closeBracket: string;
  entries: Array<[string | number, unknown]>;
  isLast: boolean;
  /** 这个层级原始的 value（用于复制按钮序列化），上层 PrettyJsonNode 透传 */
  rawValue: unknown;
}) {
  const [open, setOpen] = useState(true);
  return (
    <div>
      <div className="group flex items-baseline gap-1 leading-relaxed rounded hover:bg-accent/40">
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          aria-label={open ? "折叠" : "展开"}
          className={cn(
            "inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-opacity",
            // 展开：hover 才显示，避免视觉杂乱；折叠：一直显示，提示"能展开"
            open ? "opacity-0 group-hover:opacity-100" : "opacity-100"
          )}
        >
          {open ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronRight className="h-3 w-3" />
          )}
        </button>
        <JsonKey label={keyLabel} />
        <span>{openBracket}</span>
        {/* 行内复制按钮：紧跟开头 { 或 [，hover 整行才显示（行 group 控制可见性，
            内部 group/copy 再控制副按钮 hover 滑出）；位置稳定，展开/折叠态共用 */}
        <span className="opacity-0 group-hover:opacity-100 transition-opacity">
          <JsonCopyButton value={rawValue} />
        </span>
        {!open && (
          <>
            <span className="italic text-muted-foreground text-[10px] select-none">
              {entries.length} {isArray ? "项" : "键"}
            </span>
            <span>{closeBracket}</span>
            {!isLast && <span className="text-muted-foreground">,</span>}
          </>
        )}
      </div>
      {open && (
        <>
          {/* indent guide 竖线 = 左边框；ml 让线对齐到 chevron 中心。
              hover 加深，方便沿线读到底 */}
          <div className="border-l border-muted-foreground/40 hover:border-muted-foreground/70 pl-2 ml-[6px] transition-colors">
            {entries.map(([k, v], i) => (
              <PrettyJsonNode
                key={String(k)}
                value={v}
                keyLabel={isArray ? null : String(k)}
                isLast={i === entries.length - 1}
              />
            ))}
          </div>
          <div className="flex items-baseline gap-1 leading-relaxed">
            <span className="inline-block w-3.5 shrink-0 select-none" />
            <span>{closeBracket}</span>
            {!isLast && <span className="text-muted-foreground">,</span>}
          </div>
        </>
      )}
    </div>
  );
}

/**
 * 标量行：标量值 inline 跟 key 同一行；但**长字符串 / 含换行的字符串**单独占块，
 * 在 key 行下面缩进显示 —— 否则 flex 容器 wrap 时长串会跑到 flex 行最左端，丢对齐。
 */
function ScalarRow({
  keyLabel,
  value,
  isLast,
}: {
  keyLabel: string | null;
  value: unknown;
  isLast: boolean;
}) {
  const isMultilineString =
    typeof value === "string" && (value.length > 80 || value.includes("\n"));

  if (isMultilineString) {
    return (
      <div>
        <div className="flex items-baseline gap-1 leading-relaxed">
          <span className="inline-block w-3.5 shrink-0 select-none" />
          <JsonKey label={keyLabel} />
        </div>
        {/* 多行 value 块：颜色与父级一致；ml 比父 children 容器多一格 ——
            让 value 起始引号视觉上比 key 再缩进一级，避免和 key 列对齐造成混淆 */}
        <div className="border-l border-muted-foreground/40 hover:border-muted-foreground/70 pl-2 ml-[20px] transition-colors whitespace-pre-wrap break-all">
          <Token tone="green">
            &quot;<PrettyStringInner value={value as string} />&quot;
          </Token>
          {!isLast && <span className="text-muted-foreground">,</span>}
        </div>
      </div>
    );
  }

  return (
    <div className="flex items-baseline gap-1 leading-relaxed">
      <span className="inline-block w-3.5 shrink-0 select-none" />
      <JsonKey label={keyLabel} />
      <PrimitiveValue value={value} />
      {!isLast && <span className="text-muted-foreground">,</span>}
    </div>
  );
}

function JsonKey({ label }: { label: string | null }) {
  if (label === null) return null;
  return (
    <>
      <Token tone="sky">&quot;{label}&quot;</Token>
      <span className="text-muted-foreground">:</span>
    </>
  );
}

function PrimitiveValue({ value }: { value: unknown }): ReactNode {
  if (value === null) return <Token tone="muted">null</Token>;
  if (typeof value === "boolean" || typeof value === "number") {
    return <Token tone="amber">{String(value)}</Token>;
  }
  if (typeof value === "string") {
    return (
      <Token tone="green">
        &quot;<PrettyStringInner value={value} />&quot;
      </Token>
    );
  }
  return <span>{String(value)}</span>;
}

/**
 * 把单个字符串渲染成"真字符 + 控制字符 marker"。可选接收 `findKey` 让本字段
 * 参与 Cmd+F 全局搜索 —— 调用方提供唯一 key，本组件从 FindContext 拿对应
 * `matches`（事先在顶层算好），把命中区间包 `<mark>` 高亮；`active` 命中
 * 额外标 `data-active="true"` 让外层 scrollIntoView 跳过去。
 */
function PrettyStringInner({ value }: { value: string }): ReactNode {
  const find = useContext(FindCtx);
  const matches =
    find && find.query
      ? findMatches(value, find.query, find.regex, find.caseSensitive)
      : null;

  // 没匹配 / 不含控制字符 —— 直接返回原文，零额外节点
  if (!matches?.length && !hasControlChar(value)) {
    return value;
  }
  if (matches?.length) {
    return renderWithMatchesAndControlChars(value, matches);
  }
  return <>{expandControlChars(value, "s")}</>;
}

function Marker({
  sym,
  tone,
}: {
  sym: string;
  tone: "sky" | "emerald" | "cyan" | "amber";
}) {
  return (
    <span
      className={cn(
        "select-none",
        tone === "sky" && "text-sky-400/60",
        tone === "emerald" && "text-emerald-500/60",
        tone === "cyan" && "text-cyan-400/60",
        tone === "amber" &&
          "text-amber-600/80 dark:text-amber-400/80 bg-amber-500/10 px-0.5 rounded text-[9px] mx-px"
      )}
      aria-hidden="true"
    >
      {sym}
    </span>
  );
}

/**
 * 「字段名 + 单调字体盒子」公共壳，给 reasoning / content / tool_calls / text 等用。
 * 抽出去之后这几个字段的展示就只剩"塞进 PrettyJson / PrettyStringInner"了，
 * 主组件可读性也明显提升。
 */
/** 放大态会通过 context 传到 PayloadField，让它去掉 max-h 限制让内容自然撑满 modal */
const ZoomContext = createContext(false);

/**
 * Cmd+F 全局搜索状态。只传 query / regex / caseSensitive ——
 * 每个 PrettyStringInner（含 PrettyJson 嵌套里的）自己拿 context 跑 findMatches，
 * 命中部分包成 `<mark data-find-match>`。totalMatches 和 active 跳转由顶层用
 * `detailRef.querySelectorAll("mark[data-find-match]")` 后置统计 + DOM-level 切换
 * `data-active` 属性完成 —— 这样**所有**字符串字段（包括 tool_calls JSON 里的
 * `"id": "call_xxx"`、`"command": "ls /tmp"` 等嵌套字符串）都自动参与搜索，
 * 不用上层逐个收集 slot。
 */
interface FindCtxValue {
  query: string;
  regex: boolean;
  caseSensitive: boolean;
}
const FindCtx = createContext<FindCtxValue | null>(null);

/**
 * 同时处理"控制字符可视化"和"搜索命中高亮"。matches 是 value 的字节区间。
 *
 * - active 状态**不在这里**渲染（由顶层用 DOM querySelectorAll 后置切换 `data-active`），
 *   避免 active 一变就 re-render 整个 PrettyStringInner
 * - **不加 padding** —— 只用 background 着色，保持字符原始宽度，避免命中处文字往后挤
 */
function renderWithMatchesAndControlChars(
  value: string,
  matches: Array<[number, number]>
): ReactNode {
  const parts: ReactNode[] = [];
  let cursor = 0;
  matches.forEach(([s, e], matchIdx) => {
    if (s > cursor) {
      parts.push(
        ...expandControlChars(value.slice(cursor, s), `pre-${matchIdx}`)
      );
    }
    parts.push(
      <mark
        key={`m-${matchIdx}`}
        data-find-match
        // 只着色不加 padding —— 浏览器 <mark> 默认黄底，这里覆盖统一色
        // data-active="true" 由顶层 DOM 切换，对应 CSS 见 globals 或 inline 样式
        className="bg-yellow-300 text-black dark:bg-yellow-400/80 data-[active=true]:bg-amber-400"
      >
        {expandControlChars(value.slice(s, e), `match-${matchIdx}`)}
      </mark>
    );
    cursor = e;
  });
  if (cursor < value.length) {
    parts.push(...expandControlChars(value.slice(cursor), "tail"));
  }
  return <>{parts}</>;
}

/**
 * 把一段纯文本切成 [文字, marker, 文字, marker, ...] 数组。
 * 抽出来给 PrettyStringInner 和 renderWithMatchesAndControlChars 共用。
 */
function expandControlChars(value: string, keyPrefix: string): ReactNode[] {
  const parts: ReactNode[] = [];
  let buf = "";
  let key = 0;
  const flush = () => {
    if (buf) {
      parts.push(buf);
      buf = "";
    }
  };
  for (const ch of value) {
    const code = ch.codePointAt(0)!;
    if (ch === "\n") {
      flush();
      parts.push(<Marker key={`${keyPrefix}-n${key++}`} sym="↵" tone="sky" />);
      parts.push("\n");
    } else if (ch === "\t") {
      flush();
      parts.push(<Marker key={`${keyPrefix}-t${key++}`} sym="→" tone="emerald" />);
      parts.push("\t");
    } else if (ch === "\r") {
      flush();
      parts.push(<Marker key={`${keyPrefix}-r${key++}`} sym="⏎" tone="cyan" />);
    } else if (code < 32 || code === 127) {
      flush();
      const hex = code.toString(16).padStart(2, "0").toUpperCase();
      parts.push(
        <Marker key={`${keyPrefix}-x${key++}`} sym={`\\x${hex}`} tone="amber" />
      );
    } else {
      buf += ch;
    }
  }
  flush();
  return parts;
}

function PayloadField({
  label,
  children,
  copyText,
  copyJson,
}: {
  label: string;
  children: ReactNode;
  /** 字符串字段：标题行右侧渲染 StringCopyButton（双按钮：渲染后 / 原格式） */
  copyText?: string;
  /** JSON 字段：标题行右侧渲染 JsonCopyButton（双按钮：格式化 / 压缩单行） */
  copyJson?: unknown;
}) {
  const zoomed = useContext(ZoomContext);
  return (
    <div>
      <div className="flex items-center justify-between gap-2 mb-1 min-h-[20px]">
        <span className="text-[10px] text-muted-foreground">{label}</span>
        {copyText !== undefined && <StringCopyButton value={copyText} />}
        {copyJson !== undefined && <JsonCopyButton value={copyJson} />}
      </div>
      <div
        className={cn(
          "text-[11px] bg-background/60 p-2 rounded overflow-auto font-mono whitespace-pre-wrap break-words",
          zoomed ? "max-h-none" : "max-h-[400px]"
        )}
      >
        {children}
      </div>
    </div>
  );
}

/**
 * 放大态：Esc 关闭（capture 阶段拦截，避免被抽屉自己的 Esc 监听吃掉去关抽屉）。
 * 给 MessageRow / ResponseBlock 等"一个完整框"层级用 —— 用户排查时关心的是
 * "这条 message 整体发了/收了什么"，单独放大 tool_calls 字段意义不大。
 */
function useZoom() {
  const [zoomed, setZoomed] = useState(false);
  useEffect(() => {
    if (!zoomed) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setZoomed(false);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [zoomed]);
  return [zoomed, setZoomed] as const;
}

/**
 * 放大模态：portal 到抽屉容器 `#model-io-drawer-root`，**absolute** 覆盖整个
 * 抽屉范围 —— 而不是 fixed 覆盖整个浏览器窗口。视觉上"放大到调试器自己的尺寸"，
 * 不挡 hebweb sidebar / chat header。modal 内套 `ZoomContext.Provider value={true}`，
 * PayloadField 自动去掉 max-h 让内容撑满。fallback：找不到锚点时退回 body。
 */
function ZoomedModal({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const target =
    (typeof document !== "undefined" &&
      document.getElementById("model-io-drawer-root")) ||
    (typeof document !== "undefined" ? document.body : null);
  if (!target) return null;
  return createPortal(
    <div
      className="absolute inset-0 z-[50] bg-background flex flex-col"
      role="dialog"
      aria-modal="true"
    >
      <header className="h-12 shrink-0 px-4 flex items-center justify-between border-b border-border">
        <div className="text-sm font-medium">{title}</div>
        <Button variant="ghost" size="sm" onClick={onClose} title="关闭 (Esc)">
          <X className="w-4 h-4" />
        </Button>
      </header>
      <div className="flex-1 overflow-auto p-6 text-[12px]">
        <ZoomContext.Provider value={true}>{children}</ZoomContext.Provider>
      </div>
    </div>,
    target
  );
}

/** hover 才显示的"放大"按钮 —— 避免视觉杂乱 */
function ZoomButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className="text-muted-foreground hover:text-foreground p-1 rounded hover:bg-accent opacity-0 group-hover:opacity-100 transition-opacity"
      title="放大查看（Esc 关闭）"
    >
      <Maximize2 className="w-3.5 h-3.5" />
    </button>
  );
}

function hasControlChar(s: string): boolean {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 32 || c === 127) return true;
  }
  return false;
}

function Token({
  tone,
  children,
}: {
  tone: "muted" | "amber" | "sky" | "green";
  children: ReactNode;
}) {
  return (
    <span
      className={cn(
        tone === "muted" && "text-muted-foreground",
        tone === "amber" && "text-amber-600 dark:text-amber-400",
        tone === "sky" && "text-sky-700 dark:text-sky-300",
        tone === "green" && "text-emerald-700 dark:text-emerald-400"
      )}
    >
      {children}
    </span>
  );
}
