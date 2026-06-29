import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Editor, { DiffEditor, type OnMount } from "@monaco-editor/react";
import type { editor } from "monaco-editor";
import { toast } from "sonner";
import {
  X,
  Circle,
  Pin,
  Eye,
  Code2,
  FileText,
  GitCompare,
  ClipboardList,
  Columns2,
  Rows3,
  MessageSquarePlus,
  History,
  Loader2,
  Check,
  MessageSquareWarning,
} from "lucide-react";
import {
  useStore,
  selectCurrentEditorTabs,
  selectCurrentActiveTab,
  type EditorTab,
} from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { detectLanguage, fileName } from "@/desktop/ui/lib/fileLanguage";
import { fetchEditDiff } from "@/desktop/ui/lib/diffCache";
import { MarkdownRenderer } from "@/desktop/ui/components/MarkdownRenderer";
import type { DiffPayload, PlanComment } from "@/desktop/ui/types";
import "@/desktop/ui/lib/monacoSetup";

/**
 * 工作区编辑区（中间列）：VSCode 风格的多 tab 编辑器，承载三类异构 tab。
 *
 * - **file**：Monaco 编辑器，语法高亮 + Markdown 预览 + Ctrl/Cmd+S 写盘；选中文本实时
 *   写入 `editorSelectionRef`，chat 输入框上方出现 `path:line` 引用
 * - **diff**：Monaco DiffEditor（左右分屏 / 行内可切），只读展示某次 Run 某文件的净变化
 * - **plan**：plan markdown 预览 + 选区评论 + 评论列表 + 待审批操作条
 *
 * 三类 tab 混在同一条页签栏，按 kind 给图标区分；× 关闭、📌 固定（固定后切对话仍保留）。
 * 文件按对话隔离（store per-session）。整个组件在 DesktopShell 里懒加载，Monaco 不进主 bundle。
 */

/** 单个文件的内存态：磁盘原文 + 编辑器当前值 + 加载态。 */
interface FileState {
  diskText: string;
  draft: string;
  loading: boolean;
  error: string | null;
}

/** 暗色调色盘预设 id（其余预设都按亮色编辑器渲染）。 */
const DARK_PRESETS = new Set(["abyss"]);

/**
 * Monaco 编辑器公共选项：VSCode 默认配色（vs / vs-dark）+ 默认等宽字体栈 + 默认字号。
 * 字体栈对齐 VSCode 的 `editor.fontFamily` 默认值（mac 用 Menlo、win 用 Consolas、
 * linux 用 'Droid Sans Mono'），末尾 monospace 兜底。
 */
const VSCODE_FONT_FAMILY =
  "Menlo, Monaco, 'Courier New', Consolas, 'Droid Sans Mono', 'DejaVu Sans Mono', monospace";
const VSCODE_FONT_SIZE = 14;

/**
 * Monaco 主题跟随调色盘预设：亮色预设 → "vs"，暗色预设 → "vs-dark"。
 * 读 `.dsp-shell` 上的 `data-dsp-theme` 属性，用 MutationObserver 响应切换。
 */
function useEditorTheme(): "vs" | "vs-dark" {
  const read = () => {
    const id = document.querySelector("[data-dsp-theme]")?.getAttribute("data-dsp-theme") ?? "";
    return DARK_PRESETS.has(id) ? "vs-dark" : "vs";
  };
  const [theme, setTheme] = useState<"vs" | "vs-dark">(read);
  useEffect(() => {
    const el = document.querySelector("[data-dsp-theme]");
    if (!el) return;
    const obs = new MutationObserver(() => setTheme(read()));
    obs.observe(el, { attributes: true, attributeFilter: ["data-dsp-theme"] });
    return () => obs.disconnect();
  }, []);
  return theme;
}

function isMarkdown(path: string): boolean {
  return /\.(md|markdown)$/i.test(path);
}

function tabIcon(kind: EditorTab["kind"]) {
  if (kind === "diff") return <GitCompare className="h-3 w-3 shrink-0 opacity-70" />;
  if (kind === "gitDiff") return <GitCompare className="h-3 w-3 shrink-0 opacity-70" />;
  if (kind === "plan") return <ClipboardList className="h-3 w-3 shrink-0 opacity-70" />;
  return <FileText className="h-3 w-3 shrink-0 opacity-70" />;
}

function tabLabel(tab: EditorTab): string {
  if (tab.kind === "plan") return tab.title || "计划";
  if (tab.kind === "gitDiff") return `${fileName(tab.path)} (git)`;
  return fileName(tab.path);
}

export default function EditorPane() {
  const tabs = useStore(selectCurrentEditorTabs);
  const activeTabId = useStore(selectCurrentActiveTab);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const closeTab = useStore((s) => s.closeTab);
  const toggleTabPin = useStore((s) => s.toggleTabPin);
  const setEditorSelectionRef = useStore((s) => s.setEditorSelectionRef);
  const editorTheme = useEditorTheme();

  const activeTab = useMemo(
    () => tabs.find((t) => t.id === activeTabId) ?? null,
    [tabs, activeTabId],
  );
  const activeFilePath = activeTab?.kind === "file" ? activeTab.path : null;

  // 各文件内容缓存：path → FileState。切 tab 不丢草稿。
  const [files, setFiles] = useState<Record<string, FileState>>({});
  // 处于 markdown 预览态的文件路径集合（默认源码态）。
  const [previewing, setPreviewing] = useState<Set<string>>(new Set());
  // diff tab 的左右分屏 / 行内布局（按 tab id 记忆）。
  const [diffInline, setDiffInline] = useState<Set<string>>(new Set());
  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);

  // 打开的所有文件 tab 路径（用换行连成稳定 key 触发 effect）。
  const openFilePaths = useMemo(
    () => tabs.filter((t) => t.kind === "file").map((t) => (t as { path: string }).path),
    [tabs],
  );
  const openPathsKey = openFilePaths.join("\n");

  // 打开新文件 tab 时拉内容（已有缓存不重拉）；关掉的文件清出缓存。
  useEffect(() => {
    const paths = openPathsKey ? openPathsKey.split("\n") : [];
    for (const path of paths) {
      if (files[path]) continue;
      setFiles((prev) => ({
        ...prev,
        [path]: { diskText: "", draft: "", loading: true, error: null },
      }));
      api
        .readTextFile(path)
        .then((text) => {
          setFiles((prev) => ({
            ...prev,
            [path]: { diskText: text, draft: text, loading: false, error: null },
          }));
        })
        .catch((e: any) => {
          setFiles((prev) => ({
            ...prev,
            [path]: { diskText: "", draft: "", loading: false, error: e?.message ?? String(e) },
          }));
        });
    }
    setFiles((prev) => {
      const next: Record<string, FileState> = {};
      for (const path of paths) if (prev[path]) next[path] = prev[path];
      return Object.keys(next).length === Object.keys(prev).length ? prev : next;
    });
  }, [openPathsKey]); // eslint-disable-line react-hooks/exhaustive-deps

  const activeFile = activeFilePath ? files[activeFilePath] : undefined;
  const dirty = activeFile ? activeFile.draft !== activeFile.diskText : false;
  const activePinned = activeTab?.pinned ?? false;
  const showPreview = activeFilePath ? previewing.has(activeFilePath) : false;

  const save = useCallback(async () => {
    const path = activeFilePath;
    if (!path) return;
    const state = files[path];
    if (!state || state.draft === state.diskText) return;
    try {
      await api.writeTextFile(path, state.draft);
      setFiles((prev) => ({ ...prev, [path]: { ...prev[path], diskText: state.draft } }));
      toast.success(`已保存 ${fileName(path)}`);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }, [activeFilePath, files]);

  // 用 ref 让 Monaco 的 onMount 里注册的快捷键 / 选区回调始终调到最新值。
  const saveRef = useRef(save);
  saveRef.current = save;
  const activePathRef = useRef(activeFilePath);
  activePathRef.current = activeFilePath;

  const handleMount = useCallback<OnMount>(
    (ed, monaco) => {
      editorRef.current = ed;
      ed.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
        void saveRef.current();
      });
      // 选区变化 → 实时写入 store：非空选区写 path:line(-endLine)，空选区清 null。
      ed.onDidChangeCursorSelection((e) => {
        const path = activePathRef.current;
        const sel = e.selection;
        if (!path || sel.isEmpty()) {
          setEditorSelectionRef(null);
          return;
        }
        const endLine =
          sel.endColumn === 1 && sel.endLineNumber > sel.startLineNumber
            ? sel.endLineNumber - 1
            : sel.endLineNumber;
        setEditorSelectionRef({ path, startLine: sel.startLineNumber, endLine });
      });
    },
    [setEditorSelectionRef],
  );

  // 切走文件 tab / 卸载时清掉残留的选区引用（旧选区对新 tab 无意义）。
  useEffect(() => {
    return () => setEditorSelectionRef(null);
  }, [activeFilePath, setEditorSelectionRef]);

  const onChange = useCallback(
    (value: string | undefined) => {
      const path = activeFilePath;
      if (!path) return;
      setFiles((prev) =>
        prev[path] ? { ...prev, [path]: { ...prev[path], draft: value ?? "" } } : prev,
      );
    },
    [activeFilePath],
  );

  const togglePreview = useCallback(() => {
    const path = activeFilePath;
    if (!path) return;
    setPreviewing((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, [activeFilePath]);

  const toggleDiffInline = useCallback((id: string) => {
    setDiffInline((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  if (tabs.length === 0 || !activeTab) return null;

  return (
    <div className="flex h-full min-w-0 flex-col border-l border-border bg-background">
      {/* 页签栏：file / diff / plan 三类混排 */}
      <div className="flex h-9 shrink-0 items-stretch overflow-x-auto border-b border-border bg-muted/40 [scrollbar-width:thin]">
        {tabs.map((tab) => {
          const isActive = tab.id === activeTabId;
          const isDirty =
            tab.kind === "file" && files[tab.path]
              ? files[tab.path].draft !== files[tab.path].diskText
              : false;
          return (
            <div
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              title={tab.kind === "plan" ? tab.title : tab.path}
              className={cn(
                "group/tab flex max-w-[220px] shrink-0 cursor-pointer items-center gap-1.5 border-r border-border px-3 text-[12px]",
                isActive ? "bg-background text-foreground" : "text-muted-foreground hover:bg-accent/40",
              )}
            >
              {tab.pinned ? (
                <Pin className="h-3 w-3 shrink-0 fill-current text-primary" />
              ) : (
                tabIcon(tab.kind)
              )}
              <span className="truncate">{tabLabel(tab)}</span>
              {isDirty ? (
                <Circle className="h-2 w-2 shrink-0 fill-current text-amber-500 group-hover/tab:hidden" />
              ) : null}
              <button
                type="button"
                title="关闭"
                aria-label="关闭"
                onClick={(e) => {
                  e.stopPropagation();
                  closeTab(tab.id);
                }}
                className={cn(
                  "grid h-4 w-4 shrink-0 place-items-center rounded hover:bg-accent",
                  isDirty ? "hidden group-hover/tab:grid" : "opacity-0 group-hover/tab:opacity-100",
                )}
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          );
        })}
      </div>

      {/* 工具栏：固定（所有 tab）+ markdown 预览（文件）+ diff 布局切换（diff） */}
      <div className="flex h-8 shrink-0 items-center gap-1 border-b border-border bg-muted/20 px-2">
        <ToolbarButton
          active={activePinned}
          title={activePinned ? "取消固定（切换对话后不再保留）" : "固定（切换对话后仍保留）"}
          onClick={() => toggleTabPin(activeTab.id)}
        >
          <Pin className={cn("h-3.5 w-3.5", activePinned && "fill-current")} />
          <span>{activePinned ? "已固定" : "固定"}</span>
        </ToolbarButton>
        {activeTab.kind === "file" && isMarkdown(activeTab.path) && (
          <ToolbarButton active={showPreview} title="源码 / 预览切换" onClick={togglePreview}>
            {showPreview ? <Code2 className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
            <span>{showPreview ? "源码" : "预览"}</span>
          </ToolbarButton>
        )}
        {(activeTab.kind === "diff" || activeTab.kind === "gitDiff") && (
          <ToolbarButton
            active={false}
            title="左右分屏 / 行内切换"
            onClick={() => toggleDiffInline(activeTab.id)}
          >
            {diffInline.has(activeTab.id) ? (
              <Columns2 className="h-3.5 w-3.5" />
            ) : (
              <Rows3 className="h-3.5 w-3.5" />
            )}
            <span>{diffInline.has(activeTab.id) ? "分屏" : "行内"}</span>
          </ToolbarButton>
        )}
      </div>

      {/* 正文：按 kind 分派 */}
      <div className="relative min-h-0 flex-1">
        {activeTab.kind === "file" && (
          <FileBody
            file={activeFile}
            path={activeTab.path}
            showPreview={showPreview}
            theme={editorTheme}
            onChange={onChange}
            onMount={handleMount}
          />
        )}
        {activeTab.kind === "diff" && (
          <DiffBody
            cacheKey={activeTab.id}
            path={activeTab.path}
            theme={editorTheme}
            inline={diffInline.has(activeTab.id)}
            fetcher={(sid) => fetchEditDiff(sid, activeTab.runId, activeTab.path)}
          />
        )}
        {activeTab.kind === "gitDiff" && (
          <DiffBody
            cacheKey={activeTab.id}
            path={activeTab.path}
            theme={editorTheme}
            inline={diffInline.has(activeTab.id)}
            fetcher={() => api.gitDiffFile(activeTab.root, activeTab.path, activeTab.staged)}
          />
        )}
        {activeTab.kind === "plan" && <PlanBody planId={activeTab.planId} />}
      </div>

      {/* 底栏：文件 / diff tab 显示文件路径 + 脏标记 */}
      {activeTab.kind !== "plan" && (
        <div className="flex h-6 shrink-0 items-center justify-between border-t border-border bg-muted/40 px-3 text-[11px] text-muted-foreground">
          <span className="truncate">{activeTab.path}</span>
          {activeTab.kind === "file" && dirty && (
            <span className="shrink-0 text-amber-500">● 未保存 · ⌘/Ctrl+S</span>
          )}
        </div>
      )}
    </div>
  );
}

/** 文件正文：Monaco 编辑器 / Markdown 预览 / 加载 / 错误。 */
function FileBody({
  file,
  path,
  showPreview,
  theme,
  onChange,
  onMount,
}: {
  file: FileState | undefined;
  path: string;
  showPreview: boolean;
  theme: "vs" | "vs-dark";
  onChange: (value: string | undefined) => void;
  onMount: OnMount;
}) {
  if (!file || file.loading) {
    return (
      <div className="grid h-full place-items-center text-[13px] text-muted-foreground">加载中…</div>
    );
  }
  if (file.error) {
    return (
      <div className="grid h-full place-items-center px-6 text-center text-[13px] text-destructive">
        {file.error}
      </div>
    );
  }
  if (showPreview) {
    return (
      <div className="h-full overflow-auto px-6 py-4">
        <MarkdownRenderer markdown={file.draft} className="markdown-body" />
      </div>
    );
  }
  return (
    <Editor
      key={path}
      height="100%"
      theme={theme}
      path={path}
      language={detectLanguage(path)}
      value={file.draft}
      onChange={onChange}
      onMount={onMount}
      options={{
        fontSize: VSCODE_FONT_SIZE,
        fontFamily: VSCODE_FONT_FAMILY,
        minimap: { enabled: false },
        scrollBeyondLastLine: false,
        automaticLayout: true,
        tabSize: 2,
        renderWhitespace: "selection",
        lineNumbersMinChars: 3,
      }}
    />
  );
}

/**
 * diff 正文：Monaco DiffEditor，左右分屏 / 行内，只读。
 * 数据源由 `fetcher` 注入——Run edits 走 diffCache，git diff 走 `api.gitDiffFile`。
 */
function DiffBody({
  cacheKey,
  path,
  theme,
  inline,
  fetcher,
}: {
  cacheKey: string;
  path: string;
  theme: "vs" | "vs-dark";
  inline: boolean;
  fetcher: (sessionId: string) => Promise<DiffPayload>;
}) {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const [payload, setPayload] = useState<DiffPayload | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    setPayload(null);
    setError(null);
    fetcher(sessionId)
      .then((p) => {
        if (!cancelled) setPayload(p);
      })
      .catch((e) => {
        if (!cancelled) setError(e?.message ?? String(e));
      });
    return () => {
      cancelled = true;
    };
    // fetcher 每渲染新建，但 cacheKey 唯一标识这份 diff，用它做依赖避免重复拉
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, cacheKey]);

  if (error) {
    return (
      <div className="grid h-full place-items-center px-6 text-center text-[13px] text-destructive">
        {error}
      </div>
    );
  }
  if (!payload) {
    return (
      <div className="grid h-full place-items-center text-[13px] text-muted-foreground">
        正在加载修改…
      </div>
    );
  }
  return (
    <DiffEditor
      height="100%"
      theme={theme}
      language={detectLanguage(path)}
      original={payload.before_text}
      modified={payload.after_text}
      // 卸载时让 @monaco-editor/react 不主动 dispose 它创建的 TextModel——
      // 否则 React 拆容器与 Monaco 内部异步 reset model 抢跑，关 tab 会抛
      // "TextModel got disposed before DiffEditorWidget model got reset"。
      keepCurrentOriginalModel
      keepCurrentModifiedModel
      options={{
        readOnly: true,
        renderSideBySide: !inline,
        fontSize: VSCODE_FONT_SIZE,
        fontFamily: VSCODE_FONT_FAMILY,
        minimap: { enabled: false },
        scrollBeyondLastLine: false,
        automaticLayout: true,
        lineNumbersMinChars: 3,
      }}
    />
  );
}

function ToolbarButton({
  active,
  title,
  onClick,
  children,
}: {
  active: boolean;
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      className={cn(
        "inline-flex h-6 items-center gap-1 rounded px-2 text-[12px] transition-colors",
        active
          ? "bg-primary/10 text-primary"
          : "text-muted-foreground hover:bg-accent hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

/**
 * plan 正文：markdown 预览 + 选区评论 + 评论列表 + 待审批操作条。
 *
 * **评论的去处**：评论落盘 `plans/<plan_id>.comments.jsonl`；下一轮 user message
 * 发送时 agent_core 把 unconsumed comments 拼到 SEMI 段，agent 据此改 plan。
 */
function PlanBody({ planId }: { planId: string }) {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const planComments = useStore((s) => s.planComments);
  const replaceComments = useStore((s) => s.replaceSessionPlanComments);
  const appendComment = useStore((s) => s.appendSessionPlanComment);

  const [planMd, setPlanMd] = useState<string>("");
  const [loadingMd, setLoadingMd] = useState(false);

  useEffect(() => {
    if (!sessionId || !planId) {
      setPlanMd("");
      return;
    }
    let cancelled = false;
    setLoadingMd(true);
    Promise.all([
      api.readPlanMarkdown(sessionId, planId),
      api.listPlanComments(sessionId, planId),
    ])
      .then(([md, cmts]) => {
        if (cancelled) return;
        setPlanMd(md);
        replaceComments(sessionId, planId, cmts);
      })
      .catch((e) => {
        if (!cancelled) toast.error(`读取 plan 失败：${e}`);
      })
      .finally(() => {
        if (!cancelled) setLoadingMd(false);
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, planId, replaceComments]);

  // 选区评论：监听 markdown 容器的 selectionchange
  const markdownContainerRef = useRef<HTMLDivElement | null>(null);
  const [selection, setSelection] = useState<{ text: string } | null>(null);

  useEffect(() => {
    function onSelect() {
      const sel = window.getSelection();
      if (!sel || sel.rangeCount === 0) {
        setSelection(null);
        return;
      }
      const range = sel.getRangeAt(0);
      const container = markdownContainerRef.current;
      if (!container) return;
      if (!container.contains(range.commonAncestorContainer)) {
        setSelection(null);
        return;
      }
      const text = sel.toString().trim();
      if (text.length < 3) {
        setSelection(null);
        return;
      }
      setSelection({ text });
    }
    document.addEventListener("selectionchange", onSelect);
    return () => document.removeEventListener("selectionchange", onSelect);
  }, []);

  const [showCommentBox, setShowCommentBox] = useState(false);
  const [commentBody, setCommentBody] = useState("");
  const [commentAnchor, setCommentAnchor] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const openCommentBox = (preset?: { anchor: string }) => {
    setCommentAnchor(preset?.anchor ?? "");
    setCommentBody("");
    setShowCommentBox(true);
  };
  const submitComment = async () => {
    if (!sessionId || !planId) return;
    const body = commentBody.trim();
    if (!body) {
      toast.error("评论内容不能为空");
      return;
    }
    const anchor = commentAnchor.trim() || "(global)";
    setSubmitting(true);
    try {
      const saved = await api.addPlanComment(sessionId, planId, anchor, body);
      appendComment(sessionId, planId, saved);
      setShowCommentBox(false);
      setCommentBody("");
      setCommentAnchor("");
    } catch (e) {
      toast.error(`添加评论失败：${e}`);
    } finally {
      setSubmitting(false);
    }
  };

  if (!sessionId) {
    return <div className="p-4 text-sm text-muted-foreground">打开一个对话再查看 plan。</div>;
  }

  const comments = planId ? planComments[planId] ?? [] : [];
  const unconsumed = comments.filter((c) => !c.consumed);

  return (
    <div className="flex h-full flex-col">
      {/* 选区浮动操作条 */}
      {selection && (
        <div className="shrink-0 border-b border-border bg-amber-500/10 px-3 py-1.5 text-xs">
          <button
            type="button"
            onClick={() =>
              openCommentBox({ anchor: selection.text.slice(0, 40).replace(/\s+/g, " ") })
            }
            className="inline-flex items-center gap-1 rounded bg-amber-500 px-2 py-1 text-white"
          >
            <MessageSquarePlus className="h-3 w-3" /> 给选中段加评论
          </button>
          <span className="ml-2 text-muted-foreground">"{selection.text.slice(0, 30)}…"</span>
        </div>
      )}

      {/* 主区：markdown */}
      <div ref={markdownContainerRef} className="min-h-0 flex-1 overflow-auto px-5 py-4">
        {loadingMd ? (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" /> 读取中…
          </div>
        ) : (
          <MarkdownRenderer
            markdown={planMd}
            className="prose prose-sm max-w-none dark:prose-invert"
          />
        )}
      </div>

      {/* plan 待审批操作条（架构 §4.4.5） */}
      <PlanApprovalBar planId={planId} />

      {/* 评论区 */}
      <div className="shrink-0 border-t border-border bg-muted/30">
        <div className="flex items-center justify-between px-3 py-1.5 text-xs">
          <span className="flex items-center gap-1 text-muted-foreground">
            <History className="h-3 w-3" />
            评论 {comments.length}
            {unconsumed.length > 0 && (
              <span className="text-amber-600">（{unconsumed.length} 条待发送）</span>
            )}
          </span>
          <button
            type="button"
            onClick={() => openCommentBox()}
            className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <MessageSquarePlus className="h-3 w-3" /> 添加
          </button>
        </div>
        <ul className="max-h-40 divide-y divide-border overflow-auto">
          {comments.map((c) => (
            <CommentRow key={c.id} comment={c} />
          ))}
        </ul>
        {showCommentBox && (
          <div className="border-t border-border px-3 py-2">
            <input
              value={commentAnchor}
              onChange={(e) => setCommentAnchor(e.target.value)}
              placeholder="锚点（可选，例如 L12-15 或 选段头部 30 字）"
              className="mb-1 w-full rounded border border-border bg-background px-2 py-1 text-xs"
            />
            <textarea
              value={commentBody}
              onChange={(e) => setCommentBody(e.target.value)}
              placeholder="评论内容"
              rows={3}
              className="w-full rounded border border-border bg-background px-2 py-1 text-xs"
            />
            <div className="mt-1 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setShowCommentBox(false)}
                className="rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent"
              >
                取消
              </button>
              <button
                type="button"
                onClick={submitComment}
                disabled={submitting}
                className="rounded bg-primary px-2 py-1 text-xs text-primary-foreground disabled:opacity-50"
              >
                {submitting ? "提交中…" : "提交"}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function CommentRow({ comment }: { comment: PlanComment }) {
  return (
    <li
      className={cn(
        "px-3 py-1.5 text-xs",
        comment.consumed ? "text-muted-foreground" : "text-foreground",
      )}
    >
      <div className="flex items-center gap-2">
        <span className="rounded bg-muted px-1 py-0.5 text-[10px] text-muted-foreground">
          {comment.anchor}
        </span>
        <span className="text-[10px] text-muted-foreground">
          {new Date(comment.created_at_ms).toLocaleString()}
        </span>
        {!comment.consumed && <span className="text-[10px] text-amber-600">待发送</span>}
      </div>
      <p className="mt-0.5 whitespace-pre-wrap">{comment.body}</p>
    </li>
  );
}

/**
 * plan 待审批操作条（架构 §4.4.5）。
 *
 * 与普通 tool_call 审批共用底层 HITL 通路（`resolveApproval`），展示位置在编辑区 plan
 * tab 底部：通过 / 重新规划（带反馈）/ 拒绝。AutoMode 下挂 10s 自动通过倒计时；用户进入
 * 反馈或点任一按钮即取消倒计时。
 */
function PlanApprovalBar({ planId }: { planId: string }) {
  const pending = useStore((s) => s.pendingApproval);
  const resolveApproval = useStore((s) => s.resolveApproval);
  const currentRunMode = useStore((s) => s.currentRunMode);

  const isAuto = currentRunMode === "AutoMode" || currentRunMode === "auto";
  const planInfo = pending?.kind === "plan" ? pending.plan ?? null : null;

  const [feedbackMode, setFeedbackMode] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [remaining, setRemaining] = useState<number | null>(null);
  const [autoCancelled, setAutoCancelled] = useState(false);

  useEffect(() => {
    setFeedbackMode(false);
    setFeedback("");
    setAutoCancelled(false);
    setRemaining(planInfo && isAuto ? 10 : null);
  }, [planInfo?.plan_id, isAuto]);

  const approve = useCallback(async () => {
    setSubmitting(true);
    try {
      await resolveApproval({ kind: "allow_once" });
    } catch (e: any) {
      toast.error(e?.message ?? "审批失败");
    } finally {
      setSubmitting(false);
    }
  }, [resolveApproval]);

  useEffect(() => {
    if (remaining === null || autoCancelled || feedbackMode) return;
    if (remaining <= 0) {
      void approve();
      return;
    }
    const t = setTimeout(() => setRemaining((r) => (r === null ? null : r - 1)), 1000);
    return () => clearTimeout(t);
  }, [remaining, autoCancelled, feedbackMode, approve]);

  if (!planInfo) return null;

  const reject = async () => {
    setSubmitting(true);
    try {
      await resolveApproval({ kind: "deny" });
    } catch (e: any) {
      toast.error(e?.message ?? "审批失败");
    } finally {
      setSubmitting(false);
    }
  };

  const rejectWithFeedback = async () => {
    if (!feedback.trim()) {
      toast.error("请描述要修改的点");
      return;
    }
    setSubmitting(true);
    try {
      await resolveApproval({ kind: "deny_with_feedback", feedback: feedback.trim() });
    } catch (e: any) {
      toast.error(e?.message ?? "审批失败");
    } finally {
      setSubmitting(false);
    }
  };

  const cancelCountdown = () => {
    setAutoCancelled(true);
    setRemaining(null);
  };

  // 用户正看着别的历史 plan，提示一下待审批的是哪份
  const viewingOther = planId !== planInfo.plan_id;

  return (
    <div className="shrink-0 border-t border-amber-400/50 bg-amber-500/10">
      <div className="flex items-center gap-2 px-3 py-1.5 text-xs text-amber-700 dark:text-amber-300">
        <span className="font-medium">AI 提交了一份计划，等你审批</span>
        {planInfo.summary && (
          <span className="truncate text-[11px] opacity-80">{planInfo.summary}</span>
        )}
      </div>
      {viewingOther && (
        <div className="px-3 pb-1 text-[11px] text-muted-foreground">
          你正在看另一份计划，待审批的是「{planInfo.summary || "最新计划"}」。
        </div>
      )}
      {feedbackMode ? (
        <div className="px-3 pb-2">
          <textarea
            value={feedback}
            onChange={(e) => setFeedback(e.target.value)}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                e.preventDefault();
                if (!submitting && feedback.trim()) void rejectWithFeedback();
              }
            }}
            placeholder="告诉 AI 想怎么改这份计划（⌘/Ctrl+Enter 提交，会作为下一轮消息发给 AI）"
            rows={3}
            className="w-full rounded border border-border bg-background px-2 py-1 text-xs"
          />
          <div className="mt-1 flex justify-end gap-2">
            <button
              type="button"
              onClick={() => {
                setFeedbackMode(false);
                setFeedback("");
              }}
              disabled={submitting}
              className="rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent disabled:opacity-50"
            >
              取消
            </button>
            <button
              type="button"
              onClick={rejectWithFeedback}
              disabled={submitting || !feedback.trim()}
              className="rounded bg-destructive px-2 py-1 text-xs font-medium text-destructive-foreground hover:bg-destructive/90 disabled:opacity-50"
            >
              提交反馈让 AI 重做
            </button>
          </div>
        </div>
      ) : (
        <div className="flex items-center gap-1.5 px-3 pb-2">
          <button
            type="button"
            onClick={approve}
            disabled={submitting}
            className="inline-flex h-7 items-center gap-1 rounded-md bg-primary px-2.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            <Check className="h-3.5 w-3.5" />
            通过，开干
            {remaining !== null && !autoCancelled && (
              <span className="ml-0.5 text-[10px] opacity-80">({remaining}s)</span>
            )}
          </button>
          <div className="flex-1" />
          <button
            type="button"
            onClick={() => {
              cancelCountdown();
              setFeedbackMode(true);
            }}
            disabled={submitting}
            className="inline-flex h-7 items-center gap-1 rounded-md px-2.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50"
          >
            <MessageSquareWarning className="h-3.5 w-3.5" />
            重新规划
          </button>
          <button
            type="button"
            onClick={() => {
              cancelCountdown();
              void reject();
            }}
            disabled={submitting}
            className="inline-flex h-7 items-center gap-1 rounded-md bg-destructive/10 px-2.5 text-xs font-medium text-destructive hover:bg-destructive/20 disabled:opacity-50"
          >
            <X className="h-3.5 w-3.5" />
            拒绝
          </button>
        </div>
      )}
    </div>
  );
}
