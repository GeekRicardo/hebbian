import { useEffect, useState } from "react";
import { toast } from "sonner";
import { listen } from "@tauri-apps/api/event";
import { Bot, FolderOpen, Settings as SettingsIcon } from "lucide-react";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Label, Select } from "@/desktop/ui/components/ui/input";
import {
  DirListField,
  DirPicker,
  ToolToggleList,
} from "@/desktop/ui/components/workspaceFields";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import type { AppSettings } from "@/desktop/ui/types";

type TabKey = "general" | "conversation" | "agents";

const TABS: { key: TabKey; label: string; icon: typeof SettingsIcon }[] = [
  { key: "general", label: "通用", icon: SettingsIcon },
  { key: "conversation", label: "对话设置", icon: FolderOpen },
  { key: "agents", label: "Agent 配置", icon: Bot },
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
  return (
    <div className="space-y-3">
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

      <DirListField
        label="额外允许访问的目录"
        dirs={conv.allowed_dirs}
        onChange={(dirs) => updateConv({ allowed_dirs: dirs })}
      />

      <DirListField
        label="Skill 目录"
        dirs={conv.skill_dirs}
        onChange={(dirs) => updateConv({ skill_dirs: dirs })}
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
