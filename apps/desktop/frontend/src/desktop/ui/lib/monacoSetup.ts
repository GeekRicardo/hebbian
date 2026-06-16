/**
 * Monaco 离线接线。
 *
 * `@monaco-editor/react` 默认走 CDN 拉 Monaco——Tauri 离线环境拉不到。这里把它
 * 切到本地打包的 `monaco-editor`，并用 vite 的 `?worker` 后缀把各语言 worker 打进
 * bundle（不走 CDN、不走 `new Worker(url)` 跨域）。
 *
 * 只需在应用入口 import 一次本模块（副作用）即可生效。
 */
import { loader } from "@monaco-editor/react";
import * as monaco from "monaco-editor";

import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";

self.MonacoEnvironment = {
  getWorker(_workerId: string, label: string) {
    switch (label) {
      case "json":
        return new jsonWorker();
      case "css":
      case "scss":
      case "less":
        return new cssWorker();
      case "html":
      case "handlebars":
      case "razor":
        return new htmlWorker();
      case "typescript":
      case "javascript":
        return new tsWorker();
      default:
        return new editorWorker();
    }
  },
};

loader.config({ monaco });
