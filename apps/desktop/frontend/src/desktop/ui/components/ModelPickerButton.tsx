import { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/desktop/ui/lib/utils";
import {
  contextWindowFor,
  formatContextWindow,
} from "@/desktop/ui/lib/contextWindow";
import { useStore } from "@/desktop/ui/store/useStore";
import {
  DEFAULT_REASONING,
  REASONING_EFFORT_LABEL,
  REASONING_EFFORT_ORDER,
  effortDisplay,
  modelExposesLongContextToggle,
  modelSupportsReasoning,
} from "@/desktop/ui/lib/reasoning";
import type {
  Provider,
  ReasoningConfig,
  ReasoningEffort,
} from "@/desktop/ui/types";

function isProviderEnabled(p: Provider) {
  return p.enabled !== false;
}

function ReasoningControls({
  providerKind,
  model,
  reasoning,
  onChange,
}: {
  providerKind: string;
  model: string;
  reasoning: ReasoningConfig;
  onChange: (next: ReasoningConfig) => void;
}) {
  const enabled = reasoning.enabled ?? true;
  const effort: ReasoningEffort = reasoning.effort ?? "extra";
  const longContext = reasoning.long_context ?? false;
  const showLongContext = modelExposesLongContextToggle(providerKind, model);
  const showReasoning = modelSupportsReasoning(providerKind, model);
  return (
    <div
      onClick={(e) => e.stopPropagation()}
      className="px-3 py-2 border-t border-border bg-muted/40 space-y-2"
    >
      {showReasoning && (
        <>
          <label className="flex items-center justify-between text-[11px]">
            <span className="text-muted-foreground">启用 thinking</span>
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => onChange({ ...reasoning, enabled: e.target.checked })}
              className="h-3.5 w-3.5 cursor-pointer accent-primary"
            />
          </label>
          <div className="flex items-center justify-between gap-2 text-[11px]">
            <span
              className="text-muted-foreground shrink-0"
              title={`实际发送：${effortDisplay(providerKind, model, effort)}`}
            >
              思考强度
            </span>
            <div className="inline-flex rounded-md border border-border overflow-hidden">
              {REASONING_EFFORT_ORDER.map((level) => {
                const active = effort === level;
                return (
                  <button
                    key={level}
                    type="button"
                    disabled={!enabled}
                    onClick={() => onChange({ ...reasoning, effort: level })}
                    title={`实际发送：${effortDisplay(providerKind, model, level)}`}
                    className={cn(
                      "px-2 py-0.5 text-[10px] transition-colors",
                      active
                        ? "bg-primary text-primary-foreground"
                        : "bg-background hover:bg-accent",
                      !enabled && "opacity-50 cursor-not-allowed"
                    )}
                  >
                    {REASONING_EFFORT_LABEL[level]}
                  </button>
                );
              })}
            </div>
          </div>
        </>
      )}
      {showLongContext && (
        <label
          className="flex items-center justify-between text-[11px]"
          title="开启后请求会带 anthropic-beta: context-1m-2025-08-07，把 Sonnet/Opus 旧版本上下文从 200k 抬到 1M"
        >
          <span className="text-muted-foreground">1M 上下文</span>
          <input
            type="checkbox"
            checked={longContext}
            onChange={(e) =>
              onChange({ ...reasoning, long_context: e.target.checked })
            }
            className="h-3.5 w-3.5 cursor-pointer accent-primary"
          />
        </label>
      )}
    </div>
  );
}

/**
 * 输入框右下角的模型/供应商选择器。点击展开向上弹出的菜单，
 * 包含按 provider 分组的 model 列表，选中模型时还可调整推理参数。
 *
 * 之前这个 picker 在 ChatView 顶端 header；按用户要求一并搬到输入框内部右下角。
 */
export function ModelPickerButton() {
  const currentSession = useStore((s) => s.currentSession);
  const providersFile = useStore((s) => s.providersFile);
  const switchProviderModel = useStore((s) => s.switchProviderModel);
  const setReasoning = useStore((s) => s.setReasoning);

  const [open, setOpen] = useState(false);
  // 默认展开当前对话所用的 provider；用户可手动展开/折叠其他。
  const [expandedProviderIds, setExpandedProviderIds] = useState<Set<string>>(
    () => new Set(currentSession ? [currentSession.provider_id] : [])
  );

  // 切换会话时，重置展开集合到新的当前 provider
  useEffect(() => {
    if (!currentSession) return;
    setExpandedProviderIds(new Set([currentSession.provider_id]));
  }, [currentSession?.provider_id]);

  useEffect(() => {
    if (!open) return;
    const onClick = () => setOpen(false);
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [open]);

  if (!currentSession) return null;

  const providers = providersFile.providers;
  const enabledProviders = providers.filter(isProviderEnabled);
  const currentProvider = providers.find(
    (p) => p.id === currentSession.provider_id
  );

  async function handleSwitch(providerId: string, model: string) {
    try {
      await switchProviderModel(providerId, model);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  const currentContext = currentProvider
    ? formatContextWindow(
        contextWindowFor(currentProvider.kind, currentSession.model)
      )
    : "";
  const currentTooltip = currentProvider
    ? `${currentProvider.name} · ${currentSession.model}（上下文 ${currentContext}）`
    : "切换模型";

  return (
    <div className="relative">
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => {
            const next = !v;
            if (next) {
              setExpandedProviderIds((ids) => {
                if (ids.has(currentSession.provider_id)) return ids;
                return new Set([...ids, currentSession.provider_id]);
              });
            }
            return next;
          });
        }}
        className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
        title={currentTooltip}
      >
        <span className="truncate max-w-[160px]">{currentSession.model}</span>
        <ChevronDown className="w-3 h-3 opacity-60" />
      </button>
      {open && (
        <div
          onClick={(e) => e.stopPropagation()}
          className="absolute bottom-full right-0 mb-1 w-72 max-h-[60vh] overflow-y-auto rounded-lg border border-border bg-card shadow-lg z-[90] animate-slide-up"
        >
          {enabledProviders.length === 0 && (
            <div className="p-4 text-xs text-muted-foreground text-center">
              没有已启用的供应商
            </div>
          )}
          {enabledProviders.map((p) => {
            const isActiveProvider = p.id === currentSession.provider_id;
            const expanded = expandedProviderIds.has(p.id);
            const models =
              p.models.length > 0
                ? p.models
                : p.default_model
                ? [p.default_model]
                : [];
            return (
              <div key={p.id} className="border-b border-border last:border-b-0">
                <button
                  type="button"
                  onClick={() =>
                    setExpandedProviderIds((ids) => {
                      const next = new Set(ids);
                      if (next.has(p.id)) next.delete(p.id);
                      else next.add(p.id);
                      return next;
                    })
                  }
                  className="w-full px-3 py-1.5 text-[11px] font-semibold text-foreground bg-muted hover:bg-accent flex items-center justify-between transition-colors"
                >
                  <span>{p.name}</span>
                  <span className="inline-flex items-center gap-1 text-[10px] text-muted-foreground uppercase">
                    {p.kind}
                    <ChevronDown
                      className={cn(
                        "h-3 w-3 transition-transform",
                        !expanded && "-rotate-90"
                      )}
                    />
                  </span>
                </button>
                {expanded && models.length === 0 && (
                  <div className="px-3 py-2 text-xs text-muted-foreground italic">
                    （无模型）
                  </div>
                )}
                {expanded && models.length > 0 && (
                  <div>
                    {models.map((m) => {
                      const act =
                        isActiveProvider && m === currentSession.model;
                      const showControls =
                        act &&
                        (modelSupportsReasoning(p.kind, m) ||
                          modelExposesLongContextToggle(p.kind, m));
                      const ctx = formatContextWindow(
                        contextWindowFor(p.kind, m)
                      );
                      return (
                        <div key={`${p.id}-${m}`}>
                          <button
                            onClick={() => handleSwitch(p.id, m)}
                            title={`${p.name} · ${m}（上下文 ${ctx}）`}
                            className={cn(
                              "w-full text-left px-3 py-2 text-sm hover:bg-accent transition-colors flex items-center justify-between gap-2",
                              act && "bg-primary/10 text-primary"
                            )}
                          >
                            <span className="truncate flex-1 min-w-0">{m}</span>
                            <span className="text-[10px] text-muted-foreground shrink-0">
                              {ctx}
                            </span>
                            {act && <span className="text-xs">✓</span>}
                          </button>
                          {showControls && (
                            <ReasoningControls
                              providerKind={p.kind}
                              model={m}
                              reasoning={
                                currentSession.reasoning ?? DEFAULT_REASONING
                              }
                              onChange={(next) => {
                                void setReasoning(next).catch((e: unknown) => {
                                  const msg =
                                    e instanceof Error ? e.message : String(e);
                                  toast.error(msg);
                                });
                              }}
                            />
                          )}
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
      {currentProvider && (
        <span className="sr-only">{currentProvider.name}</span>
      )}
    </div>
  );
}
