import { create } from "zustand";
import type {
  AppSettings,
  ApprovalDecisionPayload,
  ContextUsage,
  EditEntry,
  EngineEvent,
  LogEntry,
  Message,
  MessageAttachment,
  MessageMeta,
  MemoryWriteItem,
  PendingApproval,
  PendingQuestion,
  PlanComment,
  Prompt,
  PromptsFile,
  Provider,
  ProvidersFile,
  QueuedInput,
  QuestionAnswerPayload,
  ReasoningConfig,
  SearchHit,
  Session,
  SessionMeta,
  StreamingAssistantPart,
  TodoItem,
  ToolInfo,
  WorkspaceProject,
  WorkspaceProjectInput,
} from "@/desktop/ui/types";
import { toast } from "sonner";
import { api } from "@/desktop/bridge/tauri";
import { appendOptimisticUserMessage } from "@/desktop/ui/store/sessionOptimism";
import { appendInjectedMessageAfterCurrentAssistant } from "@/desktop/ui/components/liveTimelineOrder";

const LAST_PROMPT_ID_KEY = "lastPromptId";
const LAST_PROVIDER_ID_KEY = "lastProviderId";
const LAST_MODEL_KEY = "lastModel";
const USER_AVATAR_KEY = "userAvatar";
// 工作区"待继承"项：用户上次在输入框 + 菜单里选过的 workdir / allowed_paths，
// 新建对话会自动应用，避免每次都重选。null / [] 表示用户清空了，新对话也保持空。
const PENDING_WORKDIR_KEY = "pendingWorkdir";
const PENDING_ALLOWED_PATHS_KEY = "pendingAllowedPaths";
// 全局规则继承：新建对话时沿用上一个对话的 global_rules 设置。
const GLOBAL_RULES_KEY = "globalRules";

function readStoredValue(key: string) {
  return localStorage.getItem(key) ?? "";
}

function readStoredWorkdir(): string | null {
  const raw = localStorage.getItem(PENDING_WORKDIR_KEY);
  return raw ? raw : null;
}

function readStoredAllowedPaths(): string[] {
  try {
    const raw = localStorage.getItem(PENDING_ALLOWED_PATHS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function persistPendingWorkdir(workdir: string | null) {
  if (workdir) {
    localStorage.setItem(PENDING_WORKDIR_KEY, workdir);
  } else {
    localStorage.removeItem(PENDING_WORKDIR_KEY);
  }
}

function persistPendingAllowedPaths(paths: string[]) {
  if (paths.length > 0) {
    localStorage.setItem(PENDING_ALLOWED_PATHS_KEY, JSON.stringify(paths));
  } else {
    localStorage.removeItem(PENDING_ALLOWED_PATHS_KEY);
  }
}

function readStoredGlobalRules(): string[] | null {
  try {
    const raw = localStorage.getItem(GLOBAL_RULES_KEY);
    if (raw === null) return null;
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : null;
  } catch {
    return null;
  }
}

function persistGlobalRules(rules: string[] | null) {
  if (rules === null) {
    localStorage.removeItem(GLOBAL_RULES_KEY);
  } else {
    localStorage.setItem(GLOBAL_RULES_KEY, JSON.stringify(rules));
  }
}

function normalizeAppSettings(settings: AppSettings): AppSettings {
  return {
    ...settings,
    general: {
      ...settings.general,
      show_grep_search_path: settings.general.show_grep_search_path ?? true,
      shell: settings.general.shell ?? null,
      edit_backend: settings.general.edit_backend ?? "string-replace",
    },
  };
}

function persistLastSessionConfig(config: {
  providerId?: string | null;
  model?: string | null;
  promptId?: string | null;
}) {
  if (config.providerId !== undefined) {
    localStorage.setItem(LAST_PROVIDER_ID_KEY, config.providerId ?? "");
  }
  if (config.model !== undefined) {
    localStorage.setItem(LAST_MODEL_KEY, config.model ?? "");
  }
  if (config.promptId !== undefined) {
    localStorage.setItem(LAST_PROMPT_ID_KEY, config.promptId ?? "");
  }
}

function cloneStreamingParts(
  parts: StreamingAssistantPart[]
): StreamingAssistantPart[] {
  return parts.map((part) => ({ ...part }));
}

function applyTextDelta(
  parts: StreamingAssistantPart[],
  text: string
): StreamingAssistantPart[] {
  if (!text) return parts;
  const next = cloneStreamingParts(parts);
  const last = next[next.length - 1];
  if (last?.type === "text") {
    last.text += text;
  } else {
    next.push({ type: "text", text });
  }
  return next;
}

/**
 * 推理（thinking）增量：贴在最近一段 reasoning 上；如果末尾不是
 * reasoning 就开新段，让正文段不会被推理打断顺序。
 */
function applyReasoningDelta(
  parts: StreamingAssistantPart[],
  text: string
): StreamingAssistantPart[] {
  if (!text) return parts;
  const next = cloneStreamingParts(parts);
  const last = next[next.length - 1];
  if (last?.type === "reasoning") {
    last.text += text;
  } else {
    next.push({ type: "reasoning", text });
  }
  return next;
}

function toolPartIndex(
  parts: StreamingAssistantPart[],
  index: number,
  id?: string | null
) {
  if (id) {
    const byId = parts.findIndex(
      (part) => part.type === "tool_call" && part.id === id
    );
    if (byId >= 0) return byId;
  }
  return parts.findIndex(
    (part) => part.type === "tool_call" && part.index === index
  );
}

function ensureToolPart(
  parts: StreamingAssistantPart[],
  index: number,
  id?: string | null,
  name?: string | null
): [StreamingAssistantPart[], number] {
  const next = cloneStreamingParts(parts);
  const existing = toolPartIndex(next, index, id);
  if (existing >= 0) return [next, existing];

  next.push({
    type: "tool_call",
    index,
    id,
    name,
    arguments: "",
    status: "streaming",
  });
  return [next, next.length - 1];
}

function applyToolCallDelta(
  parts: StreamingAssistantPart[],
  event: Extract<EngineEvent, { type: "tool_call_delta" }>
): StreamingAssistantPart[] {
  const [next, pos] = ensureToolPart(
    parts,
    event.index,
    event.id,
    event.name
  );
  const call = next[pos];
  if (call.type !== "tool_call") return next;
  next[pos] = {
    ...call,
    id: event.id ?? call.id,
    name: event.name ?? call.name,
    arguments: call.arguments + (event.arguments_delta ?? ""),
    status: call.status === "done" ? "done" : "streaming",
  };
  return next;
}

function applyToolStart(
  parts: StreamingAssistantPart[],
  event: Extract<EngineEvent, { type: "tool_start" }>
): StreamingAssistantPart[] {
  const [next, pos] = ensureToolPart(parts, event.index, event.id, event.name);
  const call = next[pos];
  if (call.type !== "tool_call") return next;
  next[pos] = {
    ...call,
    id: event.id,
    name: event.name,
    input: event.input,
    status: "running",
  };
  return next;
}

function removeFromSet<T>(s: Set<T>, item: T): Set<T> {
  if (!s.has(item)) return s;
  const next = new Set(s);
  next.delete(item);
  return next;
}

/** 从 record 删一个 key，返回新对象（key 不存在则原样返回）。 */
function dropKey<V>(rec: Record<string, V>, key: string): Record<string, V> {
  if (!(key in rec)) return rec;
  const { [key]: _drop, ...rest } = rec;
  return rest;
}

/**
 * Run 内时间线条目（架构 §4.2 + §4.12.5）。run 跑到一半时把"已完成 turn 快照"
 * 和"streaming 中插队的 user message"按真实发生顺序穿插起来，避免插队消息被
 * 错乱地黏在下一轮 assistant 输出之后。
 *
 * - `assistant_frozen`：一次 turn 的 TurnFinished 事件触发——把当时的
 *   streamingText / streamingParts 原样冻结，渲染走标准 MessageBubble
 *   （`streaming=false`，仍喂 streamingParts 复用渲染路径）
 * - `user_injected`：streaming 期间调 inject_user_message 即写即落后，
 *   后端返回的持久化 Message 直接放进来
 *
 * run 结束 reload session 时 slot 被清掉，由 session.messages 接管最终顺序。
 */
type LiveTimelineItem =
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

/**
 * 单个 session 在跑时的所有"软状态"（流式正文 / 推理 / 工具 / HITL）。
 * 全局字段（`streamingText` 等）只是 currentSession 这个槽的只读镜像。
 * 这样切到别的会话时，当前会话的状态不会被新会话的流冲掉，
 * 切回来还能看到原来的进度。
 */
type SessionStream = {
  requestId: string;
  streamingMessageId: string | null;
  streamingText: string;
  streamingParts: StreamingAssistantPart[];
  /**
   * Run 内时间线：当前 Run 内已完成的 turn 快照 + 期间插队的 user message，
   * 按发生顺序排好。渲染时整段排在持久化 messages 之后、当前 streaming bubble 之前。
   *
   * `assistantInsertPos` 维护"下一个 TurnFinished 快照应该插在哪儿"——
   * 每次 TurnFinished 时把当前 streaming 内容插入到这个位置，然后把游标
   * 推到末尾，使得之后到达的 user injection 排在该 turn 之后、下一个 turn 之前。
   */
  liveTimeline: LiveTimelineItem[];
  assistantInsertPos: number;
  pendingApproval: PendingApproval | null;
  pendingApprovalQueue: PendingApproval[];
  pendingQuestion: PendingQuestion | null;
  pendingQuestionQueue: PendingQuestion[];
  /** AutoMode judge 在 streaming 中产生的判官标记。ChatView 渲染气泡用。 */
  autoJudgedNotes: AutoJudgedNote[];
  /** 当前对话的 RunMode 字符串（由 run_mode_changed event 维护）。 */
  currentRunMode: string | null;
  /** 架构 §4.12：Run 当前是否处于挂起态。`null` = active；非空 = 已挂起。 */
  suspended: SuspendedInfo | null;
  /**
   * TodoWrite 维护的 todo 列表（架构 §4.4.6）。
   * 落盘到 session.jsonl，启动时由 list_todos 拉一次，之后跟 todo_list_updated 事件增量。
   */
  todos: TodoItem[];
  /**
   * 当前活跃 plan（架构 §4.4.5）。`null` = 当前没有 plan。
   * - 通过 plan_ready 事件设置；ExitPlanMode 审批通过/拒绝后保留在 store 里供回看
   * - markdown 是初始版本，用户走"编辑后通过"路径会让后端覆盖 plan 文件
   */
  activePlan: { plan_id: string; plan_path: string; markdown: string; summary: string } | null;
  /** Plan id → 该 plan 的评论列表。前端在 plan tab / 审批 popup 渲染。 */
  planComments: Record<string, PlanComment[]>;
};

export type SuspendedInfo = {
  /** "background_task" / "cron" / "manual" */
  reason: string;
  /** cron 路径：自动唤醒时间（Unix ms）。 */
  resumesAtMs?: number | null;
  /** bg-task 路径：等的 task_id 列表。 */
  waitingForTaskIds: string[];
  /** 挂起时间，用于 UI 显示「已挂起 N s」。 */
  suspendedAtMs: number;
};

export type AutoJudgedNote = {
  toolName: string;
  decision: string;
  reason?: string | null;
};

const EMPTY_MIRROR = {
  streamingMessageId: null as string | null,
  streamingText: "",
  streamingParts: [] as StreamingAssistantPart[],
  liveTimeline: [] as LiveTimelineItem[],
  assistantInsertPos: 0,
  activeRequestId: null as string | null,
  pendingApproval: null as PendingApproval | null,
  pendingApprovalQueue: [] as PendingApproval[],
  pendingQuestion: null as PendingQuestion | null,
  pendingQuestionQueue: [] as PendingQuestion[],
  autoJudgedNotes: [] as AutoJudgedNote[],
  currentRunMode: null as string | null,
  suspended: null as SuspendedInfo | null,
  todos: [] as TodoItem[],
  activePlan: null as SessionStream["activePlan"],
  planComments: {} as Record<string, PlanComment[]>,
};

function mirrorFromSlot(slot: SessionStream | undefined) {
  if (!slot) return { ...EMPTY_MIRROR };
  return {
    streamingMessageId: slot.streamingMessageId,
    streamingText: slot.streamingText,
    streamingParts: slot.streamingParts,
    liveTimeline: slot.liveTimeline,
    assistantInsertPos: slot.assistantInsertPos,
    activeRequestId: slot.requestId,
    pendingApproval: slot.pendingApproval,
    pendingApprovalQueue: slot.pendingApprovalQueue,
    pendingQuestion: slot.pendingQuestion,
    pendingQuestionQueue: slot.pendingQuestionQueue,
    autoJudgedNotes: slot.autoJudgedNotes,
    currentRunMode: slot.currentRunMode,
    suspended: slot.suspended,
    todos: slot.todos,
    activePlan: slot.activePlan,
    planComments: slot.planComments,
  };
}

function applyEventToSlot(slot: SessionStream, e: EngineEvent): SessionStream {
  // 子 agent 事件：路由到对应 Task tool call 的 nested_parts（架构 §4.4.11.8）
  const nestedCallId = "subagent_call_id" in e ? e.subagent_call_id : undefined;
  if (nestedCallId) {
    return {
      ...slot,
      streamingParts: applyNestedEvent(slot.streamingParts, nestedCallId, e),
    };
  }
  if (e.type === "text_delta") {
    if (!e.text) return slot;
    return {
      ...slot,
      streamingText: slot.streamingText + e.text,
      streamingParts: applyTextDelta(slot.streamingParts, e.text),
    };
  }
  if (e.type === "text_done") {
    if (!slot.streamingText || !e.full_text.endsWith(slot.streamingText)) {
      const delta = slot.streamingText ? "" : e.full_text;
      return {
        ...slot,
        streamingText: e.full_text,
        streamingParts: delta
          ? applyTextDelta(slot.streamingParts, delta)
          : slot.streamingParts,
      };
    }
    return slot;
  }
  if (e.type === "reasoning") {
    return { ...slot, streamingParts: applyReasoningDelta(slot.streamingParts, e.text) };
  }
  if (e.type === "tool_call_delta") {
    return { ...slot, streamingParts: applyToolCallDelta(slot.streamingParts, e) };
  }
  if (e.type === "tool_start") {
    return { ...slot, streamingParts: applyToolStart(slot.streamingParts, e) };
  }
  if (e.type === "tool_done") {
    return { ...slot, streamingParts: applyToolDone(slot.streamingParts, e) };
  }
  if (e.type === "tool_output_delta") {
    return { ...slot, streamingParts: applyToolOutputDelta(slot.streamingParts, e) };
  }
  if (e.type === "run_suspended") {
    return {
      ...slot,
      suspended: {
        reason: e.reason,
        resumesAtMs: e.resumes_at_ms ?? null,
        waitingForTaskIds: e.waiting_for_task_ids ?? [],
        suspendedAtMs: Date.now(),
      },
    };
  }
  if (e.type === "run_resumed") {
    return { ...slot, suspended: null };
  }
  if (e.type === "permission_requested") {
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
      plan: e.plan ?? null,
    };
    if (slot.pendingApproval) {
      return { ...slot, pendingApprovalQueue: [...slot.pendingApprovalQueue, approval] };
    }
    return { ...slot, pendingApproval: approval };
  }
  if (e.type === "permission_resolved") {
    if (slot.pendingApproval?.requestId === e.request_id) {
      const next = slot.pendingApprovalQueue[0] ?? null;
      return {
        ...slot,
        pendingApproval: next,
        pendingApprovalQueue: slot.pendingApprovalQueue.slice(1),
      };
    }
    return {
      ...slot,
      pendingApprovalQueue: slot.pendingApprovalQueue.filter(
        (it) => it.requestId !== e.request_id
      ),
    };
  }
  if (e.type === "permission_auto_judged") {
    // AutoMode 判官标记（架构 §4.4.4）：累积到 slot.autoJudgedNotes，ChatView 渲染气泡。
    return {
      ...slot,
      autoJudgedNotes: [
        ...slot.autoJudgedNotes,
        {
          toolName: e.tool_name,
          decision: e.decision,
          reason: e.reason ?? null,
        },
      ],
    };
  }
  if (e.type === "step_started" || e.type === "step_finished") {
    // Step 边界事件（架构 §4.2）：当前用于 metrics / 调试，UI 暂不渲染。
    return slot;
  }
  if (e.type === "run_mode_changed") {
    // 运行模式切换通知（架构 §10.2）：更新当前 RunMode 标签，状态栏 / 顶栏可消费。
    return { ...slot, currentRunMode: e.to };
  }
  if (e.type === "turn_finished") {
    // Turn 边界（架构 §3 / §4.2）：**只在本 Turn 期间真的发生过 user 插队**时才切
    // bubble——与 chat.rs `had_pending_during_run` 落盘语义对齐：
    //   - 无插队：整个 Run 累积成一条 assistant message（多 Turn 共用一个 bubble）
    //   - 有插队：按 Turn 分段落盘，每段对应一个 assistant message
    // 判定：assistantInsertPos 之后的 liveTimeline 里出现过 user_injected → 切；
    // 否则维持当前 streamingText / streamingParts 累积，下个 Turn 接着 stream。
    const hasPendingInjection = slot.liveTimeline
      .slice(slot.assistantInsertPos)
      .some((item) => item.kind === "user_injected");
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
  if (e.type === "user_question_requested") {
    const q: PendingQuestion = {
      requestId: e.request_id,
      question: e.question,
      options: e.options,
      multi: e.multi ?? false,
    };
    if (slot.pendingQuestion) {
      return { ...slot, pendingQuestionQueue: [...slot.pendingQuestionQueue, q] };
    }
    return { ...slot, pendingQuestion: q };
  }
  if (e.type === "user_question_answered") {
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
  if (e.type === "todo_list_updated") {
    // 整列表覆盖（架构 §4.4.6）
    return { ...slot, todos: e.todos };
  }
  if (e.type === "plan_ready") {
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
  }
  if (e.type === "plan_comment_added") {
    const existing = slot.planComments[e.plan_id] ?? [];
    return {
      ...slot,
      planComments: { ...slot.planComments, [e.plan_id]: [...existing, e.comment] },
    };
  }
  // edit_snapshot_created / edit_reverted / edit_revert_failed 不挂 slot——
  // editSnapshots 是 session-scoped 的持久化镜像（架构 §4.13），跟 run 生命周期
  // 解耦。这三类事件在事件分发的上层另行写入 sessionEditSnapshots。
  return slot;
}

/**
 * 从 session.active_plan 绝对路径反推前端 store 用的 activePlan 形状。
 * markdown 字段留空，由 PlanTab 打开时再 read_plan_markdown 懒加载填充。
 */
function activePlanFromPath(
  planPath: string,
): { plan_id: string; plan_path: string; markdown: string; summary: string } | null {
  const planId =
    planPath
      .split(/[/\\]/)
      .pop()
      ?.replace(/\.md$/, "") ?? "";
  if (!planId) return null;
  return { plan_id: planId, plan_path: planPath, markdown: "", summary: "" };
}

/**
 * 给指定 sessionId 的 slot 打一个 patch；slot 不存在则用 EMPTY_MIRROR + sensible
 * defaults 起一个新 slot（只放 todo / plan 这类 idempotent 状态，不掺 streaming）。
 * 若该 session 当前是 currentSession，同步刷顶层镜像字段。
 */
function patchSessionSlot(
  set: (
    partial:
      | Partial<AppState>
      | ((state: AppState) => Partial<AppState> | AppState)
  ) => void,
  get: () => AppState,
  sessionId: string,
  patch: (slot: SessionStream) => SessionStream,
) {
  set((state) => {
    const prev = state.sessionStreams[sessionId];
    const base: SessionStream =
      prev ??
      ({
        requestId: "",
        streamingMessageId: null,
        streamingText: "",
        streamingParts: [],
        liveTimeline: [],
        assistantInsertPos: 0,
        pendingApproval: null,
        pendingApprovalQueue: [],
        pendingQuestion: null,
        pendingQuestionQueue: [],
        autoJudgedNotes: [],
        currentRunMode: null,
        suspended: null,
        todos: [],
        activePlan: null,
        planComments: {},
      } satisfies SessionStream);
    const next = patch(base);
    const isCurrent = state.currentSession?.id === sessionId;
    return {
      sessionStreams: { ...state.sessionStreams, [sessionId]: next },
      ...(isCurrent ? mirrorFromSlot(next) : {}),
    };
  });
  // 避免某些 closure 不复制 get 引用——保留参数兼容 zustand 旧 API。
  void get;
}

/** edit 类事件 → 顶层 sessionEditSnapshots 增量。返回新 record 或 null（无变化）。 */
function applyEditEvent(
  current: Record<string, EditEntry[]>,
  sessionId: string,
  e: EngineEvent,
): Record<string, EditEntry[]> | null {
  if (e.type === "edit_snapshot_created") {
    const existing = current[sessionId] ?? [];
    // 去重：补全量拉取后又收到同名事件时，避免重复 push
    if (existing.some((x) => x.snapshot_id === e.snapshot_id)) return null;
    const next: EditEntry[] = [
      ...existing,
      {
        snapshot_id: e.snapshot_id,
        call_id: e.call_id,
        tool: "Edit",
        real_path: e.file_path,
        action: e.action,
        before_sha: e.before_sha,
        after_sha: e.after_sha,
        before_bytes: e.before_bytes,
        after_bytes: e.after_bytes,
        ts_ms: Date.now(),
        reverted: false,
      },
    ];
    return { ...current, [sessionId]: next };
  }
  if (e.type === "edit_reverted") {
    const existing = current[sessionId];
    if (!existing) return null;
    const next = existing.map((entry) =>
      entry.snapshot_id === e.snapshot_id
        ? { ...entry, reverted: true, reverted_at_ms: Date.now() }
        : entry,
    );
    return { ...current, [sessionId]: next };
  }
  return null;
}

function activeRequestForSession(state: AppState, sessionId: string): string | null {
  return state.sessionStreams[sessionId]?.requestId ?? null;
}

function waitForRequestRelease(
  get: () => AppState,
  sessionId: string,
  requestId: string
): Promise<void> {
  return new Promise((resolve) => {
    const startedAt = Date.now();
    const tick = () => {
      const active = get().sessionStreams[sessionId]?.requestId;
      if (active !== requestId || Date.now() - startedAt > 10_000) {
        resolve();
        return;
      }
      setTimeout(tick, 16);
    };
    tick();
  });
}

function appendUserInjectedMessage(
  slot: SessionStream,
  message: Message
): SessionStream {
  return {
    ...slot,
    liveTimeline: [...slot.liveTimeline, { kind: "user_injected", message }],
  };
}

function insertSystemNotificationBeforeNextAssistant(
  slot: SessionStream,
  message: Message
): SessionStream {
  const next = appendInjectedMessageAfterCurrentAssistant<
    LiveTimelineItem,
    StreamingAssistantPart
  >(
    {
      liveTimeline: slot.liveTimeline,
      assistantInsertPos: slot.assistantInsertPos,
      streamingText: slot.streamingText,
      streamingParts: slot.streamingParts,
    },
    { kind: "user_injected", message }
  );

  return {
    ...slot,
    liveTimeline: next.liveTimeline,
    assistantInsertPos: next.assistantInsertPos,
    streamingText: next.streamingText,
    streamingParts: next.streamingParts,
  };
}

function applyToolDone(
  parts: StreamingAssistantPart[],
  event: Extract<EngineEvent, { type: "tool_done" }>
): StreamingAssistantPart[] {
  const [next, pos] = ensureToolPart(parts, event.index, event.id);
  const call = next[pos];
  if (call.type !== "tool_call") return next;
  next[pos] = {
    ...call,
    id: event.id,
    result: event.result,
    duration_ms: event.duration_ms,
    status: "done",
    artifact_path: event.artifact_path ?? null,
  };
  return next;
}

/**
 * 工具执行期间的流式输出片段——把 chunk 累加到对应 tool_call part 的 live_output。
 * 顺序保证：dispatcher 先 emit tool_start，本事件之后；finished 前都可能来多次。
 */
function applyToolOutputDelta(
  parts: StreamingAssistantPart[],
  event: Extract<EngineEvent, { type: "tool_output_delta" }>
): StreamingAssistantPart[] {
  if (!event.chunk) return parts;
  const [next, pos] = ensureToolPart(parts, event.index, event.id);
  const call = next[pos];
  if (call.type !== "tool_call") return next;
  next[pos] = {
    ...call,
    live_output: (call.live_output ?? "") + event.chunk,
  };
  return next;
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

interface AppState {
  // providers
  providersFile: ProvidersFile;
  // prompts
  promptsFile: PromptsFile;
  prompts: Prompt[];
  pendingPromptId: string;
  userAvatar: string;
  // sessions
  sessions: SessionMeta[];
  currentSession: Session | null;

  /**
   * 每个 session 当前的流式 / HITL 软状态，按 sessionId 分槽。
   * 切换会话时不动这里的内容，只是把全局镜像换成新会话槽的副本。
   */
  sessionStreams: Record<string, SessionStream>;

  // streaming —— 全局字段是 sessionStreams[currentSession.id] 的镜像
  streamingMessageId: string | null;
  streamingText: string;
  streamingParts: StreamingAssistantPart[];
  /**
   * Run 内时间线（架构 §4.2 + §4.12.5）：已完成 turn 快照 + streaming 期间
   * 插队的 user message，按真实顺序。ChatView 据此把"插队 → 下个 turn 输出"
   * 渲染成正确的因果次序。镜像自 currentSession 的 slot。
   */
  liveTimeline: LiveTimelineItem[];
  assistantInsertPos: number;
  activeRequestId: string | null;
  /** 当前对话 AutoMode 判官累计标记（镜像自 currentSession 的 slot）。 */
  autoJudgedNotes: AutoJudgedNote[];
  /** 当前对话 RunMode 字符串。`null` 表示未收到过 RunModeChanged 事件。 */
  currentRunMode: string | null;
  /** 架构 §4.12：当前对话 Run 是否被挂起。 */
  suspended: SuspendedInfo | null;
  /** 当前对话的 todo 列表（镜像自 slot）。 */
  todos: TodoItem[];
  /** 当前对话的活跃 plan（镜像自 slot）。 */
  activePlan: SessionStream["activePlan"];
  /** 当前对话的 plan_id → 评论列表（镜像自 slot）。 */
  planComments: Record<string, PlanComment[]>;
  /**
   * 架构 §4.13：每个 session 的 Edit 快照列表。
   * - 持久化真相源是后端 `~/.hebbian/sessions/<sid>/edits-worktree/.hebbian-edits.json`
   * - 增量由 EditSnapshotCreated / EditReverted 事件维护
   * - 全量由 `refreshEdits` 在 openSession 时拉一次
   * - **session-scoped**：跟 run 生命周期解耦（run 结束 slot 删除时不会跟着清掉）
   */
  sessionEditSnapshots: Record<string, EditEntry[]>;

  /**
   * 架构 §4.14：每个 session「最近一个 Run 后台抽取写入的记忆」。
   * - 由 `memory_extracted` 事件维护，下一个 Run 开始时清空（只展示「本轮」）。
   * - **session-scoped**：run 结束 slot 删除时不跟着清掉，故单列在顶层而非 slot 内。
   * - 空数组 / 不存在 = 不渲染摘要行。
   */
  sessionMemoryWrites: Record<string, MemoryWriteItem[]>;

  /** 后端正在跑（含前台 + 后台）的会话 id 集合，用于 Sidebar 呼吸点。 */
  runningSessions: Set<string>;
  /** 后台跑完但用户尚未查看的会话 id 集合，用于 Sidebar 静态点。 */
  unreadFinishedSessions: Set<string>;
  /** agent loop 异常退出（模型请求失败等）时记录的会话 id，用于输入框上方 suggestion。发新消息时自动清空。*/
  lastRunError: { sessionId: string } | null;

  // UI
  providerDialogOpen: boolean;
  settingsOpen: boolean;
  promptsDialogOpen: boolean;

  // search
  searchQuery: string;
  searchResults: SearchHit[] | null;
  searchCaseSensitive: boolean;
  searchRegex: boolean;
  searching: boolean;

  // theme
  theme: "light" | "dark";

  // tools — Agent 工具系统
  /** 所有可用工具的元信息（从后端加载） */
  availableTools: ToolInfo[];
  /** 当前对话启用的工具名称集合 */
  enabledTools: Set<string>;

  // 上下文用量（输入框旁环形进度条数据）
  contextUsage: ContextUsage | null;
  /** 是否正在执行 /compact */
  compacting: boolean;
  refreshContextUsage: () => Promise<void>;
  compactCurrentSession: (customInstructions?: string) => Promise<void>;

  // ── edits worktree（架构 §4.13）──
  /** 回退单次 Edit（调 Tauri revert_edit 命令）。成功/失败直接在 UI 展示 toast。 */
  revertEdit: (sessionId: string, snapshotId: string) => Promise<void>;
  /** 从后端重新加载当前 session 的 edits 条目列表。 */
  refreshEdits: () => Promise<void>;

  // ── Todo / Plan（架构 §4.4.5 / §4.4.6）──
  /** 用整列表覆盖指定 session slot 的 todos——TodoTab 拉初值 / 修复重连时用。 */
  replaceSessionStreamTodos: (sessionId: string, todos: TodoItem[]) => void;
  /** 设定指定 session 的"活跃 plan"快照——PlanTab 切换历史 plan 时用。 */
  setSessionActivePlan: (
    sessionId: string,
    plan: { plan_id: string; plan_path: string; markdown: string; summary: string } | null,
  ) => void;
  /** 用整列表覆盖 sessionId / planId 下的 comments——PlanTab 拉初值时用。 */
  replaceSessionPlanComments: (
    sessionId: string,
    planId: string,
    comments: PlanComment[],
  ) => void;
  /** 给某 plan 追加一条本地评论（add_plan_comment 命令返回后调用）。 */
  appendSessionPlanComment: (
    sessionId: string,
    planId: string,
    comment: PlanComment,
  ) => void;

  // HITL — 当前一轮 run 中悬挂的审批请求
  pendingApproval: PendingApproval | null;
  pendingApprovalQueue: PendingApproval[];
  resolveApproval: (decision: ApprovalDecisionPayload) => Promise<void>;
  // HITL — 当前一轮 run 中悬挂的 agent 提问（ask 工具）
  pendingQuestion: PendingQuestion | null;
  pendingQuestionQueue: PendingQuestion[];
  resolveQuestion: (answer: QuestionAnswerPayload) => Promise<void>;

  // 架构 §4.12.6：后端 WakeupScheduler 触发的 wakeup XML + 结构化 meta，
  // 按 sessionId 暂存。
  // - 若 wakeup 到达时该 session 是 currentSession，立刻自动发出
  // - 否则暂存到这里，下次 openSession 时消费
  // meta 跟着 xml 一起暂存——切回 session 触发时仍需要 meta 标识系统通知样式。
  pendingWakeups: Record<string, { xml: string; meta: MessageMeta }>;
  /** 自动唤醒：把 wakeup XML 作 user message 发给该 session（不论前后台）。 */
  triggerWakeupResume: (
    sessionId: string,
    wakeupXml: string,
    meta: MessageMeta
  ) => Promise<void>;
  /** 给指定 session 排队一条 wakeup XML + meta，等 openSession 时消费。 */
  queueWakeupForSession: (sessionId: string, wakeupXml: string, meta: MessageMeta) => void;

  // 运行时输入队列：每个 session 一条 FIFO 队列，streaming 期间用户排进的
  // 后续 user message 暂存于此，当前 turn 跑完后自动按顺序消费。
  inputQueues: Record<string, QueuedInput[]>;
  /** 当前 session 队列的镜像（按 currentSession.id 跟随）。 */
  currentInputQueue: QueuedInput[];
  /**
   * 入队一条新输入。
   * - position='tail'（默认）：append 队尾，常规排队。
   * - position='head'：prepend 队首，对应 Shift+Enter「立即」语义——
   *   让它最先被消费。
   */
  enqueueInput: (
    content: string,
    attachments: MessageAttachment[],
    position?: "tail" | "head"
  ) => void;
  removeQueuedInput: (id: string) => void;
  /**
   * 「引导」语义（架构.md §4.2.3）：把指定的 queued 项注入到当前 run 的
   * PendingInputs 队列——agent_loop 在下一次 ModelStep 之前 drain，等价于
   * "当前 model_call + tool_call 完成后立即插队"。
   * 不限队首；任意位置可点。仅在当前 session 还在 streaming 时有意义。
   * 不传 id 默认取队首（保持 Shift+Enter 行为）。
   */
  flushQueuedItem: (id?: string) => Promise<void>;
  /**
   * 「放回输入框」：把指定 queued 项从 next_run_queue 移除，并把它的
   * content / attachments 追加到 ChatInput 草稿（composerDraft）。
   * ChatInput 的 useEffect 会消费 composerDraft 并 clear。
   */
  returnQueuedToComposer: (id: string) => void;
  /**
   * ChatInput 待回填的草稿：被 returnQueuedToComposer 写入，
   * ChatInput 消费后调 clearComposerDraft 清掉。
   */
  composerDraft: { content: string; attachments: MessageAttachment[] } | null;
  clearComposerDraft: () => void;

  // actions
  init: () => Promise<void>;
  refreshProviders: () => Promise<void>;
  saveProviders: (file: ProvidersFile) => Promise<void>;
  upsertProvider: (p: Provider) => Promise<void>;
  refreshPrompts: () => Promise<void>;
  upsertPrompt: (p: Prompt) => Promise<void>;
  deletePrompt: (id: string) => Promise<void>;
  setDefaultPrompt: (id: string | null) => Promise<void>;

  refreshSessions: () => Promise<void>;
  openSession: (id: string) => Promise<void>;
  projects: WorkspaceProject[];
  projectSidebarMode: "projects" | "all";
  selectedProjectId: string | null;
  refreshProjects: () => Promise<void>;
  saveProject: (input: WorkspaceProjectInput) => Promise<WorkspaceProject>;
  deleteProject: (id: string) => Promise<void>;
  importVscodeProject: (content: string, name?: string | null) => Promise<WorkspaceProject>;
  importProjectFile: (path: string) => Promise<WorkspaceProject>;
  setProjectSidebarMode: (mode: "projects" | "all") => void;
  openProject: (id: string) => void;
  closeProject: () => void;
  newSession: (opts?: {
    providerId?: string;
    model?: string;
    promptId?: string;
    projectId?: string | null;
  }) => Promise<void>;
  renameSession: (id: string, title: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  forkSession: (msgId: string) => Promise<void>;
  regenerateTitle: () => Promise<void>;

  /**
   * 发送 user message 并触发 run。`meta` 可选——为 wakeup notification 等系统注入
   * 走 idle 路径时用，会被透传到后端落盘的 user message 上（架构 §4.12.5）。
   */
  sendUserMessage: (
    content: string,
    attachments?: MessageAttachment[],
    meta?: MessageMeta | null,
    options?: { skipOptimisticUser?: boolean }
  ) => Promise<void>;
  cancelStreaming: () => Promise<void>;
  regenerateFrom: (assistantMsgId: string) => Promise<void>;
  /** 用同样内容重跑指定的 user 消息（被中断或失败时用）。 */
  regenerateFromUser: (userMsgId: string) => Promise<void>;
  /** 编辑指定 user 消息后重跑：截断到该消息（不含其本身之后的内容），再发送新内容。 */
  editAndRerun: (
    userMsgId: string,
    content: string,
    attachments?: MessageAttachment[]
  ) => Promise<void>;
  updateCurrentConfig: (patch: {
    provider_id?: string;
    model?: string;
    system_prompt?: string;
    prompt_id?: string;
    stream?: boolean;
    /** 设为对象更新；设为 null 显式清空。`undefined` 不动。 */
    reasoning?: ReasoningConfig | null;
  }) => Promise<void>;
  switchProviderModel: (providerId: string, model: string) => Promise<void>;
  /** 仅更新当前 session 的推理配置；传 null 重置为「沿用模型默认」。 */
  setReasoning: (reasoning: ReasoningConfig | null) => Promise<void>;

  setProviderDialogOpen: (v: boolean) => void;
  setSettingsOpen: (v: boolean) => void;
  setPromptsDialogOpen: (v: boolean) => void;
  /** 应用级设置窗口（通用 / 对话 / agent 三个 tab） */
  appSettingsOpen: boolean;
  setAppSettingsOpen: (v: boolean) => void;
  appSettings: AppSettings | null;
  refreshAppSettings: () => Promise<void>;
  saveAppSettings: (settings: AppSettings) => Promise<void>;
  /** 更新当前对话的设置（workdir / allowed_paths / enabled_tools / skill_dirs / global_rules / rules_files） */
  updateCurrentSessionSettings: (patch: {
    workdir?: string | null;
    allowed_paths?: string[] | null;
    enabled_tools?: string[] | null;
    skill_dirs?: string[] | null;
    global_rules?: string[] | null;
    rules_files?: import("@/desktop/ui/types").RuleFileState[] | null;
  }) => Promise<void>;
  /**
   * "待继承"的工作区配置：输入框左下 + 菜单选择的项目 / 目录会落到这里，
   * 新建对话时自动写入新 session。当前 session 已经存在时，setter 会同时
   * 写到本地（持久化）和 session（updateSessionSettings），让用户的修改即时生效。
   */
  pendingWorkdir: string | null;
  pendingAllowedPaths: string[];
  setPendingWorkdir: (workdir: string | null) => Promise<void>;
  setPendingAllowedPaths: (dirs: string[]) => Promise<void>;
  /** PathAccess 审批专用 */
  resolvePathAccess: (
    scope: "once" | "this_session" | "this_project" | "global"
  ) => Promise<void>;
  setPendingPromptId: (v: string) => void;
  setUserAvatar: (v: string) => void;
  toggleTheme: () => void;
  /**
   * 「日志」开关：开启后右侧工作台会显示 Model I/O 入口。
   * 仅前端 UI 行为开关，不改变后端日志落盘策略（那个走 HEBBIAN_DUMP_MODEL_IO 环境变量）。
   */
  debugEnabled: boolean;
  setDebugEnabled: (v: boolean) => void;

  /** 工具调度日志开关 */
  logEnabled: boolean;
  setLogEnabled: (v: boolean) => void;
  /** 日志条目缓冲区（上限 5000） */
  logEntries: LogEntry[];
  appendLogEntry: (line: LogEntry) => void;
  clearLogEntries: () => void;

  runSearch: (
    query: string,
    caseSensitive?: boolean,
    regex?: boolean
  ) => Promise<void>;
  clearSearch: () => void;

  /** 开启或关闭某个工具 */
  toggleTool: (name: string) => void;

  pickDefaultProvider: () => Provider | undefined;
}

function applyTheme(t: "light" | "dark") {
  if (t === "dark") document.documentElement.classList.add("dark");
  else document.documentElement.classList.remove("dark");
}

const LOG_CAPACITY = 5000;

export const useStore = create<AppState>((set, get) => ({
  providersFile: { providers: [], default_provider_id: null },
  promptsFile: { prompts: [], default_prompt_id: null },
  prompts: [],
  pendingPromptId: readStoredValue(LAST_PROMPT_ID_KEY),
  userAvatar: readStoredValue(USER_AVATAR_KEY),
  sessions: [],
  projects: [],
  projectSidebarMode: "all",
  selectedProjectId: null,
  currentSession: null,
  sessionStreams: {},
  streamingMessageId: null,
  streamingText: "",
  streamingParts: [],
  liveTimeline: [],
  assistantInsertPos: 0,
  activeRequestId: null,
  autoJudgedNotes: [],
  currentRunMode: null,
  suspended: null,
  todos: [],
  activePlan: null,
  planComments: {},
  runningSessions: new Set<string>(),
  unreadFinishedSessions: new Set<string>(),
  lastRunError: null,
  providerDialogOpen: false,
  settingsOpen: false,
  promptsDialogOpen: false,
  searchQuery: "",
  searchResults: null,
  searchCaseSensitive: false,
  searchRegex: false,
  searching: false,
  theme: (localStorage.getItem("theme") as any) ?? "light",
  debugEnabled: localStorage.getItem("hebbian.debugEnabled") === "1",
  logEnabled: localStorage.getItem("hebbian.logEnabled") === "1",
  logEntries: [],
  availableTools: [],
  // 默认只开启搜索/抓取；生图等额外工具需要用户手动开启
  enabledTools: new Set<string>(
    JSON.parse(localStorage.getItem("enabledTools") ?? '["WebSearch","Fetch"]')
  ),

  pendingWorkdir: readStoredWorkdir(),
  pendingAllowedPaths: readStoredAllowedPaths(),
  async setPendingWorkdir(workdir) {
    persistPendingWorkdir(workdir);
    set({ pendingWorkdir: workdir });
    const cur = get().currentSession;
    if (cur) {
      // 立刻把变更写到当前 session，让本对话发的消息也用新 workdir。
      // 写完后强制 getSession，避免内存对象与磁盘不同步（清空时 Tauri
      // `Option<Option<T>>` 反序列化对 null 不友好，靠 getSession 验证最终状态）。
      try {
        await api.updateSessionSettings(cur.id, { workdir });
      } catch {
        // 写入失败仍然保留 pending，让用户可以重试；session 端就保留旧值
      }
      try {
        const fresh = await api.getSession(cur.id, activeRequestForSession(get(), cur.id));
        set({ currentSession: fresh });
      } catch {
        /* ignore */
      }
    }
  },
  async setPendingAllowedPaths(dirs) {
    const next = Array.from(new Set(dirs));
    persistPendingAllowedPaths(next);
    set({ pendingAllowedPaths: next });
    const cur = get().currentSession;
    if (cur) {
      try {
        await api.updateSessionSettings(cur.id, {
          allowed_paths: next.length === 0 ? null : next,
        });
      } catch {
        /* ignore */
      }
      try {
        const fresh = await api.getSession(cur.id, activeRequestForSession(get(), cur.id));
        set({ currentSession: fresh });
      } catch {
        /* ignore */
      }
    }
  },

  contextUsage: null,
  compacting: false,
  sessionEditSnapshots: {},
  sessionMemoryWrites: {},
  async refreshContextUsage() {
    const cur = get().currentSession;
    if (!cur) {
      set({ contextUsage: null });
      return;
    }
    try {
      const usage = await api.getContextUsage(cur.id);
      set({ contextUsage: usage });
    } catch {
      // 静默失败：拿不到用量也别影响正常流程
    }
  },
  async compactCurrentSession(customInstructions?: string) {
    const cur = get().currentSession;
    if (!cur || get().compacting) return;
    set({ compacting: true });
    try {
      const usage = await api.compactSession(cur.id, customInstructions);
      const fresh = await api.getSession(cur.id, activeRequestForSession(get(), cur.id));
      set({ contextUsage: usage, currentSession: fresh });
    } finally {
      set({ compacting: false });
    }
  },

  async revertEdit(sessionId: string, snapshotId: string) {
    const result = await api.revertEdit(sessionId, snapshotId);
    if (result.success) {
      // 立刻给当前列表打 reverted 标，UI 不用等后端 refresh 才置灰
      set((state) => {
        const existing = state.sessionEditSnapshots[sessionId];
        if (!existing) return state;
        const next = existing.map((e) =>
          e.snapshot_id === snapshotId
            ? { ...e, reverted: true, reverted_at_ms: Date.now() }
            : e,
        );
        return {
          sessionEditSnapshots: { ...state.sessionEditSnapshots, [sessionId]: next },
        };
      });
      // 用后端权威数据兜底（修正 reverted_at_ms 等字段）
      const cur = get().currentSession;
      if (cur?.id === sessionId) {
        get().refreshEdits();
      }
    } else {
      throw new Error(result.error ?? "回退失败");
    }
  },

  async refreshEdits() {
    const cur = get().currentSession;
    if (!cur) return;
    try {
      const entries = await api.listEdits(cur.id);
      set((state) => ({
        sessionEditSnapshots: { ...state.sessionEditSnapshots, [cur.id]: entries },
      }));
    } catch {
      // 静默——edits-worktree 不可用时不动旧数据
    }
  },

  // ── Todo / Plan slot 写入器（架构 §4.4.5 / §4.4.6）──
  // 共用 patchSlot：拿 slot snapshot → 写回 → 若是 currentSession 同步镜像
  replaceSessionStreamTodos(sessionId: string, todos: TodoItem[]) {
    patchSessionSlot(set, get, sessionId, (slot) => ({ ...slot, todos }));
  },
  setSessionActivePlan(sessionId, plan) {
    patchSessionSlot(set, get, sessionId, (slot) => ({ ...slot, activePlan: plan }));
  },
  replaceSessionPlanComments(sessionId, planId, comments) {
    patchSessionSlot(set, get, sessionId, (slot) => ({
      ...slot,
      planComments: { ...slot.planComments, [planId]: comments },
    }));
  },
  appendSessionPlanComment(sessionId, planId, comment) {
    patchSessionSlot(set, get, sessionId, (slot) => {
      const existing = slot.planComments[planId] ?? [];
      return {
        ...slot,
        planComments: { ...slot.planComments, [planId]: [...existing, comment] },
      };
    });
  },

  pendingApproval: null,
  pendingApprovalQueue: [],
  async resolveApproval(decision: ApprovalDecisionPayload) {
    const cur = get().currentSession;
    if (!cur) return;
    const sessionId = cur.id;
    const slot = get().sessionStreams[sessionId];
    const pending = slot?.pendingApproval;
    if (!slot || !pending) return;
    const nextSlot: SessionStream = {
      ...slot,
      pendingApproval: slot.pendingApprovalQueue[0] ?? null,
      pendingApprovalQueue: slot.pendingApprovalQueue.slice(1),
    };
    set((state) => ({
      sessionStreams: { ...state.sessionStreams, [sessionId]: nextSlot },
      ...mirrorFromSlot(nextSlot),
    }));
    console.info("[permission.approval] frontend submitting tool approval", {
      sessionId,
      requestId: pending.requestId,
      kind: pending.kind,
      toolName: pending.toolName,
      decision: decision.kind,
      scope: decision.kind === "allow_and_remember" ? decision.scope ?? "session" : undefined,
      pattern: decision.kind === "allow_and_remember" ? decision.pattern ?? null : null,
      extraPatterns: decision.kind === "allow_and_remember" ? decision.extraPatterns ?? [] : [],
    });
    try {
      await api.approvePermission(
        pending.requestId,
        decision.kind,
        decision.kind === "deny_with_feedback" ? decision.feedback : undefined,
        decision.kind === "allow_and_remember" ? decision.pattern ?? null : null,
        decision.kind === "allow_and_remember"
          ? decision.scope ?? "session"
          : undefined,
        decision.kind === "allow_and_remember"
          ? decision.extraPatterns ?? []
          : []
      );
      console.info("[permission.approval] frontend tool approval accepted", {
        sessionId,
        requestId: pending.requestId,
        decision: decision.kind,
      });
    } catch (e) {
      console.error("[permission.approval] frontend tool approval failed", {
        sessionId,
        requestId: pending.requestId,
        decision: decision.kind,
        error: e,
      });
      // 失败时恢复 slot 上的弹窗，让用户重试
      set((state) => {
        const live = state.sessionStreams[sessionId];
        if (!live) return state;
        const restored: SessionStream = {
          ...live,
          pendingApproval: pending,
          pendingApprovalQueue: live.pendingApproval
            ? [live.pendingApproval, ...live.pendingApprovalQueue]
            : live.pendingApprovalQueue,
        };
        const isForeground = state.currentSession?.id === sessionId;
        return {
          ...state,
          sessionStreams: { ...state.sessionStreams, [sessionId]: restored },
          ...(isForeground ? mirrorFromSlot(restored) : {}),
        };
      });
      throw e;
    }
  },

  pendingWakeups: {},
  queueWakeupForSession(sessionId, wakeupXml, meta) {
    set((state) => ({
      pendingWakeups: { ...state.pendingWakeups, [sessionId]: { xml: wakeupXml, meta } },
    }));
  },
  async triggerWakeupResume(sessionId, wakeupXml, meta) {
    // 三分支决策（架构 §4.12.5 修订）。
    // meta = `{type:"system_notification", kind, task_id?, tool_use_id?}` 由后端 emit
    // wakeup-fired 时附带，三条路径都把它透传到后端落盘。
    //   active run（slot.requestId 在）→ 走 inject_user_message 即写即落 + 推 PendingInputs，
    //     agent_loop 在下一个 boundary drain，**不开新 run**
    //   idle 前台 → 开新 run / resume checkpoint（走 sendUserMessage，meta 透传）
    //   非前台 → 暂存到 pendingWakeups（含 meta），切回该 session 时消费
    const cur = get().currentSession;
    const isForeground = cur?.id === sessionId;
    const slot = get().sessionStreams[sessionId];
    const activeRequestId = slot?.requestId;

    if (isForeground && activeRequestId) {
      try {
        const result = await api.injectUserMessage(
          sessionId,
          activeRequestId,
          wakeupXml,
          [],
          meta
        );
        set((state) => {
          const slot = state.sessionStreams[sessionId];
          if (!slot) return state;
          const updated =
            result.message.meta?.type === "system_notification"
              ? insertSystemNotificationBeforeNextAssistant(slot, result.message)
              : appendUserInjectedMessage(slot, result.message);
          const isForeground = state.currentSession?.id === sessionId;
          return {
            ...state,
            sessionStreams: { ...state.sessionStreams, [sessionId]: updated },
            ...(isForeground ? mirrorFromSlot(updated) : {}),
          };
        });
        if (result.injected) return;
        await waitForRequestRelease(get, sessionId, activeRequestId);
      } catch (e) {
        // active run 已结束的边界 race → 回落到开新 run 路径（仍带 meta）
        console.warn(
          "[triggerWakeupResume] inject failed, falling back to sendUserMessage:",
          e
        );
      }
    }

    if (isForeground) {
      // 前台 idle：复用 sendUserMessage，把 meta 透传给后端 send_message 命令
      await get().sendUserMessage(wakeupXml, [], meta, { skipOptimisticUser: true });
      return;
    }
    // 非前台：暂存（含 meta），切回时由 openSession 调 triggerWakeupResume 消费
    get().queueWakeupForSession(sessionId, wakeupXml, meta);
  },

  inputQueues: {},
  currentInputQueue: [],
  enqueueInput(content, attachments, position = "tail") {
    const cur = get().currentSession;
    if (!cur) return;
    const trimmed = content.trim();
    if (!trimmed && attachments.length === 0) return;
    const sessionId = cur.id;
    const item: QueuedInput = {
      id:
        crypto.randomUUID?.() ??
        `q-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      content,
      attachments,
      enqueued_at: Date.now(),
    };
    set((state) => {
      const list = state.inputQueues[sessionId] ?? [];
      const next = position === "head" ? [item, ...list] : [...list, item];
      const isForeground = state.currentSession?.id === sessionId;
      return {
        inputQueues: { ...state.inputQueues, [sessionId]: next },
        ...(isForeground ? { currentInputQueue: next } : {}),
      };
    });
  },
  removeQueuedInput(id) {
    const cur = get().currentSession;
    if (!cur) return;
    const sessionId = cur.id;
    set((state) => {
      const list = state.inputQueues[sessionId] ?? [];
      const next = list.filter((it) => it.id !== id);
      if (next.length === list.length) return state;
      const isForeground = state.currentSession?.id === sessionId;
      const queues = { ...state.inputQueues };
      if (next.length === 0) delete queues[sessionId];
      else queues[sessionId] = next;
      return {
        ...state,
        inputQueues: queues,
        ...(isForeground ? { currentInputQueue: next } : {}),
      };
    });
  },
  async flushQueuedItem(id) {
    const cur = get().currentSession;
    if (!cur) return;
    const sessionId = cur.id;
    const list = get().inputQueues[sessionId] ?? [];
    if (list.length === 0) return;
    const target = id == null ? list[0] : list.find((it) => it.id === id);
    if (!target) return;
    const originalIndex = list.indexOf(target);
    const slot = get().sessionStreams[sessionId];
    const requestId = slot?.requestId;
    if (!requestId) return; // 不在 streaming：交给 ChatInput 走普通 send 路径
    // 先把队列项移除，避免 agent_loop drain 完后又被 drainNext 重复发送。
    get().removeQueuedInput(target.id);
    try {
      const result = await api.injectUserMessage(
        sessionId,
        requestId,
        target.content,
        target.attachments
      );
      // 把 user message 按真实时间序追加到 liveTimeline 末尾——这样它落在当前
      // 正在 streaming 的那个 Turn 之后（视觉上紧跟当前 assistant），同时下一次
      // TurnFinished 会把新 Turn 的 assistant 快照插到 assistantInsertPos
      // （即此 user 之前），用户后续插队消息又落到那个新 assistant 之后，依次类推。
      // run 结束 slot 被清掉时，由 reload 后的 session.messages 接管最终顺序。
      set((state) => {
        const slot = state.sessionStreams[sessionId];
        if (!slot) return state;
        const updated = appendUserInjectedMessage(slot, result.message);
        const isForeground = state.currentSession?.id === sessionId;
        return {
          ...state,
          sessionStreams: { ...state.sessionStreams, [sessionId]: updated },
          ...(isForeground ? mirrorFromSlot(updated) : {}),
        };
      });
    } catch (e) {
      // 失败时把队列项还原回原位置，让用户重试或撤回。
      set((state) => {
        const cur = state.inputQueues[sessionId] ?? [];
        const insertAt = Math.min(originalIndex, cur.length);
        const next = [...cur.slice(0, insertAt), target, ...cur.slice(insertAt)];
        const isForeground = state.currentSession?.id === sessionId;
        return {
          ...state,
          inputQueues: { ...state.inputQueues, [sessionId]: next },
          ...(isForeground ? { currentInputQueue: next } : {}),
        };
      });
      throw e;
    }
  },

  returnQueuedToComposer(id) {
    const cur = get().currentSession;
    if (!cur) return;
    const sessionId = cur.id;
    const list = get().inputQueues[sessionId] ?? [];
    const target = list.find((it) => it.id === id);
    if (!target) return;
    get().removeQueuedInput(id);
    // 同一时刻最多一条 draft 在等回填；后到的覆盖未消费的（ChatInput 应在
    // useEffect 里立即消费）。
    set({
      composerDraft: {
        content: target.content,
        attachments: target.attachments,
      },
    });
  },

  composerDraft: null,
  clearComposerDraft() {
    set({ composerDraft: null });
  },

  pendingQuestion: null,
  pendingQuestionQueue: [],
  async resolveQuestion(answer: QuestionAnswerPayload) {
    const cur = get().currentSession;
    if (!cur) return;
    const sessionId = cur.id;
    const slot = get().sessionStreams[sessionId];
    const pending = slot?.pendingQuestion;
    if (!slot || !pending) return;
    const nextSlot: SessionStream = {
      ...slot,
      pendingQuestion: slot.pendingQuestionQueue[0] ?? null,
      pendingQuestionQueue: slot.pendingQuestionQueue.slice(1),
    };
    set((state) => ({
      sessionStreams: { ...state.sessionStreams, [sessionId]: nextSlot },
      ...mirrorFromSlot(nextSlot),
    }));
    try {
      const payload: { text?: string; labels?: string[] } | undefined =
        answer.kind === "selected"
          ? { text: answer.label }
          : answer.kind === "selected_multi"
            ? { labels: answer.labels }
            : answer.kind === "custom"
              ? { text: answer.text }
              : undefined;
      await api.answerQuestion(pending.requestId, answer.kind, payload);
    } catch (e) {
      set((state) => {
        const live = state.sessionStreams[sessionId];
        if (!live) return state;
        const restored: SessionStream = {
          ...live,
          pendingQuestion: pending,
          pendingQuestionQueue: live.pendingQuestion
            ? [live.pendingQuestion, ...live.pendingQuestionQueue]
            : live.pendingQuestionQueue,
        };
        const isForeground = state.currentSession?.id === sessionId;
        return {
          ...state,
          sessionStreams: { ...state.sessionStreams, [sessionId]: restored },
          ...(isForeground ? mirrorFromSlot(restored) : {}),
        };
      });
      throw e;
    }
  },

  async init() {
    applyTheme(get().theme);
    await Promise.all([
      get().refreshProviders(),
      get().refreshPrompts(),
      get().refreshSessions(),
      get().refreshProjects(),
      // 加载工具清单（失败不影响主流程）
      api.listTools().then((tools) => set({ availableTools: tools })).catch(() => {}),
    ]);
    const first = get().sessions[0];
    if (first) {
      // 启动时若最新对话属于一个项目（project_id 在项目列表里存在），把侧栏
      // 切到「项目」模式并定位到该项目；普通对话保持「全部」默认。
      const projects = get().projects;
      if (
        first.project_id &&
        projects.some((p) => p.id === first.project_id)
      ) {
        set({
          projectSidebarMode: "projects",
          selectedProjectId: first.project_id,
        });
      }
      await get().openSession(first.id);
    }
  },

  async refreshProviders() {
    const file = await api.getProviders();
    set({ providersFile: file });
  },

  async saveProviders(file) {
    await api.saveProviders(file);
    set({ providersFile: file });
  },

  async upsertProvider(p) {
    await api.upsertProvider(p);
    await get().refreshProviders();
  },

  async refreshPrompts() {
    const f = await api.listPrompts();
    set((state) => ({
      promptsFile: f,
      prompts: f.prompts,
      pendingPromptId:
        state.pendingPromptId &&
        f.prompts.some((prompt) => prompt.id === state.pendingPromptId)
          ? state.pendingPromptId
          : f.default_prompt_id || "",
    }));
  },

  async upsertPrompt(p) {
    await api.upsertPrompt(p);
    await get().refreshPrompts();
  },

  async deletePrompt(id) {
    await api.deletePrompt(id);
    await get().refreshPrompts();
  },

  async setDefaultPrompt(id) {
    const f = await api.setDefaultPrompt(id);
    set({
      promptsFile: f,
      prompts: f.prompts,
      pendingPromptId: f.default_prompt_id ?? "",
    });
  },

  async refreshSessions() {
    const list = await api.listSessions();
    set({ sessions: list });
  },

  async refreshProjects() {
    const projects = await api.listProjects();
    set((state) => ({
      projects,
      selectedProjectId:
        state.selectedProjectId &&
        projects.some((project) => project.id === state.selectedProjectId)
          ? state.selectedProjectId
          : null,
    }));
  },

  async saveProject(input) {
    const project = await api.saveProject(input);
    await get().refreshProjects();
    return project;
  },

  async deleteProject(id) {
    await api.deleteProject(id);
    set((state) => ({
      selectedProjectId: state.selectedProjectId === id ? null : state.selectedProjectId,
    }));
    await get().refreshProjects();
  },

  async importVscodeProject(path, name) {
    const project = await api.importVscodeProject(path, name);
    await get().refreshProjects();
    return project;
  },

  async importProjectFile(path) {
    const project = await api.importProjectFile(path);
    await get().refreshProjects();
    return project;
  },

  setProjectSidebarMode(mode) {
    set({ projectSidebarMode: mode });
  },

  openProject(id) {
    set({ projectSidebarMode: "projects", selectedProjectId: id });
  },

  closeProject() {
    set({ selectedProjectId: null });
  },

  async openSession(id) {
    const activeRequestId = activeRequestForSession(get(), id);
    const s = await api.getSession(id, activeRequestId);
    persistLastSessionConfig({
      providerId: s.provider_id,
      model: s.model,
      promptId: s.prompt_id ?? "",
    });
    // 切到这个对话时，让输入框 chip 显示的 pending 跟随该对话的实际 workdir / allowed_paths。
    // 这样：
    // 1. 切对话 → chip 立即更新成目标对话的设置
    // 2. 用户在某对话里改完 pending，新建对话会继承（newSession 用 pending 注入）
    const sessionWorkdir = s.workdir ?? null;
    const sessionAllowedPaths = s.allowed_paths ?? [];
    persistPendingWorkdir(sessionWorkdir);
    persistPendingAllowedPaths(sessionAllowedPaths);
    persistGlobalRules(s.global_rules ?? null);
    // 消费 pendingWakeup：切到该 session 时若有积压的 wakeup XML，立刻发出
    const pendingWakeup = get().pendingWakeups[id];
    set((state) => {
      const { [id]: _drop, ...rest } = state.pendingWakeups;
      return {
        currentSession: s,
        pendingPromptId: s.prompt_id ?? "",
        pendingWorkdir: sessionWorkdir,
        pendingAllowedPaths: sessionAllowedPaths,
        unreadFinishedSessions: removeFromSet(state.unreadFinishedSessions, id),
        currentInputQueue: state.inputQueues[id] ?? [],
        pendingWakeups: rest,
        ...mirrorFromSlot(state.sessionStreams[id]),
      };
    });
    if (pendingWakeup) {
      // 用 microtask 异步触发，避免在 openSession 内嵌套 sendUserMessage 的 set 调用；
      // 走 triggerWakeupResume 确保选最优路径（active inject vs idle send）且带上 meta。
      queueMicrotask(() => {
        void get().triggerWakeupResume(id, pendingWakeup.xml, pendingWakeup.meta);
      });
    }
    // 把持久化的 todos / active_plan 从 Session 字段同步进 slot——
    // Session 字段已经在 api.getSession 里返回（agent_core 折叠 jsonl 时填充），
    // 这里直接落到 slot，避免 TodoTab 用户切到 tab 才拉数据的延迟。
    // 重启后用户首次打开 session 立刻能在 sidebar 看到上次的任务清单。
    if (s.todos && s.todos.length > 0) {
      get().replaceSessionStreamTodos(id, s.todos);
    }
    if (s.active_plan) {
      const plan = activePlanFromPath(s.active_plan);
      if (plan) get().setSessionActivePlan(id, plan);
    }
    get().refreshContextUsage();
    get().refreshEdits();
  },

  async newSession(opts) {
    const p =
      (opts?.providerId &&
        get().providersFile.providers.find((x) => x.id === opts.providerId)) ||
      get().pickDefaultProvider();
    if (!p) throw new Error("请先配置一个供应商");
    const m = opts?.model || p.default_model || p.models[0] || "";
    if (!m) throw new Error("请先为供应商填写至少一个模型");
    const requestedPromptId =
      opts?.promptId ?? get().promptsFile.default_prompt_id ?? get().pendingPromptId;
    const matchedPrompt = requestedPromptId
      ? get().prompts.find((x) => x.id === requestedPromptId)
      : undefined;
    const promptId = matchedPrompt?.id ?? null;
    const prompt = matchedPrompt?.content;
    const activeProjectId = opts?.projectId ?? (get().projectSidebarMode === "projects" ? get().selectedProjectId : null);
    const selectedProject = activeProjectId
      ? get().projects.find((project) => project.id === activeProjectId) ?? null
      : null;
    const projectFolders = selectedProject?.folders ?? [];
    const projectWorkdir = projectFolders[0]?.path ?? null;
    const projectAllowed = projectFolders.slice(1).map((folder) => folder.path);
    let s = await api.createSession(p.id, m, prompt ?? null, promptId, selectedProject
      ? {
          project_id: selectedProject.id,
          workdir: projectWorkdir,
          allowed_paths: projectAllowed,
        }
      : undefined);
    // 把"待继承"的 workdir / allowed_paths 立即注入新 session：
    // 输入框 + 菜单的选择是跨对话黏的，新建对话时无需用户重新选。
    const inheritWorkdir = selectedProject ? null : get().pendingWorkdir;
    const inheritAllowed = selectedProject ? [] : get().pendingAllowedPaths;
    if (!selectedProject && (inheritWorkdir || inheritAllowed.length > 0)) {
      s = await api.updateSessionSettings(s.id, {
        ...(inheritWorkdir ? { workdir: inheritWorkdir } : {}),
        ...(inheritAllowed.length > 0 ? { allowed_paths: inheritAllowed } : {}),
      });
    }
    // 继承上一个对话的全局规则设置
    const inheritGlobalRules = readStoredGlobalRules();
    if (inheritGlobalRules !== null) {
      s = await api.updateSessionSettings(s.id, {
        global_rules: inheritGlobalRules,
      });
    }
    persistLastSessionConfig({
      providerId: s.provider_id,
      model: s.model,
      promptId: s.prompt_id ?? "",
    });
    // 新建 session 几乎不可能有残留 slot，但保险起见还是从 slot 镜像（一般是空），
    // 这样不会被旧 session 残留的 streamingMessageId / pendingApproval 串味。
    set((state) => ({
      currentSession: s,
      pendingPromptId: s.prompt_id ?? "",
      pendingWorkdir: s.workdir ?? null,
      pendingAllowedPaths: s.allowed_paths ?? [],
      currentInputQueue: state.inputQueues[s.id] ?? [],
      ...mirrorFromSlot(state.sessionStreams[s.id]),
    }));
    await get().refreshSessions();
  },

  async renameSession(id, title) {
    await api.renameSession(id, title);
    await get().refreshSessions();
    if (get().currentSession?.id === id) {
      set({ currentSession: { ...get().currentSession!, title } });
    }
  },

  async deleteSession(id) {
    await api.deleteSession(id);
    const wasCurrent = get().currentSession?.id === id;
    set((state) => {
      const { [id]: _drop, ...restStreams } = state.sessionStreams;
      const { [id]: _dropQueue, ...restQueues } = state.inputQueues;
      const next: Partial<AppState> = {
        sessionStreams: restStreams,
        inputQueues: restQueues,
        runningSessions: removeFromSet(state.runningSessions, id),
        unreadFinishedSessions: removeFromSet(state.unreadFinishedSessions, id),
      };
      if (wasCurrent) {
        Object.assign(next, mirrorFromSlot(undefined));
        next.currentInputQueue = [];
      }
      return next as AppState;
    });
    await get().refreshSessions();
    if (wasCurrent) {
      const next = get().sessions[0];
      if (next) await get().openSession(next.id);
      else set({ currentSession: null });
    }
  },

  async forkSession(msgId) {
    const cur = get().currentSession;
    if (!cur) return;
    const s = await api.forkSession(cur.id, msgId);
    await get().refreshSessions();
    persistLastSessionConfig({
      providerId: s.provider_id,
      model: s.model,
      promptId: s.prompt_id ?? "",
    });
    set((state) => ({
      currentSession: s,
      pendingPromptId: s.prompt_id ?? "",
      currentInputQueue: state.inputQueues[s.id] ?? [],
      ...mirrorFromSlot(state.sessionStreams[s.id]),
    }));
  },

  async regenerateTitle() {
    const cur = get().currentSession;
    if (!cur) return;
    const s = await api.generateSessionTitle(cur.id);
    set({ currentSession: s });
    await get().refreshSessions();
  },

  async sendUserMessage(content, attachments = [], meta = null, options = {}) {
    const cur = get().currentSession;
    if (!cur) return;

    const removeQueuedForSession = (sessionId: string, id: string) => {
      set((state) => {
        const list = state.inputQueues[sessionId] ?? [];
        const next = list.filter((it) => it.id !== id);
        if (next.length === list.length) return state;
        const queues = { ...state.inputQueues };
        if (next.length === 0) delete queues[sessionId];
        else queues[sessionId] = next;
        const isForeground = state.currentSession?.id === sessionId;
        return {
          ...state,
          inputQueues: queues,
          ...(isForeground ? { currentInputQueue: next } : {}),
        };
      });
    };

    const sendForSession = async (
      baseSession: Session,
      runContent: string,
      runAttachments: MessageAttachment[]
    ) => {
      const sessionId = baseSession.id;
      // 当前 turn 跑完后（无论成功 / 失败 / 取消），把队首项作为下一轮自动 send。
      // 队列属于 session，不属于当前打开的页面；切到别的对话后也要继续 drain。
      const drainNext = () => {
        queueMicrotask(() => {
          const state = get();
          const queue = state.inputQueues[sessionId];
          if (!queue || queue.length === 0) return;
          if (state.sessionStreams[sessionId]) return; // 还有 run 在跑（异常路径）
          const head = queue[0];
          removeQueuedForSession(sessionId, head.id);
          void (async () => {
            const latest =
              get().currentSession?.id === sessionId
                ? get().currentSession
                : await api.getSession(
                    sessionId,
                    activeRequestForSession(get(), sessionId)
                  );
            if (!latest) return;
            await sendForSession(latest, head.content, head.attachments);
          })().catch(() => {
            // 后台队列失败时由 running/unread 状态提示用户回到该会话查看。
          });
        });
      };

      const tempId = "streaming";
      const requestId =
        crypto.randomUUID?.() ??
        `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      // 新 run 起始时保留上一轮的 todos / activePlan / planComments——这些是
      // session 级持久化状态（架构 §4.4.5 / §4.4.6），不该跟 streaming 一起清。
      // 之前注释说"不清空"但代码却写了 `[]`，是真正让 sidebar todo "一发新消息就消失"
      // 的前端根因。
      const priorSlot = get().sessionStreams[sessionId];
      const priorSession =
        get().currentSession?.id === sessionId ? get().currentSession : null;
      const initialSlot: SessionStream = {
        requestId,
        streamingMessageId: tempId,
        streamingText: "",
        streamingParts: [],
        liveTimeline: [],
        assistantInsertPos: 0,
        pendingApproval: null,
        pendingApprovalQueue: [],
        pendingQuestion: null,
        pendingQuestionQueue: [],
        autoJudgedNotes: [],
        currentRunMode: priorSlot?.currentRunMode ?? null,
        suspended: null,
        todos: priorSlot?.todos ?? priorSession?.todos ?? [],
        activePlan:
          priorSlot?.activePlan ??
          (priorSession?.active_plan
            ? activePlanFromPath(priorSession.active_plan)
            : null),
        planComments: priorSlot?.planComments ?? {},
      };
      set((state) => {
        const isForeground = state.currentSession?.id === sessionId;
        return {
          ...state,
          ...(isForeground && !options.skipOptimisticUser
            ? {
                currentSession: appendOptimisticUserMessage(
                  baseSession,
                  runContent,
                  runAttachments,
                  {
                    id: `pending-user-${requestId}`,
                    now: Date.now(),
                    meta,
                  }
                ),
              }
            : {}),
          sessionStreams: { ...state.sessionStreams, [sessionId]: initialSlot },
          runningSessions: new Set(state.runningSessions).add(sessionId),
          lastRunError: null,
          // 新 Run 开始 → 清掉上一轮的「本轮写入 N 条记忆」摘要（只展示当前轮）。
          sessionMemoryWrites: dropKey(state.sessionMemoryWrites, sessionId),
          ...(isForeground ? mirrorFromSlot(initialSlot) : {}),
        };
      });
      try {
        // 传空数组：后端会优先用 session.enabled_tools，再 fallback 到全局 settings。
        // 工具的开关现在统一在「设置 → 对话设置」配置。
        await api.sendMessage(
          sessionId,
          runContent,
          runAttachments,
          baseSession.stream,
          [],
          requestId,
          (e: EngineEvent) => {
            // 标题是 session 级状态（不属于 slot）：agent_core 首轮跑完后异步落盘，
            // 通过 EngineEvent 通知。这里独立处理：直接更新 currentSession.title +
            // refreshSessions 让 sidebar 同步。
            if (e.type === "session_title_changed") {
              set((state) =>
                state.currentSession?.id === e.session_id
                  ? { currentSession: { ...state.currentSession, title: e.title } }
                  : state
              );
              void get().refreshSessions();
              return;
            }
            // 记忆抽取事件（架构 §4.14）：session 级，不进 slot——抽取在 RunFinished
            // 之后异步到达，那时 slot 可能已被清掉，故跟标题一样独立处理。
            if (e.type === "memory_extracted") {
              if (e.items.length > 0) {
                set((state) => ({
                  sessionMemoryWrites: {
                    ...state.sessionMemoryWrites,
                    [e.session_id]: e.items,
                  },
                }));
              }
              return;
            }
            if (e.type === "memory_extraction_failed") {
              toast.error("记忆提取失败了", { description: "这轮对话会在下次自动补抽" });
              return;
            }
            // Edit 快照事件：session-scoped，不进 slot；run 结束 slot 被删后仍然保留
            if (e.type === "edit_snapshot_created" || e.type === "edit_reverted") {
              set((state) => {
                const next = applyEditEvent(state.sessionEditSnapshots, sessionId, e);
                return next === null ? state : { sessionEditSnapshots: next };
              });
              return;
            }
            set((state) => {
              const slot = state.sessionStreams[sessionId];
              // 槽已被替换（用户在同一会话又发了一条）或被清掉（run 已结束）→ 丢弃事件
              if (!slot || slot.requestId !== requestId) return state;
              const updated = applyEventToSlot(slot, e);
              if (updated === slot) return state;
              const isForeground = state.currentSession?.id === sessionId;
              return {
                ...state,
                sessionStreams: {
                  ...state.sessionStreams,
                  [sessionId]: updated,
                },
                ...(isForeground ? mirrorFromSlot(updated) : {}),
              };
            });
          },
          meta,
        );
        const stillForeground = get().currentSession?.id === sessionId;
        if (stillForeground) {
          const fresh = await api.getSession(
            sessionId,
            activeRequestForSession(get(), sessionId)
          );
          set((state) => {
            const { [sessionId]: _drop, ...rest } = state.sessionStreams;
            return {
              currentSession: fresh,
              sessionStreams: rest,
              runningSessions: removeFromSet(state.runningSessions, sessionId),
              // run 结束时 slot 被清掉 → 顶层 streaming 等字段走 EMPTY_MIRROR；
              // 但 todos / active_plan 是持久化状态，run 结束不应该跟着清空——
              // 从 fresh session（agent_core 折叠 jsonl 后的最新快照）拿回来。
              ...mirrorFromSlot(undefined),
              todos: fresh.todos ?? [],
              activePlan: fresh.active_plan
                ? activePlanFromPath(fresh.active_plan)
                : null,
            };
          });
          get().refreshContextUsage();
        } else {
          set((state) => {
            const { [sessionId]: _drop, ...rest } = state.sessionStreams;
            return {
              ...state,
              sessionStreams: rest,
              runningSessions: removeFromSet(state.runningSessions, sessionId),
              unreadFinishedSessions: new Set(state.unreadFinishedSessions).add(
                sessionId
              ),
            };
          });
        }
        await get().refreshSessions();
        // 标题自动生成已下沉到 agent_core：首轮 TurnFinished 后由 Harness::spawn_run
        // 异步 spawn 一个短调用 task，落 jsonl 后通过 EngineEvent::SessionTitleChanged
        // 推到前端（见上面 event handler）。前端不再主动 invoke。
      } catch (err: any) {
        const stillForeground = get().currentSession?.id === sessionId;
        // 不论前后台都先把 slot 清掉、running 摘除；后台失败再标 unread
        set((state) => {
          const { [sessionId]: _drop, ...rest } = state.sessionStreams;
          const next: Partial<AppState> = {
            sessionStreams: rest,
            runningSessions: removeFromSet(state.runningSessions, sessionId),
          };
          if (stillForeground) {
            Object.assign(next, mirrorFromSlot(undefined));
          } else {
            next.unreadFinishedSessions = new Set(state.unreadFinishedSessions).add(
              sessionId
            );
          }
          return next as AppState;
        });
        if (String(err?.message ?? err).includes("请求已中断")) {
          if (stillForeground) {
            const fresh = await api.getSession(
              sessionId,
              activeRequestForSession(get(), sessionId)
            );
            set({ currentSession: fresh });
          }
          await get().refreshSessions();
          return;
        }
        try {
          const fresh = await api.getSession(
            sessionId,
            activeRequestForSession(get(), sessionId)
          );
          if (get().currentSession?.id === sessionId) {
            set({ currentSession: fresh });
          }
          await get().refreshSessions();
        } catch {
          if (get().currentSession?.id === sessionId) {
            set({ currentSession: baseSession });
          }
        }
        // 后台失败不向 UI 抛错（用户视野不在这里，吐 toast 也无意义）
        if (stillForeground) {
          set({ lastRunError: { sessionId } });
          throw err;
        }
      } finally {
        drainNext();
      }
    };

    await sendForSession(cur, content, attachments);
  },

  async cancelStreaming() {
    // 只取消"用户当前在看的"那个会话的 run。后台的 run 不动，避免误杀。
    const cur = get().currentSession;
    if (!cur) return;
    const slot = get().sessionStreams[cur.id];
    const requestId = slot?.requestId ?? get().activeRequestId;
    if (!requestId) return;

    // 只发取消信号，**不在这里清掉 streamingParts / 也不 reload session**：
    // - 后端要花一两百 ms 才能把 partial output 落盘并返回「请求已中断」；
    // - 这中间 streamingParts 还得继续显示用户已经看到的内容；
    // - 落盘完后 sendUserMessage 的 .catch 会拉到带 partial 的最新 session，
    //   再统一清理 streaming 状态。提前 reload 会读到尚未 persist 的旧状态、
    //   把已经流到屏幕上的内容"吞掉"。
    await api.cancelMessage(requestId);
  },

  async regenerateFrom(assistantMsgId) {
    const cur = get().currentSession;
    if (!cur) return;
    const targetIdx = cur.messages.findIndex((m) => m.id === assistantMsgId);
    if (targetIdx < 1) return;
    // 重新生成的语义：回到最近一条**真实** user message，丢掉它之后的一切（已完成的
    // assistant、cancel 留下的 partial assistant、Interrupted / 切换 marker、tool 结果、
    // wakeup 系统通知……），重新触发一轮 stream。
    //
    // 关键过滤（架构 §4.12.5 修订）：wakeup notification 物理 role 也是 "user"，
    // 但带 `meta.system_notification` 标识——它不是用户主动发的消息，重新生成不该
    // 把它当作目标。回溯时同样跳过。
    let userIdx = targetIdx - 1;
    while (
      userIdx >= 0 &&
      (cur.messages[userIdx].role !== "user" ||
        cur.messages[userIdx].meta?.type === "system_notification")
    ) {
      userIdx--;
    }
    if (userIdx < 0) return;
    const targetUser = cur.messages[userIdx];
    await api.truncateInclusive(cur.id, targetUser.id);
    const refreshed = await api.getSession(cur.id, activeRequestForSession(get(), cur.id));
    set({ currentSession: refreshed });
    await get().sendUserMessage(targetUser.content, targetUser.attachments ?? []);
  },

  async regenerateFromUser(userMsgId) {
    const cur = get().currentSession;
    if (!cur) return;
    const target = cur.messages.find((m) => m.id === userMsgId);
    if (!target || target.role !== "user") return;
    await api.truncateInclusive(cur.id, userMsgId);
    const refreshed = await api.getSession(cur.id, activeRequestForSession(get(), cur.id));
    set({ currentSession: refreshed });
    await get().sendUserMessage(target.content, target.attachments ?? []);
  },

  async editAndRerun(userMsgId, content, attachments) {
    const cur = get().currentSession;
    if (!cur) return;
    const target = cur.messages.find((m) => m.id === userMsgId);
    if (!target || target.role !== "user") return;
    await api.truncateInclusive(cur.id, userMsgId);
    const refreshed = await api.getSession(cur.id, activeRequestForSession(get(), cur.id));
    set({ currentSession: refreshed });
    await get().sendUserMessage(content, attachments ?? target.attachments ?? []);
  },

  async updateCurrentConfig(patch) {
    const cur = get().currentSession;
    if (!cur) return;
    const s = await api.updateSessionConfig(cur.id, patch);
    persistLastSessionConfig({
      providerId: s.provider_id,
      model: s.model,
      promptId: s.prompt_id ?? "",
    });
    set({ currentSession: s, pendingPromptId: s.prompt_id ?? "" });
    await get().refreshSessions();
  },

  async setReasoning(reasoning) {
    await get().updateCurrentConfig({ reasoning });
  },

  async switchProviderModel(providerId, model) {
    const cur = get().currentSession;
    if (!cur) return;
    const s = await api.switchProviderModel(cur.id, providerId, model);
    persistLastSessionConfig({
      providerId: s.provider_id,
      model: s.model,
      promptId: s.prompt_id ?? "",
    });
    set({ currentSession: s, pendingPromptId: s.prompt_id ?? "" });
    await get().refreshSessions();
  },

  setProviderDialogOpen(v) {
    set({ providerDialogOpen: v });
  },
  setSettingsOpen(v) {
    set({ settingsOpen: v });
  },
  setPromptsDialogOpen(v) {
    set({ promptsDialogOpen: v });
  },

  appSettingsOpen: false,
  setAppSettingsOpen(v) {
    set({ appSettingsOpen: v });
  },
  appSettings: null,
  async refreshAppSettings() {
    const s = normalizeAppSettings(await api.getSettings());
    set({ appSettings: s, logEnabled: s.general.log_enabled });
  },
  async saveAppSettings(settings: AppSettings) {
    const normalized = normalizeAppSettings(settings);
    await api.saveSettings(normalized);
    set({ appSettings: normalized, logEnabled: normalized.general.log_enabled });
  },
  async updateCurrentSessionSettings(patch) {
    const cur = get().currentSession;
    if (!cur) return;
    const updated = await api.updateSessionSettings(cur.id, patch);
    if ("global_rules" in patch) {
      persistGlobalRules(patch.global_rules ?? null);
    }
    set({ currentSession: updated });
  },
  async resolvePathAccess(scope) {
    const cur = get().currentSession;
    if (!cur) return;
    const sessionId = cur.id;
    const slot = get().sessionStreams[sessionId];
    const pending = slot?.pendingApproval;
    if (!slot || !pending) return;
    const nextSlot: SessionStream = {
      ...slot,
      pendingApproval: slot.pendingApprovalQueue[0] ?? null,
      pendingApprovalQueue: slot.pendingApprovalQueue.slice(1),
    };
    set((state) => ({
      sessionStreams: { ...state.sessionStreams, [sessionId]: nextSlot },
      ...mirrorFromSlot(nextSlot),
    }));
    console.info("[permission.approval] frontend submitting path approval", {
      sessionId,
      requestId: pending.requestId,
      kind: pending.kind,
      toolName: pending.toolName,
      paths: pending.paths ?? [],
      scope,
    });
    try {
      await api.approvePathAccess(
        pending.requestId,
        pending.paths ?? [],
        scope,
        sessionId
      );
      console.info("[permission.approval] frontend path approval accepted", {
        sessionId,
        requestId: pending.requestId,
        scope,
      });
      // 重新拉一下 session（this_session 时 allowed_paths 已落盘）；
      // global 触发 settings 刷新；this_project / once 只动 PermissionStore，无需额外拉数据。
      if (scope === "this_session") {
        const fresh = await api.getSession(
          sessionId,
          activeRequestForSession(get(), sessionId)
        );
        if (get().currentSession?.id === sessionId) {
          set({ currentSession: fresh });
        }
      } else if (scope === "global") {
        await get().refreshAppSettings();
      }
    } catch (e) {
      console.error("[permission.approval] frontend path approval failed", {
        sessionId,
        requestId: pending.requestId,
        scope,
        error: e,
      });
      set((state) => {
        const live = state.sessionStreams[sessionId];
        if (!live) return state;
        const restored: SessionStream = {
          ...live,
          pendingApproval: pending,
          pendingApprovalQueue: live.pendingApproval
            ? [live.pendingApproval, ...live.pendingApprovalQueue]
            : live.pendingApprovalQueue,
        };
        const isForeground = state.currentSession?.id === sessionId;
        return {
          ...state,
          sessionStreams: { ...state.sessionStreams, [sessionId]: restored },
          ...(isForeground ? mirrorFromSlot(restored) : {}),
        };
      });
      throw e;
    }
  },
  setPendingPromptId(v) {
    localStorage.setItem(LAST_PROMPT_ID_KEY, v);
    set({ pendingPromptId: v });
  },
  setUserAvatar(v) {
    localStorage.setItem(USER_AVATAR_KEY, v);
    set({ userAvatar: v });
  },

  async runSearch(query, caseSensitive, regex) {
    const cs = caseSensitive ?? get().searchCaseSensitive;
    const re = regex ?? get().searchRegex;
    set({ searchQuery: query, searchCaseSensitive: cs, searchRegex: re });
    if (!query.trim()) {
      set({ searchResults: null, searching: false });
      return;
    }
    set({ searching: true });
    try {
      const hits = await api.searchSessions(query, cs, re);
      set({ searchResults: hits, searching: false });
    } catch (e) {
      set({ searching: false });
      throw e;
    }
  },
  clearSearch() {
    set({ searchQuery: "", searchResults: null, searching: false });
  },

  toggleTool(name) {
    const next = new Set(get().enabledTools);
    if (next.has(name)) {
      next.delete(name);
    } else {
      next.add(name);
    }
    // 持久化到 localStorage，下次启动保持用户选择
    localStorage.setItem("enabledTools", JSON.stringify(Array.from(next)));
    set({ enabledTools: next });
  },

  setDebugEnabled(v) {
    localStorage.setItem("hebbian.debugEnabled", v ? "1" : "0");
    set({ debugEnabled: v });
  },

  setLogEnabled(v) {
    localStorage.setItem("hebbian.logEnabled", v ? "1" : "0");
    set({ logEnabled: v });
  },
  appendLogEntry(line) {
    set((state) => {
      const next = [...state.logEntries, line];
      if (next.length > LOG_CAPACITY) next.splice(0, next.length - LOG_CAPACITY);
      return { logEntries: next };
    });
  },
  clearLogEntries() {
    set({ logEntries: [] });
  },

  toggleTheme() {
    const next = get().theme === "dark" ? "light" : "dark";
    applyTheme(next);
    localStorage.setItem("theme", next);
    set({ theme: next });
  },

  pickDefaultProvider() {
    const { providers: allProviders, default_provider_id } = get().providersFile;
    const providers = allProviders.filter((provider) => provider.enabled !== false);
    const lastProviderId = readStoredValue(LAST_PROVIDER_ID_KEY);
    if (lastProviderId) {
      const p = providers.find((x) => x.id === lastProviderId);
      if (p) return p;
    }
    if (default_provider_id) {
      const p = providers.find((x) => x.id === default_provider_id);
      if (p && p.api_key) return p;
    }
    return providers.find((p) => !!p.api_key) || providers[0];
  },
}));

// 把 store 暴露到 window，便于 Playwright / 浏览器控制台 inspect 与注入。
// 零开销，hebweb / desktop 都保留——遇到事件流问题时不用重启就能直接 dump 状态。
if (typeof window !== "undefined") {
  (window as unknown as { __hebStore?: typeof useStore }).__hebStore = useStore;
}
