export interface ScrollBoxMetrics {
  scrollHeight: number;
  clientHeight: number;
}

export function stickyBottomScrollTop({ scrollHeight, clientHeight }: ScrollBoxMetrics): number {
  return Math.max(0, scrollHeight - clientHeight);
}

/**
 * 一个滚动锚点（顶边锚定）：某条消息元素 + 它**顶边**相对滚动容器视口**顶部**的偏移。
 *
 * 锚「顶边距视口顶」是行业标准 scroll anchoring（浏览器原生 / VSCode 都这么做）：保持视口
 * 顶部那条内容在重排后不动。对超长消息（比视口还高）同样正确——视线落点稳定；底边锚定则会
 * 把长消息底部拉到固定位、反而让正看的中部抖。
 */
export interface ScrollAnchor {
  /** 锚点消息的 data-message-id。 */
  messageId: string;
  /** 重排前：锚点元素顶边距容器视口顶的像素偏移（可为负，表示顶边略滚出视口上方）。 */
  offsetFromTop: number;
}

/**
 * 给定重排前的锚点快照与重排后锚点元素的新 offsetTop，算出应恢复的 scrollTop，使锚点元素
 * 顶边回到原视觉位置（距容器视口顶 offsetFromTop）。
 *   元素顶边距视口顶 = offsetTop - scrollTop，令其 = 快照 offsetFromTop：
 *   scrollTop = offsetTop - offsetFromTop
 * 结果 clamp 到 [0, maxScrollTop]。
 */
export function anchorScrollTop(
  anchor: ScrollAnchor,
  newAnchorOffsetTop: number,
  metrics: ScrollBoxMetrics,
): number {
  const target = newAnchorOffsetTop - anchor.offsetFromTop;
  const max = stickyBottomScrollTop(metrics);
  return Math.max(0, Math.min(target, max));
}
