import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Sparkles, ChevronDown } from "lucide-react";
import { MessageBubble } from "./MessageBubble";
import { ChatInput } from "./ChatInput";
import { InputQueuePanel } from "./InputQueuePanel";
import { PermissionApprovalPopup } from "./PermissionApprovalPopup";
import { UserQuestionPopup } from "./UserQuestionPopup";
import { FindBar, findMatches, useFindController } from "./FindBar";
import { useStore } from "@/desktop/ui/store/useStore";
import { Button } from "@/desktop/ui/components/ui/button";
import { cn, hasSessionStarted } from "@/desktop/ui/lib/utils";
import { isLocalFindShortcut } from "@/desktop/ui/lib/keyboardShortcuts";
import type { MessageAttachment } from "@/desktop/ui/types";

export function ChatView() {
  const {
    currentSession,
    promptsFile,
    prompts,
    userAvatar,
    streamingMessageId,
    streamingText,
    streamingParts,
    injectedSinceStream,
    autoJudgedNotes,
    currentRunMode,
    sendUserMessage,
    cancelStreaming,
    forkSession,
    regenerateFrom,
    regenerateFromUser,
    editAndRerun,
    regenerateTitle,
    setProviderDialogOpen,
    setSettingsOpen,
    newSession,
    pendingPromptId,
    setPendingPromptId,
    updateCurrentConfig,
  } = useStore();

  const scrollRef = useRef<HTMLDivElement>(null);
  const [titleLoading, setTitleLoading] = useState(false);

  // ==== 压缩分隔条：摘要展开 / 历史对话展开 两套独立状态 ====
  // - expandedSummaries：分隔条主体点击后展开摘要正文，用来评估压缩质量
  // - expandedHistories：「历史对话」按钮点击后展开压缩前的原始消息
  const [expandedSummaries, setExpandedSummaries] = useState<Set<string>>(
    () => new Set()
  );
  const [expandedHistories, setExpandedHistories] = useState<Set<string>>(
    () => new Set()
  );
  function toggleIn(
    setter: React.Dispatch<React.SetStateAction<Set<string>>>,
    id: string
  ) {
    setter((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

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
  const userMessageHistory = currentSession.messages
    .filter((m) => m.role === "user")
    .map((m) => m.content);

  // 每条 compact_boundary 之前的消息默认折叠：模型已不再读，需要点击分隔条展开。
  // 多次压缩时每条 boundary 独立展开/折叠。
  const boundaryIndices: number[] = [];
  currentSession.messages.forEach((m, i) => {
    if (m.meta?.type === "compact_boundary") boundaryIndices.push(i);
  });
  const lastCompactBoundaryIdx =
    boundaryIndices.length > 0
      ? boundaryIndices[boundaryIndices.length - 1]
      : -1;

  // 找到指定消息归属的 boundary id（它之后最近的一个 boundary 的 message id）。
  // 没有则返回 null（属于最后一段，不会被折叠）。
  function nextBoundaryId(idx: number): string | null {
    const next = boundaryIndices.find((b) => b > idx);
    return next === undefined ? null : currentSession!.messages[next].id;
  }

  // 每条 boundary 折叠了多少条历史消息（含 marker 之前同段所有非 marker 消息）。
  const boundaryArchivedCounts: Record<string, number> = {};
  let prevBoundaryEnd = -1;
  for (const b of boundaryIndices) {
    const id = currentSession.messages[b].id;
    let count = 0;
    for (let j = prevBoundaryEnd + 1; j < b; j++) {
      if (currentSession.messages[j].role !== "marker") count++;
    }
    boundaryArchivedCounts[id] = count;
    prevBoundaryEnd = b;
  }

  // 最近一条 user 消息：允许「编辑后重跑」；
  // 若它之后没有 assistant 回复（被中断 / 失败），还允许「重新生成」。
  let lastUserMsgId: string | null = null;
  let lastUserHasAssistantAfter = false;
  for (let i = currentSession.messages.length - 1; i >= 0; i--) {
    const m = currentSession.messages[i];
    if (m.role === "user") {
      lastUserMsgId = m.id;
      for (let j = i + 1; j < currentSession.messages.length; j++) {
        if (currentSession.messages[j].role === "assistant") {
          lastUserHasAssistantAfter = true;
          break;
        }
      }
      break;
    }
  }

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
  async function handleRegenerateUser(msgId: string) {
    if (isStreaming) return;
    try {
      await regenerateFromUser(msgId);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }
  async function handleEditUser(msgId: string, nextContent: string) {
    if (isStreaming) throw new Error("生成中，无法编辑消息");
    try {
      await editAndRerun(msgId, nextContent);
    } catch (e: any) {
      toast.error(e.message || String(e));
      throw e;
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
          {currentSession.messages.map((m, i) => {
            const isBoundary = m.meta?.type === "compact_boundary";
            // 非 boundary 消息：归属下一个 boundary，未展开"历史对话"则不渲染
            if (!isBoundary) {
              const owner = nextBoundaryId(i);
              if (owner !== null && !expandedHistories.has(owner)) {
                return null;
              }
            }
            const isLatestUser = m.role === "user" && m.id === lastUserMsgId;
            const onRegenerate =
              m.role === "assistant"
                ? handleRegenerate
                : isLatestUser && !lastUserHasAssistantAfter && !isStreaming
                  ? handleRegenerateUser
                  : undefined;
            const onEdit =
              isLatestUser && !isStreaming ? handleEditUser : undefined;
            return (
            <MessageBubble
              key={m.id}
              message={m}
              session={currentSession}
              prompt={activePrompt}
              userAvatar={userAvatar}
              onFork={handleFork}
              onRegenerate={onRegenerate}
              onEdit={onEdit}
              archived={lastCompactBoundaryIdx > 0 && i < lastCompactBoundaryIdx}
              summaryExpanded={isBoundary && expandedSummaries.has(m.id)}
              onToggleSummary={
                isBoundary ? () => toggleIn(setExpandedSummaries, m.id) : undefined
              }
              historyExpanded={isBoundary && expandedHistories.has(m.id)}
              onToggleHistory={
                isBoundary ? () => toggleIn(setExpandedHistories, m.id) : undefined
              }
              archivedCount={isBoundary ? boundaryArchivedCounts[m.id] : undefined}
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
            );
          })}
          {isStreaming && (
            <MessageBubble
              streaming
              session={currentSession}
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
          {/* RunMode 状态标签（架构 §10.2）：仅在 run_mode_changed 事件来过后显示，
              ChatView 内悬浮一行——目前 UI 没有正式状态栏，先以轻量提示出现。 */}
          {isStreaming && currentRunMode ? (
            <div className="mx-auto my-1 text-[11px] uppercase tracking-wide text-muted-foreground/80">
              RunMode: {currentRunMode}
            </div>
          ) : null}
          {/* AutoMode 判官标记气泡（架构 §4.4.4）：每一个 PermissionAutoJudged 事件渲染一行。
              当前实现按时间顺序整体追加到流式 bubble 之后；run 结束 reload 时随 slot 一起清掉。 */}
          {isStreaming &&
            autoJudgedNotes.map((n, idx) => (
              <div
                key={`auto-judge-${idx}`}
                className="mx-auto my-1 text-xs text-muted-foreground"
              >
                {n.decision === "allow" ? "✓" : n.decision === "deny" ? "✗" : "?"}{" "}
                AutoMode {n.decision === "allow"
                  ? "自动放行"
                  : n.decision === "deny"
                  ? "拒绝"
                  : "转人工"}{" "}
                [{n.toolName}]
                {n.reason ? <span className="opacity-70">：{n.reason}</span> : null}
              </div>
            ))}
          {/* 「立即发送」插入的 user message：紧跟当前 streaming bubble 之后展示，
              下一轮 assistant 输出会接在它后面。run 结束 reload session 时此列表被清空。 */}
          {isStreaming &&
            injectedSinceStream.map((m) => (
              <MessageBubble
                key={m.id}
                message={m}
                session={currentSession}
                prompt={activePrompt}
                userAvatar={userAvatar}
              />
            ))}
        </div>
      </div>

      <PermissionApprovalPopup />

      <div className="relative">
        <div className="absolute inset-x-0 bottom-full pointer-events-none z-10">
          <UserQuestionPopup />
        </div>
        <InputQueuePanel />
        <ChatInput
          onSend={handleSend}
          onCancel={handleCancel}
          isStreaming={isStreaming}
          userMessageHistory={userMessageHistory}
        />
      </div>
    </div>
  );
}
