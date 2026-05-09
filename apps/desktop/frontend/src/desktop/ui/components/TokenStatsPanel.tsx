import { ArrowDownToLine, ArrowUpFromLine, Coins, Database, Layers } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import type { TokenStats } from "@/desktop/ui/types";

interface Props {
  stats: TokenStats | null;
  size?: number;
  className?: string;
}

/**
 * 紧贴输入框右侧的 token 统计图标：默认只显示一个圆形按钮（与 ContextRing 同尺寸），
 * 鼠标悬停时浮出面板展示输入 / 输出 / 缓存命中 / 缓存写入 详情。
 */
export function TokenStatsPanel({ stats, size = 14, className }: Props) {
  const empty = !stats || stats.run_count === 0;
  const hitRate =
    !empty && stats!.input_tokens > 0
      ? Math.round((stats!.cache_read_tokens / stats!.input_tokens) * 100)
      : 0;

  return (
    <div className={cn("relative group/token", className)}>
      <button
        type="button"
        tabIndex={-1}
        className={cn(
          "inline-flex items-center justify-center rounded-lg border border-transparent",
          "text-muted-foreground hover:border-border hover:bg-muted hover:text-foreground transition-colors cursor-default",
          empty && "opacity-60"
        )}
        style={{ width: size + 8, height: size + 8 }}
        aria-label="Token 用量"
      >
        <Coins className="w-3 h-3" />
      </button>

      <div
        className={cn(
          "pointer-events-none absolute bottom-full right-0 mb-2 w-52 z-50",
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
