import { useEffect } from "react";
import { Toaster, toast } from "sonner";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
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

export default function App() {
  const { init, theme } = useStore();

  useEffect(() => {
    init().catch((e) => {
      console.error("init failed:", e);
    });
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
