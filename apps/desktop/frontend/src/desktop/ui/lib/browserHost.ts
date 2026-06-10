// BrowserHost：内置浏览器承载层适配（架构 §8.5）。
//
// 接口抽象两路实现：Desktop = Tauri 子 webview（本文件 TauriBrowserHost）；
// hebweb = iframe + 代理（P2.5 再加 IframeBrowserHost）。面板 UI 与注释流只依赖
// 本接口，对承载方式无感知。
import { api } from "@/desktop/bridge/tauri";
import { listen } from "@/desktop/bridge/transport";
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { HebElementSnapshot, StyleDiffEntry } from "@/desktop/ui/lib/annotation";

export interface BrowserStateEvent {
  url: string;
  can_go_back: boolean;
  can_go_forward: boolean;
  loading: boolean;
}

export interface BrowserTitleEvent {
  url: string;
  title: string;
}

export interface BrowserBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type BrowserOrigin = "auto" | "user";

export interface BrowserHost {
  open(url: string, origin: BrowserOrigin, bounds: BrowserBounds): Promise<string>;
  navigate(url: string): Promise<string>;
  back(): Promise<void>;
  forward(): Promise<void>;
  reload(): Promise<void>;
  setBounds(bounds: BrowserBounds): Promise<void>;
  setVisible(visible: boolean): Promise<void>;
  close(): Promise<void>;
  setPicker(active: boolean): Promise<void>;
  applyStyle(prop: string, value: string): Promise<void>;
  revertStyles(): Promise<void>;
  requestStyleDiff(): Promise<void>;
  clearSelection(): Promise<void>;

  onState(cb: (s: BrowserStateEvent) => void): Promise<UnlistenFn>;
  onTitle(cb: (t: BrowserTitleEvent) => void): Promise<UnlistenFn>;
  onElement(cb: (snap: HebElementSnapshot) => void): Promise<UnlistenFn>;
  onPickerOff(cb: () => void): Promise<UnlistenFn>;
  onStyleDiff(cb: (diff: StyleDiffEntry[]) => void): Promise<UnlistenFn>;
  onEscaped(cb: (info: { url: string; reason: string }) => void): Promise<UnlistenFn>;
}

class TauriBrowserHost implements BrowserHost {
  open(url: string, origin: BrowserOrigin, bounds: BrowserBounds) {
    return api.browserOpen(url, origin, bounds);
  }
  navigate(url: string) {
    return api.browserNavigate(url);
  }
  back() {
    return api.browserBack();
  }
  forward() {
    return api.browserForward();
  }
  reload() {
    return api.browserReload();
  }
  setBounds(bounds: BrowserBounds) {
    return api.browserSetBounds(bounds);
  }
  setVisible(visible: boolean) {
    return api.browserSetVisible(visible);
  }
  close() {
    return api.browserClose();
  }
  setPicker(active: boolean) {
    return api.browserPicker(active);
  }
  applyStyle(prop: string, value: string) {
    return api.browserStyleApply(prop, value);
  }
  revertStyles() {
    return api.browserStyleRevert();
  }
  requestStyleDiff() {
    return api.browserStyleTakeDiff();
  }
  clearSelection() {
    return api.browserClearSelection();
  }
  onState(cb: (s: BrowserStateEvent) => void) {
    return listen<BrowserStateEvent>("browser://state", (e) => cb(e.payload));
  }
  onTitle(cb: (t: BrowserTitleEvent) => void) {
    return listen<BrowserTitleEvent>("browser://title", (e) => cb(e.payload));
  }
  onElement(cb: (snap: HebElementSnapshot) => void) {
    return listen<{ snapshot: HebElementSnapshot }>("browser://element", (e) =>
      cb(e.payload.snapshot)
    );
  }
  onPickerOff(cb: () => void) {
    return listen<unknown>("browser://picker-off", () => cb());
  }
  onStyleDiff(cb: (diff: StyleDiffEntry[]) => void) {
    return listen<{ diff: StyleDiffEntry[] }>("browser://style-diff", (e) => cb(e.payload.diff));
  }
  onEscaped(cb: (info: { url: string; reason: string }) => void) {
    return listen<{ url: string; reason: string }>("browser://escaped", (e) => cb(e.payload));
  }
}

let host: BrowserHost | null = null;

/** 取当前承载实现（v1 仅 Tauri；hebweb iframe 实现 P2.5 接入）。 */
export function getBrowserHost(): BrowserHost {
  if (!host) host = new TauriBrowserHost();
  return host;
}
