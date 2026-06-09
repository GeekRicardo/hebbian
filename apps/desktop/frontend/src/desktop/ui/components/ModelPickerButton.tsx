import { useEffect, useState, useMemo } from "react";
import { ChevronDown, Brain, Flame, Image, FileText, Music, Video } from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/desktop/ui/lib/utils";
import {
  contextWindowFor,
  formatContextWindow,
} from "@/desktop/ui/lib/contextWindow";
import { HoverHint } from "@/desktop/ui/components/HoverHint";
import { useStore } from "@/desktop/ui/store/useStore";
import {
  DEFAULT_REASONING,
  REASONING_EFFORT_LABEL,
  effortDisplay,
  getModelEffortOptions,
  modelExposesLongContextToggle,
  modelSupportsReasoning,
} from "@/desktop/ui/lib/reasoning";
import type {
  CatalogEntry,
  Provider,
  ReasoningConfig,
  ReasoningEffort,
} from "@/desktop/ui/types";

function isProviderEnabled(p: Provider) {
  return p.enabled !== false;
}

function providerContextWindow(
  provider: Pick<Provider, "kind" | "model_context_windows">,
  model: string,
  entry?: CatalogEntry
) {
  return provider.model_context_windows?.[model] ?? entry?.limit?.context ?? contextWindowFor(provider.kind, model);
}

function ModelReasoningBadges({
  providerKind,
  model,
  reasoning,
}: {
  providerKind: string;
  model: string;
  reasoning: ReasoningConfig;
}) {
  if (!modelSupportsReasoning(providerKind, model)) return null;
  const enabled = reasoning.enabled ?? true;
  if (!enabled) return null;
  const effort: ReasoningEffort = reasoning.effort ?? "extra";
  return (
    <span className="model-picker-trigger-badges inline-flex items-center gap-0.5" aria-hidden="true">
      <Brain className="h-3 w-3" />
      <span className="inline-flex h-4 items-center gap-0.5 rounded-full px-1 text-[9px] font-semibold leading-none">
        <Flame className="h-2.5 w-2.5" />
        {REASONING_EFFORT_LABEL[effort]}
      </span>
    </span>
  );
}

function ProviderModels({
  provider,
  currentProviderId,
  currentModel,
  onPick,
  catalogWithoutPrefix,
}: {
  provider: Pick<Provider, "id" | "name" | "kind" | "models" | "default_model" | "model_context_windows">;
  currentProviderId?: string | null;
  currentModel?: string | null;
  onPick: (model: string) => void;
  catalogWithoutPrefix: Record<string, CatalogEntry>;
}) {
  const models = provider.models.length > 0
    ? provider.models
    : provider.default_model
    ? [provider.default_model]
    : [];
  return (
    <div className="model-picker-model-popover absolute bottom-0 left-[calc(100%+8px)] w-80 rounded-lg border border-border bg-card shadow-lg z-[91] overflow-hidden animate-slide-up">
      <div className="model-picker-model-popover-head px-3 py-2 text-[11px] font-semibold text-muted-foreground">
        {provider.name} 的模型
      </div>
      <div className="model-picker-model-list max-h-[320px] overflow-y-auto py-1">
        {models.length === 0 ? (
          <div className="px-3 py-2 text-xs text-muted-foreground italic">没有可选模型</div>
        ) : (
          models.map((m) => {
            const active = provider.id === currentProviderId && m === currentModel;
            const catalogKey = `${provider.kind}/${m}`;
            const entry = catalogWithoutPrefix[m] || catalogWithoutPrefix[catalogKey];
            const ctx = formatContextWindow(providerContextWindow(provider, m, entry));
            const inputModalities = entry?.modalities?.input || [];
            const hasReasoning = entry?.reasoning || modelSupportsReasoning(provider.kind, m);
            const outputLimit = entry?.limit?.output;
            const outputFormatted = outputLimit ? formatContextWindow(outputLimit) : null;
            return (
              <button
                key={`${provider.id}-${m}`}
                type="button"
                onClick={() => onPick(m)}
                title={`${provider.name} · ${m}（上下文 ${ctx}${outputFormatted ? `，输出 ${outputFormatted}` : ""}）`}
                className={cn(
                  "model-picker-model-row w-full text-left px-3 py-2 text-sm hover:bg-accent transition-colors flex items-center justify-between gap-2",
                  active && "bg-primary/10 text-primary"
                )}
              >
                <div className="flex items-center gap-2 min-w-0 flex-1">
                  <span className="truncate flex-1 min-w-0">{m}</span>
                  {inputModalities.length > 0 && (
                    <span className="flex items-center gap-0.5 shrink-0">
                      {inputModalities.includes("image") && <Image className="w-3 h-3 text-green-600" />}
                      {inputModalities.includes("audio") && <Music className="w-3 h-3 text-orange-600" />}
                      {inputModalities.includes("video") && <Video className="w-3 h-3 text-pink-600" />}
                      {inputModalities.includes("pdf") && <FileText className="w-3 h-3 text-red-600" />}
                    </span>
                  )}
                  {hasReasoning && <Brain className="w-3 h-3 text-purple-600 shrink-0" />}
                </div>
                <div className="flex items-center gap-1 text-[10px] text-muted-foreground shrink-0">
                  <span>{ctx}</span>
                  {outputFormatted && <span className="text-muted-foreground/60">/ {outputFormatted}</span>}
                </div>
                {active && <span className="text-xs">✓</span>}
              </button>
            );
          })
        )}
      </div>
    </div>
  );
}

/**
 * Mini pill toggle：popup 里的紧凑型开关，复用 SessionSettingsDialog 流式输出的视觉
 * 语言（圆角胶囊 + 小圆点），缩到 h-4 w-7 以适配 11px 字号的环境。
 */
function PillToggle({
  checked,
  onChange,
  ariaLabel,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  ariaLabel: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative inline-flex h-4 w-7 shrink-0 rounded-full transition-colors",
        checked ? "bg-primary" : "bg-muted"
      )}
    >
      <span
        className={cn(
          "inline-block h-3 w-3 rounded-full bg-white shadow-sm transform transition-transform mt-0.5",
          checked ? "translate-x-3.5" : "translate-x-0.5"
        )}
      />
    </button>
  );
}

function ReasoningControls({
  providerKind,
  model,
  reasoning,
  catalogEntry,
  onChange,
}: {
  providerKind: string;
  model: string;
  reasoning: ReasoningConfig;
  catalogEntry?: CatalogEntry;
  onChange: (next: ReasoningConfig) => void;
}) {
  const enabled = reasoning.enabled ?? true;
  const effort: ReasoningEffort = reasoning.effort ?? "extra";
  const longContext = reasoning.long_context ?? false;
  const showLongContext = modelExposesLongContextToggle(providerKind, model);
  const showReasoning = modelSupportsReasoning(providerKind, model);
  const effortOptions = getModelEffortOptions(providerKind, model, catalogEntry);

  return (
    <div
      onClick={(e) => e.stopPropagation()}
      className="model-picker-reasoning px-3 py-2 border-t border-border bg-muted/40 space-y-2"
    >
      {showReasoning && (
        <>
          <div className="flex items-center justify-between text-[11px]">
            <span className="text-muted-foreground">启用 thinking</span>
            <PillToggle
              checked={enabled}
              onChange={(next) => onChange({ ...reasoning, enabled: next })}
              ariaLabel="启用 thinking"
            />
          </div>
          {effortOptions.length > 0 && (
            <div className="flex items-center justify-between gap-2 text-[11px]">
              <span
                className="text-muted-foreground shrink-0"
                title={`实际发送：${effortDisplay(providerKind, model, effort)}`}
              >
                思考强度
              </span>
              <div className="inline-flex rounded-md border border-border overflow-hidden">
                {effortOptions.map((level) => {
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
          )}
        </>
      )}
      {showLongContext && (
        <div
          className="flex items-center justify-between text-[11px]"
          title="开启后请求会带 anthropic-beta: context-1m-2025-08-07，把 Sonnet/Opus 旧版本上下文从 200k 抬到 1M"
        >
          <span className="text-muted-foreground">1M 上下文</span>
          <PillToggle
            checked={longContext}
            onChange={(next) => onChange({ ...reasoning, long_context: next })}
            ariaLabel="1M 上下文"
          />
        </div>
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
  const modelsCatalog = useStore((s) => s.modelsCatalog);

  const [open, setOpen] = useState(false);
  const [selectedProviderId, setSelectedProviderId] = useState<string | null>(null);
  const [pickedModel, setPickedModel] = useState<{ providerId: string; model: string } | null>(null);

  // 构建不带前缀的 catalog 映射（如 "anthropic/claude-sonnet-4-5" → "claude-sonnet-4-5"）
  const catalogWithoutPrefix = useMemo(() => {
    const map: Record<string, any> = {};
    if (modelsCatalog?.entries) {
      for (const [key, value] of Object.entries(modelsCatalog.entries)) {
        map[key] = value;
        const slashIdx = key.indexOf("/");
        if (slashIdx > 0) {
          const withoutPrefix = key.substring(slashIdx + 1);
          map[withoutPrefix] = value;
        }
      }
    }
    return map;
  }, [modelsCatalog]);

  useEffect(() => {
    if (!currentSession) return;
    setSelectedProviderId(currentSession.provider_id);
    setPickedModel(null);
  }, [currentSession?.provider_id, currentSession?.model]);

  useEffect(() => {
    if (!open) return;
    const onClick = () => {
      setOpen(false);
      setPickedModel(null);
    };
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [open]);

  const providers = providersFile.providers;
  const enabledProviders = providers.filter(isProviderEnabled);

  if (!currentSession) {
    const previewProviders: Array<{
      id: string;
      name: string;
      kind: Provider["kind"];
      models: string[];
      default_model?: string | null;
    }> = enabledProviders.length > 0
      ? enabledProviders
      : [
          {
            id: "mock-anthropic",
            name: "Anthropic",
            kind: "anthropic",
            default_model: "claude-sonnet-4-5",
            models: ["claude-sonnet-4-5", "claude-opus-4-1", "claude-haiku-4-5"],
          },
          {
            id: "mock-openai",
            name: "OpenAI",
            kind: "openai",
            default_model: "gpt-5-codex",
            models: ["gpt-5-codex", "gpt-5.1", "o4-mini"],
          },
          {
            id: "mock-deepseek",
            name: "DeepSeek",
            kind: "deepseek",
            default_model: "deepseek-v3.2",
            models: ["deepseek-v3.2", "deepseek-reasoner", "deepseek-chat"],
          },
        ];
    const selectedPreviewProviderId = selectedProviderId ?? previewProviders[0]?.id ?? null;
    const selectedPreviewProvider = previewProviders.find((p) => p.id === selectedPreviewProviderId) ?? previewProviders[0];
    const fallbackModel = pickedModel?.model ?? selectedPreviewProvider?.default_model ?? selectedPreviewProvider?.models[0] ?? "选择模型";
    const previewReasoning: ReasoningConfig = DEFAULT_REASONING;
    return (
      <div className="model-picker relative">
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            setOpen((v) => !v);
          }}
          aria-expanded={open}
          className="model-picker-trigger inline-flex items-center gap-1 rounded-full px-2 py-1 text-[11px] leading-none text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
        >
          <span className="truncate max-w-[160px] leading-none">{fallbackModel}</span>
          {selectedPreviewProvider && (
            <ModelReasoningBadges providerKind={selectedPreviewProvider.kind} model={fallbackModel} reasoning={previewReasoning} />
          )}
          <ChevronDown className="w-3 h-3 opacity-60" />
        </button>
        {open && (
          <div
            onClick={(e) => e.stopPropagation()}
            className="model-picker-popup absolute bottom-full left-0 mb-1 w-64 rounded-lg border border-border bg-card shadow-lg z-[90] animate-slide-up"
          >
            <div className="model-picker-provider-head px-3 py-2 text-[11px] font-semibold text-muted-foreground">
              供应商
            </div>
            <div className="model-picker-provider-list max-h-[280px] overflow-y-auto py-1">
              {previewProviders.map((p) => (
                <div key={p.id} className="model-picker-provider relative">
                  <button
                    type="button"
                    className={cn(
                      "model-picker-provider-toggle w-full px-3 py-2 text-[12px] font-semibold flex items-center justify-between transition-colors",
                      p.id === selectedPreviewProviderId && "is-selected"
                    )}
                    onClick={() => {
                      setSelectedProviderId(p.id);
                      setPickedModel(null);
                    }}
                  >
                    <span>{p.name}</span>
                    <span className="text-[10px] text-muted-foreground uppercase">{p.kind}</span>
                  </button>
                </div>
              ))}
            </div>
            {selectedPreviewProvider && (
              <ProviderModels
                provider={selectedPreviewProvider}
                currentProviderId={pickedModel?.providerId ?? selectedPreviewProvider.id}
                currentModel={pickedModel?.model ?? fallbackModel}
                onPick={(model) => setPickedModel({ providerId: selectedPreviewProvider.id, model })}
                catalogWithoutPrefix={catalogWithoutPrefix}
              />
            )}
            {pickedModel && selectedPreviewProvider && (
              <div className="model-picker-selected-controls">
                <ReasoningControls
                  providerKind={selectedPreviewProvider.kind}
                  model={pickedModel.model}
                  reasoning={previewReasoning}
                  catalogEntry={catalogWithoutPrefix[pickedModel.model] || catalogWithoutPrefix[`${selectedPreviewProvider.kind}/${pickedModel.model}`]}
                  onChange={() => {}}
                />
                <button type="button" className="model-picker-done" onClick={() => setOpen(false)}>
                  完成
                </button>
              </div>
            )}
          </div>
        )}
      </div>
    );
  }

  const currentProvider = providers.find(
    (p) => p.id === currentSession.provider_id
  );

  async function handleSwitch(providerId: string, model: string) {
    try {
      await switchProviderModel(providerId, model);
      setPickedModel({ providerId, model });
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  const selectedProvider = enabledProviders.find((p) => p.id === selectedProviderId) ?? currentProvider ?? enabledProviders[0] ?? null;
  const selectedModel = pickedModel?.providerId === selectedProvider?.id ? pickedModel.model : currentSession.model;
  const selectedCatalogEntry = selectedProvider
    ? catalogWithoutPrefix[selectedModel] || catalogWithoutPrefix[`${selectedProvider.kind}/${selectedModel}`]
    : undefined;

  const currentContext = currentProvider
    ? formatContextWindow(
        providerContextWindow(currentProvider, currentSession.model, selectedCatalogEntry)
      )
    : "";
  const currentTooltip = currentProvider
    ? `${currentProvider.name} · ${currentSession.model}（上下文 ${currentContext}）`
    : "切换模型";

  return (
    <div className="model-picker relative">
      <HoverHint hint={currentTooltip} align="end">
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            setOpen((v) => {
              const next = !v;
              if (next) {
                setSelectedProviderId(currentSession.provider_id);
                setPickedModel(null);
              }
              return next;
            });
          }}
          aria-expanded={open}
          className="model-picker-trigger inline-flex items-center gap-1 rounded-full px-2 py-1 text-[11px] leading-none text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
        >
          <span className="truncate max-w-[160px] leading-none">{currentSession.model}</span>
          {currentProvider && (
            <ModelReasoningBadges
              providerKind={currentProvider.kind}
              model={currentSession.model}
              reasoning={currentSession.reasoning ?? DEFAULT_REASONING}
            />
          )}
          <ChevronDown className="w-3 h-3 opacity-60" />
        </button>
      </HoverHint>
      {open && (
        <div
          onClick={(e) => e.stopPropagation()}
          className="model-picker-popup absolute bottom-full left-0 mb-1 w-64 rounded-lg border border-border bg-card shadow-lg z-[90] animate-slide-up"
        >
          <div className="model-picker-provider-head px-3 py-2 text-[11px] font-semibold text-muted-foreground">
            供应商
          </div>
          {enabledProviders.length === 0 ? (
            <div className="p-4 text-xs text-muted-foreground text-center">
              没有已启用的供应商
            </div>
          ) : (
            <div className="model-picker-provider-list max-h-[280px] overflow-y-auto py-1">
              {enabledProviders.map((p) => {
                const isSelectedProvider = p.id === selectedProvider?.id;
                return (
                  <div key={p.id} className="model-picker-provider relative">
                    <button
                      type="button"
                      onClick={() => {
                        setSelectedProviderId(p.id);
                        setPickedModel(null);
                      }}
                      className={cn(
                        "model-picker-provider-toggle w-full px-3 py-2 text-[12px] font-semibold flex items-center justify-between transition-colors",
                        isSelectedProvider && "is-selected"
                      )}
                    >
                      <span>{p.name}</span>
                      <span className="inline-flex items-center gap-1 text-[10px] text-muted-foreground uppercase">
                        {p.kind}
                        <ChevronDown className="h-3 w-3 -rotate-90" />
                      </span>
                    </button>
                  </div>
                );
              })}
            </div>
          )}
          {selectedProvider && (
            <ProviderModels
              provider={selectedProvider}
              currentProviderId={currentSession.provider_id}
              currentModel={currentSession.model}
              onPick={(model) => void handleSwitch(selectedProvider.id, model)}
              catalogWithoutPrefix={catalogWithoutPrefix}
            />
          )}
          {pickedModel && selectedProvider && (
            <div className="model-picker-selected-controls">
              <ReasoningControls
                providerKind={selectedProvider.kind}
                model={pickedModel.model}
                catalogEntry={selectedCatalogEntry}
                reasoning={currentSession.reasoning ?? DEFAULT_REASONING}
                onChange={(next) => {
                  void setReasoning(next).catch((e: unknown) => {
                    toast.error(e instanceof Error ? e.message : String(e));
                  });
                }}
              />
              <button
                type="button"
                className="model-picker-done"
                onClick={() => {
                  setOpen(false);
                  setPickedModel(null);
                }}
              >
                完成
              </button>
            </div>
          )}
        </div>
      )}
      {currentProvider && (
        <span className="sr-only">{currentProvider.name}</span>
      )}
    </div>
  );
}
