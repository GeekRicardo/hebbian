import { invoke, Channel } from "./transport";
import type {
  AppSettings,
  AuthUrlResult,
  CatalogCache,
  CodexTokenInfo,
  ContextUsage,
  DeviceCodeInfo,
  DiffPayload,
  RunEditEntry,
  EditsWorktreeStatus,
  EngineEvent,
  FetchedModel,
  ImportedToken,
  Message,
  MessageAttachment,
  MessageMeta,
  McpConfig,
  McpToolReport,
  MemoryL0,
  PlanComment,
  PlanMeta,
  PluginListItem,
  Prompt,
  PromptsFile,
  Provider,
  ProviderModelTestResult,
  ProviderPreset,
  ProviderUsageResult,
  ProvidersFile,
  ReasoningConfig,
  RevertResult,
  RuleFileInfo,
  RuleFileState,
  SearchHit,
  Session,
  BackgroundTaskOutputDto,
  SessionBackgroundReport,
  SessionMeta,
  SkillItem,
  TodoItem,
  ToolInfo,
  LogLine,
  WorkspaceProject,
  WorkspaceProjectInput,
} from "@/desktop/ui/types";

export interface InjectUserMessageResult {
  message: Message;
  injected: boolean;
}

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
  fetchProviderUsage: (providerId: string) =>
    invoke<ProviderUsageResult>("fetch_provider_usage", { providerId }),
  testProviderModel: (provider: Provider, model: string) =>
    invoke<ProviderModelTestResult>("test_provider_model", { provider, model }),
  getModelsCatalog: () => invoke<CatalogCache>("get_models_catalog"),
  refreshModelsCatalog: () => invoke<boolean>("refresh_models_catalog"),

  // 记忆查看（架构 §4.14）
  listMemories: (workdir: string | null) =>
    invoke<MemoryL0[]>("list_memories", { workdir }),
  readMemory: (id: string, workdir: string | null) =>
    invoke<string>("read_memory", { id, workdir }),

  // prompts
  listPrompts: () => invoke<PromptsFile>("list_prompts"),
  upsertPrompt: (prompt: Prompt) => invoke<Prompt>("upsert_prompt", { prompt }),
  deletePrompt: (id: string) => invoke<void>("delete_prompt", { id }),
  setDefaultPrompt: (id: string | null) =>
    invoke<PromptsFile>("set_default_prompt", { id }),

  // sessions
  listSessions: () => invoke<SessionMeta[]>("list_sessions"),
  getSession: (id: string, activeRequestId?: string | null) =>
    invoke<Session>("get_session", { id, activeRequestId: activeRequestId ?? null }),
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
  exportSessionToClaude: (sessionId: string, includeThinking: boolean) =>
    invoke<ClaudeResumeResult>("export_session_to_claude", {
      sessionId,
      includeThinking,
    }),
  listClaudeSessions: () =>
    invoke<ClaudeSessionInfo[]>("list_claude_sessions"),
  importClaudeSession: (path: string) =>
    invoke<Session>("import_claude_session", { path }),
  forkSession: (sessionId: string, upToMessageId: string) =>
    invoke<Session>("fork_session", {
      sessionId,
      upToMessageId,
    }),
  truncateAfter: (id: string, messageId: string) =>
    invoke<Session>("truncate_after", { id, messageId }),
  truncateInclusive: (id: string, messageId: string) =>
    invoke<Session>("truncate_inclusive", { id, messageId }),
  undoCompaction: (id: string, markerId: string) =>
    invoke<Session>("undo_compaction", { id, markerId }),
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

  /**
   * 发送消息，enabledTools 为本轮启用的工具名称列表（空 = 纯对话模式）。
   * meta 可选：传入则给落盘的 user message 附加 metadata（架构 §4.12.5），
   * idle 路径下的 wakeup notification 走这里时带 `{type:"system_notification", ...}`。
   */
  sendMessage: (
    sessionId: string,
    content: string,
    attachments: MessageAttachment[],
    stream: boolean,
    enabledTools: string[],
    requestId: string,
    onEvent: (e: EngineEvent) => void,
    meta?: MessageMeta | null,
    // 「继续」入口（架构 §4.3）：true = 不追加 user 消息，原样再起一次 agent_loop。
    continueRun?: boolean
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
      meta: meta ?? null,
      continueRun: continueRun ?? false,
      onEvent: channel,
    });
  },

  cancelMessage: (requestId: string) =>
    invoke<boolean>("cancel_message", { requestId }),

  /**
   * 「立即发送」入口（架构 §4.12.5 修订）：
   *
   * 即写即落——后端**先**把 user message 追加到 session.jsonl（带 meta 标记），
   * **再**推到当前 run 的 pending 队列。run 在跑则 agent_loop 在下一次 model.request
   * 之前 drain；run 已结束也不报错——消息已落盘，下次发消息会从 jsonl rebuild 看到。
   *
   * meta 可选：wakeup notification 路径传 `{type:"system_notification", kind:"bg_task_finished", task_id, tool_use_id}`；
   * 普通用户插队不传（meta=null）。
   */
  injectUserMessage: (
    sessionId: string,
    requestId: string,
    content: string,
    attachments: MessageAttachment[],
    meta?: MessageMeta | null
  ) =>
    invoke<InjectUserMessageResult>("inject_user_message", {
      sessionId,
      requestId,
      content,
      attachments,
      meta: meta ?? null,
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

  /**
   * 拉 session 的 model_io.jsonl 摘要列表（不含 request.messages / response 正文）。
   * 每条 shape：`{ ts, run_id, turn, model, kind, duration_ms, response: {type, usage}, message_count }`。
   */
  listSessionModelIo: (sessionId: string) =>
    invoke<unknown[]>("list_session_model_io", { sessionId }),

  /** 按索引拉单条完整 model_io entry（含 request.messages + response 正文）。 */
  getSessionModelIoEntry: (sessionId: string, index: number) =>
    invoke<unknown | null>("get_session_model_io_entry", { sessionId, index }),

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
    scope?: "session" | "project" | "global",
    extraPatterns?: string[]
  ) =>
    invoke<void>("approve_permission", {
      requestId,
      decision,
      feedback: feedback ?? null,
      pattern: pattern ?? null,
      scope: scope ?? "session",
      extraPatterns: extraPatterns ?? [],
    }),

  /** 用户回应一次 agent 提问（ask 工具）。UI 未实装时可立即 "cancelled" */
  answerQuestion: (
    requestId: string,
    kind: "selected" | "selected_multi" | "custom" | "cancelled" | "multi",
    payload?: { text?: string; labels?: string[]; items?: any[] }
  ) =>
    invoke<void>("answer_question", {
      requestId,
      kind,
      text: payload?.text ?? null,
      labels: payload?.labels ?? null,
      items: payload?.items ?? null,
    }),

  /**
   * 架构 §4.4.4 / §8：读取当前 session 的 `force_automode` 子开关。
   * desktop 进程级状态，重启回归 false。
   */
  getForceAutomode: (sessionId: string) =>
    invoke<boolean>("get_force_automode", { sessionId }),

  /**
   * 切换 `force_automode` 子开关；返回设置后的最新值。
   * 由 `//hands-off [on|off|toggle]` 命令解析器调用。
   */
  setForceAutomode: (sessionId: string, enabled: boolean) =>
    invoke<boolean>("set_force_automode", { sessionId, enabled }),

  /**
   * 架构 §4.4.3 / §8：读取当前 session 的 [`RunMode`]。
   * desktop 进程级状态，重启回归 `Default`。
   * 返回 PascalCase 字符串：`Default` / `PlanMode` / `AutoMode`
   */
  getRunMode: (sessionId: string) =>
    invoke<string>("get_run_mode", { sessionId }),

  /** 设置当前 session 的 [`RunMode`]；mode 接受 PascalCase 或 kebab-case。 */
  setRunMode: (sessionId: string, mode: string) =>
    invoke<string>("set_run_mode", { sessionId, mode }),

  /** 获取所有可用工具的元信息（用于前端渲染工具开关） */
  listTools: () => invoke<ToolInfo[]>("list_tools"),

  /**
   * 架构 §6.1.3 / §8：列出当前 workdir 下加载的三层 skills——
   * `~/.hebbian/skills`、`~/.hebbian/projects/<enc>/skills`、`<workdir>/.claude/skills`。
   * 用于 `//<skill-name>` 命令的注册表与 popup 列表。
   *
   * 无 workdir 时传 `"."`，后端只能列到 global 那一层（project / project_code 会因
   * 路径无效自动跳过）；与 SkillsPane 的处理一致。
   */
  listSkills: (workdir: string) =>
    invoke<SkillItem[]>("list_skills", { workdir }),

  /** 架构 §4.12.9：拉取当前 session 的后台任务报告（含 shells / cron / 挂起态）。 */
  listBackgroundTasks: (sessionId: string) =>
    invoke<SessionBackgroundReport>("list_background_tasks", { sessionId }),

  /** 强杀指定 session 的 bg shell。返回最终状态（exited/killed/failed）。 */
  killBackgroundTask: (sessionId: string, taskId: string) =>
    invoke<string>("kill_background_task", { sessionId, taskId }),

  /**
   * polling 某个后台 task 的最新输出 + 状态。前端每个展开的卡片维护自己的
   * cursor（上一次返回的 total_bytes），传回后只拿增量；同一 task 多个监听
   * 互不干扰（read_at 不动 shell 内部 read_cursor）。
   */
  readBackgroundTaskOutput: (
    sessionId: string,
    taskId: string,
    cursor: number
  ) =>
    invoke<BackgroundTaskOutputDto>("read_background_task_output", {
      sessionId,
      taskId,
      cursor,
    }),

  // ── rules ──
  /** 从 workdir + allowed_paths 发现所有规则文件（CLAUDE.md / AGENTS.md 等） */
  discoverRulesFiles: (workdir: string, allowedPaths: string[]) =>
    invoke<RuleFileInfo[]>("discover_rules_files", { workdir, allowedPaths }),

  // ── settings ──
  getSettings: () => invoke<AppSettings>("get_settings"),
  saveSettings: (settings: AppSettings) =>
    invoke<void>("save_settings", { settings }),
  getMcpConfig: () => invoke<McpConfig>("get_mcp_config"),
  saveMcpConfig: (config: McpConfig) =>
    invoke<void>("save_mcp_config", { config }),
  discoverMcpTools: () => invoke<McpToolReport[]>("discover_mcp_tools"),

  // ── plugins ──
  pluginMarketplaceAdd: (source: string) =>
    invoke<string>("plugin_marketplace_add", { source }),
  pluginMarketplaceList: () =>
    invoke<[string, string][]>("plugin_marketplace_list"),
  pluginMarketplaceListPlugins: (name: string) =>
    invoke<{ name: string; description?: string | null }[]>(
      "plugin_marketplace_list_plugins",
      { name },
    ),
  pluginMarketplaceRemove: (name: string) =>
    invoke<void>("plugin_marketplace_remove", { name }),
  pluginInstall: (name: string, marketplace?: string | null) =>
    invoke<PluginListItem>("plugin_install", {
      name,
      marketplace: marketplace ?? null,
    }),
  pluginUninstall: (name: string) =>
    invoke<void>("plugin_uninstall", { name }),
  pluginList: () => invoke<PluginListItem[]>("plugin_list"),

  // ── hooks ──
  getHooksRaw: () => invoke<string>("get_hooks_raw"),
  saveHooksRaw: (raw: string) => invoke<void>("save_hooks_raw", { raw }),

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
  /** 列出某 session 所有 Run 修改条目。 */
  listEdits: (sessionId: string) =>
    invoke<RunEditEntry[]>("list_edits", { sessionId }),

  /** 获取某 Run 某文件的 before/after 文本内容。 */
  diffEdit: (sessionId: string, runId: string, filePath: string) =>
    invoke<DiffPayload>("diff_edit", { sessionId, runId, filePath }),

  /**
   * 读盘文件文本——服务于 UI 渲染（如 Edit diff 在原文里 indexOf 定位真实行号）。
   * 上限 8MiB；不是目录 / 不存在会抛错，调用方需 catch fallback。
   */
  readTextFile: (path: string) => invoke<string>("read_text_file", { path }),

  /** 回退整个 Run 的 Edit。返回 `{ success, error? }`。 */
  revertEdit: (sessionId: string, runId: string) =>
    invoke<RevertResult>("revert_edit", { sessionId, runId }),

  /** 查询 edits-worktree 状态（git 是否可用 + 已累积条目数）。 */
  editsWorktreeStatus: (sessionId: string) =>
    invoke<EditsWorktreeStatus>("edits_worktree_status", { sessionId }),

  // ── log stream ──
  /** 订阅实时日志流。传入 handler；取消时调返回的 cancel 函数（将 active 置 false 忽略后续推送）。 */
  subscribeLogStream: (onLog: (line: LogLine) => void): (() => void) => {
    let active = true;
    const channel = new Channel<LogLine>();
    channel.onmessage = (line) => { if (active) onLog(line); };
    invoke<void>("subscribe_log_stream", { onLog: channel }).catch(() => {});
    return () => { active = false; };
  },
  /** 读取今天的日志文件内容（供 LogPane 加载历史）。文件不存在返回空字符串。 */
  readLogFile: () =>
    invoke<string>("read_log_file"),
  /** 打开独立日志查看器窗口（单例，已存在则聚焦）。 */
  openLogViewerWindow: () =>
    invoke<void>("open_log_viewer_window"),
  /** 设置日志查看器窗口是否永远置顶。 */
  setLogViewerAlwaysOnTop: (alwaysOnTop: boolean) =>
    invoke<void>("set_log_viewer_always_on_top", { alwaysOnTop }),

  // ── Todo / Plan / Plan Comments（架构 §4.4.5 / §4.4.6）──
  /** 当前 session 的 todo 列表（从 jsonl 折叠出）。打开 session / 切换时拉一次，之后跟事件增量。 */
  listTodos: (sessionId: string) =>
    invoke<TodoItem[]>("list_todos", { sessionId }),

  /** 列出 session 下所有历史 plan（按 mtime 倒序），用于 PlanTab 顶部下拉。 */
  listSessionPlans: (sessionId: string) =>
    invoke<PlanMeta[]>("list_session_plans", { sessionId }),

  /** 读取指定 plan 文件的 markdown 内容。 */
  readPlanMarkdown: (sessionId: string, planId: string) =>
    invoke<string>("read_plan_markdown", { sessionId, planId }),

  /** 用户"编辑后通过"路径：覆盖 plan 文件内容；调用方随后发 allow_once 即可。 */
  updatePlanMarkdown: (sessionId: string, planId: string, markdown: string) =>
    invoke<void>("update_plan_markdown", { sessionId, planId, markdown }),

  /** 列出某 plan 的所有评论（含已消费）。 */
  listPlanComments: (sessionId: string, planId: string) =>
    invoke<PlanComment[]>("list_plan_comments", { sessionId, planId }),

  /** 给指定 plan 加一条评论。返回带 id / created_at_ms 填好的 comment。 */
  addPlanComment: (
    sessionId: string,
    planId: string,
    anchor: string,
    body: string
  ) =>
    invoke<PlanComment>("add_plan_comment", {
      sessionId,
      planId,
      anchor,
      body,
    }),

  // 内置浏览器（架构 §8.5）。origin: "auto"=自动通道仅本地 / "user"=用户主动可公网。
  browserOpen: (
    sessionId: string,
    url: string,
    origin: "auto" | "user",
    bounds: { x: number; y: number; width: number; height: number }
  ) =>
    invoke<string>("browser_open", {
      sessionId,
      url,
      origin,
      x: bounds.x,
      y: bounds.y,
      width: bounds.width,
      height: bounds.height,
    }),
  browserNavigate: (sessionId: string, url: string) =>
    invoke<string>("browser_navigate", { sessionId, url }),
  browserBack: (sessionId: string) => invoke<void>("browser_back", { sessionId }),
  browserForward: (sessionId: string) => invoke<void>("browser_forward", { sessionId }),
  browserReload: (sessionId: string) => invoke<void>("browser_reload", { sessionId }),
  browserSetBounds: (sessionId: string, bounds: { x: number; y: number; width: number; height: number }) =>
    invoke<void>("browser_set_bounds", { sessionId, ...bounds }),
  browserSetVisible: (sessionId: string, visible: boolean) =>
    invoke<void>("browser_set_visible", { sessionId, visible }),
  browserHideOthers: (keepSession: string) =>
    invoke<void>("browser_hide_others", { keepSession }),
  browserClose: (sessionId: string) => invoke<void>("browser_close", { sessionId }),
  browserPicker: (sessionId: string, active: boolean) =>
    invoke<void>("browser_picker", { sessionId, active }),
  browserStyleApply: (sessionId: string, prop: string, value: string) =>
    invoke<void>("browser_style_apply", { sessionId, prop, value }),
  browserStyleRevert: (sessionId: string) => invoke<void>("browser_style_revert", { sessionId }),
  browserStyleTakeDiff: (sessionId: string) => invoke<void>("browser_style_take_diff", { sessionId }),
  browserClearSelection: (sessionId: string) => invoke<void>("browser_clear_selection", { sessionId }),
  browserPopout: (sessionId: string) => invoke<void>("browser_popout", { sessionId }),
  browserClosePopout: () => invoke<void>("browser_close_popout"),
};

/** 导出为 Claude 会话的结果：`claude --resume <uuid>` 可直接恢复。 */
export interface ClaudeResumeResult {
  session_uuid: string;
  resume_command: string;
  path: string;
}

/** 一个可从 Claude 导入的会话概要（扫描 ~/.claude/projects 得到）。 */
export interface ClaudeSessionInfo {
  path: string;
  uuid: string;
  title: string;
  cwd: string;
  message_count: number;
  modified_ms: number;
}

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
