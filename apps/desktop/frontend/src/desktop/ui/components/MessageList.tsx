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
import type { Message, Prompt } from "@/desktop/ui/types";

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
  isStreaming: boolean;
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
}

export const MessageList = memo(function MessageList({
  messages,
  prompt,
  userAvatar,
  isStreaming,
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

  return (
    <div>
      {messages.map((m, i) => {
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
            onFork={onFork}
            onRegenerate={onRegenerateProp}
            onEdit={onEditProp}
            archived={lastCompactBoundaryIdx > 0 && i < lastCompactBoundaryIdx}
            summaryExpanded={isBoundary && expandedSummaries.has(m.id)}
            onToggleSummary={isBoundary ? onToggleSummary : undefined}
            historyExpanded={isBoundary && expandedHistories.has(m.id)}
            onToggleHistory={isBoundary ? onToggleHistory : undefined}
            archivedCount={isBoundary ? boundaryArchivedCounts[m.id] : undefined}
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
