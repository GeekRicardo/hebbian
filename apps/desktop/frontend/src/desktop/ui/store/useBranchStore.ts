import { create } from "zustand";
import { api } from "@/desktop/bridge/tauri";
import type { EngineEvent } from "@/desktop/ui/types";

/**
 * 旁支对话 store（branch / aside session，架构 §8.5）。
 *
 * 旁支是从主对话 fork 出来的临时只读讨论：继承主对话此刻的聊天记录作上下文，只挂
 * Read / Grep，读代码、查实现、解释调用，但改不了任何文件。后端纯内存、不落盘，关掉即消失。
 *
 * 本 store 独立于主对话 useStore：旁支不进会话列表、不持久化，状态全在内存，跟着右侧 tab
 * 生死。多个旁支按 branchId 索引，每个一条独立消息流。前端只渲染 user/assistant 气泡 +
 * 折叠的工具卡片摘要（旁支不需要主对话那套 fork/rollback/审批重交互）。
 */

/** 旁支消息流里的一个工具调用摘要（只读工具，折叠展示）。 */
export type BranchToolCall = {
  id: string;
  name: string;
  /** 入参摘要（首行/截断）。 */
  argsPreview: string;
  status: "running" | "done" | "error";
};

export type BranchMessage =
  | { kind: "user"; id: string; text: string }
  | {
      kind: "assistant";
      id: string;
      text: string;
      reasoning: string;
      tools: BranchToolCall[];
    };

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
  messages: BranchMessage[];
  /** 当前输入框草稿。 */
  input: string;
  /** 正在跑一轮（streaming 中）。 */
  busy: boolean;
  /** streaming 中的实时 assistant（未落定）。 */
  liveText: string;
  liveReasoning: string;
  liveTools: BranchToolCall[];
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
  /** 切换本旁支的供应商 / 模型。 */
  setBranchModel: (branchId: string, providerId: string, model: string) => void;
  /** 发一轮消息（用 branch 自身的 provider/model；为空时后端回落主对话默认）。 */
  sendBranchMessage: (branchId: string, text: string) => Promise<void>;
};

function titleFromText(text: string): string {
  const t = text.trim().replace(/\s+/g, " ");
  return t.length > 16 ? `${t.slice(0, 16)}…` : t || "新旁支";
}

function argsPreview(input: unknown): string {
  try {
    const s = typeof input === "string" ? input : JSON.stringify(input);
    return s.length > 60 ? `${s.slice(0, 60)}…` : s;
  } catch {
    return "";
  }
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
      busy: false,
      liveText: "",
      liveReasoning: "",
      liveTools: [],
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

  setBranchModel(branchId, providerId, model) {
    set((s) => {
      const b = s.branches[branchId];
      if (!b) return s;
      return {
        branches: { ...s.branches, [branchId]: { ...b, providerId, model } },
      };
    });
  },

  async sendBranchMessage(branchId, text) {
    const trimmed = text.trim();
    if (!trimmed) return;
    const cur = get().branches[branchId];
    if (!cur || cur.busy) return;
    const { providerId, model } = cur;

    const userMsg: BranchMessage = {
      kind: "user",
      id: `u-${Date.now()}`,
      text: trimmed,
    };
    const patch = (fn: (b: Branch) => Branch) =>
      set((s) => {
        const b = s.branches[branchId];
        if (!b) return s;
        return { branches: { ...s.branches, [branchId]: fn(b) } };
      });

    patch((b) => ({
      ...b,
      title: b.messages.length === 0 ? titleFromText(trimmed) : b.title,
      messages: [...b.messages, userMsg],
      input: "",
      busy: true,
      liveText: "",
      liveReasoning: "",
      liveTools: [],
      error: null,
    }));

    const onEvent = (e: EngineEvent) => {
      switch (e.type) {
        case "text_delta":
          patch((b) => ({ ...b, liveText: b.liveText + e.text }));
          break;
        case "reasoning":
          patch((b) => ({ ...b, liveReasoning: b.liveReasoning + e.text }));
          break;
        case "tool_start":
          patch((b) => ({
            ...b,
            liveTools: [
              ...b.liveTools,
              {
                id: e.id,
                name: e.name,
                argsPreview: argsPreview(e.input),
                status: "running",
              },
            ],
          }));
          break;
        case "tool_done":
          patch((b) => ({
            ...b,
            liveTools: b.liveTools.map((t) =>
              t.id === e.id
                ? { ...t, status: e.is_error ? "error" : "done" }
                : t
            ),
          }));
          break;
        default:
          break;
      }
    };

    try {
      await api.branchSend(branchId, trimmed, providerId, model, onEvent);
      patch((b) => {
        const assistant: BranchMessage = {
          kind: "assistant",
          id: `a-${Date.now()}`,
          text: b.liveText,
          reasoning: b.liveReasoning,
          tools: b.liveTools,
        };
        return {
          ...b,
          messages: [...b.messages, assistant],
          busy: false,
          liveText: "",
          liveReasoning: "",
          liveTools: [],
        };
      });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      patch((b) => ({
        ...b,
        busy: false,
        error: message,
        // 把已流式出来的 assistant 部分也落定，避免丢失
        messages:
          b.liveText || b.liveTools.length > 0
            ? [
                ...b.messages,
                {
                  kind: "assistant" as const,
                  id: `a-${Date.now()}`,
                  text: b.liveText,
                  reasoning: b.liveReasoning,
                  tools: b.liveTools,
                },
              ]
            : b.messages,
        liveText: "",
        liveReasoning: "",
        liveTools: [],
      }));
    }
  },
}));
