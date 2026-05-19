/**
 * 架构 §8：Desktop `//` 命令系统。
 *
 * 用户在 ChatInput 输入以 `//` 开头的行时，前端把整行当成 client-side 命令，
 * 不发送给模型。每个命令在这里注册一个 handler，由 ChatInput 在提交前调用
 * `dispatchSlashCommand(value, ctx)`：
 * - 命中 → 执行 handler，返回 `{ handled: true }`；ChatInput 跳过 onSend
 * - 未命中（仍以 `//` 开头但名字陌生）→ `{ handled: true, error: "..." }`，
 *   ChatInput 显示错误 toast，避免把伪命令当 prompt 发给模型
 * - 不以 `//` 开头 → `{ handled: false }`，ChatInput 走原始流程
 *
 * 区别于历史的单斜杠命令（`/compact`）：单斜杠是早期硬编码拦截，将来再统一搬到
 * 这套系统；新命令一律 `//` 前缀，避免与模型可能输出的路径 / 引用记号冲突。
 */

import { api } from "@/desktop/bridge/tauri";

export interface SlashContext {
  sessionId: string | null;
  toast: {
    success: (msg: string) => void;
    error: (msg: string) => void;
    info?: (msg: string) => void;
  };
}

export interface SlashResult {
  handled: boolean;
  error?: string;
}

type Handler = (args: string[], ctx: SlashContext) => Promise<void>;

/**
 * 命令清单元数据，供 ChatInput 工具栏的 `//` popup 渲染列表。
 * 新增命令时在 `registry` 与 `slashCommandCatalog` 各加一条。
 */
export interface SlashCommandMeta {
  /** 不带 `//` 前缀的命令名，例如 `"force-automode"`。 */
  name: string;
  /** 在 popup 列表里跟在命令名后的参数提示，例如 `"[on|off|toggle|status]"`。 */
  args: string;
  /** 一句话描述，rendered 在列表右侧；保持 < 30 字符。 */
  desc: string;
}

export const slashCommandCatalog: SlashCommandMeta[] = [
  {
    name: "force-automode",
    args: "[on|off|toggle|status]",
    desc: "自动模式下遇到不确定操作直接拒绝",
  },
];

const registry: Record<string, Handler> = {
  "force-automode": async (args, ctx) => {
    if (!ctx.sessionId) {
      throw new Error("当前没有打开的对话");
    }
    const current = await api.getForceAutomode(ctx.sessionId);
    const arg = args[0]?.toLowerCase();
    if (arg === "status") {
      ctx.toast.success(current ? "「自动拒绝」当前已开启" : "「自动拒绝」当前已关闭");
      return;
    }
    const next = parseBoolArg(arg, current);
    const applied = await api.setForceAutomode(ctx.sessionId, next);
    ctx.toast.success(
      applied
        ? "已开启「自动拒绝」：不确定的操作不再询问，直接拒绝"
        : "已关闭「自动拒绝」"
    );
  },
};

/** 解析 `on` / `off` / `toggle`（或缺省）；缺省 = 翻转当前值。 */
function parseBoolArg(raw: string | undefined, current: boolean): boolean {
  if (!raw) return !current;
  switch (raw) {
    case "on":
    case "true":
    case "1":
    case "enable":
    case "enabled":
      return true;
    case "off":
    case "false":
    case "0":
    case "disable":
    case "disabled":
      return false;
    case "toggle":
      return !current;
    default:
      throw new Error(`无法识别的参数：${raw}（期望 on / off / toggle / status）`);
  }
}

/**
 * 尝试把一行输入当作 `//` 命令派发；返回是否被消费。
 *
 * 输入的 leading/trailing 空白由调用方负责裁剪；本函数从第一个 token 解析命令名。
 */
export async function dispatchSlashCommand(
  raw: string,
  ctx: SlashContext
): Promise<SlashResult> {
  if (!raw.startsWith("//")) {
    return { handled: false };
  }
  const body = raw.slice(2).trim();
  if (!body) {
    return { handled: true, error: "空命令：示例 `//force-automode on`" };
  }
  const [name, ...args] = body.split(/\s+/);
  const handler = registry[name];
  if (!handler) {
    return {
      handled: true,
      error: `未知命令：//${name}（已注册：${Object.keys(registry)
        .map((n) => `//${n}`)
        .join(", ")}）`,
    };
  }
  try {
    await handler(args, ctx);
    return { handled: true };
  } catch (e: any) {
    return { handled: true, error: e?.message ?? String(e) };
  }
}
