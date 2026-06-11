import clsx, { type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import { confirm as tauriConfirm } from "@tauri-apps/plugin-dialog";

/**
 * Tauri ACL 禁止原生 window.confirm()，统一走 plugin-dialog 的 message 命令。
 */
export async function ipcConfirm(message: string, title?: string): Promise<boolean> {
  return tauriConfirm(message, { title, kind: "warning" });
}

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function formatTime(ts: number) {
  const d = new Date(ts);
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  if (sameDay) {
    return d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  }
  return d.toLocaleDateString("zh-CN", { month: "2-digit", day: "2-digit" });
}

export function pathLeaf(path: string) {
  const trimmed = path.replace(/[\\/]+$/, "");
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] || trimmed || "";
}

/**
 * 若 path 在 base 目录下，返回相对部分（`base` 自身 → "."；`base/x` → "x"）；
 * 否则原样返回 path。base 为空时直接原样返回。
 * 用于把项目级允许路径渲染为「workdir 相对路径」减少噪音。
 */
export function relativizeIfUnder(path: string, base?: string | null): string {
  if (!base) return path;
  const norm = (s: string) => s.replace(/[\\/]+$/, "");
  const b = norm(base);
  const p = norm(path);
  if (!b) return path;
  if (p === b) return ".";
  if (p.startsWith(b + "/") || p.startsWith(b + "\\")) {
    return p.slice(b.length + 1);
  }
  return path;
}

export function hasSessionStarted(
  session?: { messages?: Array<{ role: string }> } | null
) {
  return !!session?.messages?.some(
    (message) => message.role === "user" || message.role === "assistant"
  );
}
