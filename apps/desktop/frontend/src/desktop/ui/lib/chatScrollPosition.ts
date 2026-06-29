export interface ScrollBoxMetrics {
  scrollHeight: number;
  clientHeight: number;
}

export function stickyBottomScrollTop({ scrollHeight, clientHeight }: ScrollBoxMetrics): number {
  return Math.max(0, scrollHeight - clientHeight);
}

/** 一个滚动锚点：某条消息元素 + 它顶边相对滚动容器视口顶的偏移（重排前快照）。 */
export interface ScrollAnchor {
  /** 锚点消息的 data-message-id。 */
  messageId: string;
  /** 重排前：锚点元素顶边距容器视口顶的像素偏移（可为负，表示略滚出上方）。 */
  offsetFromTop: number;
}

/**
 * 给定重排前的锚点快照与重排后锚点元素的新 offsetTop，算出应恢复的 scrollTop，
 * 使锚点元素回到原来的视觉位置（距容器顶 offsetFromTop 处）。
 *
 * 右侧 sidebar 展开 / 收起改变 chat 宽度 → 长文本重新换行 → 元素 offsetTop 变化。
 * 若不锚定，浏览器按 scrollTop 不变保留，当前看的内容就跳走。这里把锚点钉回原位。
 *
 * 结果 clamp 到 [0, maxScrollTop]，避免越界。
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
