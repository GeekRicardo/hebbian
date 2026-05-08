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
 * 输入框右侧的圆形上下文进度条。
 * 颜色：< 70% primary，70~90% amber，>= 90% destructive。
 */
export function ContextRing({
  used,
  budget,
  size = 28,
  className,
  onClick,
  title,
}: Props) {
  const ratio = budget > 0 ? Math.min(used / budget, 1.5) : 0;
  const display = Math.min(ratio, 1);
  const pct = Math.round(ratio * 100);

  const stroke = 3;
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
    title ?? `上下文 ${pct}%（${formatTokens(used)} / ${formatTokens(budget)} tokens）`;

  return (
    <button
      type="button"
      onClick={onClick}
      title={tooltip}
      className={cn(
        "relative inline-flex items-center justify-center rounded-full",
        "hover:bg-muted/60 transition-colors",
        onClick ? "cursor-pointer" : "cursor-default",
        className
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
      <span
        className={cn(
          "absolute inset-0 flex items-center justify-center text-[9px] font-medium tabular-nums",
          color
        )}
      >
        {pct}%
      </span>
    </button>
  );
}

function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}
