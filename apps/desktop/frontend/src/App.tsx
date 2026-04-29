import { useEffect } from "react";
import { Toaster } from "sonner";
import { Sidebar } from "@/desktop/ui/components/Sidebar";
import { ChatView } from "@/desktop/ui/components/ChatView";
import { ProvidersDialog } from "@/desktop/ui/components/ProvidersDialog";
import { SessionSettingsDialog } from "@/desktop/ui/components/SessionSettingsDialog";
import { PromptsDialog } from "@/desktop/ui/components/PromptsDialog";
import { AppSettingsDialog } from "@/desktop/ui/components/AppSettingsDialog";
import { useStore } from "@/desktop/ui/store/useStore";

export default function App() {
  const { init, theme } = useStore();

  useEffect(() => {
    init().catch((e) => {
      console.error("init failed:", e);
    });
  }, [init]);

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
