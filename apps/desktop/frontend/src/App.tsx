import { useEffect } from "react";
import { Toaster, toast } from "sonner";
import { isTauri, listen } from "@/desktop/bridge/transport";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { startDesktopBridge } from "@/desktop/bridge/desktop-bridge";
import { Sidebar } from "@/desktop/ui/components/Sidebar";
import { ChatView } from "@/desktop/ui/components/ChatView";
import { ProvidersDialog } from "@/desktop/ui/components/ProvidersDialog";
import { SessionSettingsDialog } from "@/desktop/ui/components/SessionSettingsDialog";
import { PromptsDialog } from "@/desktop/ui/components/PromptsDialog";
import { AppSettingsDialog } from "@/desktop/ui/components/AppSettingsDialog";
import { useStore } from "@/desktop/ui/store/useStore";

interface WakeupFiredPayload {
  session_id: string;
  run_id: string;
  wakeup_xml: string;
}

interface EditRevertedPayload {
  session_id: string;
  snapshot_id: string;
  file_path: string;
}

export default function App() {
  const { init, theme } = useStore();

  useEffect(() => {
    init().catch((e) => {
      console.error("init failed:", e);
    });
    // 仅 Tauri 环境启动 hebweb invoke proxy bridge——desktop 当代理把所有 Tauri command
    // 的能力暴露给同一台机器上 hebweb 端的浏览器（Playwright）使用。hebweb 没起就反复
    // 重连，没副作用。
    if (isTauri()) {
      startDesktopBridge();
    }
  }, [init]);

  // 架构 §4.12.6：后端 WakeupScheduler 触发的 wakeup-fired 全局事件 →
  // 前台 session 直接发消息；非前台暂存等用户切换时消费。
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<WakeupFiredPayload>("wakeup-fired", (e) => {
      const { session_id, wakeup_xml } = e.payload;
      const store = useStore.getState();
      const isForeground = store.currentSession?.id === session_id;
      void store.triggerWakeupResume(session_id, wakeup_xml);
      if (!isForeground) {
        const meta = store.sessions.find((s) => s.id === session_id);
        toast.info(`后台任务已完成：${meta?.title ?? session_id}`, {
          description: "切到该对话会自动继续",
        });
      }
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.warn("wakeup-fired listener failed:", err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // edits-worktree 全局事件：其他窗口回退 edit 后同步刷新当前窗口的 editSnapshots。
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<EditRevertedPayload>("edit-reverted", (e) => {
      const store = useStore.getState();
      if (store.currentSession?.id === e.payload.session_id) {
        store.refreshEdits();
      }
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.warn("edit-reverted listener failed:", err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return (
    <div className="h-screen w-screen flex overflow-hidden bg-background text-foreground">
      <Sidebar />
      <ChatView />
      <ProvidersDialog />
      <SessionSettingsDialog />
      <PromptsDialog />
      <AppSettingsDialog />
      <Toaster
        theme={theme}
        position="top-center"
        richColors
        closeButton
        toastOptions={{ className: "text-sm" }}
      />
    </div>
  );
}
