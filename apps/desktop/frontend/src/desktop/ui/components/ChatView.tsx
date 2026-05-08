import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Sparkles, ChevronDown } from "lucide-react";
import { MessageBubble } from "./MessageBubble";
import { ChatInput } from "./ChatInput";
import { PermissionApprovalPopup } from "./PermissionApprovalPopup";
import { UserQuestionPopup } from "./UserQuestionPopup";
import { FindBar, findMatches, useFindController } from "./FindBar";
import { useStore } from "@/desktop/ui/store/useStore";
import { Button } from "@/desktop/ui/components/ui/button";
import { cn, hasSessionStarted } from "@/desktop/ui/lib/utils";
import { isLocalFindShortcut } from "@/desktop/ui/lib/keyboardShortcuts";
import type { MessageAttachment, Provider } from "@/desktop/ui/types";

function isProviderEnabled(provider: Provider) {
  return provider.enabled !== false;
}

export function ChatView() {
  const {
    currentSession,
    providersFile,
    promptsFile,
    prompts,
    userAvatar,
    streamingMessageId,
    streamingText,
    streamingParts,
    sendUserMessage,
    cancelStreaming,
    forkSession,
    regenerateFrom,
    regenerateTitle,
    setProviderDialogOpen,
    setSettingsOpen,
    switchProviderModel,
    newSession,
    pendingPromptId,
    setPendingPromptId,
    updateCurrentConfig,
  } = useStore();

  const scrollRef = useRef<HTMLDivElement>(null);
  const [titleLoading, setTitleLoading] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [expandedProviderIds, setExpandedProviderIds] = useState<Set<string>>(
    () => new Set()
  );

  // ==== 对话内查找状态 ====
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findRegex, setFindRegex] = useState(false);
  const [findCase, setFindCase] = useState(false);

  // 计算每条消息的匹配区间（只对非 marker 生效）
  const matchesPerMessage = useMemo(() => {
    if (!currentSession || !findOpen || !findQuery) return [];
    return currentSession.messages.map((m) =>
      m.role === "marker"
        ? ([] as Array<[number, number]>)
        : findMatches(m.content, findQuery, findRegex, findCase)
    );
  }, [currentSession, findOpen, findQuery, findRegex, findCase]);

  const totalMatches = useMemo(
    () => matchesPerMessage.reduce((s, a) => s + a.length, 0),
    [matchesPerMessage]
  );

  const { active, setActive, next, prev } = useFindController(totalMatches);

  // 全局 active 映射到 (msgIdx, localIdx)
  const activeLocation = useMemo(() => {
    if (totalMatches === 0) return null;
    let running = 0;
    for (let i = 0; i < matchesPerMessage.length; i++) {
      const n = matchesPerMessage[i].length;
      if (active < running + n) return { msgIdx: i, localIdx: active - running };
      running += n;
    }
    return null;
  }, [matchesPerMessage, active, totalMatches]);

  // 切换对话时自动关闭查找
  useEffect(() => {
    setFindOpen(false);
    setFindQuery("");
    setActive(0);
  }, [currentSession?.id, setActive]);

  useEffect(() => {
    if (!scrollRef.current) return;
    scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [currentSession?.messages.length, streamingText, streamingParts]);

  useEffect(() => {
    if (!pickerOpen) return;
    const onClick = () => setPickerOpen(false);
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [pickerOpen]);

  // Cmd/Ctrl+F 拉起查找
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (isLocalFindShortcut(e) && currentSession) {
        e.preventDefault();
        setFindOpen(true);
        return;
      }
      if (findOpen && e.key === "Escape") {
        e.preventDefault();
        setFindOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [findOpen, currentSession]);

  // 跳转到当前 active 的高亮元素
  useLayoutEffect(() => {
    if (!findOpen || totalMatches === 0) return;
    const el = scrollRef.current?.querySelector<HTMLElement>(
      'mark[data-active="true"]'
    );
    if (el) el.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [findOpen, active, totalMatches]);

  if (!currentSession) {
    return (
      <div
        className="flex-1 flex flex-col items-center justify-center text-center px-6 drag-region"
        data-tauri-drag-region
      >
        <div className="h-14 w-14 rounded-2xl bg-gradient-to-br from-sky-500 to-indigo-600 flex items-center justify-center text-white shadow-lg mb-4 no-drag">
          <Sparkles className="w-7 h-7" />
        </div>
        <h2 className="text-lg font-semibold no-drag">开始一场新的对话</h2>
        <p className="text-sm text-muted-foreground mt-1 max-w-sm no-drag">
          在左侧点击 <b>新建对话</b>，或先前往供应商配置添加你的 API Key。
        </p>
        <div className="mt-5 flex items-center gap-2 no-drag">
          <Button
            onClick={() => {
              newSession().catch((e) => {
                toast.error(e.message || String(e));
              });
            }}
          >
            新建对话
          </Button>
          <Button variant="outline" onClick={() => setProviderDialogOpen(true)}>
            供应商配置
          </Button>
        </div>
      </div>
    );
  }

  const activePrompt = currentSession.prompt_id
    ? prompts.find((p) => p.id === currentSession.prompt_id)
    : undefined;
  const sessionStarted = hasSessionStarted(currentSession);
  const promptSelectionUnlocked = !sessionStarted;
  const fallbackPromptId = pendingPromptId || promptsFile.default_prompt_id || "";
  const editablePromptId = currentSession.prompt_id ?? fallbackPromptId;
  const normalizedPromptId =
    editablePromptId && prompts.some((p) => p.id === editablePromptId)
      ? editablePromptId
      : "";
  const promptSummary = activePrompt?.name ?? "无 Agent";
  const isStreaming = !!streamingMessageId;
  const providers = providersFile.providers;
  const enabledProviders = providers.filter(isProviderEnabled);
  const currentProvider = providers.find(
    (p) => p.id === currentSession.provider_id
  );
  const userMessageHistory = currentSession.messages
    .filter((m) => m.role === "user")
    .map((m) => m.content);

  async function handleSend(content: string, attachments: MessageAttachment[]) {
    try {
      await sendUserMessage(content, attachments);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  async function handleCancel() {
    try {
      await cancelStreaming();
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }
  async function handleFork(msgId: string) {
    try {
      await forkSession(msgId);
      toast.success("已创建分支");
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }
  async function handleRegenerate(msgId: string) {
    if (isStreaming) return;
    try {
      await regenerateFrom(msgId);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }
  async function handleRegenTitle() {
    setTitleLoading(true);
    try {
      await regenerateTitle();
      toast.success("标题已更新");
    } catch (e: any) {
      toast.error(e.message || String(e));
    } finally {
      setTitleLoading(false);
    }
  }
  async function handleSwitch(providerId: string, model: string) {
    try {
      await switchProviderModel(providerId, model);
      setPickerOpen(false);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  async function handlePromptChange(nextPromptId: string) {
    setPendingPromptId(nextPromptId);
    if (sessionStarted) return;

    const nextPrompt = prompts.find((p) => p.id === nextPromptId);
    try {
      await updateCurrentConfig({
        prompt_id: nextPromptId,
        system_prompt: nextPrompt?.content ?? "",
      });
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  return (
    <div className="flex-1 flex flex-col min-w-0 h-full relative">
      <header
        className="relative z-50 h-14 shrink-0 pl-4 pr-4 flex items-center justify-between border-b border-border bg-background/80 backdrop-blur-md drag-region"
        data-tauri-drag-region
      >
        <div className="flex items-center gap-2 min-w-0">
          <div className="flex items-center gap-2 min-w-0">
            <h1
              className="text-sm font-medium truncate max-w-[260px] drag-region"
              title={currentSession.title}
              data-tauri-drag-region
            >
              {currentSession.title}
            </h1>
            <button
              onClick={handleRegenTitle}
              disabled={titleLoading}
              className="p-1 rounded hover:bg-accent text-muted-foreground disabled:opacity-50 no-drag"
              title="用模型重新生成标题"
            >
              <Sparkles
                className={cn(
                  "w-3.5 h-3.5",
                  titleLoading && "animate-pulse text-primary"
                )}
              />
            </button>
            {promptSelectionUnlocked ? (
              prompts.length > 0 ? (
                <div className="relative min-w-[140px] max-w-[220px] rounded-md px-1 hover:bg-accent/40 transition-colors no-drag">
                  <select
                    value={normalizedPromptId}
                    onChange={(e) => handlePromptChange(e.target.value)}
                    className="h-7 w-full appearance-none bg-transparent border-0 px-1 pr-5 text-[12px] text-muted-foreground outline-none cursor-pointer no-drag"
                    aria-label="选择 Agent"
                  >
                    <option value="">无 Agent</option>
                    {prompts.map((prompt) => (
                      <option key={prompt.id} value={prompt.id}>
                        {prompt.name}
                      </option>
                    ))}
                  </select>
                  <ChevronDown className="pointer-events-none absolute right-1.5 top-1/2 h-3 w-3 -translate-y-1/2 text-muted-foreground/70" />
                </div>
              ) : (
                <span
                  className="text-[11px] text-muted-foreground drag-region"
                  data-tauri-drag-region
                >
                  无 Agent
                </span>
              )
            ) : (
              <span
                className="max-w-[220px] truncate text-[11px] text-muted-foreground drag-region"
                title={promptSummary}
                data-tauri-drag-region
              >
                {promptSummary}
              </span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-2 no-drag relative">
          <button
            onClick={(e) => {
              e.stopPropagation();
              setPickerOpen((open) => {
                const nextOpen = !open;
                if (nextOpen) {
                  const firstProviderId = enabledProviders[0]?.id;
                  setExpandedProviderIds((ids) => {
                    if (!firstProviderId || ids.has(firstProviderId)) return ids;
                    return new Set([...ids, firstProviderId]);
                  });
                }
                return nextOpen;
              });
            }}
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background hover:bg-accent px-2.5 py-1 text-xs no-drag"
          >
            <span className="text-muted-foreground">
              {currentProvider?.name ?? "未知"} ·
            </span>
            <span className="font-medium">{currentSession.model}</span>
            <ChevronDown className="w-3 h-3 opacity-60" />
          </button>
          {pickerOpen && (
            <div
              onClick={(e) => e.stopPropagation()}
              className="absolute top-full right-0 mt-1 w-72 max-h-[60vh] overflow-y-auto rounded-lg border border-border bg-card shadow-lg z-[90] animate-slide-up"
            >
              {enabledProviders.length === 0 && (
                <div className="p-4 text-xs text-muted-foreground text-center">
                  没有已启用的供应商
                </div>
              )}
              {enabledProviders.map((p) => {
                const isActiveProvider = p.id === currentSession.provider_id;
                const expanded = expandedProviderIds.has(p.id);
                const models =
                  p.models.length > 0
                    ? p.models
                    : p.default_model
                    ? [p.default_model]
                    : [];
                return (
                  <div
                    key={p.id}
                    className="border-b border-border last:border-b-0"
                  >
                    <button
                      type="button"
                      onClick={() =>
                        setExpandedProviderIds((ids) => {
                          const next = new Set(ids);
                          if (next.has(p.id)) next.delete(p.id);
                          else next.add(p.id);
                          return next;
                        })
                      }
                      className="w-full px-3 py-1.5 text-[11px] font-semibold text-foreground bg-muted hover:bg-accent flex items-center justify-between transition-colors"
                    >
                      <span>{p.name}</span>
                      <span className="inline-flex items-center gap-1 text-[10px] text-muted-foreground uppercase">
                        {p.kind}
                        <ChevronDown
                          className={cn(
                            "h-3 w-3 transition-transform",
                            !expanded && "-rotate-90"
                          )}
                        />
                      </span>
                    </button>
                    {expanded && models.length === 0 && (
                      <div className="px-3 py-2 text-xs text-muted-foreground italic">
                        （无模型）
                      </div>
                    )}
                    {expanded && models.length > 0 && (
                      <div>
                        {models.map((m) => {
                          const act =
                            isActiveProvider && m === currentSession.model;
                          return (
                            <button
                              key={`${p.id}-${m}`}
                              onClick={() => handleSwitch(p.id, m)}
                              className={cn(
                                "w-full text-left px-3 py-2 text-sm hover:bg-accent transition-colors flex items-center justify-between",
                                act && "bg-primary/10 text-primary"
                              )}
                            >
                              <span className="truncate">{m}</span>
                              {act && <span className="text-xs">✓</span>}
                            </button>
                          );
                        })}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
          <Button variant="ghost" size="sm" onClick={() => setSettingsOpen(true)}>
            对话设置
          </Button>
        </div>
      </header>

      <FindBar
        open={findOpen}
        onClose={() => setFindOpen(false)}
        state={{
          query: findQuery,
          regex: findRegex,
          caseSensitive: findCase,
          current: totalMatches === 0 ? 0 : active + 1,
          total: totalMatches,
        }}
        onChange={(patch) => {
          if (patch.query !== undefined) setFindQuery(patch.query);
          if (patch.regex !== undefined) setFindRegex(patch.regex);
          if (patch.caseSensitive !== undefined) setFindCase(patch.caseSensitive);
          setActive(0);
        }}
        onPrev={prev}
        onNext={next}
      />

      <div ref={scrollRef} className="flex-1 overflow-y-auto">
        {currentSession.messages.length === 0 && !isStreaming && (
          <div className="px-6 py-10 text-center text-sm text-muted-foreground">
            发送第一条消息开始对话
          </div>
        )}
        <div>
          {currentSession.messages.map((m, i) => (
            <MessageBubble
              key={m.id}
              message={m}
              prompt={activePrompt}
              userAvatar={userAvatar}
              onFork={handleFork}
              onRegenerate={m.role === "assistant" ? handleRegenerate : undefined}
              find={
                findOpen && findQuery
                  ? {
                      query: findQuery,
                      regex: findRegex,
                      caseSensitive: findCase,
                      activeLocalIdx:
                        activeLocation?.msgIdx === i
                          ? activeLocation!.localIdx
                          : null,
                      matchBaseIdx: 0,
                    }
                  : undefined
              }
            />
          ))}
          {isStreaming && (
            <MessageBubble
              streaming
              prompt={activePrompt}
              userAvatar={userAvatar}
              streamingParts={streamingParts}
              message={{
                id: "streaming",
                role: "assistant",
                content: streamingText,
                created_at: Date.now(),
              }}
            />
          )}
        </div>
      </div>

      <PermissionApprovalPopup />
      <UserQuestionPopup />

      <ChatInput
        onSend={handleSend}
        onCancel={handleCancel}
        isStreaming={isStreaming}
        userMessageHistory={userMessageHistory}
      />
    </div>
  );
}
