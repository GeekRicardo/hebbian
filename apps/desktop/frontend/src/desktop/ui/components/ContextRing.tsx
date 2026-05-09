import { cn } from "@/desktop/ui/lib/utils";

interface Props {
  used: number;
  budget: number;
  size?: number;
  className?: string;
  onClick?: () => void;
  title?: string;
}

/**
 * 紧贴输入框右侧的圆形上下文进度环。
 *
 * - 默认：只渲染进度环 svg，**无外框**，跟输入框 box 视觉融为一体
 * - hover：浮出大圆角方形描边背景；并通过 group/contextring 让 tooltip 浮层显示百分比
 * - 点击触发 /compact
 *
 * 颜色阈值：< 70% primary，70~90% amber，>= 90% destructive。
 */
export function ContextRing({
  used,
  budget,
  size = 14,
  className,
  onClick,
  title,
}: Props) {
  const ratio = budget > 0 ? Math.min(used / budget, 1.5) : 0;
  const display = Math.min(ratio, 1);
  const pct = Math.round(ratio * 100);

  const stroke = 2.5;
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const dash = c * display;

  const color =
    pct >= 90
      ? "text-destructive"
      : pct >= 70
        ? "text-amber-500"
        : "text-primary";

  const tooltip =
    title ?? `上下文 ${pct}% · ${formatTokens(used)} / ${formatTokens(budget)}`;

  return (
    <div className={cn("relative group/ring", className)}>
      <button
        type="button"
        onClick={onClick}
        aria-label={tooltip}
        className={cn(
          "inline-flex items-center justify-center rounded-lg border border-transparent",
          "hover:border-border hover:bg-muted transition-colors",
          onClick ? "cursor-pointer" : "cursor-default"
        )}
        style={{ width: size + 8, height: size + 8 }}
      >
        <svg
          width={size}
          height={size}
          viewBox={`0 0 ${size} ${size}`}
          className={cn("rotate-[-90deg]", color)}
        >
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            fill="none"
            strokeWidth={stroke}
            className="stroke-muted"
          />
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            fill="none"
            strokeWidth={stroke}
            strokeLinecap="round"
            strokeDasharray={`${dash} ${c}`}
            className="stroke-current transition-[stroke-dasharray] duration-300"
          />
        </svg>
      </button>
      <div
        className={cn(
          "pointer-events-none absolute bottom-full right-0 mb-1 whitespace-nowrap z-50",
          "rounded-md border border-border bg-background text-foreground shadow-lg",
          "px-2 py-1 text-[11px] tabular-nums select-none",
          "opacity-0 translate-y-1 transition-all duration-150",
          "group-hover/ring:opacity-100 group-hover/ring:translate-y-0"
        )}
      >
        {tooltip}
      </div>
    </div>
  );
}

function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}
