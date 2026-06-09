import {
  ArrowLeft,
  BriefcaseBusiness,
  FolderKanban,
  FolderOpen,
  MessageSquarePlus,
  MessagesSquare,
  Settings,
  SlidersHorizontal,
  Moon,
  Sun,
  Trash2,
  Edit3,
  Sparkles,
  Search,
  X,
  CaseSensitive,
  Command,
  Regex,
  Terminal,
  Plus,
  Upload,
  Download,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/desktop/ui/components/ui/button";
import { LoopingWebm } from "@/desktop/ui/components/LoopingWebm";
import { PathHint } from "@/desktop/ui/components/PathHint";
import { ImportClaudeDialog } from "@/desktop/ui/components/ImportClaudeDialog";
import { DirPicker, PathListField } from "@/desktop/ui/components/workspaceFields";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn, formatTime, pathLeaf } from "@/desktop/ui/lib/utils";
import {
  isGlobalSearchShortcut,
  isNewConversationShortcut,
} from "@/desktop/ui/lib/keyboardShortcuts";
import {
  findSearchMatches,
  splitHighlightedText,
} from "@/desktop/ui/lib/searchHighlight";
import type { SessionMeta, WorkspaceProject } from "@/desktop/ui/types";
import { animations } from "@/assets/animations";
import { BUILD_INFO } from "@/buildInfo";

type GroupKey = "today" | "yesterday" | "last7" | "last30" | "older";

function groupOf(updatedAt: number): GroupKey {
  const now = new Date();
  const d = new Date(updatedAt);
  const sameDay = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();
  if (sameDay(d, now)) return "today";
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  if (sameDay(d, y)) return "yesterday";
  const diff = (now.getTime() - d.getTime()) / 86400000;
  if (diff < 7) return "last7";
  if (diff < 30) return "last30";
  return "older";
}

function textMatchesQuery(text: string, query: string, caseSensitive: boolean, regex: boolean) {
  const trimmed = query.trim();
  if (!trimmed) return true;
  if (regex) {
    try {
      return new RegExp(trimmed, caseSensitive ? "" : "i").test(text);
    } catch {
      return false;
    }
  }
  return caseSensitive
    ? text.includes(trimmed)
    : text.toLowerCase().includes(trimmed.toLowerCase());
}

const GROUP_LABEL: Record<GroupKey, string> = {
  today: "今天",
  yesterday: "昨天",
  last7: "过去 7 天",
  last30: "过去 30 天",
  older: "更早",
};
const GROUP_ORDER: GroupKey[] = ["today", "yesterday", "last7", "last30", "older"];

export function Sidebar() {
  const {
    sessions,
    projects,
    projectSidebarMode,
    selectedProjectId,
    setProjectSidebarMode,
    openProject,
    closeProject,
    saveProject,
    deleteProject: deleteProjectAction,
    importVscodeProject,
    importProjectFile: importProjectFileAction,
    currentSession,
    searchQuery,
    searchResults,
    searchCaseSensitive,
    searchRegex,
    searching,
    runSearch,
    clearSearch,
    openSession,
    refreshSessions,
    deleteSession,
    renameSession,
    regenerateTitle,
    setAppSettingsOpen,
    setSettingsOpen,
    newSession,
    toggleTheme,
    theme,
    runningSessions,
    unreadFinishedSessions,
    sessionStreams,
  } = useStore();

  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameText, setRenameText] = useState("");
  const [regeneratingId, setRegeneratingId] = useState<string | null>(null);
  const [creatingSession, setCreatingSession] = useState(false);
  const [importClaudeOpen, setImportClaudeOpen] = useState(false);
  const [projectMenuOpen, setProjectMenuOpen] = useState(false);
  const [query, setQuery] = useState(searchQuery);
  const debounceRef = useRef<number | null>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const projectMenuRef = useRef<HTMLDivElement>(null);
  const selectedProject = useMemo(
    () => projects.find((project) => project.id === selectedProjectId) ?? null,
    [projects, selectedProjectId]
  );

  useEffect(() => {
    if (debounceRef.current) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => {
      runSearch(query, searchCaseSensitive).catch((e) =>
        toast.error(e.message || String(e))
      );
    }, 180);
    return () => {
      if (debounceRef.current) window.clearTimeout(debounceRef.current);
    };
  }, [query, searchCaseSensitive, searchRegex, runSearch]);

  async function commitRename(id: string) {
    const t = renameText.trim();
    if (t) {
      try {
        await renameSession(id, t);
      } catch (e: any) {
        toast.error(e.message || String(e));
      }
    }
    setRenamingId(null);
  }

  async function handleRegenerateTitle(id: string) {
    setRegeneratingId(id);
    try {
      await regenerateTitle();
      toast.success("已重新生成标题");
    } catch (e: any) {
      toast.error(e.message || String(e));
    } finally {
      setRegeneratingId(null);
    }
  }

  const handleCreateSession = useCallback(async () => {
    if (creatingSession) return;
    setCreatingSession(true);
    try {
      await newSession({
        projectId:
          projectSidebarMode === "projects" && selectedProject
            ? selectedProject.id
            : null,
      });
    } catch (e: any) {
      toast.error(e.message || String(e));
    } finally {
      setCreatingSession(false);
    }
  }, [creatingSession, newSession, projectSidebarMode, selectedProject]);

  useEffect(() => {
    if (!projectMenuOpen) return;
    function onClick(event: MouseEvent) {
      if (!projectMenuRef.current?.contains(event.target as Node)) {
        setProjectMenuOpen(false);
      }
    }
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [projectMenuOpen]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (isNewConversationShortcut(e)) {
        e.preventDefault();
        handleCreateSession();
        return;
      }

      if (isGlobalSearchShortcut(e)) {
        e.preventDefault();
        searchInputRef.current?.focus();
        searchInputRef.current?.select();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [handleCreateSession]);

  function projectWorkdir(project: WorkspaceProject) {
    return project.folders[0]?.path ?? "";
  }

  function projectAllowedPaths(project: WorkspaceProject) {
    return project.folders.slice(1).map((folder) => folder.path);
  }

  async function persistProjectProject(
    project: WorkspaceProject,
    workdir: string,
    allowedPaths: string[]
  ) {
    await saveProject({
      id: project.id,
      name: project.name,
      workdir,
      allowed_paths: allowedPaths,
      source: project.source ?? "manual",
    });
  }

  async function createProjectFromDialog() {
    setProjectMenuOpen(false);
    try {
      const dir = await openDialog({ directory: true, multiple: false });
      if (typeof dir !== "string") return;
      const name = pathLeaf(dir) || "项目";
      const project = await saveProject({
        name,
        workdir: dir,
        allowed_paths: [],
        source: "manual",
      });
      openProject(project.id);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  async function importProjectFile(vscode: boolean) {
    setProjectMenuOpen(false);
    try {
      const files = await openDialog({
        directory: false,
        multiple: false,
        filters: [{ name: "Workspace JSON", extensions: ["json", "code-workspace"] }],
      });
      if (typeof files !== "string") return;
      if (vscode) {
        const project = await importVscodeProject(files, pathLeaf(files).replace(/\.code-workspace$|\.json$/i, ""));
        openProject(project.id);
        return;
      }
      const project = await importProjectFileAction(files);
      openProject(project.id);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  function sessionBelongsToProject(session: SessionMeta, project: WorkspaceProject) {
    const workdir = projectWorkdir(project);
    return session.project_id === project.id || (!!workdir && session.workdir === workdir);
  }

  function sessionBelongsToAnyProject(session: SessionMeta) {
    return projects.some((project) => sessionBelongsToProject(session, project));
  }

  // 显示源：搜索命中或全量会话。项目栏只显示项目对话，Chat 栏只显示未绑定项目的对话。
  const displayItems: (SessionMeta & { snippet?: string | null })[] = useMemo(() => {
    const base = searchResults ?? sessions;
    if (projectSidebarMode === "projects" && selectedProject) {
      return base.filter((session) => sessionBelongsToProject(session, selectedProject));
    }
    if (projectSidebarMode === "all") {
      return base.filter((session) => !sessionBelongsToAnyProject(session));
    }
    return base;
  }, [searchResults, sessions, projectSidebarMode, selectedProject, projects]);

  const displayProjects = useMemo(() => {
    if (projectSidebarMode !== "projects" || selectedProject) return projects;
    const trimmed = query.trim();
    if (!trimmed) return projects;
    const matchingSessions = searchResults ?? sessions;
    return projects.filter((project) => {
      const projectText = `${project.name} ${project.folders.map((folder) => folder.path).join(" ")}`;
      if (textMatchesQuery(projectText, query, searchCaseSensitive, searchRegex)) return true;
      return matchingSessions.some((session) => sessionBelongsToProject(session, project));
    });
  }, [projects, projectSidebarMode, query, searchCaseSensitive, searchRegex, searchResults, selectedProject, sessions]);

  const grouped = useMemo(() => {
    const g: Record<GroupKey, typeof displayItems> = {
      today: [],
      yesterday: [],
      last7: [],
      last30: [],
      older: [],
    };
    for (const s of displayItems) g[groupOf(s.updated_at)].push(s);
    return g;
  }, [displayItems]);

  function renderSearchText(text: string, keyPrefix: string) {
    if (!searchResults || !query.trim()) return text;

    const segments = splitHighlightedText(
      text,
      findSearchMatches(text, query, searchCaseSensitive, searchRegex)
    );
    if (!segments.some((segment) => segment.highlighted)) return text;

    return segments.map((segment, index) =>
      segment.highlighted ? (
        <mark
          key={`${keyPrefix}-${index}`}
          className="rounded-sm bg-amber-300 px-0.5 text-black"
        >
          {segment.text}
        </mark>
      ) : (
        <span key={`${keyPrefix}-${index}`}>{segment.text}</span>
      )
    );
  }

  function renderSessionList() {
    return (
      <>
        {displayItems.length === 0 && (
          <div className="text-center text-xs text-muted-foreground py-10 px-4">
            {searchResults
              ? "无匹配结果"
              : selectedProject
                ? "这个项目下还没有对话"
                : "暂无对话，点击上方按钮创建"}
          </div>
        )}
        {GROUP_ORDER.map((key) => {
          const items = grouped[key];
          if (items.length === 0) return null;
          return (
            <div key={key} className="mb-2">
              <div className="px-2 py-1 text-[10px] font-semibold text-muted-foreground/80 uppercase tracking-wider">
                {GROUP_LABEL[key]}
              </div>
              <ul className="space-y-0.5">
                {items.map((s) => renderSessionItem(s))}
              </ul>
            </div>
          );
        })}
      </>
    );
  }

  function renderSessionItem(s: SessionMeta & { snippet?: string | null }) {
    const active = currentSession?.id === s.id;
    const regenerating = regeneratingId === s.id;
    const snippet = (s as any).snippet as string | undefined;
    const running = runningSessions.has(s.id);
    const unread = !active && !running && unreadFinishedSessions.has(s.id);
    // 待审批：slot 本身持有悬挂的审批 / 提问（run 结束 slot 被删，自然清除）。
    const slot = sessionStreams[s.id];
    const pendingApproval = !!(slot?.pendingApproval || slot?.pendingQuestion);
    return (
      <li key={s.id}>
        <div
          onClick={() => openSession(s.id)}
          className={cn(
            "group relative px-3 py-2 rounded-md cursor-pointer transition-colors",
            active ? "bg-accent text-accent-foreground" : "hover:bg-accent/50",
            pendingApproval ? "glow-pending" : unread && "glow-finished"
          )}
        >
          <div className="flex min-w-0 gap-2">
            <span className="flex h-5 w-2 shrink-0 items-center justify-center">
              {(running || unread || pendingApproval) && (
                <span
                  className={cn(
                    "h-2 w-2 rounded-full",
                    pendingApproval
                      ? "bg-amber-500 animate-breathe"
                      : unread
                        ? "bg-emerald-500"
                        : "bg-primary animate-breathe"
                  )}
                  title={
                    pendingApproval
                      ? "等待处理"
                      : running
                        ? "后台正在运行"
                        : "运行已完成，未查看"
                  }
                  aria-label={
                    pendingApproval
                      ? "pending"
                      : running
                        ? "running"
                        : "unread"
                  }
                />
              )}
            </span>
            <div className="min-w-0 flex-1">
              <div className="relative min-w-0">
                {renamingId === s.id ? (
                  <input
                    autoFocus
                    spellCheck={false}
                    autoCorrect="off"
                    value={renameText}
                    onChange={(e) => setRenameText(e.target.value)}
                    onBlur={() => commitRename(s.id)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") commitRename(s.id);
                      if (e.key === "Escape") setRenamingId(null);
                    }}
                    className="w-full text-sm bg-background border border-input rounded px-1.5 py-0.5 outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  />
                ) : (
                  <span className="block truncate text-sm" title={s.title}>
                    {renderSearchText(s.title, `${s.id}-title`)}
                  </span>
                )}
                <span className="pointer-events-none absolute right-0 top-0 hidden max-w-14 bg-accent/95 pl-2 text-[11px] text-muted-foreground group-hover:block">
                  {formatTime(s.updated_at)}
                </span>
              </div>
              <div className="mt-0.5 flex items-center gap-1 text-[11px] text-muted-foreground">
                <span className="truncate flex-1">{s.model}</span>
                {s.source === "cli" && (
                  <span
                    className="inline-flex items-center gap-0.5 px-1 py-0 rounded text-[10px] font-medium uppercase tracking-wide bg-primary/10 text-primary border border-primary/20 shrink-0"
                    title="本对话由 hebbian-cli 创建"
                  >
                    <Terminal className="w-2.5 h-2.5" />
                    CLI
                  </span>
                )}
                {s.source === "claude" && (
                  <span
                    className="inline-flex items-center gap-0.5 px-1 py-0 rounded text-[10px] font-medium uppercase tracking-wide bg-amber-500/10 text-amber-600 border border-amber-500/20 shrink-0"
                    title="从 Claude 导入"
                  >
                    <Download className="w-2.5 h-2.5" />
                    Claude
                  </span>
                )}
                {!renamingId && (
                  <div className="flex items-center gap-0 opacity-0 group-hover:opacity-100 transition-opacity shrink-0">
                    {active && (
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          handleRegenerateTitle(s.id);
                        }}
                        disabled={regenerating}
                        className="p-0.5 rounded hover:bg-background text-muted-foreground disabled:opacity-50"
                        title="用模型重新生成标题"
                      >
                        <Sparkles className={cn("w-3 h-3", regenerating && "animate-pulse text-primary")} />
                      </button>
                    )}
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        setRenamingId(s.id);
                        setRenameText(s.title);
                      }}
                      className="p-0.5 rounded hover:bg-background text-muted-foreground"
                      title="重命名"
                    >
                      <Edit3 className="w-3 h-3" />
                    </button>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        if (
                          confirm(`删除对话 "${s.title}"？`) &&
                          confirm(`再次确认删除对话 "${s.title}"？此操作不可撤销。`)
                        ) {
                          deleteSession(s.id).catch((err) => toast.error(err.message || String(err)));
                        }
                      }}
                      className="p-0.5 rounded hover:bg-background text-muted-foreground hover:text-destructive"
                      title="删除"
                    >
                      <Trash2 className="w-3 h-3" />
                    </button>
                  </div>
                )}
              </div>
            </div>
          </div>
          {snippet && (
            <div className="ml-4 text-[11px] text-muted-foreground/80 mt-1 line-clamp-2">
              {renderSearchText(snippet, `${s.id}-snippet`)}
            </div>
          )}
        </div>
      </li>
    );
  }

  return (
    <aside className="w-64 shrink-0 flex flex-col gap-2 p-2 pb-3">
      {/* 品牌区：无框、溶进浅灰底；仍保留 macOS traffic-light 留空（pt-8） */}
      <div
        className="h-16 px-5 pt-8 flex items-start drag-region"
        data-tauri-drag-region
      >
        <div className="flex items-end gap-2 pointer-events-none">
          <LoopingWebm
            src={animations.brandMark}
            className="h-7 w-7 rounded-md shadow-sm shrink-0"
          />
          {/* `Hebbian` 字号视觉跟 logo 高度对齐（h-7 = 28px）：
              text-2xl=24px + leading-none 让 cap-height 紧贴 logo；
              版本号小字 baseline 对齐 Hebbian 底（items-end + -mb-0.5 微调） */}
          <div
            className="flex items-end gap-1"
            title={`build ${BUILD_INFO.build} · ${BUILD_INFO.builtAt}`}
          >
            <span className="text-2xl font-semibold leading-none">Hebbian</span>
            <span className="text-[9px] font-mono leading-none text-muted-foreground/70 -mb-0.5">
              v{BUILD_INFO.version}·{BUILD_INFO.commit}
              {BUILD_INFO.dirty ? "+" : ""}
            </span>
          </div>
        </div>
      </div>

      {/* 列表卡：项目/对话切换 + 列表 + 底栏（hairline 分隔）全部聚到一块 */}
      {/* 左侧 sidebar 主体卡片（logo 下方的整张白卡）。双向阴影：朝右散得多（主投影），
          朝左散得少（薄薄一层让卡片左缘也跟窗口边脱开），整体浮起感更对称。 */}
      <div className="flex flex-1 min-h-0 flex-col rounded-3xl border border-border bg-card shadow-[10px_0_28px_-10px_rgba(0,0,0,0.28),-4px_0_16px_-8px_rgba(0,0,0,0.16)] overflow-hidden">
      <div className="px-3 py-3 no-drag">
        <div className="mb-2 grid grid-cols-2 gap-1 rounded-lg bg-muted/50 p-1">
          <button
            type="button"
            onClick={() => setProjectSidebarMode("projects")}
            className={cn(
              "h-8 rounded-md inline-flex items-center justify-center gap-1.5 text-xs font-medium transition-colors",
              projectSidebarMode === "projects"
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground"
            )}
          >
            <BriefcaseBusiness className="w-3.5 h-3.5" />
            Code
          </button>
          <button
            type="button"
            onClick={() => setProjectSidebarMode("all")}
            className={cn(
              "h-8 rounded-md inline-flex items-center justify-center gap-1.5 text-xs font-medium transition-colors",
              projectSidebarMode === "all"
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground"
            )}
          >
            <MessagesSquare className="w-3.5 h-3.5" />
            Chat
          </button>
        </div>
        <Button
          onClick={handleCreateSession}
          className="w-full justify-between"
          size="md"
          disabled={creatingSession}
        >
          <span className="inline-flex items-center gap-1.5">
            <MessageSquarePlus className="w-4 h-4" />
            新建对话
          </span>
          <span className="ml-auto inline-flex items-center gap-0.5 text-[10px] font-medium text-primary-foreground/70">
            <Command className="h-3 w-3" />
            N
          </span>
        </Button>
        <button
          onClick={() => setImportClaudeOpen(true)}
          className="mt-1.5 w-full inline-flex items-center justify-center gap-1.5 text-xs text-muted-foreground hover:text-foreground no-drag"
        >
          <Download className="w-3.5 h-3.5" />
          从 Claude 导入
        </button>
      </div>

      <ImportClaudeDialog
        open={importClaudeOpen}
        onOpenChange={setImportClaudeOpen}
        onImported={async (id) => {
          await refreshSessions();
          await openSession(id);
        }}
      />

      {projectSidebarMode === "projects" && (
        <div className="px-3 pb-2 no-drag">
          {selectedProject ? (
            <div className="space-y-2 rounded-lg border border-border bg-background/60 p-2">
              <div className="flex items-center gap-1">
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={closeProject}
                  title="返回项目列表"
                  className="shrink-0"
                >
                  <ArrowLeft className="w-4 h-4" />
                </Button>
                <div className="min-w-0">
                  <div className="truncate text-sm font-semibold">{selectedProject.name}</div>
                  {projectWorkdir(selectedProject) && (
                    <PathHint path={projectWorkdir(selectedProject)}>
                      <div className="truncate text-[11px] text-muted-foreground font-mono">
                        {pathLeaf(projectWorkdir(selectedProject))}
                      </div>
                    </PathHint>
                  )}
                </div>
              </div>
              <div className="space-y-2">
                <div className="space-y-1">
                  <div className="text-[11px] text-muted-foreground px-1">主目录</div>
                  <DirPicker
                    value={projectWorkdir(selectedProject)}
                    onChange={(v) => {
                      if (!v) return;
                      persistProjectProject(
                        selectedProject,
                        v,
                        projectAllowedPaths(selectedProject)
                      ).catch((err) => toast.error(err.message || String(err)));
                    }}
                  />
                </div>
                <PathListField
                  label="允许访问的路径"
                  paths={projectAllowedPaths(selectedProject)}
                  onChange={(paths) => {
                    persistProjectProject(selectedProject, projectWorkdir(selectedProject), paths).catch((err) =>
                      toast.error(err.message || String(err))
                    );
                  }}
                  emptyHint="暂无额外路径"
                  allowFiles
                  maxVisibleRows={5}
                  relativeTo={projectWorkdir(selectedProject)}
                />
              </div>
            </div>
          ) : (
            <div className="flex items-center justify-between">
              <div className="text-xs font-semibold text-muted-foreground">项目列表</div>
              <div className="relative" ref={projectMenuRef}>
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => setProjectMenuOpen((v) => !v)}
                  title="添加项目"
                >
                  <Plus className="w-4 h-4" />
                </Button>
                {projectMenuOpen && (
                  <div
                    onClick={(e) => e.stopPropagation()}
                    className="absolute right-0 top-full mt-1 w-44 rounded-lg border border-border bg-card shadow-lg z-[90] overflow-hidden"
                  >
                    <button
                      type="button"
                      onClick={createProjectFromDialog}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                    >
                      <FolderKanban className="w-4 h-4 text-muted-foreground" />
                      新建项目
                    </button>
                    <button
                      type="button"
                      onClick={() => importProjectFile(false)}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                    >
                      <Upload className="w-4 h-4 text-muted-foreground" />
                      导入项目
                    </button>
                    <button
                      type="button"
                      onClick={() => importProjectFile(true)}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                    >
                      <FolderOpen className="w-4 h-4 text-muted-foreground" />
                      导入 VS Code 项目
                    </button>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      )}

      {/* 搜索框 */}
      <div className="px-3 pb-2 no-drag">
        <div className="relative flex items-center">
          <Search className="w-3.5 h-3.5 absolute left-2.5 text-muted-foreground pointer-events-none" />
          <input
            ref={searchInputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="全局搜索标题 / 内容"
            spellCheck={false}
            autoCorrect="off"
            className="h-8 w-full rounded-md border border-input bg-background pl-8 pr-[5.75rem] text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
          />
          <div className="absolute right-1 flex items-center">
            <button
              onClick={() =>
                runSearch(query, searchCaseSensitive, !searchRegex).catch(() => {})
              }
              className={cn(
                "h-6 w-6 inline-flex items-center justify-center rounded text-muted-foreground hover:bg-accent",
                searchRegex && "bg-primary/20 text-primary"
              )}
              title={`正则表达式：${searchRegex ? "开" : "关"}`}
            >
              <Regex className="w-3.5 h-3.5" />
            </button>
            <button
              onClick={() =>
                runSearch(query, !searchCaseSensitive, searchRegex).catch(() => {})
              }
              className={cn(
                "h-6 w-6 inline-flex items-center justify-center rounded text-muted-foreground hover:bg-accent",
                searchCaseSensitive && "bg-primary/20 text-primary"
              )}
              title={`区分大小写：${searchCaseSensitive ? "开" : "关"}`}
            >
              <CaseSensitive className="w-3.5 h-3.5" />
            </button>
            {query && (
              <button
                onClick={() => {
                  setQuery("");
                  clearSearch();
                }}
                className="h-6 w-6 inline-flex items-center justify-center rounded text-muted-foreground hover:bg-accent"
                title="清除"
              >
                <X className="w-3.5 h-3.5" />
              </button>
            )}
          </div>
        </div>
        {searching && (
          <div className="text-[11px] text-muted-foreground mt-1 px-0.5">
            搜索中…
          </div>
        )}
        {searchResults && !searching && (
          <div className="text-[11px] text-muted-foreground mt-1 px-0.5">
            命中 {searchResults.length} 条
          </div>
        )}
      </div>

      <div className="flex-1 overflow-y-auto px-2 pb-2 no-drag">
        {projectSidebarMode === "projects" && !selectedProject ? (
          displayProjects.length === 0 ? (
            <div className="text-center text-xs text-muted-foreground py-10 px-4">
              {query.trim() ? "无匹配结果" : "暂无项目，点击上方加号创建"}
            </div>
          ) : (
            <ul className="space-y-1">
              {displayProjects.map((project) => {
                const countSource = searchResults ?? sessions;
                const count = countSource.filter((session) => sessionBelongsToProject(session, project)).length;
                return (
                  <li key={project.id}>
                    <div
                      onClick={() => openProject(project.id)}
                      className="group px-3 py-2 rounded-md cursor-pointer hover:bg-accent/50 transition-colors"
                    >
                      <div className="flex items-center gap-2">
                        <FolderKanban className="w-4 h-4 shrink-0 text-muted-foreground" />
                        <span className="min-w-0 flex-1 truncate text-sm font-medium">{project.name}</span>
                        <span className="text-[11px] text-muted-foreground">{count}</span>
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            if (
                              confirm(`删除项目 "${project.name}"？不会删除已有对话。`) &&
                              confirm(`再次确认删除项目 "${project.name}"？`)
                            ) {
                              deleteProjectAction(project.id).catch((err) =>
                                toast.error(err.message || String(err))
                              );
                            }
                          }}
                          className="p-1 rounded hover:bg-background text-muted-foreground hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity"
                          title="删除项目"
                        >
                          <Trash2 className="w-3.5 h-3.5" />
                        </button>
                      </div>
                      {projectWorkdir(project) && (
                        <PathHint path={projectWorkdir(project)}>
                          <div className="mt-0.5 truncate text-[11px] text-muted-foreground font-mono">
                            {pathLeaf(projectWorkdir(project))}
                          </div>
                        </PathHint>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )
        ) : (
          renderSessionList()
        )}
      </div>

      <div className="border-t border-border p-1.5 flex items-center gap-0.5 no-drag">
        <Button
          variant="ghost"
          size="icon"
          onClick={() => setAppSettingsOpen(true)}
          title="设置"
        >
          <Settings className="w-4 h-4" />
        </Button>
        {currentSession && (
          <Button
            variant="ghost"
            size="icon"
            onClick={() => setSettingsOpen(true)}
            title={currentSession.project_id ? "项目设置" : "对话设置"}
          >
            <SlidersHorizontal className="w-4 h-4" />
          </Button>
        )}
        <div className="flex-1" />
        <Button
          variant="ghost"
          size="icon"
          onClick={toggleTheme}
          title="切换主题"
        >
          {theme === "dark" ? <Sun className="w-4 h-4" /> : <Moon className="w-4 h-4" />}
        </Button>
      </div>
      </div>
    </aside>
  );
}
