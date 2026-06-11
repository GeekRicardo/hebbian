import { useEffect } from "react";
import { Toaster, toast } from "sonner";
import { listen } from "@/desktop/bridge/transport";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { DesktopShell } from "@/desktop/ui/components/DesktopShell";
import { SessionSettingsDialog } from "@/desktop/ui/components/SessionSettingsDialog";
import { PromptsDialog } from "@/desktop/ui/components/PromptsDialog";
import { AppSettingsDialog } from "@/desktop/ui/components/AppSettingsDialog";
import { useStore } from "@/desktop/ui/store/useStore";
import { getBrowserHost } from "@/desktop/ui/lib/browserHost";
import { buildAnnotationMessage } from "@/desktop/ui/lib/annotation";

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
  run_id: string;
}

export default function App() {
  const theme = useStore((s) => s.theme);

  useEffect(() => {
    useStore.getState().init().catch((e) => {
      console.error("init failed:", e);
    });
  }, []);  // empty deps - init only once on mount

  // 全局异常捕获：事件回调 + 异步代码中的未处理异常 → toast 报错
  useEffect(() => {
    const onError = (e: ErrorEvent) => {
      const msg = e.message || String(e.error);
      console.error("[global error]", e.error);
      toast.error(`未捕获错误: ${msg}`, { duration: 12000 });
    };
    const onRejection = (e: PromiseRejectionEvent) => {
      const msg =
        e.reason instanceof Error ? e.reason.message : String(e.reason);
      console.error("[unhandled rejection]", e.reason);
      toast.error(`未处理的异步错误: ${msg}`, { duration: 12000 });
    };
    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onRejection);
    return () => {
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onRejection);
    };
  }, []);

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

  // 内置浏览器页面内注释提交（架构 §8.5）：embedded 子 webview 或 popout 独立窗口里
  // 的注释卡片提交 → 主进程 emit browser://annotation → 这里组装成 user message 发进
  // 当前对话。放 App 级（常驻）而非 BrowserPanel——popout 注释时浏览器 tab 可能没在前台。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    getBrowserHost()
      .onAnnotation((a) => {
        const store = useStore.getState();
        if (!store.currentSession) {
          toast.error("先打开一个对话，注释才有地方发");
          return;
        }
        const { content, attachments } = buildAnnotationMessage({
          snapshot: a.snapshot,
          comment: a.comment,
          styleDiff: a.styleDiff,
        });
        void store.sendUserMessage(content, attachments);
        toast.success("页面标注已发送到对话");
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.warn("annotation listener failed:", err));
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
      <DesktopShell />
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
