import { useEffect } from "react";
import { Toaster, toast } from "sonner";
import { listen } from "@/desktop/bridge/transport";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Sidebar } from "@/desktop/ui/components/Sidebar";
import { ChatView } from "@/desktop/ui/components/ChatView";
import { RightSidebar } from "@/desktop/ui/components/RightSidebar";
import { SessionSettingsDialog } from "@/desktop/ui/components/SessionSettingsDialog";
import { PromptsDialog } from "@/desktop/ui/components/PromptsDialog";
import { AppSettingsDialog } from "@/desktop/ui/components/AppSettingsDialog";
import { useStore } from "@/desktop/ui/store/useStore";

interface WakeupFiredPayload {
  session_id: string;
  run_id: string;
  wakeup_xml: string;
  /** 架构 §4.12.5 修订：后端 WakeupEvent::message_meta() 投影出来的结构化 meta。
   *  前端透传给 inject/send 命令，落盘 user message 时挂上 → view 据此渲染系统通知条。 */
  meta: import("@/desktop/ui/types").MessageMeta;
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
  }, [init]);

  // 架构 §4.12.6：后端 WakeupScheduler 触发的 wakeup-fired 全局事件 →
  // 前台 session 直接发消息；非前台暂存等用户切换时消费。
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<WakeupFiredPayload>("wakeup-fired", (e) => {
      const { session_id, wakeup_xml, meta } = e.payload;
      const store = useStore.getState();
      const isForeground = store.currentSession?.id === session_id;
      void store.triggerWakeupResume(session_id, wakeup_xml, meta);
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
    <div className="h-screen w-screen flex overflow-hidden bg-muted/40 text-foreground">
      <Sidebar />
      <ChatView />
      <RightSidebar />
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
