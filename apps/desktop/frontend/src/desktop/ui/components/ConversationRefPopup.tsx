import { useEffect, useMemo, useRef, useState } from "react";
import { Loader2, MessageSquare, MessageSquareCode } from "lucide-react";
import { api, type ClaudeSessionInfo } from "@/desktop/bridge/tauri";
import { cn, formatTime } from "@/desktop/ui/lib/utils";
import type { SessionMeta } from "@/desktop/ui/types";

/** 统一的对话条目（来源不同但选择结果都是路径）。 */
interface ConversationItem {
  id: string;
  title: string;
  source: "hebbian" | "claude";
  path: string;
  updated_ms: number;
  message_count: number;
}

interface Props {
  /** 用户正在输入的 @ 后缀，用于实时过滤。 */
  query: string;
  /** 选中一条对话后回调，返回路径。 */
  onPick: (item: ConversationItem) => void;
  /** 关闭弹窗（Escape / 点击外部）。 */
  onClose: () => void;
  /** 键盘导航 active index（由父组件控制）。 */
  activeIndex: number;
  onActiveIndexChange: (index: number) => void;
  className?: string;
}

export function ConversationRefPopup({
  query,
  onPick,
  onClose,
  activeIndex,
  onActiveIndexChange,
  className,
}: Props) {
  const [hebbianSessions, setHebbianSessions] = useState<SessionMeta[]>([]);
  const [claudeSessions, setClaudeSessions] = useState<ClaudeSessionInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    Promise.all([
      api.listSessions().catch(() => [] as SessionMeta[]),
      api.listClaudeSessions().catch(() => [] as ClaudeSessionInfo[]),
    ]).then(([heb, claude]) => {
      if (cancelled) return;
      setHebbianSessions(heb);
      setClaudeSessions(claude);
      setLoading(false);
    });
    return () => { cancelled = true; };
  }, []);

  const items = useMemo(() => {
    const all: ConversationItem[] = [];
    for (const s of hebbianSessions) {
      if (!s.path) continue;
      all.push({
        id: `heb:${s.id}`,
        title: s.title,
        source: "hebbian",
        path: s.path,
        updated_ms: s.updated_at,
        message_count: s.message_count,
      });
    }
    for (const s of claudeSessions) {
      all.push({
        id: `cc:${s.uuid}`,
        title: s.title,
        source: "claude",
        path: s.path,
        updated_ms: s.modified_ms,
        message_count: s.message_count,
      });
    }
    all.sort((a, b) => b.updated_ms - a.updated_ms);

    if (!query) return all;
    const q = query.toLowerCase();
    return all.filter(
      (it) =>
        it.title.toLowerCase().includes(q) ||
        it.path.toLowerCase().includes(q),
    );
  }, [hebbianSessions, claudeSessions, query]);

  // 滚动 active item 到可视区域
  useEffect(() => {
    const container = listRef.current;
    if (!container || items.length === 0) return;
    const el = container.children[activeIndex] as HTMLElement | undefined;
    el?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, items.length]);

  // 外部 Enter 按键通知：ChatInput onKeyDown 中 dispatch 自定义事件
  useEffect(() => {
    function handlePick() {
      const clamped = Math.min(activeIndex, items.length - 1);
      if (clamped >= 0 && items[clamped]) {
        onPick(items[clamped]);
      }
    }
    document.addEventListener("conversation-ref-pick-active", handlePick);
    return () =>
      document.removeEventListener("conversation-ref-pick-active", handlePick);
  }, [activeIndex, items, onPick]);

  return (
    <div
      ref={listRef}
      className={cn(
        "absolute bottom-full left-0 right-0 mb-1 max-h-[40vh] overflow-y-auto rounded-lg border border-border bg-card shadow-lg z-[100]",
        className,
      )}
    >
      {loading ? (
        <div className="flex items-center justify-center py-6 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin mr-1.5" />
          加载对话列表…
        </div>
      ) : items.length === 0 ? (
        <div className="py-6 text-center text-sm text-muted-foreground">
          没有匹配的对话
        </div>
      ) : (
        items.map((it, i) => (
          <button
            key={it.id}
            type="button"
            onMouseDown={(e) => {
              e.preventDefault();
              onPick(it);
            }}
            onMouseEnter={() => onActiveIndexChange(i)}
            className={cn(
              "w-full flex items-center gap-2.5 px-3 py-1.5 text-sm text-left",
              i === activeIndex ? "bg-accent" : "hover:bg-accent/50",
            )}
          >
            {it.source === "claude" ? (
              <MessageSquareCode className="w-4 h-4 text-orange-400 shrink-0" />
            ) : (
              <MessageSquare className="w-4 h-4 text-primary shrink-0" />
            )}
            <div className="min-w-0 flex-1">
              <div className="truncate text-foreground">{it.title}</div>
              <div className="text-[11px] text-muted-foreground truncate">
                {it.message_count} 条 · {formatTime(it.updated_ms)}
              </div>
            </div>
            <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
              {it.source === "claude" ? "Claude" : "Hebbian"}
            </span>
          </button>
        ))
      )}
    </div>
  );
}

export type { ConversationItem };
