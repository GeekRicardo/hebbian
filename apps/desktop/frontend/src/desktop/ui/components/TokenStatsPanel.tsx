import { ArrowDownToLine, ArrowUpFromLine, Database, Layers } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import type { ContextUsage, TokenStats } from "@/desktop/ui/types";

interface Props {
  stats: TokenStats | null;
  contextUsage?: ContextUsage | null;
  size?: number;
  className?: string;
  onCompact?: () => void;
}

/**
 * 输入框右侧的合并状态：用上下文进度圆环承载 context 百分比，旁边显示 cache 命中率。
 */
export function TokenStatsPanel({ stats, contextUsage, size = 18, className, onCompact }: Props) {
  const empty = !stats || stats.run_count === 0;
  const hitRate =
    !empty && stats!.input_tokens > 0
      ? Math.round((stats!.cache_read_tokens / stats!.input_tokens) * 100)
      : 0;
  const contextRatio = contextUsage && contextUsage.budget_tokens > 0
    ? Math.min(contextUsage.used_tokens / contextUsage.budget_tokens, 1.5)
    : 0;
  const contextDisplay = Math.min(contextRatio, 1);
  const contextPct = Math.round(contextRatio * 100);
  const stroke = 2.5;
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const dash = c * contextDisplay;
  const color = contextPct >= 90
    ? "text-destructive"
    : contextPct >= 70
      ? "text-amber-500"
      : "text-primary";
  const title = contextUsage
    ? `缓存 ${hitRate}% · 上下文 ${contextPct}%`
    : `缓存 ${hitRate}%`;

  return (
    <div className={cn("relative group/token", className)}>
      <button
        type="button"
        tabIndex={-1}
        onClick={onCompact}
        className={cn(
          "token-stats-trigger inline-flex h-7 items-center gap-1.5 rounded-md border border-transparent px-1.5",
          "text-muted-foreground hover:bg-muted hover:text-foreground transition-colors leading-none",
          onCompact ? "cursor-pointer" : "cursor-default",
          empty && !contextUsage && "opacity-60"
        )}
        aria-label={title}
      >
        <span className="relative inline-flex shrink-0 items-center justify-center" style={{ width: size, height: size }}>
          <svg
            width={size}
            height={size}
            viewBox={`0 0 ${size} ${size}`}
            className={cn("token-stats-ring rotate-[-90deg]", color)}
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
          <span className="absolute text-[7px] font-semibold leading-none tabular-nums text-foreground/70">
            {contextUsage ? contextPct : "–"}
          </span>
        </span>
        <span className="token-stats-label inline-flex items-center gap-1 text-[10px] tabular-nums leading-none">
          <span>cache {hitRate}%</span>
          <span className="text-muted-foreground/50">/</span>
          <span>ctx {contextUsage ? contextPct : 0}%</span>
        </span>
      </button>

      <div
        className={cn(
          "pointer-events-none absolute bottom-full right-0 mb-2 w-56 z-50",
          "rounded-lg border border-border bg-background text-foreground shadow-lg",
          "px-3 py-2 text-[11px] select-none",
          "opacity-0 translate-y-1 transition-all duration-150",
          "group-hover/token:opacity-100 group-hover/token:translate-y-0 group-hover/token:pointer-events-auto"
        )}
      >
        {empty ? (
          <div className="text-muted-foreground/80">尚未发起请求</div>
        ) : (
          <>
            <div className="flex items-center justify-between mb-1.5">
              <span className="font-medium text-foreground/80">Token 用量</span>
              <span className="text-muted-foreground tabular-nums">
                ×{stats!.run_count}
              </span>
            </div>
            <Row
              icon={<ArrowUpFromLine className="w-3 h-3" />}
              label="输入"
              value={stats!.input_tokens}
              tone="text-foreground/80"
            />
            <Row
              icon={<ArrowDownToLine className="w-3 h-3" />}
              label="输出"
              value={stats!.output_tokens}
              tone="text-foreground/80"
            />
            <Row
              icon={<Database className="w-3 h-3" />}
              label="缓存命中"
              value={stats!.cache_read_tokens}
              tone="text-emerald-600 dark:text-emerald-400"
              suffix={hitRate > 0 ? ` (${hitRate}%)` : undefined}
            />
            {stats!.cache_creation_tokens > 0 && (
              <Row
                icon={<Layers className="w-3 h-3" />}
                label="缓存写入"
                value={stats!.cache_creation_tokens}
                tone="text-amber-600 dark:text-amber-400"
              />
            )}
          </>
        )}
        {contextUsage && (
          <div className="mt-2 border-t border-border pt-1.5 text-muted-foreground">
            上下文 <span className="tabular-nums text-foreground/80">{contextPct}%</span>
            <span className="ml-1 tabular-nums">
              {formatTokens(contextUsage.used_tokens)} / {formatTokens(contextUsage.budget_tokens)}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}

function Row({
  icon,
  label,
  value,
  tone,
  suffix,
}: {
  icon: React.ReactNode;
  label: string;
  value: number;
  tone: string;
  suffix?: string;
}) {
  return (
    <div className="flex items-center justify-between gap-2 py-0.5">
      <span className="inline-flex items-center gap-1.5 text-muted-foreground">
        {icon}
        {label}
      </span>
      <span className={cn("tabular-nums", tone)}>
        {formatTokens(value)}
        {suffix}
      </span>
    </div>
  );
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}
