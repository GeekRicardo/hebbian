import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { invoke, listen } from "@/desktop/bridge/transport";
import {
  Bot,
  FolderOpen,
  Settings as SettingsIcon,
  Shield,
  Sparkles,
  Trash2,
} from "lucide-react";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label, Select } from "@/desktop/ui/components/ui/input";
import {
  DirPicker,
  PathListField,
  ToolToggleList,
} from "@/desktop/ui/components/workspaceFields";
import { SkillsPane } from "@/desktop/ui/components/SkillsPane";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import type { AppSettings } from "@/desktop/ui/types";

type TabKey = "general" | "conversation" | "agents" | "permissions" | "skills";

const TABS: { key: TabKey; label: string; icon: typeof SettingsIcon }[] = [
  { key: "general", label: "通用", icon: SettingsIcon },
  { key: "conversation", label: "对话设置", icon: FolderOpen },
  { key: "agents", label: "Agent 配置", icon: Bot },
  { key: "permissions", label: "权限", icon: Shield },
  { key: "skills", label: "Skills", icon: Sparkles },
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
      <div className="flex gap-4">
        <div className="w-36 shrink-0 space-y-1">
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

        <div className="flex-1 min-w-0 space-y-4">
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
        </div>
      </div>
    </Dialog>
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
    <div className="space-y-4">
      <label className="flex items-center gap-2 cursor-pointer select-none">
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
        <span className="text-sm">开机启动</span>
      </label>

      <div className="space-y-1">
        <label className="flex items-center gap-2 cursor-pointer select-none">
          <input
            type="checkbox"
            checked={debugEnabled}
            onChange={(e) => setDebugEnabled(e.target.checked)}
            className="h-4 w-4 rounded"
          />
          <span className="text-sm">日志（开启 debug）</span>
        </label>
        <p className="pl-6 text-xs text-muted-foreground">
          开启后右侧工作台会显示 Model I/O 入口，便于查看模型请求 / 响应原文。
        </p>
      </div>
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
    <div className="space-y-4">
      <div className="space-y-1.5">
        <Label>默认工作目录（workdir）</Label>
        <DirPicker
          value={conv.workdir ?? ""}
          onChange={(v) => updateConv({ workdir: v || null })}
          placeholder="~/"
        />
      </div>

      <PathListField
        label="允许访问的路径"
        paths={conv.allowed_paths}
        onChange={(paths) => updateConv({ allowed_paths: paths })}
        allowFiles
      />

      <ToolToggleList
        label="默认启用的工具"
        availableTools={availableTools}
        enabled={conv.enabled_tools}
        onChange={(next) => updateConv({ enabled_tools: next ?? [] })}
      />
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
      <section className="space-y-1">
        <Label>规则语法</Label>
        <p className="text-xs text-muted-foreground">
          每条 pattern 形如 <span className="font-mono">Tool(arg)</span> 或 <span className="font-mono">Tool</span>（任意调用）。例：<br />
          <span className="font-mono">Bash(git status)</span> · <span className="font-mono">Bash(rm:/tmp/)</span> · <span className="font-mono">Edit(/Users/x/proj)</span> · <span className="font-mono">WebFetch(github.com)</span>
        </p>
      </section>

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


function AgentsPane({
  draft,
  setDraft,
  prompts,
}: PaneProps & { prompts: { id: string; name: string }[] }) {
  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <Label>默认 Prompt</Label>
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
      </div>
    </div>
  );
}
