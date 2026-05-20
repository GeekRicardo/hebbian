import { invoke, Channel } from "@tauri-apps/api/core";
import type {
  AppSettings,
  AuthUrlResult,
  CodexTokenInfo,
  ContextUsage,
  DeviceCodeInfo,
  DiffPayload,
  EditEntry,
  EditsWorktreeStatus,
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
  RevertResult,
  RuleFileInfo,
  RuleFileState,
  SearchHit,
  Session,
  SessionBackgroundReport,
  SessionMeta,
  ToolInfo,
  WorkspaceProject,
  WorkspaceProjectInput,
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
    promptId?: string | null,
    workspace?: {
      project_id?: string | null;
      workdir?: string | null;
      allowed_paths?: string[];
    }
  ) =>
    invoke<Session>("create_session", {
      providerId,
      model,
      systemPrompt: systemPrompt ?? null,
      promptId: promptId ?? null,
      projectId: workspace?.project_id ?? null,
      workdir: workspace?.workdir ?? null,
      allowedPaths: workspace?.allowed_paths ?? null,
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

  // workspace projects
  listProjects: () => invoke<WorkspaceProject[]>("list_projects"),
  saveProject: (input: WorkspaceProjectInput) =>
    invoke<WorkspaceProject>("save_project", { input }),
  deleteProject: (id: string) => invoke<void>("delete_project", { id }),
  importVscodeProject: (path: string, name?: string | null) =>
    invoke<WorkspaceProject>("import_vscode_project", {
      path,
      name: name ?? null,
    }),
  importProjectFile: (path: string) =>
    invoke<WorkspaceProject>("import_project_file", { path }),
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
   * 「立即发送」入口：streaming 中往当前 run 的 pending 队列推一条 user message，
   * 后端持久化到 session.json 后返回 Message——前端拿到后立刻渲染到 chat 区域，
   * agent_loop 在下一次 model.request 之前会 drain 出来加入 transcript。
   */
  injectUserMessage: (
    sessionId: string,
    requestId: string,
    content: string,
    attachments: MessageAttachment[]
  ) =>
    invoke<Message>("inject_user_message", {
      sessionId,
      requestId,
      content,
      attachments,
    }),

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

  /**
   * 用户回应一次工具审批请求（HITL）。
   *
   * `scope` 仅对 `allow_and_remember` 有意义（架构 §4.5.3）：
   * - `"session"`（默认）：仅当前对话不再询问
   * - `"project"`：当前 workdir 所有对话不再询问（其他项目不受影响）
   * - `"global"`：写到 ~/.hebbian/permissions.json（workdir = null），所有对话生效
   */
  approvePermission: (
    requestId: string,
    decision: "allow_once" | "allow_and_remember" | "deny" | "deny_with_feedback",
    feedback?: string,
    pattern?: string | null,
    scope?: "session" | "project" | "global"
  ) =>
    invoke<void>("approve_permission", {
      requestId,
      decision,
      feedback: feedback ?? null,
      pattern: pattern ?? null,
      scope: scope ?? "session",
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

  /**
   * 架构 §4.4.4 / §8：读取当前 session 的 `force_automode` 子开关。
   * desktop 进程级状态，重启回归 false。
   */
  getForceAutomode: (sessionId: string) =>
    invoke<boolean>("get_force_automode", { sessionId }),

  /**
   * 切换 `force_automode` 子开关；返回设置后的最新值。
   * 由 `//force-automode [on|off|toggle]` 命令解析器调用。
   */
  setForceAutomode: (sessionId: string, enabled: boolean) =>
    invoke<boolean>("set_force_automode", { sessionId, enabled }),

  /**
   * 架构 §4.4.3 / §8：读取当前 session 的 [`RunMode`]。
   * desktop 进程级状态，重启回归 `AskBeforeEdits`。
   * 返回 PascalCase 字符串：`AskBeforeEdits` / `EditAutomatically` / `PlanMode` / `AutoMode`
   */
  getRunMode: (sessionId: string) =>
    invoke<string>("get_run_mode", { sessionId }),

  /** 设置当前 session 的 [`RunMode`]；mode 接受 PascalCase 或 kebab-case。 */
  setRunMode: (sessionId: string, mode: string) =>
    invoke<string>("set_run_mode", { sessionId, mode }),

  /** 获取所有可用工具的元信息（用于前端渲染工具开关） */
  listTools: () => invoke<ToolInfo[]>("list_tools"),

  /** 架构 §4.12.9：拉取当前 session 的后台任务报告（含 shells / cron / 挂起态）。 */
  listBackgroundTasks: (sessionId: string) =>
    invoke<SessionBackgroundReport>("list_background_tasks", { sessionId }),

  /** 强杀指定 session 的 bg shell。返回最终状态（exited/killed/failed）。 */
  killBackgroundTask: (sessionId: string, taskId: string) =>
    invoke<string>("kill_background_task", { sessionId, taskId }),

  // ── rules ──
  /** 从 workdir + allowed_paths 发现所有规则文件（CLAUDE.md / AGENTS.md 等） */
  discoverRulesFiles: (workdir: string, allowedPaths: string[]) =>
    invoke<RuleFileInfo[]>("discover_rules_files", { workdir, allowedPaths }),

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
      allowed_paths?: string[] | null;
      enabled_tools?: string[] | null;
      skill_dirs?: string[] | null;
      global_rules?: string[] | null;
      rules_files?: RuleFileState[] | null;
    }
  ) => {
    const args: Record<string, unknown> = { id };
    if ("workdir" in patch) {
      if (patch.workdir == null) args.clearWorkdir = true;
      else args.workdir = patch.workdir;
    }
    if ("allowed_paths" in patch) {
      if (patch.allowed_paths == null) args.clearAllowedPaths = true;
      else args.allowedPaths = patch.allowed_paths;
    }
    if ("enabled_tools" in patch) {
      if (patch.enabled_tools == null) args.clearEnabledTools = true;
      else args.enabledTools = patch.enabled_tools;
    }
    if ("skill_dirs" in patch) {
      if (patch.skill_dirs == null) args.clearSkillDirs = true;
      else args.skillDirs = patch.skill_dirs;
    }
    if ("global_rules" in patch) {
      if (patch.global_rules == null) args.clearGlobalRules = true;
      else args.globalRules = patch.global_rules;
    }
    if ("rules_files" in patch) {
      if (patch.rules_files == null) args.clearRulesFiles = true;
      else args.rulesFiles = patch.rules_files;
    }
    return invoke<Session>("update_session_settings", args);
  },

  /**
   * 探测剪切板/拖拽过来的路径：是文件就读出来当 attachment，是目录就提示前端
   * 加到 allowed_paths。前端只发一次 RPC，避免来回 stat 磁盘。
   */
  attachPath: (path: string) =>
    invoke<
      | { kind: "dir"; path: string; name: string }
      | { kind: "file"; attachment: MessageAttachment }
      | { kind: "missing"; path: string }
      | { kind: "unsupported"; path: string; reason: string }
    >("attach_path", { path }),

  /**
   * PathAccess 审批专用：scope 决定持久化到哪（架构 §4.5.3）：
   * - `"once"`：只放行本次
   * - `"this_session"`：当前 session 的 allowed_paths
   * - `"this_project"`：当前 workdir 的 PermissionStore Project FilePath 规则
   * - `"global"`：settings.conversation.allowed_paths + Global FilePath 规则
   */
  approvePathAccess: (
    requestId: string,
    paths: string[],
    scope: "once" | "this_session" | "this_project" | "global",
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

  // ── edits worktree（架构 §4.13）──
  /** 列出某 session 所有 Edit 快照条目。 */
  listEdits: (sessionId: string) =>
    invoke<EditEntry[]>("list_edits", { sessionId }),

  /** 获取某次 Edit 的 before/after 文本内容。 */
  diffEdit: (sessionId: string, snapshotId: string) =>
    invoke<DiffPayload>("diff_edit", { sessionId, snapshotId }),

  /** 回退单次 Edit。返回 `{ success, error? }`。 */
  revertEdit: (sessionId: string, snapshotId: string) =>
    invoke<RevertResult>("revert_edit", { sessionId, snapshotId }),

  /** 查询 edits-worktree 状态（git 是否可用 + 已累积条目数）。 */
  editsWorktreeStatus: (sessionId: string) =>
    invoke<EditsWorktreeStatus>("edits_worktree_status", { sessionId }),
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
