import { create } from "zustand";
import type {
  AppSettings,
  ApprovalDecisionPayload,
  ContextUsage,
  EngineEvent,
  Message,
  MessageAttachment,
  PendingApproval,
  PendingQuestion,
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
  ToolInfo,
} from "@/desktop/ui/types";
import { api } from "@/desktop/bridge/tauri";
import { appendOptimisticUserMessage } from "@/desktop/ui/store/sessionOptimism";

const LAST_PROMPT_ID_KEY = "lastPromptId";
const LAST_PROVIDER_ID_KEY = "lastProviderId";
const LAST_MODEL_KEY = "lastModel";
const USER_AVATAR_KEY = "userAvatar";
// 工作区"待继承"项：用户上次在输入框 + 菜单里选过的 workdir / allowed_dirs，
// 新建对话会自动应用，避免每次都重选。null / [] 表示用户清空了，新对话也保持空。
const PENDING_WORKDIR_KEY = "pendingWorkdir";
const PENDING_ALLOWED_DIRS_KEY = "pendingAllowedDirs";

function readStoredValue(key: string) {
  return localStorage.getItem(key) ?? "";
}

function readStoredWorkdir(): string | null {
  const raw = localStorage.getItem(PENDING_WORKDIR_KEY);
  return raw ? raw : null;
}

function readStoredAllowedDirs(): string[] {
  try {
    const raw = localStorage.getItem(PENDING_ALLOWED_DIRS_KEY);
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

function persistPendingAllowedDirs(dirs: string[]) {
  if (dirs.length > 0) {
    localStorage.setItem(PENDING_ALLOWED_DIRS_KEY, JSON.stringify(dirs));
  } else {
    localStorage.removeItem(PENDING_ALLOWED_DIRS_KEY);
  }
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
   * 「立即发送」期间临时显示的 user message：渲染时排在 streaming bubble **之后**，
   * 视觉上紧跟当前正在跑的 assistant，下一轮 assistant 输出再接着它。
   * run 结束时 reload session 拿到完整 messages，slot 整个被清掉，由 messages 接管。
   */
  injectedSinceStream: Message[];
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
  injectedSinceStream: [] as Message[],
  activeRequestId: null as string | null,
  pendingApproval: null as PendingApproval | null,
  pendingApprovalQueue: [] as PendingApproval[],
  pendingQuestion: null as PendingQuestion | null,
  pendingQuestionQueue: [] as PendingQuestion[],
  autoJudgedNotes: [] as AutoJudgedNote[],
  currentRunMode: null as string | null,
  suspended: null as SuspendedInfo | null,
};

function mirrorFromSlot(slot: SessionStream | undefined) {
  if (!slot) return { ...EMPTY_MIRROR };
  return {
    streamingMessageId: slot.streamingMessageId,
    streamingText: slot.streamingText,
    streamingParts: slot.streamingParts,
    injectedSinceStream: slot.injectedSinceStream,
    activeRequestId: slot.requestId,
    pendingApproval: slot.pendingApproval,
    pendingApprovalQueue: slot.pendingApprovalQueue,
    pendingQuestion: slot.pendingQuestion,
    pendingQuestionQueue: slot.pendingQuestionQueue,
    autoJudgedNotes: slot.autoJudgedNotes,
    currentRunMode: slot.currentRunMode,
    suspended: slot.suspended,
  };
}

function applyEventToSlot(slot: SessionStream, e: EngineEvent): SessionStream {
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
  return slot;
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
  /** 「立即发送」期间临时显示的 user message：渲染在 streaming bubble 之后。 */
  injectedSinceStream: Message[];
  activeRequestId: string | null;
  /** 当前对话 AutoMode 判官累计标记（镜像自 currentSession 的 slot）。 */
  autoJudgedNotes: AutoJudgedNote[];
  /** 当前对话 RunMode 字符串。`null` 表示未收到过 RunModeChanged 事件。 */
  currentRunMode: string | null;
  /** 架构 §4.12：当前对话 Run 是否被挂起。 */
  suspended: SuspendedInfo | null;

  /** 后端正在跑（含前台 + 后台）的会话 id 集合，用于 Sidebar 呼吸点。 */
  runningSessions: Set<string>;
  /** 后台跑完但用户尚未查看的会话 id 集合，用于 Sidebar 静态点。 */
  unreadFinishedSessions: Set<string>;

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

  // HITL — 当前一轮 run 中悬挂的审批请求
  pendingApproval: PendingApproval | null;
  pendingApprovalQueue: PendingApproval[];
  resolveApproval: (decision: ApprovalDecisionPayload) => Promise<void>;
  // HITL — 当前一轮 run 中悬挂的 agent 提问（ask 工具）
  pendingQuestion: PendingQuestion | null;
  pendingQuestionQueue: PendingQuestion[];
  resolveQuestion: (answer: QuestionAnswerPayload) => Promise<void>;

  // 架构 §4.12.6：后端 WakeupScheduler 触发的 wakeup XML，按 sessionId 暂存。
  // - 若 wakeup 到达时该 session 是 currentSession，立刻自动发出
  // - 否则暂存到这里，下次 openSession 时消费
  pendingWakeups: Record<string, string>;
  /** 自动唤醒：把 wakeup XML 作 user message 发给该 session（不论前后台）。 */
  triggerWakeupResume: (sessionId: string, wakeupXml: string) => Promise<void>;
  /** 给指定 session 排队一条 wakeup XML，等 openSession 时消费。 */
  queueWakeupForSession: (sessionId: string, wakeupXml: string) => void;

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
  newSession: (opts?: {
    providerId?: string;
    model?: string;
    promptId?: string;
  }) => Promise<void>;
  renameSession: (id: string, title: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  forkSession: (msgId: string) => Promise<void>;
  regenerateTitle: () => Promise<void>;

  sendUserMessage: (content: string, attachments?: MessageAttachment[]) => Promise<void>;
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
  /** 更新当前对话的设置（workdir / allowed_dirs / enabled_tools / skill_dirs） */
  updateCurrentSessionSettings: (patch: {
    workdir?: string | null;
    allowed_dirs?: string[] | null;
    enabled_tools?: string[] | null;
    skill_dirs?: string[] | null;
  }) => Promise<void>;
  /**
   * "待继承"的工作区配置：输入框左下 + 菜单选择的项目 / 目录会落到这里，
   * 新建对话时自动写入新 session。当前 session 已经存在时，setter 会同时
   * 写到本地（持久化）和 session（updateSessionSettings），让用户的修改即时生效。
   */
  pendingWorkdir: string | null;
  pendingAllowedDirs: string[];
  setPendingWorkdir: (workdir: string | null) => Promise<void>;
  setPendingAllowedDirs: (dirs: string[]) => Promise<void>;
  /** PathAccess 审批专用 */
  resolvePathAccess: (scope: "once" | "this_project" | "all_project") => Promise<void>;
  setPendingPromptId: (v: string) => void;
  setUserAvatar: (v: string) => void;
  toggleTheme: () => void;

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

export const useStore = create<AppState>((set, get) => ({
  providersFile: { providers: [], default_provider_id: null },
  promptsFile: { prompts: [], default_prompt_id: null },
  prompts: [],
  pendingPromptId: readStoredValue(LAST_PROMPT_ID_KEY),
  userAvatar: readStoredValue(USER_AVATAR_KEY),
  sessions: [],
  currentSession: null,
  sessionStreams: {},
  streamingMessageId: null,
  streamingText: "",
  streamingParts: [],
  injectedSinceStream: [],
  activeRequestId: null,
  autoJudgedNotes: [],
  currentRunMode: null,
  suspended: null,
  runningSessions: new Set<string>(),
  unreadFinishedSessions: new Set<string>(),
  providerDialogOpen: false,
  settingsOpen: false,
  promptsDialogOpen: false,
  searchQuery: "",
  searchResults: null,
  searchCaseSensitive: false,
  searchRegex: false,
  searching: false,
  theme: (localStorage.getItem("theme") as any) ?? "light",
  availableTools: [],
  // 默认只开启搜索/抓取；生图等额外工具需要用户手动开启
  enabledTools: new Set<string>(
    JSON.parse(localStorage.getItem("enabledTools") ?? '["WebSearch","Fetch"]')
  ),

  pendingWorkdir: readStoredWorkdir(),
  pendingAllowedDirs: readStoredAllowedDirs(),
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
        const fresh = await api.getSession(cur.id);
        set({ currentSession: fresh });
      } catch {
        /* ignore */
      }
    }
  },
  async setPendingAllowedDirs(dirs) {
    const next = Array.from(new Set(dirs));
    persistPendingAllowedDirs(next);
    set({ pendingAllowedDirs: next });
    const cur = get().currentSession;
    if (cur) {
      try {
        await api.updateSessionSettings(cur.id, {
          allowed_dirs: next.length === 0 ? null : next,
        });
      } catch {
        /* ignore */
      }
      try {
        const fresh = await api.getSession(cur.id);
        set({ currentSession: fresh });
      } catch {
        /* ignore */
      }
    }
  },

  contextUsage: null,
  compacting: false,
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
      const fresh = await api.getSession(cur.id);
      set({ contextUsage: usage, currentSession: fresh });
    } finally {
      set({ compacting: false });
    }
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
    try {
      await api.approvePermission(
        pending.requestId,
        decision.kind,
        decision.kind === "deny_with_feedback" ? decision.feedback : undefined,
        decision.kind === "allow_and_remember" ? decision.pattern ?? null : null,
        decision.kind === "allow_and_remember"
          ? decision.scope ?? "session"
          : undefined
      );
    } catch (e) {
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
  queueWakeupForSession(sessionId, wakeupXml) {
    set((state) => ({
      pendingWakeups: { ...state.pendingWakeups, [sessionId]: wakeupXml },
    }));
  },
  async triggerWakeupResume(sessionId, wakeupXml) {
    const cur = get().currentSession;
    if (cur?.id === sessionId) {
      // 前台 session：复用 sendUserMessage 路径，backend 检测 checkpoint 走 resume
      await get().sendUserMessage(wakeupXml, []);
      return;
    }
    // 非前台：暂存到 pendingWakeups，等用户切到这个 session 时自动消费
    get().queueWakeupForSession(sessionId, wakeupXml);
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
      const persisted = await api.injectUserMessage(
        sessionId,
        requestId,
        target.content,
        target.attachments
      );
      // 把 user message 排在 streaming bubble **之后**临时显示——不动 messages 列表，
      // 避免它跑到当前正在跑的 assistant 之前。run 结束时 slot 整个被清掉，由 reload
      // 后的 session.messages 接管最终顺序。
      set((state) => {
        const slot = state.sessionStreams[sessionId];
        if (!slot) return state;
        const updated: SessionStream = {
          ...slot,
          injectedSinceStream: [...slot.injectedSinceStream, persisted],
        };
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
      // 加载工具清单（失败不影响主流程）
      api.listTools().then((tools) => set({ availableTools: tools })).catch(() => {}),
    ]);
    const first = get().sessions[0];
    if (first) await get().openSession(first.id);
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

  async openSession(id) {
    const s = await api.getSession(id);
    persistLastSessionConfig({
      providerId: s.provider_id,
      model: s.model,
      promptId: s.prompt_id ?? "",
    });
    // 切到这个对话时，让输入框 chip 显示的 pending 跟随该对话的实际 workdir / allowed_dirs。
    // 这样：
    // 1. 切对话 → chip 立即更新成目标对话的设置
    // 2. 用户在某对话里改完 pending，新建对话会继承（newSession 用 pending 注入）
    const sessionWorkdir = s.workdir ?? null;
    const sessionAllowedDirs = s.allowed_dirs ?? [];
    persistPendingWorkdir(sessionWorkdir);
    persistPendingAllowedDirs(sessionAllowedDirs);
    // 消费 pendingWakeup：切到该 session 时若有积压的 wakeup XML，立刻发出
    const pendingWakeup = get().pendingWakeups[id];
    set((state) => {
      const { [id]: _drop, ...rest } = state.pendingWakeups;
      return {
        currentSession: s,
        pendingPromptId: s.prompt_id ?? "",
        pendingWorkdir: sessionWorkdir,
        pendingAllowedDirs: sessionAllowedDirs,
        unreadFinishedSessions: removeFromSet(state.unreadFinishedSessions, id),
        currentInputQueue: state.inputQueues[id] ?? [],
        pendingWakeups: rest,
        ...mirrorFromSlot(state.sessionStreams[id]),
      };
    });
    if (pendingWakeup) {
      // 用 microtask 异步触发，避免在 openSession 内嵌套 sendUserMessage 的 set 调用
      queueMicrotask(() => {
        void get().sendUserMessage(pendingWakeup, []);
      });
    }
    get().refreshContextUsage();
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
    let s = await api.createSession(p.id, m, prompt ?? null, promptId);
    // 把"待继承"的 workdir / allowed_dirs 立即注入新 session：
    // 输入框 + 菜单的选择是跨对话黏的，新建对话时无需用户重新选。
    const inheritWorkdir = get().pendingWorkdir;
    const inheritAllowed = get().pendingAllowedDirs;
    if (inheritWorkdir || inheritAllowed.length > 0) {
      s = await api.updateSessionSettings(s.id, {
        ...(inheritWorkdir ? { workdir: inheritWorkdir } : {}),
        ...(inheritAllowed.length > 0 ? { allowed_dirs: inheritAllowed } : {}),
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

  async sendUserMessage(content, attachments = []) {
    const cur = get().currentSession;
    if (!cur) return;
    const sessionId = cur.id;
    // 当前 turn 跑完后（无论成功 / 失败 / 取消），把队首项作为下一轮自动 send。
    // 用微任务异步触发，避免在 finally 里递归调用栈过深。
    const drainNext = () => {
      queueMicrotask(() => {
        const state = get();
        if (state.currentSession?.id !== sessionId) return;
        const queue = state.inputQueues[sessionId];
        if (!queue || queue.length === 0) return;
        if (state.sessionStreams[sessionId]) return; // 还有 run 在跑（异常路径）
        const head = queue[0];
        state.removeQueuedInput(head.id);
        void state.sendUserMessage(head.content, head.attachments);
      });
    };
    const tempId = "streaming";
    const requestId =
      crypto.randomUUID?.() ??
      `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const initialSlot: SessionStream = {
      requestId,
      streamingMessageId: tempId,
      streamingText: "",
      streamingParts: [],
      injectedSinceStream: [],
      pendingApproval: null,
      pendingApprovalQueue: [],
      pendingQuestion: null,
      pendingQuestionQueue: [],
      autoJudgedNotes: [],
      currentRunMode: null,
      suspended: null,
    };
    set((state) => ({
      currentSession: appendOptimisticUserMessage(cur, content, attachments, {
        id: `pending-user-${requestId}`,
        now: Date.now(),
      }),
      sessionStreams: { ...state.sessionStreams, [sessionId]: initialSlot },
      runningSessions: new Set(state.runningSessions).add(sessionId),
      // sessionId === currentSession.id（这一刻一定是前台），同步镜像
      ...mirrorFromSlot(initialSlot),
    }));
    try {
      const isFirstRound = cur.messages.every((m) => m.role !== "user");
      // 传空数组：后端会优先用 session.enabled_tools，再 fallback 到全局 settings。
      // 工具的开关现在统一在「设置 → 对话设置」配置。
      await api.sendMessage(
        cur.id,
        content,
        attachments,
        cur.stream,
        [],
        requestId,
        (e: EngineEvent) => {
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
      );
      const stillForeground = get().currentSession?.id === sessionId;
      if (stillForeground) {
        const fresh = await api.getSession(sessionId);
        set((state) => {
          const { [sessionId]: _drop, ...rest } = state.sessionStreams;
          return {
            currentSession: fresh,
            sessionStreams: rest,
            runningSessions: removeFromSet(state.runningSessions, sessionId),
            ...mirrorFromSlot(undefined),
          };
        });
        get().refreshContextUsage();
      } else {
        set((state) => {
          const { [sessionId]: _drop, ...rest } = state.sessionStreams;
          return {
            sessionStreams: rest,
            runningSessions: removeFromSet(state.runningSessions, sessionId),
            unreadFinishedSessions: new Set(state.unreadFinishedSessions).add(
              sessionId
            ),
          };
        });
      }
      await get().refreshSessions();

      // 首轮对话完成后自动生成标题（失败不影响主流程）
      if (isFirstRound) {
        api
          .generateSessionTitle(sessionId)
          .then((s) => {
            if (get().currentSession?.id === s.id) {
              set({ currentSession: s });
            }
            get().refreshSessions();
          })
          .catch(() => {
            /* ignore */
          });
      }
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
          const fresh = await api.getSession(sessionId);
          set({ currentSession: fresh });
        }
        await get().refreshSessions();
        return;
      }
      try {
        const fresh = await api.getSession(sessionId);
        if (get().currentSession?.id === sessionId) {
          set({ currentSession: fresh });
        }
        await get().refreshSessions();
      } catch {
        if (get().currentSession?.id === sessionId) {
          set({ currentSession: cur });
        }
      }
      // 后台失败不向 UI 抛错（用户视野不在这里，吐 toast 也无意义）
      if (stillForeground) throw err;
    } finally {
      drainNext();
    }
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
    const idx = cur.messages.findIndex((m) => m.id === assistantMsgId);
    if (idx < 1) return;
    const prevUser = cur.messages[idx - 1];
    if (prevUser.role !== "user") return;
    await api.truncateInclusive(cur.id, prevUser.id);
    const refreshed = await api.getSession(cur.id);
    set({ currentSession: refreshed });
    await get().sendUserMessage(prevUser.content, prevUser.attachments ?? []);
  },

  async regenerateFromUser(userMsgId) {
    const cur = get().currentSession;
    if (!cur) return;
    const target = cur.messages.find((m) => m.id === userMsgId);
    if (!target || target.role !== "user") return;
    await api.truncateInclusive(cur.id, userMsgId);
    const refreshed = await api.getSession(cur.id);
    set({ currentSession: refreshed });
    await get().sendUserMessage(target.content, target.attachments ?? []);
  },

  async editAndRerun(userMsgId, content, attachments) {
    const cur = get().currentSession;
    if (!cur) return;
    const target = cur.messages.find((m) => m.id === userMsgId);
    if (!target || target.role !== "user") return;
    await api.truncateInclusive(cur.id, userMsgId);
    const refreshed = await api.getSession(cur.id);
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
    const s = await api.getSettings();
    set({ appSettings: s });
  },
  async saveAppSettings(settings: AppSettings) {
    await api.saveSettings(settings);
    set({ appSettings: settings });
  },
  async updateCurrentSessionSettings(patch) {
    const cur = get().currentSession;
    if (!cur) return;
    const updated = await api.updateSessionSettings(cur.id, patch);
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
    try {
      await api.approvePathAccess(
        pending.requestId,
        pending.paths ?? [],
        scope,
        sessionId
      );
      // 重新拉一下 session（this_project 时 allowed_dirs 已落盘）
      if (scope === "this_project") {
        const fresh = await api.getSession(sessionId);
        if (get().currentSession?.id === sessionId) {
          set({ currentSession: fresh });
        }
      } else if (scope === "all_project") {
        await get().refreshAppSettings();
      }
    } catch (e) {
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
