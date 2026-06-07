import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { invoke, listen } from "@/desktop/bridge/transport";
import {
  Bot,
  Brain,
  ChevronRight,
  FolderOpen,
  GitBranch,
  Maximize2,
  Package,
  Plug,
  RefreshCw,
  ScrollText,
  Server,
  Settings as SettingsIcon,
  Shield,
  Sparkles,
  Trash2,
} from "lucide-react";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label, Select, Textarea } from "@/desktop/ui/components/ui/input";
import {
  DirPicker,
  PathListField,
  ToolToggleList,
} from "@/desktop/ui/components/workspaceFields";
import { SkillsPane } from "@/desktop/ui/components/SkillsPane";
import { PluginsPane } from "@/desktop/ui/components/PluginsPane";
import { HooksPane } from "@/desktop/ui/components/HooksPane";
import { ProvidersPane } from "@/desktop/ui/components/ProvidersPane";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import type {
  AppSettings,
  McpConfig,
  McpServerConfig,
  McpToolReport,
  McpTransport,
  MemoryL0,
  Provider,
  SubagentDefinition,
  SubagentScope,
} from "@/desktop/ui/types";
import { api } from "@/desktop/bridge/tauri";
import LogConsole from "@/desktop/ui/components/LogConsole";
import {
  indexMcpToolReports,
  inferMcpTransport,
  normalizeMcpConfig,
  parseMcpJson,
  toCamelMcpConfig,
} from "@/desktop/ui/lib/mcpSettings";

type TabKey = "general" | "conversation" | "models" | "providers" | "agents" | "memory" | "permissions" | "skills" | "plugins" | "hooks" | "mcp" | "logs";

const TABS: { key: TabKey; label: string; icon: typeof SettingsIcon }[] = [
  { key: "general", label: "通用", icon: SettingsIcon },
  { key: "conversation", label: "对话设置", icon: FolderOpen },
  { key: "models", label: "模型", icon: Bot },
  { key: "providers", label: "供应商", icon: Server },
  { key: "agents", label: "Agents", icon: Bot },
  { key: "memory", label: "记忆", icon: Brain },
  { key: "permissions", label: "权限", icon: Shield },
  { key: "skills", label: "Skills", icon: Sparkles },
  { key: "plugins", label: "插件", icon: Package },
  { key: "hooks", label: "Hooks", icon: GitBranch },
  { key: "mcp", label: "MCP", icon: Plug },
  { key: "logs", label: "日志", icon: ScrollText },
];

/**
 * 应用级设置弹窗：通用 / 对话设置 / Agent 配置。
 * 这里设置的是**全局默认**，新对话会继承；当前对话另外通过 SessionSettingsDialog 覆盖。
 */
export function AppSettingsDialog() {
  const {
    appSettingsOpen,
    setAppSettingsOpen,
    appSettings,
    refreshAppSettings,
    saveAppSettings,
    availableTools,
    promptsFile,
    pendingAppSettingsTab,
    setPendingAppSettingsTab,
  } = useStore();

  const [tab, setTab] = useState<TabKey>("conversation");
  const [draft, setDraft] = useState<AppSettings | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!appSettingsOpen) return;
    refreshAppSettings().catch((e) => toast.error(e.message ?? String(e)));
  }, [appSettingsOpen, refreshAppSettings]);

  // 系统托盘 "设置…" 菜单项点击会派发 hebbian://open-settings
  useEffect(() => {
    const unlistenPromise = listen("hebbian://open-settings", () => {
      setAppSettingsOpen(true);
    });
    return () => {
      unlistenPromise.then((u) => u()).catch(() => {});
    };
  }, [setAppSettingsOpen]);

  // 处理 pendingAppSettingsTab：dialog 打开时如果有指定 tab，切换到该 tab 并清空
  useEffect(() => {
    if (appSettingsOpen && pendingAppSettingsTab) {
      setTab(pendingAppSettingsTab as TabKey);
      setPendingAppSettingsTab(null);
    }
  }, [appSettingsOpen, pendingAppSettingsTab, setPendingAppSettingsTab]);

  useEffect(() => {
    if (appSettingsOpen && appSettings) {
      setDraft(JSON.parse(JSON.stringify(appSettings)));
    }
  }, [appSettingsOpen, appSettings]);

  async function handleSave() {
    if (!draft) return;
    setSaving(true);
    try {
      await saveAppSettings(draft);
      toast.success("已保存");
      setAppSettingsOpen(false);
    } catch (e: any) {
      toast.error(e.message ?? String(e));
    } finally {
      setSaving(false);
    }
  }

  if (!draft) return null;

  return (
    <Dialog
      open={appSettingsOpen}
      onOpenChange={setAppSettingsOpen}
      title="设置"
      description="应用级偏好。新对话会继承「对话设置」，当前对话可在右上角单独覆盖。"
      size="2xl"
      footer={
        <>
          <Button
            variant="outline"
            onClick={() => setAppSettingsOpen(false)}
            disabled={saving}
          >
            取消
          </Button>
          <Button onClick={handleSave} disabled={saving}>
            {saving ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      <div className="flex gap-4 h-[65vh] overflow-hidden">
        <div className="w-36 shrink-0 space-y-1 overflow-y-auto">
          {TABS.map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              type="button"
              onClick={() => setTab(key)}
              className={cn(
                "w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md text-sm transition-colors text-left",
                tab === key
                  ? "bg-accent text-accent-foreground font-medium"
                  : "text-muted-foreground hover:bg-accent/40"
              )}
            >
              <Icon className="w-4 h-4" />
              {label}
            </button>
          ))}
        </div>

        <div className={cn("flex-1 min-w-0", tab === "logs" ? "overflow-hidden" : "overflow-y-auto")}>
          {tab === "logs" ? (
            <LogPane draft={draft} setDraft={setDraft} />
          ) : (
            <div className="space-y-6 pr-1">
              {tab === "general" && (
                <GeneralPane draft={draft} setDraft={setDraft} />
              )}
              {tab === "conversation" && (
                <ConversationPane
                  draft={draft}
                  setDraft={setDraft}
                  availableTools={availableTools}
                />
              )}
              {tab === "models" && (
                <ModelsPane
                  draft={draft}
                  setDraft={setDraft}
                  prompts={promptsFile.prompts}
                />
              )}
              {tab === "providers" && (
                <ProvidersPane active={tab === "providers"} />
              )}
              {tab === "agents" && (
                <SubagentsPane workdir={draft.conversation.workdir ?? null} />
              )}
              {tab === "memory" && (
                <MemoryPane
                  settings={draft}
                  onChange={setDraft}
                  workdir={draft.conversation.workdir ?? null}
                />
              )}
              {tab === "permissions" && <PermissionsPane />}
              {tab === "skills" && (
                <SkillsPane workdir={draft.conversation.workdir ?? null} scope="global" />
              )}
              {tab === "plugins" && <PluginsPane />}
              {tab === "hooks" && <HooksPane />}
              {tab === "mcp" && <McpPane />}
            </div>
          )}
        </div>
      </div>
    </Dialog>
  );
}

type McpDraft = {
  name: string;
  transport: McpTransport;
  command: string;
  argsText: string;
  envText: string;
  url: string;
  headersText: string;
  disabled: boolean;
};

const blankMcpDraft: McpDraft = {
  name: "",
  transport: "stdio",
  command: "",
  argsText: "",
  envText: "",
  url: "",
  headersText: "",
  disabled: false,
};

function McpPane() {
  const [config, setConfig] = useState<McpConfig>({ mcp_servers: {} });
  const [draft, setDraft] = useState<McpDraft>(blankMcpDraft);
  const [jsonText, setJsonText] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [discovering, setDiscovering] = useState(false);
  const [toolReports, setToolReports] = useState<Record<string, McpToolReport>>({});
  const [detailServer, setDetailServer] = useState<string | null>(null);

  const refreshTools = useCallback(async () => {
    setDiscovering(true);
    try {
      const reports = await api.discoverMcpTools();
      setToolReports(indexMcpToolReports(reports));
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setDiscovering(false);
    }
  }, []);

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const cfg = await api.getMcpConfig();
      const normalized = normalizeMcpConfig(cfg);
      setConfig(normalized);
      setJsonText(JSON.stringify(toCamelMcpConfig(normalized), null, 2));
      await refreshTools();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, [refreshTools]);

  useEffect(() => {
    reload();
  }, [reload]);

  async function saveConfig(next: McpConfig) {
    setSaving(true);
    try {
      const normalized = normalizeMcpConfig(next);
      await api.saveMcpConfig(normalized);
      setConfig(normalized);
      setJsonText(JSON.stringify(toCamelMcpConfig(normalized), null, 2));
      await refreshTools();
      toast.success("MCP 已保存");
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setSaving(false);
    }
  }

  async function addFromForm() {
    const name = draft.name.trim();
    if (!name) {
      toast.error("先给这个服务起个名字");
      return;
    }
    try {
      const server = draftToServer(draft);
      await saveConfig({
        mcp_servers: {
          ...config.mcp_servers,
          [name]: server,
        },
      });
      setDraft(blankMcpDraft);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function importJson() {
    try {
      const parsed = parseMcpJson(jsonText);
      await saveConfig(parsed);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function removeServer(name: string) {
    const next = { ...config.mcp_servers };
    delete next[name];
    await saveConfig({ mcp_servers: next });
  }

  function toggleServer(name: string) {
    const cur = config.mcp_servers[name];
    if (!cur) return;
    void saveConfig({
      mcp_servers: {
        ...config.mcp_servers,
        [name]: { ...cur, disabled: !cur.disabled },
      },
    });
  }

  const servers = Object.entries(config?.mcp_servers ?? {});
  const detailReport = detailServer ? toolReports[detailServer] : null;

  return (
    <div className="space-y-5">
      <section className="space-y-3">
        <div className="flex items-center justify-between">
          <Label>已添加</Label>
          <div className="flex items-center gap-2">
            {(loading || discovering) && (
              <span className="text-xs text-muted-foreground">
                {loading ? "加载中…" : "发现工具中…"}
              </span>
            )}
            <Button
              variant="outline"
              size="sm"
              onClick={refreshTools}
              disabled={loading || discovering || saving}
            >
              <RefreshCw className="h-3.5 w-3.5" />
              刷新工具
            </Button>
          </div>
        </div>
        {servers.length === 0 ? (
          <div className="rounded-md border border-dashed px-3 py-4 text-center text-xs text-muted-foreground">
            还没有 MCP 服务
          </div>
        ) : (
          <div className="space-y-2">
            {servers.map(([name, server]) => {
              const report = toolReports[name];
              const toolCount = report?.tools?.length ?? 0;
              const hasError = Boolean(report?.error);
              return (
                <div key={name} className="rounded-md border bg-background p-3">
                  <div className="flex items-start gap-3">
                    <input
                      type="checkbox"
                      checked={!server.disabled}
                      onChange={() => toggleServer(name)}
                      className="mt-1 h-4 w-4 rounded"
                      aria-label={server.disabled ? "启用服务" : "停用服务"}
                    />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{name}</span>
                        <span className="rounded border px-1.5 py-0.5 text-[11px] text-muted-foreground">
                          {transportLabel(server.transport ?? inferMcpTransport(server))}
                        </span>
                        <span
                          className={cn(
                            "rounded border px-1.5 py-0.5 text-[11px]",
                            hasError
                              ? "border-destructive/40 text-destructive"
                              : "text-muted-foreground"
                          )}
                        >
                          {server.disabled
                            ? "已停用"
                            : hasError
                              ? "发现失败"
                              : `${toolCount} 个工具`}
                        </span>
                      </div>
                      <p className="mt-1 break-all text-xs text-muted-foreground">
                        {server.transport === "stdio" || inferMcpTransport(server) === "stdio"
                          ? [server.command, ...(server.args ?? [])].filter(Boolean).join(" ")
                          : server.url}
                      </p>
                    </div>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setDetailServer(name)}
                      disabled={discovering}
                    >
                      详情
                      <ChevronRight className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => removeServer(name)}
                      disabled={saving}
                    >
                      删除
                    </Button>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </section>

      <section className="space-y-2">
        <Label>表单添加</Label>
        <div className="grid gap-2 sm:grid-cols-2">
          <Input
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            placeholder="名称"
          />
          <Select
            value={draft.transport}
            onChange={(e) =>
              setDraft({ ...draft, transport: e.target.value as McpTransport })
            }
          >
            <option value="stdio">本地命令</option>
            <option value="streamable_http">HTTP</option>
            <option value="sse">SSE</option>
          </Select>
        </div>
        {draft.transport === "stdio" ? (
          <div className="grid gap-2 sm:grid-cols-2">
            <Input
              value={draft.command}
              onChange={(e) => setDraft({ ...draft, command: e.target.value })}
              placeholder="命令，例如 npx"
            />
            <Input
              value={draft.argsText}
              onChange={(e) => setDraft({ ...draft, argsText: e.target.value })}
              placeholder="参数，用空格分隔"
            />
          </div>
        ) : (
          <Input
            value={draft.url}
            onChange={(e) => setDraft({ ...draft, url: e.target.value })}
            placeholder="https://example.com/mcp"
          />
        )}
        <div className="grid gap-2 sm:grid-cols-2">
          <Textarea
            value={draft.envText}
            onChange={(e) => setDraft({ ...draft, envText: e.target.value })}
            placeholder={"环境变量，每行一个\nKEY=value"}
            className="min-h-[84px]"
          />
          <Textarea
            value={draft.headersText}
            onChange={(e) => setDraft({ ...draft, headersText: e.target.value })}
            placeholder={"请求头，每行一个\nAuthorization=Bearer ..."}
            className="min-h-[84px]"
          />
        </div>
        <Button onClick={addFromForm} disabled={saving}>
          添加
        </Button>
      </section>

      <section className="space-y-2">
        <Label>粘贴 JSON</Label>
        <Textarea
          value={jsonText}
          onChange={(e) => setJsonText(e.target.value)}
          className="min-h-[220px] font-mono text-xs"
          spellCheck={false}
        />
        <Button variant="outline" onClick={importJson} disabled={saving}>
          保存 JSON
        </Button>
      </section>
      <Dialog
        open={detailServer !== null}
        onOpenChange={(open) => {
          if (!open) setDetailServer(null);
        }}
        title={detailServer ? `${detailServer} 的工具` : "工具"}
        description={
          detailReport?.error
            ? "这个服务暂时没能返回工具列表。"
            : `${detailReport?.tools?.length ?? 0} 个可用工具`
        }
        size="lg"
      >
        {detailReport?.error ? (
          <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
            {detailReport.error}
          </div>
        ) : detailReport && detailReport.tools.length > 0 ? (
          <div className="space-y-2">
            {detailReport.tools.map((tool) => (
              <div key={tool.runtime_name || tool.name} className="rounded-md border p-3">
                <div className="font-medium">{tool.name}</div>
                <p className="mt-1 text-sm text-muted-foreground">
                  {tool.description.trim() || "这个工具没有描述。"}
                </p>
              </div>
            ))}
          </div>
        ) : (
          <div className="rounded-md border border-dashed px-3 py-4 text-center text-sm text-muted-foreground">
            还没有发现工具
          </div>
        )}
      </Dialog>
    </div>
  );
}

// ─── OpenChamber 风格的一致性表单行 ──────────────────────────────────
// 每个设置项等宽等高：左边固定 48 单位宽的 label，右边弹性控件区。
function FieldRow({
  label,
  description,
  children,
}: {
  label: string;
  description?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5 py-1.5 sm:flex-row sm:items-start sm:gap-4">
      <div className="flex min-w-0 flex-col sm:w-48 shrink-0 pt-0.5">
        <span className="text-sm font-medium text-foreground">{label}</span>
        {description && (
          <span className="text-xs text-muted-foreground mt-0.5">{description}</span>
        )}
      </div>
      <div className="flex min-w-0 flex-1 items-center gap-2">
        {children}
      </div>
    </div>
  );
}

type PaneProps = {
  draft: AppSettings;
  setDraft: (s: AppSettings) => void;
};

function GeneralPane({ draft, setDraft }: PaneProps) {
  const debugEnabled = useStore((s) => s.debugEnabled);
  const setDebugEnabled = useStore((s) => s.setDebugEnabled);
  // 收集所有 provider 的 model id 去重——自动模式判官白名单从这里勾选。
  const [allModelIds, setAllModelIds] = useState<string[]>([]);
  useEffect(() => {
    api.getProviders().then((f) => {
      const ids = new Set<string>();
      for (const p of f.providers) for (const m of p.models ?? []) ids.add(m);
      setAllModelIds([...ids].sort());
    });
  }, []);
  const automodeModels = draft.general.automode_models ?? [];
  const toggleAutomodeModel = (id: string) => {
    const next = automodeModels.includes(id)
      ? automodeModels.filter((x) => x !== id)
      : [...automodeModels, id];
    setDraft({ ...draft, general: { ...draft.general, automode_models: next } });
  };
  return (
    <div className="space-y-1">
      <FieldRow label="开机启动" description="登录时自动启动 Hebbian">
        <input
          type="checkbox"
          checked={draft.general.launch_at_login}
          onChange={(e) =>
            setDraft({
              ...draft,
              general: { ...draft.general, launch_at_login: e.target.checked },
            })
          }
          className="h-4 w-4 rounded"
        />
      </FieldRow>

      <FieldRow label="显示 Grep 位置" description="在搜索代码的结果里显示这次查的是哪个文件夹">
        <input
          type="checkbox"
          checked={draft.general.show_grep_search_path}
          onChange={(e) =>
            setDraft({
              ...draft,
              general: {
                ...draft.general,
                show_grep_search_path: e.target.checked,
              },
            })
          }
          className="h-4 w-4 rounded"
        />
      </FieldRow>

      <FieldRow label="命令 Shell" description="运行命令前先用这个 Shell 读取你的 PATH；留空时使用系统默认 Shell">
        <input
          type="text"
          value={draft.general.shell ?? ""}
          onChange={(e) =>
            setDraft({
              ...draft,
              general: {
                ...draft.general,
                shell: e.target.value.trim() ? e.target.value : null,
              },
            })
          }
          placeholder="/bin/zsh"
          className="w-72 rounded-md border border-border bg-background px-2 py-1 text-sm"
        />
      </FieldRow>

      <FieldRow label="Debug 日志" description="开启后右侧工作台会显示 Model I/O 入口，便于查看模型请求/响应原文">
        <input
          type="checkbox"
          checked={debugEnabled}
          onChange={(e) => setDebugEnabled(e.target.checked)}
          className="h-4 w-4 rounded"
        />
      </FieldRow>

      <FieldRow
        label="文件编辑方式"
        description="切换 Edit 工具的实现。「精确替换」适合小块精确改动；「行号 patch」在大文件局部改动时更省 token，下一次对话生效"
      >
        <select
          value={draft.general.edit_backend ?? "string-replace"}
          onChange={(e) =>
            setDraft({
              ...draft,
              general: {
                ...draft.general,
                edit_backend: e.target.value as "string-replace" | "hashline",
              },
            })
          }
          className="rounded-md border border-border bg-background px-2 py-1 text-sm"
        >
          <option value="string-replace">精确替换（默认）</option>
          <option value="hashline">行号 patch（实验）</option>
        </select>
      </FieldRow>

      <FieldRow
        label="回答中断后点「继续」的方式"
        description="模型回答被截断、被拒、或请求失败时，输入框上方会出现「继续」。「自动续跑」最省心：直接接着上次跑，失败就重发、截断就接着写；也可改成先发一条「继续」消息，或只把光标移到输入框让你自己改写"
      >
        <select
          value={draft.general.continue_strategy ?? "resume_loop"}
          onChange={(e) =>
            setDraft({
              ...draft,
              general: {
                ...draft.general,
                continue_strategy: e.target.value as
                  | "resume_loop"
                  | "send_continue"
                  | "manual",
              },
            })
          }
          className="rounded-md border border-border bg-background px-2 py-1 text-sm"
        >
          <option value="resume_loop">自动续跑（默认）</option>
          <option value="send_continue">发一条「继续」消息</option>
          <option value="manual">手动续（聚焦输入框）</option>
        </select>
      </FieldRow>

      <FieldRow
        label="自动模式可用的模型"
        description="勾选的模型在「自动模式」下会自己判断命令安不安全、替你放行；没勾的模型切到自动模式时会提示并转成手动审批。判断质量取决于模型能力，建议用较强的模型"
      >
        <div className="w-72 max-h-[168px] overflow-y-auto rounded-md border border-border bg-background p-1.5 space-y-0.5">
          {allModelIds.length === 0 ? (
            <div className="text-xs text-muted-foreground px-1 py-1">
              还没有可选模型，先去「模型」里添加
            </div>
          ) : (
            allModelIds.map((id) => (
              <label
                key={id}
                className="flex items-center gap-2 px-1.5 py-1 rounded text-sm cursor-pointer hover:bg-muted/50 select-none"
              >
                <input
                  type="checkbox"
                  checked={automodeModels.includes(id)}
                  onChange={() => toggleAutomodeModel(id)}
                  className="h-4 w-4 rounded shrink-0"
                />
                <span className="font-mono text-xs truncate">{id}</span>
              </label>
            ))
          )}
        </div>
      </FieldRow>
    </div>
  );
}

function ConversationPane({
  draft,
  setDraft,
  availableTools,
}: PaneProps & { availableTools: { name: string; description: string }[] }) {
  const conv = draft.conversation;
  const updateConv = (patch: Partial<typeof conv>) =>
    setDraft({ ...draft, conversation: { ...conv, ...patch } });

  return (
    <div className="space-y-1">
      <FieldRow label="默认工作目录" description="新建对话默认的 workdir">
        <DirPicker
          value={conv.workdir ?? ""}
          onChange={(v) => updateConv({ workdir: v || null })}
          placeholder="~/"
        />
      </FieldRow>

      <div className="pt-2">
        <PathListField
          label="允许访问的路径"
          paths={conv.allowed_paths}
          onChange={(paths) => updateConv({ allowed_paths: paths })}
          allowFiles
        />
      </div>

      <div className="pt-2">
        <ToolToggleList
          label="默认启用的工具"
          availableTools={availableTools}
          enabled={conv.enabled_tools}
          onChange={(next) => updateConv({ enabled_tools: next ?? [] })}
        />
      </div>
    </div>
  );
}

// ─── 长期记忆配置（架构 §4.14）──────────────────────────────────────
function MemoryPane({
  settings,
  onChange,
  workdir,
}: {
  settings: AppSettings;
  onChange: (s: AppSettings) => void;
  workdir: string | null;
}) {
  const [providers, setProviders] = useState<Provider[]>([]);
  const [testing, setTesting] = useState<string | null>(null);
  // 已沉淀记忆（架构 §4.14）：列出 L0，点开按需读全文。
  const [memories, setMemories] = useState<MemoryL0[]>([]);
  const [openId, setOpenId] = useState<string | null>(null);
  const [detail, setDetail] = useState<Record<string, string>>({});

  useEffect(() => {
    api.getProviders().then((f) => setProviders(f.providers));
  }, []);

  useEffect(() => {
    api.listMemories(workdir).then(setMemories).catch(() => setMemories([]));
  }, [workdir]);

  const toggleMemory = async (id: string) => {
    if (openId === id) {
      setOpenId(null);
      return;
    }
    setOpenId(id);
    if (detail[id] === undefined) {
      try {
        const body = await api.readMemory(id, workdir);
        setDetail((d) => ({ ...d, [id]: body }));
      } catch (e) {
        setDetail((d) => ({ ...d, [id]: `读取失败：${e}` }));
      }
    }
  };

  const globalMems = memories.filter((m) => m.id.startsWith("global/"));
  const projectMems = memories.filter((m) => m.id.startsWith("proj/"));

  const renderMemoryRow = (m: MemoryL0) => (
    <div key={m.id} className="rounded border">
      <button
        type="button"
        onClick={() => toggleMemory(m.id)}
        className="flex w-full items-start gap-2 px-2 py-1.5 text-left text-sm hover:bg-accent/40"
      >
        <span className="mt-0.5 shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
          {m.category || "其他"}
        </span>
        <span className="min-w-0 flex-1">{m.summary}</span>
        <ChevronRight
          className={cn(
            "mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform",
            openId === m.id && "rotate-90"
          )}
        />
      </button>
      {openId === m.id && (
        <pre className="max-h-60 overflow-auto whitespace-pre-wrap border-t bg-muted/30 px-2 py-1.5 text-xs text-foreground/90">
          {detail[m.id] ?? "加载中…"}
        </pre>
      )}
    </div>
  );

  const addModel = async (providerId: string, model: string) => {
    // 实测连通
    setTesting(`${providerId}/${model}`);
    try {
      const p = providers.find((x) => x.id === providerId);
      if (!p) throw new Error("provider 不存在");
      // 连通失败时 testProviderModel 直接 reject，落到下面 catch；resolve 即视为成功。
      await api.testProviderModel(p, model);
      onChange({
        ...settings,
        memory: {
          ...settings.memory,
          models: [...settings.memory.models, { provider_id: providerId, model }],
        },
      });
      toast.success("已添加");
    } catch (e) {
      toast.error(`实测失败: ${e}`);
    } finally {
      setTesting(null);
    }
  };

  const removeModel = (idx: number) => {
    onChange({
      ...settings,
      memory: {
        ...settings.memory,
        models: settings.memory.models.filter((_, i) => i !== idx),
      },
    });
  };

  const moveModel = (idx: number, dir: "up" | "down") => {
    const arr = [...settings.memory.models];
    const target = dir === "up" ? idx - 1 : idx + 1;
    if (target < 0 || target >= arr.length) return;
    [arr[idx], arr[target]] = [arr[target], arr[idx]];
    onChange({ ...settings, memory: { ...settings.memory, models: arr } });
  };

  return (
    <div className="space-y-6">
      {/* 总开关 */}
      <div className="flex items-center justify-between">
        <div>
          <div className="font-medium">启用长期记忆</div>
          <div className="text-sm text-muted-foreground">
            关闭后不注入记忆清单、不跑后台抽取（手动 ReadMemory/WriteMemory 不受影响）
          </div>
        </div>
        <input
          type="checkbox"
          checked={settings.memory.enabled}
          onChange={(e) =>
            onChange({
              ...settings,
              memory: { ...settings.memory, enabled: e.target.checked },
            })
          }
          className="h-4 w-4"
        />
      </div>

      {/* 抽取模型 fallback 链 */}
      <div>
        <div className="font-medium mb-2">抽取模型（按序 fallback）</div>
        <div className="text-sm text-muted-foreground mb-3">
          后台抽取按顺序尝试，每个模型最多重试 5 次。添加时会实测连通性。
        </div>
        <div className="space-y-2">
          {settings.memory.models.map((m, i) => (
            <div key={i} className="flex items-center gap-2 p-2 border rounded">
              <span className="flex-1 text-sm">
                {m.provider_id} / {m.model}
              </span>
              <button
                type="button"
                onClick={() => moveModel(i, "up")}
                disabled={i === 0}
                className="text-xs px-2 py-1 disabled:opacity-30"
              >
                ↑
              </button>
              <button
                type="button"
                onClick={() => moveModel(i, "down")}
                disabled={i === settings.memory.models.length - 1}
                className="text-xs px-2 py-1 disabled:opacity-30"
              >
                ↓
              </button>
              <button
                type="button"
                onClick={() => removeModel(i)}
                className="text-xs px-2 py-1 text-red-600"
              >
                删除
              </button>
            </div>
          ))}
          {settings.memory.models.length === 0 && (
            <div className="text-sm text-muted-foreground">
              未配置模型 = 不跑后台抽取（等同关闭）
            </div>
          )}
        </div>
        <div className="mt-3 flex gap-2">
          <select
            id="mem-provider"
            className="flex-1 px-2 py-1 border rounded text-sm"
            defaultValue=""
          >
            <option value="" disabled>
              选择 provider
            </option>
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name || p.id}
              </option>
            ))}
          </select>
          <input
            id="mem-model"
            type="text"
            placeholder="model id"
            className="flex-1 px-2 py-1 border rounded text-sm"
          />
          <button
            type="button"
            onClick={() => {
              const pSel = document.getElementById("mem-provider") as HTMLSelectElement;
              const mInput = document.getElementById("mem-model") as HTMLInputElement;
              const pid = pSel.value;
              const mid = mInput.value.trim();
              if (!pid || !mid) {
                toast.error("请选择 provider 并填写 model");
                return;
              }
              addModel(pid, mid);
              mInput.value = "";
            }}
            disabled={!!testing}
            className="px-3 py-1 bg-blue-600 text-white rounded text-sm disabled:opacity-50"
          >
            {testing ? "实测中..." : "添加"}
          </button>
        </div>
      </div>

      {/* 全局记忆列表 */}
      <div>
        <div className="font-medium mb-2">全局记忆（{globalMems.length}）</div>
        {globalMems.length === 0 ? (
          <div className="text-sm text-muted-foreground">还没有沉淀任何全局记忆</div>
        ) : (
          <div className="space-y-1.5">{globalMems.map(renderMemoryRow)}</div>
        )}
      </div>

      {/* 项目记忆列表（当前对话绑定项目时显示） */}
      {workdir && (
        <div>
          <div className="font-medium mb-2">项目记忆（{projectMems.length}）</div>
          <div className="text-sm text-muted-foreground mb-2">当前项目：{workdir}</div>
          {projectMems.length === 0 ? (
            <div className="text-sm text-muted-foreground">
              这个项目还没有沉淀记忆——聊几轮后台会自动记，或让我用 WriteMemory 记
            </div>
          ) : (
            <div className="space-y-1.5">{projectMems.map(renderMemoryRow)}</div>
          )}
        </div>
      )}
    </div>
  );
}

// ─── 权限管理（全局 allow / deny + 路径白名单）─────────────────────────
function PermissionsPane() {
  const [allow, setAllow] = useState<string[]>([]);
  const [deny, setDeny] = useState<string[]>([]);
  const [paths, setPaths] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [newAllow, setNewAllow] = useState("");
  const [newDeny, setNewDeny] = useState("");
  const [newPath, setNewPath] = useState("");

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const [a, d, ps] = await Promise.all([
        invoke<string[]>("list_permissions", { scope: "global", effect: "allow" }),
        invoke<string[]>("list_permissions", { scope: "global", effect: "deny" }),
        invoke<string[]>("list_permission_paths", { scope: "global" }),
      ]);
      setAllow(a);
      setDeny(d);
      setPaths(ps);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  async function addPattern(effect: "allow" | "deny", pattern: string) {
    const p = pattern.trim();
    if (!p) return;
    try {
      await invoke("add_permission", { scope: "global", effect, pattern: p });
      if (effect === "allow") setNewAllow("");
      else setNewDeny("");
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function removePattern(effect: "allow" | "deny", pattern: string) {
    try {
      await invoke("remove_permission", { scope: "global", effect, pattern });
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function addPath() {
    const p = newPath.trim();
    if (!p) return;
    try {
      await invoke("add_permission_path", { scope: "global", path: p });
      setNewPath("");
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function removePath(p: string) {
    try {
      await invoke("remove_permission_path", { scope: "global", path: p });
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  return (
    <div className="space-y-5">
      <div className="space-y-1">
        <Label>规则语法</Label>
        <p className="text-xs text-muted-foreground">
          每条 pattern 形如 <span className="font-mono">Tool(arg)</span> 或 <span className="font-mono">Tool</span>（任意调用）。例：<br />
          <span className="font-mono">Bash(git status)</span> · <span className="font-mono">Bash(rm:/tmp/)</span> · <span className="font-mono">Edit(/Users/x/proj)</span> · <span className="font-mono">WebFetch(github.com)</span>
        </p>
      </div>

      <PatternList
        title="允许 (allow)"
        emptyHint="允许 agent 自动执行命中此 pattern 的工具调用"
        items={allow}
        value={newAllow}
        setValue={setNewAllow}
        onAdd={() => addPattern("allow", newAllow)}
        onRemove={(p) => removePattern("allow", p)}
        accent="allow"
        loading={loading}
      />

      <PatternList
        title="拒绝 (deny)"
        emptyHint="命中此 pattern 的调用直接拒绝（优先级高于 allow）"
        items={deny}
        value={newDeny}
        setValue={setNewDeny}
        onAdd={() => addPattern("deny", newDeny)}
        onRemove={(p) => removePattern("deny", p)}
        accent="deny"
        loading={loading}
      />

      <section className="space-y-2">
        <Label>全局允许的路径（paths 白名单）</Label>
        <p className="text-xs text-muted-foreground">
          扩展 agent 可访问的目录或文件。effects 中的路径前缀命中此列表 → 不触发 PathAccess 审批。
        </p>
        <div className="flex gap-2">
          <Input
            value={newPath}
            onChange={(e) => setNewPath(e.target.value)}
            placeholder="/abs/path/to/dir 或 文件"
          />
          <Button onClick={addPath} disabled={!newPath.trim()}>
            添加
          </Button>
        </div>
        {paths.length === 0 ? (
          <p className="text-xs text-muted-foreground">暂无</p>
        ) : (
          <ul className="space-y-1">
            {paths.map((p) => (
              <li
                key={p}
                className="flex items-center justify-between px-2 py-1 rounded border text-sm"
              >
                <span className="font-mono break-all">{p}</span>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => removePath(p)}
                  aria-label={`删除 ${p}`}
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

function PatternList({
  title,
  emptyHint,
  items,
  value,
  setValue,
  onAdd,
  onRemove,
  accent,
  loading,
}: {
  title: string;
  emptyHint: string;
  items: string[];
  value: string;
  setValue: (v: string) => void;
  onAdd: () => void;
  onRemove: (p: string) => void;
  accent: "allow" | "deny";
  loading: boolean;
}) {
  return (
    <section className="space-y-2">
      <div className="flex items-center justify-between">
        <Label>{title}</Label>
        {loading && <span className="text-xs text-muted-foreground">加载中…</span>}
      </div>
      <p className="text-xs text-muted-foreground">{emptyHint}</p>
      <div className="flex gap-2">
        <Input
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="例：Bash(git status)"
          onKeyDown={(e) => {
            if (e.key === "Enter") onAdd();
          }}
        />
        <Button onClick={onAdd} disabled={!value.trim()}>
          添加
        </Button>
      </div>
      {items.length === 0 ? (
        <p className="text-xs text-muted-foreground">暂无</p>
      ) : (
        <ul className="space-y-1">
          {items.map((p) => (
            <li
              key={p}
              className={cn(
                "flex items-center justify-between px-2 py-1 rounded border text-sm",
                accent === "allow"
                  ? "border-emerald-500/30 bg-emerald-500/5"
                  : "border-red-500/30 bg-red-500/5"
              )}
            >
              <span className="font-mono break-all">{p}</span>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => onRemove(p)}
                aria-label={`删除 ${p}`}
              >
                <Trash2 className="w-3.5 h-3.5" />
              </Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function transportLabel(transport: McpTransport) {
  if (transport === "stdio") return "本地命令";
  if (transport === "streamable_http") return "HTTP";
  return "SSE";
}

function draftToServer(draft: McpDraft): McpServerConfig {
  const transport = draft.transport;
  if (transport === "stdio" && !draft.command.trim()) {
    throw new Error("本地命令不能为空");
  }
  if (transport !== "stdio" && !draft.url.trim()) {
    throw new Error("URL 不能为空");
  }
  return {
    name: draft.name.trim(),
    transport,
    command: transport === "stdio" ? draft.command.trim() : null,
    args: transport === "stdio" ? splitArgs(draft.argsText) : [],
    env: parseKeyValueLines(draft.envText),
    url: transport === "stdio" ? null : draft.url.trim(),
    headers: parseKeyValueLines(draft.headersText),
    disabled: draft.disabled,
  };
}

function parseKeyValueLines(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    const idx = line.indexOf("=");
    if (idx <= 0) {
      throw new Error(`格式不对：${line}`);
    }
    out[line.slice(0, idx).trim()] = line.slice(idx + 1).trim();
  }
  return out;
}

function splitArgs(text: string): string[] {
  const trimmed = text.trim();
  return trimmed ? trimmed.split(/\s+/) : [];
}


function ModelsPane({
  draft,
  setDraft,
  prompts,
}: PaneProps & { prompts: { id: string; name: string }[] }) {
  return (
    <div className="space-y-1">
      <FieldRow label="默认 Prompt" description="新建对话自动使用的 system prompt">
        <Select
          value={draft.agents.default_prompt_id ?? ""}
          onChange={(e) =>
            setDraft({
              ...draft,
              agents: {
                ...draft.agents,
                default_prompt_id: e.target.value || null,
              },
            })
          }
        >
          <option value="">（无）</option>
          {prompts.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>
      </FieldRow>
    </div>
  );
}

// ─── Subagents 面板（P6）────────────────────────────────────────────────────

const SUBAGENT_TEMPLATE = `---
description: "描述这个 agent 的用途"
---
你是一个专注于...的助手。
`;

function buildSubagentContent(def: SubagentDefinition): string {
  let fm = `---\ndescription: "${def.description}"`;
  if (def.tools && def.tools.length > 0) {
    fm += `\ntools: [${def.tools.join(", ")}]`;
  }
  if (def.model) fm += `\nmodel: ${def.model}`;
  if (def.max_iterations != null) fm += `\nmax_iterations: ${def.max_iterations}`;
  fm += `\n---\n${def.system_prompt}`;
  return fm;
}

function SubagentsPane({ workdir }: { workdir: string | null }) {
  const [subagents, setSubagents] = useState<SubagentDefinition[]>([]);
  const [loading, setLoading] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [editContent, setEditContent] = useState("");
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [newContent, setNewContent] = useState(SUBAGENT_TEMPLATE);
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const defs = await invoke<SubagentDefinition[]>("list_subagents", {
        workdir: workdir ?? null,
      });
      setSubagents(defs);
    } catch (e) {
      toast.error(String(e));
    } finally {
      setLoading(false);
    }
  }, [workdir]);

  useEffect(() => {
    load();
  }, [load]);

  const toggleEnabled = async (def: SubagentDefinition) => {
    const scope: SubagentScope = workdir ? { Project: workdir } : "Global";
    try {
      await invoke("set_subagent_enabled", {
        name: def.name,
        scope,
        enabled: !def.enabled,
      });
      await load();
    } catch (e) {
      toast.error(String(e));
    }
  };

  const startEdit = (def: SubagentDefinition) => {
    setEditing(def.name);
    setEditContent(buildSubagentContent(def));
  };

  const saveEdit = async () => {
    if (!editing) return;
    setSaving(true);
    try {
      await invoke("save_subagent", { name: editing, content: editContent });
      setEditing(null);
      await load();
      toast.success("已保存");
    } catch (e) {
      toast.error(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (name: string) => {
    try {
      await invoke("delete_subagent", { name, workdir: workdir ?? null });
      if (editing === name) setEditing(null);
      await load();
      toast.success("已删除");
    } catch (e) {
      toast.error(String(e));
    }
  };

  const saveNew = async () => {
    const trimmed = newName.trim();
    if (!trimmed) {
      toast.error("请填写 agent 名称");
      return;
    }
    setSaving(true);
    try {
      await invoke("save_subagent", { name: trimmed, content: newContent });
      setCreating(false);
      setNewName("");
      setNewContent(SUBAGENT_TEMPLATE);
      await load();
      toast.success("已创建");
    } catch (e) {
      toast.error(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">
          定义可被 Task 工具调用的子 agent，每个 agent 有独立的 system prompt 和工具集。
        </p>
        <Button size="sm" onClick={() => { setCreating(true); setEditing(null); }}>
          新建
        </Button>
      </div>

      {creating && (
        <div className="border rounded-lg p-3 space-y-2 bg-muted/30">
          <Input
            placeholder="agent 名称（如 code-reviewer）"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
          />
          <textarea
            className="w-full h-48 text-xs font-mono border rounded p-2 bg-background resize-y focus:outline-none focus:ring-1 focus:ring-ring"
            value={newContent}
            onChange={(e) => setNewContent(e.target.value)}
          />
          <div className="flex gap-2">
            <Button size="sm" onClick={saveNew} disabled={saving}>
              {saving ? "保存中…" : "保存"}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => { setCreating(false); setNewName(""); setNewContent(SUBAGENT_TEMPLATE); }}
            >
              取消
            </Button>
          </div>
        </div>
      )}

      {loading && <p className="text-sm text-muted-foreground">加载中…</p>}

      {subagents.map((def) => (
        <div key={def.name} className="border rounded-lg p-3 space-y-2">
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={def.enabled}
              onChange={() => toggleEnabled(def)}
              className="h-4 w-4 cursor-pointer"
            />
            <span className="font-medium text-sm">{def.name}</span>
            <span className="text-xs text-muted-foreground flex-1 truncate">{def.description}</span>
            <Button
              size="sm"
              variant="ghost"
              className="h-6 px-2 text-xs"
              onClick={() => editing === def.name ? setEditing(null) : startEdit(def)}
            >
              {editing === def.name ? "收起" : "编辑"}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="h-6 px-2 text-destructive hover:text-destructive"
              onClick={() => handleDelete(def.name)}
            >
              <Trash2 className="h-3 w-3" />
            </Button>
          </div>

          {editing === def.name && (
            <div className="space-y-2 pt-1">
              <textarea
                className="w-full h-48 text-xs font-mono border rounded p-2 bg-background resize-y focus:outline-none focus:ring-1 focus:ring-ring"
                value={editContent}
                onChange={(e) => setEditContent(e.target.value)}
              />
              <div className="flex gap-2">
                <Button size="sm" onClick={saveEdit} disabled={saving}>
                  {saving ? "保存中…" : "保存"}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setEditing(null)}>
                  取消
                </Button>
              </div>
            </div>
          )}
        </div>
      ))}

      {!loading && subagents.length === 0 && !creating && (
        <p className="text-sm text-muted-foreground text-center py-6">
          还没有 agent 定义。点击「新建」创建第一个。
        </p>
      )}
    </div>
  );
}

// ─── 日志面板（DOM 日志控制台，搜索/等级过滤/虚拟滚动在 LogConsole 内）──
function LogPane({ draft, setDraft }: PaneProps) {
  const setLogEnabled = useStore((s) => s.setLogEnabled);

  const logEnabled = draft.general.log_enabled;
  const todayStr = new Date().toISOString().slice(0, 10);
  const logPath = `~/.hebbian/logs/hebbian.log.${todayStr}`;

  function handleToggle(checked: boolean) {
    setDraft({ ...draft, general: { ...draft.general, log_enabled: checked } });
    setLogEnabled(checked);
  }

  return (
    <div className="h-full flex flex-col gap-3 pr-1">
      <div className="flex items-center justify-between shrink-0">
        <label className="flex items-center gap-2 text-sm cursor-pointer select-none">
          <input
            type="checkbox"
            checked={logEnabled}
            onChange={(e) => handleToggle(e.target.checked)}
            className="h-4 w-4 rounded"
          />
          实时日志
          <span className="text-xs text-muted-foreground font-mono">{logPath}</span>
        </label>
        <button
          type="button"
          onClick={() => api.openLogViewerWindow()}
          className="rounded p-0.5 text-muted-foreground hover:text-foreground"
          title="在独立窗口中查看"
        >
          <Maximize2 className="h-3.5 w-3.5" />
        </button>
      </div>

      {/* flex-1 撑满剩余高度 */}
      <div className="min-h-0 flex-1">
        <LogConsole />
      </div>
    </div>
  );
}
