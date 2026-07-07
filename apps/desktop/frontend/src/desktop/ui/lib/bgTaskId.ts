/**
 * 从 Bash 工具的 result 文本里提取后台 task_id（`bash_NNN`）。
 *
 * 兼容两种返回文案：
 * - 当前：`[bash_001] 已在后台启动` / `[bash_001] 60s 内未结束，已转后台`
 * - 旧版：`task_id=bash_001 cmd=...`
 *
 * chat 区工具卡片（MessageBubble）与右侧 sidebar（BackgroundTaskPanel）共用同一份，
 * 避免两处各写一套正则导致一处匹配、一处漏匹配（曾因 `\]\s+后台` 要求 `]` 后紧跟
 * “后台”二字而对 `] 已在后台启动` 全部 NO MATCH）。
 */
const BASH_TASK_ID_RE = /(?:task_id=|\[)(bash_\d+)/;
const SUBAGENT_TASK_ID_RE = /task_id=(subagent-[\w-]+)/;

export function extractBgTaskId(result: string | null | undefined): string | null {
  if (!result) return null;
  const m = result.match(BASH_TASK_ID_RE);
  return m ? m[1] : null;
}

export function extractSubagentTaskId(result: string | null | undefined): string | null {
  if (!result) return null;
  const m = result.match(SUBAGENT_TASK_ID_RE);
  return m ? m[1] : null;
}
