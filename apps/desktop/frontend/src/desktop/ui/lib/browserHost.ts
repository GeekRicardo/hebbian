// BrowserHost：内置浏览器承载层适配（架构 §8.5）。
//
// 接口抽象两路实现：Desktop = Tauri 子 webview（本文件 TauriBrowserHost）；
// hebweb = iframe + 代理（P2.5 再加 IframeBrowserHost）。面板 UI 与注释流只依赖
// 本接口，对承载方式无感知。
//
// 多对话多实例：每个对话一个子 webview（后端按 session_id 区分）。所有命令带
// sessionId 定位实例；导航/标题/逃逸等事件回调把 sessionId 透出来，面板按当前
// 对话过滤——别的对话的实例事件不串进来。
import { api } from "@/desktop/bridge/tauri";
import { listen } from "@/desktop/bridge/transport";
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { HebElementSnapshot, StyleDiffEntry } from "@/desktop/ui/lib/annotation";

/** 页面内注释卡片提交的上行载荷（架构 §8.5）。 */
export interface AnnotationSubmit {
  snapshot: HebElementSnapshot;
  comment: string;
  styleDiff: StyleDiffEntry[];
  /** 浏览器绑定的对话 id（提交回这个对话，不串到当前打开的别的对话） */
  boundSessionId?: string | null;
}

/** 修改队列批量提交：多个元素的改动。 */
export interface AnnotationBatchItem {
  snapshot: HebElementSnapshot;
  styleDiff: StyleDiffEntry[];
}

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
  open(sessionId: string, url: string, origin: BrowserOrigin, bounds: BrowserBounds): Promise<string>;
  navigate(sessionId: string, url: string): Promise<string>;
  back(sessionId: string): Promise<void>;
  forward(sessionId: string): Promise<void>;
  reload(sessionId: string): Promise<void>;
  setBounds(sessionId: string, bounds: BrowserBounds): Promise<void>;
  setVisible(sessionId: string, visible: boolean): Promise<void>;
  /** 切对话时隐藏除 keepSession 外的所有实例，避免叠在面板上。 */
  hideOthers(keepSession: string): Promise<void>;
  close(sessionId: string): Promise<void>;
  setPicker(sessionId: string, active: boolean): Promise<void>;
  clearSelection(sessionId: string): Promise<void>;
  popout(sessionId: string): Promise<void>;
  closePopout(): Promise<void>;

  onState(cb: (sessionId: string, s: BrowserStateEvent) => void): Promise<UnlistenFn>;
  onTitle(cb: (sessionId: string, t: BrowserTitleEvent) => void): Promise<UnlistenFn>;
  onPickerOff(cb: (sessionId: string) => void): Promise<UnlistenFn>;
  onAnnotation(cb: (a: AnnotationSubmit) => void): Promise<UnlistenFn>;
  onAnnotationBatch(
    cb: (items: AnnotationBatchItem[], boundSessionId?: string | null) => void
  ): Promise<UnlistenFn>;
  onEscaped(
    cb: (sessionId: string, info: { url: string; reason: string }) => void
  ): Promise<UnlistenFn>;
  onPopout(cb: (sessionId: string, open: boolean) => void): Promise<UnlistenFn>;
}

class TauriBrowserHost implements BrowserHost {
  open(sessionId: string, url: string, origin: BrowserOrigin, bounds: BrowserBounds) {
    return api.browserOpen(sessionId, url, origin, bounds);
  }
  navigate(sessionId: string, url: string) {
    return api.browserNavigate(sessionId, url);
  }
  back(sessionId: string) {
    return api.browserBack(sessionId);
  }
  forward(sessionId: string) {
    return api.browserForward(sessionId);
  }
  reload(sessionId: string) {
    return api.browserReload(sessionId);
  }
  setBounds(sessionId: string, bounds: BrowserBounds) {
    return api.browserSetBounds(sessionId, bounds);
  }
  setVisible(sessionId: string, visible: boolean) {
    return api.browserSetVisible(sessionId, visible);
  }
  hideOthers(keepSession: string) {
    return api.browserHideOthers(keepSession);
  }
  close(sessionId: string) {
    return api.browserClose(sessionId);
  }
  setPicker(sessionId: string, active: boolean) {
    return api.browserPicker(sessionId, active);
  }
  clearSelection(sessionId: string) {
    return api.browserClearSelection(sessionId);
  }
  popout(sessionId: string) {
    return api.browserPopout(sessionId);
  }
  closePopout() {
    return api.browserClosePopout();
  }
  onState(cb: (sessionId: string, s: BrowserStateEvent) => void) {
    return listen<BrowserStateEvent & { session_id: string }>("browser://state", (e) => {
      const { session_id, ...s } = e.payload;
      cb(session_id, s);
    });
  }
  onTitle(cb: (sessionId: string, t: BrowserTitleEvent) => void) {
    return listen<BrowserTitleEvent & { sessionId: string }>("browser://title", (e) =>
      cb(e.payload.sessionId, { url: e.payload.url, title: e.payload.title })
    );
  }
  onPickerOff(cb: (sessionId: string) => void) {
    return listen<{ sessionId: string }>("browser://picker-off", (e) => cb(e.payload.sessionId));
  }
  onAnnotation(cb: (a: AnnotationSubmit) => void) {
    return listen<AnnotationSubmit>("browser://annotation", (e) => cb(e.payload));
  }
  onAnnotationBatch(cb: (items: AnnotationBatchItem[], boundSessionId?: string | null) => void) {
    return listen<{ items: AnnotationBatchItem[]; boundSessionId?: string | null }>(
      "browser://annotation-batch",
      (e) => cb(e.payload.items, e.payload.boundSessionId)
    );
  }
  onEscaped(cb: (sessionId: string, info: { url: string; reason: string }) => void) {
    return listen<{ sessionId: string; url: string; reason: string }>("browser://escaped", (e) =>
      cb(e.payload.sessionId, { url: e.payload.url, reason: e.payload.reason })
    );
  }
  onPopout(cb: (sessionId: string, open: boolean) => void) {
    return listen<{ sessionId: string; open: boolean }>("browser://popout", (e) =>
      cb(e.payload.sessionId, e.payload.open)
    );
  }
}

let host: BrowserHost | null = null;

/** 取当前承载实现（v1 仅 Tauri；hebweb iframe 实现 P2.5 接入）。 */
export function getBrowserHost(): BrowserHost {
  if (!host) host = new TauriBrowserHost();
  return host;
}
