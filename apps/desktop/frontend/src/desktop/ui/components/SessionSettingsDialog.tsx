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
import type { Provider } from "@/desktop/ui/types";

function isProviderEnabled(provider: Provider) {
  return provider.enabled !== false;
}

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
  // allowedDirs 是给 DirListField 的"扁平视图"——initial(可改) + 已宣告 runtime + 待宣告 pending
  // 都合到一个数组里，对话已开始时整个列表被 lockedDirs 标记成只读，仅"添加"按钮可用。
  const [allowedDirs, setAllowedDirs] = useState<string[] | null>(null);
  const [skillDirs, setSkillDirs] = useState<string[] | null>(null);
  const [enabledTools, setEnabledTools] = useState<string[] | null>(null);

  // 对话是否已经发出过 user message——只有非空才需要锁定 / 走 pending 通道。
  const conversationStarted = !!currentSession?.messages?.some(
    (m) => m.role === "user"
  );

  // 对话已开始时，session.allowed_dirs（initial）+ runtime_allowed_dirs + pending 全部锁定，
  // 用户只能通过点 "+" 追加（保存时后端把新增项落到 pending_runtime_allowed_dirs）。
  const sessionRuntimeDirs = [
    ...(currentSession?.runtime_allowed_dirs ?? []),
    ...(currentSession?.pending_runtime_allowed_dirs ?? []),
  ];

  useEffect(() => {
    if (settingsOpen && currentSession) {
      setProviderId(currentSession.provider_id);
      setModel(currentSession.model);
      setSystemPrompt(currentSession.system_prompt ?? "");
      setPromptId(currentSession.prompt_id ?? "");
      setStream(currentSession.stream);
      setWorkdir(currentSession.workdir ?? null);
      // 把 initial / announced / pending 拼成扁平视图供 DirListField 渲染
      const initial = currentSession.allowed_dirs ?? null;
      const runtimeDirs = [
        ...(currentSession.runtime_allowed_dirs ?? []),
        ...(currentSession.pending_runtime_allowed_dirs ?? []),
      ];
      if (initial === null && runtimeDirs.length === 0) {
        setAllowedDirs(null);
      } else {
        setAllowedDirs([...(initial ?? []), ...runtimeDirs]);
      }
      setSkillDirs(currentSession.skill_dirs ?? null);
      setEnabledTools(currentSession.enabled_tools ?? null);
      // 拉一次全局 settings 用作 placeholder
      refreshAppSettings().catch(() => {});
    }
  }, [settingsOpen, currentSession, refreshAppSettings]);

  const provider = providersFile.providers.find((p) => p.id === providerId);
  const selectableProviders = providersFile.providers.filter(
    (p) => isProviderEnabled(p) || p.id === providerId
  );

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
                const p = selectableProviders.find((x) => x.id === id);
                if (p) setModel(p.default_model || p.models[0] || "");
              }}
            >
              {selectableProviders.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name} ({p.kind}
                  {p.enabled === false ? "，已停用" : ""})
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
            lockedDirs={
              // 对话已开始：当前 session 持久化里已知的目录全部 locked。
              // 用户在 UI 里新加的（还在 state 里、还没保存）不在 locked 里，可删。
              conversationStarted
                ? [
                    ...(currentSession.allowed_dirs ?? []),
                    ...sessionRuntimeDirs,
                  ]
                : undefined
            }
            lockedHint={
              conversationStarted
                ? "对话已开始，已生效的目录不能再移除；新追加项会在下一条消息发送时通过 <workspace-update> 通知模型，不会改 system prompt。"
                : undefined
            }
            trailing={
              !conversationStarted && allowedDirs !== null && (
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
