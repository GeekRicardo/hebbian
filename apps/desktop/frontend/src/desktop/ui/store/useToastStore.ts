import { create } from "zustand";

/**
 * 输入框上方「模型异常退出」toast 区的独立状态（架构 §7.3）。
 *
 * 单独成 store 而不混进巨型 useStore：这些 toast 是纯 UI 瞬时态，不进 transcript、
 * 不落盘，与会话槽生命周期解耦——独立一份最干净，避免污染会话状态机。
 */
export interface ModelToast {
  /** 唯一 id；相同 dedupKey 的后来者覆盖前者，避免同类提示刷屏。 */
  id: string;
  level: "info" | "warn" | "error";
  message: string;
}

interface ToastState {
  toasts: ModelToast[];
  /** 推一条 toast。`dedupKey` 命中已有则原地替换（不新增）。 */
  push: (t: { level: ModelToast["level"]; message: string; dedupKey?: string }) => void;
  dismiss: (id: string) => void;
}

let seq = 0;

export const useToastStore = create<ToastState>((set) => ({
  toasts: [],
  push: ({ level, message, dedupKey }) => {
    const id = dedupKey ?? `toast-${++seq}`;
    set((s) => {
      const without = s.toasts.filter((t) => t.id !== id);
      // 新消息追加到末尾——ToastRegion 用 column 顺序渲染，新的在下、旧的往上挤。
      return { toasts: [...without, { id, level, message }] };
    });
  },
  dismiss: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));
