/**
 * 历史消息列表 —— 从 ChatView 拆出来的 memo 组件。
 *
 * 目的：避免流式期间 ChatView 高频 setState 把所有历史 bubble 都重渲。
 * 这里**故意不**接收整个 session 对象 —— session 的引用每次 store 事件都会变，
 * 让所有 bubble 跟着 memo bust。我们只接需要的小 props（消息数组本身的引用、
 * find 状态、几个稳定的回调），shallow compare 就能挡住绝大多数无关重渲染。
 *
 * 调用方在 ChatView 必须：
 * - 所有 callback 用 useCallback 包，引用稳定
 * - find 对象用 useMemo 包
 * - matchesPerMessage / nextBoundaryId / boundaryArchivedCounts 用 useMemo 包
 * 否则 memo 失效。
 */

import { memo, useMemo } from "react";
import { MessageBubble } from "./MessageBubble";
import { reorderForWakeupView } from "./liveTimelineOrder";
import type { Message, Prompt, MemoryWriteItem } from "@/desktop/ui/types";

interface FindCtx {
  query: string;
  regex: boolean;
  caseSensitive: boolean;
  matchesPerMessage: Array<Array<[number, number]>>;
  activeLocation: { msgIdx: number; localIdx: number } | null;
}

export interface MessageListProps {
  messages: Message[];
  prompt?: Prompt;
  userAvatar?: string;
  sessionId?: string;
  isStreaming: boolean;
  reserveBottomForQuestionPopup: boolean;
  lastUserMsgId: string | null;
  lastUserHasAssistantAfter: boolean;
  lastCompactBoundaryIdx: number;
  /** 每条非 boundary 消息归属下一个 boundary id（null 表示属于最后一段）。长度 = messages.length */
  ownerBoundaryByIndex: Array<string | null>;
  expandedHistories: Set<string>;
  expandedSummaries: Set<string>;
  boundaryArchivedCounts: Record<string, number>;
  find: FindCtx | null;
  onFork: (id: string) => void;
  onRegenerate: (id: string) => void;
  onRegenerateUser: (id: string) => void;
  onEdit: (id: string, nextContent: string) => Promise<void> | void;
  onToggleSummary: (id: string) => void;
  onToggleHistory: (id: string) => void;
  onUndoCompaction?: (markerId: string) => void;
  /** compact_boundary marker ID set：可撤销的压缩标记（之后无非 marker 消息）。 */
  undoableCompactionIds?: Set<string>;
  /**
   * 删除尾部消息（只允许从后往前删）：最后一个 run 的 assistant 可删（删整个 run
   * 输出）；最后一条 user 仅当其后无 assistant 时可删。
   */
  onDelete?: (id: string, role: string) => void;
}

export const MessageList = memo(function MessageList({
  messages,
  prompt,
  userAvatar,
  sessionId,
  isStreaming,
  reserveBottomForQuestionPopup,
  lastUserMsgId,
  lastUserHasAssistantAfter,
  lastCompactBoundaryIdx,
  ownerBoundaryByIndex,
  expandedHistories,
  expandedSummaries,
  boundaryArchivedCounts,
  find,
  onFork,
  onRegenerate,
  onRegenerateUser,
  onEdit,
  onToggleSummary,
  onToggleHistory,
  onUndoCompaction,
  undoableCompactionIds,
  onDelete,
}: MessageListProps) {
  /**
   * 把 messages 转成 (m, i, baseMatchIdx) 元组：每条消息在「全局命中数组」里
   * 的起始下标（matchBaseIdx）—— FindBar 高亮跳转时用全局 index 定位。
   *
   * 提前算好避免在 map 里跑累加 sum。
   */
  const matchBaseByIndex = useMemo(() => {
    const out: number[] = new Array(messages.length).fill(0);
    if (!find) return out;
    let running = 0;
    for (let i = 0; i < messages.length; i++) {
      out[i] = running;
      running += find.matchesPerMessage[i]?.length ?? 0;
    }
    return out;
  }, [messages.length, find]);

  // 计算视觉顺序：wakeup 被推迟到对应 assistant 之后；其他保持物理顺序。
  // viewOrder 是原 index 序列——后续按物理 index 索引的字段（boundary / archived /
  // find activeLocation / matchBase）仍按原 i 取值，**仅渲染顺序**调整。
  const viewOrder = useMemo(() => reorderForWakeupView(messages), [messages]);

  // 记忆汇总 marker（架构 §4.14）落盘时是独立一条 Role::Marker，但渲染要"提"回它
  // 所属的那条 assistant 气泡里（正文下方、操作行上方），不单独占行。这里建
  // 「assistant 消息 id → 紧跟其后的 memory_writes marker items」映射，并收集需在
  // 渲染时跳过的 marker id（避免它再独立渲染一行）。
  const { memoryWritesByAssistant, hiddenMemoryMarkerIds } = useMemo(() => {
    const byAssistant: Record<string, MemoryWriteItem[]> = {};
    const hidden = new Set<string>();
    for (let i = 0; i < messages.length; i++) {
      const m = messages[i];
      if (m.role !== "marker" || m.meta?.type !== "memory_writes") continue;
      // 往前找最近一条 assistant 挂上去；找不到就保留 marker 独立渲染兜底。
      for (let j = i - 1; j >= 0; j--) {
        if (messages[j].role === "assistant") {
          byAssistant[messages[j].id] = m.meta.items;
          hidden.add(m.id);
          break;
        }
      }
    }
    return { memoryWritesByAssistant: byAssistant, hiddenMemoryMarkerIds: hidden };
  }, [messages]);

  // 可删消息集合（删除只允许从后往前）：
  // - 最后一条真实 user 之后的 assistant（= 最后一个 run 的输出）
  // - 最后一条真实 user 自身，仅当其后已无 assistant
  const deletableIds = useMemo(() => {
    const ids = new Set<string>();
    if (!onDelete || isStreaming) return ids;
    let lastUserIdx = -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      const m = messages[i];
      if (m.role === "user" && m.meta?.type !== "system_notification") {
        lastUserIdx = i;
        break;
      }
    }
    if (lastUserIdx < 0) return ids;
    let hasAssistantAfter = false;
    for (let i = lastUserIdx + 1; i < messages.length; i++) {
      if (messages[i].role === "assistant") {
        ids.add(messages[i].id);
        hasAssistantAfter = true;
      }
    }
    if (!hasAssistantAfter) ids.add(messages[lastUserIdx].id);
    return ids;
  }, [messages, onDelete, isStreaming]);

  return (
    <div>
      {viewOrder.map((i) => {
        const m = messages[i];
        // 被"提"进 assistant 气泡的记忆 marker 不再独立渲染（见 memoryWritesByAssistant）。
        if (hiddenMemoryMarkerIds.has(m.id)) return null;
        const isBoundary = m.meta?.type === "compact_boundary";
        if (!isBoundary) {
          const owner = ownerBoundaryByIndex[i];
          if (owner !== null && !expandedHistories.has(owner)) {
            return null;
          }
        }
        const isLatestUser = m.role === "user" && m.id === lastUserMsgId;
        const onRegenerateProp =
          m.role === "assistant"
            ? onRegenerate
            : isLatestUser && !lastUserHasAssistantAfter && !isStreaming
              ? onRegenerateUser
              : undefined;
        const onEditProp =
          isLatestUser && !isStreaming ? onEdit : undefined;
        return (
          <MessageBubble
            key={m.id}
            message={m}
            prompt={prompt}
            userAvatar={userAvatar}
            sessionId={sessionId}
            reserveBottomForQuestionPopup={
              reserveBottomForQuestionPopup && m.role === "assistant" && i === messages.length - 1
            }
            onFork={onFork}
            onRegenerate={onRegenerateProp}
            onEdit={onEditProp}
            onDelete={deletableIds.has(m.id) ? onDelete : undefined}
            archived={lastCompactBoundaryIdx > 0 && i < lastCompactBoundaryIdx}
            summaryExpanded={isBoundary && expandedSummaries.has(m.id)}
            onToggleSummary={isBoundary ? onToggleSummary : undefined}
            historyExpanded={isBoundary && expandedHistories.has(m.id)}
            onToggleHistory={isBoundary ? onToggleHistory : undefined}
            archivedCount={isBoundary ? boundaryArchivedCounts[m.id] : undefined}
            canUndoCompaction={isBoundary && undoableCompactionIds?.has(m.id)}
            onUndoCompaction={isBoundary ? onUndoCompaction : undefined}
            memoryWrites={memoryWritesByAssistant[m.id]}
            find={
              find
                ? {
                    query: find.query,
                    regex: find.regex,
                    caseSensitive: find.caseSensitive,
                    activeLocalIdx:
                      find.activeLocation?.msgIdx === i
                        ? find.activeLocation.localIdx
                        : null,
                    matchBaseIdx: matchBaseByIndex[i],
                  }
                : undefined
            }
          />
        );
      })}
    </div>
  );
});
