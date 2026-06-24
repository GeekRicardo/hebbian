import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { Download, Loader2, Search } from "lucide-react";
import { Dialog } from "./ui/dialog";
import { api, type ClaudeSessionInfo } from "@/desktop/bridge/tauri";
import { formatTime, pathLeaf } from "@/desktop/ui/lib/utils";

interface ImportClaudeDialogProps {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  /** 导入成功后回调，传新建 session 的 id，由父去刷新列表 + 打开它。 */
  onImported: (sessionId: string) => void;
  /**
   * 项目入口导入时传入：只展示 cwd 精确等于该项目目录的 Claude 会话，
   * 且导入的会话显式归属此项目。全局入口不传 = 展示全部、不绑定项目。
   */
  projectFilter?: { id: string; path: string; name: string } | null;
}

const NO_DIR = "（没有项目目录）";

/**
 * 从 Claude 导入一段对话。
 * - 全局入口：列出全部会话，按原项目目录分组。
 * - 项目入口（projectFilter）：只列 cwd 精确等于该项目目录的会话，导入后归属此项目。
 */
export function ImportClaudeDialog({
  open,
  onOpenChange,
  onImported,
  projectFilter,
}: ImportClaudeDialogProps) {
  const [loading, setLoading] = useState(false);
  const [items, setItems] = useState<ClaudeSessionInfo[]>([]);
  const [query, setQuery] = useState("");
  const [importingPath, setImportingPath] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setLoading(true);
    setQuery("");
    api
      .listClaudeSessions()
      .then(setItems)
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, [open]);

  // 过滤 + 按目录分组，组按组内最新时间倒序。
  const groups = useMemo(() => {
    const q = query.trim().toLowerCase();
    // 项目入口：只保留 cwd 精确等于该项目目录的会话。
    const scoped = projectFilter
      ? items.filter((i) => i.cwd === projectFilter.path)
      : items;
    const filtered = q
      ? scoped.filter(
          (i) =>
            i.title.toLowerCase().includes(q) ||
            i.cwd.toLowerCase().includes(q)
        )
      : scoped;
    const map = new Map<string, ClaudeSessionInfo[]>();
    for (const it of filtered) {
      const key = it.cwd || NO_DIR;
      const list = map.get(key);
      if (list) list.push(it);
      else map.set(key, [it]);
    }
    const latest = (list: ClaudeSessionInfo[]) =>
      Math.max(...list.map((x) => x.modified_ms));
    return [...map.entries()].sort((a, b) => latest(b[1]) - latest(a[1]));
  }, [items, query, projectFilter]);

  async function handleImport(it: ClaudeSessionInfo) {
    setImportingPath(it.path);
    try {
      // 项目入口：显式归属当前项目（project_id + 项目 workdir）；全局入口不传。
      const session = await api.importClaudeSession(
        it.path,
        projectFilter?.id ?? null,
        projectFilter?.path ?? null,
      );
      toast.success(`已导入「${session.title}」`);
      onOpenChange(false);
      onImported(session.id);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setImportingPath(null);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title={projectFilter ? `导入到「${projectFilter.name}」` : "从 Claude 导入"}
      description={
        projectFilter
          ? "只列出这个项目目录下的 Claude 对话，导入后归在本项目里。"
          : "选一段 Claude 里的对话搬进来，按项目分组。导入后可在这里接着聊。"
      }
      size="lg"
    >
      <div className="relative mb-3">
        <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索标题或目录"
          className="w-full h-9 pl-8 pr-3 rounded-lg border border-border bg-background text-sm outline-none focus:border-primary/50"
        />
      </div>

      {loading ? (
        <div className="py-12 text-center text-sm text-muted-foreground">
          <Loader2 className="inline h-4 w-4 animate-spin mr-1.5" />
          扫描中…
        </div>
      ) : groups.length === 0 ? (
        <div className="py-12 text-center text-sm text-muted-foreground">
          {projectFilter ? "这个项目目录下没有 Claude 对话" : "没有可导入的会话"}
        </div>
      ) : (
        <div className="space-y-4 max-h-[55vh] overflow-y-auto pr-1">
          {groups.map(([cwd, list]) => (
            <div key={cwd}>
              <div
                className="text-xs font-medium text-muted-foreground mb-1.5 truncate"
                title={cwd}
              >
                {cwd === NO_DIR ? cwd : pathLeaf(cwd)}
                <span className="text-muted-foreground/50"> · {list.length}</span>
              </div>
              <div className="space-y-1">
                {list.map((it) => (
                  <button
                    key={it.path}
                    onClick={() => handleImport(it)}
                    disabled={importingPath !== null}
                    className="w-full text-left rounded-lg border border-border px-3 py-2 hover:bg-accent/50 disabled:opacity-50 flex items-center gap-2"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="text-sm truncate">{it.title}</div>
                      <div className="text-[11px] text-muted-foreground">
                        {it.message_count} 条 · {formatTime(it.modified_ms)}
                      </div>
                    </div>
                    {importingPath === it.path ? (
                      <Loader2 className="h-4 w-4 animate-spin shrink-0" />
                    ) : (
                      <Download className="h-4 w-4 text-muted-foreground shrink-0" />
                    )}
                  </button>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </Dialog>
  );
}
