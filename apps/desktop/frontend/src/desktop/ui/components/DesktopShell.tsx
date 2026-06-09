import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useMemo, useState, type CSSProperties, type PointerEvent } from "react";
import {
  ChevronDown,
  ChevronRight,
  Code2,
  Edit3,
  FolderOpen,
  Import,
  MessageSquarePlus,
  Palette,
  Plus,
  Search,
  Settings,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { ChatView } from "@/desktop/ui/components/ChatView";
import { RightSidebar } from "@/desktop/ui/components/RightSidebar";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn, pathLeaf } from "@/desktop/ui/lib/utils";
import type { SessionMeta, WorkspaceProject } from "@/desktop/ui/types";
import "./desktopShell.css";

interface ProjectBucket {
  id: string;
  name: string;
  path: string;
  projectId: string | null;
  sessions: SessionMeta[];
}

const THEME_PRESETS = [
  { id: "glacier", name: "冰湖蓝绿", hue: 208, colors: ["#EAF4FF", "#EEF0FF", "#E8FFF5"] },
  { id: "mist", name: "雾蓝灰", hue: 214, colors: ["#EEF4FA", "#F3F6FA", "#E8F0F7"] },
  { id: "porcelain", name: "青瓷灰", hue: 190, colors: ["#EEF8F8", "#F4F8F6", "#E7F1F2"] },
  { id: "moon", name: "月白灰", hue: 204, colors: ["#F6F8FA", "#EEF5F7", "#F9FAFB"] },
];

function projectPath(project: WorkspaceProject) {
  return project.folders[0]?.path ?? "";
}

function clampColor(value: number) {
  return Math.max(0, Math.min(255, Math.round(value)));
}

function hslToRgb(hue: number, saturation: number, lightness: number) {
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
  const segment = hue / 60;
  const x = chroma * (1 - Math.abs((segment % 2) - 1));
  const [r1, g1, b1] =
    segment < 1 ? [chroma, x, 0] :
    segment < 2 ? [x, chroma, 0] :
    segment < 3 ? [0, chroma, x] :
    segment < 4 ? [0, x, chroma] :
    segment < 5 ? [x, 0, chroma] :
    [chroma, 0, x];
  const m = lightness - chroma / 2;
  return {
    r: clampColor((r1 + m) * 255),
    g: clampColor((g1 + m) * 255),
    b: clampColor((b1 + m) * 255),
  };
}

function shiftedRgba(
  base: { r: number; g: number; b: number },
  delta: { r: number; g: number; b: number },
  alpha: number
) {
  return `rgba(${clampColor(base.r + delta.r)}, ${clampColor(base.g + delta.g)}, ${clampColor(base.b + delta.b)}, ${alpha})`;
}

function hueStyle(hue: number): CSSProperties {
  const accent = `hsl(${hue} 92% 55%)`;
  const accent2 = `hsl(${(hue + 28) % 360} 92% 64%)`;
  const baseMain = hslToRgb(208, 0.92, 0.58);
  const currentMain = hslToRgb(hue, 0.92, 0.58);
  const delta = {
    r: currentMain.r - baseMain.r,
    g: currentMain.g - baseMain.g,
    b: currentMain.b - baseMain.b,
  };
  return {
    "--dsp-accent": accent,
    "--dsp-accent-2": accent2,
    "--dsp-accent-soft": `hsl(${hue} 92% 55% / 0.12)`,
    "--dsp-chat-wash": `hsl(${hue} 72% 58% / 0.045)`,
    "--dsp-chat-bubble-a": `hsl(${hue} 80% 62% / 0.12)`,
    "--dsp-chat-bubble-b": `hsl(${(hue + 42) % 360} 76% 64% / 0.1)`,
    "--dsp-chat-bubble-c": `hsl(${(hue + 148) % 360} 58% 66% / 0.08)`,
    "--dsp-chat-bubble-d": `hsl(${(hue + 318) % 360} 70% 68% / 0.08)`,
    "--dsp-chat-panel": `hsl(${hue} 58% 99% / 0.7)`,
    "--dsp-right-bg": `linear-gradient(180deg, hsl(${hue} 52% 98% / 0.82), hsl(${hue} 42% 95% / 0.72))`,
    "--dsp-right-card": `hsl(${hue} 44% 99% / 0.88)`,
    "--dsp-user-bubble": `linear-gradient(135deg, hsl(${hue} 92% 55% / 0.1), hsl(${hue} 52% 99% / 0.94))`,
    "--dsp-user-line": `hsl(${hue} 92% 55% / 0.18)`,
    "--dsp-orb-shadow": `0 20px 50px hsl(${hue} 92% 55% / 0.16)`,
    "--dsp-hero-strip": shiftedRgba({ r: 224, g: 235, b: 247 }, delta, 0.58),
    "--dsp-hero-orb": shiftedRgba({ r: 92, g: 150, b: 255 }, delta, 0.14),
    "--dsp-hero-panel-a": shiftedRgba({ r: 255, g: 255, b: 255 }, delta, 0.96),
    "--dsp-hero-panel-b": shiftedRgba({ r: 235, g: 246, b: 255 }, delta, 0.82),
    "--dsp-hero-panel-c": shiftedRgba({ r: 223, g: 245, b: 232 }, delta, 0.72),
    "--primary": `${hue} 92% 58%`,
    "--ring": `${hue} 92% 58%`,
    "--dsp-sidebar": `hsl(${hue} 48% 94% / 0.86)`,
    "--dsp-bg": `hsl(${hue} 42% 97%)`,
    "--dsp-canvas": `hsl(${hue} 52% 99%)`,
    "--dsp-line": `hsl(${hue} 36% 26% / 0.09)`,
    "--dsp-line-strong": `hsl(${hue} 44% 28% / 0.16)`,
    "--dsp-shadow": `0 18px 50px hsl(${hue} 34% 26% / 0.13)`,
    "--dsp-shadow-soft": `0 10px 26px hsl(${hue} 34% 26% / 0.08)`,
  } as CSSProperties;
}

function DesktopHueControl({ hue, setHue }: { hue: number; setHue: (hue: number) => void }) {
  const [open, setOpen] = useState(false);
  const angle = (hue - 90) * (Math.PI / 180);
  const dotX = 48 + Math.cos(angle) * 34;
  const dotY = 48 + Math.sin(angle) * 34;

  function pickFromPointer(event: PointerEvent<HTMLDivElement>) {
    const rect = event.currentTarget.getBoundingClientRect();
    const x = event.clientX - rect.left - rect.width / 2;
    const y = event.clientY - rect.top - rect.height / 2;
    const next = (Math.round((Math.atan2(y, x) * 180) / Math.PI + 90) + 360) % 360;
    setHue(next);
  }

  return (
    <div className="dsp-hue-control">
      <button className="dsp-hue-button" type="button" onClick={() => setOpen((value) => !value)} title="调整色系">
        <Palette size={15} />
      </button>
      {open && (
        <div className="dsp-hue-popover" onClick={(event) => event.stopPropagation()}>
          <div className="dsp-hue-title">统一色系</div>
          <div className="dsp-theme-presets">
            {THEME_PRESETS.map((preset) => (
              <button
                key={preset.id}
                type="button"
                className={cn("dsp-theme-preset", Math.abs(preset.hue - hue) < 3 && "is-active")}
                onClick={() => setHue(preset.hue)}
                title={preset.name}
              >
                <span className="dsp-theme-preset-swatch" style={{ background: `linear-gradient(135deg, ${preset.colors.join(", ")})` }} />
                <span>{preset.name}</span>
              </button>
            ))}
          </div>
          <div className="dsp-hue-ring" onPointerDown={pickFromPointer} onPointerMove={(event) => event.buttons === 1 && pickFromPointer(event)}>
            <span className="dsp-hue-ring-dot" style={{ left: dotX, top: dotY }} />
          </div>
          <input
            aria-label="调整色系"
            className="dsp-hue-slider"
            type="range"
            min={0}
            max={359}
            value={hue}
            onChange={(event) => setHue(Number(event.target.value))}
          />
          <div className="dsp-hue-meta">
            <span>#{hue.toString(16).padStart(2, "0").toUpperCase()}</span>
            <span style={{ color: `hsl(${hue} 92% 45%)` }}>●</span>
          </div>
        </div>
      )}
    </div>
  );
}

function relativeTime(ts: number) {
  const diff = Date.now() - ts;
  const min = 60_000;
  const hour = 3_600_000;
  const day = 86_400_000;
  if (diff < min) return "刚刚";
  if (diff < hour) return `${Math.floor(diff / min)}分钟前`;
  if (diff < day) return `${Math.floor(diff / hour)}小时前`;
  if (diff < 3 * day) return `${Math.floor(diff / day)}天前`;
  return new Date(ts).toLocaleDateString("zh-CN", { month: "2-digit", day: "2-digit" });
}

function sessionMatchesQuery(session: SessionMeta, query: string, caseSensitive: boolean, regex: boolean) {
  const text = `${session.title} ${session.model}`;
  if (!query.trim()) return true;
  if (regex) {
    try {
      return new RegExp(query, caseSensitive ? "" : "i").test(text);
    } catch {
      return false;
    }
  }
  return caseSensitive
    ? text.includes(query)
    : text.toLowerCase().includes(query.toLowerCase());
}

function buildProjectBuckets(
  projects: WorkspaceProject[],
  sessions: SessionMeta[],
  query: string,
  caseSensitive: boolean,
  regex: boolean
): ProjectBucket[] {
  const buckets: ProjectBucket[] = projects.map((project) => ({
    id: project.id,
    name: project.name,
    path: projectPath(project),
    projectId: project.id,
    sessions: [],
  }));
  const defaultBucket: ProjectBucket = {
    id: "default",
    name: "默认项目",
    path: "未归入项目的对话",
    projectId: null,
    sessions: [],
  };

  for (const session of sessions) {
    if (!sessionMatchesQuery(session, query, caseSensitive, regex)) continue;
    const bucket = buckets.find((item) => {
      const matchedById = !!session.project_id && session.project_id === item.projectId;
      const matchedByWorkdir = !!session.workdir && !!item.path && session.workdir === item.path;
      return matchedById || matchedByWorkdir;
    });
    (bucket ?? defaultBucket).sessions.push(session);
  }

  for (const bucket of buckets) {
    bucket.sessions.sort((a, b) => b.updated_at - a.updated_at);
  }
  defaultBucket.sessions.sort((a, b) => b.updated_at - a.updated_at);
  return [...buckets, defaultBucket];
}

function DesktopSidebar({ hue, setHue }: { hue: number; setHue: (hue: number) => void }) {
  const {
    sessions,
    projects,
    currentSession,
    openSession,
    newSession,
    saveProject,
    importProjectFile,
    importVscodeProject,
    deleteSession,
    deleteProject,
    runningSessions,
    unreadFinishedSessions,
    sessionStreams,
    setAppSettingsOpen,
  } = useStore();
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchRegex, setSearchRegex] = useState(false);
  const [searchCase, setSearchCase] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [activeTab, setActiveTab] = useState<"code" | "chat">("code");

  const buckets = useMemo(
    () => buildProjectBuckets(projects, sessions, query, searchCase, searchRegex),
    [projects, query, searchCase, searchRegex, sessions]
  );

  const filteredBuckets = useMemo(() => {
    if (activeTab === "chat") {
      return buckets.filter((b) => b.projectId === null);
    }
    return buckets.filter((b) => b.projectId !== null);
  }, [buckets, activeTab]);

  async function handleNewSession(projectId: string | null) {
    try {
      await newSession({ projectId });
    } catch (error: any) {
      toast.error(error.message || String(error));
    }
  }

  async function handleCreateProject() {
    try {
      const dir = await openDialog({ directory: true, multiple: false });
      if (typeof dir !== "string") return;
      await saveProject({
        name: pathLeaf(dir) || "项目",
        workdir: dir,
        allowed_paths: [],
        source: "manual",
      });
    } catch (error: any) {
      toast.error(error.message || String(error));
    }
  }

  async function handleImportProject(vscode: boolean) {
    try {
      const file = await openDialog({
        directory: false,
        multiple: false,
        filters: [{ name: "Workspace JSON", extensions: ["json", "code-workspace"] }],
      });
      if (typeof file !== "string") return;
      if (vscode) {
        await importVscodeProject(file, pathLeaf(file).replace(/\.code-workspace$|\.json$/i, ""));
        return;
      }
      await importProjectFile(file);
    } catch (error: any) {
      toast.error(error.message || String(error));
    }
  }

  async function handleDeleteProject(id: string, name: string) {
    if (!confirm(`删除项目 "${name}"？项目下的对话不会被删除。`)) return;
    if (!confirm(`再次确认删除项目 "${name}"？`)) return;
    try {
      await deleteProject(id);
    } catch (error: any) {
      toast.error(error.message || String(error));
    }
  }

  function toggleBucket(id: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function handleDeleteSession(session: SessionMeta) {
    if (!confirm(`删除对话 "${session.title}"？`)) return;
    if (!confirm(`再次确认删除对话 "${session.title}"？此操作不可撤销。`)) return;
    try {
      await deleteSession(session.id);
    } catch (error: any) {
      toast.error(error.message || String(error));
    }
  }

  function renderSessionRow(session: SessionMeta) {
    const active = currentSession?.id === session.id;
    const running = runningSessions.has(session.id);
    const unread = !active && !running && unreadFinishedSessions.has(session.id);
    const slot = sessionStreams[session.id];
    const pendingApproval = !!(slot?.pendingApproval || slot?.pendingQuestion);
    const timeStr = relativeTime(session.updated_at);
    return (
      <div
        key={session.id}
        className={cn(
          "dsp-session-row-wrap",
          active && "is-active",
          unread && "is-unread",
          pendingApproval && "is-approval"
        )}
      >
        <button
          type="button"
          className="dsp-session-row"
          onClick={() => openSession(session.id)}
        >
          <span className={cn("dsp-session-status", running && "is-running", unread && "is-unread")} />
          <span className="dsp-session-main">
            <strong>{session.title}</strong>
            <span className="dsp-session-time">{timeStr}</span>
          </span>
        </button>
        <button
          type="button"
          className="dsp-session-delete"
          title="删除对话"
          onClick={() => handleDeleteSession(session)}
        >
          <Trash2 size={12} />
        </button>
      </div>
    );
  }

  return (
    <aside className="dsp-sidebar">
      <div className="dsp-window-space" data-tauri-drag-region />
      <div className="dsp-sidebar-card">
        <div className="dsp-sidebar-tabs">
          <button className={cn(activeTab === "code" && "is-active")} type="button" onClick={() => setActiveTab("code")}><Code2 size={14} />code</button>
          <button className={cn(activeTab === "chat" && "is-active")} type="button" onClick={() => setActiveTab("chat")}><Edit3 size={14} />chat</button>
        </div>

        {activeTab === "chat" && (
          <button className="dsp-sidebar-new-chat" type="button" onClick={() => handleNewSession(null)}>
            <MessageSquarePlus size={14} />
            新建对话
          </button>
        )}

        <div className="dsp-project-toolbar">
          <div className="dsp-project-toolbar-head">
            <span>{activeTab === "chat" ? "对话" : "项目"}</span>
          </div>
          {activeTab === "code" && (
            <div className="dsp-project-actions">
              <button type="button" title="新建项目" onClick={handleCreateProject}>
                <Plus size={13} />
                <span>新建项目</span>
              </button>
              <button type="button" title="导入项目" onClick={() => handleImportProject(false)}>
                <Import size={13} />
                <span>导入项目</span>
              </button>
              <button type="button" title="导入 VS Code 项目" onClick={() => handleImportProject(true)}>
                <FolderOpen size={13} />
                <span>导入 VS Code</span>
              </button>
            </div>
          )}
          <div className={cn("dsp-project-search", searchOpen && "is-open")}>
            <label>
              <Search size={13} />
              <input
                value={query}
                onFocus={() => setSearchOpen(true)}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="搜索"
              />
            </label>
            {searchOpen && (
              <div className="dsp-search-options">
                <button
                  type="button"
                  className={cn(searchCase && "is-active")}
                  onClick={() => setSearchCase((v) => !v)}
                  title="区分大小写"
                >
                  Aa
                </button>
                <button
                  type="button"
                  className={cn(searchRegex && "is-active")}
                  onClick={() => setSearchRegex((v) => !v)}
                  title="正则表达式"
                >
                  .*
                </button>
              </div>
            )}
          </div>
        </div>

        <div className="dsp-project-groups">
          {activeTab === "chat" ? (
            <div className="dsp-project-session-list has-overflow">
              {filteredBuckets.flatMap((bucket) => bucket.sessions).map(renderSessionRow)}
            </div>
          ) : (
            filteredBuckets.map((bucket) => {
              const isCollapsed = collapsed.has(bucket.id);
              return (
                <section className="dsp-project-group" key={bucket.id}>
                  <div className="dsp-project-heading-wrap">
                    <button className="dsp-project-heading" type="button" onClick={() => toggleBucket(bucket.id)}>
                      {isCollapsed ? <ChevronRight size={15} /> : <ChevronDown size={15} />}
                      <FolderOpen size={15} />
                      <span>
                        <strong>{bucket.name}</strong>
                      </span>
                      <em>{bucket.sessions.length}</em>
                    </button>
                    <button
                      className="dsp-project-add"
                      type="button"
                      title="在这个项目中新建对话"
                      onClick={() => handleNewSession(bucket.projectId)}
                    >
                      <Plus size={13} />
                    </button>
                    <button
                      type="button"
                      className="dsp-project-delete"
                      title="删除项目"
                      onClick={() => handleDeleteProject(bucket.projectId!, bucket.name)}
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>

                  {!isCollapsed && (
                    <div className="dsp-project-session-list has-overflow">
                      {bucket.sessions.slice(0, 8).map(renderSessionRow)}
                    </div>
                  )}
                </section>
              );
            })
          )}
        </div>

        <div className="dsp-sidebar-footer">
          <button type="button" onClick={() => setAppSettingsOpen(true)}><Settings size={14} />设置</button>
          <DesktopHueControl hue={hue} setHue={setHue} />
        </div>
      </div>
    </aside>
  );
}

function DesktopEmptyState() {
  return (
    <div className="dsp-empty-state">
      <div className="dsp-welcome-brand">
        <div className="dsp-brand-mark">
          <span className="dsp-hero-traffic" aria-hidden="true" />
          <span className="dsp-hero-corner" aria-hidden="true" />
          <span className="dsp-hero-sidebar" aria-hidden="true" />
          <span className="dsp-hero-grid" aria-hidden="true" />
          <span className="dsp-hero-cardlet" aria-hidden="true">
            <span className="dsp-hero-logo">H</span>
          </span>
          <span className="dsp-hero-line is-one" aria-hidden="true" />
          <span className="dsp-hero-line is-two" aria-hidden="true" />
          <span className="dsp-hero-line is-three" aria-hidden="true" />
          <span className="dsp-hero-bubble is-a" aria-hidden="true" />
          <span className="dsp-hero-bubble is-b" aria-hidden="true" />
        </div>
      </div>
      <h3>你想用 Hebbian 做什么</h3>
    </div>
  );
}

function DesktopChat() {
  return (
    <main className="dsp-chat dsp-chat-host">
      <ChatView emptyState={<DesktopEmptyState />} />
    </main>
  );
}

export function DesktopShell() {
  const [hue, setHue] = useState(208);
  return (
    <div className="dsp-shell" style={hueStyle(hue)}>
      <DesktopSidebar hue={hue} setHue={setHue} />
      <DesktopChat />
      <RightSidebar
        defaultWidth={640}
        minWidth={500}
        maxWidth={960}
        storagePrefix="hebbian.desktopShell.rightSidebar.wide"
      />
    </div>
  );
}

export default DesktopShell;
