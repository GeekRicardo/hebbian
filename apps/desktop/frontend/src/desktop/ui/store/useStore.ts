import { create } from "zustand";
import type {
  AppSettings,
  ApprovalDecisionPayload,
  ContextUsage,
  EngineEvent,
  MessageAttachment,
  PendingApproval,
  PendingQuestion,
  Prompt,
  PromptsFile,
  Provider,
  ProvidersFile,
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

function readStoredValue(key: string) {
  return localStorage.getItem(key) ?? "";
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

  // streaming
  streamingMessageId: string | null;
  streamingText: string;
  streamingParts: StreamingAssistantPart[];
  activeRequestId: string | null;

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
  streamingMessageId: null,
  streamingText: "",
  streamingParts: [],
  activeRequestId: null,
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
    JSON.parse(localStorage.getItem("enabledTools") ?? '["web_search","web_fetch"]')
  ),

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
    const pending = get().pendingApproval;
    if (!pending) return;
    // 乐观清空，避免双击
    const next = get().pendingApprovalQueue[0] ?? null;
    set({
      pendingApproval: next,
      pendingApprovalQueue: get().pendingApprovalQueue.slice(1),
    });
    try {
      await api.approvePermission(
        pending.requestId,
        decision.kind,
        decision.kind === "deny_with_feedback" ? decision.feedback : undefined
      );
    } catch (e) {
      // 失败时恢复弹窗，让用户重试
      set((state) => ({
        pendingApproval: pending,
        pendingApprovalQueue: next
          ? [next, ...state.pendingApprovalQueue]
          : state.pendingApprovalQueue,
      }));
      throw e;
    }
  },

  pendingQuestion: null,
  pendingQuestionQueue: [],
  async resolveQuestion(answer: QuestionAnswerPayload) {
    const pending = get().pendingQuestion;
    if (!pending) return;
    const next = get().pendingQuestionQueue[0] ?? null;
    set({
      pendingQuestion: next,
      pendingQuestionQueue: get().pendingQuestionQueue.slice(1),
    });
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
      set((state) => ({
        pendingQuestion: pending,
        pendingQuestionQueue: next
          ? [next, ...state.pendingQuestionQueue]
          : state.pendingQuestionQueue,
      }));
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
    set({
      currentSession: s,
      pendingPromptId: s.prompt_id ?? "",
      streamingMessageId: null,
      streamingText: "",
      streamingParts: [],
    });
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
    const s = await api.createSession(p.id, m, prompt ?? null, promptId);
    persistLastSessionConfig({
      providerId: s.provider_id,
      model: s.model,
      promptId: s.prompt_id ?? "",
    });
    set({
      currentSession: s,
      pendingPromptId: s.prompt_id ?? "",
      streamingParts: [],
    });
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
    set({
      currentSession: s,
      pendingPromptId: s.prompt_id ?? "",
      streamingParts: [],
    });
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
    const tempId = "streaming";
    const requestId =
      crypto.randomUUID?.() ??
      `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    set({
      currentSession: appendOptimisticUserMessage(cur, content, attachments, {
        id: `pending-user-${requestId}`,
        now: Date.now(),
      }),
      streamingMessageId: tempId,
      streamingText: "",
      streamingParts: [],
      activeRequestId: requestId,
      pendingApproval: null,
      pendingApprovalQueue: [],
      pendingQuestion: null,
      pendingQuestionQueue: [],
    });
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
          if (get().activeRequestId !== requestId) return;
          if (e.type === "text_delta") {
            set({
              streamingText: get().streamingText + e.text,
              streamingParts: applyTextDelta(get().streamingParts, e.text),
            });
          }
          // complete 路径（不支持 stream tools 的 provider）只发 text_done；
          // 这里把 full_text 一次性同步进 streamingText，避免界面上空白。
          if (e.type === "text_done") {
            const cur = get().streamingText;
            if (!cur || !e.full_text.endsWith(cur)) {
              const delta = cur ? "" : e.full_text;
              set({
                streamingText: e.full_text,
                streamingParts: delta
                  ? applyTextDelta(get().streamingParts, delta)
                  : get().streamingParts,
              });
            }
          }
          if (e.type === "reasoning") {
            set({
              streamingParts: applyReasoningDelta(get().streamingParts, e.text),
            });
          }
          if (e.type === "tool_call_delta") {
            set({
              streamingParts: applyToolCallDelta(
                get().streamingParts,
                e
              ),
            });
          }
          if (e.type === "tool_start") {
            set({
              streamingParts: applyToolStart(get().streamingParts, e),
            });
          }
          if (e.type === "tool_done") {
            set({
              streamingParts: applyToolDone(get().streamingParts, e),
            });
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
            };
            set((state) =>
              state.pendingApproval
                ? {
                    pendingApprovalQueue: [
                      ...state.pendingApprovalQueue,
                      approval,
                    ],
                  }
                : { pendingApproval: approval }
            );
          }
          if (e.type === "permission_resolved") {
            set((state) => {
              if (state.pendingApproval?.requestId === e.request_id) {
                const next = state.pendingApprovalQueue[0] ?? null;
                return {
                  pendingApproval: next,
                  pendingApprovalQueue: state.pendingApprovalQueue.slice(1),
                };
              }
              return {
                pendingApprovalQueue: state.pendingApprovalQueue.filter(
                  (item) => item.requestId !== e.request_id
                ),
              };
            });
          }
          if (e.type === "user_question_requested") {
            const question: PendingQuestion = {
                requestId: e.request_id,
                question: e.question,
                options: e.options,
                multi: e.multi ?? false,
            };
            set((state) =>
              state.pendingQuestion
                ? {
                    pendingQuestionQueue: [
                      ...state.pendingQuestionQueue,
                      question,
                    ],
                  }
                : { pendingQuestion: question }
            );
          }
          if (e.type === "user_question_answered") {
            set((state) => {
              if (state.pendingQuestion?.requestId === e.request_id) {
                const next = state.pendingQuestionQueue[0] ?? null;
                return {
                  pendingQuestion: next,
                  pendingQuestionQueue: state.pendingQuestionQueue.slice(1),
                };
              }
              return {
                pendingQuestionQueue: state.pendingQuestionQueue.filter(
                  (item) => item.requestId !== e.request_id
                ),
              };
            });
          }
        },
      );
      const fresh = await api.getSession(cur.id);
      set({
        currentSession: fresh,
        streamingMessageId: null,
        streamingText: "",
        streamingParts: [],
        activeRequestId: null,
        pendingApproval: null,
        pendingApprovalQueue: [],
        pendingQuestion: null,
        pendingQuestionQueue: [],
      });
      await get().refreshSessions();
      get().refreshContextUsage();

      // 首轮对话完成后自动生成标题（失败不影响主流程）
      if (isFirstRound) {
        api
          .generateSessionTitle(cur.id)
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
      set({
        streamingMessageId: null,
        streamingText: "",
        streamingParts: [],
        activeRequestId: null,
        pendingApproval: null,
        pendingApprovalQueue: [],
        pendingQuestion: null,
        pendingQuestionQueue: [],
      });
      if (String(err?.message ?? err).includes("请求已中断")) {
        const current = get().currentSession;
        if (current) {
          const fresh = await api.getSession(current.id);
          set({ currentSession: fresh });
          await get().refreshSessions();
        }
        return;
      }
      try {
        const fresh = await api.getSession(cur.id);
        if (get().currentSession?.id === cur.id) {
          set({ currentSession: fresh });
        }
        await get().refreshSessions();
      } catch {
        if (get().currentSession?.id === cur.id) {
          set({ currentSession: cur });
        }
      }
      throw err;
    }
  },

  async cancelStreaming() {
    const requestId = get().activeRequestId;
    if (!requestId) return;

    // 只发取消信号，**不在这里清掉 streamingParts / 也不 reload session**：
    // - 后端要花一两百 ms 才能把 partial output 落盘并返回「请求已中断」；
    // - 这中间 streamingParts 还得继续显示用户已经看到的内容；
    // - 落盘完后 sendUserMessage 的 .catch 会拉到带 partial 的最新 session，
    //   再统一清理 streaming 状态。提前 reload 会读到尚未 persist 的旧状态、
    //   把已经流到屏幕上的内容“吞掉”。
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
    const pending = get().pendingApproval;
    if (!pending) return;
    const sessionId = get().currentSession?.id ?? null;
    const next = get().pendingApprovalQueue[0] ?? null;
    set({
      pendingApproval: next,
      pendingApprovalQueue: get().pendingApprovalQueue.slice(1),
    });
    try {
      await api.approvePathAccess(
        pending.requestId,
        pending.paths ?? [],
        scope,
        sessionId
      );
      // 重新拉一下 session（this_project 时 allowed_dirs 已落盘）
      if (scope === "this_project" && sessionId) {
        const fresh = await api.getSession(sessionId);
        set({ currentSession: fresh });
      } else if (scope === "all_project") {
        await get().refreshAppSettings();
      }
    } catch (e) {
      set((state) => ({
        pendingApproval: pending,
        pendingApprovalQueue: next
          ? [next, ...state.pendingApprovalQueue]
          : state.pendingApprovalQueue,
      }));
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
