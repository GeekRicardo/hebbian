import type { CatalogEntry } from "@/desktop/ui/types";
import { ModelCard } from "./ModelCard";

interface FamilyGroupProps {
  /** family 名称（如 "claude-sonnet"） */
  family: string;
  /** 该 family 下的所有模型 */
  models: ModelItem[];
  /** 当前选中的模型列表 */
  selectedModels: string[];
  /** 切换模型选中状态 */
  onToggleModel: (modelId: string) => void;
  /** models.dev catalog（用于查找元数据） */
  catalog: Record<string, CatalogEntry> | null;
  /** 用户手动 override 的 context/output */
  overrides: Record<string, { context?: string; output?: string }>;
  /** 更新某个模型的 override */
  onUpdateOverride: (modelId: string, patch: { context?: string; output?: string }) => void;
}

export interface ModelItem {
  id: string;
  owned_by?: string | null;
}

/**
 * Family 分组：按 family 展示一组模型（单列列表，限定高度内滚动）。
 */
export function FamilyGroup({
  family,
  models,
  selectedModels,
  onToggleModel,
  catalog,
  overrides,
  onUpdateOverride,
}: FamilyGroupProps) {
  // 构建不带前缀的 catalog 映射（如 "anthropic/claude-sonnet-4-5" → "claude-sonnet-4-5"）
  const catalogLookup = (modelId: string): CatalogEntry | undefined => {
    if (!catalog) return undefined;
    // 1. 精确匹配
    if (catalog[modelId]) return catalog[modelId];
    // 2. 尝试带前缀的变体
    for (const [key, value] of Object.entries(catalog)) {
      if (key.endsWith("/" + modelId) || key === modelId) return value;
    }
    return undefined;
  };

  return (
    <div className="space-y-1.5">
      {/* Family 标题 */}
      <div className="flex items-center gap-2 px-1">
        <div className="text-xs font-medium text-muted-foreground">{family}</div>
        <div className="text-[10px] text-muted-foreground/60">
          {models.length}
        </div>
      </div>

      {/* 模型卡片单列列表 */}
      <div className="space-y-1">
        {models.map((model) => (
          <ModelCard
            key={model.id}
            modelId={model.id}
            entry={catalogLookup(model.id)}
            override={overrides[model.id]}
            selected={selectedModels.includes(model.id)}
            onClick={() => onToggleModel(model.id)}
            onContextChange={(value) =>
              onUpdateOverride(model.id, { context: value })
            }
            onOutputChange={(value) =>
              onUpdateOverride(model.id, { output: value })
            }
          />
        ))}
      </div>
    </div>
  );
}
