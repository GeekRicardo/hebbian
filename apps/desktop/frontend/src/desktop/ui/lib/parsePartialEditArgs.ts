/**
 * 架构 §4.13.8：Edit/Write 工具的流式参数解析。
 *
 * 模型在 stream 阶段通过 ToolCallDelta { arguments_delta } 逐 token 吐 JSON，
 * 此时还不能 JSON.parse。本函数容错地扫出 string 类型字段的「已收部分」，
 * 让 DiffViewer 立即把 `old_string` / `new_string` / `content` / `file_path` 渲出来。
 *
 * 设计取舍：
 * - 只识别顶层对象的若干已知 string 字段；不递归、不处理嵌套对象。
 * - 字符串字段未闭合时也返回已收的部分内容（带 \n 等转义还原），让 UI 渲到光标位置。
 * - 解析失败（明显坏 JSON）整体返回 {}，由调用方决定渲不渲。
 */

const TARGET_KEYS = ["file_path", "old_string", "new_string", "content"] as const;
type TargetKey = (typeof TARGET_KEYS)[number];

export interface PartialEditArgs {
  file_path?: string;
  old_string?: string;
  new_string?: string;
  content?: string;
}

export function parsePartialEditArgs(text: string): PartialEditArgs {
  const out: PartialEditArgs = {};
  if (!text) return out;

  // 跳过开头空白和可能的 `{`
  let i = 0;
  while (i < text.length && /\s/.test(text[i])) i++;
  if (text[i] === "{") i++;

  while (i < text.length) {
    // 跳过空白、逗号
    while (i < text.length && /[\s,]/.test(text[i])) i++;
    if (text[i] !== '"') break;
    i++;

    // 读 key
    const keyStart = i;
    while (i < text.length && text[i] !== '"') {
      if (text[i] === "\\") i++;
      i++;
    }
    if (i >= text.length) break;
    const key = text.slice(keyStart, i);
    i++;

    // 跳过 : 与空白
    while (i < text.length && /\s/.test(text[i])) i++;
    if (text[i] !== ":") break;
    i++;
    while (i < text.length && /\s/.test(text[i])) i++;

    // 值：只关心 string 字段；其它类型直接跳过
    if (text[i] === '"') {
      i++;
      const [value, nextI, closed] = readStringValue(text, i);
      if (TARGET_KEYS.includes(key as TargetKey)) {
        (out as Record<string, string>)[key] = value;
      }
      i = nextI;
      // 未闭合 → 已经到流末，结束
      if (!closed) break;
    } else {
      // 非 string 值（number / bool / object / array）：试图跳到下一个逗号或 `}` 层级
      i = skipNonStringValue(text, i);
      if (i < 0) break;
    }
  }

  return out;
}

/**
 * 从开引号之后开始读 JSON string 内容，处理转义。
 * 返回 [unescaped, nextIndex, closed]：
 * - closed = true 表示遇到了配对的 `"`，nextIndex 指向其后第一个字符
 * - closed = false 表示流到这里就断了，把已读字符当成「已收部分」返回
 */
function readStringValue(s: string, start: number): [string, number, boolean] {
  let out = "";
  let i = start;
  while (i < s.length) {
    const ch = s[i];
    if (ch === '"') {
      return [out, i + 1, true];
    }
    if (ch === "\\") {
      i++;
      if (i >= s.length) {
        // 转义符也没收完：把前面那个 `\` 吞掉，返回到这里为止
        return [out, i, false];
      }
      const esc = s[i];
      switch (esc) {
        case '"':
          out += '"';
          break;
        case "\\":
          out += "\\";
          break;
        case "/":
          out += "/";
          break;
        case "n":
          out += "\n";
          break;
        case "r":
          out += "\r";
          break;
        case "t":
          out += "\t";
          break;
        case "b":
          out += "\b";
          break;
        case "f":
          out += "\f";
          break;
        case "u": {
          if (i + 4 < s.length) {
            const hex = s.slice(i + 1, i + 5);
            if (/^[0-9a-fA-F]{4}$/.test(hex)) {
              out += String.fromCharCode(parseInt(hex, 16));
              i += 4;
            } else {
              out += "\\u";
            }
          } else {
            // \uXXXX 没收完：到这截断
            return [out, s.length, false];
          }
          break;
        }
        default:
          out += esc;
      }
      i++;
      continue;
    }
    out += ch;
    i++;
  }
  return [out, i, false];
}

/**
 * 跳过非 string 类型的值。简单做：扫到当前对象层级的下一个逗号或 `}` 之前。
 * 处理嵌套 {} / [] 配对，但不处理深层 string 内的特殊字符（够用即可）。
 */
function skipNonStringValue(s: string, start: number): number {
  let depth = 0;
  let inStr = false;
  let escape = false;
  for (let i = start; i < s.length; i++) {
    const ch = s[i];
    if (inStr) {
      if (escape) {
        escape = false;
      } else if (ch === "\\") {
        escape = true;
      } else if (ch === '"') {
        inStr = false;
      }
      continue;
    }
    if (ch === '"') {
      inStr = true;
    } else if (ch === "{" || ch === "[") {
      depth++;
    } else if (ch === "}" || ch === "]") {
      if (depth === 0) return i;
      depth--;
    } else if (ch === "," && depth === 0) {
      return i;
    }
  }
  return -1;
}

/**
 * 推断 Edit/Write 工具的 action 标签，与 EditAction 对齐。
 * 仅作 UI 展示，权威值在 EditEntry.action（落盘时由后端决定）。
 */
export function inferDiffAction(
  toolName: string | null | undefined,
  args: PartialEditArgs,
): "create" | "overwrite" | "modify" {
  if (toolName === "Write") return "overwrite";
  // Edit：old_string 完全为空 → create；否则 modify
  if (!args.old_string) return "create";
  return "modify";
}

/**
 * 提取 DiffViewer 的两端文本。
 *
 * - Edit: before = old_string, after = new_string
 * - Write: before = "", after = content
 *
 * 这是架构 §4.13.8 流式 diff 的语义：参数本身就是 diff 的两端，
 * 不读磁盘文件。
 */
export function diffSidesFromArgs(
  toolName: string | null | undefined,
  args: PartialEditArgs,
): { beforeText: string; afterText: string } {
  if (toolName === "Write") {
    return { beforeText: "", afterText: args.content ?? "" };
  }
  // 其它（包括 Edit / 未知 name）按 Edit 处理
  return {
    beforeText: args.old_string ?? "",
    afterText: args.new_string ?? "",
  };
}
