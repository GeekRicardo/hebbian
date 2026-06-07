import React from "react";
import ReactDOM from "react-dom/client";
import { Toaster } from "sonner";
import App from "./App";
import LogViewerApp from "./desktop/ui/components/LogViewerApp";
import { ErrorBoundary } from "./desktop/ui/components/ErrorBoundary";
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
} else {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </React.StrictMode>
  );
}
