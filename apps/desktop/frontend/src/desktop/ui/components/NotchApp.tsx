import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import NotificationCard, { type NotchPayload } from "./NotificationCard";

export default function NotchApp() {
  const [currentPayload, setCurrentPayload] = useState<NotchPayload | null>(null);

  useEffect(() => {
    document.body.style.background = "transparent";
    document.body.style.margin = "0";
    document.body.style.overflow = "hidden";
    document.documentElement.style.background = "transparent";

    // 初始隐藏，后端 show() 接管
    const win = getCurrentWebviewWindow();
    win.hide();

    const handler = (event: CustomEvent) => {
      const payload = event.detail as NotchPayload;
      setCurrentPayload(payload);
    };

    window.addEventListener("notch-update", handler as EventListener);
    return () => {
      window.removeEventListener("notch-update", handler as EventListener);
    };
  }, []);

  if (!currentPayload) return null;

  return (
    <NotificationCard
      payload={currentPayload}
      onDismiss={() => {
        invoke("notify_dismiss");
      }}
      onOpen={() => {
        invoke("notify_click");
      }}
    />
  );
}
