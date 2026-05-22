import {
  createContext,
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
  Copy,
  Check,
  PanelLeftClose,
  PanelLeftOpen,
  Maximize2,
} from "lucide-react";
import { api } from "@/desktop/bridge/tauri";
import { Button } from "@/desktop/ui/components/ui/button";
import { cn } from "@/desktop/ui/lib/utils";
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

export function ModelIoInspector({ sessionId, open, onClose }: Props) {
  const [entries, setEntries] = useState<ModelIoEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [selected, setSelected] = useState<number>(0);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  // ─── Cmd+F 全局搜索状态 ─────────────────────────────────────────────────
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findRegex, setFindRegex] = useState(false);
  const [findCase, setFindCase] = useState(false);
  const [findActive, setFindActive] = useState(0);
  const detailRef = useRef<HTMLDivElement>(null);

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

  // 切换 entry / 关抽屉时关 find
  useEffect(() => {
    setFindOpen(false);
    setFindQuery("");
    setFindActive(0);
  }, [selected, open]);

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

  // ESC 关闭抽屉 / find（zoom 自己 capture 阶段已先吃掉 Esc，所以不互相干扰）
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (findOpen) {
          setFindOpen(false);
          e.stopPropagation();
          return;
        }
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose, findOpen]);

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

  return (
    <div className="fixed inset-0 z-[100] flex" role="dialog" aria-modal="true">
      {/* 半透明遮罩 —— 点它关闭 */}
      <div
        className="absolute inset-0 bg-background/40 backdrop-blur-[2px]"
        onClick={onClose}
      />
      {/* 右侧抽屉 */}
      <div
        id="model-io-drawer-root"
        className="relative ml-auto h-full w-[min(1100px,75vw)] border-l border-border bg-background shadow-2xl flex flex-col"
        data-testid="model-io-drawer"
      >
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
                <ol className="h-full overflow-y-auto">
                  {entries.map((e, idx) => (
                    <RequestRow
                      key={`${e.run_id}-${e.turn}-${idx}`}
                      entry={e}
                      index={idx}
                      active={idx === selected}
                      matchCount={perEntryMatchCount[idx] ?? 0}
                      onClick={() => setSelected(idx)}
                    />
                  ))}
                </ol>
              )}
            </aside>

            {/* 右：详情 —— 相对定位让 FindBar 浮在右上角 */}
            <section
              ref={detailRef}
              className="relative flex-1 min-w-0 overflow-y-auto"
            >
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
          </div>
        )}
      </div>
    </div>
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

      {/* system prompt（默认折叠） */}
      {entry.request?.system ? (
        <CollapsibleBlock
          open={systemOpen}
          onToggle={() => setSystemOpen((v) => !v)}
          label="system prompt"
          sublabel={`${entry.request.system.length} 字符`}
        >
          <pre className="px-3 py-2 text-[11px] whitespace-pre-wrap break-words bg-muted/30 max-h-[400px] overflow-auto rounded font-mono">
            <PrettyStringInner value={entry.request.system} />
          </pre>
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
        <PayloadField label="reasoning">
          <PrettyStringInner
            value={msg.reasoning}
          />
        </PayloadField>
      ) : null}
      {msg.content ? (
        <PayloadField label="content">
          <PrettyStringInner
            value={msg.content}
          />
        </PayloadField>
      ) : null}
      {msg.tool_calls && msg.tool_calls.length > 0 ? (
        <PayloadField label="tool_calls">
          <PrettyJson value={msg.tool_calls} />
        </PayloadField>
      ) : null}
      {msg.results && msg.results.length > 0 ? (
        <PayloadField label="tool results">
          <PrettyJson value={msg.results} />
        </PayloadField>
      ) : null}
      {msg.attachments && msg.attachments.length > 0 ? (
        <PayloadField label="attachments">
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
        <PayloadField label="reasoning">
          <PrettyStringInner
            value={response.reasoning}
          />
        </PayloadField>
      ) : null}
      {response?.text ? (
        <PayloadField label="text">
          <PrettyStringInner value={response.text} />
        </PayloadField>
      ) : null}
      {response?.calls && response.calls.length > 0 ? (
        <PayloadField label="tool_calls">
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

// ─── helpers ────────────────────────────────────────────────────────────────

function CollapsibleBlock({
  open,
  onToggle,
  label,
  sublabel,
  children,
}: {
  open: boolean;
  onToggle: () => void;
  label: string;
  sublabel?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="border border-border rounded overflow-hidden">
      <button
        onClick={onToggle}
        className="w-full px-3 py-1.5 flex items-center gap-2 text-left hover:bg-accent/30 bg-muted/30"
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
      {open ? <div>{children}</div> : null}
    </div>
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
function PrettyJson({ value }: { value: unknown }) {
  return <PrettyJsonNode value={value} keyLabel={null} isLast />;
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
}: {
  keyLabel: string | null;
  isArray: boolean;
  openBracket: string;
  closeBracket: string;
  entries: Array<[string | number, unknown]>;
  isLast: boolean;
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
}: {
  label: string;
  children: ReactNode;
}) {
  const zoomed = useContext(ZoomContext);
  return (
    <div>
      <div className="text-[10px] text-muted-foreground mb-1">{label}</div>
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
