import { useState, useRef, useEffect, useCallback, type FC } from "react";
import ReactMarkdown from "react-markdown";
import type { GraphNode, KnowledgeGraph } from "@understand-anything/core/types";
import { useDashboardStore } from "../store";

// ---- file-path → node index (cached per graph object) ----
interface PathIndex {
  fileByPath: Map<string, string>; // filePath → file-node id
  nodesByPath: Map<string, GraphNode[]>; // filePath → all nodes on that file
}
const pathIndexCache = new WeakMap<KnowledgeGraph, PathIndex>();
function getPathIndex(graph: KnowledgeGraph): PathIndex {
  const cached = pathIndexCache.get(graph);
  if (cached) return cached;
  const fileByPath = new Map<string, string>();
  const nodesByPath = new Map<string, GraphNode[]>();
  for (const n of graph.nodes) {
    if (!n.filePath) continue;
    let arr = nodesByPath.get(n.filePath);
    if (!arr) {
      arr = [];
      nodesByPath.set(n.filePath, arr);
    }
    arr.push(n);
    if (n.type === "file" && !fileByPath.has(n.filePath)) fileByPath.set(n.filePath, n.id);
  }
  // 没有 file 类型节点的路径，退回该路径下第一个节点
  for (const [p, arr] of nodesByPath) {
    if (!fileByPath.has(p)) fileByPath.set(p, arr[0].id);
  }
  const idx = { fileByPath, nodesByPath };
  pathIndexCache.set(graph, idx);
  return idx;
}

// ---- localStorage keys ----
const LS_API_KEY = "ua-chat-apikey";
const LS_MODEL = "ua-chat-model";
const LS_ENDPOINT = "ua-chat-endpoint";
const LS_CONVOS = "ua-chat-conversations";
const LS_ACTIVE_CONVO = "ua-chat-active-convo";

// ---- types ----
interface ChatMessage {
  role: "user" | "assistant";
  content: string;
}

interface Conversation {
  id: string;
  title: string;
  createdAt: string;
  messages: ChatMessage[];
}

const MAX_SOURCE_LINES = 120; // max lines of source to inline per node
const MAX_SEARCH_NODES = 10; // max graph nodes to include in context
const MAX_SOURCE_NODES = 5; // max nodes whose source we actually inline

/** Generate stable short id */
function uid(): string {
  return Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
}

/** Detect language from file extension */
function langFromPath(p: string): string {
  const ext = p.split(".").pop()?.toLowerCase();
  const m: Record<string, string> = {
    rs: "rust", ts: "typescript", tsx: "tsx", js: "javascript", jsx: "jsx",
    py: "python", css: "css", html: "html", json: "json", md: "markdown",
    toml: "toml", yaml: "yaml", yml: "yaml", sh: "bash", sql: "sql",
    go: "go", rb: "ruby", swift: "swift", kt: "kotlin", java: "java",
  };
  return m[ext ?? ""] ?? "";
}

/** Build rich context: project info + layers + matching nodes + their source code */
function buildChatContext(query: string): string {
  const { graph, searchEngine, sourceContent, nodeIdToLayerIds } =
    useDashboardStore.getState();
  if (!graph) return "";
  const layers = graph.layers;

  const parts: string[] = [];

  // -- project overview --
  parts.push(`## 项目: ${graph.project.name}`);
  parts.push(graph.project.description);
  parts.push(
    `语言: ${graph.project.languages.join(", ")}  |  框架: ${graph.project.frameworks.join(", ")}`,
  );
  parts.push("");

  // -- layers --
  if (graph.layers.length > 0) {
    parts.push("### 架构分层");
    for (const l of graph.layers) {
      parts.push(`- **${l.name}**: ${l.description} (${l.nodeIds.length} 节点)`);
    }
    parts.push("");
  }

  // -- search relevant nodes --
  let relevantIds: string[] = [];
  if (searchEngine && query.trim()) {
    relevantIds = searchEngine
      .search(query, { limit: MAX_SEARCH_NODES })
      .map((r) => r.nodeId);
  }

  const relevantNodes = relevantIds
    .map((id) => graph.nodes.find((n) => n.id === id))
    .filter(Boolean);

  if (relevantNodes.length === 0) {
    parts.push(
      "回答时请引用具体的文件路径和函数名，这样用户可以点击跳转。",
    );
  } else {
    parts.push("### 与用户问题最相关的代码组件");
    for (const node of relevantNodes) {
      const layerIds = nodeIdToLayerIds.get(node!.id);
      const layerNames = layerIds
        ? [...layerIds]
            .map((lid) => layers.find((l) => l.id === lid)?.name)
            .filter(Boolean)
            .join(", ")
        : "";

      parts.push(
        `- **\`${node!.name}\`** (${node!.type})` +
          (layerNames ? ` — 层: ${layerNames}` : "") +
          (node!.filePath ? ` — 文件: \`${node!.filePath}\`` : ""),
      );
      parts.push(`  ${node!.summary}`);
      if (node!.languageNotes) parts.push(`  💡 ${node!.languageNotes}`);
    }
    parts.push("");

    // -- inline source for top N file nodes --
    if (sourceContent) {
      const fileNodes = relevantNodes
        .filter((n) => n!.filePath && sourceContent[n!.filePath])
        .slice(0, MAX_SOURCE_NODES);
      if (fileNodes.length > 0) {
        parts.push("### 相关源码");
        for (const node of fileNodes) {
          const sc = sourceContent[node!.filePath!];
          if (!sc) continue;
          const lines = sc.content.split("\n");
          const preview =
            lines.length <= MAX_SOURCE_LINES
              ? sc.content
              : lines.slice(0, MAX_SOURCE_LINES).join("\n") +
                `\n... (共 ${lines.length} 行，已截断)`;
          parts.push(
            `#### \`${node!.filePath}\` (${sc.language}, ${sc.lineCount} 行)`,
          );
          parts.push("```" + langFromPath(node!.filePath!) + "\n" + preview + "\n```");
          parts.push("");
        }
      }
    }

    // -- reference guide --
    parts.push(
      "### 引用格式（让用户能点击跳转）\n" +
        "1. 引用图谱节点：用 `[@节点ID]`（见下方列表），渲染为按钮，点击在图谱定位并打开源码。\n" +
        "2. 引用文件/代码位置：在行内代码里直接写文件路径，可带行号，如 `apps/cli/src/main.rs` 或 `apps/cli/src/main.rs:42`；带行号会定位到对应函数/类并高亮。\n" +
        "尽量多用这两种格式，方便用户点击查看源码。",
    );
    parts.push(
      relevantNodes
        .map((n) => `- ${n!.name} → \`[@${n!.id}]\``)
        .join("\n"),
    );
  }

  parts.push("\n回答简洁准确，基于图谱和源码数据，不要编造不存在的信息。");
  return parts.join("\n");
}

// ---- custom link: node:// → jump to graph + open code ----
const MarkdownLink: FC<{ href?: string; children?: React.ReactNode }> = ({
  href,
  children,
}) => {
  const navigateToNodeInLayer = useDashboardStore((s) => s.navigateToNodeInLayer);
  const openCodeViewer = useDashboardStore((s) => s.openCodeViewer);
  const graph = useDashboardStore((s) => s.graph);

  if (href?.startsWith("node://")) {
    const nodeId = decodeURIComponent(href.slice(7));
    const node = graph?.nodes.find((n) => n.id === nodeId);
    return (
      <button
        type="button"
        onClick={() => {
          navigateToNodeInLayer(nodeId);
          if (node?.filePath) openCodeViewer(nodeId);
        }}
        className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-accent/10 text-accent hover:bg-accent/20 transition-colors text-xs font-mono cursor-pointer"
        title={node?.summary ?? nodeId}
      >
        {children ?? (node?.name ?? nodeId)}
      </button>
    );
  }
  return (
    <a href={href} target="_blank" rel="noopener noreferrer" className="text-accent underline">
      {children}
    </a>
  );
};

// ---- inline code: 若是图谱里存在的文件路径（可带 :行号）则可点击跳转 ----
const CodeRef: FC<{ className?: string; children?: React.ReactNode }> = ({
  className,
  children,
}) => {
  const graph = useDashboardStore((s) => s.graph);
  const navigateToNodeInLayer = useDashboardStore((s) => s.navigateToNodeInLayer);
  const openCodeViewer = useDashboardStore((s) => s.openCodeViewer);

  const text = String(children ?? "");
  // 代码块（带语言 class 或多行）保持原样高亮，不当路径处理
  const isBlock = /language-/.test(className ?? "") || text.includes("\n");

  if (!isBlock && graph) {
    const candidate = text.trim();
    // 形如 path/to/file.ext 或 path/to/file.ext:42，无空格
    const m = candidate.match(/^([\w./@-]+\.[\w]+)(?::(\d+))?$/);
    if (m) {
      const path = m[1];
      const line = m[2] ? parseInt(m[2], 10) : null;
      const idx = getPathIndex(graph);
      if (idx.fileByPath.has(path)) {
        const handleJump = () => {
          let targetId = idx.fileByPath.get(path)!;
          // 带行号时，定位到包含该行的函数/类节点（带高亮）
          if (line != null) {
            const within = idx.nodesByPath
              .get(path)
              ?.find(
                (n) =>
                  n.type !== "file" &&
                  n.lineRange &&
                  line >= n.lineRange[0] &&
                  line <= n.lineRange[1],
              );
            if (within) targetId = within.id;
          }
          navigateToNodeInLayer(targetId);
          openCodeViewer(targetId);
        };
        return (
          <button
            type="button"
            onClick={handleJump}
            className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-accent/10 text-accent hover:bg-accent/20 active:bg-accent/30 transition-colors text-xs font-mono cursor-pointer align-baseline"
            title={`打开 ${candidate}`}
          >
            <svg className="w-3 h-3 shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 7h8m0 0v8m0-8l-8 8-4-4-6 6" />
            </svg>
            {children}
          </button>
        );
      }
    }
  }

  return <code className={className}>{children}</code>;
};

// ---- conversation list sidebar ----
function ConversationList({
  convos,
  activeId,
  onSelect,
  onNew,
  onDelete,
}: {
  convos: Conversation[];
  activeId: string;
  onSelect: (id: string) => void;
  onNew: () => void;
  onDelete: (id: string) => void;
}) {
  return (
    <div className="flex flex-col h-full bg-surface">
      <div className="flex items-center justify-between px-3 py-2.5 border-b border-border-subtle shrink-0">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-text-muted">
          对话
        </span>
        <button
          type="button"
          onClick={onNew}
          className="min-w-[36px] min-h-[36px] flex items-center justify-center rounded-lg text-accent hover:bg-accent/10 active:scale-95 transition-all"
          title="新建对话"
        >
          <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 4v16m8-8H4" />
          </svg>
        </button>
      </div>
      <div className="flex-1 overflow-auto">
        {convos.map((c) => (
          <div
            key={c.id}
            className={`group flex items-center gap-2 pl-3 pr-1.5 py-2.5 cursor-pointer border-b border-border-subtle/50 transition-colors ${
              c.id === activeId ? "bg-accent/10" : "hover:bg-elevated active:bg-elevated"
            }`}
            onClick={() => onSelect(c.id)}
          >
            <div className="flex-1 min-w-0">
              <div className="text-xs text-text-secondary truncate">
                {c.title || "新对话"}
              </div>
              <div className="text-[10px] text-text-muted mt-0.5">
                {c.messages.length} 条消息
              </div>
            </div>
            {convos.length > 1 && (
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  onDelete(c.id);
                }}
                className="shrink-0 min-w-[34px] min-h-[34px] flex items-center justify-center rounded-lg text-text-muted/60 hover:text-red-400 hover:bg-red-400/10 active:scale-95 transition-all md:opacity-0 md:group-hover:opacity-100"
                title="删除对话"
              >
                <svg className="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
                </svg>
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

// ---- load conversations from localStorage ----
function loadConversations(): Conversation[] {
  try {
    const raw = localStorage.getItem(LS_CONVOS);
    return raw ? (JSON.parse(raw) as Conversation[]) : [];
  } catch {
    return [];
  }
}

function saveConversations(convos: Conversation[]) {
  localStorage.setItem(LS_CONVOS, JSON.stringify(convos));
}

// ---- main component ----
export default function ChatPanel() {
  const graph = useDashboardStore((s) => s.graph);
  const sourceContent = useDashboardStore((s) => s.sourceContent);

  // conversations
  const [convos, setConvos] = useState<Conversation[]>(loadConversations);
  const [activeConvoId, setActiveConvoId] = useState<string>(() => {
    const saved = localStorage.getItem(LS_ACTIVE_CONVO);
    const list = loadConversations();
    if (saved && list.find((c) => c.id === saved)) return saved;
    return list[0]?.id ?? "";
  });

  // ensure at least one conversation exists
  useEffect(() => {
    if (convos.length === 0 && activeConvoId === "") {
      const seed: Conversation = {
        id: uid(),
        title: "",
        createdAt: new Date().toISOString(),
        messages: [],
      };
      setConvos([seed]);
      setActiveConvoId(seed.id);
      saveConversations([seed]);
      localStorage.setItem(LS_ACTIVE_CONVO, seed.id);
    }
  }, [convos.length, activeConvoId]);

  // derive title from first user message
  const deriveTitle = useCallback((text: string): string => {
    // truncate and strip markdown
    const clean = text.replace(/[#*`~\[\]()]/g, "").trim();
    return clean.length <= 40 ? clean : clean.slice(0, 40) + "…";
  }, []);

  const activeConvo = convos.find((c) => c.id === activeConvoId) ?? null;
  const messages = activeConvo?.messages ?? [];

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [showConvoList, setShowConvoList] = useState(false);

  const [apiKey, setApiKey] = useState(() => localStorage.getItem(LS_API_KEY) ?? "");
  const [model, setModel] = useState(
    () => localStorage.getItem(LS_MODEL) ?? "deepseek-chat",
  );
  const [endpoint, setEndpoint] = useState(
    () => localStorage.getItem(LS_ENDPOINT) ?? "https://api.deepseek.com/v1/chat/completions",
  );

  const bottomRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, sending]);

  // persist convos whenever they change
  useEffect(() => {
    saveConversations(convos);
  }, [convos]);

  // persist active convo id
  useEffect(() => {
    if (activeConvoId) localStorage.setItem(LS_ACTIVE_CONVO, activeConvoId);
  }, [activeConvoId]);

  const handleSaveSettings = useCallback(() => {
    localStorage.setItem(LS_API_KEY, apiKey);
    localStorage.setItem(LS_MODEL, model);
    localStorage.setItem(LS_ENDPOINT, endpoint);
    setShowSettings(false);
  }, [apiKey, model, endpoint]);

  const updateMessages = useCallback(
    (updater: (prev: ChatMessage[]) => ChatMessage[]) => {
      setConvos((prev) => {
        const next = prev.map((c) =>
          c.id === activeConvoId ? { ...c, messages: updater(c.messages) } : c,
        );
        return next;
      });
    },
    [activeConvoId],
  );

  const handleSend = useCallback(async () => {
    const text = input.trim();
    if (!text || sending || !apiKey || !activeConvoId) return;

    setInput("");
    updateMessages((prev) => [...prev, { role: "user", content: text }]);
    setSending(true);

    // Auto-derive title
    setConvos((prev) =>
      prev.map((c) =>
        c.id === activeConvoId && !c.title
          ? { ...c, title: deriveTitle(text) }
          : c,
      ),
    );

    const context = buildChatContext(text);
    const systemPrompt = context
      ? `你是代码库专家助手。下面的信息包含项目架构、知识图谱搜索结果和相关源码，请据此回答用户问题。\n\n${context}`
      : "你是代码库专家助手。";

    try {
      const currentMessages = useDashboardStore.getState()
        ? convos.find((c) => c.id === activeConvoId)?.messages ?? []
        : [];
      const historyExcludingLast = currentMessages.filter(
        (_, i) => i < currentMessages.length - 1,
      );

      const res = await fetch(endpoint, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${apiKey}`,
        },
        body: JSON.stringify({
          model,
          messages: [
            { role: "system", content: systemPrompt },
            ...historyExcludingLast.map((m) => ({
              role: m.role,
              content: m.content,
            })),
            { role: "user", content: text },
          ],
          max_tokens: 4096,
          temperature: 0.3,
        }),
      });

      if (!res.ok) {
        const errBody = await res.json().catch(() => ({}));
        const errMsg =
          (errBody as { error?: { message?: string } })?.error?.message ??
          `HTTP ${res.status}`;
        throw new Error(errMsg);
      }

      const data = (await res.json()) as {
        choices?: { message?: { content?: string } }[];
      };
      const reply =
        data.choices?.[0]?.message?.content ?? "(模型返回了空内容)";

      // Pre-process node references: [@nodeId] → node:// links
      const processedReply = reply.replace(
        /\[@([^\]]+)\]/g,
        (_match: string, nodeId: string) => {
          const node = graph?.nodes.find((n) => n.id === nodeId);
          const label = node?.name ?? nodeId;
          return `[\`${label}\`](node://${encodeURIComponent(nodeId)})`;
        },
      );

      updateMessages((prev) => [
        ...prev,
        { role: "assistant", content: processedReply },
      ]);
    } catch (err) {
      updateMessages((prev) => [
        ...prev,
        {
          role: "assistant",
          content: `❌ **出错了**: ${err instanceof Error ? err.message : String(err)}`,
        },
      ]);
    } finally {
      setSending(false);
    }
  }, [
    input,
    sending,
    apiKey,
    endpoint,
    model,
    activeConvoId,
    convos,
    graph,
    sourceContent,
    updateMessages,
    deriveTitle,
  ]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  const handleNewConvo = useCallback(() => {
    const seed: Conversation = {
      id: uid(),
      title: "",
      createdAt: new Date().toISOString(),
      messages: [],
    };
    setConvos((prev) => [seed, ...prev]);
    setActiveConvoId(seed.id);
    setInput("");
  }, []);

  const handleDeleteConvo = useCallback(
    (id: string) => {
      setConvos((prev) => {
        const next = prev.filter((c) => c.id !== id);
        if (next.length === 0) {
          // shouldn't happen — UI blocks delete on last convo
          const seed: Conversation = {
            id: uid(),
            title: "",
            createdAt: new Date().toISOString(),
            messages: [],
          };
          setActiveConvoId(seed.id);
          return [seed];
        }
        if (id === activeConvoId) setActiveConvoId(next[0].id);
        return next;
      });
    },
    [activeConvoId],
  );

  const canSend = input.trim().length > 0 && !sending && apiKey.length > 0;

  return (
    <div className="h-full relative flex bg-surface">
      {/* Conversation list — overlay so it never squeezes the chat on narrow screens */}
      {showConvoList && (
        <>
          <div
            className="absolute inset-0 z-20 bg-black/40"
            onClick={() => setShowConvoList(false)}
          />
          <div className="absolute inset-y-0 left-0 z-30 w-[240px] max-w-[80%] border-r border-border-subtle shadow-2xl animate-slide-in-left">
            <ConversationList
              convos={convos}
              activeId={activeConvoId ?? ""}
              onSelect={(id) => {
                setActiveConvoId(id);
                setShowConvoList(false);
              }}
              onNew={handleNewConvo}
              onDelete={handleDeleteConvo}
            />
          </div>
        </>
      )}

      {/* Main chat area */}
      <div className="flex-1 flex flex-col min-w-0 min-h-0">
        {/* settings panel */}
        {showSettings && (
          <div className="border-b border-border-subtle bg-elevated p-4 space-y-3 shrink-0 animate-slide-up">
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-semibold text-text-primary">API 设置</h3>
              <button
                type="button"
                onClick={() => setShowSettings(false)}
                className="text-text-muted hover:text-text-primary"
              >
                <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
                </svg>
              </button>
            </div>
            <label className="block">
              <span className="text-[11px] text-text-muted uppercase tracking-wider">API 地址</span>
              <input type="url" value={endpoint} onChange={(e) => setEndpoint(e.target.value)}
                className="w-full mt-1 px-3 py-1.5 rounded-lg bg-root border border-border-subtle text-sm text-text-primary font-mono focus:outline-none focus:border-accent"
                placeholder="https://api.deepseek.com/v1/chat/completions" />
            </label>
            <label className="block">
              <span className="text-[11px] text-text-muted uppercase tracking-wider">模型</span>
              <input type="text" value={model} onChange={(e) => setModel(e.target.value)}
                className="w-full mt-1 px-3 py-1.5 rounded-lg bg-root border border-border-subtle text-sm text-text-primary font-mono focus:outline-none focus:border-accent"
                placeholder="deepseek-chat" />
            </label>
            <label className="block">
              <span className="text-[11px] text-text-muted uppercase tracking-wider">API Key</span>
              <input type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)}
                className="w-full mt-1 px-3 py-1.5 rounded-lg bg-root border border-border-subtle text-sm text-text-primary font-mono focus:outline-none focus:border-accent"
                placeholder="sk-..." />
              <span className="text-[10px] text-text-muted mt-1 block">仅在浏览器本地存储，不上传任何服务器</span>
            </label>
            <button type="button" onClick={handleSaveSettings}
              className="w-full px-4 py-1.5 rounded-lg bg-accent text-white text-sm font-medium hover:bg-accent/80 transition-colors">
              保存
            </button>
          </div>
        )}

        {/* messages */}
        <div className="flex-1 min-h-0 overflow-auto px-4 py-3 space-y-4">
          {messages.length === 0 && !sending && (
            <div className="text-center text-text-muted text-sm py-12 space-y-3">
              <div className="text-4xl">💬</div>
              <p className="font-medium text-text-secondary">边聊天边理解项目</p>
              <p className="text-xs leading-relaxed max-w-[280px] mx-auto">
                {apiKey
                  ? graph
                    ? "尽管问——我会结合知识图谱和源码来回答，引用可以点击跳转到图谱。"
                    : "图谱还没加载……不过仍然可以聊天。"
                  : "点击 ⚙ 设置 API Key（默认 DeepSeek，支持 OpenAI 兼容接口）"}
              </p>
            </div>
          )}

          {messages.map((msg, i) => (
            <div
              key={i}
              className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}
            >
              <div
                className={`max-w-[90%] rounded-xl px-4 py-2.5 text-sm leading-relaxed ${
                  msg.role === "user"
                    ? "bg-accent/15 text-text-primary rounded-br-sm"
                    : "bg-elevated text-text-primary rounded-bl-sm border border-border-subtle"
                }`}
              >
                {msg.role === "assistant" ? (
                  <div className="prose prose-sm prose-invert max-w-none break-words [&_pre]:bg-root [&_pre]:border [&_pre]:border-border-subtle [&_pre]:rounded-lg [&_pre]:p-3 [&_pre]:overflow-auto [&_pre]:text-xs [&_code]:text-accent [&_code]:font-mono [&_p]:mb-2 [&_ul]:mb-2 [&_ol]:mb-2 [&_li]:mb-0.5">
                    <ReactMarkdown components={{ a: MarkdownLink, code: CodeRef }}>
                      {msg.content}
                    </ReactMarkdown>
                  </div>
                ) : (
                  <p className="whitespace-pre-wrap break-words">{msg.content}</p>
                )}
              </div>
            </div>
          ))}

          {sending && (
            <div className="flex justify-start">
              <div className="bg-elevated border border-border-subtle rounded-xl rounded-bl-sm px-4 py-2.5">
                <div className="flex items-center gap-2 text-text-muted">
                  <span className="flex gap-1">
                    <span className="w-1.5 h-1.5 rounded-full bg-accent animate-bounce [animation-delay:0ms]" />
                    <span className="w-1.5 h-1.5 rounded-full bg-accent animate-bounce [animation-delay:150ms]" />
                    <span className="w-1.5 h-1.5 rounded-full bg-accent animate-bounce [animation-delay:300ms]" />
                  </span>
                </div>
              </div>
            </div>
          )}

          <div ref={bottomRef} />
        </div>

        {/* input */}
        <div className="shrink-0 border-t border-border-subtle p-3">
          <div className="flex items-center gap-2">
            {/* convo list toggle */}
            <button
              type="button"
              onClick={() => setShowConvoList((v) => !v)}
              className={`shrink-0 min-w-[40px] min-h-[40px] flex items-center justify-center rounded-lg transition-colors active:scale-95 ${
                showConvoList
                  ? "bg-accent/15 text-accent"
                  : "text-text-muted hover:text-text-primary hover:bg-elevated active:bg-elevated"
              }`}
              title="对话列表"
            >
              <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 6h16M4 12h16M4 18h7" />
              </svg>
            </button>

            <input
              ref={inputRef}
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={apiKey ? "问点关于项目的事……" : "先设置 API Key"}
              disabled={sending || !apiKey}
              className="flex-1 px-3 py-2 rounded-lg bg-root border border-border-subtle text-sm text-text-primary placeholder:text-text-muted focus:outline-none focus:border-accent disabled:opacity-50"
            />

            <button
              type="button"
              onClick={() => setShowSettings((v) => !v)}
              className={`shrink-0 min-w-[40px] min-h-[40px] flex items-center justify-center rounded-lg transition-colors active:scale-95 ${
                showSettings ? "bg-accent/15 text-accent" : "text-text-muted hover:text-text-primary hover:bg-elevated active:bg-elevated"
              }`}
              title="API 设置"
            >
              <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.066 2.573c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.573 1.066c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.066-2.573c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z" />
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
              </svg>
            </button>

            <button
              type="button"
              onClick={handleSend}
              disabled={!canSend}
              className="shrink-0 min-w-[40px] min-h-[40px] flex items-center justify-center rounded-lg bg-accent text-white hover:bg-accent/80 active:scale-95 transition-transform disabled:opacity-40 disabled:cursor-not-allowed"
            >
              <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 19l9 2-9-18-9 18 9-2zm0 0v-8" />
              </svg>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
