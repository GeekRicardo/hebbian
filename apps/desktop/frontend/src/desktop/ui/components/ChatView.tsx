import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Sparkles, ChevronDown, RotateCw, Scissors } from "lucide-react";
import { MessageBubble, formatCompactDuration } from "./MessageBubble";
import { MessageList } from "./MessageList";
import { MemoryWriteSummary } from "./MemoryWriteSummary";
import { ChatInput } from "./ChatInput";
import { InputQueuePanel } from "./InputQueuePanel";
import { ToastRegion } from "./ToastRegion";
import { ContinueBar } from "./ContinueBar";
import {
  filterMessagesDuplicatedInLiveTimeline,
  runningTimelineRenderItems,
} from "./liveTimelineOrder";
import { PermissionApprovalPopup } from "./PermissionApprovalPopup";
import { runModeLabel } from "./RunModeChip";
import { UserQuestionPopup } from "./UserQuestionPopup";
import { FindBar, findMatches, useFindController } from "./FindBar";
import { useStore } from "@/desktop/ui/store/useStore";
import { Button } from "@/desktop/ui/components/ui/button";
import { cn, hasSessionStarted, ipcConfirm } from "@/desktop/ui/lib/utils";
import { isLocalFindShortcut, isTerminalFocusTarget } from "@/desktop/ui/lib/keyboardShortcuts";
import { stickyBottomScrollTop } from "@/desktop/ui/lib/chatScrollPosition";
import { shouldUseNewConversationInputLayout } from "@/desktop/ui/newConversationLayout";
import type { MessageAttachment } from "@/desktop/ui/types";

const PINNED_USER_MESSAGE_VISIBLE = false;

interface ChatViewProps {
  emptyState?: ReactNode;
}

export function ChatView({ emptyState }: ChatViewProps = {}) {
  const {
    currentSession,
    promptsFile,
    prompts,
    userAvatar,
    streamingMessageId,
    streamingText,
    streamingParts,
    liveTimeline,
    assistantInsertPos,
    currentRunMode,
    sendUserMessage,
    cancelStreaming,
    forkSession,
    regenerateFrom,
    regenerateFromUser,
    editAndRerun,
    regenerateTitle,
    openAppSettingsAt,
    newSession,
    pendingPromptId,
    setPendingPromptId,
    updateCurrentConfig,
    debugEnabled,
    appSettings,
    modelRetry,
    contextCompacted,
    undoCompaction,
    deleteTrailingMessage,
    sessionMemoryWrites,
    sessionLastRunDurationMs,
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
  // ==== 浮动 user 消息（sticky header）====
  // 当某条 user 消息顶部滚出 chat 视口上方时，在 chat 顶部浮动它的截断副本。
  // 点击浮动条 → 滚动到该 user 消息真实位置，让真实顶边与浮动区下边缘重合，
  // 视觉上"浮动的"替换为"真实的"。随后设置锚定。
  // 死区规则（仅向下滚动生效）：锚定消息的 top 还在浮动区下方 → 阻止上一条浮动，
  // 等 anchor 的 top 滚出浮动区下边缘才解除。向上滚动时跳过死区，让上一条自然浮动。
  const [pinnedUserId, setPinnedUserId] = useState<string | null>(null);
  // 锚定信息：点击浮动条后锁定当前消息 id。
  // 死区判断直接用 anchorBottom 与 containerTop - PINNED_HEIGHT_PX 比较，
  // 不需要额外存储偏移量。
  // 用 ref 不走 state，避免与 pinnedUserId 的 setState 形成循环。
  const anchorRef = useRef<{ userId: string } | null>(null);
  // 追踪滚动方向：死区只在向下滚动时生效，向上滚动跳过死区让上一条自然浮动。
  const lastScrollTopRef = useRef(0);
  // 程序化滚动标志：scrollToPinnedMessage / scrollToPrevUserMessage 触发 scrollTo 时
  // handleScroll 会跟着触发，此时不应清除 anchor。
  const isProgrammaticScrollRef = useRef(false);
  // 浮动副本最大高度（截断 ~2 行），也是几何判定的基准。
  const PINNED_HEIGHT_PX = 72;
  const PINNED_ALIGN_TOLERANCE_PX = 4;


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

  const isStreaming = !!streamingMessageId;
  const rawMessages = currentSession?.messages ?? [];
  const liveTimelineMessages = useMemo(
    () => liveTimeline.flatMap((item) => (item.kind === "user_injected" ? [item.message] : [])),
    [liveTimeline]
  );
  const messages = useMemo(
    () =>
      isStreaming
        ? filterMessagesDuplicatedInLiveTimeline(rawMessages, liveTimelineMessages)
        : rawMessages,
    [isStreaming, rawMessages, liveTimelineMessages]
  );
  const userMessageHistory = useMemo(
    () => rawMessages.filter((m) => m.role === "user").map((m) => m.content),
    [rawMessages]
  );
  const isNewConversationLayout = shouldUseNewConversationInputLayout({
    userMessageCount: userMessageHistory.length,
    isStreaming,
  });

  // 计算每条消息的匹配区间（只对非 marker 生效）
  const matchesPerMessage = useMemo(() => {
    if (!currentSession || !findOpen || !findQuery) return [];
    return messages.map((m) =>
      m.role === "marker"
        ? ([] as Array<[number, number]>)
        : findMatches(m.content, findQuery, findRegex, findCase)
    );
  }, [currentSession, messages, findOpen, findQuery, findRegex, findCase]);

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

  // 切对话时强制贴回底部 + 重置浮动状态
  useEffect(() => {
    stickToBottomRef.current = true;
    anchorRef.current = null;
    setPinnedUserId(null);
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

  // 右侧工作台展开/收起会改变 chat 宽度，长文本重新换行后 scrollHeight 变大。
  // 如果用户原本贴底，应在这次重排后仍贴底；如果用户在看历史，则保持原位置不抢滚动。
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      if (!stickToBottomRef.current) return;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        el.scrollTop = stickyBottomScrollTop(el);
      });
    });
    observer.observe(el);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, []);

  // 监听滚动：贴底检测 + 浮动副本几何判定 + 死区保护
  const BOTTOM_SLACK_PX = 80;
  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;

    // 程序化滚动（点击浮动条 / 上箭头触发的 scrollTo）跳过方向判定和死区，
    // 只更新 lastScrollTopRef 供下一次用户滚动使用。
    if (isProgrammaticScrollRef.current) {
      lastScrollTopRef.current = el.scrollTop;
      isProgrammaticScrollRef.current = false;
      // 贴底检测仍然需要
      const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
      stickToBottomRef.current = distanceFromBottom <= BOTTOM_SLACK_PX;
      return;
    }

    // 贴底检测
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickToBottomRef.current = distanceFromBottom <= BOTTOM_SLACK_PX;

    // 滚动方向：死区只在向下滚动时生效，向上滚动跳过死区。
    const scrollingDown = el.scrollTop > lastScrollTopRef.current;
    lastScrollTopRef.current = el.scrollTop;

    // 浮动区下边缘 = 容器顶端 + 浮动区高度
    const containerTop = el.getBoundingClientRect().top;
    const pinnedBottom = containerTop + PINNED_HEIGHT_PX;

    const userBubbles = el.querySelectorAll<HTMLElement>('[data-message-role="user"]');
    if (userBubbles.length === 0) {
      setPinnedUserId(null);
      anchorRef.current = null;
      return;
    }

    // 找「最靠近浮动区、顶部已被遮挡」的 user 消息（最后一条 top < pinnedBottom 的）
    let pinned: HTMLElement | null = null;
    for (const b of userBubbles) {
      const r = b.getBoundingClientRect();
      if (r.top < pinnedBottom - PINNED_ALIGN_TOLERANCE_PX) {
        pinned = b;
      } else {
        break;
      }
    }

    if (!pinned) {
      setPinnedUserId(null);
      anchorRef.current = null;
      return;
    }
    const id = pinned.getAttribute("data-message-id");
    if (!id) return;

    // 死区检查：仅在向下滚动时生效。
    // 向上滚动时跳过死区，让上一条消息自然浮动。
    // 向下滚动时：锚定消息的 top 还在浮动区下边缘之上 → 阻止 anchor 及其之前的消息浮动，
    // 等 anchor 完全滚出浮动区（top > pinnedBottom）才解除锚定。
    const anchor0 = anchorRef.current;
    if (anchor0 && scrollingDown) {
      const anchor = anchor0;
      const anchorEl = el.querySelector<HTMLElement>(`[data-message-id="${anchor.userId}"]`);
      if (anchorEl) {
        const anchorRect = anchorEl.getBoundingClientRect();
        if (anchorRect.top < pinnedBottom) {
          // anchor 的 top 还在浮动区下方 → 死区内，阻止 anchor 及之前的消息浮动
          let pinnedIsAnchorOrBefore = false;
          for (const b of userBubbles) {
            if (b === pinned) { pinnedIsAnchorOrBefore = true; break; }
            if (b.getAttribute("data-message-id") === anchor.userId) { pinnedIsAnchorOrBefore = true; break; }
          }
          if (pinnedIsAnchorOrBefore) {
            setPinnedUserId(null);
            return;
          }
          // pinned 在 anchor 之后（更新的消息）→ 正常浮动，不受死区保护
        } else {
          // anchor 的 top 已滚出浮动区 → 解除锚定
          anchorRef.current = null;
        }
      } else {
        anchorRef.current = null;
      }
    } else if (anchor0 && !scrollingDown) {
      // 向上滚动：anchor 的 top 已在可见区域内 → 解除锚定
      const anchorEl = el.querySelector<HTMLElement>(`[data-message-id="${anchor0.userId}"]`);
      if (anchorEl) {
        const anchorRect = anchorEl.getBoundingClientRect();
        if (anchorRect.top >= pinnedBottom) {
          anchorRef.current = null;
        }
      } else {
        anchorRef.current = null;
      }
    }

    setPinnedUserId(id);
  }, []);

  // 点击浮动条：滚动到该 user 消息真实位置，让真实顶边 = 浮动区下边缘。
  // 同时设置锚定 = 该消息 + 死区（容器顶部往下 2 倍浮动区高度）。
  const scrollToPinnedMessage = useCallback(() => {
    if (!pinnedUserId) return;
    const el = scrollRef.current;
    if (!el) return;
    const target = el.querySelector<HTMLElement>(`[data-message-id="${pinnedUserId}"]`);
    if (!target) return;

    // 目标滚动位置：让真实消息的 top 对齐到浮动区下边缘（PINNED_HEIGHT_PX）
    const top = target.offsetTop - PINNED_HEIGHT_PX;

    // 锚定当前消息，死区判断用 anchorBottom
    anchorRef.current = { userId: pinnedUserId };

    setPinnedUserId(null);
    isProgrammaticScrollRef.current = true;
    el.scrollTo({ top, behavior: "instant" });
  }, [pinnedUserId]);

  // 获取当前 pinned 消息之前的上一条 user message id
  const getPrevUserMessageId = useCallback((currentId: string): string | null => {
    let found = false;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (found && messages[i].role === "user") {
        return messages[i].id;
      }
      if (messages[i].id === currentId) {
        found = true;
      }
    }
    return null;
  }, [messages]);

  // 上箭头：跳到上一条 user 消息，重置锚定到新目标
  const scrollToPrevUserMessage = useCallback(() => {
    if (!pinnedUserId) return;
    const prevId = getPrevUserMessageId(pinnedUserId);
    if (!prevId) return;
    const el = scrollRef.current;
    if (!el) return;
    const target = el.querySelector<HTMLElement>(`[data-message-id="${prevId}"]`);
    if (!target) return;

    // 目标滚动位置：让真实消息的 top 对齐到浮动区下边缘（PINNED_HEIGHT_PX）
    const top = target.offsetTop - PINNED_HEIGHT_PX;

    // 锚定新消息，死区判断用 anchorBottom
    anchorRef.current = { userId: prevId };

    setPinnedUserId(null);
    isProgrammaticScrollRef.current = true;
    el.scrollTo({ top, behavior: "instant" });
  }, [pinnedUserId, getPrevUserMessageId]);

  // Cmd/Ctrl+F 拉起查找
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // 终端聚焦时 Ctrl+F 是 readline 前移，不能被查找截胡（内置终端-spec.md §5）
      if (isTerminalFocusTarget(document.activeElement)) return;
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

  // 前台窗口 + 焦点不在输入类元素时，Enter 把焦点切到 chat 输入框，
  // 用户可以直接打字。审批/提问弹窗打开时不干预（避免抢走弹窗 textarea 的 Enter）。
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Enter" || e.isComposing) return;
      // 终端聚焦时 Enter 是命令换行，不抢去聊天输入框
      if (isTerminalFocusTarget(document.activeElement)) return;
      // 弹窗打开时不干预
      const state = useStore.getState();
      if (state.pendingApproval || state.pendingQuestion) return;
      // 焦点已在输入类元素上时不干预（textarea / input / contentEditable / select）
      const el = document.activeElement;
      if (
        el instanceof HTMLTextAreaElement ||
        (el instanceof HTMLInputElement &&
          (!el.type || el.type === "text" || el.type === "search")) ||
        (el instanceof HTMLElement && el.isContentEditable) ||
        el instanceof HTMLSelectElement
      )
        return;
      // FindBar 打开时不干预（搜索框已获焦或即将获焦）
      if (findOpen) return;
      // 找到 chat 输入框并聚焦
      const textarea = document.querySelector<HTMLTextAreaElement>(
        ".chat-input-textarea",
      );
      textarea?.focus();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [findOpen]);

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

    // boundary 之后无非 marker 消息 = 可撤销（刚压缩完、还没产生新对话）
    const undoableIds = new Set<string>();
    for (const b of indices) {
      const hasContentAfter = messages
        .slice(b + 1)
        .some((m) => m.role !== "marker");
      if (!hasContentAfter) undoableIds.add(messages[b].id);
    }

    return { lastIdx, ownerByIndex, archivedCounts, undoableIds };
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
      // 点发送那一下就请求折叠右侧工作台（与「Run 跑完自动展开」配对）：
      // sidebar 先缓慢折叠，输入框随后过渡到全宽。
      useStore.getState().triggerCollapseRightSidebar();
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
  const handleUndoCompaction = useCallback(
    (markerId: string) => {
      undoCompaction(markerId);
    },
    [undoCompaction]
  );
  const handleDeleteMessage = useCallback(
    async (msgId: string, role: string) => {
      if (isStreaming) return;
      const ok = await ipcConfirm(
        role === "assistant"
          ? "删除这条回复？本轮回复的全部内容都会被删除，且无法恢复。"
          : "删除这条消息？删除后无法恢复。",
        "删除消息"
      );
      if (!ok) return;
      try {
        await deleteTrailingMessage(msgId);
      } catch (e: any) {
        toast.error(e.message || String(e));
      }
    },
    [isStreaming, deleteTrailingMessage]
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
          <Button variant="outline" onClick={() => openAppSettingsAt("providers")}>
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
        className="relative z-50 h-14 shrink-0 pl-4 pr-4 flex items-center justify-between drag-region"
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
            ) : null /* session 已锁定 prompt 后不再显示其名字——位置让给 header 右侧的 debug session id（debug off 时整行空） */}
          </div>
        </div>
        {/* debug 开启时在 header 右侧显示当前对话的 session 文件夹 id
            （~/.hebbian/sessions/<id>），方便对照 jsonl */}
        {debugEnabled && currentSession?.id ? (
          <span
            className="ml-auto shrink-0 truncate font-mono text-[10px] text-muted-foreground/70 no-drag select-text max-w-[260px]"
            title={`session ${currentSession.id}`}
          >
            {currentSession.id}
          </span>
        ) : null}
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

      {/* FloatingTaskPanel 已下线（2026-05-26）：todo 列表只在右侧工作台「任务清单」tab 展示，
          避免与 sidebar 重复 + 浮在 chat 上挡正文。TodoListUpdated 事件由 RightSidebar
          监听，自动展开 sidebar 并聚焦该 tab。 */}

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
        {isNewConversationLayout && (
          emptyState ?? (
            <div className="px-6 py-10 text-center text-sm text-muted-foreground">
              发送第一条消息开始对话
            </div>
          )
        )}
        <MessageList
          messages={messages}
          prompt={activePrompt}
          userAvatar={userAvatar}
          sessionId={currentSession?.id}
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
          onUndoCompaction={handleUndoCompaction}
          undoableCompactionIds={boundaryInfo.undoableIds}
          onDelete={handleDeleteMessage}
        />
        <div>
          {/* Run 内时间线：已完成 turn 快照 + streaming 期间的插队 user message，
              按真实发生顺序排好（架构 §4.2 + §4.12.5）。冻结快照走标准 MessageBubble
              复用渲染（streaming=false，但仍喂 streamingParts），下个 Turn 的输出
              起新的 streaming bubble 接在末尾——这样插队消息总落在它真正回应的
              Turn 之后、下个 Turn 之前。 */}
          {isStreaming &&
            runningTimelineRenderItems(
              liveTimeline,
              assistantInsertPos,
              isStreaming
            ).map((renderItem) => {
              if (renderItem.kind === "streaming") {
                return (
                  <MessageBubble
                    key="streaming"
                    streaming
                    prompt={activePrompt}
                    userAvatar={userAvatar}
                    sessionId={currentSession?.id}
                    appSettings={appSettings ?? undefined}
                    streamingParts={streamingParts}
                    message={{
                      id: "streaming",
                      role: "assistant",
                      content: streamingText,
                      created_at: Date.now(),
                    }}
                  />
                );
              }
              const item = renderItem.item;
              return item.kind === "assistant_frozen" ? (
                <MessageBubble
                  key={item.id}
                  prompt={activePrompt}
                  userAvatar={userAvatar}
                  sessionId={currentSession?.id}
                  appSettings={appSettings ?? undefined}
                  streamingParts={item.parts}
                  message={{
                    id: item.id,
                    role: "assistant",
                    content: item.text,
                    created_at: item.created_at,
                  }}
                />
              ) : (
                <MessageBubble
                  key={item.message.id}
                  message={item.message}
                  prompt={activePrompt}
                  userAvatar={userAvatar}
                  sessionId={currentSession?.id}
                  appSettings={appSettings ?? undefined}
                />
              );
            })}
          {/* RunMode 状态标签（架构 §10.2）：仅在 run_mode_changed 事件来过后显示，
              ChatView 内悬浮一行——目前 UI 没有正式状态栏，先以轻量提示出现。 */}
          {isStreaming && currentRunMode ? (
            <div className="mx-auto my-1 text-[11px] tracking-wide text-muted-foreground/80">
              当前模式：{runModeLabel(currentRunMode)}
            </div>
          ) : null}
          {currentSession && sessionLastRunDurationMs[currentSession.id] != null && !isStreaming ? (
            <div className="mx-auto my-1 text-center text-[11px] text-muted-foreground/80">
              本轮完成 · {formatCompactDuration(sessionLastRunDurationMs[currentSession.id])}
            </div>
          ) : null}
          {/* 本轮后台记忆抽取写入的记忆（架构 §4.14）：会话末尾一行低调摘要，可展开。
              run 结束后异步到达，故不依赖 isStreaming。 */}
          {currentSession && sessionMemoryWrites[currentSession.id]?.length ? (
            <MemoryWriteSummary items={sessionMemoryWrites[currentSession.id]} />
          ) : null}

        </div>
      </div>

      {/* 浮动 user 消息副本（sticky header）：某条 user 消息顶边滚出视口上方时，
          在 chat 区顶端浮一个截断副本。点击浮动条主体 → 滚动到真实消息位置对齐浮动区，
          浮动消失。右侧上箭头 → 跳转到上一条 user 消息。手动滚动破坏锚定后箭头跟随
          浮动条一起消失。 */}
      {PINNED_USER_MESSAGE_VISIBLE && pinnedUserId && (() => {
        const msg = messages.find((m) => m.id === pinnedUserId);
        if (!msg || msg.role !== "user") return null;
        const prevId = getPrevUserMessageId(pinnedUserId);
        return (
          <div className="pointer-events-none absolute inset-x-0 top-0 z-50">
            <div className="relative bg-background/95 shadow-md backdrop-blur-sm flex items-stretch">
              <button
                type="button"
                onClick={scrollToPinnedMessage}
                title="点击回到此消息"
                className="pointer-events-auto block flex-1 cursor-pointer text-left min-w-0"
              >
                <div className="overflow-hidden [mask-image:linear-gradient(to_bottom,black_60%,transparent)]"
                     style={{ maxHeight: PINNED_HEIGHT_PX }}>
                  <MessageBubble
                    message={msg}
                    prompt={activePrompt}
                    userAvatar={userAvatar}
                    sessionId={currentSession?.id}
                    appSettings={appSettings ?? undefined}
                  />
                </div>
              </button>
              {prevId && (
                <button
                  type="button"
                  onClick={scrollToPrevUserMessage}
                  title="跳转到上一条用户消息"
                  className="pointer-events-auto flex items-center justify-center w-8 shrink-0 text-muted-foreground hover:bg-accent hover:text-foreground transition-colors"
                >
                  <ChevronDown className="h-4 w-4 rotate-180" />
                </button>
              )}
            </div>
          </div>
        );
      })()}
      {/* 放大预览 portal 锚点：只覆盖 chat 区域（不含 header / input / sidebar）。
          内层放大内容用 `absolute inset-3` 撑满锚点 + 12px padding。 */}
      <div
        id="chat-fullscreen-anchor"
        className="pointer-events-none absolute inset-0 z-[60]"
      />
    </div>

    {/* ChatInput 包裹层的"三态"：
        - 全新对话（无任何消息且未在跑）→ 整块上移到 chat 区中部 + 宽度收 1/4（视觉对齐
          "新对话居中"模板，输入框比正式会话短一截，强调"还没开始"）。
        - 跑步态（streaming）→ translate-y-0 全宽贴底（"动画式下沉"，幅度=漂浮高度+24px）。
        - 跑完 / 历史会话静默态 → 上抬 24px 全宽（让卡片跟窗口底脱开一点"飘"的观感）。
        translateY 不改 flex 流分配，所以 messages 区高度始终是 flex-1，第一条消息
        一发出来就自然填满，输入框跟着下沉到底——transition-all 让 transform 和 width
        一起平滑过渡。 */}
    {/* 用 margin-bottom 而不是 translateY：translateY 只视觉位移，messages 区高度
        不变，上浮后会把最后一条消息盖住；margin-bottom 算进 flex 主轴尺寸，会让
        flex-1 的 messages 区跟着压缩，消息和输入框各占各的空间。 */}
    <div
        className={`chat-input-shell relative mx-auto transition-all duration-300 ease-out ${
          isNewConversationLayout
            ? "w-3/4 mb-[44vh]"
            : isStreaming
            ? "w-full mb-0"
            : "w-full mb-[23px]"
        }`}
      >
        <div className="absolute inset-x-0 bottom-full pointer-events-none z-30">
          <ToastRegion />
        </div>
        <div className="absolute inset-x-0 bottom-full pointer-events-none z-20">
          <PermissionApprovalPopup />
        </div>
        <div className="absolute inset-x-0 bottom-full pointer-events-none z-10">
          <UserQuestionPopup />
        </div>
        {modelRetry && (
          <div className="flex items-center gap-1.5 px-3 pb-1 text-xs text-amber-700 dark:text-amber-300">
            <RotateCw className="h-3 w-3 animate-spin" />
            <span>
              模型出错，重试中 {modelRetry.attempt}/{modelRetry.max}…
            </span>
          </div>
        )}
        {contextCompacted && (
          <div className="flex items-center gap-1.5 px-3 pb-1 text-xs text-blue-600 dark:text-blue-400">
            <Scissors className="h-3 w-3" />
            <span>
              上下文已自动压缩（{Math.round(contextCompacted.before_tokens / 1000)}k → {Math.round(contextCompacted.after_tokens / 1000)}k token）
            </span>
          </div>
        )}
        <InputQueuePanel />
        <ContinueBar
          onSend={(text) => handleSend(text, [])}
          onFocusInput={() =>
            document
              .querySelector<HTMLTextAreaElement>(".chat-input-textarea")
              ?.focus()
          }
        />
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
