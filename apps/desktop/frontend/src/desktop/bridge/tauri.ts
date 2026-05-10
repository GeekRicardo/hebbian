import { invoke, Channel } from "@tauri-apps/api/core";
import type {
  AppSettings,
  AuthUrlResult,
  CodexTokenInfo,
  ContextUsage,
  DeviceCodeInfo,
  EngineEvent,
  FetchedModel,
  ImportedToken,
  Message,
  MessageAttachment,
  Prompt,
  PromptsFile,
  Provider,
  ProviderModelTestResult,
  ProviderPreset,
  ProvidersFile,
  ReasoningConfig,
  SearchHit,
  Session,
  SessionMeta,
  ToolInfo,
} from "@/desktop/ui/types";

export const api = {
  // providers
  getProviders: () => invoke<ProvidersFile>("get_providers"),
  saveProviders: (file: ProvidersFile) =>
    invoke<void>("save_providers", { file }),
  upsertProvider: (provider: Provider) =>
    invoke<Provider>("upsert_provider", { provider }),
  listProviderPresets: () =>
    invoke<ProviderPreset[]>("list_provider_presets"),
  fetchProviderModels: (provider: Provider) =>
    invoke<FetchedModel[]>("fetch_provider_models", { provider }),
  testProviderModel: (provider: Provider, model: string) =>
    invoke<ProviderModelTestResult>("test_provider_model", { provider, model }),

  // prompts
  listPrompts: () => invoke<PromptsFile>("list_prompts"),
  upsertPrompt: (prompt: Prompt) => invoke<Prompt>("upsert_prompt", { prompt }),
  deletePrompt: (id: string) => invoke<void>("delete_prompt", { id }),
  setDefaultPrompt: (id: string | null) =>
    invoke<PromptsFile>("set_default_prompt", { id }),

  // sessions
  listSessions: () => invoke<SessionMeta[]>("list_sessions"),
  getSession: (id: string) => invoke<Session>("get_session", { id }),
  createSession: (
    providerId: string,
    model: string,
    systemPrompt?: string | null,
    promptId?: string | null
  ) =>
    invoke<Session>("create_session", {
      providerId,
      model,
      systemPrompt: systemPrompt ?? null,
      promptId: promptId ?? null,
    }),
  renameSession: (id: string, title: string) =>
    invoke<Session>("rename_session", { id, title }),
  deleteSession: (id: string) => invoke<void>("delete_session", { id }),
  forkSession: (sessionId: string, upToMessageId: string) =>
    invoke<Session>("fork_session", {
      sessionId,
      upToMessageId,
    }),
  truncateAfter: (id: string, messageId: string) =>
    invoke<Session>("truncate_after", { id, messageId }),
  truncateInclusive: (id: string, messageId: string) =>
    invoke<Session>("truncate_inclusive", { id, messageId }),
  searchSessions: (query: string, caseSensitive: boolean, regex: boolean) =>
    invoke<SearchHit[]>("search_sessions", { query, caseSensitive, regex }),
  updateSessionConfig: (
    id: string,
    patch: {
      provider_id?: string;
      model?: string;
      system_prompt?: string;
      prompt_id?: string;
      stream?: boolean;
      /** 设为对象更新；设为 null 显式清空。 */
      reasoning?: ReasoningConfig | null;
    }
  ) => {
    const reasoningGiven = Object.prototype.hasOwnProperty.call(patch, "reasoning");
    return invoke<Session>("update_session_config", {
      id,
      providerId: patch.provider_id ?? null,
      model: patch.model ?? null,
      systemPrompt: patch.system_prompt ?? null,
      promptId: patch.prompt_id ?? null,
      stream: patch.stream ?? null,
      reasoning: reasoningGiven && patch.reasoning != null ? patch.reasoning : null,
      clearReasoning: reasoningGiven && patch.reasoning == null,
    });
  },
  switchProviderModel: (id: string, providerId: string, model: string) =>
    invoke<Session>("switch_provider_model", {
      id,
      newProviderId: providerId,
      newModel: model,
    }),
  generateSessionTitle: (id: string) =>
    invoke<Session>("generate_session_title", { id }),

  /** 发送消息，enabledTools 为本轮启用的工具名称列表（空 = 纯对话模式） */
  sendMessage: (
    sessionId: string,
    content: string,
    attachments: MessageAttachment[],
    stream: boolean,
    enabledTools: string[],
    requestId: string,
    onEvent: (e: EngineEvent) => void
  ) => {
    const channel = new Channel<EngineEvent>();
    channel.onmessage = onEvent;
    return invoke<Message>("send_message", {
      sessionId,
      content,
      attachments,
      stream,
      enabledTools,
      requestId,
      onEvent: channel,
    });
  },

  cancelMessage: (requestId: string) =>
    invoke<boolean>("cancel_message", { requestId }),

  /**
   * 预览「真实发给模型的 payload」(OpenAI 风格 messages + tools + workspace XML)。
   * `uptoMessageId` 截断到指定消息(含),用于在每条 bubble 上显示「这条出现时模型看到的载荷」。
   */
  previewSessionPayload: (sessionId: string, uptoMessageId?: string | null) =>
    invoke<unknown>("preview_session_payload", {
      sessionId,
      uptoMessageId: uptoMessageId ?? null,
    }),

  /** 当前 session 的上下文用量（用于输入框旁的环形进度条） */
  getContextUsage: (sessionId: string) =>
    invoke<ContextUsage>("get_context_usage", { sessionId }),

  /** 主动压缩当前 session 的上下文。返回压缩后的用量。 */
  compactSession: (sessionId: string, customInstructions?: string) =>
    invoke<ContextUsage>("compact_session", {
      sessionId,
      customInstructions: customInstructions ?? null,
    }),

  /** 用户回应一次工具审批请求（HITL） */
  approvePermission: (
    requestId: string,
    decision: "allow_once" | "allow_and_remember" | "deny" | "deny_with_feedback",
    feedback?: string,
    pattern?: string | null
  ) =>
    invoke<void>("approve_permission", {
      requestId,
      decision,
      feedback: feedback ?? null,
      pattern: pattern ?? null,
    }),

  /** 用户回应一次 agent 提问（ask 工具）。UI 未实装时可立即 "cancelled" */
  answerQuestion: (
    requestId: string,
    kind: "selected" | "selected_multi" | "custom" | "cancelled",
    payload?: { text?: string; labels?: string[] }
  ) =>
    invoke<void>("answer_question", {
      requestId,
      kind,
      text: payload?.text ?? null,
      labels: payload?.labels ?? null,
    }),

  /** 获取所有可用工具的元信息（用于前端渲染工具开关） */
  listTools: () => invoke<ToolInfo[]>("list_tools"),

  // ── settings ──
  getSettings: () => invoke<AppSettings>("get_settings"),
  saveSettings: (settings: AppSettings) =>
    invoke<void>("save_settings", { settings }),

  /**
   * 更新对话级设置；任一字段不传 = 保持原值，传 `null` = 显式清空（回退全局默认）。
   *
   * 实现把每个字段拆成两个 IPC 参数（设值 vs 清空），绕过 Tauri/serde 对
   * `Option<Option<T>>` 中 `null` 直接折叠成外层 None 的歧义——清空请求若被
   * 折叠就再也无法表达，会造成 chip 看似清掉但 session.workdir 还在的 bug。
   */
  updateSessionSettings: (
    id: string,
    patch: {
      workdir?: string | null;
      allowed_dirs?: string[] | null;
      enabled_tools?: string[] | null;
      skill_dirs?: string[] | null;
    }
  ) => {
    const args: Record<string, unknown> = { id };
    if ("workdir" in patch) {
      if (patch.workdir == null) args.clearWorkdir = true;
      else args.workdir = patch.workdir;
    }
    if ("allowed_dirs" in patch) {
      if (patch.allowed_dirs == null) args.clearAllowedDirs = true;
      else args.allowedDirs = patch.allowed_dirs;
    }
    if ("enabled_tools" in patch) {
      if (patch.enabled_tools == null) args.clearEnabledTools = true;
      else args.enabledTools = patch.enabled_tools;
    }
    if ("skill_dirs" in patch) {
      if (patch.skill_dirs == null) args.clearSkillDirs = true;
      else args.skillDirs = patch.skill_dirs;
    }
    return invoke<Session>("update_session_settings", args);
  },

  /**
   * 探测剪切板/拖拽过来的路径：是文件就读出来当 attachment，是目录就提示前端
   * 加到 allowed_dirs。前端只发一次 RPC，避免来回 stat 磁盘。
   */
  attachPath: (path: string) =>
    invoke<
      | { kind: "dir"; path: string; name: string }
      | { kind: "file"; attachment: MessageAttachment }
      | { kind: "missing"; path: string }
      | { kind: "unsupported"; path: string; reason: string }
    >("attach_path", { path }),

  /** PathAccess 审批专用：scope 决定持久化到哪。"this_project" = session，"all_project" = 全局，"once" = 只放行本次 */
  approvePathAccess: (
    requestId: string,
    paths: string[],
    scope: "once" | "this_project" | "all_project",
    sessionId: string | null
  ) =>
    invoke<void>("approve_path_access", {
      requestId,
      paths,
      scope,
      sessionId,
    }),

  // oauth — OpenAI/Codex Device flow
  oauthCodexStart: () => invoke<DeviceCodeInfo>("oauth_codex_start"),
  oauthCodexPoll: (deviceCode: string) =>
    invoke<CodexTokenInfo | null>("oauth_codex_poll", { deviceCode }),
  oauthCodexRefresh: (refreshToken: string) =>
    invoke<CodexTokenInfo>("oauth_codex_refresh", { refreshToken }),

  // oauth — OpenAI PKCE 浏览器流程
  oauthOpenAIStart: () => invoke<AuthUrlResult>("oauth_openai_start"),
  oauthOpenAIExchange: (sessionId: string, code: string, state?: string) =>
    invoke<ImportedToken>("oauth_openai_exchange", {
      sessionId,
      code,
      state: state ?? null,
    }),

  // oauth — Claude Code
  oauthClaudeStart: () => invoke<AuthUrlResult>("oauth_claude_start"),
  oauthClaudeExchange: (sessionId: string, code: string) =>
    invoke<ImportedToken>("oauth_claude_exchange", { sessionId, code }),
  oauthClaudeRefresh: (refreshToken: string) =>
    invoke<ImportedToken>("oauth_claude_refresh", { refreshToken }),
  oauthClaudeCodeImport: () =>
    invoke<ImportedToken>("oauth_claude_code_import"),

  // oauth — Gemini
  oauthGeminiStart: () => invoke<AuthUrlResult>("oauth_gemini_start"),
  oauthGeminiExchange: (sessionId: string, code: string) =>
    invoke<ImportedToken>("oauth_gemini_exchange", { sessionId, code }),
  oauthGeminiRefresh: (
    refreshToken: string,
    clientId: string,
    clientSecret: string
  ) =>
    invoke<ImportedToken>("oauth_gemini_refresh", {
      refreshToken,
      clientId,
      clientSecret,
    }),
  oauthGeminiCliImport: () =>
    invoke<ImportedToken>("oauth_gemini_cli_import"),

  // DeepSeek 账号登录（chat.deepseek.com 入口），返回 token，前端把它写入
  // provider.api_key 作为 Bearer。
  deepseekLogin: (input: DeepseekLoginInput) =>
    invoke<DeepseekLoginToken>("deepseek_login", { input }),
};

export interface DeepseekLoginInput {
  email?: string | null;
  mobile?: string | null;
  area_code?: string | null;
  password: string;
  device_id?: string | null;
}

export interface DeepseekLoginToken {
  token: string;
  login: string;
}
