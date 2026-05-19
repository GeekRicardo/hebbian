import { useEffect, useState, useCallback } from "react";
import {
  X,
  Columns2,
  Rows3,
  Maximize2,
  Minimize2,
  ArrowLeft,
  ArrowRight,
} from "lucide-react";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import type { DiffPayload, EditEntry } from "@/desktop/ui/types";

type DiffMode = "inline" | "split" | "fullscreen";

interface DiffPanelProps {
  sessionId: string;
  entry: EditEntry;
  onClose: () => void;
}

interface DiffRow {
  left: string;
  right: string;
  kind: "same" | "add" | "remove";
}

/** LCS-based diff: computes aligned rows marking added/removed/same lines. */
function computeDiff(beforeLines: string[], afterLines: string[]): DiffRow[] {
  const m = beforeLines.length;
  const n = afterLines.length;

  // DP table for LCS
  const dp = new Uint16Array((m + 1) * (n + 1));
  const idx = (i: number, j: number) => i * (n + 1) + j;

  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (beforeLines[i - 1] === afterLines[j - 1]) {
        dp[idx(i, j)] = dp[idx(i - 1, j - 1)] + 1;
      } else {
        dp[idx(i, j)] = Math.max(dp[idx(i - 1, j)], dp[idx(i, j - 1)]);
      }
    }
  }

  // Backtrack to produce aligned rows
  const rev: DiffRow[] = [];
  let i = m;
  let j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && beforeLines[i - 1] === afterLines[j - 1]) {
      rev.push({ left: beforeLines[i - 1], right: afterLines[j - 1], kind: "same" });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || dp[idx(i, j - 1)] >= dp[idx(i - 1, j)])) {
      rev.push({ left: "", right: afterLines[j - 1], kind: "add" });
      j--;
    } else {
      rev.push({ left: beforeLines[i - 1], right: "", kind: "remove" });
      i--;
    }
  }
  return rev.reverse();
}

export function DiffPanel({ sessionId, entry, onClose }: DiffPanelProps) {
  const [payload, setPayload] = useState<DiffPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<DiffMode>("split");

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .diffEdit(sessionId, entry.snapshot_id)
      .then((p) => {
        if (!cancelled) setPayload(p);
      })
      .catch((e) => {
        if (!cancelled) setError(e?.message ?? String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, entry.snapshot_id]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (mode === "fullscreen") {
          setMode("split");
        } else {
          onClose();
        }
      }
    },
    [mode, onClose],
  );

  useEffect(() => {
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [handleKeyDown]);

  function cycleMode() {
    setMode((prev) => {
      if (prev === "split") return "inline";
      if (prev === "inline") return "fullscreen";
      return "split";
    });
  }

  const actionLabel =
    entry.action === "create"
      ? "创建文件"
      : entry.action === "overwrite"
        ? "覆盖文件"
        : "修改文件";

  if (mode === "fullscreen") {
    return (
      <div className="fixed inset-0 z-[100] flex flex-col bg-background">
        <DiffHeader
          filePath={entry.real_path}
          actionLabel={actionLabel}
          mode={mode}
          diffRows={payload ? computeDiff(payload.before_text.split("\n"), payload.after_text.split("\n")) : null}
          onCycleMode={cycleMode}
          onClose={onClose}
        />
        <DiffBody
          payload={payload}
          loading={loading}
          error={error}
          mode={mode}
        />
      </div>
    );
  }

  // Non-blocking floating panel: pointer-events only on the card itself,
  // clicks pass through to chat behind it.
  return (
    <div className="fixed inset-0 z-[90] pointer-events-none">
      <div className="absolute right-4 top-[150px] w-[85vw] max-w-[960px] max-h-[85vh] flex flex-col rounded-xl border border-border bg-background/95 shadow-xl backdrop-blur pointer-events-auto">
        <DiffHeader
          filePath={entry.real_path}
          actionLabel={actionLabel}
          mode={mode}
          diffRows={payload ? computeDiff(payload.before_text.split("\n"), payload.after_text.split("\n")) : null}
          onCycleMode={cycleMode}
          onClose={onClose}
        />
        <DiffBody
          payload={payload}
          loading={loading}
          error={error}
          mode={mode}
        />
      </div>
    </div>
  );
}

function DiffHeader({
  filePath,
  actionLabel,
  mode,
  diffRows,
  onCycleMode,
  onClose,
}: {
  filePath: string;
  actionLabel: string;
  mode: DiffMode;
  diffRows: DiffRow[] | null;
  onCycleMode: () => void;
  onClose: () => void;
}) {
  const modeLabel =
    mode === "split" ? "分栏" : mode === "inline" ? "行内" : "全屏";
  const ModeIcon =
    mode === "split" ? Columns2 : mode === "inline" ? Rows3 : Minimize2;

  const changeCount =
    diffRows?.filter((r) => r.kind !== "same").length ?? null;

  return (
    <div className="flex items-center justify-between gap-2 border-b border-border bg-muted/30 px-3 py-2 shrink-0">
      <div className="min-w-0 flex items-center gap-2">
        <span className="truncate text-[12px] font-medium font-mono">
          {pathLeaf(filePath)}
        </span>
        <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
          {actionLabel}
        </span>
        {changeCount !== null && (
          <span className="text-[10px] text-muted-foreground">
            {changeCount === 0 ? "无变更" : `${changeCount} 行差异`}
          </span>
        )}
      </div>
      <div className="flex items-center gap-1">
        <button
          type="button"
          onClick={onCycleMode}
          className="inline-flex items-center gap-1 rounded px-2 py-1 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground"
          title={`当前：${modeLabel}。点击切换。`}
        >
          <ModeIcon className="h-3.5 w-3.5" />
          <span>{modeLabel}</span>
        </button>
        <button
          type="button"
          onClick={onClose}
          className="grid h-6 w-6 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
          title="关闭 (Esc)"
        >
          <X className="h-4 w-4" />
        </button>
      </div>
    </div>
  );
}

function DiffBody({
  payload,
  loading,
  error,
  mode,
}: {
  payload: DiffPayload | null;
  loading: boolean;
  error: string | null;
  mode: DiffMode;
}) {
  if (loading) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm text-muted-foreground py-16">
        加载差异…
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm text-destructive py-16 px-4 text-center">
        {error}
      </div>
    );
  }

  if (!payload) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm text-muted-foreground py-16">
        无差异数据
      </div>
    );
  }

  if (!payload.before_text && !payload.after_text) {
    return (
      <div className="flex-1 flex items-center justify-center text-sm text-muted-foreground py-16">
        文件为空（无变更）
      </div>
    );
  }

  const beforeLines = payload.before_text.split("\n");
  const afterLines = payload.after_text.split("\n");
  const diffRows = computeDiff(beforeLines, afterLines);

  if (mode === "inline") {
    return (
      <div className="flex-1 overflow-auto font-mono text-[11px] leading-relaxed">
        <div className="px-3 py-2">
          <InlineDiff diffRows={diffRows} />
        </div>
      </div>
    );
  }

  return <SplitDiff diffRows={diffRows} />;
}

function InlineDiff({ diffRows }: { diffRows: DiffRow[] }) {
  // Count same/remove lines for before display, same/add for after
  const beforeRows = diffRows.filter((r) => r.kind !== "add");
  const afterRows = diffRows.filter((r) => r.kind !== "remove");

  return (
    <div className="space-y-3">
      <div>
        <div className="mb-1 text-[10px] text-muted-foreground flex items-center gap-1">
          <ArrowLeft className="h-3 w-3" />
          修改前
        </div>
        <div className="rounded border border-border bg-muted/20 p-2">
          {beforeRows.map((row, i) => (
            <div
              key={i}
              className={cn(
                "min-h-[1.4em] whitespace-pre-wrap break-all",
                row.kind === "remove" && "bg-destructive/10 text-destructive",
              )}
            >
              {row.left || " "}
            </div>
          ))}
        </div>
      </div>
      <div>
        <div className="mb-1 text-[10px] text-muted-foreground flex items-center gap-1">
          <ArrowRight className="h-3 w-3" />
          修改后
        </div>
        <div className="rounded border border-border bg-muted/20 p-2">
          {afterRows.map((row, i) => (
            <div
              key={i}
              className={cn(
                "min-h-[1.4em] whitespace-pre-wrap break-all",
                row.kind === "add" && "bg-green-500/10 text-green-700 dark:text-green-400",
              )}
            >
              {row.right || " "}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function SplitDiff({ diffRows }: { diffRows: DiffRow[] }) {
  let lineNum = 0;

  return (
    <div className="flex-1 overflow-auto font-mono text-[11px] leading-relaxed">
      <div className="flex divide-x divide-border">
        {/* Before (left) */}
        <div className="flex-1 min-w-0">
          <div className="sticky top-0 bg-muted/40 border-b border-border px-2 py-1 text-[10px] text-muted-foreground flex items-center gap-1">
            <ArrowLeft className="h-3 w-3" />
            修改前
          </div>
          <div className="p-2">
            {diffRows.map((row, i) => {
              if (row.kind === "add") {
                return (
                  <div key={i} className="min-h-[1.4em] bg-muted/10" />
                );
              }
              lineNum++;
              return (
                <div
                  key={i}
                  className={cn(
                    "min-h-[1.4em] whitespace-pre-wrap break-all px-1",
                    row.kind === "remove" && "bg-destructive/10 text-destructive",
                  )}
                >
                  <span className="text-muted-foreground select-none mr-2 inline-block w-8 text-right text-[9px]">
                    {lineNum}
                  </span>
                  {row.left || " "}
                </div>
              );
            })}
          </div>
        </div>
        {/* After (right) */}
        <div className="flex-1 min-w-0">
          <div className="sticky top-0 bg-muted/40 border-b border-border px-2 py-1 text-[10px] text-muted-foreground flex items-center gap-1">
            <ArrowRight className="h-3 w-3" />
            修改后
          </div>
          <div className="p-2">
            {(() => {
              let n = 0;
              return diffRows.map((row, i) => {
                if (row.kind === "remove") {
                  return (
                    <div key={i} className="min-h-[1.4em] bg-muted/10" />
                  );
                }
                n++;
                return (
                  <div
                    key={i}
                    className={cn(
                      "min-h-[1.4em] whitespace-pre-wrap break-all px-1",
                      row.kind === "add" && "bg-green-500/10 text-green-700 dark:text-green-400",
                    )}
                  >
                    <span className="text-muted-foreground select-none mr-2 inline-block w-8 text-right text-[9px]">
                      {n}
                    </span>
                    {row.right || " "}
                  </div>
                );
              });
            })()}
          </div>
        </div>
      </div>
    </div>
  );
}

function pathLeaf(filePath: string): string {
  const parts = filePath.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || filePath;
}
