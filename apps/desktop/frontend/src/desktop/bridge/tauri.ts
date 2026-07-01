import { invoke, Channel, listen, subscribeSession, transportMode } from "./transport";
import type {
  AppSettings,
  ActiveGoal,
  AuthUrlResult,
  BranchInfo,
  CatalogCache,
  CodexTokenInfo,
  ContextUsage,
  DeviceCodeInfo,
  DiffPayload,
  DirEntry,
  GitProjectStatus,
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
  readClaudeSessionPreview: (path: string) =>
    invoke<ClaudeSessionPreview>("read_claude_session_preview", { path }),
  importClaudeSession: (
    path: string,
    projectId?: string | null,
    workdir?: string | null,
  ) =>
    invoke<Session>("import_claude_session", {
      path,
      projectId: projectId ?? null,
      workdir: workdir ?? null,
    }),
  forkSession: (sessionId: string, upToMessageId: string) =>
    invoke<Session>("fork_session", {
      sessionId,
      upToMessageId,
    }),

  // ---- 旁支对话（branch / aside session，架构 §8.5）----
  /** 从主对话 fork 一条只读旁支讨论；upToMessageId 为分叉点（含），null = 继承全部历史。 */
  branchCreate: (sessionId: string, upToMessageId?: string | null) =>
    invoke<BranchInfo>("branch_create", {
      sessionId,
      upToMessageId: upToMessageId ?? null,
    }),
  /** 向旁支发一轮消息，事件流走 EngineEvent channel（与主对话同款渲染）。 */
  branchSend: (
    branchId: string,
    content: string,
    attachments: MessageAttachment[],
    providerId: string | null,
    model: string | null,
    onEvent: (e: EngineEvent) => void
  ) => {
    const channel = new Channel<EngineEvent>();
    channel.onmessage = onEvent;
    return invoke<Message>("branch_send", {
      branchId,
      content,
      attachments,
      providerId,
      model,
      onEvent: channel,
    });
  },
  /** 关闭一条旁支（丢弃内存历史）。 */
  branchDiscard: (branchId: string) =>
    invoke<void>("branch_discard", { branchId }),
  /** 停止一条旁支正在跑的 run（置位 cancel flag 中断 agent loop）。 */
  branchCancel: (branchId: string) =>
    invoke<void>("branch_cancel", { branchId }),

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

  subscribeSessionEvents: async (
    sessionId: string,
    onEvent: (e: EngineEvent) => void
  ) => {
    await subscribeSession(sessionId);
    if (transportMode === "web") {
      return listen<EngineEvent>("engine-event", (e) => onEvent(e.payload));
    }
    const channel = new Channel<EngineEvent>();
    channel.onmessage = onEvent;
    await invoke<void>("subscribe_session_events", {
      sessionId,
      onEvent: channel,
    });
    return () => {
      channel.onmessage = null;
    };
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

  /**
   * 架构 §4.8.3 / §8：读取当前 session 的 //goal 目标（无目标返回 null）。
   */
  getActiveGoal: (sessionId: string) =>
    invoke<ActiveGoal | null>("get_active_goal", { sessionId }),

  /** 设置 //goal 目标条件（覆盖已有）。 */
  setActiveGoal: (sessionId: string, condition: string) =>
    invoke<void>("set_active_goal", { sessionId, condition }),

  /** 清除 //goal 目标。 */
  clearActiveGoal: (sessionId: string) =>
    invoke<void>("clear_active_goal", { sessionId }),

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
   * 探测粘贴/拖拽过来的路径形态：文件或目录都让前端加进 allowed_paths（引用语义，
   * 由 agent 按需 Read），找不到则回 missing 让前端当普通文本插入。不读文件内容。
   */
  attachPath: (path: string) =>
    invoke<
      | { kind: "file"; path: string }
      | { kind: "dir"; path: string }
      | { kind: "missing"; path: string }
    >("attach_path", { path }),

  /**
   * Desktop 原生拖拽分流：后端按每个磁盘路径判定——支持的小图片 / 文本读成附件
   * （kind 与 MessageAttachment 对齐），其余（目录 / 大文件 / 二进制 / 未知类型）回
   * reference 只引用路径，由前端加进 allowed_paths。missing = 路径不存在。
   */
  dropPaths: (paths: string[]) =>
    invoke<
      (
        | { kind: "image"; name: string; media_type: string; data: string }
        | { kind: "text_file"; name: string; media_type: string; content: string }
        | { kind: "reference"; path: string }
        | { kind: "missing"; path: string }
      )[]
    >("drop_paths", { paths }),

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

  /**
   * 列目录直接子项（非递归）——文件树面板按需展开一层。
   * dir-first 排序，隐藏项靠后；非目录 / 不存在会抛错。
   */
  readDir: (path: string) => invoke<DirEntry[]>("read_dir", { path }),

  /**
   * 把编辑器内容写回磁盘——文件查看器 Ctrl/Cmd+S 落盘。
   * 仅覆盖已存在的常规文件；新建 / 目录会抛错。
   */
  writeTextFile: (path: string, content: string) =>
    invoke<void>("write_text_file", { path, content }),

  /** 回退整个 Run 的 Edit。返回 `{ success, error? }`。 */
  revertEdit: (sessionId: string, runId: string) =>
    invoke<RevertResult>("revert_edit", { sessionId, runId }),

  // ── Git 源代码管理（架构 §4.12.13）──
  /** 列多个项目根的 git 状态（非 git 仓库的根自动跳过）。 */
  gitStatus: (roots: string[]) =>
    invoke<GitProjectStatus[]>("git_status", { roots }),

  /** 取某文件相对 git 的 diff 两侧文本。staged=true：HEAD vs index；false：index/HEAD vs 工作区。 */
  gitDiffFile: (root: string, path: string, staged: boolean) =>
    invoke<DiffPayload>("git_diff_file", { root, path, staged }),

  /** 暂存单个文件（git add）。 */
  gitStage: (root: string, path: string) =>
    invoke<void>("git_stage", { root, path }),

  /** 取消暂存单个文件（git reset HEAD）。 */
  gitUnstage: (root: string, path: string) =>
    invoke<void>("git_unstage", { root, path }),

  /** 丢弃单个文件工作区改动（不可逆）。tracked → checkout 还原；untracked → 删文件。 */
  gitDiscard: (root: string, path: string, untracked: boolean) =>
    invoke<void>("git_discard", { root, path, untracked }),

  /** 提交已暂存内容（不带 -a / 不 push）。返回新 commit 短 sha。 */
  gitCommit: (root: string, message: string) =>
    invoke<string>("git_commit", { root, message }),

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
  browserAllowUnload: (sessionId: string) => invoke<void>("browser_allow_unload", { sessionId }),
  browserSetBounds: (sessionId: string, bounds: { x: number; y: number; width: number; height: number }) =>
    invoke<void>("browser_set_bounds", { sessionId, ...bounds }),
  browserSetVisible: (sessionId: string, visible: boolean) =>
    invoke<void>("browser_set_visible", { sessionId, visible }),
  browserHideOthers: (keepSession: string) =>
    invoke<void>("browser_hide_others", { keepSession }),
  browserListOpen: () => invoke<Array<[string, string]>>("browser_list_open"),
  browserClose: (sessionId: string) => invoke<void>("browser_close", { sessionId }),
  browserPicker: (sessionId: string, active: boolean) =>
    invoke<void>("browser_picker", { sessionId, active }),
  browserStyleApply: (sessionId: string, prop: string, value: string) =>
    invoke<void>("browser_style_apply", { sessionId, prop, value }),
  browserStyleRevert: (sessionId: string) => invoke<void>("browser_style_revert", { sessionId }),
  browserStyleTakeDiff: (sessionId: string) => invoke<void>("browser_style_take_diff", { sessionId }),
  browserClearSelection: (sessionId: string) => invoke<void>("browser_clear_selection", { sessionId }),
  browserPopout: (sessionId: string) => invoke<void>("browser_popout", { sessionId }),
  browserClosePopout: (sessionId: string) =>
    invoke<void>("browser_close_popout", { sessionId }),

  // 内置终端（架构 §8 内置终端）。全局单例，不绑 session。
  terminalOpen: (cwd: string | null, cols: number, rows: number) =>
    invoke<string>("terminal_open", { cwd, cols, rows }),
  terminalWrite: (id: string, data: string) =>
    invoke<void>("terminal_write", { id, data }),
  terminalResize: (id: string, cols: number, rows: number) =>
    invoke<void>("terminal_resize", { id, cols, rows }),
  terminalClose: (id: string) => invoke<void>("terminal_close", { id }),
  terminalAttach: (id: string) =>
    invoke<{ scrollback_b64: string; alive: boolean }>("terminal_attach", { id }),
  terminalList: () =>
    invoke<{
      terminals: { id: string; cwd: string; alive: boolean }[];
      order: string[];
      active_view: "embedded" | "popout";
    }>("terminal_list"),
  terminalPopout: () => invoke<void>("terminal_popout"),
  terminalClosePopout: () => invoke<void>("terminal_close_popout"),

  // ── 微信渠道（架构 §7.5.1，Desktop 内嵌运行）──
  /** 申请登录二维码，返回 SVG（直接 inline 显示）+ 轮询用的 qrcode_id。 */
  wechatLoginStart: () =>
    invoke<WeChatQrCode>("wechat_login_start"),
  /** 轮询一次扫码状态；confirmed 时后端已存凭证并启动后台运行。 */
  wechatLoginPoll: (qrcodeId: string) =>
    invoke<WeChatLoginPoll>("wechat_login_poll", { qrcodeId }),
  /** 查询登录 / 运行状态。 */
  wechatStatus: () =>
    invoke<WeChatStatus>("wechat_status"),
  /** 用已存凭证启动后台运行（进程重启后重新拉起）。 */
  wechatStart: (botId: string) =>
    invoke<void>("wechat_start", { botId }),
  /** 停止后台运行（不删凭证）。 */
  wechatStop: () =>
    invoke<void>("wechat_stop"),
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

/** 一条 Claude 会话的完整预览数据（含消息列表供渲染）。 */
export interface ClaudeSessionPreview {
  title: string;
  model: string;
  cwd: string;
  messages: Message[];
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

/** 微信登录二维码：svg 直接 inline 显示，qrcode_id 用于轮询。 */
export interface WeChatQrCode {
  svg: string;
  qrcode_id: string;
}

/** 微信扫码轮询结果。confirmed 时后端已存凭证并启动后台运行。 */
export type WeChatLoginPoll =
  | { status: "waiting" }
  | { status: "scanned" }
  | { status: "confirmed"; bot_id: string }
  | { status: "expired" };

/** 微信渠道状态。 */
export interface WeChatStatus {
  logged_in: boolean;
  running: boolean;
  bot_id: string | null;
}
