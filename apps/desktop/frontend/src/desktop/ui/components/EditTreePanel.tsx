import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { ChevronDown, ChevronRight, FilePenLine, Rewind } from "lucide-react";
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
            title="撤销本次对话的所有文件修改"
            className="rounded px-1 text-amber-600 hover:bg-amber-500/10 disabled:opacity-50"
          >
            <Rewind className="h-3.5 w-3.5" />
          </button>
        )}
      </div>
      {expanded && (
        <div className="space-y-2 bg-muted/10 p-2">
          {files.map((file) => (
            <RunFileDiff
              key={file.real_path}
              sessionId={sessionId}
              runId={run.run_id}
              file={file}
            />
          ))}
        </div>
      )}
    </section>
  );
}

function RunFileDiff({
  sessionId,
  runId,
  file,
}: {
  sessionId: string;
  runId: string;
  file: TurnFileChange;
}) {
  const [payload, setPayload] = useState<DiffPayload | null>(null);
  const [mode, setMode] = useState<DiffMode>("inline");

  useEffect(() => {
    let cancelled = false;
    api.diffEdit(sessionId, runId, file.real_path)
      .then((p) => { if (!cancelled) setPayload(p); })
      .catch((e) => {
        if (!cancelled) toast.error(e?.message ?? String(e));
      });
    return () => { cancelled = true; };
  }, [sessionId, runId, file.real_path]);

  // 删除类没有 after 内容，直接展示标记，不渲染 diff 详情
  if (file.action === "delete") {
    return (
      <div className="flex items-center gap-2 rounded border border-border/40 bg-background px-2 py-1.5 text-[10px]">
        <span className="rounded bg-red-500/15 px-1 font-medium text-red-600">已删除</span>
        <span className="truncate font-mono">{pathLeaf(file.real_path)}</span>
      </div>
    );
  }

  if (!payload) {
    return (
      <div className="rounded border border-border/40 bg-background px-2 py-1 text-[10px] text-muted-foreground">
        正在加载 {pathLeaf(file.real_path)} 的修改…
      </div>
    );
  }

  return (
    <DiffViewer
      beforeText={payload.before_text}
      afterText={payload.after_text}
      filePath={file.real_path}
      actionLabel={actionLabel(file.action)}
      mode={mode}
      onCycleMode={() => setMode((m) => (m === "inline" ? "split" : "inline"))}
      badge="本次净变化"
      maxRows={80}
      collapseContext={3}
      className="border-border/50"
    />
  );
}

function actionLabel(action: TurnFileChange["action"]): string {
  if (action === "create") return "创建文件";
  if (action === "overwrite") return "覆盖文件";
  if (action === "delete") return "删除文件";
  return "修改文件";
}

function pathLeaf(filePath: string): string {
  const parts = filePath.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || filePath;
}
