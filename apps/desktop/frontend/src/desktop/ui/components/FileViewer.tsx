import { useCallback, useEffect, useRef, useState } from "react";
import Editor, { type OnMount } from "@monaco-editor/react";
import type { editor } from "monaco-editor";
import { toast } from "sonner";
import { X, Circle, Pin, Eye, Code2 } from "lucide-react";
import {
  useStore,
  selectCurrentOpenFiles,
  selectCurrentActiveFile,
} from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { detectLanguage, fileName } from "@/desktop/ui/lib/fileLanguage";
import { MarkdownRenderer } from "@/desktop/ui/components/MarkdownRenderer";
import "@/desktop/ui/lib/monacoSetup";

/**
 * 文件查看器（中间列）：多 tab 文件页签 + Monaco 编辑器。
 *
 * - 文件按对话隔离（store per-session）；每个文件一个 tab，× 关闭、📌 固定、脏标记小圆点
 * - Markdown 文件可在「源码 ↔ 预览」间切换（预览复用 chat 同款 MarkdownRenderer）
 * - 选中编辑器里的文本 → 实时写入 store 的 editorSelectionRef，输入框上方出现一条
 *   `path:line` 引用；取消选中即清除（ChatInput 订阅渲染）
 * - Monaco 提供语法高亮，为以后接 LSP/autocomplete 留口
 * - Ctrl/Cmd+S 把当前文件写回磁盘（`api.writeTextFile`），仅覆盖已存在文件
 * - 列宽由 DesktopShell 控制（可拖、不持久化）；本组件只负责内容
 *
 * 整个组件在 DesktopShell 里懒加载（React.lazy），Monaco 不进主 bundle。
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

export default function FileViewer() {
  const openFiles = useStore(selectCurrentOpenFiles);
  const activeFilePath = useStore(selectCurrentActiveFile);
  const setActiveFile = useStore((s) => s.setActiveFile);
  const closeFile = useStore((s) => s.closeFile);
  const toggleFilePin = useStore((s) => s.toggleFilePin);
  const setEditorSelectionRef = useStore((s) => s.setEditorSelectionRef);
  const editorTheme = useEditorTheme();

  // 各文件内容缓存：path → FileState。切 tab 不丢草稿。
  const [files, setFiles] = useState<Record<string, FileState>>({});
  // 处于 markdown 预览态的文件路径集合（默认源码态）。
  const [previewing, setPreviewing] = useState<Set<string>>(new Set());
  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);

  const openPaths = openFiles.map((e) => e.path).join("\n");

  // 打开新文件时拉内容（已有缓存不重拉）。
  useEffect(() => {
    const paths = openPaths ? openPaths.split("\n") : [];
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
    // 关掉的文件清出缓存
    setFiles((prev) => {
      const next: Record<string, FileState> = {};
      for (const path of paths) if (prev[path]) next[path] = prev[path];
      return Object.keys(next).length === Object.keys(prev).length ? prev : next;
    });
  }, [openPaths]); // eslint-disable-line react-hooks/exhaustive-deps

  const active = activeFilePath ? files[activeFilePath] : undefined;
  const dirty = active ? active.draft !== active.diskText : false;
  const activePinned = openFiles.find((e) => e.path === activeFilePath)?.pinned ?? false;
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
        // 选区终点落在某行行首时不把该行算进去（更符合直觉）。
        const endLine =
          sel.endColumn === 1 && sel.endLineNumber > sel.startLineNumber
            ? sel.endLineNumber - 1
            : sel.endLineNumber;
        setEditorSelectionRef({ path, startLine: sel.startLineNumber, endLine });
      });
    },
    [setEditorSelectionRef],
  );

  // 切走当前文件 / 卸载时清掉残留的选区引用（旧选区对新文件无意义）。
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

  if (openFiles.length === 0 || !activeFilePath) return null;

  return (
    <div className="flex h-full min-w-0 flex-col border-l border-border bg-background">
      {/* 文件 tab 页签栏 */}
      <div className="flex h-9 shrink-0 items-stretch overflow-x-auto border-b border-border bg-muted/40 [scrollbar-width:thin]">
        {openFiles.map((entry) => {
          const path = entry.path;
          const isActive = path === activeFilePath;
          const isDirty = files[path] ? files[path].draft !== files[path].diskText : false;
          return (
            <div
              key={path}
              onClick={() => setActiveFile(path)}
              title={path}
              className={cn(
                "group/tab flex max-w-[200px] shrink-0 cursor-pointer items-center gap-1.5 border-r border-border px-3 text-[12px]",
                isActive
                  ? "bg-background text-foreground"
                  : "text-muted-foreground hover:bg-accent/40",
              )}
            >
              {entry.pinned && <Pin className="h-3 w-3 shrink-0 fill-current text-primary" />}
              <span className="truncate">{fileName(path)}</span>
              {isDirty ? (
                <Circle className="h-2 w-2 shrink-0 fill-current text-amber-500 group-hover/tab:hidden" />
              ) : null}
              <button
                type="button"
                title="关闭"
                aria-label="关闭"
                onClick={(e) => {
                  e.stopPropagation();
                  closeFile(path);
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

      {/* 工具栏：固定 / markdown 预览切换 */}
      <div className="flex h-8 shrink-0 items-center gap-1 border-b border-border bg-muted/20 px-2">
        <ToolbarButton
          active={activePinned}
          title={activePinned ? "取消固定（切换对话后不再保留）" : "固定（切换对话后仍保留）"}
          onClick={() => toggleFilePin(activeFilePath)}
        >
          <Pin className={cn("h-3.5 w-3.5", activePinned && "fill-current")} />
          <span>{activePinned ? "已固定" : "固定"}</span>
        </ToolbarButton>
        {isMarkdown(activeFilePath) && (
          <ToolbarButton active={showPreview} title="源码 / 预览切换" onClick={togglePreview}>
            {showPreview ? <Code2 className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
            <span>{showPreview ? "源码" : "预览"}</span>
          </ToolbarButton>
        )}
      </div>

      {/* 编辑器 / 预览 / 状态 */}
      <div className="relative min-h-0 flex-1">
        {active?.loading && (
          <div className="grid h-full place-items-center text-[13px] text-muted-foreground">
            加载中…
          </div>
        )}
        {active?.error && (
          <div className="grid h-full place-items-center px-6 text-center text-[13px] text-destructive">
            {active.error}
          </div>
        )}
        {active && !active.loading && !active.error && showPreview && (
          <div className="h-full overflow-auto px-6 py-4">
            <MarkdownRenderer markdown={active.draft} className="markdown-body" />
          </div>
        )}
        {active && !active.loading && !active.error && !showPreview && (
          <Editor
            key={activeFilePath}
            height="100%"
            theme={editorTheme}
            path={activeFilePath}
            language={detectLanguage(activeFilePath)}
            value={active.draft}
            onChange={onChange}
            onMount={handleMount}
            options={{
              fontSize: 13,
              minimap: { enabled: false },
              scrollBeyondLastLine: false,
              automaticLayout: true,
              tabSize: 2,
              renderWhitespace: "selection",
              // 行号列宽减半（默认 5 字符 → 3），少占左侧空间给正文
              lineNumbersMinChars: 3,
            }}
          />
        )}
      </div>

      {/* 底栏：路径 + 脏标记 + 保存提示 */}
      <div className="flex h-6 shrink-0 items-center justify-between border-t border-border bg-muted/40 px-3 text-[11px] text-muted-foreground">
        <span className="truncate">{activeFilePath}</span>
        {dirty && <span className="shrink-0 text-amber-500">● 未保存 · ⌘/Ctrl+S</span>}
      </div>
    </div>
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
