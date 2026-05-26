import { useState, useEffect } from "react";
import { toast } from "sonner";
import { FilePenLine, Plus, Rewind, ChevronDown, ChevronRight } from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import { cn, formatTime } from "@/desktop/ui/lib/utils";
import { focusToolCall } from "@/desktop/ui/lib/focusToolCall";
import type { EditEntry } from "@/desktop/ui/types";

// 稳定空数组引用：zustand selector 用浅比较，每次返回新 `[]` 会触发无限重渲染。
const EMPTY_ENTRIES: EditEntry[] = [];

/**
 * 旧版本浮动卡片——已被 RightSidebar 内的 `EditTreeTab` 替代（架构 §4.13.x 修订）。
 * 保留 export 占位让老的 import 还能通过类型检查；本身永远不渲染。
 */
export function EditTreePanel() {
  return null;
}

/**
 * 工作台 sidebar 内的「修改文件」tab 内容（架构 §4.13 修订）。
 *
 * 展示当前 session 所有 Edit 工具快照，按文件路径分组、支持单次回退。
 * 数据源（`useStore.sessionEditSnapshots[currentSessionId]`，session-scoped），
 * 嵌入 RightSidebar tab 区。空状态显示 hint 而不是 return null（沿 sidebar 风格）。
 */
export function EditTreeTab() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const editSnapshots = useStore(
    (s) => (sessionId ? s.sessionEditSnapshots[sessionId] : undefined) ?? EMPTY_ENTRIES,
  );
  const revertEdit = useStore((s) => s.revertEdit);
  const refreshEdits = useStore((s) => s.refreshEdits);
  const [reverting, setReverting] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    // 切到本 tab 时主动拉一次后端权威列表——覆盖「应用启动后没开过 run」的场景：
    // openSession 那边的 refreshEdits 也会跑，这里是兜底（特别是 hebweb 上 listen
    // 不到 edit 事件、只能靠拉的场景）。
    void refreshEdits();
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
  }, [sessionId, refreshEdits]);

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

  if (!sessionId) {
    return (
      <div className="grid h-full place-items-center px-4 py-8 text-center text-[11px] text-muted-foreground">
        <div>
          <FilePenLine className="mx-auto mb-2 h-5 w-5 opacity-40" />
          当前没打开对话
        </div>
      </div>
    );
  }

  if (editSnapshots.length === 0) {
    return (
      <div className="grid h-full place-items-center px-4 py-8 text-center text-[11px] text-muted-foreground">
        <div>
          <FilePenLine className="mx-auto mb-2 h-5 w-5 opacity-40" />
          还没有 Edit 修改。
          <br />
          <span className="text-[10px]">
            模型用 <code className="rounded bg-muted px-1">Edit</code> 修改文件后会出现在这里。
          </span>
        </div>
      </div>
    );
  }

  const activeEntries = editSnapshots.filter((e) => !e.reverted);
  const revertedEntries = editSnapshots.filter((e) => e.reverted);

  return (
    <div className="text-[12px]">
      {activeEntries.length > 0 && (
        <EditSection
          entries={activeEntries}
          reverting={reverting}
          onRevert={handleRevert}
        />
      )}
      {revertedEntries.length > 0 && (
        <EditSection
          title="已回退"
          entries={revertedEntries}
          reverting={reverting}
          onRevert={undefined}
          dimmed
        />
      )}
    </div>
  );
}

function EditSection({
  title,
  entries,
  reverting,
  onRevert,
  dimmed,
}: {
  title?: string;
  entries: EditEntry[];
  reverting: Set<string>;
  onRevert?: (snapshotId: string) => void;
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
    <div className="space-y-2 px-2 py-2">
      {title && (
        <div className="px-1 text-[10px] uppercase tracking-wide text-muted-foreground">
          {title}
        </div>
      )}
      {[...groups.entries()].map(([filePath, fileEntries]) => {
        const groupKey = filePath;
        const expanded = expandedGroups.has(groupKey);
        const latest = fileEntries[0];
        return (
          <div
            key={groupKey}
            className={cn(
              "overflow-hidden rounded-md border border-border/60 bg-background transition-all",
              // 默认：与大卡片同款的模糊散开阴影；hover 才切成 neobrutalism 错位
              "shadow-[-3px_2px_8px_-2px_rgba(0,0,0,0.10),-1px_1px_2px_-1px_rgba(0,0,0,0.06)]",
              "dark:shadow-[-3px_2px_8px_-2px_rgba(0,0,0,0.45),-1px_1px_2px_-1px_rgba(0,0,0,0.3)]",
              "hover:shadow-[-6px_8px_4px_1px_rgba(0,0,0,0.50),-18px_21px_14px_-1px_rgba(0,0,0,0.16)] dark:hover:shadow-[-6px_8px_4px_1px_rgba(0,0,0,0.75),-18px_21px_14px_-1px_rgba(0,0,0,0.40)]",
              "hover:translate-x-px hover:-translate-y-px hover:border-border",
              dimmed && "opacity-60",
            )}
          >
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
              className="flex w-full items-center gap-1 px-2 py-1.5 text-left text-[11px] transition-colors hover:bg-accent/30"
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
            {expanded && (
              <div className="border-t border-border/40 bg-muted/20 py-1">
                {fileEntries.map((entry, idx) => {
                  // 反向 patch 的天然限制：回退非最新一次 Edit 时，patch 的上下文
                  // 行可能已被后续 Edit 改动，git apply 会拒绝。让用户提前知道。
                  const isLatest = idx === fileEntries.length - 1;
                  const revertHint = isLatest
                    ? "撤销本次修改"
                    : "撤销此次修改；若后续 Edit 改动了同一段，可能因冲突失败";
                  return (
                  <div
                    key={entry.snapshot_id}
                    className={cn(
                      "flex items-center gap-1 px-3 py-1 text-[10px] transition-colors",
                      !dimmed && "hover:bg-accent/40 cursor-pointer",
                      dimmed && "opacity-40"
                    )}
                    onClick={() => focusToolCall(entry.call_id)}
                    title="跳到对话里的这次修改"
                    role="button"
                  >
                    <span className="flex-1 truncate text-muted-foreground">
                      {entry.action === "create" ? "创建" : entry.action === "overwrite" ? "覆写" : "修改"}
                      {" · "}
                      {entry.before_bytes === entry.after_bytes
                        ? `${entry.before_bytes}B`
                        : `${entry.before_bytes}→${entry.after_bytes}B`}
                    </span>
                    {onRevert && !entry.reverted && (
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          onRevert(entry.snapshot_id);
                        }}
                        disabled={reverting.has(entry.snapshot_id)}
                        title={revertHint}
                        className="rounded px-1 text-[9px] text-amber-600 hover:bg-amber-500/10 disabled:opacity-50"
                      >
                        <Rewind className="h-3 w-3" />
                      </button>
                    )}
                  </div>
                );
                })}
              </div>
            )}
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
