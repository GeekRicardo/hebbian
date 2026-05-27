import { useEffect, useState } from "react";
import { api } from "@/desktop/bridge/tauri";

/**
 * 计算 `oldString` 在 `originalText` 里的起始行号（1-based）。
 *
 * - `oldString` 为空 / 未匹配上 → 返回 1（DiffViewer 默认行号语义）。
 * - 命中多次：取第一次出现的位置。前端无法消歧，但 agent-core 的
 *   `unique_match` 校验已保证后端真正写盘时 oldString 全局唯一，所以
 *   流式预览阶段第一次出现的位置和真实落点一致。
 */
export function lineOfOldString(originalText: string, oldString: string): number {
  if (!originalText || !oldString) return 1;
  const idx = originalText.indexOf(oldString);
  if (idx < 0) return 1;
  let line = 1;
  for (let i = 0; i < idx; i++) {
    if (originalText.charCodeAt(i) === 10) line++;
  }
  return line;
}

/**
 * 把可能的相对路径解析到绝对路径。
 * - 已是绝对路径（unix `/` 开头 / windows drive `C:\`）→ 原样返回。
 * - 相对路径且有 workdir → `workdir/path`（用 `/` 拼接，跨平台 readTextFile 都吃）。
 * - 否则返回 null（无法解析就跳过读盘）。
 */
function resolvePath(path: string | undefined, workdir: string | null): string | null {
  if (!path) return null;
  if (path.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(path)) return path;
  if (!workdir) return null;
  const sep = workdir.endsWith("/") || workdir.endsWith("\\") ? "" : "/";
  return `${workdir}${sep}${path}`;
}

/**
 * Edit diff 渲染时获取「原文件文本」用于定位 old_string 起始行号。
 *
 * 三种调用点行为：
 * - 流式：file_path / workdir 解析出绝对路径后读一次盘，缓存住，后续 args 字符级
 *   抖动不重读。
 * - 审批 / 非放大 detail：参数已完整，同样读一次盘。
 * - 完整 payload 可用时（放大 detail）调用方不应调本 hook，传 base=1 即可。
 *
 * 读盘失败（路径错 / 文件不存在 / 超 8MiB）→ originalText 保持 null；调用方
 * 应 fallback 到 base=1。
 */
export function useOriginalFileText(
  filePath: string | undefined,
  workdir: string | null,
  enabled: boolean,
): string | null {
  const [text, setText] = useState<string | null>(null);
  const absPath = resolvePath(filePath, workdir);

  useEffect(() => {
    if (!enabled || !absPath) {
      setText(null);
      return;
    }
    let cancelled = false;
    api
      .readTextFile(absPath)
      .then((t) => {
        if (!cancelled) setText(t);
      })
      .catch(() => {
        if (!cancelled) setText(null);
      });
    return () => {
      cancelled = true;
    };
  }, [absPath, enabled]);

  return text;
}
