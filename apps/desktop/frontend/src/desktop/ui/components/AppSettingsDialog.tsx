import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { invoke, listen } from "@/desktop/bridge/transport";
import {
  Bot,
  ChevronRight,
  FolderOpen,
  Plug,
  RefreshCw,
  ScrollText,
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
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import type {
  AppSettings,
  McpConfig,
  McpServerConfig,
  McpToolReport,
  McpTransport,
} from "@/desktop/ui/types";
import { api } from "@/desktop/bridge/tauri";
import { init as initGhostty, Terminal, FitAddon } from "ghostty-web";
import {
  indexMcpToolReports,
  inferMcpTransport,
  normalizeMcpConfig,
  parseMcpJson,
  toCamelMcpConfig,
} from "@/desktop/ui/lib/mcpSettings";

type TabKey = "general" | "conversation" | "agents" | "permissions" | "skills" | "mcp" | "logs";

const TABS: { key: TabKey; label: string; icon: typeof SettingsIcon }[] = [
  { key: "general", label: "通用", icon: SettingsIcon },
  { key: "conversation", label: "对话设置", icon: FolderOpen },
  { key: "agents", label: "Agent 配置", icon: Bot },
  { key: "permissions", label: "权限", icon: Shield },
  { key: "skills", label: "Skills", icon: Sparkles },
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
      size="lg"
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
              {tab === "agents" && (
                <AgentsPane
                  draft={draft}
                  setDraft={setDraft}
                  prompts={promptsFile.prompts}
                />
              )}
              {tab === "permissions" && <PermissionsPane />}
              {tab === "skills" && (
                <SkillsPane workdir={draft.conversation.workdir ?? null} scope="global" />
              )}
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

      <FieldRow label="Debug 日志" description="开启后右侧工作台会显示 Model I/O 入口，便于查看模型请求/响应原文">
        <input
          type="checkbox"
          checked={debugEnabled}
          onChange={(e) => setDebugEnabled(e.target.checked)}
          className="h-4 w-4 rounded"
        />
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


function AgentsPane({
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

// ─── 日志面板（ghostty-web 终端渲染）────────────────────────────────
const LEVEL_ANSI: Record<string, string> = {
  ERROR: "\x1b[31m",
  WARN:  "\x1b[33m",
  INFO:  "\x1b[32m",
  DEBUG: "\x1b[34m",
  TRACE: "\x1b[90m",
};

// WASM 只初始化一次；第一次打开日志面板时触发
let _wasmReady: Promise<void> | null = null;
function ensureWasm() {
  if (!_wasmReady) _wasmReady = initGhostty();
  return _wasmReady;
}

function LogPane({ draft, setDraft }: PaneProps) {
  const setLogEnabled = useStore((s) => s.setLogEnabled);
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);

  const logEnabled = draft.general.log_enabled;
  const todayStr = new Date().toISOString().slice(0, 10);
  const logPath = `~/.hebbian/logs/hebbian.log.${todayStr}`;

  useEffect(() => {
    let active = true;
    let cancelStream: (() => void) | null = null;

    ensureWasm().then(async () => {
      if (!active || !containerRef.current) return;

      const term = new Terminal({
        fontSize: 11,
        fontFamily: "ui-monospace, 'Cascadia Code', Menlo, Consolas, monospace",
        theme: {
          background: "#0a0a0a",
          foreground: "#cccccc",
          black: "#1e1e1e",   brightBlack: "#808080",
          red: "#f44747",     brightRed: "#f44747",
          green: "#6a9955",   brightGreen: "#b5cea8",
          yellow: "#dcdcaa",  brightYellow: "#dcdcaa",
          blue: "#569cd6",    brightBlue: "#9cdcfe",
          magenta: "#c586c0", brightMagenta: "#c586c0",
          cyan: "#4ec9b0",    brightCyan: "#4ec9b0",
          white: "#d4d4d4",   brightWhite: "#ffffff",
        },
        scrollback: 50000,
        disableStdin: true,
        convertEol: true,
      });

      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(containerRef.current);
      fit.fit();
      fit.observeResize();
      termRef.current = term;

      // 加载今天的历史日志文件（含 ANSI 颜色，终端直接渲染）
      try {
        const text = await api.readLogFile();
        if (active && text.trim()) {
          term.write(text);
          term.scrollToBottom();
        }
      } catch {}

      // 订阅实时 tracing 广播，按 level 附加 ANSI 颜色
      cancelStream = api.subscribeLogStream((line) => {
        if (!active) return;
        const c = LEVEL_ANSI[line.level] ?? "\x1b[0m";
        term.write(
          `${line.ts} ${c}[${line.level}]\x1b[0m \x1b[2m${line.target}\x1b[0m ${line.message}\r\n`
        );
        term.scrollToBottom();
      });
    }).catch(() => {});

    return () => {
      active = false;
      cancelStream?.();
      termRef.current?.dispose();
      termRef.current = null;
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
          onClick={() => termRef.current?.clear()}
          className="text-xs text-muted-foreground hover:text-foreground underline"
        >
          清空
        </button>
      </div>

      {/* ghostty-web 终端挂载点；flex-1 撑满剩余高度 */}
      <div ref={containerRef} className="flex-1 rounded-lg overflow-hidden" />
    </div>
  );
}
