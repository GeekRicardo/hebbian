import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

/** 纯静态本地开发配置：STATIC_MODE=true，base="/"，无 token gate */
export default defineConfig({
  base: "/",
  define: {
    "import.meta.env.VITE_STATIC_MODE": JSON.stringify("true"),
  },
  plugins: [react(), tailwindcss()],
});
