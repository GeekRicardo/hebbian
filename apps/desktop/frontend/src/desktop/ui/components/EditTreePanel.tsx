import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { ChevronDown, ChevronRight, FilePenLine, RotateCcw } from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn, formatTime } from "@/desktop/ui/lib/utils";
import { DiffViewer, type DiffMode } from "./DiffPanel";
import type { DiffPayload, RunEditEntry, TurnFileChange } from "@/desktop/ui/types";

const EMPTY_RUNS: RunEditEntry[] = [];

export function EditTreePanel() {
  return null;
}

export function EditTreeTab() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const runs = useStore(
    (s) => (sessionId ? s.sessionEditSnapshots[sessionId] : undefined) ?? EMPTY_RUNS,
  );
  const revertEdit = useStore((s) => s.revertEdit);
  const [reverting, setReverting] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    void useStore.getState().refreshEdits();
    api.editsWorktreeStatus(sessionId)
      .then((status) => {
        if (!cancelled && !status.enabled && status.entry_count === 0) {
          toast.warning("Git 不可用，修改记录功能已禁用", {
            description: "请确认系统已安装 git CLI 工具。",
          });
        }
      })
      .catch(() => {});
    return () => { cancelled = true; };
  }, [sessionId]);

  async function handleRevert(runId: string) {
    if (!sessionId) return;
    setReverting((prev) => new Set(prev).add(runId));
    try {
      await revertEdit(sessionId, runId);
      toast.success("已回退本次修改");
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setReverting((prev) => {
        const next = new Set(prev);
        next.delete(runId);
        return next;
      });
    }
  }

  if (!sessionId) return <EmptyState text="当前没打开对话" />;
  if (runs.length === 0) {
    return <EmptyState text="还没有文件修改。" hint="模型修改文件后会出现在这里。" />;
  }

  const sorted = [...runs].sort((a, b) => b.finished_at_ms - a.finished_at_ms);

  return (
    <div className="space-y-2 px-2 py-2 text-[12px]">
      {sorted.map((run, idx) => (
        <RunGroup
          key={run.run_id}
          sessionId={sessionId}
          run={run}
          label={idx === 0 ? "最新一次修改" : "较早的修改"}
          defaultExpanded={idx === 0}
          reverting={reverting.has(run.run_id)}
          onRevert={handleRevert}
        />
      ))}
    </div>
  );
}

function EmptyState({ text, hint }: { text: string; hint?: string }) {
  return (
    <div className="grid h-full place-items-center px-4 py-8 text-center text-[11px] text-muted-foreground">
      <div>
        <FilePenLine className="mx-auto mb-2 h-5 w-5 opacity-40" />
        {text}
        {hint && <><br /><span className="text-[10px]">{hint}</span></>}
      </div>
    </div>
  );
}

function RunGroup({
  sessionId,
  run,
  label,
  defaultExpanded,
  reverting,
  onRevert,
}: {
  sessionId: string;
  run: RunEditEntry;
  label: string;
  defaultExpanded: boolean;
  reverting: boolean;
  onRevert: (runId: string) => void;
}) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  const files = useMemo(
    () => [...run.files].sort((a, b) => a.real_path.localeCompare(b.real_path)),
    [run.files],
  );

  return (
    <section
      id={`run-edits-${run.run_id}`}
      className={cn(
        "overflow-hidden rounded-md border border-border/60 bg-background",
        "shadow-[-3px_2px_8px_-2px_rgba(0,0,0,0.10),-1px_1px_2px_-1px_rgba(0,0,0,0.06)]",
        "dark:shadow-[-3px_2px_8px_-2px_rgba(0,0,0,0.45),-1px_1px_2px_-1px_rgba(0,0,0,0.3)]",
        run.reverted && "opacity-60",
      )}
    >
      <div className="flex items-center gap-1 border-b border-border/50 px-2 py-1.5">
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="flex min-w-0 flex-1 items-center gap-1 text-left"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
          <span className="truncate font-medium">{label}</span>
          <span className="shrink-0 text-[10px] text-muted-foreground">
            {files.length} 个文件 · {formatTime(run.finished_at_ms)}
          </span>
        </button>
        {!run.reverted && (
          <button
            type="button"
            onClick={() => onRevert(run.run_id)}
            disabled={reverting}
            title="回退这次修改"
            aria-label="回退这次修改"
            className="grid h-6 w-6 place-items-center rounded-full text-muted-foreground transition-colors hover:bg-amber-500/10 hover:text-amber-700 disabled:opacity-50 dark:hover:text-amber-300"
          >
            <RotateCcw className={cn("h-3.5 w-3.5", reverting && "animate-spin")} />
          </button>
        )}
      </div>
      {expanded && (
        <div className="space-y-2 bg-muted/10 p-2">
          {files.map((file, idx) => (
            <RunFileCard
              key={file.real_path}
              sessionId={sessionId}
              runId={run.run_id}
              file={file}
              defaultExpanded={idx === 0}
            />
          ))}
        </div>
      )}
    </section>
  );
}

/**
 * 单个文件的可折叠子卡片：标题行（action 角标 + 文件名 + 大小变化 + 折叠箭头）
 * 点击切换展开/折叠 diff。删除类不渲染 diff，仅展示标题。
 */
function RunFileCard({
  sessionId,
  runId,
  file,
  defaultExpanded,
}: {
  sessionId: string;
  runId: string;
  file: TurnFileChange;
  defaultExpanded: boolean;
}) {
  const isDelete = file.action === "delete";
  const [expanded, setExpanded] = useState(defaultExpanded && !isDelete);
  const [payload, setPayload] = useState<DiffPayload | null>(null);
  const [mode, setMode] = useState<DiffMode>("inline");

  // 仅展开且非删除时才拉 diff——折叠的文件不发请求，省网络。
  useEffect(() => {
    if (!expanded || isDelete || payload) return;
    let cancelled = false;
    api.diffEdit(sessionId, runId, file.real_path)
      .then((p) => { if (!cancelled) setPayload(p); })
      .catch((e) => {
        if (!cancelled) toast.error(e?.message ?? String(e));
      });
    return () => { cancelled = true; };
  }, [expanded, isDelete, payload, sessionId, runId, file.real_path]);

  return (
    <div className="overflow-hidden rounded border border-border/50 bg-background">
      <button
        type="button"
        onClick={() => !isDelete && setExpanded((v) => !v)}
        className={cn(
          "flex w-full items-center gap-1.5 px-2 py-1.5 text-left text-[10px]",
          !isDelete && "hover:bg-accent/30",
        )}
      >
        {isDelete ? (
          <span className="w-3 shrink-0" />
        ) : expanded ? (
          <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
        ) : (
          <ChevronRight className="h-3 w-3 shrink-0 text-muted-foreground" />
        )}
        <span className={cn("shrink-0 rounded px-1 font-medium", actionBadgeClass(file.action))}>
          {actionLabel(file.action)}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono">{pathLeaf(file.real_path)}</span>
        <span className="shrink-0 text-[9px] text-muted-foreground">
          {file.before_bytes}→{file.after_bytes}B
        </span>
      </button>
      {expanded && !isDelete && (
        payload ? (
          <DiffViewer
            beforeText={payload.before_text}
            afterText={payload.after_text}
            filePath={file.real_path}
            actionLabel={actionLabel(file.action)}
            mode={mode}
            onCycleMode={() => setMode((m) => (m === "inline" ? "split" : "inline"))}
            badge="本次净变化"
            hideHeaderMeta
            maxRows={80}
            collapseContext={3}
            className="rounded-none border-0 border-t border-border/40"
          />
        ) : (
          <div className="border-t border-border/40 px-2 py-1.5 text-[10px] text-muted-foreground">
            正在加载修改…
          </div>
        )
      )}
    </div>
  );
}

function actionLabel(action: TurnFileChange["action"]): string {
  if (action === "create") return "创建";
  if (action === "overwrite") return "覆盖";
  if (action === "delete") return "删除";
  return "修改";
}

function actionBadgeClass(action: TurnFileChange["action"]): string {
  if (action === "create") return "bg-emerald-500/15 text-emerald-600";
  if (action === "delete") return "bg-red-500/15 text-red-600";
  if (action === "overwrite") return "bg-amber-500/15 text-amber-600";
  return "bg-sky-500/15 text-sky-600";
}

function pathLeaf(filePath: string): string {
  const parts = filePath.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || filePath;
}
