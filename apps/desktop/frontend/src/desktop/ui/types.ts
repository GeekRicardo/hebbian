export type ProviderKind = "openai" | "anthropic" | "gemini" | "deepseek";

export type AuthMode =
  | "api_key"
  | "oauth_codex"
  | "oauth_claude_code"
  | "oauth_gemini_cli";

export interface Provider {
  id: string;
  name: string;
  kind: ProviderKind;
  enabled?: boolean;
  auth_mode: AuthMode;
  base_url: string;
  api_key: string;
  refresh_token?: string | null;
  token_expires_at?: number | null;
  account_id?: string | null;
  extra_headers: Record<string, string>;
  models: string[];
  default_model?: string | null;
  /** 是否把这个 provider 用作「标题生成模型」。整份配置最多一个 provider 应该勾上。 */
  title_gen_enabled?: boolean;
  /** 配合 title_gen_enabled 的具体模型 id（必须出现在 `models` 列表中）。 */
  title_gen_model?: string | null;
  /** 注入 Claude Code 客户端特征（banner / billing header / metadata / context_management）。 */
  claude_code_compat?: boolean;
}

export interface ProvidersFile {
  providers: Provider[];
  default_provider_id?: string | null;
}

export interface ProviderPreset {
  id: string;
  name: string;
  kind: ProviderKind;
  base_url: string;
  models: string[];
  default_model?: string | null;
  website: string;
  note: string;
}

export interface FetchedModel {
  id: string;
  owned_by?: string | null;
}

export interface ProviderModelTestResult {
  model: string;
  prompt: string;
  response_preview: string;
  input_tokens: number;
  output_tokens: number;
}

export interface Prompt {
  id: string;
  name: string;
  avatar: string;
  content: string;
  created_at: number;
  updated_at: number;
}

export interface PromptsFile {
  default_prompt_id?: string | null;
  prompts: Prompt[];
}

export interface WorkspaceFolder {
  path: string;
  name?: string | null;
}

export interface WorkspaceProject {
  id: string;
  name: string;
  folders: WorkspaceFolder[];
  source?: string | null;
  created_at: number;
  updated_at: number;
}

export interface WorkspaceProjectInput {
  id?: string | null;
  name: string;
  workdir: string;
  allowed_paths?: string[];
  source?: string | null;
}

export type Role = "system" | "user" | "assistant" | "marker";

export type MessageMeta =
  | {
      type: "switch";
      from_provider: string;
      from_model: string;
      to_provider: string;
      to_model: string;
    }
  | {
      type: "interrupted";
    }
  | {
      type: "compact_boundary";
      summary: string;
      before_tokens: number;
      after_tokens: number;
    }
  | {
      type: "reasoning_switch";
      from?: ReasoningConfig | null;
      to?: ReasoningConfig | null;
    }
  // 架构 §4.12.5 修订：系统注入的通知（wakeup / cron 等）。物理 role 仍为 user
  // （喂给 model API），但 view 层据此渲染成系统通知条而不是用户气泡。
  | {
      type: "system_notification";
      /** 通知来源类别：bg_task_finished / cron_fired 等。 */
      kind: string;
      /** 关联的后台 task_id（bg_task_finished 才有）。 */
      task_id?: string | null;
      /** 触发该通知的 tool_call.id；surface 用它把通知关联回 tool_call 卡片。 */
      tool_use_id?: string | null;
    };

/** 当前 session 的上下文用量（来自 get_context_usage / compact_session） */
export interface ContextUsage {
  used_tokens: number;
  budget_tokens: number;
}

/** 整个 session 累积的 token 用量（落盘在 session.json 的 token_stats 字段） */
export interface TokenStats {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  run_count: number;
}

export interface Message {
  id: string;
  role: Role;
  content: string;
  attachments?: MessageAttachment[];
  tool_calls?: MessageToolCall[];
  parts?: MessagePart[];
  created_at: number;
  meta?: MessageMeta | null;
  /** 子 NestedRun 事件来源标识（架构 §4.4.11.8）。`存在` 时前端按此字段嵌套渲染到父 Task 卡片内部。 */
  subagent_call_id?: string | null;
}

export interface MessageToolCall {
  id: string;
  name: string;
  input: unknown;
  result?: string | null;
  duration_ms?: number | null;
}

export type ToolCallStatus = "streaming" | "running" | "done";

export type MessagePart =
  | {
      type: "text";
      text: string;
    }
  | {
      type: "reasoning";
      text: string;
    }
  | {
      type: "tool_call";
      id: string;
      name: string;
      input: unknown;
      arguments?: string;
      result?: string | null;
      duration_ms?: number | null;
      /** 工具输出超阈值时落盘的工件路径（架构 §4.4.9 / §4.12.11 Phase 2） */
      artifact_path?: string | null;
    };

export type StreamingAssistantPart =
  | {
      type: "text";
      text: string;
    }
  | {
      type: "reasoning";
      text: string;
    }
  | {
      type: "tool_call";
      index: number;
      id?: string | null;
      name?: string | null;
      arguments: string;
      input?: unknown;
      result?: string | null;
      duration_ms?: number | null;
      status: ToolCallStatus;
      /** 工具输出超阈值时落盘的工件路径（架构 §4.4.9 / §4.12.11 Phase 2） */
      artifact_path?: string | null;
      /**
       * 工具执行中的流式输出累积（架构 §4.4.1）。Bash 前台等待期间
       * `tool_output_delta` 事件按 chunk 追加到这里，渲染层在 status="running"
       * 时把它当作"实时控制台"显示。`tool_done.result` 到来后变成聚合最终结果，
       * 这个字段可保留供折叠展示，亦可清空。
       */
      live_output?: string;
      /**
       * Task 工具的嵌套子事件（架构 §4.4.11.8 / P7）。
       * 带 `subagent_call_id == 本 call.id` 的子事件路由到这里，
       * 渲染层在 Task 卡片内嵌套显示子工具调用 / 子文本 / 子推理。
       */
      nested_parts?: StreamingAssistantPart[];
    };

/**
 * 运行时输入队列项：streaming 期间用户排进来的下一条 / 下几条 user message。
 * 当前 turn 跑完后按 FIFO 自动消费——每条都作为独立的 user message 起一个新 turn。
 */
export interface QueuedInput {
  id: string;
  content: string;
  attachments: MessageAttachment[];
  /** 入队时间戳，仅用于显示。 */
  enqueued_at: number;
}

export type MessageAttachment =
  | {
      kind: "text_file";
      name: string;
      media_type: string;
      content: string;
    }
  | {
      kind: "image";
      name: string;
      media_type: string;
      data: string;
    };

/**
 * 推理强度。Anthropic 翻译成 budget_tokens（Extra ≈ 32k），
 * OpenAI 翻译成 reasoning_effort（Extra 钳到 high）。
 */
export type ReasoningEffort = "low" | "medium" | "high" | "extra";

export interface ReasoningConfig {
  /**
   * 是否启用 thinking / reasoning。`undefined` = 沿用模型默认。
   * 对支持 thinking 的模型（claude-opus-4 / gpt-5 等），UI 默认填 true。
   */
  enabled?: boolean;
  /** 推理强度。`undefined` = 默认 extra。 */
  effort?: ReasoningEffort;
  /**
   * Anthropic 1M 上下文开关。仅对 Sonnet 4 / Sonnet 4.5 / Opus 4.x 老型号有意义；
   * 4.6+ 默认 1M，此开关被服务端忽略。`undefined` = 不传 beta header。
   */
  long_context?: boolean;
}

export interface Session {
  id: string;
  title: string;
  provider_id: string;
  model: string;
  system_prompt?: string | null;
  prompt_id?: string | null;
  stream: boolean;
  messages: Message[];
  /** 对话工作目录。null = 用全局默认（通常 ~/）。 */
  workdir?: string | null;
  /**
   * 对话起始时的允许路径覆盖。null = 用全局默认。
   * 一旦本对话发出过 user message，UI 不再允许从这里删除条目（破坏 prompt cache + 已生效行为）。
   * 运行时新增的允许路径请使用 `runtime_allowed_paths` / `pending_runtime_allowed_paths`。
   */
  allowed_paths?: string[] | null;
  /** 对话开始之后追加、已通过 `<workspace-update>` 通知模型的允许路径。UI 只读。 */
  runtime_allowed_paths?: string[];
  /** 对话开始之后追加、还没通知模型的允许路径。下次发消息时随 user message 注入。UI 只读。 */
  pending_runtime_allowed_paths?: string[];
  /** 对话启用的非内置工具。null = 用全局默认。 */
  enabled_tools?: string[] | null;
  /** 对话使用的 skill 目录列表。null = 用全局默认。 */
  skill_dirs?: string[] | null;
  /** 推理 / thinking 配置。undefined = 沿用模型默认。 */
  reasoning?: ReasoningConfig | null;
  /** 整个对话累计 token 用量（含缓存命中 / 写入）；新建对话时为 null。 */
  token_stats?: TokenStats | null;
  /** 创建该 session 的 surface："desktop" / "cli"。老对话可能为 null/undefined。 */
  source?: string | null;
  /** 创建该 session 时绑定的 workspace/project。 */
  project_id?: string | null;
  created_at: number;
  updated_at: number;
  /** 启用的全局规则文件路径列表。null = 继承全局默认。 */
  global_rules?: string[] | null;
  /** 项目规则文件开关状态。null = 自动发现（workdir 下的默认 on）。 */
  rules_files?: RuleFileState[] | null;
  /** TodoWrite 工具维护的当前 todo 列表（架构 §4.4.6）。落盘 jsonl，重启可恢复。 */
  todos?: TodoItem[];
  /** PlanMode ExitPlanMode 落盘后写入"当前 plan"的绝对路径（架构 §4.4.5）。 */
  active_plan?: string | null;
  /** 进入 PlanMode 之前的 RunMode；ExitPlanMode 审批通过后据此切回去（架构 §4.4.5）。 */
  pre_plan_mode?: string | null;
}

/** 规则文件来源（决定默认开关状态） */
export type RuleSource = "global" | "workdir" | "allowed_path";

/** 发现请求返回给前端的轻量信息 */
export interface RuleFileInfo {
  path: string;
  source: RuleSource;
}

/** 前端保存的规则文件开关状态 */
export interface RuleFileState {
  path: string;
  enabled: boolean;
}

/**
 * 架构 §6.1.3：一条已加载的 skill；后端 `list_skills` 返回的形态。
 * 与 [`crate::tools::skill::Skill`] 字段对齐。
 */
export type SkillSource = "global" | "project" | "project_code";

export interface SkillItem {
  /** 目录名——`<skills_dir>/<name>/SKILL.md` 拼路径用，永远存在。 */
  name: string;
  /**
   * frontmatter `name:` 字段，仅当与目录名**不同**时存在。命令面板优先展示它作为
   * 公开名（Claude Code 风格 skill 经常目录名简写、frontmatter 名完整，如
   * `karpathy` → `karpathy-guidelines`）；dispatchSlashCommand 时 alias / name
   * 任一命中即可。
   */
  alias?: string | null;
  description: string;
  path: string;
  source: SkillSource;
  enabled: boolean;
  /**
   * 所属 collection id（架构 §6.1.3）。仅 Global source 的 skill 可能有值；
   * Project / ProjectCode 永远 null/缺失。前端按这个分组展示。
   */
  collection_id?: string | null;
}

/**
 * Skill 集合（架构 §6.1.3）：一次从 GitHub 仓库或本地目录批量导入的 skill 包。
 * SkillsPane 按 id 分组展示，支持"卸载整组"。
 */
export interface SkillCollection {
  id: string;
  label: string;
  source:
    | { kind: "github"; repo_url: string; subpath?: string | null }
    | { kind: "dir"; src_dir: string }
    /**
     * 虚拟集合（架构 §6.1.3.1）：用户手放 / 老导入的 skill 没有 sidecar 记录时，
     * 后端 `list_skill_collections` 为每个孤儿 skill 合成一条 Local 集合
     * （label = skill 目录名，path = 物理目录绝对路径）。前端把它们跟显式集合
     * 一样按 id 分组渲染——id 形如 `local:<skill-name>`。
     */
    | { kind: "local"; path: string };
  imported_at: string;
  skills: string[];
}

export interface AppSettings {
  general: {
    launch_at_login: boolean;
    show_grep_search_path: boolean;
    shell?: string | null;
    log_enabled: boolean;
    edit_backend: "string-replace" | "hashline";
    /** 允许启用自动模式判官的模型 id 列表（架构 §4.4.4）。 */
    automode_models: string[];
  };
  conversation: {
    workdir?: string | null;
    allowed_paths: string[];
    enabled_tools: string[];
    skill_dirs: string[];
    global_rules: string[];
  };
  agents: {
    default_prompt_id?: string | null;
  };
  memory: {
    enabled: boolean;
    models: Array<{ provider_id: string; model: string }>;
  };
}

export type McpTransport = "stdio" | "streamable_http" | "sse";

export interface McpServerConfig {
  name?: string;
  transport?: McpTransport | null;
  command?: string | null;
  args: string[];
  env: Record<string, string>;
  url?: string | null;
  headers: Record<string, string>;
  disabled: boolean;
}

export interface McpConfig {
  mcp_servers: Record<string, McpServerConfig>;
}

export interface McpToolInfo {
  server_name: string;
  name: string;
  runtime_name: string;
  description: string;
  input_schema: unknown;
}

export interface McpToolReport {
  server_name: string;
  transport: McpTransport;
  disabled: boolean;
  tools: McpToolInfo[];
  error?: string | null;
}

export interface SessionMeta {
  id: string;
  title: string;
  provider_id: string;
  model: string;
  created_at: number;
  updated_at: number;
  message_count: number;
  /** YYYY-MM-DD 本地日期，前端分组用 */
  date: string;
  /** 创建该 session 的 surface："desktop" / "cli"。Sidebar 用于显示徽章。 */
  source?: string | null;
  /** 创建该 session 时绑定的 workspace/project。 */
  project_id?: string | null;
  /** 对话工作目录，用于项目列表兜底匹配老会话。 */
  workdir?: string | null;
}

export interface SearchHit extends SessionMeta {
  snippet?: string | null;
  matched_in: "" | "title" | "content";
}

/**
 * 引擎事件——后端通过 Tauri Channel 流式推送给前端。
 *
 * 维护注意：此类型必须与 `apps/desktop/src/engine/mod.rs` 的 `EngineEvent` 枚举
 * 保持字段级同步。新增/修改 EventPayload variant 时需同时更新：
 * 1. protocol::event::EventPayload（crates/protocol/src/event.rs）
 * 2. engine/mod.rs EngineEvent
 * 3. chat.rs agent_event_to_engine_event 翻译函数
 * 4. 本文件 EngineEvent 类型 + useStore.ts applyEventToSlot 处理函数
 */
export type EngineEvent =
  | { type: "text_delta"; text: string; subagent_call_id?: string | null }
  | { type: "text_done"; full_text: string; subagent_call_id?: string | null }
  | { type: "reasoning"; text: string; subagent_call_id?: string | null }
  | {
      type: "tool_call_delta";
      index: number;
      id?: string | null;
      name?: string | null;
      arguments_delta?: string | null;
      subagent_call_id?: string | null;
    }
  | { type: "tool_start"; index: number; id: string; name: string; input: unknown; subagent_call_id?: string | null }
  | {
      type: "tool_done";
      index: number;
      id: string;
      result: string;
      duration_ms: number;
      /** 工具输出超阈值时落盘的工件路径（架构 §4.4.9 / §4.12.11 Phase 2） */
      artifact_path?: string | null;
      subagent_call_id?: string | null;
    }
  | {
      /**
       * 工具执行中的流式输出片段（架构 §4.4.1）。Bash 前台等待期间
       * stdout/stderr 增量按 chunk 推过来；append 到对应工具卡片的实时输出区。
       * `tool_done.result` 是聚合后的最终文本，二者语义不冲突。
       */
      type: "tool_output_delta";
      index: number;
      id: string;
      chunk: string;
      subagent_call_id?: string | null;
    }
  | {
      /** 架构 §4.12：Run 进入挂起态。 */
      type: "run_suspended";
      reason: string;
      resumes_at_ms?: number | null;
      waiting_for_task_ids?: string[];
    }
  | {
      /** 架构 §4.12：Run 从挂起态恢复。 */
      type: "run_resumed";
      cause: string;
    }
  | {
      type: "permission_requested";
      request_id: string;
      tool_name: string;
      input: unknown;
      summary: string;
      risk: "low" | "medium" | "high" | "critical";
      /** 当 kind=PathAccess 时附带越界路径列表 */
      paths?: string[];
      kind?: "tool_call" | "path_access" | "plan" | "continue_long_run";
      /**
       * 命令级记忆指纹（仅 BashTool 当前会带）：完整规范化命令字符串，
       * 例如 `"git status -uno README.md"`。UI 据此切 token 渲染
       * "记住 git status / 记住 git" 两档按钮。等于 `commandSegments[0]`。
       */
      fingerprint?: string | null;
      /**
       * Bash / PowerShell 的所有段 fingerprint（架构 §4.4.2）。
       * compound 命令 `cd /tmp && touch foo` → `["cd /tmp", "touch foo"]`。
       * UI 据此展示每段独立 allow 按钮 + 「整条都允许」按钮。
       */
      command_segments?: string[];
      /**
       * 完整段级状态（含只读 / 已白名单 / 不可记忆 / 待审批），弹窗逐段展示：
       * 已白名单段标 ✓、rm 段红色禁选（架构 §4.4.2.3）。
       */
      segments?: ApprovalSegment[];
      /**
       * 整条命令任何作用域都不可记住（危险复合模式，架构 §4.4.2.2）。
       * 为 true 时弹窗隐藏记忆/作用域区，只留「允许此次 / 拒绝」。
       */
      refuse_remember?: boolean;
      /**
       * Plan 审批专属（架构 §4.4.5），仅 kind="plan" 时填。前端据此在
       * PermissionApprovalPopup 渲染完整 plan markdown + 三按钮
       * （通过 / 编辑后通过 / 重新规划带反馈）。
       */
      plan?: PlanPermissionDto | null;
    }
  | {
      type: "permission_resolved";
      request_id: string;
      decision: "allow_once" | "allow_and_remember" | "deny" | "deny_with_feedback";
    }
  | {
      // AutoMode judge 决策（架构 §4.4.4）。UI 可在消息流里渲染审计气泡。
      type: "permission_auto_judged";
      tool_name: string;
      decision: "allow" | "deny" | "ask";
      reason?: string;
    }
  | {
      // 一次性轻量通知（架构 §4.4.4）。前端渲染成 toast，不进 transcript。
      // dedup_key 非空时按它去重，避免同类提示刷屏。
      type: "notice";
      level: "info" | "warn" | "error";
      message: string;
      dedup_key?: string;
    }
  | {
      // Step 边界（架构 §4.2）。step_kind = model 是模型调用；tool 是工具批次。
      type: "step_started";
      step_kind: "model" | "tool";
      step_index: number;
    }
  | {
      type: "step_finished";
      step_kind: "model" | "tool";
      step_index: number;
    }
  | {
      // 运行模式切换（架构 §10.2）。
      type: "run_mode_changed";
      from: string;
      to: string;
    }
  | {
      /**
       * Turn 边界——一次"模型请求 + 可选 tool_call 批"结束（架构 §3 / §4.2）。
       * 前端据此把当前 streaming bubble 冻结成"已完成 turn 快照"，下一个 Turn 起
       * 一个新的 streaming bubble；保证 streaming 中的插队 user message 总是落在
       * 它真正回应的那个 Turn 之后、下一个 Turn 之前。
       */
      type: "turn_finished";
      stop_reason: string;
    }
  | {
      type: "user_question_requested";
      request_id: string;
      question: string;
      options: { label: string; description: string }[];
      multi?: boolean;
    }
  | {
      type: "user_question_answered";
      request_id: string;
      kind: "selected" | "selected_multi" | "custom" | "cancelled";
      text: string;
    }
  | { type: "error"; message: string }
  | {
      type: "edit_snapshot_created";
      call_id: string;
      snapshot_id: string;
      file_path: string;
      action: "create" | "overwrite" | "modify";
      before_sha: string;
      after_sha: string;
      before_bytes: number;
      after_bytes: number;
    }
  | {
      type: "edit_reverted";
      snapshot_id: string;
      file_path: string;
    }
  | {
      type: "edit_revert_failed";
      snapshot_id: string;
      file_path: string;
      error: string;
    }
  | {
      /** agent_core 后台 task 异步生成的标题已落盘 jsonl。前端用它更新 sidebar + chat header。 */
      type: "session_title_changed";
      session_id: string;
      title: string;
    }
  | {
      /** TodoWrite 工具更新了 todo 列表（架构 §4.4.6）。整列表覆盖。 */
      type: "todo_list_updated";
      todos: TodoItem[];
    }
  | {
      /** ExitPlanMode 落盘了一份 plan markdown，紧随会有 permission_requested(kind=plan)。 */
      type: "plan_ready";
      plan_id: string;
      plan_path: string;
      plan_markdown: string;
      summary: string;
    }
  | {
      /** 用户加了一条 plan 评论。前端追加到当前 plan 的 comment 列表。 */
      type: "plan_comment_added";
      plan_id: string;
      comment: PlanComment;
    }
  | {
      /** 一个 Run 跑完后，后台记忆抽取写入了若干条记忆（架构 §4.14）。
       *  前端在该会话末尾渲染一行「本轮写入 N 条记忆」摘要，可展开看明细。 */
      type: "memory_extracted";
      session_id: string;
      items: MemoryWriteItem[];
    }
  | {
      /** 后台记忆抽取的 fallback 模型链全部失败（架构 §4.14）。前端弹 toast 提示。 */
      type: "memory_extraction_failed";
      session_id: string;
      reason: string;
    };

/** 后台记忆抽取写入的单条记忆（架构 §4.14）。随 memory_extracted 事件下发。 */
export interface MemoryWriteItem {
  /** 记忆 id，形如 `proj/architecture` / `global/lang-pref`。 */
  id: string;
  /** 一句话摘要，展开区每行显示这个。 */
  summary: string;
  /** 作用域标签："project" | "global"，据此显示徽章颜色。 */
  scope: string;
}

/** 一条记忆的 L0（注入初筛 + 设置页清单用，架构 §4.14）。id 前缀 global/ 或 proj/ 即作用域。 */
export interface MemoryL0 {
  id: string;
  summary: string;
  category: string;
}

/** TodoWrite 工具维护的单项 todo。三态 checkbox。 */
export interface TodoItem {
  id: string;
  content: string;
  activeForm: string;
  status: "pending" | "in_progress" | "completed";
}

/** Plan 评论（用户对 plan markdown 加的反馈）。 */
export interface PlanComment {
  id: string;
  plan_id: string;
  /** 锚定 plan markdown 某段（v1 用纯文本，例如 "L12-15" 或选段首尾词）。 */
  anchor: string;
  body: string;
  created_at_ms: number;
  /** 是否已被注入到下一轮 user message。 */
  consumed: boolean;
}

/** Plan 审批 popup 渲染需要的元信息（permission_requested kind=plan 附带）。 */
export interface PlanPermissionDto {
  plan_id: string;
  plan_path: string;
  plan_markdown: string;
  summary: string;
}

/** Plan 列表元数据（list_session_plans 返回）。 */
export interface PlanMeta {
  plan_id: string;
  plan_path: string;
  title: string;
  updated_at_ms: number;
  is_active: boolean;
}

/** 复合命令里单段相对白名单的状态（架构 §4.4.2.3）。 */
export type ApprovalSegmentStatus =
  | "readonly" // 只读：免审批、免记忆（灰显）
  | "whitelisted" // 已命中 allow 规则：本次无需处理（✓）
  | "unmemorable" // rm/dd 等：红色、不可勾选、每次必审
  | "needs_approval"; // 会写且未进白名单：本次可勾选记忆

/** 一段命令 + 它的白名单状态。审批弹窗逐段渲染。 */
export interface ApprovalSegment {
  fingerprint: string;
  status: ApprovalSegmentStatus;
}

/** 一次待审批请求（HITL） */
export interface PendingApproval {
  requestId: string;
  toolName: string;
  input: unknown;
  summary: string;
  risk: "low" | "medium" | "high" | "critical";
  /** PathAccess 类审批专用：越界路径列表 */
  paths?: string[];
  kind: "tool_call" | "path_access" | "plan" | "continue_long_run";
  /** 命令级记忆指纹（BashTool 会带），用于 UI 渲染前缀按钮 */
  fingerprint?: string | null;
  /** Bash 多段命令的所有段 fingerprint，compound 时由 UI 展开每段独立按钮 */
  commandSegments?: string[];
  /** 完整段级状态（只读 / 已白名单 / 不可记忆 / 待审批），逐段展示用 */
  segments?: ApprovalSegment[];
  /** 危险复合模式：任何作用域都不可记住，弹窗隐藏记忆区只留允许此次/拒绝 */
  refuseRemember?: boolean;
  /** Plan 审批专用（架构 §4.4.5）：plan markdown + 元信息，仅 kind="plan" 时填 */
  plan?: PlanPermissionDto | null;
}

/** 用户对审批的回应 */
export type ApprovalDecisionPayload =
  | { kind: "allow_once" }
  | {
      kind: "allow_and_remember";
      /**
       * 命令级记忆前缀。给 `"git status"` 表示之后所有 `git status*` 都直接放行；
       * 给 `"git"` 表示所有 `git *` 直接放行；不传退回工具名级记忆（对 Bash 等会被
       * 后端兜回 AllowOnce）。
       */
      pattern?: string | null;
      /**
       * compound 命令场景的额外段前缀（架构 §4.4.2）。
       * 例：`cd /tmp && touch foo` 弹审批，用户选「整条都允许」 →
       * `pattern = "cd"`, `extraPatterns = ["touch"]`。后端循环写多条规则
       * 让段级判定"全段 allow"一次满足。
       */
      extraPatterns?: string[];
      /**
       * 记忆生效范围（架构 §4.5.3）：
       * - `"session"`（默认）：仅本对话内不再询问，写到 session.jsonl
       * - `"project"`：当前 workdir 所有对话生效，写到 ~/.hebbian/permissions.json
       *   并带 workdir 字段；其他项目不受影响
       * - `"global"`：写到 ~/.hebbian/permissions.json（workdir = null），所有对话生效
       */
      scope?: "session" | "project" | "global";
    }
  | { kind: "deny" }
  | { kind: "deny_with_feedback"; feedback: string };

/** agent 主动向用户提问（ask 工具） */
export interface PendingQuestion {
  requestId: string;
  question: string;
  options: { label: string; description: string }[];
  /** 是否允许多选 */
  multi: boolean;
}

/** 用户对一次提问的回应 */
export type QuestionAnswerPayload =
  | { kind: "selected"; label: string }
  | { kind: "selected_multi"; labels: string[] }
  | { kind: "custom"; text: string }
  | { kind: "cancelled" };

/** 工具元信息（list_tools 命令返回） */
export interface ToolInfo {
  name: string;
  description: string;
  /** 对应 lucide-react 图标名称（kebab-case） */
  icon: string;
}

/** 实时日志行（与后端 observability::LogLine 字段对齐）。 */
export interface LogLine {
  level: "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE";
  target: string;
  message: string;
  /** "HH:MM:SS.mmm" */
  ts: string;
}

/** @deprecated 用 LogLine 替代 */
export type LogEntry = LogLine;

/** 架构 §4.12.9：BackgroundTaskPanel 轮询拉到的后台任务条目。 */
export interface BackgroundTaskInfo {
  task_id: string;
  state: string; // "running" | "exited" | "killed" | "failed"
  command: string;
  cwd: string;
  elapsed_secs: number;
  log_path: string | null;
}

export interface PendingCron {
  run_id: string;
  fire_at_ms: number;
  seconds_remaining: number;
  reason: string;
}

export interface SessionBackgroundReport {
  shells: BackgroundTaskInfo[];
  pending_crons: PendingCron[];
  has_suspended_checkpoint: boolean;
}

/**
 * `read_background_task_output` 的返回值。前端 polling 取增量。
 * `total_bytes` 是下次调用要回传的 cursor；`chunk` 是自上次 cursor 后的新增内容。
 * `state` ∈ "running" / "exited" / "killed" / "failed"。task 已不在注册表时返回
 * 空 chunk + state="exited"，让前端切到 message.tool_call.result 显示。
 */
export interface BackgroundTaskOutputDto {
  total_bytes: number;
  chunk: string;
  state: string;
  bytes_dropped: number;
}

export interface DeviceCodeInfo {
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
}

export interface CodexTokenInfo {
  access_token: string;
  refresh_token?: string | null;
  account_id?: string | null;
  expires_at?: number | null;
}

/** PKCE 浏览器 OAuth 启动结果 */
export interface AuthUrlResult {
  auth_url: string;
  session_id: string;
  state: string;
  redirect_uri: string;
}

/** Claude / Gemini / OpenAI 统一的 token 结果 */
export interface ImportedToken {
  access_token: string;
  refresh_token?: string | null;
  account_id?: string | null;
  expires_at?: number | null;
  client_id?: string | null;
  client_secret?: string | null;
}

// ========== Edits Worktree（架构 §4.13）==========

export type EditAction = "create" | "overwrite" | "modify";

export interface EditEntry {
  snapshot_id: string;
  call_id: string;
  tool: string;
  real_path: string;
  action: EditAction;
  before_sha: string;
  after_sha: string;
  before_bytes: number;
  after_bytes: number;
  ts_ms: number;
  reverted: boolean;
  reverted_at_ms?: number | null;
}

export interface DiffPayload {
  before_text: string;
  after_text: string;
  before_sha: string;
  after_sha: string;
  file_path: string;
  action: string;
}

export interface RevertResult {
  success: boolean;
  error?: string | null;
}

export interface EditsWorktreeStatus {
  enabled: boolean;
  entry_count: number;
}

/** Subagent 定义（对应 Rust SubagentDefinition，架构 §4.4.11.4）。 */
export interface SubagentDefinition {
  name: string;
  description: string;
  tools?: string[] | null;
  model?: string | null;
  max_iterations?: number | null;
  system_prompt: string;
  /** 合并两层 enabled 状态后的结果。 */
  enabled: boolean;
}

/** Subagent 启用 scope（对应 Rust SubagentScope）。 */
export type SubagentScope = "Global" | { Project: string };
