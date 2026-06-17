import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, File as FileIcon, Folder, FolderOpen, RotateCcw } from "lucide-react";
import { useStore, selectCurrentActiveFile } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
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

  if (!sessionId) return <EmptyState text="当前没打开对话" />;
  if (roots.length === 0) {
    return <EmptyState text="这个对话还没绑定工作目录" hint="在对话设置里选一个目录后，文件会出现在这里。" />;
  }

  return (
    <div className="py-1 text-[13px]">
      {roots.map((root) => (
        <RootNode key={root} path={root} multiRoot={roots.length > 1} />
      ))}
    </div>
  );
}

function rootLabel(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}

/** 顶层根：默认展开，展示根目录名（多根时）。 */
function RootNode({ path, multiRoot }: { path: string; multiRoot: boolean }) {
  return (
    <DirNode
      path={path}
      name={rootLabel(path)}
      depth={0}
      defaultExpanded={!multiRoot}
      isRoot
    />
  );
}

function DirNode({
  path,
  name,
  depth,
  defaultExpanded = false,
  isRoot = false,
}: {
  path: string;
  name: string;
  depth: number;
  defaultExpanded?: boolean;
  isRoot?: boolean;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
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
        onClick={() => setExpanded((v) => !v)}
        icon={
          <>
            {expanded ? (
              <ChevronDown className="h-3.5 w-3.5 shrink-0 opacity-60" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5 shrink-0 opacity-60" />
            )}
            {expanded ? (
              <FolderOpen className="h-3.5 w-3.5 shrink-0 text-amber-500" />
            ) : (
              <Folder className="h-3.5 w-3.5 shrink-0 text-amber-500" />
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
              className="grid h-5 w-5 place-items-center rounded text-muted-foreground opacity-0 hover:bg-accent hover:text-foreground group-hover/row:opacity-100"
            >
              <RotateCcw className="h-3 w-3" />
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
              <DirNode key={entry.path} path={entry.path} name={entry.name} depth={depth + 1} />
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
  const active = useStore((s) => selectCurrentActiveFile(s) === path);
  return (
    <Row
      depth={depth}
      active={active}
      onClick={() => openFile(path)}
      icon={
        <>
          <span className="w-3.5 shrink-0" />
          <FileIcon className="h-3.5 w-3.5 shrink-0 opacity-60" />
        </>
      }
      label={name}
    />
  );
}

const INDENT = 12;

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
        "group/row flex h-7 cursor-pointer items-center gap-1 rounded-sm pr-1 transition-colors",
        active ? "bg-accent text-foreground" : "hover:bg-accent/50",
      )}
      style={{ paddingLeft: `${6 + depth * INDENT}px` }}
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
      className={cn("flex h-6 items-center truncate text-[12px]", danger ? "text-destructive" : "text-muted-foreground")}
      style={{ paddingLeft: `${6 + depth * INDENT}px` }}
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
