import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

const host = process.env.TAURI_DEV_HOST;
const frontendRoot = fileURLToPath(new URL("frontend", import.meta.url));
const distDir = fileURLToPath(new URL("dist", import.meta.url));

export default defineConfig(async () => ({
  root: frontendRoot,
  plugins: [react()],
  resolve: {
    alias: {
      "@": `${frontendRoot}/src`,
    },
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
  },
}));
