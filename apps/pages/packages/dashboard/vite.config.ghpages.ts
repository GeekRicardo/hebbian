import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

/**
 * GitHub Pages 静态构建配置
 *
 * 特点:
 * - 纯静态输出，零服务端依赖
 * - base: './' 适配 GitHub Pages 子目录部署
 * - VITE_STATIC_MODE 标记跳过 TokenGate，数据从静态文件加载
 *
 * 构建流程:
 *   1. node scripts/bundle-graph.mjs  → 打包 graph + 源码到 public/
 *   2. vite build --config vite.config.ghpages.ts → dist-ghpages/
 *   3. 部署 dist-ghpages/ 到 GitHub Pages
 */
export default defineConfig({
  base: "./",
  define: {
    "import.meta.env.VITE_STATIC_MODE": JSON.stringify("true"),
  },
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "dist-ghpages",
    target: "esnext",
  },
  esbuild: {
    target: "esnext",
  },
});
