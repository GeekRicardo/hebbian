import { useEffect, useRef, useState } from "react";
import { Wallet, Zap } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import { api } from "@/desktop/bridge/tauri";
import type {
  ClaudeUsageInfo,
  DeepSeekBalanceInfo,
  Provider,
  ProviderUsageResult,
  TokenStats,
  UsageProgress,
} from "@/desktop/ui/types";

const REFRESH_MS = 3 * 60 * 1000; // 3 分钟轮询

// DeepSeek 官方定价（2026-06）：按 CNY 计，cache miss 价格，单位 元/M token
const DS_PRICE: Record<string, { input: number; output: number; cacheRead: number }> = {
  default: { input: 2.0,  output: 8.0,  cacheRead: 0.5  }, // deepseek-v3/chat 系列
  reason:  { input: 4.0,  output: 16.0, cacheRead: 1.0  }, // deepseek-r1/reasoner 系列
};

function getDeepSeekPrice(model: string) {
  const lower = model.toLowerCase();
  if (lower.includes("reasoner") || lower.includes("-r1")) return DS_PRICE.reason;
  return DS_PRICE.default;
}

/** 把 token 数量 × 单价折算成 CNY，返回 "¥X.XXXX" 格式字符串。 */
function calcCostCNY(stats: TokenStats, model: string): string {
  const p = getDeepSeekPrice(model);
  const M = 1_000_000;
  const cost =
    (stats.input_tokens - stats.cache_read_tokens) * (p.input / M) +
    stats.cache_read_tokens * (p.cacheRead / M) +
    stats.output_tokens * (p.output / M);
  if (cost < 0.0001) return "¥0.00";
  if (cost < 0.01) return `¥${cost.toFixed(4)}`;
  return `¥${cost.toFixed(2)}`;
}

// ---------- Claude usage 渲染 ----------

function claudeProgressLabel(p: UsageProgress | null | undefined): string {
  if (!p) return "";
  return `${Math.round(p.utilization)}%`;
}

function claudeProgressColor(pct: number): string {
  if (pct >= 90) return "text-destructive";
  if (pct >= 70) return "text-amber-500";
  return "text-emerald-500";
}

/** 距额度刷新还剩多久：3d5h / 5h30m / 45m / <1m。 */
function formatRemaining(sec: number): string {
  if (sec <= 0) return "即将";
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  if (d > 0) return `${d}d${h > 0 ? `${h}h` : ""}`;
  if (h > 0) return `${h}h${m > 0 ? `${m}m` : ""}`;
  if (m > 0) return `${m}m`;
  return "<1m";
}

function ClaudeTooltip({ info }: { info: ClaudeUsageInfo }) {
  const rows: { label: string; pct: number; remaining: number }[] = [];
  if (info.five_hour) {
    rows.push({
      label: "5 小时窗口",
      pct: Math.round(info.five_hour.utilization),
      remaining: info.five_hour.remaining_seconds,
    });
  }
  if (info.seven_day) {
    rows.push({
      label: "7 天窗口",
      pct: Math.round(info.seven_day.utilization),
      remaining: info.seven_day.remaining_seconds,
    });
  }
  if (info.seven_day_sonnet) {
    rows.push({
      label: "7 天 Sonnet",
      pct: Math.round(info.seven_day_sonnet.utilization),
      remaining: info.seven_day_sonnet.remaining_seconds,
    });
  }
  return (
    <div className="flex flex-col gap-1.5 text-[11px]">
      <div className="flex items-center justify-between gap-4">
        <span className="font-medium text-foreground/80">Claude 用量</span>
        {info.plan && (
          <span className="rounded bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium leading-none text-primary">
            {info.plan}
          </span>
        )}
      </div>
      {rows.map((r) => (
        <div key={r.label} className="flex items-center justify-between gap-4">
          <span className="text-muted-foreground">{r.label}</span>
          <span className="flex items-baseline gap-2">
            {r.remaining > 0 && (
              <span className="tabular-nums text-[10px] text-muted-foreground/60">
                {formatRemaining(r.remaining)}后刷新
              </span>
            )}
            <span className={cn("tabular-nums font-medium", claudeProgressColor(r.pct))}>
              {r.pct}%
            </span>
          </span>
        </div>
      ))}
      {rows.length === 0 && (
        <span className="text-muted-foreground/70">暂无数据</span>
      )}
      {info.email && (
        <div className="mt-1 border-t border-border pt-1 text-muted-foreground/80 truncate">
          {info.email}
        </div>
      )}
    </div>
  );
}

// ---------- DeepSeek balance 渲染 ----------

function primaryBalance(balances: DeepSeekBalanceInfo): string {
  if (!balances.available) return "不可用";
  const cny = balances.entries.find((e) => e.currency.toUpperCase() === "CNY");
  const entry = cny ?? balances.entries[0];
  if (!entry) return "";
  const sym = entry.currency.toUpperCase() === "CNY" ? "¥" : "$";
  return `${sym}${parseFloat(entry.total_balance).toFixed(2)}`;
}

function DeepSeekTooltip({
  balances,
  sessionCost,
}: {
  balances: DeepSeekBalanceInfo;
  sessionCost: string | null;
}) {
  return (
    <div className="flex flex-col gap-1.5 text-[11px]">
      <span className="font-medium text-foreground/80">DeepSeek 账户</span>
      {balances.entries.map((e) => {
        const sym = e.currency.toUpperCase() === "CNY" ? "¥" : "$";
        return (
          <div key={e.currency} className="flex items-center justify-between gap-4">
            <span className="text-muted-foreground">余额</span>
            <span className="tabular-nums text-foreground/80">
              {sym}{parseFloat(e.total_balance).toFixed(4)}
            </span>
          </div>
        );
      })}
      {!balances.available && (
        <span className="text-destructive">账户不可用</span>
      )}
      {sessionCost && (
        <div className="mt-0.5 border-t border-border pt-1 flex items-center justify-between gap-4">
          <span className="text-muted-foreground">本次对话</span>
          <span className="tabular-nums text-foreground/80">~{sessionCost}</span>
        </div>
      )}
    </div>
  );
}

// ---------- 主组件 ----------

interface Props {
  provider: Provider | null;
  tokenStats: TokenStats | null;
  model: string;
  className?: string;
}

export function ProviderUsageIndicator({ provider, tokenStats, model, className }: Props) {
  const [result, setResult] = useState<ProviderUsageResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  async function doFetch() {
    if (!provider) return;
    abortRef.current?.abort();
    const ctrl = new AbortController();
    abortRef.current = ctrl;
    try {
      const res = await api.fetchProviderUsage(provider.id);
      if (!ctrl.signal.aborted) {
        setResult(res);
        setError(null);
      }
    } catch (e: any) {
      if (!ctrl.signal.aborted) {
        setError(e?.message ?? String(e));
      }
    }
  }

  useEffect(() => {
    if (!provider) {
      setResult(null);
      setError(null);
      return;
    }
    void doFetch();
    timerRef.current = setInterval(doFetch, REFRESH_MS);
    return () => {
      abortRef.current?.abort();
      if (timerRef.current) clearInterval(timerRef.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider?.id]);

  // 不支持的 provider 或未加载时不渲染
  if (!provider || !result || result.kind === "unsupported") return null;

  const sessionCost =
    result.kind === "deepseek" && tokenStats && tokenStats.run_count > 0
      ? calcCostCNY(tokenStats, model)
      : null;

  // ---- Claude ----
  if (result.kind === "claude") {
    const p5h = result.five_hour;
    const pct = p5h ? Math.round(p5h.utilization) : null;
    const label = pct !== null ? `${pct}%` : claudeProgressLabel(result.seven_day);
    const color = pct !== null ? claudeProgressColor(pct) : "text-muted-foreground";

    return (
      <div className={cn("relative group/usage", className)}>
        <button
          type="button"
          tabIndex={-1}
          className="inline-flex h-7 items-center gap-1 rounded-md border border-transparent px-1.5 text-muted-foreground hover:bg-muted hover:text-foreground transition-colors cursor-default"
          aria-label={`Claude 用量 ${label}${result.plan ? ` · ${result.plan}` : ""}`}
        >
          <Zap className={cn("w-3 h-3", color)} />
          <span className={cn("text-[10px] tabular-nums leading-none", color)}>{label}</span>
          {result.plan && (
            <span className="text-[10px] leading-none text-muted-foreground/70">
              {result.plan}
            </span>
          )}
        </button>
        <div
          className={cn(
            "pointer-events-none absolute bottom-full right-0 mb-2 w-52 z-50",
            "rounded-lg border border-border bg-background text-foreground shadow-lg",
            "px-3 py-2 select-none",
            "opacity-0 translate-y-1 transition-all duration-150",
            "group-hover/usage:opacity-100 group-hover/usage:translate-y-0 group-hover/usage:pointer-events-auto"
          )}
        >
          {error ? (
            <span className="text-[11px] text-destructive">{error}</span>
          ) : (
            <ClaudeTooltip info={result} />
          )}
        </div>
      </div>
    );
  }

  // ---- DeepSeek ----
  if (result.kind === "deepseek") {
    const available = result.balances.available;

    return (
      <div className={cn("relative group/usage", className)}>
        <button
          type="button"
          tabIndex={-1}
          className="inline-flex h-7 items-center rounded-md border border-transparent px-1.5 text-muted-foreground hover:bg-muted hover:text-foreground transition-colors cursor-default"
          aria-label="DeepSeek 账户"
        >
          <Wallet className={cn("w-3 h-3", available ? "text-emerald-500" : "text-destructive")} />
        </button>
        <div
          className={cn(
            "pointer-events-none absolute bottom-full right-0 mb-2 w-52 z-50",
            "rounded-lg border border-border bg-background text-foreground shadow-lg",
            "px-3 py-2 select-none",
            "opacity-0 translate-y-1 transition-all duration-150",
            "group-hover/usage:opacity-100 group-hover/usage:translate-y-0 group-hover/usage:pointer-events-auto"
          )}
        >
          {error ? (
            <span className="text-[11px] text-destructive">{error}</span>
          ) : (
            <DeepSeekTooltip balances={result.balances} sessionCost={sessionCost} />
          )}
        </div>
      </div>
    );
  }

  return null;
}
