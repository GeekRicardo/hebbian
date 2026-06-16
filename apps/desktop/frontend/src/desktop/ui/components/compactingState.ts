/**
 * 压缩态按会话隔离的判定纯函数。
 *
 * 背景：压缩要调一次 LLM、耗时数秒到数十秒。曾经压缩态是全局单标志，
 * 且复用了输入框的本地 `sending`——压缩 A 会话时切到 B，B 的发送按钮被
 * 残留态禁用。修复后压缩态以「正在压缩的会话 id」承载，每个输入框只关心
 * 「当前这个会话是否正在压缩」。
 */

/** 当前会话是否正在压缩。`compactingSessionId` 为 null 表示没有会话在压缩。 */
export function isSessionCompacting(
  compactingSessionId: string | null,
  currentSessionId: string | null,
): boolean {
  return compactingSessionId !== null && compactingSessionId === currentSessionId;
}

/**
 * 压缩结束后是否应把结果回填到「当前显示的会话」。
 * 压缩耗时里用户可能切走，只有仍停留在发起压缩的会话时才回填，
 * 否则会把这个会话的数据错误覆盖到当前显示的另一个会话上。
 */
export function shouldApplyCompactionResult(
  startedSessionId: string,
  currentSessionId: string | null,
): boolean {
  return currentSessionId === startedSessionId;
}
