// 超链接点击去向统一收口（架构 §8.5）。
//
// 聊天正文 Markdown 链接、工具卡片里的 URL 等所有 http(s)/file 超链接点击都经这里，
// 按全局设置 general.link_open_target 分流：system = 系统默认浏览器；builtin = 内置
// 浏览器 tab。Tauri webview 里裸 <a> 点击会把整个 app 导航走，必须拦截改走这里。

import { openUrl as openExternalUrl } from "@tauri-apps/plugin-opener";
import { isTauri } from "@/desktop/bridge/transport";
import { useStore } from "@/desktop/ui/store/useStore";

export type LinkOpenTarget = "system" | "builtin";

async function openInSystemBrowser(url: string) {
  try {
    await openExternalUrl(url);
    return;
  } catch (error) {
    if (isTauri()) throw error;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

/**
 * 打开一个超链接。target 省略时读全局设置（默认 system）。
 * 内置档把 url 交给 store 信号通道，由 RightSidebar/BrowserPanel 切 tab + 导航。
 */
export async function openLink(url: string, target?: LinkOpenTarget) {
  const resolved =
    target ?? useStore.getState().appSettings?.general.link_open_target ?? "system";
  if (resolved === "builtin") {
    useStore.getState().requestBrowserNavigate(url);
    return;
  }
  await openInSystemBrowser(url);
}