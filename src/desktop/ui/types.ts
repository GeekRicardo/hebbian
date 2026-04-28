export type ProviderKind = "openai" | "anthropic" | "gemini";

export type AuthMode =
  | "api_key"
  | "oauth_codex"
  | "oauth_claude_code"
  | "oauth_gemini_cli";

export interface Provider {
  id: string;
  name: string;
  kind: ProviderKind;
  auth_mode: AuthMode;
  base_url: string;
  api_key: string;
  refresh_token?: string | null;
  token_expires_at?: number | null;
  account_id?: string | null;
  extra_headers: Record<string, string>;
  models: string[];
  default_model?: string | null;
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
    };

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

export interface Session {
  id: string;
  title: string;
  provider_id: string;
  model: string;
  system_prompt?: string | null;
  prompt_id?: string | null;
  stream: boolean;
  messages: Message[];
  created_at: number;
  updated_at: number;
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
}

export interface SearchHit extends SessionMeta {
  snippet?: string | null;
  matched_in: "" | "title" | "content";
}

/** 引擎事件——后端通过 Tauri Channel 流式推送给前端 */
export type EngineEvent =
  | { type: "text_delta"; text: string }
  | { type: "text_done"; full_text: string }
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
    }
  | {
      type: "permission_resolved";
      request_id: string;
      decision: "allow_once" | "allow_and_remember" | "deny" | "deny_with_feedback";
    }
  | {
      type: "user_question_requested";
      request_id: string;
      question: string;
      options: { label: string; description: string }[];
    }
  | {
      type: "user_question_answered";
      request_id: string;
      kind: "selected" | "custom" | "cancelled";
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
}

/** 用户对审批的回应 */
export type ApprovalDecisionPayload =
  | { kind: "allow_once" }
  | { kind: "allow_and_remember" }
  | { kind: "deny" }
  | { kind: "deny_with_feedback"; feedback: string };

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
