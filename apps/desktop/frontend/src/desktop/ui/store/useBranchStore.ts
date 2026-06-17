import { create } from "zustand";
import { api } from "@/desktop/bridge/tauri";
import type {
  EngineEvent,
  Message,
  MessageAttachment,
  MessagePart,
  StreamingAssistantPart,
} from "@/desktop/ui/types";
import {
  applyReasoningDelta,
  applyTextDelta,
  applyToolCallDelta,
  applyToolDone,
  applyToolOutputDelta,
  applyToolStart,
  finalizeOpenReasoning,
} from "@/desktop/ui/store/streamingParts";

/**
 * 旁支对话 store（branch / aside session，架构 §8.5）。
 *
 * 旁支是从主对话 fork 出来的临时只读讨论：继承主对话此刻的聊天记录作上下文，挂只读工具
 * （Read / Grep / WebSearch / Fetch / ReadMemory + MCP），读代码、查实现、查资料，但改不了
 * 任何文件、跑不了命令。后端纯内存、不落盘，关掉即消失。
 *
 * 本 store 独立于主对话 useStore：旁支不进会话列表、不持久化，状态全在内存，跟着右侧 tab
 * 生死。多个旁支按 branchId 索引，每个一条独立消息流。**渲染与主对话同源**——messages 直接
 * 持 storage `Message`、流式态用 `StreamingAssistantPart[]`，前端复用 `MessageBubble` 渲染，
 * 跟主对话完全一致（reasoning 折叠、工具卡片展开 / 实时输出、附件等都自动有）。
 */

export type Branch = {
  branchId: string;
  boundSessionId: string;
  /** 标题：取首条用户输入的前若干字，空时占位。 */
  title: string;
  inheritedCount: number;
  createdAt: number;
  /** 本旁支用的供应商 / 模型（默认继承主对话，可在输入框旁切换；不影响主对话）。 */
  providerId: string | null;
  model: string | null;
  /** 已落定的消息流（storage Message，与主对话同构，喂给 MessageBubble 渲染）。 */
  messages: Message[];
  /** 当前输入框草稿。 */
  input: string;
  /** 当前待发附件（贴图 / 拖入，发送后清空）。 */
  attachments: MessageAttachment[];
  /** 正在跑一轮（streaming 中）。 */
  busy: boolean;
  /** streaming 中的实时 assistant 正文（喂 MessageBubble 的 message.content）。 */
  liveText: string;
  /** streaming 中的实时 assistant 片段（文本 / 推理 / 工具卡片，喂 MessageBubble 的 streamingParts）。 */
  liveParts: StreamingAssistantPart[];
  error: string | null;
};

type BranchState = {
  /** branchId → Branch。 */
  branches: Record<string, Branch>;
  /** 当前激活的 branchId（右侧子 tab 选中项）。 */
  activeBranchId: string | null;

  /** 列出某主对话下的旁支（按创建时间）。 */
  branchesForSession: (sessionId: string) => Branch[];

  /** 新建一条旁支（fork 主对话当前历史），成功后激活它。默认模型继承主对话。 */
  createBranch: (
    sessionId: string,
    defaultProviderId: string | null,
    defaultModel: string | null
  ) => Promise<void>;
  /** 选中子 tab。 */
  selectBranch: (branchId: string) => void;
  /** 关闭一条旁支（丢弃内存历史）。 */
  discardBranch: (branchId: string) => Promise<void>;
  /** 设置输入框草稿。 */
  setBranchInput: (branchId: string, value: string) => void;
  /** 设置当前待发附件。 */
  setBranchAttachments: (
    branchId: string,
    attachments: MessageAttachment[]
  ) => void;
  /** 切换本旁支的供应商 / 模型。 */
  setBranchModel: (branchId: string, providerId: string, model: string) => void;
  /** 发一轮消息（正文 + 附件；用 branch 自身的 provider/model，为空时后端回落主对话默认）。 */
  sendBranchMessage: (
    branchId: string,
    text: string,
    attachments: MessageAttachment[]
  ) => Promise<void>;
  /** 停止正在跑的一轮（置位后端 cancel flag）。 */
  cancelBranch: (branchId: string) => Promise<void>;
};

function titleFromText(text: string): string {
  const t = text.trim().replace(/\s+/g, " ");
  return t.length > 16 ? `${t.slice(0, 16)}…` : t || "新旁支";
}

/**
 * 失败 / 中断兜底：把已流式出来的 liveParts 落定成一条 assistant `Message`，
 * 避免用户已经看到的内容丢失（正常成功路径直接用后端返回的 Message，不走这里）。
 * 返回 null 表示这一轮还没产出任何东西，无需落定。
 */
function liveToMessage(
  liveText: string,
  liveParts: StreamingAssistantPart[]
): Message | null {
  const finalized = finalizeOpenReasoning(liveParts);
  const parts: MessagePart[] = finalized.flatMap((p): MessagePart[] => {
    if (p.type === "text") return p.text ? [{ type: "text", text: p.text }] : [];
    if (p.type === "reasoning")
      return [{ type: "reasoning", text: p.text, duration_ms: p.duration_ms ?? null }];
    // tool_call：丢掉流式专属字段（index/status/live_output），转成持久化形态。
    return [
      {
        type: "tool_call",
        id: p.id ?? "",
        name: p.name ?? "",
        input: p.input,
        arguments: p.arguments,
        result: p.result ?? null,
        duration_ms: p.duration_ms ?? null,
        is_error: p.is_error ?? false,
        artifact_path: p.artifact_path ?? null,
      },
    ];
  });
  if (parts.length === 0 && !liveText) return null;
  return {
    id: `a-${Date.now()}`,
    role: "assistant",
    content: liveText,
    parts,
    created_at: Date.now(),
  };
}

export const useBranchStore = create<BranchState>((set, get) => ({
  branches: {},
  activeBranchId: null,

  branchesForSession(sessionId) {
    return Object.values(get().branches)
      .filter((b) => b.boundSessionId === sessionId)
      .sort((a, b) => a.createdAt - b.createdAt);
  },

  async createBranch(sessionId, defaultProviderId, defaultModel) {
    const info = await api.branchCreate(sessionId, null);
    const branch: Branch = {
      branchId: info.branch_id,
      boundSessionId: info.bound_session_id,
      title: "新旁支",
      inheritedCount: info.inherited_count,
      createdAt: Date.now(),
      providerId: defaultProviderId,
      model: defaultModel,
      messages: [],
      input: "",
      attachments: [],
      busy: false,
      liveText: "",
      liveParts: [],
      error: null,
    };
    set((s) => ({
      branches: { ...s.branches, [branch.branchId]: branch },
      activeBranchId: branch.branchId,
    }));
  },

  selectBranch(branchId) {
    set({ activeBranchId: branchId });
  },

  async discardBranch(branchId) {
    await api.branchDiscard(branchId).catch(() => {});
    set((s) => {
      const branches = { ...s.branches };
      const discarded = branches[branchId];
      delete branches[branchId];
      let active = s.activeBranchId;
      if (active === branchId) {
        // 切到同主对话下剩余的第一条旁支；没有则置空
        const siblings = Object.values(branches)
          .filter((b) => b.boundSessionId === discarded?.boundSessionId)
          .sort((a, b) => a.createdAt - b.createdAt);
        active = siblings[0]?.branchId ?? null;
      }
      return { branches, activeBranchId: active };
    });
  },

  setBranchInput(branchId, value) {
    set((s) => {
      const b = s.branches[branchId];
      if (!b) return s;
      return { branches: { ...s.branches, [branchId]: { ...b, input: value } } };
    });
  },

  setBranchAttachments(branchId, attachments) {
    set((s) => {
      const b = s.branches[branchId];
      if (!b) return s;
      return {
        branches: { ...s.branches, [branchId]: { ...b, attachments } },
      };
    });
  },

  setBranchModel(branchId, providerId, model) {
    set((s) => {
      const b = s.branches[branchId];
      if (!b) return s;
      return {
        branches: { ...s.branches, [branchId]: { ...b, providerId, model } },
      };
    });
  },

  async sendBranchMessage(branchId, text, attachments) {
    const trimmed = text.trim();
    if (!trimmed && attachments.length === 0) return;
    const cur = get().branches[branchId];
    if (!cur || cur.busy) return;
    const { providerId, model } = cur;

    const now = Date.now();
    const userMsg: Message = {
      id: `u-${now}`,
      role: "user",
      content: trimmed,
      attachments,
      created_at: now,
    };
    const patch = (fn: (b: Branch) => Branch) =>
      set((s) => {
        const b = s.branches[branchId];
        if (!b) return s;
        return { branches: { ...s.branches, [branchId]: fn(b) } };
      });

    patch((b) => ({
      ...b,
      title:
        b.messages.length === 0
          ? titleFromText(trimmed || "（附件）")
          : b.title,
      messages: [...b.messages, userMsg],
      input: "",
      attachments: [],
      busy: true,
      liveText: "",
      liveParts: [],
      error: null,
    }));

    // 事件流累积进 liveText / liveParts，与主对话同一套折叠逻辑（MessageBubble 直接渲染）。
    const onEvent = (e: EngineEvent) => {
      switch (e.type) {
        case "text_delta":
          patch((b) => ({
            ...b,
            liveText: b.liveText + e.text,
            liveParts: applyTextDelta(b.liveParts, e.text),
          }));
          break;
        case "text_done":
          patch((b) => ({
            ...b,
            liveText: e.full_text || b.liveText,
          }));
          break;
        case "reasoning":
          patch((b) => ({
            ...b,
            liveParts: applyReasoningDelta(b.liveParts, e.text),
          }));
          break;
        case "tool_call_delta":
          patch((b) => ({ ...b, liveParts: applyToolCallDelta(b.liveParts, e) }));
          break;
        case "tool_start":
          patch((b) => ({ ...b, liveParts: applyToolStart(b.liveParts, e) }));
          break;
        case "tool_done":
          patch((b) => ({ ...b, liveParts: applyToolDone(b.liveParts, e) }));
          break;
        case "tool_output_delta":
          patch((b) => ({ ...b, liveParts: applyToolOutputDelta(b.liveParts, e) }));
          break;
        default:
          break;
      }
    };

    try {
      // 后端返回本轮落定的 assistant Message（含完整 parts），用它入 messages 最准确。
      const assistant = await api.branchSend(
        branchId,
        trimmed || "（见附件）",
        attachments,
        providerId,
        model,
        onEvent
      );
      patch((b) => ({
        ...b,
        messages: [...b.messages, assistant],
        busy: false,
        liveText: "",
        liveParts: [],
      }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // 用户主动停止（branch_cancel → run_aside 返回「请求已中断」）不算错误，不飘红。
      const interrupted = message.includes("请求已中断");
      patch((b) => {
        // 后端没返回 message（失败 / 中断）→ 用已流式出来的 liveParts 落定成一条 assistant，
        // 避免已经显示给用户的内容丢失。
        const salvaged = liveToMessage(b.liveText, b.liveParts);
        return {
          ...b,
          busy: false,
          error: interrupted ? null : message,
          messages: salvaged ? [...b.messages, salvaged] : b.messages,
          liveText: "",
          liveParts: [],
        };
      });
    }
  },

  async cancelBranch(branchId) {
    const cur = get().branches[branchId];
    if (!cur || !cur.busy) return;
    // 只发取消信号；run_aside 落定后 branchSend 的 catch 会统一清 busy / 落定已流式内容。
    await api.branchCancel(branchId).catch(() => {});
  },
}));
