// 内置浏览器的 URL 归一化 / 两档安全校验 / 聊天流检测（架构 §8.5）。
//
// 与 Rust 侧 apps/desktop/src/browser/url_policy.rs 共用同一份 case 清单
// （见两侧测试文件头部注释）：前端校验只是 UX，Rust 校验才是安全边界，
// 改任何规则必须两侧同步。

/** 自动通道（聊天流检测 / auto-follow / agent 触发）只允许本地网段。 */
export function isLocalPreviewHostname(hostname: string): boolean {
  const host = hostname.trim().toLowerCase().replace(/^\[/, "").replace(/\]$/, "");
  if (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host === "host.docker.internal" ||
    host.endsWith(".local") ||
    host === "::1" ||
    host === "0.0.0.0" || // dev server bind-all，归一化时重写成 127.0.0.1
    host === "::"
  ) {
    return true;
  }
  const octets = parseIpv4(host);
  if (!octets) return false;
  const [a, b] = octets as [number, number, number, number];
  return (
    a === 10 ||
    a === 127 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168)
  );
}

/** 云元数据等探测式地址，任何档位都拒绝。 */
export function isBlockedProbeHostname(hostname: string): boolean {
  const host = hostname.trim().toLowerCase().replace(/^\[/, "").replace(/\]$/, "");
  if (host === "metadata.google.internal" || host === "metadata.goog") return true;
  const octets = parseIpv4(host);
  if (!octets) return false;
  const [a, b] = octets as [number, number, number, number];
  // 169.254.0.0/16 链路本地段整段拒绝（含 169.254.169.254 云元数据）
  return a === 169 && b === 254;
}

function parseIpv4(hostname: string): number[] | null {
  const parts = hostname.split(".");
  if (parts.length !== 4) return null;
  const octets: number[] = [];
  for (const part of parts) {
    if (!/^\d{1,3}$/.test(part)) return null;
    const n = Number(part);
    if (n > 255) return null;
    octets.push(n);
  }
  return octets;
}

/**
 * 宽松输入 → 规范 URL 字符串。非法输入返回 null。
 * - 纯端口数字（2-5 位）补全为 http://127.0.0.1:<port>/
 * - 无 scheme 补 http://
 * - 0.0.0.0 / :: 重写为 127.0.0.1（dev server 常见监听地址，浏览不可达）
 * - 仅放行 http / https
 */
export function normalizePreviewUrlInput(input: string): string | null {
  let value = input.trim();
  if (!value) return null;
  if (/^\d{2,5}$/.test(value)) {
    value = `http://127.0.0.1:${value}`;
  } else if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(value)) {
    // 像真浏览器一样补 scheme：本地地址用 http，公网域名默认 https
    const bareHost = value.split("/")[0].split(":")[0];
    const scheme = isLocalPreviewHostname(bareHost) ? "http" : "https";
    value = `${scheme}://${value}`;
  }
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  const host = url.hostname.replace(/^\[/, "").replace(/\]$/, "");
  if (host === "0.0.0.0" || host === "::") url.hostname = "127.0.0.1";
  if (!url.pathname) url.pathname = "/";
  return url.toString();
}

export type PreviewOrigin = "auto" | "user";

/**
 * 两档校验（架构 §8.5-4）：auto 档仅本地网段；user 档放行公网 http(s)。
 * 探测式地址两档都拒。返回规范化 URL 或 null。
 */
export function validatePreviewUrl(input: string, origin: PreviewOrigin): string | null {
  const normalized = normalizePreviewUrlInput(input);
  if (!normalized) return null;
  const hostname = new URL(normalized).hostname;
  if (isBlockedProbeHostname(hostname)) return null;
  if (origin === "auto" && !isLocalPreviewHostname(hostname)) return null;
  return normalized;
}

// ───────────────────────── 聊天流 URL 检测（双阈值） ─────────────────────────

const MAX_DETECTED_URLS = 4;
const NEARBY_CONTEXT_CHARS = 120;

const LOCAL_URL_CANDIDATE_RE =
  /\b(?:https?:\/\/)?(?:localhost|(?:[\w-]+\.)?localhost|host\.docker\.internal|[\w.-]+\.local|127(?:\.\d{1,3}){3}|0\.0\.0\.0|10(?:\.\d{1,3}){3}|192\.168(?:\.\d{1,3}){2}|172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2}|\[::1\])(?::\d{2,5})?(?:\/[^\s'"<>)\]]*)?/gi;

const DEV_SERVER_COMMAND_RE =
  /\b(?:(?:npm|pnpm|yarn|bun)\s+(?:run\s+)?(?:dev|start|serve|preview)|vite(?:\s|$)|next\s+dev|nuxt\s+dev|astro\s+dev|remix\s+dev|webpack(?:-dev-server|\s+serve)|react-scripts\s+start|storybook(?:\s+dev)?|svelte-kit\s+dev|cargo\s+run|trunk\s+serve|python3?\s+-m\s+http\.server)\b/i;

const DEV_SERVER_OUTPUT_RE =
  /\b(?:vite v?\d|local:\s*https?:\/\/|network:\s*https?:\/\/|ready in \d+(?:\.\d+)?\s*(?:ms|s)|ready on\s+https?:\/\/|started server|server started|compiled successfully|webpack compiled|app running at|serving at|listening on\s+|listening at\s+)\b/i;

const ASSISTANT_ACTION_RE =
  /\b(?:open|visit|browse|view|check(?:\s+it)?\s+out|go\s+to|preview)\b|(?:打开|访问|前往|查看|预览)/i;

const ASSISTANT_STATUS_RE =
  /\b(?:served|serving|running|started|available|reachable|live\s+at|running\s+at|available\s+at|listening\s+on)\b|(?:运行在|启动于|已启动|可访问|可预览|本地服务)/i;

const NON_PREVIEW_CONTEXT_RE =
  /\b(?:health check|bearer token|model_io|otlp|telemetry)\b|\/(?:health|v\d+\/|metrics|readyz?|livez?)(?:\b|\/|\?)/i;

/** 检测输入：发消息后由调用方从消息流里抽出来的纯文本片段。 */
export interface PreviewDetectSource {
  kind: "assistant" | "tool_command" | "tool_output";
  text: string;
}

export type PreviewDetectMode = "card" | "autoOpen";

function pathLooksLikePage(normalized: string): boolean {
  try {
    const pathname = decodeURIComponent(new URL(normalized).pathname).toLowerCase();
    if (/^\/(?:health|metrics|readyz?|livez?|v\d+)(?:\/|$)/.test(pathname)) return false;
    if (/\/(?:health|metrics|readyz?|livez?)(?:\/|$)/.test(pathname)) return false;
    return true;
  } catch {
    return false;
  }
}

function surroundingContext(text: string, index: number, length: number): string {
  const lineStart = text.lastIndexOf("\n", Math.max(0, index - 1));
  const lineEnd = text.indexOf("\n", index + length);
  const line = text.slice(lineStart === -1 ? 0 : lineStart + 1, lineEnd === -1 ? text.length : lineEnd);
  if (line.length <= NEARBY_CONTEXT_CHARS * 2) return line;
  const start = Math.max(0, index - NEARBY_CONTEXT_CHARS);
  const end = Math.min(text.length, index + length + NEARBY_CONTEXT_CHARS);
  return text.slice(start, end);
}

function sourceCanAdvertise(source: PreviewDetectSource, mode: PreviewDetectMode): boolean {
  const text = source.text;
  if (source.kind === "assistant") {
    const looksLikeDevServer = DEV_SERVER_OUTPUT_RE.test(text);
    if (mode === "autoOpen") return looksLikeDevServer;
    if (looksLikeDevServer) return true;
    if (NON_PREVIEW_CONTEXT_RE.test(text)) return false;
    return ASSISTANT_ACTION_RE.test(text) || ASSISTANT_STATUS_RE.test(text);
  }
  const commandLike = source.kind === "tool_command" && DEV_SERVER_COMMAND_RE.test(text);
  const outputLike = source.kind === "tool_output" && DEV_SERVER_OUTPUT_RE.test(text);
  if (mode === "autoOpen") return outputLike;
  return commandLike || outputLike;
}

/**
 * 从消息片段（新 → 旧排列）提取候选预览 URL。
 * card 模式宽松（给候选 chips）；autoOpen 模式严格（只认 dev server 输出特征，
 * 调用方还应限定"最近一条 user 消息之后"的片段——自动打开只响应本轮）。
 */
export function extractPreviewUrls(
  sources: PreviewDetectSource[],
  mode: PreviewDetectMode
): string[] {
  const urls: string[] = [];
  const seen = new Set<string>();
  for (const source of sources) {
    if (!sourceCanAdvertise(source, mode)) continue;
    const looksLikeDevServer = DEV_SERVER_OUTPUT_RE.test(source.text);
    for (const match of source.text.matchAll(LOCAL_URL_CANDIDATE_RE)) {
      if (source.kind === "assistant" && !looksLikeDevServer) {
        const ctx = surroundingContext(source.text, match.index ?? 0, match[0].length);
        if (NON_PREVIEW_CONTEXT_RE.test(ctx)) continue;
        if (!ASSISTANT_ACTION_RE.test(ctx) && !ASSISTANT_STATUS_RE.test(ctx)) continue;
      }
      const candidate = match[0].replace(/[`),.;]+$/g, "");
      const normalized = validatePreviewUrl(candidate, "auto");
      if (!normalized || !pathLooksLikePage(normalized) || seen.has(normalized)) continue;
      seen.add(normalized);
      urls.push(normalized);
      if (urls.length >= MAX_DETECTED_URLS) return urls;
    }
  }
  return urls;
}

/** 会话消息 → 检测源（新→旧）。assistant 文本 + 工具调用的 command(input)/output(result)。 */
export function messagesToDetectSources(
  messages: { role: string; content?: string; parts?: unknown[]; tool_calls?: unknown[] }[]
): PreviewDetectSource[] {
  const sources: PreviewDetectSource[] = [];
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i];
    if (m.role === "assistant" && m.content) {
      sources.push({ kind: "assistant", text: m.content });
    }
    const calls = Array.isArray(m.tool_calls) ? m.tool_calls : [];
    for (const c of calls as { name?: string; input?: unknown; result?: unknown }[]) {
      // 只认命令执行类工具——Read/Grep 结果里的 URL 不该触发预览（对齐 deepseek-gui）。
      if (c.name !== "Bash" && c.name !== "PowerShell") continue;
      const cmd =
        typeof (c.input as { command?: unknown })?.command === "string"
          ? (c.input as { command: string }).command
          : "";
      if (cmd) sources.push({ kind: "tool_command", text: cmd });
      if (typeof c.result === "string" && c.result) {
        sources.push({ kind: "tool_output", text: c.result });
      }
    }
  }
  return sources;
}

/** 地址栏紧凑显示：host + 非根路径。 */
export function formatPreviewUrlLabel(url: string): string {
  try {
    const parsed = new URL(url);
    const path = parsed.pathname === "/" ? "" : parsed.pathname;
    return `${parsed.host}${path}`;
  } catch {
    return url;
  }
}
