import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  ArrowLeft,
  Clock,
  Download,
  FileText,
  Hash,
  Loader2,
  MessageSquare,
  Search,
} from "lucide-react";
import { Dialog } from "./ui/dialog";
import { MessageBubble } from "./MessageBubble";
import { api, type ClaudeSessionInfo, type ClaudeSessionPreview } from "@/desktop/bridge/tauri";
import { formatTime, pathLeaf } from "@/desktop/ui/lib/utils";
import type { Message } from "@/desktop/ui/types";

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
 *
 * 列表项点击 → 弹出预览弹窗（MessageBubble 渲染整段对话），
 * 预览弹窗里提供「导入」按钮。
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

  // 预览态
  const [previewSession, setPreviewSession] = useState<ClaudeSessionInfo | null>(null);
  const [preview, setPreview] = useState<ClaudeSessionPreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [importing, setImporting] = useState(false);

  // 打开时加载会话列表
  useEffect(() => {
    if (!open) return;
    setLoading(true);
    setQuery("");
    setPreviewSession(null);
    setPreview(null);
    api
      .listClaudeSessions()
      .then(setItems)
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, [open]);

  // 过滤 + 按目录分组，组按组内最新时间倒序。
  // 搜索匹配标题 + UUID。
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
            i.uuid.toLowerCase().includes(q)
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

  // 加载预览
  async function handlePreview(session: ClaudeSessionInfo) {
    setPreviewSession(session);
    setPreview(null);
    setPreviewLoading(true);
    try {
      const data = await api.readClaudeSessionPreview(session.path);
      setPreview(data);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
      setPreviewSession(null);
    } finally {
      setPreviewLoading(false);
    }
  }

  // 导入
  async function handleImport(session: ClaudeSessionInfo) {
    setImporting(true);
    try {
      // 项目入口：显式归属当前项目（project_id + 项目 workdir）；全局入口不传。
      const result = await api.importClaudeSession(
        session.path,
        projectFilter?.id ?? null,
        projectFilter?.path ?? null,
      );
      toast.success(`已导入「${result.title}」`);
      onOpenChange(false);
      onImported(result.id);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setImporting(false);
    }
  }

  function backToList() {
    setPreviewSession(null);
    setPreview(null);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        onOpenChange(v);
        if (!v) { setPreviewSession(null); setPreview(null); }
      }}
      title={
        previewSession
          ? previewSession.title
          : projectFilter
            ? `导入到「${projectFilter.name}」`
            : "从 Claude 导入"
      }
      description={
        previewSession
          ? `UUID: ${previewSession.uuid} · ${previewSession.message_count} 条消息`
          : projectFilter
            ? "只列出这个项目目录下的 Claude 对话，导入后归在本项目里。"
            : "选一段 Claude 里的对话搬进来，按项目分组。点击预览内容，满意了再导入。"
      }
      size={previewSession ? "2xl" : "lg"}
    >
      {previewSession ? (
        /* ── 预览视图 ── */
        <div className="flex flex-col h-full min-h-[40vh]">
          {/* 顶栏：返回 + 导入 */}
          <div className="flex items-center gap-2 pb-3 border-b border-border mb-3 shrink-0">
            <button
              type="button"
              onClick={backToList}
              className="inline-flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground px-2 py-1 rounded-md hover:bg-accent/50"
            >
              <ArrowLeft className="w-3.5 h-3.5" />
              返回列表
            </button>
            <div className="flex-1" />
            <button
              type="button"
              onClick={() => handleImport(previewSession)}
              disabled={importing}
              className="inline-flex items-center gap-1.5 text-xs font-medium px-3 py-1.5 rounded-lg bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            >
              {importing ? (
                <Loader2 className="w-3.5 h-3.5 animate-spin" />
              ) : (
                <Download className="w-3.5 h-3.5" />
              )}
              导入此对话
            </button>
          </div>

          {/* 预览内容：消息列表 */}
          {previewLoading ? (
            <div className="py-12 text-center text-sm text-muted-foreground">
              <Loader2 className="inline h-4 w-4 animate-spin mr-1.5" />
              加载中…
            </div>
          ) : !preview ? (
            <div className="py-12 text-center text-sm text-muted-foreground">
              无法加载对话内容
            </div>
          ) : (
            <div className="flex-1 overflow-y-auto space-y-3 pr-1 min-h-0">
              {preview.messages.length === 0 ? (
                <div className="py-12 text-center text-sm text-muted-foreground">
                  对话内容为空
                </div>
              ) : (
                preview.messages.map((msg: Message) => (
                  <div key={msg.id} className="px-0.5">
                    <MessageBubble
                      message={msg}
                      userAvatar={undefined}
                    />
                  </div>
                ))
              )}
            </div>
          )}
        </div>
      ) : (
        /* ── 列表视图 ── */
        <>
          <div className="relative mb-3">
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索标题或 UUID…"
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
              {projectFilter
                ? "这个项目目录下没有 Claude 对话"
                : query
                  ? "没有匹配的对话"
                  : "没有可导入的会话"}
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
                        onClick={() => handlePreview(it)}
                        className="w-full text-left rounded-lg border border-border px-3 py-2 hover:bg-accent/50 flex items-center gap-2"
                      >
                        <MessageSquare className="w-4 h-4 text-muted-foreground shrink-0" />
                        <div className="min-w-0 flex-1">
                          <div className="text-sm truncate">{it.title}</div>
                          <div className="flex items-center gap-2 text-[11px] text-muted-foreground mt-0.5">
                            <span className="inline-flex items-center gap-1">
                              <Hash className="w-3 h-3" />
                              <code className="text-[10px] bg-muted/50 px-1 rounded">{it.uuid}</code>
                            </span>
                            <span>·</span>
                            <span className="inline-flex items-center gap-1">
                              <MessageSquare className="w-3 h-3" />
                              {it.message_count} 条
                            </span>
                            <span>·</span>
                            <span className="inline-flex items-center gap-1">
                              <Clock className="w-3 h-3" />
                              {formatTime(it.modified_ms)}
                            </span>
                          </div>
                        </div>
                      </button>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </Dialog>
  );
}
