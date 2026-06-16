/**
 * 统一传输抽象 —— Tauri 与 WebSocket 二选一。
 *
 * runtime detect：`window.__TAURI_INTERNALS__` 存在走 Tauri，否则走 hebweb
 * (`/ws` WebSocket）。组件不需要感知 surface 差异，照常 `invoke(cmd, args)` /
 * `listen(name, handler)` / `new Channel<T>()`。
 *
 * v1 hebweb 仅镜像了核心 7 个命令；其余 cmd 在浏览器模式下会被 server reject
 * 为 "not implemented in hebweb v1"，由调用方决定是否优雅降级。
 */

import {
  invoke as tauriInvoke,
  Channel as TauriChannel,
} from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";

// ─── runtime detect ───────────────────────────────────────────────────────

const IS_TAURI =
  typeof window !== "undefined" &&
  ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);

export const transportMode: "tauri" | "web" = IS_TAURI ? "tauri" : "web";

/** Tauri SDK 的 `isTauri` 同义函数，让旧代码改 import 路径即可使用 */
export function isTauri(): boolean {
  return IS_TAURI;
}

/**
 * 本页面是否跑在内置浏览器子 webview 内（自举 / popout）。
 *
 * 后端 browser 模块给子 webview 注入 `window.__HEB_EMBEDDED__=true`。内置浏览器
 * 打开 hebbian 自己的前端（自举）时，被嵌前端据此隐藏浏览器/终端这类宿主专属功能——
 * 否则会无意义套娃，且 BrowserPanel mount 即调 browser_hide_others 触发 ACL 报错。
 */
const IS_EMBEDDED =
  typeof window !== "undefined" &&
  (window as { __HEB_EMBEDDED__?: boolean }).__HEB_EMBEDDED__ === true;

export function isEmbeddedPreview(): boolean {
  return IS_EMBEDDED;
}

// ─── WS client (web mode only) ────────────────────────────────────────────

type PendingInvoke = {
  resolve: (v: unknown) => void;
  reject: (e: unknown) => void;
};

type EventHandler = (event: { payload: unknown }) => void;

class WsClient {
  private url: string;
  private ws: WebSocket | null = null;
  private connecting: Promise<void> | null = null;
  private pending = new Map<string, PendingInvoke>();
  // 每个 event-name 对应一组 handler（hebweb 当前只发 "engine-event"）
  private handlers = new Map<string, Set<EventHandler>>();
  /// 当前订阅的 session_id；同一连接同时只订阅一个 session
  private subscribedSession: string | null = null;

  constructor() {
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    this.url = `${proto}://${window.location.host}/ws`;
  }

  private async ensureConnected(): Promise<void> {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) return;
    if (this.connecting) return this.connecting;

    this.connecting = new Promise((resolve, reject) => {
      const ws = new WebSocket(this.url);
      this.ws = ws;
      ws.addEventListener("open", () => {
        this.connecting = null;
        resolve();
      });
      ws.addEventListener("message", (e) => this.onMessage(e.data));
      ws.addEventListener("error", (e) => {
        this.connecting = null;
        reject(e);
      });
      ws.addEventListener("close", () => {
        this.ws = null;
        this.connecting = null;
        // 把所有 pending 标记失败
        for (const [, p] of this.pending) {
          p.reject(new Error("ws connection closed"));
        }
        this.pending.clear();
        this.subscribedSession = null;
      });
    });
    return this.connecting;
  }

  private onMessage(raw: unknown) {
    if (typeof raw !== "string") return;
    let msg: { type: string; [k: string]: unknown };
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }
    switch (msg.type) {
      case "hello":
      case "subscribed":
        return;
      case "invoke_response": {
        const id = msg.id as string;
        const p = this.pending.get(id);
        if (!p) return;
        this.pending.delete(id);
        if (msg.ok) p.resolve(msg.data);
        else p.reject(new Error((msg.error as string) ?? "invoke failed"));
        return;
      }
      case "event": {
        const name = msg.name as string;
        const payload = msg.payload;
        const set = this.handlers.get(name);
        if (set) {
          for (const h of set) h({ payload });
        }
        return;
      }
    }
  }

  async subscribe(sessionId: string): Promise<void> {
    await this.ensureConnected();
    if (this.subscribedSession === sessionId) return;
    this.subscribedSession = sessionId;
    this.ws?.send(JSON.stringify({ type: "subscribe", session_id: sessionId }));
  }

  async invoke<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
    await this.ensureConnected();
    // 约定：tauri.ts 在调 send_message / approve_permission / answer_question / ...
    // 时通过 args.sessionId / args.id / args.session_id 传 session 信息。
    // 这里 best-effort 提取，让 server 能按 session 路由。
    const sessionId = pickSessionId(args);
    if (sessionId && this.subscribedSession !== sessionId) {
      await this.subscribe(sessionId);
    }
    const id = randomId();
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, {
        resolve: (v) => resolve(v as T),
        reject,
      });
      this.ws?.send(
        JSON.stringify({
          type: "invoke",
          id,
          cmd,
          args,
          session_id: sessionId ?? null,
        }),
      );
    });
  }

  listen(name: string, handler: EventHandler): UnlistenFn {
    let set = this.handlers.get(name);
    if (!set) {
      set = new Set();
      this.handlers.set(name, set);
    }
    set.add(handler);
    return () => set!.delete(handler);
  }
}

const wsClient = IS_TAURI ? null : new WsClient();

function pickSessionId(args: Record<string, unknown>): string | null {
  const candidates = ["sessionId", "session_id", "id"];
  for (const k of candidates) {
    const v = args[k];
    if (typeof v === "string" && v.length > 0) return v;
  }
  return null;
}

function randomId(): string {
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
}

// ─── 统一 invoke / listen / Channel ────────────────────────────────────────

export async function invoke<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  const finalArgs = args ?? {};
  if (IS_TAURI) return tauriInvoke<T>(cmd, finalArgs);
  return wsClient!.invoke<T>(cmd, finalArgs);
}

export async function listen<T>(
  name: string,
  handler: (event: { payload: T }) => void,
): Promise<UnlistenFn> {
  if (IS_TAURI) return tauriListen<T>(name, handler);
  return wsClient!.listen(name, handler as EventHandler);
}

/**
 * Tauri 的 `new Channel<EngineEvent>()` 等价物。
 *
 * - Tauri 模式：内部委托给真 Channel，行为不变
 * - Web 模式：首次设置 `onmessage` 时自动 listen `engine-event`，事件来自 hebweb
 *   的 WS 广播；调用方把本对象作为 invoke 参数传入时 server 会忽略它（hebweb 的
 *   send_message 不需要 channel，事件走广播）
 */
export class Channel<T> {
  private _onmessage: ((p: T) => void) | null = null;
  private _tauri: TauriChannel<T> | null = null;
  private _unlisten: UnlistenFn | null = null;

  constructor() {
    if (IS_TAURI) {
      this._tauri = new TauriChannel<T>();
    }
  }

  get onmessage(): ((p: T) => void) | null {
    if (this._tauri) return this._tauri.onmessage as ((p: T) => void) | null;
    return this._onmessage;
  }

  set onmessage(handler: ((p: T) => void) | null) {
    if (this._tauri) {
      this._tauri.onmessage = handler ?? (() => {});
      return;
    }
    this._onmessage = handler;
    if (handler && !this._unlisten && wsClient) {
      this._unlisten = wsClient.listen("engine-event", (e) => {
        const fn = this._onmessage;
        if (fn) fn(e.payload as T);
      });
    }
  }

  /**
   * Tauri 序列化 Channel 时会调用其内部 toJSON 返回 `__CHANNEL__ID__` 占位。
   * Web 模式下 server 不依赖 channel 参数，返回 null 即可。
   */
  toJSON() {
    return this._tauri ?? null;
  }
}
