// 内置浏览器 URL 策略测试（inline node sanity check，项目惯例：前端无 vitest）。
// 跑法：pnpm exec tsx frontend/src/desktop/ui/lib/previewUrl.test.ts
//
// ⚠️ 共享 case 清单：下列「两档校验」用例与 Rust 侧
// apps/desktop/src/browser/url_policy.rs 的单测一一对应，改任何一侧必须同步另一侧。

import {
  extractPreviewUrls,
  formatPreviewUrlLabel,
  isBlockedProbeHostname,
  isLocalPreviewHostname,
  messagesToDetectSources,
  normalizePreviewUrlInput,
  validatePreviewUrl,
} from "./previewUrl.ts";

function expectEqual(name: string, actual: unknown, expected: unknown) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${name}: expected ${e}, got ${a}`);
  }
}

// ── 归一化 ──────────────────────────────────────────────────────────────
expectEqual("纯端口补全", normalizePreviewUrlInput("3000"), "http://127.0.0.1:3000/");
expectEqual("本地无 scheme 补 http", normalizePreviewUrlInput("localhost:5173"), "http://localhost:5173/");
expectEqual("公网无 scheme 补 https", normalizePreviewUrlInput("example.com"), "https://example.com/");
expectEqual("公网带端口路径补 https", normalizePreviewUrlInput("example.com:8443/app"), "https://example.com:8443/app");
expectEqual("局域网 IP 补 http", normalizePreviewUrlInput("192.168.1.5:8080"), "http://192.168.1.5:8080/");
expectEqual("0.0.0.0 重写", normalizePreviewUrlInput("http://0.0.0.0:5173"), "http://127.0.0.1:5173/");
expectEqual("保留路径", normalizePreviewUrlInput("127.0.0.1:3000/settings?a=1"), "http://127.0.0.1:3000/settings?a=1");
expectEqual("拒绝 ftp", normalizePreviewUrlInput("ftp://127.0.0.1/x"), null);
expectEqual("拒绝空串", normalizePreviewUrlInput("   "), null);
expectEqual("拒绝纯乱码", normalizePreviewUrlInput("http://"), null);

// ── 本地网段判定 ─────────────────────────────────────────────────────────
expectEqual("localhost", isLocalPreviewHostname("localhost"), true);
expectEqual("sub.localhost", isLocalPreviewHostname("app.localhost"), true);
expectEqual("docker", isLocalPreviewHostname("host.docker.internal"), true);
expectEqual("mdns", isLocalPreviewHostname("mymac.local"), true);
expectEqual("loopback", isLocalPreviewHostname("127.0.0.1"), true);
expectEqual("loopback 任意", isLocalPreviewHostname("127.1.2.3"), true);
expectEqual("net 10", isLocalPreviewHostname("10.0.0.8"), true);
expectEqual("net 172.16", isLocalPreviewHostname("172.16.0.2"), true);
expectEqual("net 172.31", isLocalPreviewHostname("172.31.255.1"), true);
expectEqual("net 172.32 否", isLocalPreviewHostname("172.32.0.1"), false);
expectEqual("net 192.168", isLocalPreviewHostname("192.168.1.10"), true);
expectEqual("ipv6 loop", isLocalPreviewHostname("::1"), true);
expectEqual("公网域名 否", isLocalPreviewHostname("example.com"), false);
expectEqual("公网 IP 否", isLocalPreviewHostname("8.8.8.8"), false);

// ── 探测地址黑名单 ───────────────────────────────────────────────────────
expectEqual("aws 元数据", isBlockedProbeHostname("169.254.169.254"), true);
expectEqual("链路本地整段", isBlockedProbeHostname("169.254.1.1"), true);
expectEqual("gcp 元数据", isBlockedProbeHostname("metadata.google.internal"), true);
expectEqual("普通公网不拦", isBlockedProbeHostname("example.com"), false);

// ── 两档校验（与 Rust url_policy.rs 共享 case）─────────────────────────────
expectEqual("auto 放行本地", validatePreviewUrl("localhost:3000", "auto"), "http://localhost:3000/");
expectEqual("auto 拒绝公网", validatePreviewUrl("https://example.com", "auto"), null);
expectEqual("user 放行公网", validatePreviewUrl("https://example.com", "user"), "https://example.com/");
expectEqual("user 放行本地", validatePreviewUrl("3000", "user"), "http://127.0.0.1:3000/");
expectEqual("user 拒元数据", validatePreviewUrl("http://169.254.169.254/latest", "user"), null);
expectEqual("auto 拒元数据", validatePreviewUrl("169.254.169.254", "auto"), null);
expectEqual("user 拒 ftp", validatePreviewUrl("ftp://example.com", "user"), null);
expectEqual("auto 0.0.0.0 重写后放行", validatePreviewUrl("0.0.0.0:5173", "auto"), "http://127.0.0.1:5173/");

// ── 聊天流检测：autoOpen 严格档 ──────────────────────────────────────────
const viteOutput = {
  kind: "tool_output" as const,
  text: "  VITE v5.4.11  ready in 320 ms\n\n  ➜  Local:   http://localhost:5173/\n  ➜  Network: use --host to expose",
};
expectEqual(
  "vite 输出触发 autoOpen",
  extractPreviewUrls([viteOutput], "autoOpen"),
  ["http://localhost:5173/"]
);
expectEqual(
  "纯聊天提及不触发 autoOpen",
  extractPreviewUrls([{ kind: "assistant", text: "你可以打开 http://localhost:5173 看看" }], "autoOpen"),
  []
);
expectEqual(
  "card 档放行带动作词的提及",
  extractPreviewUrls([{ kind: "assistant", text: "你可以打开 http://localhost:5173 看看" }], "card"),
  ["http://localhost:5173/"]
);
expectEqual(
  "无动作词的裸 URL 不进 card",
  extractPreviewUrls([{ kind: "assistant", text: "配置位于 http://localhost:9999/internal" }], "card"),
  []
);
expectEqual(
  "health 路径剔除",
  extractPreviewUrls(
    [{ kind: "tool_output", text: "server started, listening on http://127.0.0.1:8080/health" }],
    "card"
  ),
  []
);
expectEqual(
  "CDP devtools 端点不进检测",
  extractPreviewUrls(
    [
      {
        kind: "assistant",
        text: "现在打开浏览器 tab 访问 http://127.0.0.1:9229/devtools/browser/1de09720-cc00-4d31-8ff1-17544fabe2be 看看",
      },
    ],
    "card"
  ),
  []
);
expectEqual(
  "json/version 发现端点不进检测",
  extractPreviewUrls(
    [{ kind: "tool_output", text: "listening on http://127.0.0.1:9229/json/version" }],
    "card"
  ),
  []
);
expectEqual(
  "dev 命令触发 card",
  extractPreviewUrls(
    [{ kind: "tool_command", text: "pnpm dev --port 4321 # http://localhost:4321" }],
    "card"
  ),
  ["http://localhost:4321/"]
);
expectEqual(
  "去重且上限 4",
  extractPreviewUrls(
    [
      {
        kind: "tool_output",
        text: "listening on http://127.0.0.1:3001\nlistening on http://127.0.0.1:3001\nlistening on http://127.0.0.1:3002\nlistening on http://127.0.0.1:3003\nlistening on http://127.0.0.1:3004\nlistening on http://127.0.0.1:3005",
      },
    ],
    "card"
  ).length,
  4
);
expectEqual(
  "公网 URL 永不进检测",
  extractPreviewUrls(
    [{ kind: "tool_output", text: "server started, listening on http://evil.example.com/" }],
    "autoOpen"
  ),
  []
);

// ── messagesToDetectSources → 检测链路 ───────────────────────────────────
const convo = [
  { role: "user", content: "起个 dev server" },
  {
    role: "assistant",
    content: "好的，正在启动",
    tool_calls: [
      { name: "Bash", input: { command: "pnpm dev" }, result: "VITE v5 ready in 200 ms\nLocal: http://localhost:5173/" },
    ],
  },
];
const detected = extractPreviewUrls(messagesToDetectSources(convo), "autoOpen");
expectEqual("从工具输出自动检测 dev URL", detected, ["http://localhost:5173/"]);
expectEqual(
  "非 Bash 工具不进检测",
  extractPreviewUrls(
    messagesToDetectSources([
      { role: "assistant", content: "x", tool_calls: [{ name: "Read", input: { path: "http://localhost:3000" }, result: "http://localhost:3000" }] },
    ]),
    "autoOpen"
  ),
  []
);

// ── 标签 ────────────────────────────────────────────────────────────────
expectEqual("label 根路径省略", formatPreviewUrlLabel("http://localhost:5173/"), "localhost:5173");
expectEqual("label 带路径", formatPreviewUrlLabel("http://localhost:5173/settings"), "localhost:5173/settings");

console.log("previewUrl.test.ts: all assertions passed");
