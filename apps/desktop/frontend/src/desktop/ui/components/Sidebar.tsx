import {
  MessageSquarePlus,
  Settings,
  Server,
  Moon,
  Sun,
  Trash2,
  Edit3,
  Sparkles,
  Search,
  X,
  CaseSensitive,
  Command,
  Regex,
  Terminal,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/desktop/ui/components/ui/button";
import { LoopingWebm } from "@/desktop/ui/components/LoopingWebm";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn, formatTime } from "@/desktop/ui/lib/utils";
import {
  isGlobalSearchShortcut,
  isNewConversationShortcut,
} from "@/desktop/ui/lib/keyboardShortcuts";
import {
  findSearchMatches,
  splitHighlightedText,
} from "@/desktop/ui/lib/searchHighlight";
import type { SessionMeta } from "@/desktop/ui/types";
import { animations } from "@/assets/animations";

type GroupKey = "today" | "yesterday" | "last7" | "last30" | "older";

function groupOf(updatedAt: number): GroupKey {
  const now = new Date();
  const d = new Date(updatedAt);
  const sameDay = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();
  if (sameDay(d, now)) return "today";
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  if (sameDay(d, y)) return "yesterday";
  const diff = (now.getTime() - d.getTime()) / 86400000;
  if (diff < 7) return "last7";
  if (diff < 30) return "last30";
  return "older";
}

const GROUP_LABEL: Record<GroupKey, string> = {
  today: "今天",
  yesterday: "昨天",
  last7: "过去 7 天",
  last30: "过去 30 天",
  older: "更早",
};
const GROUP_ORDER: GroupKey[] = ["today", "yesterday", "last7", "last30", "older"];

export function Sidebar() {
  const {
    sessions,
    currentSession,
    searchQuery,
    searchResults,
    searchCaseSensitive,
    searchRegex,
    searching,
    runSearch,
    clearSearch,
    openSession,
    deleteSession,
    renameSession,
    regenerateTitle,
    setProviderDialogOpen,
    setAppSettingsOpen,
    newSession,
    toggleTheme,
    theme,
  } = useStore();

  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameText, setRenameText] = useState("");
  const [regeneratingId, setRegeneratingId] = useState<string | null>(null);
  const [creatingSession, setCreatingSession] = useState(false);
  const [query, setQuery] = useState(searchQuery);
  const debounceRef = useRef<number | null>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (debounceRef.current) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => {
      runSearch(query, searchCaseSensitive).catch((e) =>
        toast.error(e.message || String(e))
      );
    }, 180);
    return () => {
      if (debounceRef.current) window.clearTimeout(debounceRef.current);
    };
  }, [query, searchCaseSensitive, searchRegex, runSearch]);

  async function commitRename(id: string) {
    const t = renameText.trim();
    if (t) {
      try {
        await renameSession(id, t);
      } catch (e: any) {
        toast.error(e.message || String(e));
      }
    }
    setRenamingId(null);
  }

  async function handleRegenerateTitle(id: string) {
    setRegeneratingId(id);
    try {
      await regenerateTitle();
      toast.success("已重新生成标题");
    } catch (e: any) {
      toast.error(e.message || String(e));
    } finally {
      setRegeneratingId(null);
    }
  }

  const handleCreateSession = useCallback(async () => {
    if (creatingSession) return;
    setCreatingSession(true);
    try {
      await newSession();
    } catch (e: any) {
      toast.error(e.message || String(e));
    } finally {
      setCreatingSession(false);
    }
  }, [creatingSession, newSession]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (isNewConversationShortcut(e)) {
        e.preventDefault();
        handleCreateSession();
        return;
      }

      if (isGlobalSearchShortcut(e)) {
        e.preventDefault();
        searchInputRef.current?.focus();
        searchInputRef.current?.select();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [handleCreateSession]);

  // 显示源：搜索命中或全量会话
  const displayItems: (SessionMeta & { snippet?: string | null })[] = useMemo(() => {
    if (searchResults) return searchResults;
    return sessions;
  }, [searchResults, sessions]);

  const grouped = useMemo(() => {
    const g: Record<GroupKey, typeof displayItems> = {
      today: [],
      yesterday: [],
      last7: [],
      last30: [],
      older: [],
    };
    for (const s of displayItems) g[groupOf(s.updated_at)].push(s);
    return g;
  }, [displayItems]);

  function renderSearchText(text: string, keyPrefix: string) {
    if (!searchResults || !query.trim()) return text;

    const segments = splitHighlightedText(
      text,
      findSearchMatches(text, query, searchCaseSensitive, searchRegex)
    );
    if (!segments.some((segment) => segment.highlighted)) return text;

    return segments.map((segment, index) =>
      segment.highlighted ? (
        <mark
          key={`${keyPrefix}-${index}`}
          className="rounded-sm bg-amber-300 px-0.5 text-black"
        >
          {segment.text}
        </mark>
      ) : (
        <span key={`${keyPrefix}-${index}`}>{segment.text}</span>
      )
    );
  }

  return (
    <aside className="w-64 shrink-0 flex flex-col border-r border-border bg-card/30">
      <div
        className="h-16 px-5 pt-8 flex items-start border-b border-border drag-region"
        data-tauri-drag-region
      >
        <div className="flex items-center gap-2 pointer-events-none">
          <LoopingWebm
            src={animations.brandMark}
            className="h-7 w-7 rounded-md shadow-sm"
          />
          <span className="text-sm font-semibold">Hebbian</span>
        </div>
      </div>

      <div className="px-3 py-3 no-drag">
        <Button
          onClick={handleCreateSession}
          className="w-full justify-between"
          size="md"
          disabled={creatingSession}
        >
          <span className="inline-flex items-center gap-1.5">
            <MessageSquarePlus className="w-4 h-4" />
            新建对话
          </span>
          <span className="ml-auto inline-flex items-center gap-0.5 text-[10px] font-medium text-primary-foreground/70">
            <Command className="h-3 w-3" />
            N
          </span>
        </Button>
      </div>

      {/* 搜索框 */}
      <div className="px-3 pb-2 no-drag">
        <div className="relative flex items-center">
          <Search className="w-3.5 h-3.5 absolute left-2.5 text-muted-foreground pointer-events-none" />
          <input
            ref={searchInputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="全局搜索标题 / 内容"
            spellCheck={false}
            autoCorrect="off"
            className="h-8 w-full rounded-md border border-input bg-background pl-8 pr-[5.75rem] text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
          />
          <div className="absolute right-1 flex items-center">
            <button
              onClick={() =>
                runSearch(query, searchCaseSensitive, !searchRegex).catch(() => {})
              }
              className={cn(
                "h-6 w-6 inline-flex items-center justify-center rounded text-muted-foreground hover:bg-accent",
                searchRegex && "bg-primary/20 text-primary"
              )}
              title={`正则表达式：${searchRegex ? "开" : "关"}`}
            >
              <Regex className="w-3.5 h-3.5" />
            </button>
            <button
              onClick={() =>
                runSearch(query, !searchCaseSensitive, searchRegex).catch(() => {})
              }
              className={cn(
                "h-6 w-6 inline-flex items-center justify-center rounded text-muted-foreground hover:bg-accent",
                searchCaseSensitive && "bg-primary/20 text-primary"
              )}
              title={`区分大小写：${searchCaseSensitive ? "开" : "关"}`}
            >
              <CaseSensitive className="w-3.5 h-3.5" />
            </button>
            {query && (
              <button
                onClick={() => {
                  setQuery("");
                  clearSearch();
                }}
                className="h-6 w-6 inline-flex items-center justify-center rounded text-muted-foreground hover:bg-accent"
                title="清除"
              >
                <X className="w-3.5 h-3.5" />
              </button>
            )}
          </div>
        </div>
        {searching && (
          <div className="text-[11px] text-muted-foreground mt-1 px-0.5">
            搜索中…
          </div>
        )}
        {searchResults && !searching && (
          <div className="text-[11px] text-muted-foreground mt-1 px-0.5">
            命中 {searchResults.length} 条
          </div>
        )}
      </div>

      <div className="flex-1 overflow-y-auto px-2 pb-2 no-drag">
        {displayItems.length === 0 && (
          <div className="text-center text-xs text-muted-foreground py-10 px-4">
            {searchResults
              ? "无匹配结果"
              : "暂无对话，点击上方按钮创建"}
          </div>
        )}
        {GROUP_ORDER.map((key) => {
          const items = grouped[key];
          if (items.length === 0) return null;
          return (
            <div key={key} className="mb-2">
              <div className="px-2 py-1 text-[10px] font-semibold text-muted-foreground/80 uppercase tracking-wider">
                {GROUP_LABEL[key]}
              </div>
              <ul className="space-y-0.5">
                {items.map((s) => {
                  const active = currentSession?.id === s.id;
                  const regenerating = regeneratingId === s.id;
                  const snippet = (s as any).snippet as string | undefined;
                  return (
                    <li key={s.id}>
                      <div
                        onClick={() => openSession(s.id)}
                        className={cn(
                          "group px-3 py-2 rounded-md cursor-pointer transition-colors",
                          active
                            ? "bg-accent text-accent-foreground"
                            : "hover:bg-accent/50"
                        )}
                      >
                        <div className="flex items-center justify-between gap-2">
                          {renamingId === s.id ? (
                            <input
                              autoFocus
                              spellCheck={false}
                              autoCorrect="off"
                              value={renameText}
                              onChange={(e) => setRenameText(e.target.value)}
                              onBlur={() => commitRename(s.id)}
                              onKeyDown={(e) => {
                                if (e.key === "Enter") commitRename(s.id);
                                if (e.key === "Escape") setRenamingId(null);
                              }}
                              className="flex-1 text-sm bg-background border border-input rounded px-1.5 py-0.5 outline-none focus-visible:ring-2 focus-visible:ring-ring"
                            />
                          ) : (
                            <span
                              className="text-sm truncate flex-1"
                              title={s.title}
                            >
                              {renderSearchText(s.title, `${s.id}-title`)}
                            </span>
                          )}
                          {!renamingId && (
                            <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
                              {active && (
                                <button
                                  onClick={(e) => {
                                    e.stopPropagation();
                                    handleRegenerateTitle(s.id);
                                  }}
                                  disabled={regenerating}
                                  className="p-1 rounded hover:bg-background text-muted-foreground disabled:opacity-50"
                                  title="用模型重新生成标题"
                                >
                                  <Sparkles
                                    className={cn(
                                      "w-3.5 h-3.5",
                                      regenerating && "animate-pulse text-primary"
                                    )}
                                  />
                                </button>
                              )}
                              <button
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setRenamingId(s.id);
                                  setRenameText(s.title);
                                }}
                                className="p-1 rounded hover:bg-background text-muted-foreground"
                                title="重命名"
                              >
                                <Edit3 className="w-3.5 h-3.5" />
                              </button>
                              <button
                                onClick={(e) => {
                                  e.stopPropagation();
                                  if (confirm(`删除对话 "${s.title}"？`)) {
                                    deleteSession(s.id).catch((err) =>
                                      toast.error(err.message || String(err))
                                    );
                                  }
                                }}
                                className="p-1 rounded hover:bg-background text-muted-foreground hover:text-destructive"
                                title="删除"
                              >
                                <Trash2 className="w-3.5 h-3.5" />
                              </button>
                            </div>
                          )}
                        </div>
                        <div className="flex items-center gap-2 mt-0.5 text-[11px] text-muted-foreground">
                          <span className="truncate">{s.model}</span>
                          {s.source === "cli" && (
                            <span
                              className="inline-flex items-center gap-0.5 px-1 py-0 rounded text-[10px] font-medium uppercase tracking-wide bg-primary/10 text-primary border border-primary/20 shrink-0"
                              title="本对话由 hebbian-cli 创建"
                            >
                              <Terminal className="w-2.5 h-2.5" />
                              CLI
                            </span>
                          )}
                          <span className="ml-auto shrink-0">
                            {formatTime(s.updated_at)}
                          </span>
                        </div>
                        {snippet && (
                          <div className="text-[11px] text-muted-foreground/80 mt-1 line-clamp-2">
                            {renderSearchText(snippet, `${s.id}-snippet`)}
                          </div>
                        )}
                      </div>
                    </li>
                  );
                })}
              </ul>
            </div>
          );
        })}
      </div>

      <div className="border-t border-border p-2 flex items-center gap-1 no-drag">
        <Button
          variant="ghost"
          size="icon"
          onClick={() => setProviderDialogOpen(true)}
          title="供应商配置"
        >
          <Server className="w-4 h-4" />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          onClick={() => setAppSettingsOpen(true)}
          title="设置（通用 / 对话 / Agent）"
        >
          <Settings className="w-4 h-4" />
        </Button>
        <div className="flex-1" />
        <Button
          variant="ghost"
          size="icon"
          onClick={toggleTheme}
          title="切换主题"
        >
          {theme === "dark" ? <Sun className="w-4 h-4" /> : <Moon className="w-4 h-4" />}
        </Button>
      </div>
    </aside>
  );
}
