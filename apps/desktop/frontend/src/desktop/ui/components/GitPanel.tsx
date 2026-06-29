import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { Codicon } from "./Codicon";
import { gitDiffTabId } from "@/desktop/ui/store/useStore";
import type { GitFileStatus, GitProjectStatus } from "@/desktop/ui/types";

/**
 * 源代码管理（Git）栏：VSCode SCM 风格（架构 §4.12.13）。
 *
 * - 按项目（git 仓库根）分组；每项目分 Staged / Changes 两段
 * - 点文件 → 在中间编辑区开 git diff（HEAD/index vs 工作区）
 * - 行尾 hover 操作：stage / unstage / discard（discard 二次确认）
 * - 项目顶部 commit message 输入 + 提交（仅 Staged 非空可用）
 *
 * 与「修改文件」栏的分工：那栏是 AI 单次 Run 的 edits 快照、可整 Run 回退；
 * 本栏是项目本身的 git 状态（用户手改 + AI 改混在一起，相对 HEAD/index）。
 *
 * ⚠️ discard / commit 直接动用户真实仓库、不可逆，不在 edits-worktree 回退保护内。
 */
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

  // 切到本 tab / 切对话时拉一次（不轮询，避免频繁起 git 进程）。
  useEffect(() => {
    refresh();
  }, [refresh]);

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
        <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">
          {project.files.length} 项改动
        </span>
      </button>

      {open && (
        <>
          {/* commit 区：有暂存才显示输入 */}
          <div className="border-t border-border/60 px-2 py-1.5">
            <div className="flex items-center gap-1">
              <input
                value={message}
                onChange={(e) => setMessage(e.target.value)}
                onKeyDown={(e) => {
                  if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                    e.preventDefault();
                    void commit();
                  }
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
            <Group title="暂存的更改" count={staged.length}>
              {staged.map((f) => (
                <FileRow key={`s:${f.path}`} root={project.root} file={f} staged onChanged={onChanged} />
              ))}
            </Group>
          )}
          {changes.length > 0 && (
            <Group title="更改" count={changes.length}>
              {changes.map((f) => (
                <FileRow key={`w:${f.path}`} root={project.root} file={f} staged={false} onChanged={onChanged} />
              ))}
            </Group>
          )}
          {project.files.length === 0 && (
            <div className="px-3 py-2 text-[11px] text-muted-foreground">工作区干净，无改动。</div>
          )}
        </>
      )}
    </section>
  );
}

function Group({ title, count, children }: { title: string; count: number; children: React.ReactNode }) {
  return (
    <div className="pb-1">
      <div className="px-3 py-1 text-[10px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
        {title} · {count}
      </div>
      {children}
    </div>
  );
}

function FileRow({
  root,
  file,
  staged,
  onChanged,
}: {
  root: string;
  file: GitFileStatus;
  staged: boolean;
  onChanged: () => void;
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

  return (
    <div
      className={cn(
        "group/git flex h-[22px] items-center gap-1.5 px-3 text-[12px]",
        isActive ? "bg-accent text-accent-foreground" : "hover:bg-accent/70",
      )}
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
        <span className="min-w-0 truncate font-mono">{leafName(file.path)}</span>
      </button>

      {/* 行尾操作（hover 显示），busy 时禁用 */}
      <div className="flex shrink-0 items-center gap-0.5 opacity-0 group-hover/git:opacity-100">
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
            <IconBtn
              title="丢弃改动（不可恢复）"
              onClick={() => setConfirmDiscard(true)}
              disabled={busy}
              danger
            >
              <Codicon name="discard" className="text-[13px]" />
            </IconBtn>
            <IconBtn
              title="暂存"
              onClick={() => act(() => api.gitStage(root, file.path), "暂存")}
              disabled={busy}
            >
              <Codicon name="add" className="text-[13px]" />
            </IconBtn>
          </>
        )}
      </div>
    </div>
  );
}

function IconBtn({
  title,
  onClick,
  disabled,
  danger = false,
  children,
}: {
  title: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
  children: React.ReactNode;
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

/** 取要展示的单字状态码：暂存看 index 态 X，未暂存看 worktree 态 Y；未跟踪统一 U。 */
function statusCode(file: GitFileStatus, staged: boolean): string {
  if (file.untracked) return "U";
  const c = (staged ? file.x : file.y).trim();
  return c || "M";
}

function statusColor(code: string): string {
  switch (code) {
    case "A":
      return "text-emerald-600 dark:text-emerald-400";
    case "D":
      return "text-rose-600 dark:text-rose-400";
    case "U":
      return "text-sky-600 dark:text-sky-400";
    case "R":
      return "text-violet-600 dark:text-violet-400";
    default:
      return "text-amber-600 dark:text-amber-400";
  }
}

function leafName(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}

function EmptyState({ text, hint }: { text: string; hint?: string }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-1 px-6 text-center text-muted-foreground">
      <p className="text-[13px]">{text}</p>
      {hint && <p className="text-[12px] opacity-70">{hint}</p>}
    </div>
  );
}