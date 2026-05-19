import { useState, useEffect } from "react";
import { toast } from "sonner";
import { FilePenLine, Plus, Rewind, X, ChevronDown, ChevronRight } from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn, formatTime } from "@/desktop/ui/lib/utils";
import type { EditEntry } from "@/desktop/ui/types";
import { DiffPanel } from "./DiffPanel";

/**
 * 架构 §4.13：EditTree 浮动卡片。
 *
 * 展示当前 session 所有 Edit 工具快照条目，支持：
 * - 折叠药丸 / 展开卡片
 * - 按文件路径查看 before/after 差异
 * - 单次回退（revert）
 *
 * 三种状态：
 * - 无快照条目 → 组件不渲染
 * - 有条目 → 默认展开；点 X 折叠为药丸
 * - 折叠 → 显示「N 次修改」药丸；点开恢复
 */
export function EditTreePanel() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const editSnapshots = useStore((s) => s.editSnapshots);
  const revertEdit = useStore((s) => s.revertEdit);
  const [collapsed, setCollapsed] = useState(false);
  const [diffEntry, setDiffEntry] = useState<EditEntry | null>(null);
  const [reverting, setReverting] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    api.editsWorktreeStatus(sessionId)
      .then((status) => {
        if (!cancelled && !status.enabled && status.entry_count === 0) {
          toast.warning("Git 不可用，Edit 修改记录功能已禁用", {
            description: "请确认系统已安装 git CLI 工具。",
          });
        }
      })
      .catch(() => { /* 静默 */ });
    return () => { cancelled = true; };
  }, [sessionId]);

  if (!sessionId || editSnapshots.length === 0) return null;

  const activeEntries = editSnapshots.filter((e) => !e.reverted);
  const revertedEntries = editSnapshots.filter((e) => e.reverted);

  async function handleRevert(snapshotId: string) {
    if (!sessionId) return;
    setReverting((prev) => new Set(prev).add(snapshotId));
    try {
      await revertEdit(sessionId, snapshotId);
      toast.success("已回退修改");
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setReverting((prev) => {
        const next = new Set(prev);
        next.delete(snapshotId);
        return next;
      });
    }
  }

  if (collapsed) {
    return (
      <button
        type="button"
        onClick={() => setCollapsed(false)}
        className="pointer-events-auto absolute right-4 top-[150px] z-30 inline-flex items-center gap-1.5 rounded-full border border-border bg-background/95 px-2.5 py-1 text-[11px] text-muted-foreground shadow-sm backdrop-blur transition-colors hover:bg-background hover:text-foreground"
        title="展开 Edit 修改树"
      >
        <FilePenLine className="h-3 w-3" />
        <span>
          {activeEntries.length} 次修改
          {revertedEntries.length > 0 && `（${revertedEntries.length} 已回退）`}
        </span>
      </button>
    );
  }

  return (
    <>
      <div className="pointer-events-auto absolute right-4 top-[150px] z-30 w-[300px] overflow-hidden rounded-lg border border-border bg-background/95 shadow-md backdrop-blur">
        <div className="flex items-center justify-between gap-2 border-b border-border bg-muted/30 px-2.5 py-1.5">
          <div className="min-w-0">
            <div className="text-[11px] font-medium leading-tight">修改记录</div>
            <div className="mt-0.5 text-[10px] text-muted-foreground">
              {activeEntries.length} 次修改
              {revertedEntries.length > 0 && ` · ${revertedEntries.length} 已回退`}
            </div>
          </div>
          <button
            type="button"
            onClick={() => setCollapsed(true)}
            className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
            title="收起"
            aria-label="收起修改记录面板"
          >
            <X className="h-3 w-3" />
          </button>
        </div>

        <div className="max-h-[50vh] overflow-auto">
          {activeEntries.length > 0 && (
            <EditSection
              entries={activeEntries}
              reverting={reverting}
              onRevert={handleRevert}
              onDiff={setDiffEntry}
            />
          )}
          {revertedEntries.length > 0 && (
            <EditSection
              title="已回退"
              entries={revertedEntries}
              reverting={reverting}
              onRevert={undefined}
              onDiff={setDiffEntry}
              dimmed
            />
          )}
        </div>
      </div>

      {diffEntry && sessionId && (
        <DiffPanel
          sessionId={sessionId}
          entry={diffEntry}
          onClose={() => setDiffEntry(null)}
        />
      )}
    </>
  );
}

function EditSection({
  title,
  entries,
  reverting,
  onRevert,
  onDiff,
  dimmed,
}: {
  title?: string;
  entries: EditEntry[];
  reverting: Set<string>;
  onRevert?: (snapshotId: string) => void;
  onDiff: (entry: EditEntry) => void;
  dimmed?: boolean;
}) {
  // 按文件路径分组
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());

  const groups = new Map<string, EditEntry[]>();
  for (const entry of entries) {
    const existing = groups.get(entry.real_path) ?? [];
    existing.push(entry);
    groups.set(entry.real_path, existing);
  }

  return (
    <div className="px-2.5 py-2">
      {title && (
        <div className="mb-1 text-[10px] uppercase tracking-wide text-muted-foreground">
          {title}
        </div>
      )}
      {[...groups.entries()].map(([filePath, fileEntries]) => {
        const groupKey = filePath;
        const expanded = expandedGroups.has(groupKey);
        const latest = fileEntries[0];
        return (
          <div key={groupKey} className="mb-1">
            <button
              type="button"
              onClick={() =>
                setExpandedGroups((prev) => {
                  const next = new Set(prev);
                  if (next.has(groupKey)) next.delete(groupKey);
                  else next.add(groupKey);
                  return next;
                })
              }
              className={cn(
                "flex w-full items-center gap-1 rounded-md px-1.5 py-1 text-left text-[11px] transition-colors hover:bg-accent/50",
                dimmed && "opacity-50"
              )}
            >
              {fileEntries.length > 1 ? (
                expanded ? (
                  <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
                ) : (
                  <ChevronRight className="h-3 w-3 shrink-0 text-muted-foreground" />
                )
              ) : (
                <span className="w-3 shrink-0" />
              )}
              <ActionIcon action={latest.action} />
              <span className="truncate flex-1 font-mono text-[10px]">
                {pathLeaf(latest.real_path)}
              </span>
              <span className="shrink-0 text-[9px] text-muted-foreground">
                {formatTime(latest.ts_ms)}
              </span>
            </button>
            {expanded &&
              fileEntries.map((entry) => (
                <div
                  key={entry.snapshot_id}
                  className={cn(
                    "ml-5 flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px]",
                    dimmed && "opacity-40"
                  )}
                >
                  <span className="flex-1 truncate text-muted-foreground">
                    {entry.action === "create" ? "创建" : entry.action === "overwrite" ? "覆写" : "修改"}
                    {" · "}
                    {entry.before_bytes === entry.after_bytes
                      ? `${entry.before_bytes}B`
                      : `${entry.before_bytes}→${entry.after_bytes}B`}
                  </span>
                  <button
                    type="button"
                    onClick={() => onDiff(entry)}
                    className="rounded px-1 text-[9px] text-muted-foreground hover:bg-accent hover:text-foreground"
                  >
                    对比
                  </button>
                  {onRevert && !entry.reverted && (
                    <button
                      type="button"
                      onClick={() => onRevert(entry.snapshot_id)}
                      disabled={reverting.has(entry.snapshot_id)}
                      className="rounded px-1 text-[9px] text-amber-600 hover:bg-amber-500/10 disabled:opacity-50"
                    >
                      <Rewind className="h-3 w-3" />
                    </button>
                  )}
                </div>
              ))}
          </div>
        );
      })}
    </div>
  );
}

function ActionIcon({ action }: { action: EditEntry["action"] }) {
  const cls = "h-3 w-3 shrink-0";
  if (action === "create") return <Plus className={cn(cls, "text-green-500")} />;
  if (action === "overwrite")
    return <FilePenLine className={cn(cls, "text-amber-500")} />;
  return <FilePenLine className={cn(cls, "text-sky-500")} />;
}

function pathLeaf(filePath: string): string {
  const parts = filePath.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || filePath;
}
