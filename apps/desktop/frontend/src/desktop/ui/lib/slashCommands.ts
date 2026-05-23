/**
 * 架构 §8：Desktop `//` 命令系统。
 *
 * 命令分两类，语义不同（§8.1）：
 *
 * 1. **内置控制命令**（如 `//force-automode`）——本地派发，**不发给 LLM**：
 *    handler 直接调 Tauri command 改 desktop 进程态 / session meta，结果以 toast 回显，
 *    不写 transcript / 不写 model request。失败 fail-closed 弹 toast。
 *
 * 2. **Skill 命令**（如 `//commit`，对应 `~/.hebbian/skills/commit/SKILL.md`）——
 *    本地识别 + 改写后发给 LLM：handler 把 `//<name> [args]` 改写成普通 user message
 *    `/<name> [args]` 并走正常发送路径，模型读到后会自动调用 [`Skill`] 工具读 SKILL.md。
 *    skill 命令必然写进 transcript（这是模型理解上下文的前提）。
 *
 * 调用方（ChatInput）在 submit 路径上调 `dispatchSlashCommand(value, ctx, skills)`：
 * - 命中（内置或 skill）→ 执行 handler，返回 `{ handled: true }`；ChatInput 跳过原始 onSend
 * - 未命中（仍以 `//` 开头但名字陌生且不是 skill）→ `{ handled: true, error: "..." }`，
 *   ChatInput 显示错误 toast，避免把伪命令当 prompt 发给模型
 * - 不以 `//` 开头 → `{ handled: false }`，ChatInput 走原始流程
 *
 * 区别于历史的单斜杠命令（`/compact`）：单斜杠是早期硬编码拦截，将来再统一搬到
 * 这套系统；新命令一律 `//` 前缀，避免与模型可能输出的路径 / 引用记号冲突。
 */

import { api } from "@/desktop/bridge/tauri";
import type { SkillItem } from "@/desktop/ui/types";

export interface SlashContext {
  sessionId: string | null;
  toast: {
    success: (msg: string) => void;
    error: (msg: string) => void;
    info?: (msg: string) => void;
  };
  /**
   * 把一段文本作为 user message 发出去（走正常 onSend 路径）。
   * 仅 skill 命令会用，内置控制命令永远不调（§8.1.5）。
   */
  sendPrompt: (text: string) => Promise<void>;
}

export interface SlashResult {
  handled: boolean;
  error?: string;
}

type BuiltinHandler = (args: string[], ctx: SlashContext) => Promise<void>;

/**
 * 命令清单元数据，供 ChatInput 工具栏的 `//` popup 渲染列表。
 *
 * `kind` 区分命令类型：
 * - `"builtin"`：内置控制命令，handler 在本文件 [`builtinRegistry`] 里
 * - `"skill"`：动态 skill 命令，handler 由 [`dispatchSlashCommand`] 统一处理
 */
export interface SlashCommandMeta {
  /** 不带 `//` 前缀的命令名，例如 `"force-automode"`。 */
  name: string;
  /** 在 popup 列表里跟在命令名后的参数提示，例如 `"[on|off|toggle|status]"`。 */
  args: string;
  /** 一句话描述，rendered 在列表右侧；保持 < 60 字符。 */
  desc: string;
  kind: "builtin" | "skill";
  /** skill 专有：标识它来自哪个 scope（用于 popup 角标）。 */
  skillSource?: SkillItem["source"];
}

/** 内置命令的静态清单，供 popup 列表的"内置"分组使用。 */
export const builtinSlashCommands: SlashCommandMeta[] = [
  {
    name: "force-automode",
    args: "[on|off|toggle|status]",
    desc: "自动模式下遇到不确定操作直接拒绝",
    kind: "builtin",
  },
];

const builtinRegistry: Record<string, BuiltinHandler> = {
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

/** Skill 公开名：frontmatter alias 优先（与目录名不同时），回退目录名。 */
function skillDisplayName(s: SkillItem): string {
  return s.alias && s.alias !== s.name ? s.alias : s.name;
}

/**
 * 把动态 skills 拼到内置清单后，得到 popup 完整列表。
 *
 * - 仅展示 `enabled=true` 的 skill（被禁用的就不该出现在命令面板里）
 * - 同名 skill 若与内置命令冲突，**内置优先**（理论上不该撞，撞了内置覆盖）
 * - skill 命令的 args 提示固定为 `[args…]`，desc 取 SKILL.md 的 description 截断
 * - **公开名**用 alias（frontmatter `name:`）优先，目录名做副名——和 SkillTool
 *   description 给模型看到的列表保持一致，避免用户敲 `//karpathy-guidelines` 时
 *   popup 里只显示目录名 `karpathy` 看不到对应项
 */
export function buildSlashCommandCatalog(skills: SkillItem[]): SlashCommandMeta[] {
  const builtinNames = new Set(builtinSlashCommands.map((c) => c.name));
  const skillMetas = skills
    .filter((s) => s.enabled && !builtinNames.has(skillDisplayName(s)))
    .map<SlashCommandMeta>((s) => ({
      name: skillDisplayName(s),
      args: "[args…]",
      desc: trimDesc(s.description),
      kind: "skill",
      skillSource: s.source,
    }));
  return [...builtinSlashCommands, ...skillMetas];
}

function trimDesc(s: string, limit = 80): string {
  const oneLine = s.replace(/\s+/g, " ").trim();
  if (oneLine.length <= limit) return oneLine;
  return oneLine.slice(0, limit) + "…";
}

/**
 * 尝试把一行输入当作 `//` 命令派发；返回是否被消费。
 *
 * `skills` 来自上层（ChatInput 在 workdir 变化时调 `api.listSkills` 拉一次）。
 * 命令查找顺序：内置 → enabled skill。
 *
 * 输入的 leading/trailing 空白由调用方负责裁剪；本函数从第一个 token 解析命令名。
 */
export async function dispatchSlashCommand(
  raw: string,
  ctx: SlashContext,
  skills: SkillItem[]
): Promise<SlashResult> {
  if (!raw.startsWith("//")) {
    return { handled: false };
  }
  const body = raw.slice(2).trim();
  if (!body) {
    return { handled: true, error: "空命令：示例 `//force-automode on`" };
  }
  const [name, ...args] = body.split(/\s+/);

  // 1) 内置控制命令
  const builtin = builtinRegistry[name];
  if (builtin) {
    try {
      await builtin(args, ctx);
      return { handled: true };
    } catch (e: any) {
      return { handled: true, error: e?.message ?? String(e) };
    }
  }

  // 2) Skill 命令：转化为 user message 让模型自动调 Skill 工具
  //    匹配规则：目录名 (`s.name`) 或 frontmatter alias 任一命中即可
  const skill = skills.find((s) => s.name === name || s.alias === name);
  if (skill) {
    if (!skill.enabled) {
      return {
        handled: true,
        error: `Skill「${name}」已禁用，请到 Skills 面板启用后再用`,
      };
    }
    try {
      // 转发文本：用模型在 SkillTool description 里看到的公开名（alias 优先），
      // 让模型按 `/<skill-name> <args>` 的形式触发 Skill 工具。SKILL.md 的内容由
      // SkillTool 在 tool_result 里回填——这里不内嵌内容，避免重复污染上下文。
      const forwardName = skillDisplayName(skill);
      const forwarded = args.length > 0 ? `/${forwardName} ${args.join(" ")}` : `/${forwardName}`;
      await ctx.sendPrompt(forwarded);
      return { handled: true };
    } catch (e: any) {
      return { handled: true, error: e?.message ?? String(e) };
    }
  }

  // 3) 既不是内置也不是 skill → 提示用户已注册名单（公开名）
  const knownBuiltin = Object.keys(builtinRegistry)
    .map((n) => `//${n}`)
    .join(", ");
  const knownSkills = skills
    .filter((s) => s.enabled)
    .map((s) => `//${skillDisplayName(s)}`)
    .join(", ");
  const hint = knownSkills ? `${knownBuiltin}; skills: ${knownSkills}` : knownBuiltin;
  return {
    handled: true,
    error: `未知命令：//${name}（已注册：${hint}）`,
  };
}
