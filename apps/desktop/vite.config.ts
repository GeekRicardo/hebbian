import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";

const host = process.env.TAURI_DEV_HOST;
const frontendRoot = fileURLToPath(new URL("frontend", import.meta.url));
const distDir = fileURLToPath(new URL("dist", import.meta.url));

function git(args: string): string {
  try {
    return execSync(`git ${args}`, { encoding: "utf8" }).trim();
  } catch {
    return "";
  }
}

function buildInfo() {
  const conf = JSON.parse(
    readFileSync(
      fileURLToPath(new URL("tauri.conf.json", import.meta.url)),
      "utf8"
    )
  );
  return {
    version: conf.version as string,
    build: git("rev-list --count HEAD") || "0",
    commit: git("rev-parse --short HEAD") || "unknown",
    dirty: git("status --porcelain") !== "",
    builtAt: new Date().toISOString(),
  };
}

export default defineConfig(async () => ({
  root: frontendRoot,
  plugins: [react()],
  resolve: {
    alias: {
      "@": `${frontendRoot}/src`,
    },
  },
  define: {
    __BUILD_INFO__: JSON.stringify(buildInfo()),
  },
  build: {
    outDir: distDir,
    emptyOutDir: true,
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: "ws", host, port: 1421 }
      : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
    // dev 模式下把 /ws 与 hebweb 同步 API 代理到本地 hebweb，方便在未压缩源码下
    // 调试 web transport（HEBWEB_PROXY=端口 时启用，不影响 tauri / build）。
    proxy: process.env.HEBWEB_PROXY
      ? {
          "/ws": {
            target: `ws://127.0.0.1:${process.env.HEBWEB_PROXY}`,
            ws: true,
          },
        }
      : undefined,
  },
}));
