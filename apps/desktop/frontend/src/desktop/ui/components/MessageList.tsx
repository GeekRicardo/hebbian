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

/**
 * 计算消息渲染顺序（架构 §4.12.5 修订）。
 *
 * 物理 jsonl 顺序可能出现 wakeup 在 assistant 之前——因为 wakeup 是即写即落
 * （bash 后台任务完成的瞬间），而 assistant 等 stream 完成才落盘。view 上把
 * wakeup 卡片排在 tool_call 卡片之前是反直觉的（wakeup 是该 tool_call 的回应）。
 *
 * 重排策略：
 * - 普通消息按物理顺序
 * - `meta.system_notification` 的 user message 带 `tool_use_id`，找它之后第一个
 *   `tool_calls` 含该 id 的 assistant：把 wakeup **推迟到该 assistant 之后**
 * - 找不到（wakeup 物理位置已在 assistant 之后 / 或 tool_call 已经不存在）→
 *   保留原位，兜底
 *
 * 返回值是原 index 序列。所有按物理 index 索引的字段（find / boundary /
 * archived 等）都拿原 index 取值，保持正确。
 */
export function reorderForWakeupView(messages: Message[]): number[] {
  // 1. 标记需要"推迟"的 wakeup：找它后面的目标 assistant 原 index
  const deferTo = new Map<number, number>(); // wakeupIdx -> targetAssistantIdx
  for (let i = 0; i < messages.length; i++) {
    const m = messages[i];
    if (m.role !== "user" || m.meta?.type !== "system_notification") continue;
    const toolUseId = m.meta.tool_use_id;
    if (!toolUseId) continue;
    for (let j = i + 1; j < messages.length; j++) {
      const target = messages[j];
      if (target.role === "assistant" && target.tool_calls?.some((tc) => tc.id === toolUseId)) {
        deferTo.set(i, j);
        break;
      }
    }
  }

  // 2. 按 target 反向索引：assistantIdx -> wakeupIdx[]
  const pendingByAssistant = new Map<number, number[]>();
  for (const [wakeupIdx, assistantIdx] of deferTo) {
    const list = pendingByAssistant.get(assistantIdx) ?? [];
    list.push(wakeupIdx);
    pendingByAssistant.set(assistantIdx, list);
  }

  // 3. 渲染顺序：跳过被 defer 的 wakeup，在 target assistant 之后插入
  const order: number[] = [];
  for (let i = 0; i < messages.length; i++) {
    if (deferTo.has(i)) continue; // wakeup 跳过原位
    order.push(i);
    const pending = pendingByAssistant.get(i);
    if (pending) {
      for (const wakeupIdx of pending) order.push(wakeupIdx);
    }
  }
  return order;
}

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

  // 计算视觉顺序：wakeup 被推迟到对应 assistant 之后；其他保持物理顺序。
  // viewOrder 是原 index 序列——后续按物理 index 索引的字段（boundary / archived /
  // find activeLocation / matchBase）仍按原 i 取值，**仅渲染顺序**调整。
  const viewOrder = useMemo(() => reorderForWakeupView(messages), [messages]);

  return (
    <div>
      {viewOrder.map((i) => {
        const m = messages[i];
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
