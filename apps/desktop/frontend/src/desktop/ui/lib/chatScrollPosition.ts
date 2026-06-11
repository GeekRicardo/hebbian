export interface ScrollBoxMetrics {
  scrollHeight: number;
  clientHeight: number;
}

export function stickyBottomScrollTop({ scrollHeight, clientHeight }: ScrollBoxMetrics): number {
  return Math.max(0, scrollHeight - clientHeight);
}
