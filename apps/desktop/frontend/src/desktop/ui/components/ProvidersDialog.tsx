import { useEffect, useState } from "react";
import { toast } from "sonner";
import {
  Activity,
  Plus,
  Trash2,
  KeyRound,
  Globe,
  Boxes,
  Check,
  Download,
  LogIn,
  Package,
  Loader2,
  ExternalLink,
} from "lucide-react";
import { nanoid } from "nanoid";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label, SecretInput, Select, Textarea } from "@/desktop/ui/components/ui/input";
import { OAuthDialog } from "./OAuthDialog";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import type {
  AuthMode,
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
};

const CODEX_OAUTH_BASE_URL = "https://chatgpt.com/backend-api/codex";
const CODEX_OAUTH_MODELS = ["gpt-5.4", "gpt-5.4-mini"];

const SUPPORTED_AUTH_MODES: Record<ProviderKind, AuthMode[]> = {
  openai: ["api_key", "oauth_codex"],
  anthropic: ["api_key", "oauth_claude_code"],
  gemini: ["api_key", "oauth_gemini_cli"],
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

export function ProvidersDialog() {
  const {
    providerDialogOpen,
    setProviderDialogOpen,
    providersFile,
    saveProviders,
  } = useStore();

  const [draft, setDraft] = useState(providersFile);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("providers");
  const [presets, setPresets] = useState<ProviderPreset[]>([]);
  const [fetching, setFetching] = useState(false);
  const [fetchedModels, setFetchedModels] = useState<FetchedModel[] | null>(null);
  const [oauthOpen, setOauthOpen] = useState(false);
  const [oauthMode, setOauthMode] = useState<AuthMode | null>(null);
  const [modelsText, setModelsText] = useState("");
  const [headersText, setHeadersText] = useState("");
  const [testingModel, setTestingModel] = useState(false);
  const [modelTest, setModelTest] = useState<{
    providerId: string;
    model: string;
    ok: boolean;
    message: string;
  } | null>(null);

  useEffect(() => {
    if (providerDialogOpen) {
      setDraft(providersFile);
      setSelectedId(providersFile.providers[0]?.id ?? null);
      setModelsText(modelsToText(providersFile.providers[0]?.models ?? []));
      setHeadersText(headersToText(providersFile.providers[0]?.extra_headers ?? {}));
      setFetchedModels(null);
      setModelTest(null);
      setTab("providers");
      api.listProviderPresets().then(setPresets).catch(() => {});
    }
  }, [providerDialogOpen, providersFile]);

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
      base_url: getOfficialBaseUrl(kind),
      api_key: "",
      refresh_token: null,
      token_expires_at: null,
      account_id: null,
      extra_headers: {},
      models: [],
      default_model: null,
      ...overrides,
    };
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

  function addFromPreset(preset: ProviderPreset) {
    const p = blankProvider(preset.kind, {
      id: nanoid(),
      name: preset.name,
      kind: preset.kind,
      base_url: preset.base_url,
      models: [...preset.models],
      default_model: preset.models[0] ?? null,
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
      toast.success("已保存");
      setProviderDialogOpen(false);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  function setDefault(id: string | null) {
    setDraft({ ...draft, default_provider_id: id });
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
    k === "openai" ? "OpenAI 兼容" : k === "anthropic" ? "Anthropic" : "Gemini";
  const modelForTest = current ? getModelForTest(current) : "";

  return (
    <>
      <Dialog
        open={providerDialogOpen}
        onOpenChange={setProviderDialogOpen}
        title="供应商配置"
        description="支持 OpenAI / Anthropic / Gemini 三种协议，可手填或 OAuth 登录"
        size="xl"
        footer={
          <>
            <Button variant="outline" onClick={() => setProviderDialogOpen(false)}>
              取消
            </Button>
            <Button onClick={handleSave}>保存</Button>
          </>
        }
      >
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
                    onClick={() => {
                      setSelectedId(p.id);
                      setModelsText(modelsToText(p.models));
                      setHeadersText(headersToText(p.extra_headers));
                      setFetchedModels(null);
                      setModelTest(null);
                    }}
                    className={cn(
                      "px-3 py-2 rounded-md cursor-pointer flex items-center justify-between gap-2",
                      selectedId === p.id
                        ? "bg-accent text-accent-foreground"
                        : "hover:bg-accent/50"
                    )}
                  >
                    <div className="min-w-0 flex-1">
                      <div className="text-sm truncate">{p.name}</div>
                      <div className="text-[11px] text-muted-foreground flex items-center gap-1">
                        <span>{kindLabel(p.kind)}</span>
                        {!p.api_key && (
                          <span className="text-amber-500">未填 Key</span>
                        )}
                      </div>
                    </div>
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
                        <Boxes className="w-3 h-3" /> 模型（每行一个）
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
                    <Textarea
                      rows={4}
                      value={modelsText}
                      spellCheck={false}
                      autoCorrect="off"
                      onChange={(e) => {
                        const text = e.target.value;
                        setModelsText(text);
                        updateCurrent({ models: parseModelsText(text) });
                      }}
                    />
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
                    {fetchedModels && (
                      <div className="rounded-md border border-border bg-accent/30 p-2 max-h-40 overflow-y-auto">
                        <div className="text-[11px] text-muted-foreground mb-1">
                          点击加入 / 移除（已选 {current.models.length}）
                        </div>
                        <div className="flex flex-wrap gap-1">
                          {fetchedModels.map((m) => {
                            const picked = current.models.includes(m.id);
                            return (
                              <button
                                key={m.id}
                                type="button"
                                onClick={() => toggleFetchedModel(m.id)}
                                className={cn(
                                  "text-xs px-2 py-1 rounded-md border transition-colors",
                                  picked
                                    ? "border-primary bg-primary/10 text-primary"
                                    : "border-border hover:bg-accent"
                                )}
                              >
                                {m.id}
                              </button>
                            );
                          })}
                        </div>
                      </div>
                    )}
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
      </Dialog>

      <OAuthDialog
        open={oauthOpen}
        mode={oauthMode}
        onOpenChange={(v) => setOauthOpen(v)}
        onSuccess={onOAuthSuccess}
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
