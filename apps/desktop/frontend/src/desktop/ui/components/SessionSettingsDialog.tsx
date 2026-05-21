import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Bot, ChevronDown, ChevronRight, RotateCcw, Zap, FileText, FolderOpen, Sparkles } from "lucide-react";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Label, Select, Textarea } from "@/desktop/ui/components/ui/input";
import { AvatarPreview } from "@/desktop/ui/components/AvatarField";
import { SkillsPane } from "@/desktop/ui/components/SkillsPane";
import { invoke } from "@/desktop/bridge/transport";
import {
  DirPicker,
  PathListField,
  ToolToggleList,
} from "@/desktop/ui/components/workspaceFields";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import { api } from "@/desktop/bridge/tauri";
import type { Provider, RuleFileInfo, RuleFileState } from "@/desktop/ui/types";

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
  // allowedPaths 是给 PathListField 的"扁平视图"——initial(可改) + 已宣告 runtime + 待宣告 pending
  // 都合到一个数组里，对话已开始时整个列表被 lockedPaths 标记成只读，仅添加按钮可用。
  const [allowedPaths, setAllowedPaths] = useState<string[] | null>(null);
  const [enabledTools, setEnabledTools] = useState<string[] | null>(null);

  // Rules 分区
  const [globalRules, setGlobalRules] = useState<string[] | null>(null);
  const [rulesFiles, setRulesFiles] = useState<RuleFileState[] | null>(null);
  const [discoveredRules, setDiscoveredRules] = useState<RuleFileInfo[]>([]);
  const [rulesLoading, setRulesLoading] = useState(false);

  // 对话是否已经发出过 user message——只有非空才需要锁定 / 走 pending 通道。
  const conversationStarted = !!currentSession?.messages?.some(
    (m) => m.role === "user"
  );

  // 对话已开始时，session.allowed_paths（initial）+ runtime_allowed_paths + pending 全部锁定，
  // 用户只能通过添加按钮追加（保存时后端把新增项落到 pending_runtime_allowed_paths）。
  const sessionRuntimePaths = [
    ...(currentSession?.runtime_allowed_paths ?? []),
    ...(currentSession?.pending_runtime_allowed_paths ?? []),
  ];

  useEffect(() => {
    if (settingsOpen && currentSession) {
      setProviderId(currentSession.provider_id);
      setModel(currentSession.model);
      setSystemPrompt(currentSession.system_prompt ?? "");
      setPromptId(currentSession.prompt_id ?? "");
      setStream(currentSession.stream);
      setWorkdir(currentSession.workdir ?? null);
      // 把 initial / announced / pending 拼成扁平视图供 PathListField 渲染
      const initial = currentSession.allowed_paths ?? null;
      const runtimePaths = [
        ...(currentSession.runtime_allowed_paths ?? []),
        ...(currentSession.pending_runtime_allowed_paths ?? []),
      ];
      if (initial === null && runtimePaths.length === 0) {
        setAllowedPaths(null);
      } else {
        setAllowedPaths([...(initial ?? []), ...runtimePaths]);
      }
      setEnabledTools(currentSession.enabled_tools ?? null);
      setGlobalRules(currentSession.global_rules ?? null);
      setRulesFiles(currentSession.rules_files ?? null);
      // 拉一次全局 settings 用作 placeholder
      refreshAppSettings().catch(() => {});
      // 扫所有规则文件（全局 default + 项目祖先链）
      setRulesLoading(true);
      const wd = currentSession.workdir ?? null;
      const allowed = currentSession.allowed_paths ?? [];
      invoke<RuleFileInfo[]>("discover_all_rules", {
        workdir: wd,
        allowedPaths: allowed,
        globalCandidates:
          currentSession.global_rules ?? null,
      })
        .then(setDiscoveredRules)
        .catch(() => setDiscoveredRules([]))
        .finally(() => setRulesLoading(false));
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
        allowed_paths: allowedPaths,
        enabled_tools: enabledTools,
        global_rules: globalRules,
        rules_files: rulesFiles,
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
  const inheritedAllowedPaths = globalDefaults?.allowed_paths ?? [];
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
        <CollapsibleSection
          title="目录与工具"
          icon={<FolderOpen className="w-4 h-4 text-muted-foreground" />}
          description="以下字段留空 = 继承全局设置；任意改动都会成为本对话的覆盖。"
        >
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

          <PathListField
            label="允许访问的路径"
            paths={allowedPaths ?? []}
            inheritedPaths={allowedPaths === null ? inheritedAllowedPaths : undefined}
            onChange={(paths) => setAllowedPaths(paths)}
            lockedPaths={
              // 对话已开始：当前 session 持久化里已知的路径全部 locked。
              // 用户在 UI 里新加的（还在 state 里、还没保存）不在 locked 里，可删。
              conversationStarted
                ? [
                    ...(currentSession.allowed_paths ?? []),
                    ...sessionRuntimePaths,
                  ]
                : undefined
            }
            lockedHint={
              conversationStarted
                ? "对话已开始，已生效的路径不能再移除；新追加项会在下一条消息发送时通过 <workspace-update> 通知模型，不会改 system prompt。"
                : undefined
            }
            allowFiles
            trailing={
              !conversationStarted && allowedPaths !== null && (
                <button
                  type="button"
                  onClick={() => setAllowedPaths(null)}
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
        </CollapsibleSection>

        {/* ── Skills（项目级管理与导入）── */}
        <CollapsibleSection
          title="Skills"
          icon={<Sparkles className="w-4 h-4 text-muted-foreground" />}
          description="管理本对话 workdir 加载的 skills，可从本地目录 / Git 仓库 / ~/.claude/skills 导入到当前项目或全局。"
        >
          <SkillsPane workdir={workdir} scope="project" />
        </CollapsibleSection>

        {/* ── Rules ── */}
        <CollapsibleSection
          title="规则"
          icon={<FileText className="w-4 h-4 text-muted-foreground" />}
          description="自动注入到对话上下文，无需 agent 手动读取。复选框控制本对话是否启用该文件。"
        >
          <RulesList
            discovered={discoveredRules}
            loading={rulesLoading}
            globalEnabled={
              globalRules ?? globalDefaults?.global_rules ?? []
            }
            onToggleGlobal={(path) => {
              const cur = new Set(
                globalRules ?? globalDefaults?.global_rules ?? []
              );
              if (cur.has(path)) cur.delete(path);
              else cur.add(path);
              setGlobalRules(Array.from(cur));
            }}
            rulesFiles={rulesFiles ?? []}
            onToggleProject={(info) => {
              const next = rulesFiles
                ? [...rulesFiles]
                : discoveredRules
                    .filter((r) => r.source !== "global")
                    .map((r) => ({
                      path: r.path,
                      enabled: r.source === "workdir",
                    }));
              const idx = next.findIndex((s) => s.path === info.path);
              if (idx >= 0) {
                next[idx] = { ...next[idx], enabled: !next[idx].enabled };
              } else {
                next.push({
                  path: info.path,
                  enabled: !(info.source === "workdir"),
                });
              }
              setRulesFiles(next);
            }}
          />
        </CollapsibleSection>
      </div>
    </Dialog>
  );
}

/**
 * 规则文件列表：按 source 分两段。
 * - 上段「全局」：列 source==="global" 的文件，复选框对应 session.global_rules
 * - 下段「项目」：列 source==="workdir"/"allowed_path"，复选框对应 session.rules_files
 *
 * 中间一条分隔线 + 小字 label 视觉分割。
 */
function RulesList({
  discovered,
  loading,
  globalEnabled,
  onToggleGlobal,
  rulesFiles,
  onToggleProject,
}: {
  discovered: RuleFileInfo[];
  loading: boolean;
  globalEnabled: string[];
  onToggleGlobal: (path: string) => void;
  rulesFiles: RuleFileState[];
  onToggleProject: (info: RuleFileInfo) => void;
}) {
  const globalRules = discovered.filter((r) => r.source === "global");
  const projectRules = discovered.filter((r) => r.source !== "global");
  const globalSet = new Set(globalEnabled);

  if (loading) {
    return <div className="text-xs text-muted-foreground py-1">扫描中…</div>;
  }
  if (discovered.length === 0) {
    return (
      <div className="text-xs text-muted-foreground py-1">
        未发现 CLAUDE.md / AGENTS.md
      </div>
    );
  }
  return (
    <div className="space-y-3">
      <div>
        <div className="text-[11px] text-muted-foreground mb-1 uppercase tracking-wide">
          全局
        </div>
        {globalRules.length === 0 ? (
          <div className="text-xs text-muted-foreground py-1">
            未配置全局规则文件
          </div>
        ) : (
          <ul className="space-y-1">
            {globalRules.map((info) => {
              const enabled = globalSet.has(info.path);
              return (
                <li key={info.path}>
                  <button
                    type="button"
                    onClick={() => onToggleGlobal(info.path)}
                    className="flex items-center gap-2 w-full text-left px-2 py-1 rounded text-xs transition-colors hover:bg-accent"
                  >
                    <span
                      className={cn(
                        "inline-block w-2 h-2 rounded-full shrink-0",
                        enabled ? "bg-primary" : "bg-muted-foreground/30"
                      )}
                    />
                    <span className="truncate font-mono">{info.path}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <div className="border-t border-border" />

      <div>
        <div className="text-[11px] text-muted-foreground mb-1 uppercase tracking-wide">
          项目
        </div>
        {projectRules.length === 0 ? (
          <div className="text-xs text-muted-foreground py-1">
            当前 workdir 与 allowed_paths 下没发现规则文件
          </div>
        ) : (
          <ul className="space-y-1 max-h-60 overflow-y-auto">
            {projectRules.map((info) => {
              const state = rulesFiles.find((s) => s.path === info.path);
              const enabled = state?.enabled ?? info.source === "workdir";
              return (
                <li key={info.path}>
                  <button
                    type="button"
                    onClick={() => onToggleProject(info)}
                    className="flex items-center gap-2 w-full text-left px-2 py-1 rounded text-xs transition-colors hover:bg-accent"
                  >
                    <span
                      className={cn(
                        "inline-block w-2 h-2 rounded-full shrink-0",
                        enabled ? "bg-primary" : "bg-muted-foreground/30"
                      )}
                    />
                    <span className="shrink-0 px-1 rounded text-[9px] uppercase bg-muted">
                      {info.source === "workdir" ? "wd" : "allowed"}
                    </span>
                    <span className="truncate font-mono">{info.path}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

/**
 * 可折叠区段：头部点击切换展开/收起，默认折叠。
 * 用于「目录」「Skills」「规则」三段二级配置——首屏只露常用项（provider/model/agent/stream），
 * 高级配置默认收起避免视觉过载。
 */
function CollapsibleSection({
  title,
  icon,
  defaultOpen = false,
  description,
  children,
}: {
  title: string;
  icon?: React.ReactNode;
  defaultOpen?: boolean;
  description?: string;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className="space-y-3 border-t border-border pt-4">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="w-full flex items-center gap-2 text-left -mx-1 px-1 py-1 rounded hover:bg-accent/40 transition-colors"
      >
        {open ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground" />
        )}
        {icon}
        <span className="text-sm font-medium">{title}</span>
      </button>
      {open && (
        <div className="space-y-4">
          {description && (
            <div className="text-xs text-muted-foreground">{description}</div>
          )}
          {children}
        </div>
      )}
    </div>
  );
}
