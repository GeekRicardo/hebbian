import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import {
  Activity,
  Plus,
  Trash2,
  KeyRound,
  Globe,
  Boxes,
  Check,
  GripVertical,
  Download,
  LogIn,
  Package,
  Loader2,
  ExternalLink,
  Save,
  Copy,
} from "lucide-react";
import { nanoid } from "nanoid";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label, SecretInput, Select, Textarea } from "@/desktop/ui/components/ui/input";
import { OAuthDialog } from "./OAuthDialog";
import { DeepseekLoginDialog } from "./DeepseekLoginDialog";
import { FamilyGroup } from "./FamilyGroup";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import type {
  AuthMode,
  CatalogEntry,
  FetchedModel,
  Provider,
  ProviderKind,
  ProviderPreset,
} from "@/desktop/ui/types";
import { cn } from "@/desktop/ui/lib/utils";

type Tab = "providers" | "presets";

const OFFICIAL_BASE_URLS: Record<ProviderKind, string> = {
  openai: "https://api.openai.com/v1",
  anthropic: "https://api.anthropic.com",
  gemini: "https://generativelanguage.googleapis.com",
  deepseek: "https://chat.deepseek.com",
};

const CODEX_OAUTH_BASE_URL = "https://chatgpt.com/backend-api/codex";
const CODEX_OAUTH_MODELS = ["gpt-5.4", "gpt-5.4-mini"];

const SUPPORTED_AUTH_MODES: Record<ProviderKind, AuthMode[]> = {
  openai: ["api_key", "oauth_codex"],
  anthropic: ["api_key", "oauth_claude_code"],
  gemini: ["api_key", "oauth_gemini_cli"],
  deepseek: ["api_key"],
};

function normalizeBaseUrl(baseUrl: string) {
  return baseUrl.trim().replace(/\/+$/, "");
}

function getOfficialBaseUrl(kind: ProviderKind) {
  return OFFICIAL_BASE_URLS[kind];
}

function shouldAutoReplaceBaseUrl(baseUrl: string) {
  const normalized = normalizeBaseUrl(baseUrl);
  if (!normalized) return true;
  return Object.values(OFFICIAL_BASE_URLS).some(
    (officialUrl) => normalizeBaseUrl(officialUrl) === normalized
  );
}

function getCompatibleAuthMode(kind: ProviderKind, authMode: AuthMode) {
  return SUPPORTED_AUTH_MODES[kind].includes(authMode) ? authMode : "api_key";
}

function isDeepseekProvider(p: Provider | null | undefined) {
  if (!p) return false;
  const url = (p.base_url || "").toLowerCase();
  return (
    url.includes("deepseek.com") ||
    p.id.toLowerCase().includes("deepseek") ||
    p.name.toLowerCase().includes("deepseek")
  );
}

function parseTokenCount(text: string): number | null {
  const raw = text.trim().toLowerCase();
  if (!raw) return null;
  const match = raw.match(/^(\d+(?:\.\d+)?)([km])?$/);
  if (!match) return null;
  const n = Number(match[1]);
  if (!Number.isFinite(n) || n <= 0) return null;
  const multiplier = match[2] === "m" ? 1_000_000 : match[2] === "k" ? 1_000 : 1;
  return Math.round(n * multiplier);
}

function modelsToText(models: string[]) {
  return models.join("\n");
}

function parseModelsText(text: string) {
  return text
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
}

function headersToText(headers: Record<string, string>) {
  return Object.entries(headers)
    .map(([key, value]) => `${key}: ${value}`)
    .join("\n");
}

function parseHeadersText(text: string) {
  const headers: Record<string, string> = {};
  for (const rawLine of text.split("\n")) {
    const line = rawLine.trim();
    if (!line) continue;
    const colon = line.indexOf(":");
    const equals = line.indexOf("=");
    const sep =
      colon >= 0 && equals >= 0 ? Math.min(colon, equals) : Math.max(colon, equals);
    if (sep <= 0) continue;
    const key = line.slice(0, sep).trim();
    const value = line.slice(sep + 1).trim();
    if (key && value) headers[key] = value;
  }
  return headers;
}

/**
 * 供应商管理 pane（嵌入设置弹窗的「供应商」tab）。
 *
 * `active` 表示当前 tab 是否可见；从不可见切换到可见时会重新同步 draft（丢弃未保存改动）。
 * 保存走 pane 内部按钮（providers.json 与 AppSettings 是两份独立的存储）。
 */
export function ProvidersPane({ active }: { active: boolean }) {
  const {
    providersFile,
    saveProviders,
    modelsCatalog,
  } = useStore();

  const [draft, setDraft] = useState(providersFile);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("providers");
  const [presets, setPresets] = useState<ProviderPreset[]>([]);
  const [fetching, setFetching] = useState(false);
  const [fetchedModels, setFetchedModels] = useState<FetchedModel[] | null>(null);
  const [oauthOpen, setOauthOpen] = useState(false);
  const [oauthMode, setOauthMode] = useState<AuthMode | null>(null);
  const [deepseekLoginOpen, setDeepseekLoginOpen] = useState(false);
  const [modelsText, setModelsText] = useState("");
  const [headersText, setHeadersText] = useState("");
  const [draggingProviderId, setDraggingProviderId] = useState<string | null>(null);
  const draggingProviderIdRef = useRef<string | null>(null);
  const [testingModel, setTestingModel] = useState(false);
  const [modelTest, setModelTest] = useState<{
    providerId: string;
    model: string;
    ok: boolean;
    message: string;
  } | null>(null);

  useEffect(() => {
    if (active) {
      setDraft(providersFile);
      setSelectedId(providersFile.providers[0]?.id ?? null);
      setModelsText(modelsToText(providersFile.providers[0]?.models ?? []));
      setHeadersText(headersToText(providersFile.providers[0]?.extra_headers ?? {}));
      setFetchedModels(null);
      setModelTest(null);
      setTab("providers");
      api.listProviderPresets().then(setPresets).catch(() => {});
    }
  }, [active, providersFile]);

  const current = draft.providers.find((p) => p.id === selectedId);

  function updateCurrent(patch: Partial<Provider>) {
    if (!current) return;
    setDraft({
      ...draft,
      providers: draft.providers.map((p) =>
        p.id === current.id ? { ...p, ...patch } : p
      ),
    });
  }

  function blankProvider(
    kind: ProviderKind = "openai",
    overrides: Partial<Provider> = {}
  ): Provider {
    return {
      id: nanoid(),
      name: "新供应商",
      kind,
      auth_mode: "api_key",
      enabled: true,
      base_url: getOfficialBaseUrl(kind),
      api_key: "",
      refresh_token: null,
      token_expires_at: null,
      account_id: null,
      extra_headers: {},
      models: [],
      fetched_models: null,
      model_context_windows: {},
      default_model: null,
      title_gen_enabled: false,
      title_gen_model: null,
      claude_code_compat: false,
      ...overrides,
    };
  }

  /**
   * 切换「标题生成模型」开关：
   * - 关闭：仅清掉当前 provider 的 title_gen 字段
   * - 打开：把其他 provider 的 title_gen_enabled 全部置 false（互斥）
   */
  function toggleTitleGen(enabled: boolean) {
    if (!current) return;
    setDraft((d) => ({
      ...d,
      providers: d.providers.map((p) => {
        if (p.id === current.id) {
          return {
            ...p,
            title_gen_enabled: enabled,
            title_gen_model: enabled
              ? (p.title_gen_model ?? p.default_model ?? p.models[0] ?? null)
              : null,
          };
        }
        return enabled ? { ...p, title_gen_enabled: false } : p;
      }),
    }));
  }

  function addBlank() {
    const p = blankProvider();
    setDraft({ ...draft, providers: [...draft.providers, p] });
    setSelectedId(p.id);
    setModelsText(modelsToText(p.models));
    setHeadersText(headersToText(p.extra_headers));
    setModelTest(null);
    setTab("providers");
  }

  function copyProvider(source: Provider) {
    // 复制供应商，生成新 ID 并修改名称
    const newProvider: Provider = {
      ...source,
      id: nanoid(),
      name: `${source.name} (副本)`,
      // 清空敏感信息
      api_key: "",
      refresh_token: null,
      token_expires_at: null,
      account_id: null,
    };
    setDraft({ ...draft, providers: [...draft.providers, newProvider] });
    setSelectedId(newProvider.id);
    setModelsText(modelsToText(newProvider.models));
    setHeadersText(headersToText(newProvider.extra_headers));
    setModelTest(null);
    setTab("providers");
    toast.success(`已复制 ${source.name}`);
  }

  function addFromPreset(preset: ProviderPreset) {
    const p = blankProvider(preset.kind, {
      id: nanoid(),
      name: preset.name,
      kind: preset.kind,
      base_url: preset.base_url,
      models: [...preset.models],
      default_model: preset.default_model || preset.models[0] || null,
    });
    setDraft({ ...draft, providers: [...draft.providers, p] });
    setSelectedId(p.id);
    setModelsText(modelsToText(p.models));
    setHeadersText(headersToText(p.extra_headers));
    setModelTest(null);
    setTab("providers");
    toast.success(`已添加 ${preset.name}，请填入 API Key`);
  }

  function removeCurrent() {
    if (!current) return;
    if (!confirm(`删除供应商 "${current.name}"？`)) return;
    const next = draft.providers.filter((p) => p.id !== current.id);
    setDraft({
      ...draft,
      providers: next,
      default_provider_id:
        draft.default_provider_id === current.id ? null : draft.default_provider_id,
    });
    setSelectedId(next[0]?.id ?? null);
    setModelsText(modelsToText(next[0]?.models ?? []));
    setHeadersText(headersToText(next[0]?.extra_headers ?? {}));
    setModelTest(null);
  }

  async function handleSave() {
    try {
      await saveProviders(draft);
      toast.success("已保存供应商配置");
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  function setDefault(id: string | null) {
    setDraft({ ...draft, default_provider_id: id });
  }

  function selectProvider(p: Provider) {
    setSelectedId(p.id);
    setModelsText(modelsToText(p.models));
    setHeadersText(headersToText(p.extra_headers));
    setFetchedModels(null);
    setModelTest(null);
  }

  function moveProvider(dragId: string, overId: string, placement: "before" | "after") {
    setDraft((currentDraft) => {
      if (dragId === overId) return currentDraft;
      const from = currentDraft.providers.findIndex((p) => p.id === dragId);
      const to = currentDraft.providers.findIndex((p) => p.id === overId);
      if (from < 0 || to < 0) return currentDraft;
      if (from < to && placement === "before") return currentDraft;
      if (from > to && placement === "after") return currentDraft;

      const providers = [...currentDraft.providers];
      const [moved] = providers.splice(from, 1);
      const overIndex = providers.findIndex((p) => p.id === overId);
      providers.splice(placement === "after" ? overIndex + 1 : overIndex, 0, moved);
      return { ...currentDraft, providers };
    });
  }

  function updateProviderDrag(clientX: number, clientY: number) {
    const dragId = draggingProviderIdRef.current;
    if (!dragId) return;
    const target = document
      .elementFromPoint(clientX, clientY)
      ?.closest<HTMLElement>("[data-provider-id]");
    const overId = target?.dataset.providerId;
    if (!target || !overId || overId === dragId) return;

    const rect = target.getBoundingClientRect();
    const placement = clientY > rect.top + rect.height / 2 ? "after" : "before";
    moveProvider(dragId, overId, placement);
  }

  function startProviderDrag(e: React.PointerEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    const provider = draft.providers.find((p) => p.id === id);
    if (!provider) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    draggingProviderIdRef.current = id;
    setDraggingProviderId(id);
    selectProvider(provider);
  }

  function finishProviderDrag(e: React.PointerEvent) {
    if (!draggingProviderIdRef.current) return;
    e.preventDefault();
    e.stopPropagation();
    if ((e.currentTarget as HTMLElement).hasPointerCapture(e.pointerId)) {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    }
    draggingProviderIdRef.current = null;
    setDraggingProviderId(null);
  }

  async function fetchModels() {
    if (!current) return;
    if (!current.api_key) {
      toast.error("请先填写 API Key");
      return;
    }
    setFetching(true);
    setFetchedModels(null);
    try {
      const list = await api.fetchProviderModels(current);
      if (list.length === 0) {
        toast.warning("接口返回 0 个模型");
      } else {
        toast.success(`拉取到 ${list.length} 个模型`);
      }
      setFetchedModels(list);
      updateCurrent({ fetched_models: list.map((model) => model.id) });
    } catch (e: any) {
      toast.error(e.message || String(e));
    } finally {
      setFetching(false);
    }
  }

  function getModelForTest(provider: Provider) {
    return provider.default_model || provider.models[0] || "";
  }

  async function testCurrentModel() {
    if (!current) return;
    if (!current.api_key) {
      toast.error("请先填写 API Key");
      return;
    }

    const model = getModelForTest(current);
    if (!model) {
      toast.error("请先填写至少一个模型");
      return;
    }

    setTestingModel(true);
    setModelTest(null);
    try {
      const result = await api.testProviderModel(current, model);
      const preview = result.response_preview.trim();
      const message = preview ? `回复：${preview}` : "请求成功";
      setModelTest({
        providerId: current.id,
        model,
        ok: true,
        message,
      });
      toast.success(`模型可用：${model}`);
    } catch (e: any) {
      const message = e.message || String(e);
      setModelTest({
        providerId: current.id,
        model,
        ok: false,
        message,
      });
      toast.error(message);
    } finally {
      setTestingModel(false);
    }
  }

  function toggleFetchedModel(id: string) {
    if (!current) return;
    const existing = new Set(current.models);
    if (existing.has(id)) existing.delete(id);
    else existing.add(id);
    const models = Array.from(existing);
    setModelsText(modelsToText(models));
    updateCurrent({ models });
  }

  function updateModelContextWindow(modelId: string, value: string) {
    if (!current) return;
    const parsed = parseTokenCount(value);
    const next = { ...(current.model_context_windows ?? {}) };
    if (parsed == null) {
      delete next[modelId];
    } else {
      next[modelId] = parsed;
    }
    updateCurrent({ model_context_windows: next });
  }

  function openOAuth(mode: AuthMode) {
    setOauthMode(mode);
    setOauthOpen(true);
  }

  function handleKindChange(nextKind: ProviderKind) {
    if (!current || current.kind === nextKind) return;
    updateCurrent({
      kind: nextKind,
      auth_mode: getCompatibleAuthMode(nextKind, current.auth_mode),
      base_url: shouldAutoReplaceBaseUrl(current.base_url)
        ? getOfficialBaseUrl(nextKind)
        : current.base_url,
    });
  }

  function onOAuthSuccess(info: {
    api_key: string;
    refresh_token?: string;
    account_id?: string;
    token_expires_at?: number;
  }) {
    const codexPatch =
      oauthMode === "oauth_codex"
        ? {
            auth_mode: "oauth_codex" as AuthMode,
            base_url: CODEX_OAUTH_BASE_URL,
            models: CODEX_OAUTH_MODELS,
            default_model:
              current?.default_model && CODEX_OAUTH_MODELS.includes(current.default_model)
                ? current.default_model
                : CODEX_OAUTH_MODELS[0],
          }
        : oauthMode === "oauth_claude_code"
          ? { auth_mode: "oauth_claude_code" as AuthMode }
          : oauthMode === "oauth_gemini_cli"
            ? { auth_mode: "oauth_gemini_cli" as AuthMode }
            : {};
    updateCurrent({
      ...codexPatch,
      api_key: info.api_key,
      refresh_token: info.refresh_token ?? null,
      account_id: info.account_id ?? null,
      token_expires_at: info.token_expires_at ?? null,
    });
    if ("models" in codexPatch && codexPatch.models) {
      setModelsText(modelsToText(codexPatch.models));
    }
  }

  const kindLabel = (k: ProviderKind) =>
    k === "openai"
      ? "OpenAI 兼容"
      : k === "anthropic"
        ? "Anthropic"
        : k === "deepseek"
          ? "DeepSeek 网页"
          : "Gemini";
  const modelForTest = current ? getModelForTest(current) : "";

  return (
    <>
      <div className="flex items-center gap-2 mb-3 border-b border-border -mt-1 pb-3">
          <button
            onClick={() => setTab("providers")}
            className={cn(
              "px-3 py-1.5 rounded-md text-sm font-medium",
              tab === "providers"
                ? "bg-accent text-accent-foreground"
                : "text-muted-foreground hover:bg-accent/50"
            )}
          >
            已配置
          </button>
          <button
            onClick={() => setTab("presets")}
            className={cn(
              "px-3 py-1.5 rounded-md text-sm font-medium inline-flex items-center gap-1",
              tab === "presets"
                ? "bg-accent text-accent-foreground"
                : "text-muted-foreground hover:bg-accent/50"
            )}
          >
            <Package className="w-3.5 h-3.5" />
            内置预设 ({presets.length})
          </button>
        </div>

        {tab === "presets" ? (
          <PresetsGrid presets={presets} onPick={addFromPreset} />
        ) : (
          <div className="flex gap-4 min-h-[460px]">
            <div className="w-56 shrink-0 border-r border-border pr-3">
              <div className="flex items-center justify-between mb-2">
                <span className="text-xs font-medium text-muted-foreground">
                  供应商
                </span>
                <button
                  onClick={addBlank}
                  className="h-6 w-6 inline-flex items-center justify-center rounded hover:bg-accent text-muted-foreground"
                  title="添加空白"
                >
                  <Plus className="w-4 h-4" />
                </button>
              </div>
              <ul className="space-y-0.5">
                {draft.providers.map((p) => (
                  <li
                    key={p.id}
                    data-provider-id={p.id}
                    onPointerMove={(e) => updateProviderDrag(e.clientX, e.clientY)}
                    onClick={() => selectProvider(p)}
                    className={cn(
                      "group px-2 py-2 rounded-md cursor-pointer flex items-center justify-between gap-2 transition-colors",
                      selectedId === p.id
                        ? "bg-accent text-accent-foreground"
                        : "hover:bg-accent/50",
                      draggingProviderId === p.id && "opacity-60"
                    )}
                  >
                    <GripVertical
                      className="h-4 w-4 shrink-0 cursor-grab touch-none text-muted-foreground/60 active:cursor-grabbing"
                      onPointerDown={(e) => startProviderDrag(e, p.id)}
                      onPointerMove={(e) => updateProviderDrag(e.clientX, e.clientY)}
                      onPointerUp={finishProviderDrag}
                      onPointerCancel={finishProviderDrag}
                      aria-hidden="true"
                    />
                    <div className="min-w-0 flex-1">
                      <div className="text-sm truncate">{p.name}</div>
                      <div className="text-[11px] text-muted-foreground flex items-center gap-1">
                        <span>{kindLabel(p.kind)}</span>
                        {p.enabled === false && (
                          <span className="text-muted-foreground">已停用</span>
                        )}
                        {!p.api_key && (
                          <span className="text-amber-500">未填 Key</span>
                        )}
                      </div>
                    </div>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        copyProvider(p);
                      }}
                      className="h-6 w-6 inline-flex items-center justify-center rounded hover:bg-accent text-muted-foreground hover:text-foreground opacity-0 group-hover:opacity-100 transition-opacity"
                      title="复制供应商"
                    >
                      <Copy className="w-3.5 h-3.5" />
                    </button>
                    {draft.default_provider_id === p.id && (
                      <Check className="w-3.5 h-3.5 text-primary" />
                    )}
                  </li>
                ))}
                {draft.providers.length === 0 && (
                  <li className="text-xs text-muted-foreground px-3 py-4 text-center">
                    尚无供应商 — 切到「内置预设」或点 +
                  </li>
                )}
              </ul>
            </div>

            <div className="flex-1 min-w-0 space-y-4">
              {!current ? (
                <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
                  选择或新增一个供应商
                </div>
              ) : (
                <>
                  <div className="grid grid-cols-2 gap-3">
                    <div className="space-y-1.5">
                      <Label>名称</Label>
                      <Input
                        value={current.name}
                        spellCheck={false}
                        autoCorrect="off"
                        onChange={(e) => updateCurrent({ name: e.target.value })}
                      />
                    </div>
                    <div className="space-y-1.5">
                      <Label>API 格式</Label>
                      <Select
                        value={current.kind}
                        onChange={(e) =>
                          handleKindChange(e.target.value as ProviderKind)
                        }
                      >
                        <option value="openai">OpenAI 兼容</option>
                        <option value="anthropic">Anthropic</option>
                        <option value="gemini">Google Gemini</option>
                      </Select>
                    </div>
                  </div>

                  <div className="space-y-1.5">
                    <label className="inline-flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={current.enabled !== false}
                        onChange={(e) =>
                          updateCurrent({ enabled: e.target.checked })
                        }
                      />
                      启用
                    </label>
                  </div>

                  <div className="space-y-1.5">
                    <Label>认证方式</Label>
                    <div className="flex flex-wrap gap-2">
                      <AuthChip
                        active={current.auth_mode === "api_key"}
                        onClick={() => updateCurrent({ auth_mode: "api_key" })}
                        label="API Key"
                      />
                      {current.kind === "openai" && (
                        <AuthChip
                          active={current.auth_mode === "oauth_codex"}
                          onClick={() => updateCurrent({ auth_mode: "oauth_codex" })}
                          label="Codex OAuth (ChatGPT)"
                          oauth
                          onOAuth={() => openOAuth("oauth_codex")}
                        />
                      )}
                      {current.kind === "anthropic" && (
                        <AuthChip
                          active={current.auth_mode === "oauth_claude_code"}
                          onClick={() =>
                            updateCurrent({ auth_mode: "oauth_claude_code" })
                          }
                          label="Claude Code"
                          oauth
                          onOAuth={() => openOAuth("oauth_claude_code")}
                        />
                      )}
                      {current.kind === "gemini" && (
                        <AuthChip
                          active={current.auth_mode === "oauth_gemini_cli"}
                          onClick={() =>
                            updateCurrent({ auth_mode: "oauth_gemini_cli" })
                          }
                          label="Gemini CLI"
                          oauth
                          onOAuth={() => openOAuth("oauth_gemini_cli")}
                        />
                      )}
                    </div>
                  </div>

                  <div className="space-y-1.5">
                    <Label className="inline-flex items-center gap-1">
                      <Globe className="w-3 h-3" /> Base URL
                    </Label>
                    <Input
                      value={current.base_url}
                      spellCheck={false}
                      autoCorrect="off"
                      onChange={(e) => updateCurrent({ base_url: e.target.value })}
                    />
                  </div>

                  <div className="space-y-1.5">
                    <Label className="inline-flex items-center gap-1">
                      <KeyRound className="w-3 h-3" />
                      {current.auth_mode === "api_key"
                        ? "API Key"
                        : "Access Token"}
                    </Label>
                    <SecretInput
                      spellCheck={false}
                      autoCorrect="off"
                      value={current.api_key}
                      onChange={(e) => updateCurrent({ api_key: e.target.value })}
                      placeholder={
                        current.auth_mode === "api_key"
                          ? "sk-... / ..."
                          : "由 OAuth 自动填入，也可手动粘贴"
                      }
                    />
                    {isDeepseekProvider(current) && (
                      <div className="flex items-center justify-between gap-2 pt-0.5">
                        <p className="text-[11px] text-muted-foreground">
                          没有 API Key？可直接用 DeepSeek 账号登录获取 token。
                        </p>
                        <button
                          type="button"
                          onClick={() => setDeepseekLoginOpen(true)}
                          className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-[11px] hover:bg-accent text-primary"
                        >
                          <LogIn className="w-3 h-3" />
                          用账号登录 DeepSeek
                        </button>
                      </div>
                    )}
                    {current.account_id && (
                      <p className="text-[11px] text-muted-foreground">
                        已绑定账号：{current.account_id}
                      </p>
                    )}
                  </div>

                  <div className="space-y-1.5">
                    <Label>额外请求头</Label>
                    <Textarea
                      rows={2}
                      value={headersText}
                      spellCheck={false}
                      autoCorrect="off"
                      placeholder="Header-Name: value"
                      onChange={(e) => {
                        const text = e.target.value;
                        setHeadersText(text);
                        updateCurrent({ extra_headers: parseHeadersText(text) });
                      }}
                    />
                    <p className="text-[11px] text-muted-foreground">
                      每行一个，支持 `Key: Value` 或 `Key=Value`，用于兼容自建代理。
                    </p>
                  </div>

                  <div className="space-y-1.5">
                    <div className="flex items-center justify-between">
                      <Label className="inline-flex items-center gap-1">
                        <Boxes className="w-3 h-3" /> 模型列表
                      </Label>
                      <div className="flex items-center gap-1.5">
                        <button
                          type="button"
                          onClick={testCurrentModel}
                          disabled={testingModel || !current.api_key || !modelForTest}
                          className="text-xs inline-flex items-center gap-1 px-2 py-1 rounded-md hover:bg-accent text-primary disabled:opacity-50"
                        >
                          {testingModel ? (
                            <Loader2 className="w-3 h-3 animate-spin" />
                          ) : (
                            <Activity className="w-3 h-3" />
                          )}
                          测试模型
                        </button>
                        <button
                          type="button"
                          onClick={fetchModels}
                          disabled={fetching || !current.api_key}
                          className="text-xs inline-flex items-center gap-1 px-2 py-1 rounded-md hover:bg-accent text-primary disabled:opacity-50"
                        >
                          {fetching ? (
                            <Loader2 className="w-3 h-3 animate-spin" />
                          ) : (
                            <Download className="w-3 h-3" />
                          )}
                          拉取模型列表
                        </button>
                      </div>
                    </div>
                    {modelTest &&
                      modelTest.providerId === current.id &&
                      modelTest.model === modelForTest && (
                        <div
                          className={cn(
                            "rounded-md border px-2 py-1.5 text-xs",
                            modelTest.ok
                              ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
                              : "border-destructive/30 bg-destructive/10 text-destructive"
                          )}
                        >
                          {modelTest.ok ? "测试通过" : "测试失败"} ·{" "}
                          {modelTest.model} · {modelTest.message}
                        </div>
                      )}
                    {(fetchedModels && fetchedModels.length > 0) || (current.fetched_models && current.fetched_models.length > 0) ? (
                      <div className="pt-2">
                        <div className="text-xs text-muted-foreground mb-2">
                          点击卡片选中模型（已选 {current.models.length}）。上下文可手动修改，留空走自动识别
                          {fetchedModels && ` · 共 ${fetchedModels.length} 个模型`}
                          {!fetchedModels && current.fetched_models && ` · 显示缓存 · 共 ${current.fetched_models.length} 个模型`}
                        </div>
                        <div className="max-h-[420px] overflow-y-auto space-y-4 pr-1 -mr-1">
                          {groupModelsByFamily(
                            fetchedModels || current.fetched_models!.map(id => ({ id, owned_by: null })),
                            modelsCatalog?.entries || null
                          ).map(
                            ({ family, models }) => (
                              <FamilyGroup
                                key={family}
                                family={family}
                                models={models}
                                selectedModels={current.models}
                                onToggleModel={toggleFetchedModel}
                                catalog={modelsCatalog?.entries || null}
                                contextWindows={current.model_context_windows ?? {}}
                                onUpdateContextWindow={updateModelContextWindow}
                              />
                            )
                          )}
                        </div>
                      </div>
                    ) : fetchedModels ? (
                      <div className="text-xs text-muted-foreground py-4 text-center">
                        未拉取到任何模型
                      </div>
                    ) : null}
                  </div>

                  <div className="space-y-1.5">
                    <Label>默认模型</Label>
                    <Select
                      value={current.default_model ?? ""}
                      onChange={(e) =>
                        updateCurrent({
                          default_model: e.target.value || null,
                        })
                      }
                    >
                      <option value="">（不设置）</option>
                      {current.models.map((m) => (
                        <option key={m} value={m}>
                          {m}
                        </option>
                      ))}
                    </Select>
                  </div>

                  <div className="space-y-1.5">
                    <label className="inline-flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={current.title_gen_enabled === true}
                        onChange={(e) => toggleTitleGen(e.target.checked)}
                        disabled={current.models.length === 0}
                      />
                      <span>标题生成模型</span>
                      <span className="text-[11px] text-muted-foreground">
                        勾选后用此供应商为新对话生成标题（全局唯一）
                      </span>
                    </label>
                    {current.title_gen_enabled && (
                      <Select
                        value={current.title_gen_model ?? ""}
                        onChange={(e) =>
                          updateCurrent({
                            title_gen_model: e.target.value || null,
                          })
                        }
                      >
                        <option value="">（请选择模型）</option>
                        {current.models.map((m) => (
                          <option key={m} value={m}>
                            {m}
                          </option>
                        ))}
                      </Select>
                    )}
                  </div>

                  {current.kind === "anthropic" && (
                    <div className="space-y-1.5">
                      <label className="inline-flex items-center gap-2 text-sm">
                        <input
                          type="checkbox"
                          checked={current.claude_code_compat === true}
                          onChange={(e) =>
                            updateCurrent({ claude_code_compat: e.target.checked })
                          }
                        />
                        <span>Claude Code 兼容模式</span>
                        <span className="text-[11px] text-muted-foreground">
                          注入 Claude Code 客户端特征，用于兼容需要身份校验的代理
                        </span>
                      </label>
                      {current.claude_code_compat && (
                        <div className="rounded-md border border-border bg-accent/30 p-3 text-[11px] font-mono space-y-0.5 text-muted-foreground max-h-48 overflow-y-auto">
                          <div className="text-foreground font-medium text-xs mb-1.5 font-sans">
                            注入的 HTTP Header
                          </div>
                          {[
                            ["user-agent", "claude-cli/2.1.150 (external, cli)"],
                            ["x-app", "cli"],
                            ["anthropic-version", "2023-06-01"],
                            ["anthropic-dangerous-direct-browser-access", "true"],
                            ["anthropic-beta", "claude-code-20250219,interleaved-thinking-2025-05-14,…"],
                            ["x-stainless-lang", "js"],
                            ["x-stainless-os", "MacOS"],
                            ["x-stainless-arch", "arm64"],
                            ["x-stainless-runtime", "node"],
                          ].map(([k, v]) => (
                            <div key={k} className="flex gap-2">
                              <span className="text-foreground shrink-0">{k}:</span>
                              <span className="truncate">{v}</span>
                            </div>
                          ))}
                          <div className="text-foreground font-medium text-xs mt-2 mb-1.5 font-sans">
                            注入的请求体字段
                          </div>
                          <div className="flex gap-2">
                            <span className="text-foreground shrink-0">system:</span>
                            <span>[billing-header, Claude Code banner, agent desc]</span>
                          </div>
                          <div className="flex gap-2">
                            <span className="text-foreground shrink-0">metadata.user_id:</span>
                            <span>{"{device_id, session_id}"}</span>
                          </div>
                          <div className="flex gap-2">
                            <span className="text-foreground shrink-0">context_management:</span>
                            <span>{"{edits: [clear_thinking]}"}</span>
                          </div>
                        </div>
                      )}
                    </div>
                  )}

                  <div className="pt-2 flex items-center justify-between border-t border-border">
                    <label className="inline-flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={draft.default_provider_id === current.id}
                        onChange={(e) =>
                          setDefault(e.target.checked ? current.id : null)
                        }
                      />
                      设为默认
                    </label>
                    <Button variant="destructive" size="sm" onClick={removeCurrent}>
                      <Trash2 className="w-3.5 h-3.5" />
                      删除
                    </Button>
                  </div>
                </>
              )}
            </div>
          </div>
        )}

      {/* ── 全局：视觉辅助模型 ─────────────────────────────────── */}
      <div className="mt-4 pt-3 border-t border-border space-y-2">
        <h4 className="text-sm font-medium">视觉辅助模型</h4>
        <p className="text-[11px] text-muted-foreground leading-relaxed">
          当聊天模型不支持图片时，自动用这里选的模型先&ldquo;看图&rdquo;，把图片转成文字描述再发给聊天模型。
          不配则跳过（图片附件直接发给聊天模型）。
        </p>
        <div className="grid grid-cols-2 gap-2">
          <div className="space-y-1">
            <label className="text-xs text-muted-foreground">供应商</label>
            <Select
              value={draft.vision_provider_id ?? ""}
              onChange={(e) => {
                const pid = e.target.value || null;
                setDraft({
                  ...draft,
                  vision_provider_id: pid,
                  // 切换供应商时重置模型选择
                  vision_model: pid
                    ? (draft.providers.find((p) => p.id === pid)?.default_model ??
                       draft.providers.find((p) => p.id === pid)?.models[0] ?? null)
                    : null,
                });
              }}
            >
              <option value="">（不启用）</option>
              {draft.providers
                .filter((p) => p.enabled !== false)
                .map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
            </Select>
          </div>
          {draft.vision_provider_id && (
            <div className="space-y-1">
              <label className="text-xs text-muted-foreground">模型</label>
              <Select
                value={draft.vision_model ?? ""}
                onChange={(e) =>
                  setDraft({ ...draft, vision_model: e.target.value || null })
                }
              >
                <option value="">（请选择）</option>
                {(draft.providers.find((p) => p.id === draft.vision_provider_id)?.models ?? []).map(
                  (m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ),
                )}
              </Select>
            </div>
          )}
        </div>
      </div>

      <div className="flex justify-end gap-2 pt-3 mt-3 border-t border-border">
        <Button onClick={handleSave}>
          <Save className="w-3.5 h-3.5" />
          保存供应商配置
        </Button>
      </div>

      <OAuthDialog
        open={oauthOpen}
        mode={oauthMode}
        onOpenChange={(v) => setOauthOpen(v)}
        onSuccess={onOAuthSuccess}
      />

      <DeepseekLoginDialog
        open={deepseekLoginOpen}
        onOpenChange={setDeepseekLoginOpen}
        onSuccess={(result) => {
          // 登录得到的 token 只能给 chat.deepseek.com web 协议用，
          // 必须把 provider 切到 deepseek kind + base_url。
          const next = {
            api_key: result.token,
            account_id: result.login,
            auth_mode: "api_key" as AuthMode,
            kind: "deepseek" as ProviderKind,
            base_url: "https://chat.deepseek.com",
          };
          updateCurrent(next);
          // 默认补一份模型清单（如果当前模型字段为空）
          if (current && current.models.length === 0) {
            const defaults = [
              "deepseek-v4-pro",
              "deepseek-v4-flash",
              "deepseek-v4-pro-search",
              "deepseek-v4-flash-search",
              "deepseek-v4-vision",
            ];
            updateCurrent({
              models: defaults,
              default_model: "deepseek-v4-pro",
            });
            setModelsText(modelsToText(defaults));
          }
        }}
      />
    </>
  );
}

function PresetsGrid({
  presets,
  onPick,
}: {
  presets: ProviderPreset[];
  onPick: (p: ProviderPreset) => void;
}) {
  const groups: Record<string, ProviderPreset[]> = {};
  for (const p of presets) {
    const k =
      p.kind === "openai" ? "OpenAI 兼容" : p.kind === "anthropic" ? "Anthropic" : "Gemini";
    (groups[k] ??= []).push(p);
  }
  return (
    <div className="space-y-4 pb-2">
      {Object.entries(groups).map(([group, items]) => (
        <div key={group}>
          <div className="text-xs font-medium text-muted-foreground mb-2">
            {group}
          </div>
          <div className="grid grid-cols-2 md:grid-cols-3 gap-2">
            {items.map((p) => (
              <div
                key={p.id}
                className="rounded-lg border border-border p-3 bg-card hover:border-primary/50 transition-colors"
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="font-medium text-sm truncate">{p.name}</div>
                    <div className="text-[11px] text-muted-foreground truncate">
                      {p.note}
                    </div>
                  </div>
                  <a
                    href={p.website}
                    target="_blank"
                    rel="noreferrer"
                    className="text-muted-foreground hover:text-foreground shrink-0"
                    title="官网"
                    onClick={(e) => e.stopPropagation()}
                  >
                    <ExternalLink className="w-3.5 h-3.5" />
                  </a>
                </div>
                <div className="mt-2 text-[11px] text-muted-foreground break-all">
                  {p.base_url}
                </div>
                <div className="mt-2 flex flex-wrap gap-1">
                  {p.models.slice(0, 3).map((m) => (
                    <span
                      key={m}
                      className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground truncate max-w-[120px]"
                    >
                      {m}
                    </span>
                  ))}
                  {p.models.length > 3 && (
                    <span className="text-[10px] text-muted-foreground">
                      +{p.models.length - 3}
                    </span>
                  )}
                </div>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => onPick(p)}
                  className="w-full mt-3"
                >
                  <Plus className="w-3.5 h-3.5" />
                  添加
                </Button>
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function AuthChip({
  active,
  onClick,
  label,
  oauth,
  onOAuth,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  oauth?: boolean;
  onOAuth?: () => void;
}) {
  return (
    <div
      className={cn(
        "inline-flex items-center rounded-md border text-sm overflow-hidden",
        active ? "border-primary bg-primary/10" : "border-border"
      )}
    >
      <button
        type="button"
        onClick={onClick}
        className={cn(
          "px-3 py-1.5 hover:bg-accent/50 transition-colors",
          active && "text-primary font-medium"
        )}
      >
        {label}
      </button>
      {oauth && onOAuth && (
        <button
          type="button"
          onClick={onOAuth}
          className="px-2 py-1.5 border-l border-inherit hover:bg-accent/50 text-primary inline-flex items-center gap-1"
          title="启动 OAuth 登录"
        >
          <LogIn className="w-3.5 h-3.5" />
        </button>
      )}
    </div>
  );
}

/**
 * 按 family 分组模型列表。
 * 如果 models.dev catalog 中没有该模型的 family 信息，则根据模型 ID 前缀推断。
 */
function groupModelsByFamily(
  models: FetchedModel[],
  catalog: Record<string, CatalogEntry> | null
): { family: string; models: FetchedModel[] }[] {
  const groups = new Map<string, FetchedModel[]>();

  // 构建不带前缀的 catalog 映射（如 "anthropic/claude-sonnet-4-5" → "claude-sonnet-4-5"）
  const catalogWithoutPrefix: Record<string, CatalogEntry> = {};
  if (catalog) {
    for (const [key, value] of Object.entries(catalog)) {
      catalogWithoutPrefix[key] = value;
      const slashIdx = key.indexOf("/");
      if (slashIdx > 0) {
        const withoutPrefix = key.substring(slashIdx + 1);
        catalogWithoutPrefix[withoutPrefix] = value;
      }
    }
  }

  for (const model of models) {
    const entry = catalogWithoutPrefix?.[model.id] || catalog?.[model.id];
    let family = entry?.family;

    // 如果 catalog 中没有 family，根据模型 ID 前缀推断（去掉可能的 provider 前缀）
    if (!family) {
      const id = model.id.toLowerCase();
      const idWithoutPrefix = id.includes("/") ? id.split("/").pop() || id : id;

      if (idWithoutPrefix.startsWith("gpt-") || idWithoutPrefix.startsWith("o1") || idWithoutPrefix.startsWith("o3")) {
        family = "GPT";
      } else if (idWithoutPrefix.startsWith("claude-")) {
        if (idWithoutPrefix.includes("opus")) family = "Claude Opus";
        else if (idWithoutPrefix.includes("sonnet")) family = "Claude Sonnet";
        else if (idWithoutPrefix.includes("haiku")) family = "Claude Haiku";
        else family = "Claude";
      } else if (idWithoutPrefix.startsWith("gemini-")) {
        if (idWithoutPrefix.includes("pro")) family = "Gemini Pro";
        else if (idWithoutPrefix.includes("flash")) family = "Gemini Flash";
        else family = "Gemini";
      } else if (idWithoutPrefix.startsWith("deepseek-")) {
        if (idWithoutPrefix.includes("r1") || idWithoutPrefix.includes("reasoner")) family = "DeepSeek Reasoner";
        else family = "DeepSeek";
      } else if (idWithoutPrefix.startsWith("qwen-")) {
        family = "Qwen";
      } else {
        family = "其他";
      }
    }

    if (!groups.has(family)) {
      groups.set(family, []);
    }
    groups.get(family)!.push(model);
  }

  // 按 family 名称排序
  return Array.from(groups.entries())
    .map(([family, models]) => ({ family, models }))
    .sort((a, b) => a.family.localeCompare(b.family));
}
