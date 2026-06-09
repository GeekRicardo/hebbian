import type { CatalogEntry } from "@/desktop/ui/types";
import { cn } from "@/desktop/ui/lib/utils";
import { Check, Brain, Wrench, Image, FileText, Music, Video } from "lucide-react";

interface ModelCardProps {
  /** 模型 ID（如 "claude-sonnet-4"） */
  modelId: string;
  /** models.dev 元数据（可能不存在） */
  entry?: CatalogEntry;
  /** 用户手动设置的 context window */
  contextOverride?: number;
  /** 是否已选中 */
  selected: boolean;
  /** 点击事件 */
  onClick: () => void;
  /** context 输入框变化 */
  onContextChange: (value: string) => void;
}

/**
 * 模型卡片（单行布局）：展示模型名、能力徽章、输出上限，并允许手动设置上下文窗口。
 * 用于 ProvidersPane 的模型列表。
 *
 * 优先级：用户设置 > models.dev entry > 默认 200K
 */
export function ModelCard({
  modelId,
  entry,
  contextOverride,
  selected,
  onClick,
  onContextChange,
}: ModelCardProps) {
  const displayName = entry?.name || modelId;
  const reasoning = entry?.reasoning;
  const toolCall = entry?.tool_call;
  const inputModalities = entry?.modalities?.input || [];

  const contextValue =
    contextOverride != null
      ? formatTokens(contextOverride)
      : entry?.limit?.context != null
        ? formatTokens(entry.limit.context)
        : "200K";
  const outputValue = entry?.limit?.output != null ? formatTokens(entry.limit.output) : "64K";
  const hasContextOverride = contextOverride != null;

  return (
    <div
      className={cn(
        "flex items-center gap-3 px-3 py-2 rounded-lg border transition-all cursor-pointer",
        selected
          ? "border-primary bg-primary/5 ring-1 ring-primary/20"
          : "border-border hover:border-primary/50 hover:bg-accent/30"
      )}
      onClick={onClick}
    >
      {/* 选中勾选框 */}
      <div
        className={cn(
          "w-4 h-4 rounded border flex items-center justify-center shrink-0",
          selected
            ? "border-primary bg-primary text-primary-foreground"
            : "border-muted-foreground/40"
        )}
      >
        {selected && <Check className="w-3 h-3" />}
      </div>

      {/* 模型名称 + 徽章 */}
      <div className="flex items-center gap-2 min-w-0 flex-1">
        <span className="text-sm font-medium truncate" title={displayName}>
          {displayName}
        </span>
        {/* 模态徽章 */}
        {inputModalities.length > 0 && (
          <span className="flex items-center gap-0.5 shrink-0">
            {inputModalities.includes("image") && (
              <Image className="w-3 h-3 text-green-600" />
            )}
            {inputModalities.includes("audio") && (
              <Music className="w-3 h-3 text-orange-600" />
            )}
            {inputModalities.includes("video") && (
              <Video className="w-3 h-3 text-pink-600" />
            )}
            {inputModalities.includes("pdf") && (
              <FileText className="w-3 h-3 text-red-600" />
            )}
          </span>
        )}
        {/* 能力徽章 */}
        {reasoning && <Brain className="w-3 h-3 text-purple-600 shrink-0" />}
        {toolCall && <Wrench className="w-3 h-3 text-blue-600 shrink-0" />}
      </div>

      {/* context / output 输入框 */}
      <div className="flex items-center gap-2 shrink-0" onClick={(e) => e.stopPropagation()}>
        <label className="flex items-center gap-1 text-xs text-muted-foreground">
          <span>上下文</span>
          <input
            type="text"
            value={contextValue}
            onChange={(e) => onContextChange(e.target.value)}
            className={cn(
              "w-16 px-1.5 py-0.5 text-xs rounded border bg-background",
              hasContextOverride ? "border-primary/50" : "border-border"
            )}
            placeholder="200K"
          />
        </label>
        <span className="text-xs text-muted-foreground">输出 {outputValue}</span>
      </div>
    </div>
  );
}

/**
 * 格式化 token 数为人类可读的短格式（如 200000 → "200K"）。
 */
function formatTokens(n: number): string {
  if (n >= 1_000_000) {
    const v = n / 1_000_000;
    return v % 1 === 0 ? `${v}M` : `${v.toFixed(1)}M`;
  }
  if (n >= 1_000) {
    const v = n / 1_000;
    return v % 1 === 0 ? `${v}K` : `${v.toFixed(1)}K`;
  }
  return n.toString();
}
