import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useStore, selectCurrentActiveFilePath } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { Codicon } from "./Codicon";
import type { DirEntry } from "@/desktop/ui/types";

/**
 * 文件树面板：右侧工作台的一个 tab（图标排在第一位）。
 *
 * 多根（VSCode 多项目风格）：workdir + allowed_paths + runtime_allowed_paths 各算一个根。
 * 目录懒加载——展开一层拉一层（`api.readDir`），不递归遍历整仓库。
 * 点文件 → `openFile(path)` 把它送进中间的文件查看器列。
 */

export function FileTreeTab() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const workdir = useStore((s) => s.currentSession?.workdir ?? null);
  const allowedPaths = useStore((s) => s.currentSession?.allowed_paths ?? null);
  const runtimePaths = useStore((s) => s.currentSession?.runtime_allowed_paths ?? null);
  const storageKey = sessionId ? `hebbian.fileTree.expanded.${sessionId}` : null;
  const loadedStorageKeyRef = useRef<string | null>(null);
  const [expandedByPath, setExpandedByPath] = useState<Record<string, boolean>>({});

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

  if (!sessionId) return <EmptyState text="当前没打开对话" />;
  if (roots.length === 0) {
    return <EmptyState text="这个对话还没绑定工作目录" hint="在对话设置里选一个目录后，文件会出现在这里。" />;
  }

  return (
    <div className="py-1 font-[var(--font-sans)] text-[13px] text-foreground">
      {roots.map((root) => (
        <RootNode
          key={root}
          path={root}
          multiRoot={roots.length > 1}
          expandedByPath={expandedByPath}
          setNodeExpanded={setNodeExpanded}
        />
      ))}
    </div>
  );
}

function rootLabel(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}

/** 顶层根：默认展开，展示根目录名（多根时）。 */
function RootNode({
  path,
  multiRoot,
  expandedByPath,
  setNodeExpanded,
}: {
  path: string;
  multiRoot: boolean;
  expandedByPath: Record<string, boolean>;
  setNodeExpanded: (path: string, expanded: boolean) => void;
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
    />
  );
}

function DirNode({
  path,
  name,
  depth,
  defaultExpanded = false,
  isRoot = false,
  expandedByPath,
  setNodeExpanded,
}: {
  path: string;
  name: string;
  depth: number;
  defaultExpanded?: boolean;
  isRoot?: boolean;
  expandedByPath: Record<string, boolean>;
  setNodeExpanded: (path: string, expanded: boolean) => void;
}) {
  const expanded = expandedByPath[path] ?? defaultExpanded;
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setEntries(await api.readDir(path));
    } catch (e: any) {
      setError(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, [path]);

  // 首次展开才拉子项；展开过的目录不重复拉（手动刷新除外）。
  useEffect(() => {
    if (expanded && entries === null && !loading) void load();
  }, [expanded, entries, loading, load]);

  return (
    <div>
      <Row
        depth={depth}
        active={false}
        onClick={() => setNodeExpanded(path, !expanded)}
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
          {loading && entries === null && (
            <Hint depth={depth + 1} text="加载中…" />
          )}
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
              />
            ) : (
              <FileNode key={entry.path} path={entry.path} name={entry.name} depth={depth + 1} />
            ),
          )}
        </div>
      )}
    </div>
  );
}

function FileNode({ path, name, depth }: { path: string; name: string; depth: number }) {
  const openFile = useStore((s) => s.openFile);
  const active = useStore((s) => selectCurrentActiveFilePath(s) === path);
  return (
    <Row
      depth={depth}
      active={active}
      onClick={() => openFile(path)}
      icon={
        <>
          <span className="w-3.5 shrink-0" />
          <Codicon name="file" className="shrink-0 text-[16px] text-muted-foreground" />
        </>
      }
      label={name}
    />
  );
}

const INDENT = 8;

function Row({
  depth,
  active,
  onClick,
  icon,
  label,
  bold = false,
  trailing = null,
}: {
  depth: number;
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
  bold?: boolean;
  trailing?: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "group/row flex h-[22px] cursor-pointer items-center gap-1 pr-1 text-[13px] leading-none transition-colors",
        active ? "bg-accent text-accent-foreground" : "hover:bg-accent/70",
      )}
      style={{ paddingLeft: `${4 + depth * INDENT}px` }}
      onClick={onClick}
    >
      {icon}
      <span className={cn("min-w-0 flex-1 truncate", bold && "font-medium")}>{label}</span>
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
