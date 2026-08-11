import type {
  EngineEvent,
  PendingApproval,
  PendingQuestion,
  StreamingAssistantPart,
} from "@/desktop/ui/types";
import type { LiveTimelineItem } from "@/desktop/ui/components/liveTimelineOrder";
import type { SessionStream } from "./useStore";
import {
  applyReasoningDelta,
  applyTextDelta,
  applyTextDone,
  applyToolCallDelta,
  applyToolDone,
  applyToolOutputDelta,
  applyToolStart,
  finalizeOpenReasoning,
  // 显式带 .ts 扩展名（tsconfig allowImportingTsExtensions=true）：这是本文件唯一的
  // 运行时 import，带扩展名才能被 `node --experimental-strip-types` 直接解析、跑
  // slotReducer.test.ts（node 原生 ESM 不补全无扩展名相对路径）。Vite 同样接受。
} from "./streamingParts.ts";

/**
 * 单个 session 运行中「软状态」的纯 reducer：把一条 EngineEvent 折叠进 SessionStream。
 *
 * 从 useStore 抽出来独立成文件，目的有二：
 * 1. 它本就是个无副作用纯函数 `(slot, e) => slot`，独立后可被 standalone 单测覆盖
 *    （useStore 顶层有 zustand / tauri bridge 等运行时 import，整文件 import 进不了
 *    `node --experimental-strip-types`）；
 * 2. 把 streaming / liveTimeline / HITL 的状态机逻辑收敛在一处，便于回归。
 *
 * 唯一对外副作用（permission_auto_judged 的 deny toast）留在 useStore 调用层，
 * 让本函数保持纯净 —— 这样它在测试里不依赖 sonner。
 */

/** 从 record 删一个 key，返回新对象（key 不存在则原样返回）。 */
function dropKey<V>(rec: Record<string, V>, key: string): Record<string, V> {
  if (!(key in rec)) return rec;
  const { [key]: _drop, ...rest } = rec;
  return rest;
}

/**
 * 给 streamingParts 里 id===callId 的 tool_call part 设/清 `isJudging`
 * （AutoMode judge 评估中的黄色呼吸，架构 §4.4.4）。callId 空或找不到则原样返回。
 */
function setPartJudging(
  parts: StreamingAssistantPart[],
  callId: string,
  on: boolean
): StreamingAssistantPart[] {
  if (!callId) return parts;
  let changed = false;
  const next = parts.map((p) => {
    if (p.type === "tool_call" && p.id === callId && Boolean(p.isJudging) !== on) {
      changed = true;
      return { ...p, isJudging: on };
    }
    return p;
  });
  return changed ? next : parts;
}

/**
 * 子 agent 事件路由（架构 §4.4.11.8 / P7）：把带 `subagent_call_id` 的事件
 * 追加到对应 Task tool call 的 `nested_parts` 里，而不是顶层 streamingParts。
 * 这样 Task 卡片能在展开时渲染子工具调用 / 子文本 / 子推理。
 */
function applyNestedEvent(
  parts: StreamingAssistantPart[],
  callId: string,
  event: EngineEvent
): StreamingAssistantPart[] {
  return parts.map((p) => {
    if (p.type !== "tool_call" || p.id !== callId) return p;
    const nested = p.nested_parts ?? [];
    let newNested = nested;
    if (event.type === "text_delta" && event.text) {
      newNested = applyTextDelta(nested, event.text);
    } else if (event.type === "reasoning") {
      newNested = applyReasoningDelta(nested, event.text);
    } else if (event.type === "reasoning_duration") {
      const last = nested[nested.length - 1];
      if (last?.type === "reasoning" && last.duration_ms == null) {
        newNested = [...nested];
        newNested[newNested.length - 1] = { ...last, duration_ms: event.ms };
      }
    } else if (event.type === "reasoning_signature") {
      const last = nested[nested.length - 1];
      if (last?.type === "reasoning") {
        newNested = [...nested];
        newNested[newNested.length - 1] = { ...last, signature: event.signature };
      }
    } else if (event.type === "tool_call_delta") {
      newNested = applyToolCallDelta(nested, event);
    } else if (event.type === "tool_start") {
      newNested = applyToolStart(nested, event);
    } else if (event.type === "tool_done") {
      newNested = applyToolDone(nested, event);
    } else if (event.type === "tool_output_delta") {
      newNested = applyToolOutputDelta(nested, event);
    }
    if (newNested === nested) return p;
    return { ...p, nested_parts: newNested };
  });
}

export function applyEventToSlot(slot: SessionStream, e: EngineEvent): SessionStream {
  // 子 agent 事件：路由到对应 Task tool call 的 nested_parts（架构 §4.4.11.8）
  const nestedCallId = "subagent_call_id" in e ? e.subagent_call_id : undefined;
  if (nestedCallId) {
    return {
      ...slot,
      streamingParts: applyNestedEvent(slot.streamingParts, nestedCallId, e),
    };
  }

  switch (e.type) {
    case "run_finished":
      return { ...slot, streamingParts: finalizeOpenReasoning(slot.streamingParts) };

    case "model_retry":
      // 重试：回退到本 ModelStep 起点的快照（step_started{model} 时存的 retryBase），
      // 立即丢弃失败 attempt 已流出的残片（文本 / 推理 / 流式工具调用），但**保留本
      // step 之前的一切**——含多 Turn 共用 bubble 里前几轮已输出的文本与已执行的工具卡。
      // 失败 attempt 流到一半才报错时，这里的回退让残片立刻消失、不会叠到新 attempt 上。
      return {
        ...slot,
        streamingText: slot.retryBaseText ?? slot.streamingText,
        streamingParts: slot.retryBaseParts ?? slot.streamingParts,
        modelRetry: { attempt: e.attempt, max: e.max, reason: e.reason },
      };

    case "text_delta": {
      if (!e.text) return slot;
      // 失败 attempt 的残片已在 model_retry 时回退掉，这里只管正常追加；清空 modelRetry
      // 让「重试中」指示在新内容流出时消失。
      return {
        ...slot,
        modelRetry: null,
        streamingText: slot.streamingText + e.text,
        streamingParts: applyTextDelta(slot.streamingParts, e.text),
      };
    }

    case "text_done": {
      const { streamingText, streamingParts } = applyTextDone(
        slot.streamingText,
        slot.streamingParts,
        e.full_text
      );
      if (streamingText === slot.streamingText && streamingParts === slot.streamingParts) {
        return slot;
      }
      return { ...slot, streamingText, streamingParts };
    }

    case "reasoning":
      return {
        ...slot,
        modelRetry: null,
        streamingParts: applyReasoningDelta(slot.streamingParts, e.text),
      };

    case "reasoning_duration": {
      // 后端送来思考块的精确墙钟时长——用它定格最后一个未结束 reasoning 段的 duration_ms，
      // 覆盖前端秒表的估算值（架构 §3.1.1）。
      const parts = slot.streamingParts;
      const last = parts[parts.length - 1];
      if (last?.type !== "reasoning" || last.duration_ms != null) return slot;
      const next = [...parts];
      next[next.length - 1] = { ...last, duration_ms: e.ms };
      return { ...slot, streamingParts: next };
    }

    case "reasoning_signature": {
      // Anthropic thinking block 签名——写到最后一个 reasoning 段，落盘时随消息持久化。
      const parts = slot.streamingParts;
      const last = parts[parts.length - 1];
      if (last?.type !== "reasoning") return slot;
      const next = [...parts];
      next[next.length - 1] = { ...last, signature: e.signature };
      return { ...slot, streamingParts: next };
    }

    case "tool_call_delta":
      return {
        ...slot,
        modelRetry: null,
        streamingParts: applyToolCallDelta(slot.streamingParts, e),
      };

    case "tool_start":
      return { ...slot, streamingParts: applyToolStart(slot.streamingParts, e) };

    case "tool_done":
      return { ...slot, streamingParts: applyToolDone(slot.streamingParts, e) };

    case "tool_output_delta":
      return { ...slot, streamingParts: applyToolOutputDelta(slot.streamingParts, e) };

    case "run_suspended":
      return {
        ...slot,
        suspended: {
          reason: e.reason,
          resumesAtMs: e.resumes_at_ms ?? null,
          waitingForTaskIds: e.waiting_for_task_ids ?? [],
          suspendedAtMs: Date.now(),
        },
      };

    case "run_resumed":
      return { ...slot, suspended: null };

    case "permission_requested": {
      const approval: PendingApproval = {
        requestId: e.request_id,
        toolName: e.tool_name,
        input: e.input,
        summary: e.summary,
        risk: e.risk,
        paths: e.paths ?? [],
        kind: e.kind ?? "tool_call",
        fingerprint: e.fingerprint ?? null,
        commandSegments: e.command_segments ?? [],
        segments: e.segments ?? [],
        refuseRemember: e.refuse_remember ?? false,
        plan: e.plan ?? null,
      };
      // AutoMode judge 会接管这条审批时（后端判定：AutoMode + 模型在白名单），**先不弹
      // 审批框**——只给触发审批的工具卡片挂「judge 评估中」黄色呼吸，审批数据暂存进
      // judgingRequests。judge 异步出结果：ALLOW/DENY 由 permission_resolved 清掉（从不
      // 显示框），ASK 由 permission_auto_judged 把 approval 取出转入 pendingApproval 显形
      // （架构 §4.4.4）。改用后端权威的 `auto_handled`，不靠前端 currentRunMode 推断。
      if (e.auto_handled) {
        const callId = e.call_id ?? "";
        return {
          ...slot,
          streamingParts: callId
            ? setPartJudging(slot.streamingParts, callId, true)
            : slot.streamingParts,
          judgingRequests: {
            ...slot.judgingRequests,
            [e.request_id]: { callId, approval },
          },
        };
      }
      if (slot.pendingApproval) {
        return { ...slot, pendingApprovalQueue: [...slot.pendingApprovalQueue, approval] };
      }
      return { ...slot, pendingApproval: approval };
    }

    case "permission_resolved": {
      // 若这条审批曾被 judge 接管（黄色呼吸），先清掉呼吸 + 暂存。judge ALLOW/DENY
      // 自动 resolve 时走这里，审批数据从 judgingRequests 丢弃，从不显示框。
      const judging = slot.judgingRequests[e.request_id];
      const baseParts = judging
        ? setPartJudging(slot.streamingParts, judging.callId, false)
        : slot.streamingParts;
      const baseJudging = judging
        ? dropKey(slot.judgingRequests, e.request_id)
        : slot.judgingRequests;
      if (slot.pendingApproval?.requestId === e.request_id) {
        const next = slot.pendingApprovalQueue[0] ?? null;
        return {
          ...slot,
          streamingParts: baseParts,
          judgingRequests: baseJudging,
          pendingApproval: next,
          pendingApprovalQueue: slot.pendingApprovalQueue.slice(1),
        };
      }
      return {
        ...slot,
        streamingParts: baseParts,
        judgingRequests: baseJudging,
        pendingApprovalQueue: slot.pendingApprovalQueue.filter(
          (it) => it.requestId !== e.request_id
        ),
      };
    }

    case "permission_auto_judged": {
      // judge 出结果 → 先清掉这条审批暂存的黄色呼吸（无论后续显形与否，judge 已定论）。
      // deny Edit/Write 的提示 toast 由 useStore 调用层负责（保持本 reducer 无副作用）。
      const judging = e.request_id ? slot.judgingRequests[e.request_id] : undefined;
      const clearedParts = judging
        ? setPartJudging(slot.streamingParts, judging.callId, false)
        : slot.streamingParts;
      // requires_human=false（judge 自动放行 / 自动拒）：只清呼吸，最终 resolve 由
      // permission_resolved 兜底，这里不动 pendingApproval。
      if (!e.requires_human) {
        return { ...slot, streamingParts: clearedParts };
      }
      // requires_human=true（ASK / 普通 AutoMode 命令类 DENY）：把被 judge 接管而未显示、
      // 暂存在 judgingRequests 的审批框显形——带上 judge 的 reason 转入 pendingApproval
      // （已有框则排队），交用户最终拍板（架构 §4.4.4）。
      const clearedJudging =
        judging && e.request_id
          ? dropKey(slot.judgingRequests, e.request_id)
          : slot.judgingRequests;
      const revealed: PendingApproval | null = judging
        ? { ...judging.approval, autoJudgeReason: e.reason ?? null }
        : null;
      if (!revealed) {
        // 容错：judgingRequests 里没暂存（理论上 auto_handled 必有），只清呼吸。
        return { ...slot, streamingParts: clearedParts, judgingRequests: clearedJudging };
      }
      if (slot.pendingApproval) {
        return {
          ...slot,
          streamingParts: clearedParts,
          judgingRequests: clearedJudging,
          pendingApprovalQueue: [...slot.pendingApprovalQueue, revealed],
        };
      }
      return {
        ...slot,
        streamingParts: clearedParts,
        judgingRequests: clearedJudging,
        pendingApproval: revealed,
      };
    }

    case "step_started":
      // ModelStep 起点（架构 §4.2）：快照当前 streaming 累积，作为本 step 内 model_retry
      // 的回退基线。每个 Turn 的模型请求都发一次 StepStarted{Model}，覆盖上个快照——多
      // Turn 共用一个 bubble 时，turn N 的基线自然含 turn 1..N-1 的累积。tool step 不快照。
      if (e.step_kind === "model") {
        return {
          ...slot,
          retryBaseText: slot.streamingText,
          retryBaseParts: slot.streamingParts,
        };
      }
      return slot;

    case "step_finished":
      // Step 边界事件（架构 §4.2）：当前用于 metrics / 调试，UI 暂不渲染。
      return slot;

    case "run_mode_changed":
      // 运行模式切换通知（架构 §10.2）：更新当前 RunMode 标签，状态栏 / 顶栏可消费。
      return { ...slot, currentRunMode: e.to };

    case "turn_finished": {
      // Turn 边界（架构 §3 / §4.2）：**只在本 Turn 期间真的发生过 user 插队**时才切
      // bubble——与 chat.rs `had_pending_during_run` 落盘语义对齐：
      //   - 无插队：整个 Run 累积成一条 assistant message（多 Turn 共用一个 bubble）
      //   - 有插队：按 Turn 分段落盘，每段对应一个 assistant message
      // 判定：assistantInsertPos 之后的 liveTimeline 里出现过**分段触发项** → 切；
      // 否则维持当前 streamingText / streamingParts 累积，下个 Turn 接着 stream。
      // 分段触发项 = 真正的用户插队消息，或 cron_fired 定时唤醒。
      //   - bg_task_finished：某 tool_call 的异步回应，由 wakeup 排序钉到对应 assistant
      //     段之后，不是新对话轮次，不分段——继续累积进当前 bubble。
      //   - cron_fired：定时唤醒是一次全新的对话轮次（后端也落成独立 assistant message），
      //     必须冻结分段，否则每轮唤醒的输出全叠进同一个 bubble，无限堆叠、tool 卡片糊成一团。
      const isSegmentTrigger = (item: LiveTimelineItem): boolean => {
        if (item.kind !== "user_injected") return false;
        const meta = item.message.meta;
        if (meta?.type !== "system_notification") return true; // 真正的用户插队
        return meta.kind === "cron_fired"; // 系统通知里只有 cron 唤醒算新轮次
      };
      const hasPendingInjection = slot.liveTimeline
        .slice(slot.assistantInsertPos)
        .some(isSegmentTrigger);
      if (!hasPendingInjection) return slot;
      if (slot.streamingText.length === 0 && slot.streamingParts.length === 0) {
        // 插队消息已挂在末尾但当前 Turn 没产出（罕见，例如审批拒绝直接终止）——
        // 不冻结空 bubble，只把游标推过 timeline 末尾，下个 Turn 起新 streaming。
        return { ...slot, assistantInsertPos: slot.liveTimeline.length };
      }
      const frozen: LiveTimelineItem = {
        kind: "assistant_frozen",
        id: `frozen-${slot.requestId}-${slot.liveTimeline.length}`,
        text: slot.streamingText,
        parts: slot.streamingParts,
        created_at: Date.now(),
      };
      const next = [...slot.liveTimeline];
      next.splice(slot.assistantInsertPos, 0, frozen);
      return {
        ...slot,
        streamingText: "",
        streamingParts: [],
        liveTimeline: next,
        assistantInsertPos: next.length,
      };
    }

    case "user_question_requested": {
      const q: PendingQuestion = {
        requestId: e.request_id,
        question: e.question,
        options: e.options,
        multi: e.multi ?? false,
        questions: e.questions ?? [],
      };
      if (slot.pendingQuestion) {
        return { ...slot, pendingQuestionQueue: [...slot.pendingQuestionQueue, q] };
      }
      return { ...slot, pendingQuestion: q };
    }

    case "user_question_answered": {
      if (slot.pendingQuestion?.requestId === e.request_id) {
        const next = slot.pendingQuestionQueue[0] ?? null;
        return {
          ...slot,
          pendingQuestion: next,
          pendingQuestionQueue: slot.pendingQuestionQueue.slice(1),
        };
      }
      return {
        ...slot,
        pendingQuestionQueue: slot.pendingQuestionQueue.filter(
          (it) => it.requestId !== e.request_id
        ),
      };
    }

    case "todo_list_updated":
      // 整列表覆盖（架构 §4.4.6）
      return { ...slot, todos: e.todos };

    case "plan_ready":
      // ExitPlanMode 落盘了一份新 plan；记下当前活跃 plan。后续 permission_requested
      // 携带同一 plan 内容由 popup 直接消费。
      return {
        ...slot,
        activePlan: {
          plan_id: e.plan_id,
          plan_path: e.plan_path,
          markdown: e.plan_markdown,
          summary: e.summary,
        },
      };

    case "plan_comment_added": {
      const existing = slot.planComments[e.plan_id] ?? [];
      return {
        ...slot,
        planComments: { ...slot.planComments, [e.plan_id]: [...existing, e.comment] },
      };
    }

    default:
      // run_started / message_appended / usage / error / notice / context_compaction_* /
      // session_title_* / memory_* / goal_* / run_edits_* 等事件不挂 slot——它们在
      // useStore 事件分发上层另行处理（session 级状态 / toast / 气泡渲染等）。
      return slot;
  }
}
