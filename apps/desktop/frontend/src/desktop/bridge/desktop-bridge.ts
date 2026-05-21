/**
 * Desktop bridge client —— 让 Tauri 前端当 hebweb 的 invoke proxy。
 *
 * 仅在 Tauri 环境启动：connect 到 hebweb 的 /ws/bridge，注册自己；
 * mediator 收到外部浏览器（Playwright）的 invoke 时转发过来，本端调真实
 * `tauriInvoke(cmd, args)`，把结果回传——浏览器拿到的就是 desktop 完整后端响应。
 *
 * Step 1：仅处理 sync invoke。流式 Channel / 全局 listen 待 Step 2。
 */

import { invoke as tauriInvoke, Channel as TauriChannel } from "@tauri-apps/api/core";

/** 哪些命令需要注入 Channel<EngineEvent> 转发流式事件。当前仅 send_message。 */
const CHANNEL_COMMANDS = new Set(["send_message"]);
/** 哪个字段是 Tauri Channel 参数名。 */
const CHANNEL_FIELD = "onEvent";

type BridgeOutbound =
  | { type: "welcome"; server_version: string }
  | { type: "proxy_invoke"; req_id: string; cmd: string; args: unknown };

const DEFAULT_BRIDGE_URL = "ws://127.0.0.1:38080/ws/bridge";
const RECONNECT_DELAY_MS = 3000;
/** 心跳间隔：每 10s 发一次 ping。hebweb 收到未知 type 会忽略，仅用于探测连接死活。 */
const PING_INTERVAL_MS = 10000;

let started = false;
let reconnecting = false;

export function startDesktopBridge(url: string = DEFAULT_BRIDGE_URL) {
  if (started) return;
  started = true;
  connect(url);
}

function connect(url: string) {
  reconnecting = false;
  let ws: WebSocket;
  let pingTimer: ReturnType<typeof setInterval> | null = null;

  function cleanupAndReconnect(reason: string) {
    if (reconnecting) return;
    reconnecting = true;
    if (pingTimer != null) {
      clearInterval(pingTimer);
      pingTimer = null;
    }
    try {
      ws?.close();
    } catch {}
    console.info(`[desktop-bridge] ${reason}, reconnecting in ${RECONNECT_DELAY_MS}ms`);
    setTimeout(() => connect(url), RECONNECT_DELAY_MS);
  }

  try {
    ws = new WebSocket(url);
  } catch (e) {
    console.warn("[desktop-bridge] failed to construct WS", e);
    cleanupAndReconnect("construct failed");
    return;
  }

  ws.onopen = () => {
    ws.send(
      JSON.stringify({
        type: "register",
        client_label: `desktop-${Date.now().toString(36)}`,
      }),
    );
    console.info("[desktop-bridge] registered to", url);
    // 心跳：定期 send，对端只要还活着 send 就成功；对端死了 send 会抛/触发 onclose
    pingTimer = setInterval(() => {
      try {
        if (ws.readyState !== WebSocket.OPEN) {
          cleanupAndReconnect("readyState != OPEN");
          return;
        }
        ws.send(JSON.stringify({ type: "ping" }));
      } catch (e) {
        cleanupAndReconnect("ping send failed");
      }
    }, PING_INTERVAL_MS);
  };

  ws.onmessage = async (e) => {
    if (typeof e.data !== "string") return;
    let msg: BridgeOutbound;
    try {
      msg = JSON.parse(e.data);
    } catch {
      return;
    }
    if (msg.type === "welcome") {
      console.info("[desktop-bridge] welcome from server", msg.server_version);
      return;
    }
    if (msg.type === "proxy_invoke") {
      const { req_id, cmd, args } = msg;
      const argsObj = (args ?? {}) as Record<string, unknown>;

      // 需要注入 Tauri Channel 的命令：把 channel.onmessage 的每条事件通过 ws
      // 转发给 mediator，mediator 再按 session_id 路由给浏览器订阅者。
      if (CHANNEL_COMMANDS.has(cmd)) {
        const sessionId =
          (argsObj["sessionId"] as string | undefined) ??
          (argsObj["session_id"] as string | undefined);
        if (sessionId) {
          const channel = new TauriChannel<unknown>();
          channel.onmessage = (payload) => {
            ws.send(
              JSON.stringify({
                type: "channel_event",
                req_id,
                session_id: sessionId,
                payload,
              }),
            );
          };
          argsObj[CHANNEL_FIELD] = channel;
        }
      }

      try {
        const data = await tauriInvoke<unknown>(cmd, argsObj);
        ws.send(
          JSON.stringify({
            type: "proxy_response",
            req_id,
            ok: true,
            data: data ?? null,
          }),
        );
      } catch (err) {
        ws.send(
          JSON.stringify({
            type: "proxy_response",
            req_id,
            ok: false,
            error: err instanceof Error ? err.message : String(err),
          }),
        );
      }
    }
  };

  ws.onerror = (e) => {
    // onerror 不一定带 onclose（macOS WKWebSocket 偶发），主动触发 reconnect
    console.warn("[desktop-bridge] ws error", e);
    cleanupAndReconnect("ws error");
  };

  ws.onclose = () => {
    cleanupAndReconnect("disconnected");
  };
}
