import { useEffect, useMemo, useState, useCallback } from "react";
import { Highlight, themes } from "prism-react-renderer";
import type { GraphNode, KnowledgeGraph, GraphEdge } from "@understand-anything/core/types";
import { useDashboardStore } from "../store";
import { useI18n } from "../contexts/I18nContext";

interface CodeViewerProps {
  accessToken: string;
  presentation?: "sidebar" | "modal";
  onClose?: () => void;
  onExpand?: () => void;
}

// ---- symbol index: name → 定义节点（函数/类/模块/概念）, cached per graph ----
const DEFINITION_TYPES = new Set(["function", "class", "module", "concept"]);
const IDENTIFIER_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
const symbolIndexCache = new WeakMap<KnowledgeGraph, Map<string, GraphNode[]>>();

function getSymbolIndex(graph: KnowledgeGraph): Map<string, GraphNode[]> {
  const cached = symbolIndexCache.get(graph);
  if (cached) return cached;
  const map = new Map<string, GraphNode[]>();
  for (const n of graph.nodes) {
    if (!DEFINITION_TYPES.has(n.type)) continue;
    if (!IDENTIFIER_RE.test(n.name)) continue;
    let arr = map.get(n.name);
    if (!arr) {
      arr = [];
      map.set(n.name, arr);
    }
    arr.push(n);
  }
  symbolIndexCache.set(graph, map);
  return map;
}

interface SymbolPopup {
  name: string;
  defs: GraphNode[];
  refs: { edge: GraphEdge; from: GraphNode }[];
  x: number;
  y: number;
  view: "menu" | "defs" | "refs";
}

interface SourceFile {
  path: string;
  language: string;
  content: string;
  sizeBytes: number;
  lineCount: number;
}

type SourceState =
  | { status: "idle" | "loading"; source: null; error: null }
  | { status: "loaded"; source: SourceFile; error: null }
  | { status: "error"; source: null; error: string };

function fileContentUrl(filePath: string, token: string): string {
  const params = new URLSearchParams({ token, path: filePath });
  return `/file-content.json?${params.toString()}`;
}

function fallbackLanguage(filePath: string | undefined): string {
  const ext = filePath?.split(".").pop()?.toLowerCase();
  const byExt: Record<string, string> = {
    css: "css",
    go: "go",
    html: "markup",
    js: "javascript",
    jsx: "jsx",
    json: "json",
    md: "markdown",
    py: "python",
    rb: "ruby",
    rs: "rust",
    sh: "bash",
    ts: "typescript",
    tsx: "tsx",
    yaml: "yaml",
    yml: "yaml",
  };
  return ext ? byExt[ext] ?? "text" : "text";
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function CodeViewer({
  accessToken,
  presentation = "sidebar",
  onClose,
  onExpand,
}: CodeViewerProps) {
  const graph = useDashboardStore((s) => s.graph);
  const domainGraph = useDashboardStore((s) => s.domainGraph);
  const viewMode = useDashboardStore((s) => s.viewMode);
  const codeViewerNodeId = useDashboardStore((s) => s.codeViewerNodeId);
  const closeCodeViewer = useDashboardStore((s) => s.closeCodeViewer);
  const openCodeViewer = useDashboardStore((s) => s.openCodeViewer);
  const navigateToNodeInLayer = useDashboardStore((s) => s.navigateToNodeInLayer);
  const sourceContent = useDashboardStore((s) => s.sourceContent);

  const activeGraph = viewMode === "domain" && domainGraph ? domainGraph : graph;
  // Files tab always builds its tree from the structural graph, so a node ID opened from
  // there may not exist in the active (domain) graph — fall back to the structural graph.
  const node =
    activeGraph?.nodes.find((n) => n.id === codeViewerNodeId) ??
    graph?.nodes.find((n) => n.id === codeViewerNodeId) ??
    null;

  // ---- symbol popup (jump-to-definition / find-references) ----
  const [popup, setPopup] = useState<SymbolPopup | null>(null);

  const symbolIndex = useMemo(
    () => (graph ? getSymbolIndex(graph) : new Map<string, GraphNode[]>()),
    [graph],
  );

  const handleSymbolClick = useCallback(
    (name: string, e: React.MouseEvent) => {
      if (!graph) return;
      const allDefs = symbolIndex.get(name) ?? [];
      if (allDefs.length === 0) return;
      // 同名符号可能跨语言重复（TS 的 Message vs Rust 的 Message）。
      // 优先保留与当前文件同扩展名的定义，避免把无关语言的定义/引用混进来。
      const curExt = node?.filePath?.split(".").pop()?.toLowerCase();
      let defs = allDefs;
      if (curExt) {
        const sameExt = allDefs.filter(
          (d) => d.filePath?.split(".").pop()?.toLowerCase() === curExt,
        );
        if (sameExt.length > 0) defs = sameExt;
      }
      const defIds = new Set(defs.map((d) => d.id));
      // 引用 = 指向定义节点的入边；排除 contains / exports（结构性自身关系，非使用引用）
      const refs: { edge: GraphEdge; from: GraphNode }[] = [];
      for (const edge of graph.edges) {
        if (!defIds.has(edge.target)) continue;
        if (edge.type === "contains" || edge.type === "exports") continue;
        if (defIds.has(edge.source)) continue; // 跳过定义节点之间的互链
        const from = graph.nodes.find((n) => n.id === edge.source);
        if (from) refs.push({ edge, from });
      }
      setPopup({ name, defs, refs, x: e.clientX, y: e.clientY, view: "menu" });
    },
    [graph, symbolIndex, node?.filePath],
  );

  const jumpToNode = useCallback(
    (nodeId: string) => {
      navigateToNodeInLayer(nodeId);
      openCodeViewer(nodeId);
      setPopup(null);
    },
    [navigateToNodeInLayer, openCodeViewer],
  );

  // 关闭 popup：Escape / 点击别处
  useEffect(() => {
    if (!popup) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPopup(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [popup]);

  const [state, setState] = useState<SourceState>({
    status: "idle",
    source: null,
    error: null,
  });
  const { t } = useI18n();

  useEffect(() => {
    if (!node?.filePath) {
      setState({ status: "error", source: null, error: "This node does not have a file path." });
      return;
    }

    // Static mode: look up from embedded source-content map
    if (accessToken === "__static__" && sourceContent) {
      const cached = sourceContent[node.filePath];
      if (cached) {
        setState({
          status: "loaded",
          source: {
            path: node.filePath,
            language: cached.language,
            content: cached.content,
            sizeBytes: cached.sizeBytes,
            lineCount: cached.lineCount,
          },
          error: null,
        });
      } else {
        setState({
          status: "error",
          source: null,
          error: `Source for "${node.filePath}" is not included in the static build.`,
        });
      }
      return;
    }

    if (accessToken === "__static__" && !sourceContent) {
      setState({ status: "loading", source: null, error: null });
      return;
    }

    if (accessToken === "__demo__") {
      setState({
        status: "error",
        source: null,
        error: "Source preview is available only when the local dashboard server is running.",
      });
      return;
    }

    const controller = new AbortController();
    setState({ status: "loading", source: null, error: null });

    fetch(fileContentUrl(node.filePath, accessToken), { signal: controller.signal })
      .then(async (res) => {
        const data = (await res.json()) as SourceFile | { error?: string };
        if (!res.ok) {
          throw new Error("error" in data && data.error ? data.error : "Source unavailable");
        }
        setState({ status: "loaded", source: data as SourceFile, error: null });
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) return;
        setState({
          status: "error",
          source: null,
          error: err instanceof Error ? err.message : String(err),
        });
      });

    return () => controller.abort();
  }, [accessToken, node?.filePath, sourceContent]);

  const highlightedRange = useMemo(() => {
    if (!node?.lineRange) return null;
    return { start: node.lineRange[0], end: node.lineRange[1] };
  }, [node?.lineRange]);

  if (!node) {
    return (
      <div className="h-full w-full flex items-center justify-center bg-surface">
        <p className="text-text-muted text-sm">{t.codeViewer.noFile}</p>
      </div>
    );
  }

  const source = state.source;
  const language = source?.language ?? fallbackLanguage(node.filePath);
  const lineInfo = highlightedRange
    ? `${t.codeViewer.lines} ${highlightedRange.start}-${highlightedRange.end}`
    : t.codeViewer.fullFile;
  const isModal = presentation === "modal";
  const handleClose = onClose ?? closeCodeViewer;

  return (
    <div className="h-full w-full flex flex-col bg-surface overflow-hidden">
      <div className="flex items-start gap-3 px-4 py-3 bg-elevated border-b border-border-subtle shrink-0">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 mb-1">
            <span
              className="text-[10px] font-semibold uppercase tracking-wider px-2 py-0.5 rounded border"
              style={{
                color: "var(--color-node-file)",
                borderColor: "color-mix(in srgb, var(--color-node-file) 30%, transparent)",
                backgroundColor: "color-mix(in srgb, var(--color-node-file) 10%, transparent)",
              }}
            >
              {language}
            </span>
            <span className="text-[10px] text-text-muted">{lineInfo}</span>
          </div>
          <div className="text-sm font-heading text-text-primary truncate" title={node.name}>
            {node.name}
          </div>
          {node.filePath && (
            <div className="text-[11px] font-mono text-text-muted truncate mt-0.5" title={node.filePath}>
              {node.filePath}
            </div>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {onExpand && (
            <button
              type="button"
              onClick={onExpand}
              className="text-text-muted hover:text-text-primary transition-colors"
              title={t.codeViewer.openLarger}
              aria-label={t.codeViewer.openLarger}
            >
              <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 9V4h5M20 15v5h-5M4 4l6 6M20 20l-6-6" />
              </svg>
            </button>
          )}
          <button
            type="button"
            onClick={handleClose}
            className="text-text-muted hover:text-text-primary transition-colors"
            title={isModal ? t.codeViewer.closeExpanded : t.codeViewer.closeViewer}
            aria-label={isModal ? t.codeViewer.closeExpanded : t.codeViewer.closeViewer}
          >
            <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
            </svg>
          </button>
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-auto bg-root">
        {state.status === "loading" && (
          <div className="p-5 text-sm text-text-muted">{t.codeViewer.loading}</div>
        )}

        {state.status === "error" && (
          <div className="p-5">
            <div className="rounded-lg border border-border-subtle bg-elevated p-4">
              <div className="text-sm font-medium text-text-primary mb-2">{t.codeViewer.sourceUnavailable}</div>
              <p className="text-sm text-text-secondary leading-relaxed">{state.error}</p>
            </div>
          </div>
        )}

        {source && (
          <>
            <div className="px-4 py-2 border-b border-border-subtle bg-surface text-[11px] text-text-muted flex items-center justify-between">
              <span>{source.lineCount} {t.codeViewer.linesLabel}</span>
              <span>{formatBytes(source.sizeBytes)}</span>
            </div>
            <Highlight code={source.content} language={language} theme={themes.vsDark}>
              {({ className, style, tokens, getLineProps, getTokenProps }) => (
                <pre
                  className={`${className} min-w-max p-0 m-0 ${
                    isModal ? "text-xs leading-5" : "text-[11px] leading-5"
                  } font-mono`}
                  style={{ ...style, background: "transparent" }}
                >
                  {tokens.map((line, index) => {
                    const lineNumber = index + 1;
                    const isHighlighted =
                      highlightedRange !== null &&
                      lineNumber >= highlightedRange.start &&
                      lineNumber <= highlightedRange.end;
                    const lineProps = getLineProps({ line });
                    return (
                      <div
                        key={lineNumber}
                        {...lineProps}
                        className={`${lineProps.className} flex ${
                          isHighlighted ? "bg-accent/15" : "hover:bg-elevated/40"
                        }`}
                      >
                        <span className="w-12 shrink-0 select-none border-r border-border-subtle pr-3 text-right text-text-muted bg-surface/60">
                          {lineNumber}
                        </span>
                        <span className="pl-3 pr-6 whitespace-pre">
                          {line.map((token, key) => {
                            const props = getTokenProps({ token });
                            const trimmed = token.content.trim();
                            const isSymbol =
                              trimmed.length >= 2 &&
                              IDENTIFIER_RE.test(trimmed) &&
                              symbolIndex.has(trimmed);
                            if (isSymbol) {
                              return (
                                <span
                                  key={key}
                                  {...props}
                                  onClick={(e) => {
                                    e.stopPropagation();
                                    handleSymbolClick(trimmed, e);
                                  }}
                                  className={`${props.className ?? ""} cursor-pointer rounded-sm underline decoration-dotted decoration-accent/40 underline-offset-2 hover:decoration-accent hover:bg-accent/10`}
                                />
                              );
                            }
                            return <span key={key} {...props} />;
                          })}
                        </span>
                      </div>
                    );
                  })}
                </pre>
              )}
            </Highlight>
          </>
        )}
      </div>

      {/* 符号点击弹窗：跳转定义 / 查找引用 */}
      {popup && (
        <>
          <div className="fixed inset-0 z-40" onClick={() => setPopup(null)} />
          <div
            className="fixed z-50 w-64 max-h-[60vh] overflow-auto rounded-lg border border-border-medium bg-elevated shadow-2xl text-sm"
            style={{
              left: Math.min(popup.x, window.innerWidth - 268),
              top: Math.min(popup.y, window.innerHeight - 200),
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="px-3 py-2 border-b border-border-subtle flex items-center justify-between sticky top-0 bg-elevated">
              <span className="font-mono text-accent truncate">{popup.name}</span>
              <button
                type="button"
                onClick={() => setPopup(null)}
                className="text-text-muted hover:text-text-primary shrink-0 ml-2"
              >
                <svg className="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
                </svg>
              </button>
            </div>

            {popup.view === "menu" && (
              <div className="p-1">
                <button
                  type="button"
                  onClick={() => {
                    if (popup.defs.length === 1) jumpToNode(popup.defs[0].id);
                    else setPopup({ ...popup, view: "defs" });
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2.5 rounded-md text-left text-text-secondary hover:bg-surface active:bg-surface transition-colors"
                >
                  <svg className="w-4 h-4 shrink-0 text-accent" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 7h8m0 0v8m0-8l-8 8-4-4-6 6" />
                  </svg>
                  跳转定义
                  {popup.defs.length > 1 && (
                    <span className="ml-auto text-[10px] text-text-muted">{popup.defs.length}</span>
                  )}
                </button>
                <button
                  type="button"
                  disabled={popup.refs.length === 0}
                  onClick={() => setPopup({ ...popup, view: "refs" })}
                  className="w-full flex items-center gap-2 px-3 py-2.5 rounded-md text-left text-text-secondary hover:bg-surface active:bg-surface transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                >
                  <svg className="w-4 h-4 shrink-0 text-accent" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M8 7h12m0 0l-4-4m4 4l-4 4M16 17H4m0 0l4 4m-4-4l4-4" />
                  </svg>
                  查找引用
                  <span className="ml-auto text-[10px] text-text-muted">{popup.refs.length}</span>
                </button>
              </div>
            )}

            {popup.view === "defs" && (
              <div className="py-1">
                {popup.defs.map((d) => (
                  <button
                    key={d.id}
                    type="button"
                    onClick={() => jumpToNode(d.id)}
                    className="w-full px-3 py-2 text-left hover:bg-surface active:bg-surface transition-colors"
                  >
                    <div className="flex items-center gap-1.5">
                      <span className="text-[9px] uppercase text-text-muted">{d.type}</span>
                      <span className="text-text-secondary font-mono text-xs truncate">{d.name}</span>
                    </div>
                    {d.filePath && (
                      <div className="text-[10px] text-text-muted font-mono truncate mt-0.5">
                        {d.filePath}{d.lineRange ? `:${d.lineRange[0]}` : ""}
                      </div>
                    )}
                  </button>
                ))}
              </div>
            )}

            {popup.view === "refs" && (
              <div className="py-1">
                {popup.refs.length === 0 && (
                  <div className="px-3 py-3 text-xs text-text-muted">没有找到引用</div>
                )}
                {popup.refs.map(({ edge, from }, i) => (
                  <button
                    key={`${from.id}-${i}`}
                    type="button"
                    onClick={() => jumpToNode(from.id)}
                    className="w-full px-3 py-2 text-left hover:bg-surface active:bg-surface transition-colors"
                  >
                    <div className="flex items-center gap-1.5">
                      <span className="text-[9px] uppercase text-accent/70">{edge.type}</span>
                      <span className="text-text-secondary font-mono text-xs truncate">{from.name}</span>
                    </div>
                    {from.filePath && (
                      <div className="text-[10px] text-text-muted font-mono truncate mt-0.5">
                        {from.filePath}
                      </div>
                    )}
                  </button>
                ))}
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}
