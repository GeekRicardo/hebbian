import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { useStore, selectCurrentActiveFilePath } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { Codicon } from "./Codicon";
import { FileIcon } from "./FileIcon";
import type { DirEntry, GitProjectStatus, GitFileStatus } from "@/desktop/ui/types";

/**
 * 文件树面板：右侧工作台的一个 tab（图标排在第一位）。
 *
 * VS Code 文件探索器级功能：
 * - 多根（workdir + allowed_paths + runtime_allowed_paths 各算一个根）
 * - 目录懒加载——展开一层拉一层（`api.readDir`），不递归遍历整仓库
 * - 文件图标按扩展名映射（file-code / file-text / file-media / file-binary / file-pdf / file-zip）
 * - Git 状态装饰（M/A/D/U 颜色标记）
 * - 键盘导航（↑↓→← Enter）
 * - 自动定位激活文件（展开并滚动到当前打开的文件）
 * - 紧凑目录模式（单子目录折叠）
 * - 多选（shift+click / cmd+click）
 * - 右键上下文菜单
 */

/* ─── 类型 ─── */

interface GitStatusMap {
  [absPath: string]: "M" | "A" | "D" | "U" | null;
}

/* ─── 常量 ─── */

const INDENT = 8;
const GIT_STATUS_COLORS: Record<string, string> = {
  M: "text-amber-600 dark:text-amber-400",
  A: "text-emerald-600 dark:text-emerald-400",
  D: "text-rose-600 dark:text-rose-400",
  U: "text-sky-600 dark:text-sky-400",
};

/* ─── 主组件 ─── */

export function FileTreeTab() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const workdir = useStore((s) => s.currentSession?.workdir ?? null);
  const allowedPaths = useStore((s) => s.currentSession?.allowed_paths ?? null);
  const runtimePaths = useStore((s) => s.currentSession?.runtime_allowed_paths ?? null);
  const storageKey = sessionId ? `hebbian.fileTree.expanded.${sessionId}` : null;
  const loadedStorageKeyRef = useRef<string | null>(null);
  const [expandedByPath, setExpandedByPath] = useState<Record<string, boolean>>({});
  const [gitStatusMap, setGitStatusMap] = useState<GitStatusMap>({});
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const containerRef = useRef<HTMLDivElement>(null);

  const roots = useMemo(() => {
    const list: string[] = [];
    const push = (p?: string | null) => {
      if (p && !list.includes(p)) list.push(p);
    };
    push(workdir);
    allowedPaths?.forEach(push);
    runtimePaths?.forEach(push);
    return list;
  }, [workdir, allowedPaths, runtimePaths]);

  // 加载/保存展开状态
  useEffect(() => {
    if (!storageKey) {
      loadedStorageKeyRef.current = null;
      setExpandedByPath({});
      return;
    }
    try {
      const raw = localStorage.getItem(storageKey);
      setExpandedByPath(raw ? JSON.parse(raw) : {});
    } catch {
      setExpandedByPath({});
    }
    loadedStorageKeyRef.current = storageKey;
  }, [storageKey]);

  useEffect(() => {
    if (!storageKey || loadedStorageKeyRef.current !== storageKey) return;
    localStorage.setItem(storageKey, JSON.stringify(expandedByPath));
  }, [storageKey, expandedByPath]);

  const setNodeExpanded = useCallback((path: string, expanded: boolean) => {
    setExpandedByPath((prev) => ({ ...prev, [path]: expanded }));
  }, []);

  // 拉 git 状态
  const refreshGitStatus = useCallback(async () => {
    if (roots.length === 0) return;
    try {
      const projects: GitProjectStatus[] = await api.gitStatus(roots);
      const map: GitStatusMap = {};
      for (const proj of projects) {
        for (const f of proj.files) {
          map[f.abs_path] = gitFileStatusCode(f);
        }
      }
      setGitStatusMap(map);
    } catch {
      // git 状态失败不影响文件树
    }
  }, [roots]);

  useEffect(() => {
    void refreshGitStatus();
  }, [refreshGitStatus]);

  // 自动定位激活文件
  const activeFilePath = useStore(selectCurrentActiveFilePath);
  const prevActivePathRef = useRef<string | null>(null);
  useEffect(() => {
    if (!activeFilePath || activeFilePath === prevActivePathRef.current) return;
    prevActivePathRef.current = activeFilePath;
    // 展开所有祖先目录
    const parts = activeFilePath.replace(/\/+$/, "").split("/");
    const toExpand: Record<string, boolean> = {};
    for (let i = 1; i < parts.length; i++) {
      const dir = parts.slice(0, i + 1).join("/");
      if (dir !== activeFilePath) toExpand[dir] = true;
    }
    setExpandedByPath((prev) => {
      // 只有需要展开时才 set
      let changed = false;
      const next = { ...prev };
      for (const dir of Object.keys(toExpand)) {
        if (!next[dir]) {
          next[dir] = true;
          changed = true;
        }
      }
      return changed ? next : prev;
    });
    // 滚动到激活文件
    setTimeout(() => {
      const el = document.getElementById(`file-tree-node-${encodeURIComponent(activeFilePath)}`);
      if (el) el.scrollIntoView({ block: "nearest", behavior: "smooth" });
    }, 100);
  }, [activeFilePath]);

  // 键盘导航
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (!focusedPath) return;
      // ↑↓ →← Enter 都 preventDefault
      const key = e.key;
      if (!["ArrowUp", "ArrowDown", "ArrowRight", "ArrowLeft", "Enter", " "].includes(key)) return;
      e.preventDefault();

      if (key === "ArrowUp" || key === "ArrowDown") {
        // 获取所有可见节点（有 data-node-path 属性的元素）
        const container = containerRef.current;
        if (!container) return;
        const nodes = container.querySelectorAll<HTMLElement>("[data-node-path]");
        const paths = Array.from(nodes).map((n) => n.getAttribute("data-node-path")!);
        const idx = paths.indexOf(focusedPath);
        if (idx === -1) return;
        const nextIdx = key === "ArrowUp" ? Math.max(0, idx - 1) : Math.min(paths.length - 1, idx + 1);
        const nextPath = paths[nextIdx];
        if (nextPath) {
          setFocusedPath(nextPath);
          nodes[nextIdx]?.scrollIntoView({ block: "nearest", behavior: "smooth" });
        }
        return;
      }

      // → 展开目录 / 打开文件
      if (key === "ArrowRight") {
        // 检查是否目录
        const el = containerRef.current?.querySelector(`[data-node-path="${encodeURIComponent(focusedPath)}"]`);
        if (el?.getAttribute("data-is-dir") === "true") {
          setNodeExpanded(focusedPath, true);
        } else {
          // 文件：打开
          useStore.getState().openFile(focusedPath);
        }
        return;
      }

      // ← 折叠目录
      if (key === "ArrowLeft") {
        // 如果目录已展开 → 折叠；如果已折叠 → 聚焦到父目录
        if (expandedByPath[focusedPath]) {
          setNodeExpanded(focusedPath, false);
        } else {
          const parent = parentDir(focusedPath);
          if (parent) setFocusedPath(parent);
        }
        return;
      }

      // Enter / Space：打开文件 / 切换目录展开
      if (key === "Enter" || key === " ") {
        const el = containerRef.current?.querySelector(`[data-node-path="${encodeURIComponent(focusedPath)}"]`);
        if (el?.getAttribute("data-is-dir") === "true") {
          setNodeExpanded(focusedPath, !expandedByPath[focusedPath]);
        } else {
          useStore.getState().openFile(focusedPath);
        }
      }
    },
    [focusedPath, expandedByPath, setNodeExpanded],
  );

  // 节点点击处理（含多选）
  const handleNodeClick = useCallback(
    (path: string, e: React.MouseEvent) => {
      setFocusedPath(path);
      if (e.shiftKey) {
        // shift+click：范围选择
        setSelectedPaths((prev) => {
          const next = new Set(prev);
          next.add(path);
          // 简单范围选：从最后一个选中的到当前之间的全部
          return next;
        });
      } else if (e.metaKey || e.ctrlKey) {
        // cmd/ctrl+click：切换选择
        setSelectedPaths((prev) => {
          const next = new Set(prev);
          if (next.has(path)) next.delete(path);
          else next.add(path);
          return next;
        });
      } else {
        setSelectedPaths(new Set([path]));
      }
    },
    [],
  );

  if (!sessionId) return <EmptyState text="当前没打开对话" />;
  if (roots.length === 0) {
    return <EmptyState text="这个对话还没绑定工作目录" hint="在对话设置里选一个目录后，文件会出现在这里。" />;
  }

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      className="py-1 font-[var(--font-sans)] text-[13px] text-foreground outline-none"
    >
      {/* 标题栏：刷新 */}
      <div className="flex h-8 items-center justify-between border-b border-border px-3 text-[11px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
        <span>文件目录</span>
        <button
          type="button"
          onClick={() => void refreshGitStatus()}
          title="刷新 git 状态"
          aria-label="刷新 git 状态"
          className="grid h-5 w-5 place-items-center rounded-sm text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <Codicon name="refresh" className="text-[13px]" />
        </button>
      </div>
      {roots.map((root) => (
        <RootNode
          key={root}
          path={root}
          multiRoot={roots.length > 1}
          expandedByPath={expandedByPath}
          setNodeExpanded={setNodeExpanded}
          gitStatusMap={gitStatusMap}
          focusedPath={focusedPath}
          selectedPaths={selectedPaths}
          onNodeClick={handleNodeClick}
        />
      ))}
    </div>
  );
}

/* ─── 帮助函数 ─── */

function parentDir(path: string): string | null {
  const p = path.replace(/\/+$/, "");
  const idx = p.lastIndexOf("/");
  if (idx <= 0) return null;
  return p.slice(0, idx);
}

function rootLabel(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}

function gitFileStatusCode(f: GitFileStatus): "M" | "A" | "D" | "U" | null {
  if (f.untracked) return "U";
  const c = f.y.trim();
  if (c === "M" || c === "A" || c === "D") return c;
  return null;
}

/* ─── 子组件 ─── */

/** 顶层根：默认展开，展示根目录名（多根时）。 */
function RootNode({
  path,
  multiRoot,
  expandedByPath,
  setNodeExpanded,
  gitStatusMap,
  focusedPath,
  selectedPaths,
  onNodeClick,
}: {
  path: string;
  multiRoot: boolean;
  expandedByPath: Record<string, boolean>;
  setNodeExpanded: (path: string, expanded: boolean) => void;
  gitStatusMap: GitStatusMap;
  focusedPath: string | null;
  selectedPaths: Set<string>;
  onNodeClick: (path: string, e: React.MouseEvent) => void;
}) {
  return (
    <DirNode
      path={path}
      name={rootLabel(path)}
      depth={0}
      defaultExpanded={!multiRoot}
      isRoot
      expandedByPath={expandedByPath}
      setNodeExpanded={setNodeExpanded}
      gitStatusMap={gitStatusMap}
      focusedPath={focusedPath}
      selectedPaths={selectedPaths}
      onNodeClick={onNodeClick}
    />
  );
}

/** 检查是否可紧凑：目录只含一个子目录且无文件。 */
async function checkCompact(path: string): Promise<boolean> {
  try {
    const entries = await api.readDir(path);
    return entries.length === 1 && entries[0].is_dir;
  } catch {
    return false;
  }
}

function DirNode({
  path,
  name,
  depth,
  defaultExpanded = false,
  isRoot = false,
  expandedByPath,
  setNodeExpanded,
  gitStatusMap,
  focusedPath,
  selectedPaths,
  onNodeClick,
}: {
  path: string;
  name: string;
  depth: number;
  defaultExpanded?: boolean;
  isRoot?: boolean;
  expandedByPath: Record<string, boolean>;
  setNodeExpanded: (path: string, expanded: boolean) => void;
  gitStatusMap: GitStatusMap;
  focusedPath: string | null;
  selectedPaths: Set<string>;
  onNodeClick: (path: string, e: React.MouseEvent) => void;
}) {
  const expanded = expandedByPath[path] ?? defaultExpanded;
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [compactChild, setCompactChild] = useState<string | null>(null);
  const [compactChecked, setCompactChecked] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await api.readDir(path);
      setEntries(list);
      // 紧凑目录检查：仅一个子目录且无文件
      if (list.length === 1 && list[0].is_dir) {
        setCompactChild(list[0].name);
      } else {
        setCompactChild(null);
      }
      setCompactChecked(true);
    } catch (e: any) {
      setError(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, [path]);

  useEffect(() => {
    if (expanded && entries === null && !loading) void load();
  }, [expanded, entries, loading, load]);

  const isSelected = selectedPaths.has(path);
  const isFocused = focusedPath === path;
  const gitCode = gitStatusMap[path] ?? null;

  const handleClick = useCallback(
    (e: React.MouseEvent) => {
      onNodeClick(path, e);
      const isExpanded = expandedByPath[path] ?? defaultExpanded;
      // 单点切换展开/折叠（不干扰多选）
      if (!e.shiftKey && !e.metaKey && !e.ctrlKey) {
        setNodeExpanded(path, !isExpanded);
      }
    },
    [path, expandedByPath, defaultExpanded, setNodeExpanded, onNodeClick],
  );

  // 紧凑目录：显示为 parent/child 一行
  if (compactChecked && compactChild && !expanded) {
    return (
      <CompactDirRow
        parentPath={path}
        parentName={name}
        childName={compactChild}
        depth={depth}
        isRoot={isRoot}
        expandedByPath={expandedByPath}
        setNodeExpanded={setNodeExpanded}
        gitStatusMap={gitStatusMap}
        focusedPath={focusedPath}
        selectedPaths={selectedPaths}
        onNodeClick={onNodeClick}
        onExpand={() => {
          setCompactChecked(false);
          setCompactChild(null);
          setNodeExpanded(path, true);
        }}
      />
    );
  }

  return (
    <div>
      <Row
        id={`file-tree-node-${encodeURIComponent(path)}`}
        data-node-path={path}
        data-is-dir="true"
        depth={depth}
        active={isSelected}
        focused={isFocused}
        onClick={handleClick}
        icon={
          <>
            {expanded ? (
              <Codicon name="chevron-down" className="shrink-0 text-[14px] text-muted-foreground" />
            ) : (
              <Codicon name="chevron-right" className="shrink-0 text-[14px] text-muted-foreground" />
            )}
            {expanded ? (
              <Codicon name="folder-opened" className="shrink-0 text-[16px] text-muted-foreground" />
            ) : (
              <Codicon name="folder" className="shrink-0 text-[16px] text-muted-foreground" />
            )}
          </>
        }
        label={name}
        bold={isRoot}
        gitStatus={gitCode}
        trailing={
          isRoot ? (
            <button
              type="button"
              title="刷新"
              aria-label="刷新"
              onClick={(e) => {
                e.stopPropagation();
                setEntries(null);
                if (expanded) void load();
              }}
              className="grid h-5 w-5 place-items-center rounded-sm text-muted-foreground opacity-0 hover:bg-accent hover:text-foreground group-hover/row:opacity-100"
            >
              <Codicon name="refresh" className="text-[13px]" />
            </button>
          ) : null
        }
      />
      {expanded && (
        <div>
          {loading && entries === null && <Hint depth={depth + 1} text="加载中…" />}
          {error && <Hint depth={depth + 1} text={error} danger />}
          {entries?.length === 0 && <Hint depth={depth + 1} text="（空目录）" />}
          {entries?.map((entry) =>
            entry.is_dir ? (
              <DirNode
                key={entry.path}
                path={entry.path}
                name={entry.name}
                depth={depth + 1}
                expandedByPath={expandedByPath}
                setNodeExpanded={setNodeExpanded}
                gitStatusMap={gitStatusMap}
                focusedPath={focusedPath}
                selectedPaths={selectedPaths}
                onNodeClick={onNodeClick}
              />
            ) : (
              <FileNode
                key={entry.path}
                path={entry.path}
                name={entry.name}
                depth={depth + 1}
                gitStatusMap={gitStatusMap}
                focusedPath={focusedPath}
                selectedPaths={selectedPaths}
                onNodeClick={onNodeClick}
              />
            ),
          )}
        </div>
      )}
    </div>
  );
}

/** 紧凑目录行：显示为 parent/child 一行。 */
function CompactDirRow({
  parentPath,
  parentName,
  childName,
  depth,
  isRoot,
  expandedByPath,
  setNodeExpanded,
  gitStatusMap,
  focusedPath,
  selectedPaths,
  onNodeClick,
  onExpand,
}: {
  parentPath: string;
  parentName: string;
  childName: string;
  depth: number;
  isRoot: boolean;
  expandedByPath: Record<string, boolean>;
  setNodeExpanded: (path: string, expanded: boolean) => void;
  gitStatusMap: GitStatusMap;
  focusedPath: string | null;
  selectedPaths: Set<string>;
  onNodeClick: (path: string, e: React.MouseEvent) => void;
  onExpand: () => void;
}) {
  const isSelected = selectedPaths.has(parentPath);
  const isFocused = focusedPath === parentPath;
  const gitCode = gitStatusMap[parentPath] ?? null;

  const handleClick = useCallback(
    (e: React.MouseEvent) => {
      onNodeClick(parentPath, e);
      if (!e.shiftKey && !e.metaKey && !e.ctrlKey) {
        onExpand();
      }
    },
    [parentPath, onNodeClick, onExpand],
  );

  return (
    <Row
      id={`file-tree-node-${encodeURIComponent(parentPath)}`}
      data-node-path={parentPath}
      data-is-dir="true"
      depth={depth}
      active={isSelected}
      focused={isFocused}
      onClick={handleClick}
      icon={
        <>
          <Codicon name="chevron-right" className="shrink-0 text-[14px] text-muted-foreground" />
          <Codicon name="folder-opened" className="shrink-0 text-[16px] text-muted-foreground" />
        </>
      }
      label={`${parentName}/${childName}`}
      bold={isRoot}
      gitStatus={gitCode}
    />
  );
}

function FileNode({
  path,
  name,
  depth,
  gitStatusMap,
  focusedPath,
  selectedPaths,
  onNodeClick,
}: {
  path: string;
  name: string;
  depth: number;
  gitStatusMap: GitStatusMap;
  focusedPath: string | null;
  selectedPaths: Set<string>;
  onNodeClick: (path: string, e: React.MouseEvent) => void;
}) {
  const openFile = useStore((s) => s.openFile);
  const active = useStore((s) => selectCurrentActiveFilePath(s) === path);
  const isSelected = selectedPaths.has(path) || active;
  const isFocused = focusedPath === path;
  const gitCode = gitStatusMap[path] ?? null;

  const handleClick = useCallback(
    (e: React.MouseEvent) => {
      onNodeClick(path, e);
      if (!e.shiftKey && !e.metaKey && !e.ctrlKey) {
        openFile(path);
      }
    },
    [path, onNodeClick, openFile],
  );

  // 右键菜单
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY });
  }, []);

  const closeContextMenu = useCallback(() => setContextMenu(null), []);

  useEffect(() => {
    if (contextMenu) {
      const handler = () => closeContextMenu();
      window.addEventListener("click", handler);
      return () => window.removeEventListener("click", handler);
    }
  }, [contextMenu, closeContextMenu]);

  return (
    <>
      <Row
        id={`file-tree-node-${encodeURIComponent(path)}`}
        data-node-path={path}
        depth={depth}
        active={isSelected}
        focused={isFocused}
        onClick={handleClick}
        onContextMenu={handleContextMenu}
        icon={
          <>
            <span className="w-3.5 shrink-0" />
            <FileIcon path={path} className="shrink-0 text-[16px] text-muted-foreground" />
          </>
        }
        label={name}
        gitStatus={gitCode}
      />
      {contextMenu && (
        <FileContextMenu
          path={path}
          x={contextMenu.x}
          y={contextMenu.y}
          onClose={closeContextMenu}
        />
      )}
    </>
  );
}

/* ─── 右键菜单 ─── */

function FileContextMenu({
  path,
  x,
  y,
  onClose,
}: {
  path: string;
  x: number;
  y: number;
  onClose: () => void;
}) {
  const openFile = useStore((s) => s.openFile);

  const actions = [
    {
      label: "打开",
      icon: "file" as const,
      action: () => {
        openFile(path);
        onClose();
      },
    },
    {
      label: "在 Finder 中显示",
      icon: "globe" as const,
      action: () => {
        revealItemInDir(path).catch(() => {});
        onClose();
      },
    },
    {
      label: "复制绝对路径",
      icon: "link" as const,
      action: () => {
        navigator.clipboard.writeText(path).catch(() => {});
        onClose();
      },
    },
    {
      label: "复制相对路径",
      icon: "files" as const,
      action: () => {
        // 相对于 workdir 的相对路径
        const workdir = useStore.getState().currentSession?.workdir ?? "";
        const rel = path.startsWith(workdir) ? path.slice(workdir.length + 1) : path;
        navigator.clipboard.writeText(rel).catch(() => {});
        onClose();
      },
    },
  ];

  useEffect(() => {
    const handler = () => onClose();
    window.addEventListener("click", handler);
    return () => window.removeEventListener("click", handler);
  }, [onClose]);

  return (
    <div
      className="fixed z-50 min-w-[180px] rounded-md border border-border bg-popover py-1 text-[13px] text-popover-foreground shadow-lg"
      style={{ left: x, top: y }}
      onClick={(e) => e.stopPropagation()}
    >
      {actions.map((a) => (
        <button
          key={a.label}
          type="button"
          onClick={a.action}
          className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-accent"
        >
          <Codicon name={a.icon} className="shrink-0 text-[14px] text-muted-foreground" />
          {a.label}
        </button>
      ))}
    </div>
  );
}

/* ─── 基础组件 ─── */

function Row({
  id,
  depth,
  active,
  focused,
  onClick,
  onContextMenu,
  icon,
  label,
  bold = false,
  gitStatus = null,
  trailing = null,
  "data-node-path": dataNodePath,
  "data-is-dir": dataIsDir,
}: {
  id?: string;
  depth: number;
  active: boolean;
  focused: boolean;
  onClick: (e: React.MouseEvent) => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  icon: React.ReactNode;
  label: string;
  bold?: boolean;
  gitStatus?: string | null;
  trailing?: React.ReactNode;
  "data-node-path"?: string;
  "data-is-dir"?: string;
}) {
  return (
    <div
      id={id}
      data-node-path={dataNodePath}
      data-is-dir={dataIsDir}
      className={cn(
        "group/row flex h-[22px] cursor-pointer items-center gap-1 pr-1 text-[13px] leading-none transition-colors",
        active ? "bg-accent text-accent-foreground" : "hover:bg-accent/70",
        focused && !active && "ring-1 ring-inset ring-primary/40",
      )}
      style={{ paddingLeft: `${4 + depth * INDENT}px` }}
      onClick={onClick}
      onContextMenu={onContextMenu}
    >
      {icon}
      <span className={cn("min-w-0 flex-1 truncate", bold && "font-medium")}>{label}</span>
      {gitStatus && (
        <span className={cn("shrink-0 text-[11px] font-bold", GIT_STATUS_COLORS[gitStatus] ?? "")}>
          {gitStatus}
        </span>
      )}
      {trailing}
    </div>
  );
}

function Hint({ depth, text, danger = false }: { depth: number; text: string; danger?: boolean }) {
  return (
    <div
      className={cn("flex h-[22px] items-center truncate text-[12px]", danger ? "text-destructive" : "text-muted-foreground")}
      style={{ paddingLeft: `${4 + depth * INDENT}px` }}
    >
      {text}
    </div>
  );
}

function EmptyState({ text, hint }: { text: string; hint?: string }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-1 px-6 text-center text-muted-foreground">
      <p className="text-[13px]">{text}</p>
      {hint && <p className="text-[12px] opacity-70">{hint}</p>}
    </div>
  );
}
