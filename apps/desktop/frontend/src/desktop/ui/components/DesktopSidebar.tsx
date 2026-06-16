import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { createPortal } from "react-dom";
import { useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent } from "react";
import {
  ArrowUpFromLine,
  Code2,
  Edit3,
  Folder,
  FolderOpen,
  GripVertical,
  Import,
  MessageSquarePlus,
  Palette,
  Plus,
  Search,
  Settings,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn, ipcConfirm, pathLeaf } from "@/desktop/ui/lib/utils";
import { api } from "@/desktop/bridge/tauri";
import type { SessionMeta, WorkspaceProject } from "@/desktop/ui/types";
import { ImportClaudeDialog } from "@/desktop/ui/components/ImportClaudeDialog";
import "./desktopShell.css";

/* ── Types ── */

interface ProjectBucket {
  id: string;
  name: string;
  path: string;
  projectId: string | null;
  sessions: SessionMeta[];
}

/* ── Theme presets ── */

type ThemePreset = {
  id: string;
  name: string;
  hue: number;
  colors: string[];
};

const THEME_PRESETS: ThemePreset[] = [
  { id: "glacier", name: "冰湖蓝绿", hue: 208, colors: ["#EAF4FF", "#EEF0FF", "#E8FFF5"] },
  { id: "mist", name: "雾蓝灰", hue: 214, colors: ["#EEF4FA", "#F3F6FA", "#E8F0F7"] },
  { id: "porcelain", name: "青瓷灰", hue: 190, colors: ["#EEF8F8", "#F4F8F6", "#E7F1F2"] },
  { id: "moon", name: "月白灰", hue: 204, colors: ["#F6F8FA", "#EEF5F7", "#F9FAFB"] },
  { id: "abyss", name: "深海墨蓝", hue: 206, colors: ["#07111C", "#102033", "#4DB8FF"] },
];

/* ── Sidebar 偏好持久化（本机记忆，跨设备/重装不保留） ── */

const PROJECT_ORDER_KEY = "hebbian.sidebar.projectOrder";
const PROJECT_COLLAPSED_KEY = "hebbian.sidebar.collapsed";
/** 对话列表高度下限，也是默认固定高度（拖动只能等于或更高） */
const SESSION_LIST_MIN_H = 180;
/** 项目卡片间距，需与 .dsp-project-group margin-bottom 保持一致（用于拖拽补位计算） */
const PROJECT_GROUP_GAP = 4;

function readStringArray(key: string): string[] {
  try {
    const raw = localStorage.getItem(key);
    const parsed = raw ? JSON.parse(raw) : null;
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/* ── Helpers ── */

function projectPath(project: WorkspaceProject) {
  return project.folders[0]?.path ?? "";
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

/** 锚点 rect → fixed 定位 style：浮窗底边贴着锚点顶边（间隙由容器内 padding 桥接） */
function popupStyle(r: DOMRect): CSSProperties {
  return {
    position: "fixed",
    left: r.left + r.width / 2,
    top: r.top,
    transform: "translate(-50%, -100%)",
  };
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
    const bucket = buckets.find(
      (b) => b.projectId === session.project_id || (!!b.path && session.workdir === b.path)
    );
    (bucket ?? defaultBucket).sessions.push(session);
  }

  for (const bucket of buckets) {
    bucket.sessions.sort((a, b) => b.updated_at - a.updated_at);
  }
  defaultBucket.sessions.sort((a, b) => b.updated_at - a.updated_at);
  return [...buckets, defaultBucket];
}

/* ── DesktopHueControl ── */

function DesktopHueControl({
  hue,
  setHue,
  themeId,
  setThemeId,
}: {
  hue: number;
  setHue: (hue: number) => void;
  themeId: string;
  setThemeId: (themeId: string) => void;
}) {
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
                className={cn("dsp-theme-preset", preset.id === themeId && "is-active")}
                onClick={() => {
                  setThemeId(preset.id);
                  setHue(preset.hue);
                }}
                title={preset.name}
              >
                <span className="dsp-theme-preset-swatch" style={{ background: `linear-gradient(135deg, ${preset.colors.join(", ")})` }} />
                <span className="dsp-theme-preset-name">{preset.name}</span>
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

/* ── DesktopSidebar ── */

export function DesktopSidebar({
  hue,
  setHue,
  themeId,
  setThemeId,
}: {
  hue: number;
  setHue: (hue: number) => void;
  themeId: string;
  setThemeId: (themeId: string) => void;
}) {
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
    refreshSessions,
    runningSessions,
    unreadFinishedSessions,
    sessionStreams,
    setAppSettingsOpen,
  } = useStore();
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchRegex, setSearchRegex] = useState(false);
  const [searchCase, setSearchCase] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set(readStringArray(PROJECT_COLLAPSED_KEY)));
  const [order, setOrder] = useState<string[]>(() => readStringArray(PROJECT_ORDER_KEY));
  const [activeTab, setActiveTab] = useState<"code" | "chat">("code");
  const [sbVisible, setSbVisible] = useState<Set<string>>(() => new Set());
  const sbTimers = useRef<Map<string, number>>(new Map());

  /* 对话列表高度（仅内存，重启重置为 SESSION_LIST_MIN_H）；拖动手柄改写它 */
  const [listHeights, setListHeights] = useState<Map<string, number>>(() => new Map());
  const heightDragRef = useRef<{ id: string; startY: number; startH: number } | null>(null);

  /* 项目拖拽排序（pointer 驱动）：drag 是渲染态，dragRef 存拖拽过程的测量快照 */
  const [drag, setDrag] = useState<{
    fromIndex: number;
    targetIndex: number;
    dy: number;
    height: number;
  } | null>(null);
  const dragRef = useRef<{ fromIndex: number; targetIndex: number; startY: number; centers: number[] } | null>(null);
  const groupRefs = useRef<Map<string, HTMLElement>>(new Map());

  /* ── hover 浮出选项（0.3s 延迟显示，离开有宽限期，方便挪到浮窗上点击） ── */
  const [importDialogOpen, setImportDialogOpen] = useState(false);
  const [hoverPopup, setHoverPopup] = useState<string | null>(null);
  const [popupAnchor, setPopupAnchor] = useState<HTMLElement | null>(null);
  const [popupRect, setPopupRect] = useState<DOMRect | null>(null);
  const showTimer = useRef<number | null>(null);
  const hideTimer = useRef<number | null>(null);

  /* 滚动时同步 popup 位置 */
  useEffect(() => {
    if (!hoverPopup || !popupAnchor) return;
    const sync = () => setPopupRect(popupAnchor.getBoundingClientRect());
    const containers = document.querySelectorAll(".dsp-project-groups, .dsp-project-session-list, .dsp-sidebar-card");
    containers.forEach((c) => c.addEventListener("scroll", sync, { passive: true }));
    window.addEventListener("scroll", sync, { passive: true });
    return () => {
      containers.forEach((c) => c.removeEventListener("scroll", sync));
      window.removeEventListener("scroll", sync);
    };
  }, [hoverPopup, popupAnchor]);

  function clearTimers() {
    if (showTimer.current !== null) { window.clearTimeout(showTimer.current); showTimer.current = null; }
    if (hideTimer.current !== null) { window.clearTimeout(hideTimer.current); hideTimer.current = null; }
  }

  function closeHover() {
    clearTimers();
    setHoverPopup(null);
    setPopupAnchor(null);
    setPopupRect(null);
  }

  /* 鼠标进入锚点按钮：0.3s 后弹出。
     fire 时复核 :hover——streaming 时 store 高频重渲染会丢失 mouseleave，
     不复核会出现鼠标已离开却照常弹出的误触。 */
  function openHover(key: string, el: HTMLElement) {
    clearTimers();
    showTimer.current = window.setTimeout(() => {
      if (!el.matches(":hover")) return;
      setPopupAnchor(el);
      setPopupRect(el.getBoundingClientRect());
      setHoverPopup(key);
    }, 300);
  }

  /* 鼠标离开按钮或浮窗：留 150ms 宽限期，期间挪到对方身上可取消关闭 */
  function scheduleClose() {
    if (showTimer.current !== null) { window.clearTimeout(showTimer.current); showTimer.current = null; }
    if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(closeHover, 150);
  }

  /* 鼠标进入浮窗：取消待关闭 */
  function keepHover() {
    if (hideTimer.current !== null) { window.clearTimeout(hideTimer.current); hideTimer.current = null; }
  }

  async function handleExportClaude(session: SessionMeta) {
    try {
      const result = await api.exportSessionToClaude(session.id, true);
      toast.success("已导出到 Claude", {
        description: `运行 \`${result.resume_command}\` 可继续`,
      });
    } catch (error: any) {
      toast.error(error.message || String(error));
    }
  }

  function sbShow(id: string) {
    const timers = sbTimers.current;
    const existing = timers.get(id);
    if (existing !== undefined) window.clearTimeout(existing);
    timers.delete(id);
    setSbVisible((prev) => { const n = new Set(prev); n.add(id); return n; });
  }

  function sbHideAfter(id: string, ms: number) {
    const timers = sbTimers.current;
    const existing = timers.get(id);
    if (existing !== undefined) window.clearTimeout(existing);
    const tid = window.setTimeout(() => {
      timers.delete(id);
      setSbVisible((prev) => { const n = new Set(prev); n.delete(id); return n; });
    }, ms);
    timers.set(id, tid);
  }

  const buckets = useMemo(
    () => buildProjectBuckets(projects, sessions, query, searchCase, searchRegex),
    [projects, query, searchCase, searchRegex, sessions]
  );

  const filteredBuckets = useMemo(() => {
    if (activeTab === "chat") {
      return buckets.filter((b) => b.projectId === null);
    }
    const projectBuckets = buckets.filter((b) => b.projectId !== null);
    if (order.length === 0) return projectBuckets;
    const rank = new Map(order.map((id, i) => [id, i]));
    // 未在 order 里的（新项目）排到末尾，保持原相对顺序
    return [...projectBuckets].sort(
      (a, b) => (rank.get(a.id) ?? Number.MAX_SAFE_INTEGER) - (rank.get(b.id) ?? Number.MAX_SAFE_INTEGER)
    );
  }, [buckets, activeTab, order]);

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
    if (!await ipcConfirm(`删除项目 "${name}"？项目下的对话不会被删除。`, "删除项目")) return;
    if (!await ipcConfirm(`再次确认删除项目 "${name}"？`, "删除项目")) return;
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
      localStorage.setItem(PROJECT_COLLAPSED_KEY, JSON.stringify([...next]));
      return next;
    });
  }

  /* ── 项目拖拽排序（pointer 驱动，整块跟手 + 其他项补位预览）── */

  function commitOrder(ids: string[]) {
    setOrder(ids);
    localStorage.setItem(PROJECT_ORDER_KEY, JSON.stringify(ids));
  }

  function onDragHandleDown(index: number, e: PointerEvent<HTMLSpanElement>) {
    e.preventDefault();
    e.stopPropagation();
    const els = filteredBuckets.map((b) => groupRefs.current.get(b.id));
    if (els.some((el) => !el)) return;
    const centers = els.map((el) => {
      const r = el!.getBoundingClientRect();
      return r.top + r.height / 2;
    });
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    dragRef.current = { fromIndex: index, targetIndex: index, startY: e.clientY, centers };
    setDrag({ fromIndex: index, targetIndex: index, dy: 0, height: els[index]!.getBoundingClientRect().height });
  }

  function onDragHandleMove(e: PointerEvent<HTMLSpanElement>) {
    const d = dragRef.current;
    if (!d) return;
    const dy = e.clientY - d.startY;
    const dragged = d.centers[d.fromIndex] + dy;
    let target = d.fromIndex;
    while (target > 0 && dragged < d.centers[target - 1]) target--;
    while (target < d.centers.length - 1 && dragged > d.centers[target + 1]) target++;
    d.targetIndex = target;
    setDrag((prev) => (prev ? { ...prev, dy, targetIndex: target } : prev));
  }

  function onDragHandleUp(e: PointerEvent<HTMLSpanElement>) {
    const d = dragRef.current;
    if (!d) return;
    (e.target as HTMLElement).releasePointerCapture(e.pointerId);
    if (d.targetIndex !== d.fromIndex) {
      const ids = filteredBuckets.map((b) => b.id);
      const [moved] = ids.splice(d.fromIndex, 1);
      ids.splice(d.targetIndex, 0, moved);
      commitOrder(ids);
    }
    dragRef.current = null;
    setDrag(null);
  }

  /** 拖拽中每个项目的 translateY：被拖项跟手，区间内其他项让位补位 */
  function dragShift(index: number): number {
    if (!drag) return 0;
    if (index === drag.fromIndex) return drag.dy;
    const span = drag.height + PROJECT_GROUP_GAP;
    if (drag.fromIndex < drag.targetIndex && index > drag.fromIndex && index <= drag.targetIndex) return -span;
    if (drag.fromIndex > drag.targetIndex && index >= drag.targetIndex && index < drag.fromIndex) return span;
    return 0;
  }

  /* 对话列表底边拖动改高度（下限 SESSION_LIST_MIN_H，不持久化） */
  function onHeightGripDown(id: string, currentH: number, e: PointerEvent<HTMLDivElement>) {
    e.preventDefault();
    e.stopPropagation();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    heightDragRef.current = { id, startY: e.clientY, startH: currentH };
  }
  function onHeightGripMove(e: PointerEvent<HTMLDivElement>) {
    const hd = heightDragRef.current;
    if (!hd) return;
    const next = Math.max(hd.startH + (e.clientY - hd.startY), SESSION_LIST_MIN_H);
    setListHeights((prev) => {
      const map = new Map(prev);
      map.set(hd.id, next);
      return map;
    });
  }
  function onHeightGripUp(e: PointerEvent<HTMLDivElement>) {
    if (!heightDragRef.current) return;
    (e.target as HTMLElement).releasePointerCapture(e.pointerId);
    heightDragRef.current = null;
  }

  async function handleDeleteSession(session: SessionMeta) {
    if (!await ipcConfirm(`删除对话 "${session.title}"？`, "删除对话")) return;
    if (!await ipcConfirm(`再次确认删除对话 "${session.title}"？此操作不可撤销。`, "删除对话")) return;
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
          onMouseEnter={(e) => openHover(`export:${session.id}`, e.currentTarget)}
          onMouseLeave={scheduleClose}
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
          <button
            className="dsp-sidebar-new-chat"
            type="button"
            onClick={() => handleNewSession(null)}
            onMouseEnter={(e) => openHover("import", e.currentTarget)}
            onMouseLeave={scheduleClose}
          >
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
            <div className="dsp-project-session-list is-chat">
              {filteredBuckets.flatMap((bucket) => bucket.sessions).map(renderSessionRow)}
            </div>
          ) : (
            (() => {
              // 最底部「展开着的」项目：未被手动拖过时撑满到「设置」上沿
              const lastExpandedId = [...filteredBuckets]
                .reverse()
                .find((b) => !collapsed.has(b.id))?.id;
              return filteredBuckets.map((bucket, index) => {
                const isCollapsed = collapsed.has(bucket.id);
                const manualH = listHeights.get(bucket.id);
                // 拖拽时关掉撑满，避免被拖项高度突变破坏补位测量
                const fill = !drag && !isCollapsed && bucket.id === lastExpandedId && manualH === undefined;
                const isDragging = drag?.fromIndex === index;
                const shift = dragShift(index);
                return (
                  <section
                    ref={(el) => {
                      if (el) groupRefs.current.set(bucket.id, el);
                      else groupRefs.current.delete(bucket.id);
                    }}
                    className={cn("dsp-project-group", fill && "is-fill", isDragging && "is-dragging", drag && "is-reordering")}
                    style={shift ? { transform: `translateY(${shift}px)` } : undefined}
                    key={bucket.id}
                  >
                    <div className="dsp-project-heading-wrap">
                      <span
                        className="dsp-project-drag-handle"
                        title="拖动调整顺序"
                        onPointerDown={(e) => onDragHandleDown(index, e)}
                        onPointerMove={onDragHandleMove}
                        onPointerUp={onDragHandleUp}
                      >
                        <GripVertical size={13} />
                      </span>
                      <button className="dsp-project-heading" type="button" onClick={() => toggleBucket(bucket.id)}>
                        {isCollapsed ? <Folder size={15} /> : <FolderOpen size={15} />}
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
                        onMouseEnter={(e) => openHover(`import:${bucket.id}`, e.currentTarget)}
                        onMouseLeave={scheduleClose}
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
                      <>
                        <div
                          className={cn(
                            "dsp-project-session-list has-overflow",
                            fill && "is-fill",
                            sbVisible.has(bucket.id) && "is-visible"
                          )}
                          style={manualH !== undefined ? { height: manualH, maxHeight: "none" } : undefined}
                          onScroll={() => { sbShow(bucket.id); sbHideAfter(bucket.id, 3000); }}
                          onMouseEnter={() => sbShow(bucket.id)}
                          onMouseLeave={() => sbHideAfter(bucket.id, 3000)}
                        >
                          {bucket.sessions.map(renderSessionRow)}
                        </div>
                        <div
                          className="dsp-list-resize-grip"
                          title="拖动调整高度"
                          onPointerDown={(e) => {
                            const list = e.currentTarget.previousElementSibling as HTMLElement | null;
                            onHeightGripDown(bucket.id, list?.clientHeight ?? SESSION_LIST_MIN_H, e);
                          }}
                          onPointerMove={onHeightGripMove}
                          onPointerUp={onHeightGripUp}
                        />
                      </>
                    )}
                  </section>
                );
              });
            })()
          )}
        </div>

        <div className="dsp-sidebar-footer">
          <button type="button" onClick={() => setAppSettingsOpen(true)}><Settings size={14} />设置</button>
          <DesktopHueControl hue={hue} setHue={setHue} themeId={themeId} setThemeId={setThemeId} />
        </div>
      </div>

      <ImportClaudeDialog
        open={importDialogOpen}
        onOpenChange={setImportDialogOpen}
        onImported={(sessionId) => {
          refreshSessions();
          openSession(sessionId);
        }}
      />

      {/* Portal 浮窗：渲染到 body，不被 sidebar overflow 裁剪 */}
      {hoverPopup && popupRect &&
        createPortal(
          <div
            className="dsp-hover-popup"
            style={popupStyle(popupRect)}
            onMouseEnter={keepHover}
            onMouseLeave={scheduleClose}
          >
            {hoverPopup.startsWith("export:") ? (
              <button
                type="button"
                className="dsp-hover-popup-btn"
                onClick={() => {
                  const s = sessions.find((x) => x.id === hoverPopup.slice(7));
                  if (s) handleExportClaude(s);
                  closeHover();
                }}
              >
                <ArrowUpFromLine size={11} />
                导出到 Claude
              </button>
            ) : (
              <button
                type="button"
                className="dsp-hover-popup-btn"
                onClick={() => {
                  closeHover();
                  setImportDialogOpen(true);
                }}
              >
                <Import size={11} />
                从 Claude 导入
              </button>
            )}
          </div>,
          document.body
        )}
    </aside>
  );
}