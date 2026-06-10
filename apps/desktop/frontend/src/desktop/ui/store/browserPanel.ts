// 内置浏览器面板的开关状态（架构 §8.5）。
// 独立小 store：RightSidebar 的浏览器图标与 DesktopShell 的面板渲染共享它，
// 不污染千行 useStore。元素选中/样式编辑等瞬时状态留在 BrowserPanel 本地。
import { create } from "zustand";

interface BrowserPanelState {
  open: boolean;
  /** 打开面板时可携带一个待加载 URL（聊天流 chip 点击用）。 */
  pendingUrl: string | null;
  openPanel: (url?: string) => void;
  closePanel: () => void;
  toggle: () => void;
  consumePendingUrl: () => string | null;
}

export const useBrowserPanel = create<BrowserPanelState>((set, get) => ({
  open: false,
  pendingUrl: null,
  openPanel: (url) => set({ open: true, pendingUrl: url ?? null }),
  closePanel: () => set({ open: false }),
  toggle: () => set((s) => ({ open: !s.open })),
  consumePendingUrl: () => {
    const url = get().pendingUrl;
    if (url) set({ pendingUrl: null });
    return url;
  },
}));
