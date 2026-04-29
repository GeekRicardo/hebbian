import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Bot, RotateCcw, Zap } from "lucide-react";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Label, Select, Textarea } from "@/desktop/ui/components/ui/input";
import { AvatarPreview } from "@/desktop/ui/components/AvatarField";
import {
  DirListField,
  DirPicker,
  ToolToggleList,
} from "@/desktop/ui/components/workspaceFields";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import { api } from "@/desktop/bridge/tauri";

export function SessionSettingsDialog() {
  const {
    settingsOpen,
    setSettingsOpen,
    currentSession,
    providersFile,
    prompts,
    updateCurrentConfig,
    appSettings,
    refreshAppSettings,
    availableTools,
  } = useStore();

  const [providerId, setProviderId] = useState("");
  const [model, setModel] = useState("");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [promptId, setPromptId] = useState("");
  const [stream, setStream] = useState(true);

  // workspace 覆盖：null = 用全局默认；undefined = 字段被首次打开时还没初始化
  const [workdir, setWorkdir] = useState<string | null>(null);
  const [allowedDirs, setAllowedDirs] = useState<string[] | null>(null);
  const [skillDirs, setSkillDirs] = useState<string[] | null>(null);
  const [enabledTools, setEnabledTools] = useState<string[] | null>(null);

  useEffect(() => {
    if (settingsOpen && currentSession) {
      setProviderId(currentSession.provider_id);
      setModel(currentSession.model);
      setSystemPrompt(currentSession.system_prompt ?? "");
      setPromptId(currentSession.prompt_id ?? "");
      setStream(currentSession.stream);
      setWorkdir(currentSession.workdir ?? null);
      setAllowedDirs(currentSession.allowed_dirs ?? null);
      setSkillDirs(currentSession.skill_dirs ?? null);
      setEnabledTools(currentSession.enabled_tools ?? null);
      // 拉一次全局 settings 用作 placeholder
      refreshAppSettings().catch(() => {});
    }
  }, [settingsOpen, currentSession, refreshAppSettings]);

  const provider = providersFile.providers.find((p) => p.id === providerId);

  async function handleSave() {
    if (!currentSession) return;
    try {
      await updateCurrentConfig({
        provider_id: providerId,
        model,
        system_prompt: systemPrompt,
        prompt_id: promptId,
        stream,
      });
      // 单独保存 workspace 字段；空数组也算"明确清空覆盖"，需要传 null
      await api.updateSessionSettings(currentSession.id, {
        workdir,
        allowed_dirs: allowedDirs,
        skill_dirs: skillDirs,
        enabled_tools: enabledTools,
      });
      toast.success("已更新");
      setSettingsOpen(false);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  function applyPrompt(pid: string) {
    setPromptId(pid);
    const p = prompts.find((x) => x.id === pid);
    if (p) setSystemPrompt(p.content);
  }

  if (!currentSession) return null;

  const globalDefaults = appSettings?.conversation;
  const inheritedAllowedDirs = globalDefaults?.allowed_dirs ?? [];
  const inheritedSkillDirs = globalDefaults?.skill_dirs ?? [];
  const inheritedEnabledTools = globalDefaults?.enabled_tools ?? [];
  const inheritedWorkdir = globalDefaults?.workdir ?? "~/";

  return (
    <Dialog
      open={settingsOpen}
      onOpenChange={setSettingsOpen}
      title="对话设置"
      description="调整当前对话的供应商、模型、Agent、流式开关"
      size="lg"
      footer={
        <>
          <Button variant="outline" onClick={() => setSettingsOpen(false)}>
            取消
          </Button>
          <Button onClick={handleSave}>保存</Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="grid grid-cols-2 gap-3">
          <div className="space-y-1.5">
            <Label>供应商</Label>
            <Select
              value={providerId}
              onChange={(e) => {
                const id = e.target.value;
                setProviderId(id);
                const p = providersFile.providers.find((x) => x.id === id);
                if (p) setModel(p.default_model || p.models[0] || "");
              }}
            >
              {providersFile.providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name} ({p.kind})
                </option>
              ))}
            </Select>
          </div>
          <div className="space-y-1.5">
            <Label>模型</Label>
            <Select value={model} onChange={(e) => setModel(e.target.value)}>
              {(provider?.models ?? []).map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
              {provider && !provider.models.includes(model) && (
                <option value={model}>{model}</option>
              )}
            </Select>
          </div>
        </div>

        <div className="space-y-1.5">
          <Label>Agent</Label>
          <div className="grid grid-cols-3 gap-2">
            <button
              type="button"
              onClick={() => {
                setPromptId("");
              }}
              className={cn(
                "flex items-center gap-2 px-3 py-2 rounded-md border text-left text-sm transition-colors",
                promptId === ""
                  ? "border-primary bg-primary/10"
                  : "border-border hover:bg-accent"
              )}
            >
              <span className="h-6 w-6 rounded-md bg-muted flex items-center justify-center text-sm">
                ∅
              </span>
              <span className="truncate">无 Agent</span>
            </button>
            {prompts.map((p) => (
              <button
                key={p.id}
                type="button"
                onClick={() => applyPrompt(p.id)}
                className={cn(
                  "flex items-center gap-2 px-3 py-2 rounded-md border text-left text-sm transition-colors",
                  promptId === p.id
                    ? "border-primary bg-primary/10"
                    : "border-border hover:bg-accent"
                )}
                title={p.content}
              >
                <AvatarPreview
                  value={p.avatar}
                  fallback={<Bot className="h-3.5 w-3.5" />}
                  className="h-6 w-6 shrink-0 text-sm"
                />
                <span className="truncate">{p.name}</span>
              </button>
            ))}
          </div>
        </div>

        <div className="space-y-1.5">
          <Label>系统指令（覆盖 Agent 内容）</Label>
          <Textarea
            rows={5}
            value={systemPrompt}
            spellCheck={false}
            autoCorrect="off"
            onChange={(e) => setSystemPrompt(e.target.value)}
            placeholder="可直接修改，选择上方预置会自动填入"
          />
        </div>

        <div className="flex items-center justify-between border-t border-border pt-3">
          <div className="flex items-center gap-2">
            <Zap className="w-4 h-4 text-muted-foreground" />
            <div>
              <div className="text-sm font-medium">流式输出</div>
              <div className="text-xs text-muted-foreground">
                关闭后模型一次性返回完整回复
              </div>
            </div>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={stream}
            onClick={() => setStream((v) => !v)}
            className={cn(
              "relative inline-flex h-6 w-11 shrink-0 rounded-full transition-colors",
              stream ? "bg-primary" : "bg-muted"
            )}
          >
            <span
              className={cn(
                "inline-block h-5 w-5 rounded-full bg-white shadow transform transition-transform mt-0.5",
                stream ? "translate-x-5" : "translate-x-0.5"
              )}
            />
          </button>
        </div>

        {/* ── Workspace 覆盖 ── */}
        <div className="space-y-4 border-t border-border pt-4">
          <div className="text-xs text-muted-foreground">
            以下字段留空 = 继承全局设置；任意改动都会成为本对话的覆盖。
          </div>

          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <Label>工作目录（workdir）</Label>
              {workdir !== null && (
                <button
                  type="button"
                  onClick={() => setWorkdir(null)}
                  className="text-[11px] text-muted-foreground hover:text-foreground inline-flex items-center gap-1"
                  title="恢复使用全局默认"
                >
                  <RotateCcw className="w-3 h-3" />
                  恢复默认
                </button>
              )}
            </div>
            <DirPicker
              value={workdir ?? ""}
              onChange={(v) => setWorkdir(v || null)}
              placeholder={inheritedWorkdir}
            />
          </div>

          <DirListField
            label="允许访问的目录"
            dirs={allowedDirs ?? []}
            inheritedDirs={allowedDirs === null ? inheritedAllowedDirs : undefined}
            onChange={(dirs) => setAllowedDirs(dirs)}
            trailing={
              allowedDirs !== null && (
                <button
                  type="button"
                  onClick={() => setAllowedDirs(null)}
                  className="text-[11px] text-muted-foreground hover:text-foreground inline-flex items-center gap-1 mr-1"
                >
                  <RotateCcw className="w-3 h-3" />
                  恢复默认
                </button>
              )
            }
          />

          <DirListField
            label="Skill 目录"
            dirs={skillDirs ?? []}
            inheritedDirs={skillDirs === null ? inheritedSkillDirs : undefined}
            onChange={(dirs) => setSkillDirs(dirs)}
            trailing={
              skillDirs !== null && (
                <button
                  type="button"
                  onClick={() => setSkillDirs(null)}
                  className="text-[11px] text-muted-foreground hover:text-foreground inline-flex items-center gap-1 mr-1"
                >
                  <RotateCcw className="w-3 h-3" />
                  恢复默认
                </button>
              )
            }
          />

          <ToolToggleList
            label="启用的工具"
            availableTools={availableTools}
            enabled={enabledTools}
            inheritedEnabled={inheritedEnabledTools}
            onChange={(next) => setEnabledTools(next)}
            trailing={
              enabledTools !== null && (
                <button
                  type="button"
                  onClick={() => setEnabledTools(null)}
                  className="text-[11px] text-muted-foreground hover:text-foreground inline-flex items-center gap-1 mr-1"
                >
                  <RotateCcw className="w-3 h-3" />
                  恢复默认
                </button>
              )
            }
          />
        </div>
      </div>
    </Dialog>
  );
}
