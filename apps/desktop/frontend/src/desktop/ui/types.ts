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
   * 对话起始时的允许目录覆盖。null = 用全局默认。
   * 一旦本对话发出过 user message，UI 不再允许从这里删除条目（破坏 prompt cache + 已生效行为）。
   * 运行时新增的允许目录请使用 `runtime_allowed_dirs` / `pending_runtime_allowed_dirs`。
   */
  allowed_dirs?: string[] | null;
  /** 对话开始之后追加、已通过 `<workspace-update>` 通知模型的允许目录。UI 只读。 */
  runtime_allowed_dirs?: string[];
  /** 对话开始之后追加、还没通知模型的允许目录。下次发消息时随 user message 注入。UI 只读。 */
  pending_runtime_allowed_dirs?: string[];
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
  created_at: number;
  updated_at: number;
}

export interface AppSettings {
  general: {
    launch_at_login: boolean;
  };
  conversation: {
    workdir?: string | null;
    allowed_dirs: string[];
    enabled_tools: string[];
    skill_dirs: string[];
  };
  agents: {
    default_prompt_id?: string | null;
  };
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
}

export interface SearchHit extends SessionMeta {
  snippet?: string | null;
  matched_in: "" | "title" | "content";
}

/** 引擎事件——后端通过 Tauri Channel 流式推送给前端 */
export type EngineEvent =
  | { type: "text_delta"; text: string }
  | { type: "text_done"; full_text: string }
  | { type: "reasoning"; text: string }
  | {
      type: "tool_call_delta";
      index: number;
      id?: string | null;
      name?: string | null;
      arguments_delta?: string | null;
    }
  | { type: "tool_start"; index: number; id: string; name: string; input: unknown }
  | {
      type: "tool_done";
      index: number;
      id: string;
      result: string;
      duration_ms: number;
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
       * "记住 git status / 记住 git" 两档按钮。
       */
      fingerprint?: string | null;
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
  | { type: "error"; message: string };

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
       * 记忆生效范围：
       * - `"session"`（默认）：仅本对话内不再询问，写到 session.jsonl
       * - `"global"`：写到 ~/.hebbian/permissions.json，所有对话生效
       */
      scope?: "session" | "global";
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
