import type { Message, StreamingAssistantPart } from "@/desktop/ui/types";

export type RunningTimelineRenderItem<T> =
  | { kind: "timeline"; item: T }
  | { kind: "streaming" };

/**
 * Run 内时间线条目（架构 §4.2 + §4.12.5）。run 跑到一半时把"已完成 turn 快照"
 * 和"streaming 中插队的 user message / 系统通知"按真实发生顺序穿插起来。
 *
 * - `assistant_frozen`：一次 turn 的 TurnFinished 事件触发——把当时的
 *   streamingText / streamingParts 原样冻结，渲染走标准 MessageBubble
 * - `user_injected`：streaming 期间注入的持久化 Message（用户插队 / 系统通知）
 *
 * run 结束 reload session 时由 session.messages 接管最终顺序。
 */
export type LiveTimelineItem =
  | {
      kind: "assistant_frozen";
      /** 冻结时的临时 id；reload 后会被真正的 message id 替代 */
      id: string;
      text: string;
      parts: StreamingAssistantPart[];
      created_at: number;
    }
  | {
      kind: "user_injected";
      message: Message;
    };

/** 从 assistant parts 提取持有的 tool_call id（wakeup 排序锚点）。 */
function toolCallIdsFromParts(parts: StreamingAssistantPart[]): string[] {
  const ids: string[] = [];
  for (const p of parts) {
    if (p.type === "tool_call" && p.id) ids.push(p.id);
  }
  return ids;
}

/**
 * liveTimeline 渲染项的 wakeup 排序投影：与 reorderForWakeupView 同一套规则，
 * 让「流式中」与「reload 后」顺序一致（run 结束 reload 不跳变）。
 * `streamingParts` 是当前正在产出的 assistant 段，作为合法 defer 目标。
 */
export function liveTimelineWakeupProjector(
  streamingParts: StreamingAssistantPart[]
): WakeupOrderProjector<LiveTimelineItem> {
  return {
    notificationToolUseId: (item) =>
      item.kind === "user_injected" &&
      item.message.role === "user" &&
      item.message.meta?.type === "system_notification"
        ? item.message.meta.tool_use_id
        : null,
    assistantToolCallIds: (item) =>
      item.kind === "assistant_frozen" ? toolCallIdsFromParts(item.parts) : null,
    streamingToolCallIds: () => toolCallIdsFromParts(streamingParts),
  };
}

/**
 * wakeup / 后台通知排序的唯一规则（架构 §4.12.5 修订）。
 *
 * 物理顺序里 system_notification 可能落在 assistant 之前——通知是即写即落（后台
 * 任务完成的瞬间），而 assistant 等 stream 完成才落盘。view 上把通知排在它所回应
 * 的 tool_call 卡片之前是反直觉的，所以把通知**推迟到含该 tool_call 的 assistant
 * 之后**。
 *
 * 这是与具体数据形态无关的纯排序核心：调用方用两个投影函数描述每一项——
 * - `notificationToolUseId(item)`：该项是带 tool_use_id 的系统通知 → 返回 id；否则 null
 * - `assistantToolCallIds(item)`：该项是 assistant 段 → 返回它持有的 tool_call id 列表
 *
 * reload 后的 `Message[]` 与 run 进行中的 liveTimeline 渲染项共用它，保证
 * 「流式中」与「reload 后」给出同一套顺序，run 结束 reload 不跳变。
 *
 * 返回原 index 序列（按目标 assistant 之后插入被推迟的通知）。
 */
export function reorderWakeupOrder<T>(
  items: T[],
  notificationToolUseId: (item: T) => string | null | undefined,
  assistantToolCallIds: (item: T) => string[] | null | undefined
): number[] {
  const ownsToolCall = (idx: number, toolUseId: string): boolean =>
    assistantToolCallIds(items[idx])?.some((id) => id === toolUseId) ?? false;

  // 1. 标记需要"推迟"的通知：钉到持有该 tool_call 的 assistant 之后。
  //    先向后找（reload 场景通知即写即落，物理常在 assistant 前）；找不到再向前找
  //    （run 进行中 assistant 段已冻结、物理在通知前）。双向查找让两个场景同源。
  const deferTo = new Map<number, number>(); // notificationIdx -> targetAssistantIdx
  for (let i = 0; i < items.length; i++) {
    const toolUseId = notificationToolUseId(items[i]);
    if (!toolUseId) continue;
    let target = -1;
    for (let j = i + 1; j < items.length; j++) {
      if (ownsToolCall(j, toolUseId)) {
        target = j;
        break;
      }
    }
    if (target < 0) {
      for (let j = i - 1; j >= 0; j--) {
        if (ownsToolCall(j, toolUseId)) {
          target = j;
          break;
        }
      }
    }
    if (target >= 0) deferTo.set(i, target);
  }

  // 2. 按 target 反向索引：assistantIdx -> notificationIdx[]
  const pendingByAssistant = new Map<number, number[]>();
  for (const [notificationIdx, assistantIdx] of deferTo) {
    const list = pendingByAssistant.get(assistantIdx) ?? [];
    list.push(notificationIdx);
    pendingByAssistant.set(assistantIdx, list);
  }

  // 3. 渲染顺序：跳过被 defer 的通知，在 target assistant 之后插入
  const order: number[] = [];
  for (let i = 0; i < items.length; i++) {
    if (deferTo.has(i)) continue; // 通知跳过原位
    order.push(i);
    const pending = pendingByAssistant.get(i);
    if (pending) {
      for (const notificationIdx of pending) order.push(notificationIdx);
    }
  }
  return order;
}

/**
 * reload 后的消息渲染顺序：reorderWakeupOrder 的 Message[] 适配。
 *
 * 返回值是原 index 序列。所有按物理 index 索引的字段（find / boundary /
 * archived 等）都拿原 index 取值，保持正确。
 */
export function reorderForWakeupView(messages: Message[]): number[] {
  return reorderWakeupOrder(
    messages,
    (m) =>
      m.role === "user" && m.meta?.type === "system_notification" ? m.meta.tool_use_id : null,
    (m) => (m.role === "assistant" ? m.tool_calls?.map((tc) => tc.id) : null)
  );
}

export function filterMessagesDuplicatedInLiveTimeline<T extends { id: string }>(
  messages: T[],
  liveTimelineMessages: Array<{ id: string }>
): T[] {
  if (liveTimelineMessages.length === 0) return messages;
  const liveIds = new Set(liveTimelineMessages.map((message) => message.id));
  return messages.filter((message) => !liveIds.has(message.id));
}

/**
 * 投影函数：从渲染项里读出 wakeup 排序所需的语义。
 * - `notificationToolUseId`：该项是带 tool_use_id 的系统通知 → 返回 id；否则 null
 * - `assistantToolCallIds`：该项是 assistant 段 → 返回它持有的 tool_call id 列表
 * streaming 占位项由 `streamingToolCallIds` 单独提供（当前正在产出的 assistant 段）。
 */
export interface WakeupOrderProjector<T> {
  notificationToolUseId: (item: T) => string | null | undefined;
  assistantToolCallIds: (item: T) => string[] | null | undefined;
  streamingToolCallIds?: () => string[] | null | undefined;
}

export function runningTimelineRenderItems<T>(
  items: T[],
  streamingInsertPos: number,
  shouldShowStreaming: boolean,
  projector?: WakeupOrderProjector<T>
): Array<RunningTimelineRenderItem<T>> {
  const rendered: Array<RunningTimelineRenderItem<T>> = !shouldShowStreaming
    ? items.map((item) => ({ kind: "timeline", item }))
    : (() => {
        const insertAt = Math.max(0, Math.min(streamingInsertPos, items.length));
        return [
          ...items.slice(0, insertAt).map((item) => ({ kind: "timeline" as const, item })),
          { kind: "streaming" as const },
          ...items.slice(insertAt).map((item) => ({ kind: "timeline" as const, item })),
        ];
      })();

  if (!projector) return rendered;

  // 与 reload 同源：把带 tool_use_id 的通知钉到含该 tool_call 的 assistant（含 streaming
  // 段）之后。streaming 占位也作为合法 defer 目标——后台任务正是当前流式段发起的。
  const order = reorderWakeupOrder(
    rendered,
    (ri) => (ri.kind === "timeline" ? projector.notificationToolUseId(ri.item) : null),
    (ri) =>
      ri.kind === "streaming"
        ? projector.streamingToolCallIds?.()
        : projector.assistantToolCallIds(ri.item)
  );
  return order.map((i) => rendered[i]);
}
