/**
 * 把焦点送到 chat 区域里某个 tool_call 卡片上：自动滚到视图中央、展开折叠态、
 * 闪一下边框作为视觉确认。
 *
 * 流程：
 *  1. 派发 `focus-tool-call` 自定义事件，所有 `ToolCallTimeline` 实例都在监听
 *  2. 拥有这个 call 的 timeline 看到匹配的 id，会把对应 key 加进 expandedToolCalls
 *  3. 给 React 两帧时间完成 expand 渲染，再 querySelector 抓 DOM 节点
 *  4. `scrollIntoView` + 加 `focus-flash` class（800ms 自动移除）
 *
 * 为什么用 DOM + CustomEvent 而不是 zustand：tool_call 的 `expandedToolCalls`
 * state 是 per-MessageBubble 局部 state，要外置成全局会牵动好几个传参链；
 * 自定义事件让组件解耦——sidebar 只需要喊「我要这个 callId」，监听端自己决定要不要响应。
 */
export const FOCUS_TOOL_CALL_EVENT = "focus-tool-call";

export function focusToolCall(callId: string): void {
  if (!callId) return;
  window.dispatchEvent(
    new CustomEvent<string>(FOCUS_TOOL_CALL_EVENT, { detail: callId })
  );
  // 等两帧让 ToolCallTimeline 收到事件 → setState → 重渲染 → 折叠态展开
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLElement>(
        `[data-tool-call-id="${CSS.escape(callId)}"]`
      );
      if (!el) return;
      el.scrollIntoView({ behavior: "smooth", block: "center" });
      el.classList.add("focus-flash");
      // 动画时长 800ms 后清掉 class，避免 DOM 上堆积
      window.setTimeout(() => el.classList.remove("focus-flash"), 900);
    });
  });
}
