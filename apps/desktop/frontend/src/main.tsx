import React from "react";
import ReactDOM from "react-dom/client";
import { Toaster } from "sonner";
import App from "./App";
import LogViewerApp from "./desktop/ui/components/LogViewerApp";
import { TerminalSurface } from "./desktop/ui/components/TerminalSurface";
import { ErrorBoundary } from "./desktop/ui/components/ErrorBoundary";
import { ToolRenderPreviewApp } from "./desktop/ui/components/ToolRenderPreviewApp";
import "@vscode/codicons/dist/codicon.css";
import "./index.css";

const params = new URLSearchParams(window.location.search);
if (params.has("log-viewer")) {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary>
        <LogViewerApp />
      </ErrorBoundary>
      <Toaster position="top-center" richColors closeButton toastOptions={{ className: "text-sm" }} />
    </React.StrictMode>
  );
} else if (params.has("terminal-popout")) {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary>
        <div className="h-screen w-screen">
          <TerminalSurface variant="popout" />
        </div>
      </ErrorBoundary>
    </React.StrictMode>
  );
} else if (params.has("tool-preview")) {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary>
        <ToolRenderPreviewApp />
      </ErrorBoundary>
      <Toaster position="top-center" richColors closeButton toastOptions={{ className: "text-sm" }} />
    </React.StrictMode>
  );
} else {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </React.StrictMode>
  );
}
