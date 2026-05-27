import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import NotchApp from "./desktop/ui/components/NotchApp";
import LogViewerApp from "./desktop/ui/components/LogViewerApp";
import "./index.css";

const params = new URLSearchParams(window.location.search);
if (params.has("notch")) {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <NotchApp />
    </React.StrictMode>
  );
} else if (params.has("log-viewer")) {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <LogViewerApp />
    </React.StrictMode>
  );
} else {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}
