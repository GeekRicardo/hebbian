import { api } from "@/desktop/bridge/tauri";
import type { DiffPayload } from "@/desktop/ui/types";

/**
 * 某次 Run 某文件 diff 的进程内缓存：按 `sid:runId:path` memo 化 Promise。
 *
 * 修改文件栏的 +/- 徽章与中间编辑区的 Monaco DiffEditor 都要这份 before/after，
 * 共用一次请求避免双拉。edits-worktree 的快照按 Run 定格、内容不变，缓存可长期有效。
 */
const cache = new Map<string, Promise<DiffPayload>>();

const cacheKey = (sessionId: string, runId: string, path: string) =>
  `${sessionId}\u0000${runId}\u0000${path}`;

export function fetchEditDiff(
  sessionId: string,
  runId: string,
  path: string,
): Promise<DiffPayload> {
  const key = cacheKey(sessionId, runId, path);
  let pending = cache.get(key);
  if (!pending) {
    pending = api.diffEdit(sessionId, runId, path).catch((e) => {
      // 失败不缓存：下次重试还能拉
      cache.delete(key);
      throw e;
    });
    cache.set(key, pending);
  }
  return pending;
}