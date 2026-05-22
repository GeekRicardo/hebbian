import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Sparkles, ChevronDown, FileJson } from "lucide-react";
import {
  MessageBubble,
  FloatingTaskPanel,
  extractLatestTodoSnapshot,
} from "./MessageBubble";
import { MessageList } from "./MessageList";
import { BackgroundTaskPanel } from "./BackgroundTaskPanel";
import { ModelIoInspector } from "./ModelIoInspector";
import { EditTreePanel } from "./EditTreePanel";
import { ChatInput } from "./ChatInput";
import { InputQueuePanel } from "./InputQueuePanel";
import { PermissionApprovalPopup } from "./PermissionApprovalPopup";
import { runModeLabel } from "./RunModeChip";
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
  /**
   * 自动贴底逻辑：用户在底部时才自动滚；一旦用户主动往上滚（离底 > BOTTOM_SLACK_PX）
   * 就关掉自动滚，避免流式 delta 把他拽回去。回到底部时（或新 session 切换）重新打开。
   *
   * 用 ref 不用 state 是因为 onScroll 不应该触发组件重渲染（一秒可能几十次）。
   */
  const stickToBottomRef = useRef(true);
  const [titleLoading, setTitleLoading] = useState(false);
  const [modelIoOpen, setModelIoOpen] = useState(false);

  // ==== 压缩分隔条：摘要展开 / 历史对话展开 两套独立状态 ====
  // - expandedSummaries：分隔条主体点击后展开摘要正文，用来评估压缩质量
  // - expandedHistories：「历史对话」按钮点击后展开压缩前的原始消息
  const [expandedSummaries, setExpandedSummaries] = useState<Set<string>>(
    () => new Set()
  );
  const [expandedHistories, setExpandedHistories] = useState<Set<string>>(
    () => new Set()
  );
  // Set state 的切换闭包：每次给 setter 传新 Set（React 浅比较生效）
  const toggleInSet = (
    setter: React.Dispatch<React.SetStateAction<Set<string>>>,
    id: string
  ) => {
    setter((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

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

  // 切对话时强制贴回底部 + 重置 stick 标志
  useEffect(() => {
    stickToBottomRef.current = true;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [currentSession?.id]);

  // 流式 delta / 新消息：仅当用户当前贴底时才自动滚。
  // 这里依赖 streamingText / streamingParts 等高频变化的 ref 来触发 effect，
  // 但 effect 内部 O(1)：读一次 scrollTop / scrollHeight，没贴底直接 return。
  useEffect(() => {
    if (!stickToBottomRef.current) return;
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [currentSession?.messages.length, streamingText, streamingParts]);

  // 监听用户滚动：离底超过阈值就关掉自动滚，回到底部再打开。
  const BOTTOM_SLACK_PX = 80;
  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickToBottomRef.current = distanceFromBottom <= BOTTOM_SLACK_PX;
  }, []);

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

  // ⚠️ 所有 hooks 必须在 early return 之前完成（React Hooks Rules）。
  // currentSession 可能为 null（初次启动 / 还没选 session）—— 下面所有派生 hooks
  // 都用 nullable handle，return 留到 hooks 全部声明之后再判。

  // 简单派生：useCallback 的依赖会用到，必须在 useCallback 之前声明
  const isStreaming = !!streamingMessageId;

  const messages = currentSession?.messages ?? [];
  const userMessageHistory = useMemo(
    () => messages.filter((m) => m.role === "user").map((m) => m.content),
    [messages]
  );

  /**
   * 一次性算出 boundary 相关派生：indices / lastIdx / 每条非 boundary 归属哪个 boundary id /
   * 每个 boundary 折叠多少条原始消息。这些都只依赖 messages 数组本身（同一 ref 时不重算）。
   */
  const boundaryInfo = useMemo(() => {
    const indices: number[] = [];
    messages.forEach((m, i) => {
      if (m.meta?.type === "compact_boundary") indices.push(i);
    });
    const lastIdx = indices.length > 0 ? indices[indices.length - 1] : -1;

    const ownerByIndex: Array<string | null> = new Array(messages.length).fill(null);
    let cursor = 0;
    for (let i = 0; i < messages.length; i++) {
      while (cursor < indices.length && indices[cursor] <= i) cursor++;
      ownerByIndex[i] =
        cursor < indices.length ? messages[indices[cursor]].id : null;
    }

    const archivedCounts: Record<string, number> = {};
    let prevEnd = -1;
    for (const b of indices) {
      const id = messages[b].id;
      let count = 0;
      for (let j = prevEnd + 1; j < b; j++) {
        if (messages[j].role !== "marker") count++;
      }
      archivedCounts[id] = count;
      prevEnd = b;
    }

    return { lastIdx, ownerByIndex, archivedCounts };
  }, [messages]);

  // 最近一条 user 消息：允许「编辑后重跑」；
  // 若它之后没有 assistant 回复（被中断 / 失败），还允许「重新生成」。
  const { lastUserMsgId, lastUserHasAssistantAfter } = useMemo(() => {
    let id: string | null = null;
    let hasAssistantAfter = false;
    for (let i = messages.length - 1; i >= 0; i--) {
      const m = messages[i];
      if (m.role === "user") {
        id = m.id;
        for (let j = i + 1; j < messages.length; j++) {
          if (messages[j].role === "assistant") {
            hasAssistantAfter = true;
            break;
          }
        }
        break;
      }
    }
    return { lastUserMsgId: id, lastUserHasAssistantAfter: hasAssistantAfter };
  }, [messages]);

  const handleSend = useCallback(
    async (content: string, attachments: MessageAttachment[]) => {
      // 用户主动发消息 → 期望看到自己刚发的消息，强制贴回底部
      // （即使之前在看历史）。流式 delta 也会重新跟随到底。
      stickToBottomRef.current = true;
      try {
        await sendUserMessage(content, attachments);
      } catch (e: any) {
        toast.error(e.message || String(e));
      }
    },
    [sendUserMessage]
  );

  const handleCancel = useCallback(async () => {
    try {
      await cancelStreaming();
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }, [cancelStreaming]);

  const handleFork = useCallback(
    async (msgId: string) => {
      try {
        await forkSession(msgId);
        toast.success("已创建分支");
      } catch (e: any) {
        toast.error(e.message || String(e));
      }
    },
    [forkSession]
  );

  const handleRegenerate = useCallback(
    async (msgId: string) => {
      if (isStreaming) return;
      try {
        await regenerateFrom(msgId);
      } catch (e: any) {
        toast.error(e.message || String(e));
      }
    },
    [isStreaming, regenerateFrom]
  );

  const handleRegenerateUser = useCallback(
    async (msgId: string) => {
      if (isStreaming) return;
      try {
        await regenerateFromUser(msgId);
      } catch (e: any) {
        toast.error(e.message || String(e));
      }
    },
    [isStreaming, regenerateFromUser]
  );

  const handleEditUser = useCallback(
    async (msgId: string, nextContent: string) => {
      if (isStreaming) throw new Error("生成中，无法编辑消息");
      try {
        await editAndRerun(msgId, nextContent);
      } catch (e: any) {
        toast.error(e.message || String(e));
        throw e;
      }
    },
    [isStreaming, editAndRerun]
  );

  const handleToggleSummary = useCallback(
    (id: string) => toggleInSet(setExpandedSummaries, id),
    []
  );
  const handleToggleHistory = useCallback(
    (id: string) => toggleInSet(setExpandedHistories, id),
    []
  );

  /**
   * find 上下文打包：依赖搜索状态 + matchesPerMessage + activeLocation。
   * find 关闭时直接 null —— MessageList 走 null 分支，不为每个 bubble 算 find prop。
   */
  const findCtxForList = useMemo(
    () =>
      findOpen && findQuery
        ? {
            query: findQuery,
            regex: findRegex,
            caseSensitive: findCase,
            matchesPerMessage,
            activeLocation,
          }
        : null,
    [findOpen, findQuery, findRegex, findCase, matchesPerMessage, activeLocation]
  );

  // ── 以下为非-hook 派生 & early return。所有 hooks 必须在这条线之上。 ──

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
  const latestTodos = extractLatestTodoSnapshot(currentSession, streamingParts);

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
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setModelIoOpen(true)}
            title="查看本会话所有发给模型的请求 / 响应"
            data-testid="open-model-io"
          >
            <FileJson className="w-3.5 h-3.5 mr-1" />
            Model I/O
          </Button>
          <Button variant="ghost" size="sm" onClick={() => setSettingsOpen(true)}>
            {currentSession?.project_id ? "项目设置" : "对话设置"}
          </Button>
        </div>
      </header>

      {currentSession?.id ? (
        <ModelIoInspector
          sessionId={currentSession.id}
          open={modelIoOpen}
          onClose={() => setModelIoOpen(false)}
        />
      ) : null}

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

      {latestTodos && latestTodos.length > 0 && (
        <FloatingTaskPanel
          key={currentSession.id}
          todos={latestTodos}
          streaming={isStreaming}
        />
      )}

      <BackgroundTaskPanel />

      <EditTreePanel />

      {/* chat 区域：header 下方、ChatInput 上方的消息列表区。
          所有"放大预览"（DiffViewer fullscreen / ExpandButton 放大）都 portal 到下面的
          #chat-fullscreen-anchor，确保只覆盖此区域、不挡 sidebar / 标题栏 / 输入框。
          详见架构.md §4.13.9 chat 区域定义。 */}
      <div className="relative flex-1 min-h-0">
      <div
        ref={scrollRef}
        className="absolute inset-0 overflow-y-auto"
        onScroll={handleScroll}
      >
        {messages.length === 0 && !isStreaming && (
          <div className="px-6 py-10 text-center text-sm text-muted-foreground">
            发送第一条消息开始对话
          </div>
        )}
        <MessageList
          messages={messages}
          prompt={activePrompt}
          userAvatar={userAvatar}
          isStreaming={isStreaming}
          lastUserMsgId={lastUserMsgId}
          lastUserHasAssistantAfter={lastUserHasAssistantAfter}
          lastCompactBoundaryIdx={boundaryInfo.lastIdx}
          ownerBoundaryByIndex={boundaryInfo.ownerByIndex}
          expandedHistories={expandedHistories}
          expandedSummaries={expandedSummaries}
          boundaryArchivedCounts={boundaryInfo.archivedCounts}
          find={findCtxForList}
          onFork={handleFork}
          onRegenerate={handleRegenerate}
          onRegenerateUser={handleRegenerateUser}
          onEdit={handleEditUser}
          onToggleSummary={handleToggleSummary}
          onToggleHistory={handleToggleHistory}
        />
        <div>
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
          {/* RunMode 状态标签（架构 §10.2）：仅在 run_mode_changed 事件来过后显示，
              ChatView 内悬浮一行——目前 UI 没有正式状态栏，先以轻量提示出现。 */}
          {isStreaming && currentRunMode ? (
            <div className="mx-auto my-1 text-[11px] tracking-wide text-muted-foreground/80">
              当前模式：{runModeLabel(currentRunMode)}
            </div>
          ) : null}
          {/* 自动模式下，每个被判定的工具调用对应一条提示。事件流：PermissionAutoJudged。 */}
          {isStreaming &&
            autoJudgedNotes.map((n, idx) => (
              <div
                key={`auto-judge-${idx}`}
                className="mx-auto my-1 text-xs text-muted-foreground"
              >
                {n.decision === "allow" ? "✓" : n.decision === "deny" ? "✗" : "?"}{" "}
                {n.decision === "allow"
                  ? "自动放行"
                  : n.decision === "deny"
                  ? "自动拒绝"
                  : "需要询问"}{" "}
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
                prompt={activePrompt}
                userAvatar={userAvatar}
              />
            ))}
        </div>
      </div>
        {/* 放大预览 portal 锚点：只覆盖 chat 区域（不含 header / input / sidebar）。
            内层放大内容用 `absolute inset-3` 撑满锚点 + 12px padding。 */}
        <div
          id="chat-fullscreen-anchor"
          className="pointer-events-none absolute inset-0 z-[60]"
        />
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
