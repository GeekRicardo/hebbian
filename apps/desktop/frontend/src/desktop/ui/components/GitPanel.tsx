import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { Codicon } from "./Codicon";
import { FileIcon } from "./FileIcon";
import { gitDiffTabId } from "@/desktop/ui/store/useStore";
import type { GitFileStatus, GitProjectStatus } from "@/desktop/ui/types";

/**
 * 源代码管理（Git）栏：VS Code SCM 风格。
 */
interface TreeGroup {
  dir: string;
  name: string;
  files: GitFileStatus[];
  children: TreeGroup[];
}

const GIT_STATUS_COLORS: Record<string, string> = {
  M: "text-amber-600 dark:text-amber-400",
  A: "text-emerald-600 dark:text-emerald-400",
  D: "text-rose-600 dark:text-rose-400",
  U: "text-sky-600 dark:text-sky-400",
};

/* ─── 主组件 ─── */

export function GitPanel() {
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

  const [projects, setProjects] = useState<GitProjectStatus[]>([]);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    if (roots.length === 0) {
      setProjects([]);
      return;
    }
    setLoading(true);
    try {
      setProjects(await api.gitStatus(roots));
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, [roots]);

  useFocusRefresh(refresh);

  if (!sessionId) return <EmptyState text="当前没打开对话" />;
  if (roots.length === 0) {
    return <EmptyState text="这个对话还没绑定工作目录" hint="在对话设置里选一个目录后，git 改动会出现在这里。" />;
  }

  return (
    <div className="flex h-full flex-col bg-background text-foreground">
      <div className="flex h-8 shrink-0 items-center justify-between border-b border-border bg-muted/40 px-3 text-[11px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
        <span className="flex items-center gap-1.5">
          <Codicon name="source-control" className="text-[14px]" />
          源代码管理
        </span>
        <button
          type="button"
          onClick={refresh}
          title="刷新"
          aria-label="刷新"
          className="grid h-6 w-6 place-items-center rounded-sm hover:bg-accent hover:text-foreground"
        >
          {loading ? <Loader2 className="h-3 w-3 animate-spin" /> : <Codicon name="refresh" className="text-[13px]" />}
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {projects.length === 0 ? (
          <EmptyState
            text={loading ? "读取中…" : "没有 git 仓库或工作区无改动"}
            hint={loading ? undefined : "工作区里改动文件后，点刷新查看。"}
          />
        ) : (
          projects.map((proj) => (
            <ProjectSection key={proj.root} project={proj} onChanged={refresh} />
          ))
        )}
      </div>
    </div>
  );
}

function useFocusRefresh(refresh: () => void) {
  useEffect(() => {
    refresh();
  }, [refresh]); // eslint-disable-line react-hooks/exhaustive-deps
}

/* ─── 目录树构建 ─── */

function buildTree(files: GitFileStatus[]): TreeGroup[] {
  const root: TreeGroup = { dir: "", name: "", files: [], children: [] };
  const map: Record<string, TreeGroup> = { "": root };

  for (const f of files) {
    const parts = f.path.split("/");
    let current = "";
    for (let i = 0; i < parts.length - 1; i++) {
      const parent = current;
      current = current ? `${current}/${parts[i]}` : parts[i];
      if (!map[current]) {
        const group: TreeGroup = { dir: current, name: parts[i], files: [], children: [] };
        map[current] = group;
        map[parent].children.push(group);
      }
    }
    if (map[current]) {
      map[current].files.push(f);
    }
  }

  function sortGroup(g: TreeGroup) {
    g.children.sort((a, b) => a.name.localeCompare(b.name));
    g.files.sort((a, b) => a.path.localeCompare(b.path));
    for (const c of g.children) sortGroup(c);
  }
  sortGroup(root);

  return root.children;
}

function countStats(files: GitFileStatus[]): { added: number; modified: number; deleted: number } {
  const stats = { added: 0, modified: 0, deleted: 0 };
  for (const f of files) {
    if (f.untracked) { stats.added++; continue; }
    const y = f.y.trim();
    if (y === "A") stats.added++;
    else if (y === "D") stats.deleted++;
    else stats.modified++;
  }
  return stats;
}

/* ─── 子组件 ─── */

function ProjectSection({
  project,
  onChanged,
}: {
  project: GitProjectStatus;
  onChanged: () => void;
}) {
  const [message, setMessage] = useState("");
  const [committing, setCommitting] = useState(false);
  const [open, setOpen] = useState(true);

  const staged = project.files.filter((f) => f.staged);
  const changes = project.files.filter((f) => !f.staged);
  const stagedTree = useMemo(() => buildTree(staged), [staged]);
  const changesTree = useMemo(() => buildTree(changes), [changes]);
  const stagedStats = useMemo(() => countStats(staged), [staged]);
  const changesStats = useMemo(() => countStats(changes), [changes]);
  const totalFiles = project.files.length;

  const commit = async () => {
    if (!message.trim() || staged.length === 0) return;
    setCommitting(true);
    try {
      const sha = await api.gitCommit(project.root, message.trim());
      toast.success(`已提交 ${sha}`);
      setMessage("");
      onChanged();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setCommitting(false);
    }
  };

  return (
    <section className="border-b border-border">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex h-[22px] w-full items-center gap-1.5 px-3 text-left text-[12px] hover:bg-accent/70"
        aria-expanded={open}
        title={open ? "折叠项目" : "展开项目"}
      >
        {open ? (
          <Codicon name="chevron-down" className="shrink-0 text-[13px] text-muted-foreground" />
        ) : (
          <Codicon name="chevron-right" className="shrink-0 text-[13px] text-muted-foreground" />
        )}
        <Codicon name="repo" className="shrink-0 text-[13px] text-muted-foreground" />
        <span className="truncate font-medium">{project.name}</span>
        {project.branch && (
          <span className="shrink-0 rounded-sm bg-muted px-1 text-[10px] text-muted-foreground">
            {project.branch}
          </span>
        )}
        <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">{totalFiles} 项</span>
      </button>

      {open && (
        <>
          <div className="border-t border-border/60 px-2 py-1.5">
            <div className="flex items-center gap-1">
              <input
                value={message}
                onChange={(e) => setMessage(e.target.value)}
                onKeyDown={(e) => {
                  if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); void commit(); }
                }}
                placeholder={staged.length === 0 ? "先暂存改动再提交" : "提交信息（⌘/Ctrl+Enter）"}
                disabled={staged.length === 0}
                className="h-7 min-w-0 flex-1 border border-border bg-background px-2 text-xs outline-none focus:border-primary disabled:opacity-50"
              />
              <button
                type="button"
                onClick={commit}
                disabled={committing || staged.length === 0 || !message.trim()}
                title="提交已暂存内容"
                className="inline-flex h-7 shrink-0 items-center gap-1 bg-primary px-2 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-40"
              >
                {committing ? <Loader2 className="h-3 w-3 animate-spin" /> : <Codicon name="check" className="text-[13px]" />}
                提交
              </button>
            </div>
          </div>

          {staged.length > 0 && (
            <div>
              <GroupHeader title="暂存的更改" count={staged.length} stats={stagedStats} />
              <TreeGroupPanel trees={stagedTree} root={project.root} staged onChanged={onChanged} />
            </div>
          )}

          {changes.length > 0 && (
            <div>
              <GroupHeader title="更改" count={changes.length} stats={changesStats} />
              <TreeGroupPanel trees={changesTree} root={project.root} staged={false} onChanged={onChanged} />
            </div>
          )}

          {totalFiles === 0 && (
            <div className="px-3 py-2 text-[11px] text-muted-foreground">工作区干净，无改动。</div>
          )}
        </>
      )}
    </section>
  );
}

function GroupHeader({
  title,
  count,
  stats,
}: {
  title: string;
  count: number;
  stats: { added: number; modified: number; deleted: number };
}) {
  const statParts: string[] = [];
  if (stats.added > 0) statParts.push(`+${stats.added}`);
  if (stats.deleted > 0) statParts.push(`-${stats.deleted}`);
  if (stats.modified > 0) statParts.push(`~${stats.modified}`);

  return (
    <div className="flex items-center justify-between px-3 py-1 text-[10px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
      <span>{title} · {count}</span>
      {statParts.length > 0 && (
        <span className="shrink-0 text-[10px]">
          {statParts.map((s, i) => (
            <span
              key={i}
              className={cn("ml-1", s.startsWith("+") ? "text-emerald-500" : s.startsWith("-") ? "text-rose-500" : "text-amber-500")}
            >
              {s}
            </span>
          ))}
        </span>
      )}
    </div>
  );
}

/** 展开树：递归渲染目录和文件。 */
function TreeGroupPanel({
  trees,
  root,
  staged,
  onChanged,
  depth = 0,
}: {
  trees: TreeGroup[];
  root: string;
  staged: boolean;
  onChanged: () => void;
  depth?: number;
}) {
  const [expandedByDir, setExpandedByDir] = useState<Record<string, boolean>>({});

  return (
    <>
      {trees.map((group) => {
        const expanded = expandedByDir[group.dir] !== false;
        return (
          <div key={group.dir}>
            <div
              className="flex h-[22px] cursor-pointer items-center gap-1.5 px-3 text-[12px] transition-colors hover:bg-accent/70"
              style={{ paddingLeft: `${12 + depth * 12}px` }}
              onClick={() => setExpandedByDir((prev) => ({ ...prev, [group.dir]: !expanded }))}
            >
              {expanded ? (
                <Codicon name="chevron-down" className="shrink-0 text-[13px] text-muted-foreground" />
              ) : (
                <Codicon name="chevron-right" className="shrink-0 text-[13px] text-muted-foreground" />
              )}
              <Codicon name="folder-opened" className="shrink-0 text-[14px] text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate">{group.name}</span>
              <span className="shrink-0 text-[10px] text-muted-foreground">
                {group.files.length + group.children.length}
              </span>
            </div>
            {expanded && (
              <>
                <TreeGroupPanel trees={group.children} root={root} staged={staged} onChanged={onChanged} depth={depth + 1} />
                {group.files.map((f) => (
                  <GitFileRow key={f.path} root={root} file={f} staged={staged} onChanged={onChanged} depth={depth + 1} />
                ))}
              </>
            )}
          </div>
        );
      })}
    </>
  );
}

function GitFileRow({
  root,
  file,
  staged,
  onChanged,
  depth,
}: {
  root: string;
  file: GitFileStatus;
  staged: boolean;
  onChanged: () => void;
  depth: number;
}) {
  const openGitDiff = useStore((s) => s.openGitDiff);
  const activeTabId = useStore((s) => {
    const sid = s.currentSession?.id;
    return sid ? s.activeTabBySession[sid] ?? null : null;
  });
  const [busy, setBusy] = useState(false);
  const [confirmDiscard, setConfirmDiscard] = useState(false);

  const isActive = activeTabId === gitDiffTabId(root, file.path, staged);
  const code = statusCode(file, staged);

  const act = async (fn: () => Promise<void>, label: string) => {
    setBusy(true);
    try {
      await fn();
      onChanged();
    } catch (e: any) {
      toast.error(`${label}失败：${e?.message ?? String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const parts = file.path.split("/");
  const dirPart = parts.length > 1 ? parts.slice(0, -1).join("/") : null;

  return (
    <div
      className={cn(
        "group/git flex h-[22px] items-center gap-1.5 px-3 text-[12px]",
        isActive ? "bg-accent text-accent-foreground" : "hover:bg-accent/70",
      )}
      style={{ paddingLeft: `${12 + depth * 12}px` }}
    >
      <button
        type="button"
        onClick={() => openGitDiff(root, file.path, staged)}
        title={file.path}
        className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
      >
        <span className={cn("w-3.5 shrink-0 text-center font-mono text-[11px] font-bold", statusColor(code))}>
          {code}
        </span>
        <FileIcon path={file.path} className="shrink-0 text-[14px] text-muted-foreground" />
        <span className="min-w-0 truncate font-mono">{leafName(file.path)}</span>
        {dirPart && (
          <span className="hidden truncate pl-1 text-[10px] text-muted-foreground/60 group-hover/git:block">
            …/{dirPart}
          </span>
        )}
      </button>

      <div className="flex shrink-0 items-center gap-0.5">
        {confirmDiscard ? (
          <button
            type="button"
            onClick={() => act(() => api.gitDiscard(root, file.path, file.untracked), "丢弃")}
            disabled={busy}
            title="确认丢弃（不可恢复）"
            className="inline-flex h-5 items-center bg-destructive px-1.5 text-[10px] font-medium text-destructive-foreground"
          >
            确认丢弃
          </button>
        ) : staged ? (
          <IconBtn
            title="取消暂存"
            onClick={() => act(() => api.gitUnstage(root, file.path), "取消暂存")}
            disabled={busy}
          >
            <Codicon name="remove" className="text-[13px]" />
          </IconBtn>
        ) : (
          <>
            <IconBtn title="丢弃改动（不可恢复）" onClick={() => setConfirmDiscard(true)} disabled={busy} danger>
              <Codicon name="discard" className="text-[13px]" />
            </IconBtn>
            <IconBtn title="暂存" onClick={() => act(() => api.gitStage(root, file.path), "暂存")} disabled={busy}>
              <Codicon name="add" className="text-[13px]" />
            </IconBtn>
          </>
        )}
      </div>
    </div>
  );
}

/* ─── 帮助函数 ─── */

function statusCode(file: GitFileStatus, staged: boolean): string {
  if (file.untracked) return "U";
  const c = (staged ? file.x : file.y).trim();
  return c || "M";
}

function statusColor(code: string): string {
  return GIT_STATUS_COLORS[code] ?? "text-amber-600 dark:text-amber-400";
}

function leafName(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}

function IconBtn({ title, onClick, disabled, danger = false, children }: {
  title: string; onClick: () => void; disabled?: boolean; danger?: boolean; children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      aria-label={title}
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "grid h-5 w-5 place-items-center rounded-sm text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40",
        danger && "hover:bg-destructive/10 hover:text-destructive",
      )}
    >
      {children}
    </button>
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
