import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  ChevronsRight,
  ChevronsLeft,
  Terminal,
  FilePenLine,
  FileJson,
  ListChecks,
  ClipboardList,
} from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";
import { BackgroundTaskTab } from "./BackgroundTaskPanel";
import { EditTreeTab } from "./EditTreePanel";
import { ModelIoInspector } from "./ModelIoInspector";
import { TodoTab } from "./TodoTab";
import { PlanTab } from "./PlanTab";

/**
 * 右侧工作台：固定列布局（被 ChatView 的 grid 让位），承载「后台任务 / 修改文件」两个 tab。
 *
 * 设计要点：
 * - **不浮动**：作为 ChatView 的 grid 第三列存在，挤压 chat 区域（不是 overlay）
 * - **可折叠**：右上角箭头按钮收起到 32px 宽，只剩 tab 图标列；再点恢复
 * - **左边缘可拖**：用户拖动左侧 4px 抓手改 sidebar 宽度（240-600 范围）
 * - **状态持久化**：宽度 / 折叠态 / 当前 tab 全部走 localStorage，跨会话刷新保留
 *
 * 内容数据全部走对应 tab 组件自己 fetch（BackgroundTaskTab / EditTreeTab）；
 * sidebar 不持有业务数据，仅管布局。
 */

type TabId = "tasks" | "edits" | "todos" | "plans";

const TAB_IDS: TabId[] = ["tasks", "edits", "todos", "plans"];

const STORAGE_WIDTH = "hebbian.rightSidebar.width";
const STORAGE_COLLAPSED = "hebbian.rightSidebar.collapsed";
const STORAGE_TAB = "hebbian.rightSidebar.tab";

const DEFAULT_WIDTH = 320;
const MIN_WIDTH = 240;
const MAX_WIDTH = 600;
const COLLAPSED_WIDTH = 36;

function loadInitial<T>(key: string, fallback: T, parse: (raw: string) => T): T {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    return parse(raw);
  } catch {
    return fallback;
  }
}

export function RightSidebar() {
  // 首次打开默认折叠（仅显示 36px 图标列），用户主动点开。
  // localStorage 有记录则用记录值。
  const [collapsed, setCollapsed] = useState(() =>
    loadInitial(STORAGE_COLLAPSED, true, (s) => s === "1")
  );
  const [width, setWidth] = useState(() =>
    loadInitial(STORAGE_WIDTH, DEFAULT_WIDTH, (s) => {
      const n = Number(s);
      return Number.isFinite(n) && n >= MIN_WIDTH && n <= MAX_WIDTH ? n : DEFAULT_WIDTH;
    })
  );
  const [tab, setTab] = useState<TabId>(() =>
    loadInitial<TabId>(STORAGE_TAB, "tasks", (s) =>
      (TAB_IDS as string[]).includes(s) ? (s as TabId) : "tasks"
    )
  );

  // Model I/O Drawer 由本 sidebar 持有：debug 开启时多一个入口，点击打开覆盖式查看器。
  // 不放进 tab 内嵌是因为 Inspector 信息密度极大（RequestDetail/N 条 MessageRow/嵌套 PrettyJson），
  // 320px tab 容不下。
  const debugEnabled = useStore((s) => s.debugEnabled);
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const todos = useStore((s) => s.todos);
  const [modelIoOpen, setModelIoOpen] = useState(false);
  const closeModelIo = useCallback(() => setModelIoOpen(false), []);

  // todo 列表变化时自动聚焦：用 (id, status) 列表的 stable hash 判断是否"真变化"——
  // 仅模型 TodoWrite 触发的 store 写入会让这个 key 变；mirrorFromSlot 同步引用不算。
  // 用户原话："新增任务列表时，自动展开右侧 sidebar 并聚焦任务列表 tab"。
  //
  // **跨 session 切换不抢焦点**：用 sessionIdRef 跟踪 session 边界，切到新 session
  // 时只重置基线、不触发 setTab。仅同一 session 内 todosKey 变化（=agent 调了
  // TodoWrite）才聚焦。这样：
  //   - 用户从无 todo 的 session 切到有 todo 的 session → 保留用户当前 tab 偏好
  //   - 用户停在某 session → agent 加任务 → sidebar 自动跳出来 todos tab
  const todosKey = useMemo(
    () => todos.map((t) => `${t.id}:${t.status}`).join("|"),
    [todos],
  );
  const prevTodosKeyRef = useRef(todosKey);
  const prevSessionIdRef = useRef(sessionId);
  useEffect(() => {
    // 切 session（含从 null 进入第一个 session）：重置基线，不抢焦点
    if (prevSessionIdRef.current !== sessionId) {
      prevSessionIdRef.current = sessionId;
      prevTodosKeyRef.current = todosKey;
      return;
    }
    if (todosKey === prevTodosKeyRef.current) return;
    prevTodosKeyRef.current = todosKey;
    if (todos.length === 0) return;
    setCollapsed(false);
    setTab("todos");
  }, [sessionId, todosKey, todos.length]);

  // 持久化折叠状态
  useEffect(() => {
    localStorage.setItem(STORAGE_COLLAPSED, collapsed ? "1" : "0");
  }, [collapsed]);
  useEffect(() => {
    if (!collapsed) localStorage.setItem(STORAGE_WIDTH, String(width));
  }, [width, collapsed]);
  useEffect(() => {
    localStorage.setItem(STORAGE_TAB, tab);
  }, [tab]);

  // 拖拽逻辑：mousedown 在左边缘 → 进入 dragging 模式 → mousemove 更新宽度
  const dragRef = useRef<{ startX: number; startWidth: number } | null>(null);
  const onDragStart = useCallback(
    (e: React.MouseEvent) => {
      if (collapsed) return;
      e.preventDefault();
      dragRef.current = { startX: e.clientX, startWidth: width };
      document.body.style.cursor = "ew-resize";
      document.body.style.userSelect = "none";

      const onMove = (ev: MouseEvent) => {
        if (!dragRef.current) return;
        // 拖向左 = 增宽；拖向右 = 减宽
        const delta = dragRef.current.startX - ev.clientX;
        const next = Math.min(
          MAX_WIDTH,
          Math.max(MIN_WIDTH, dragRef.current.startWidth + delta)
        );
        setWidth(next);
      };
      const onUp = () => {
        dragRef.current = null;
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [collapsed, width]
  );

  // 折叠态：仅显示一列图标 + 展开按钮；点图标也直接展开到对应 tab
  if (collapsed) {
    return (
      <>
        <aside
          className="relative flex h-full flex-col border-l border-border"
          style={{ width: `${COLLAPSED_WIDTH}px` }}
        >
          <button
            type="button"
            onClick={() => setCollapsed(false)}
            className="grid h-9 w-full place-items-center border-b border-border text-muted-foreground hover:bg-accent hover:text-foreground"
            title="展开工作台"
            aria-label="展开工作台"
          >
            <ChevronsLeft className="h-4 w-4" />
          </button>
          <SidebarIconButton
            icon={<Terminal className="h-4 w-4" />}
            label="后台任务"
            onClick={() => {
              setTab("tasks");
              setCollapsed(false);
            }}
            active={tab === "tasks"}
          />
          <SidebarIconButton
            icon={<FilePenLine className="h-4 w-4" />}
            label="修改文件"
            onClick={() => {
              setTab("edits");
              setCollapsed(false);
            }}
            active={tab === "edits"}
          />
          <SidebarIconButton
            icon={<ListChecks className="h-4 w-4" />}
            label="任务清单"
            onClick={() => {
              setTab("todos");
              setCollapsed(false);
            }}
            active={tab === "todos"}
          />
          <SidebarIconButton
            icon={<ClipboardList className="h-4 w-4" />}
            label="计划"
            onClick={() => {
              setTab("plans");
              setCollapsed(false);
            }}
            active={tab === "plans"}
          />
          {debugEnabled && sessionId && (
            <SidebarIconButton
              icon={<FileJson className="h-4 w-4" />}
              label="Model I/O"
              onClick={() => setModelIoOpen(true)}
              active={false}
            />
          )}
        </aside>
        {sessionId && (
          <ModelIoInspector
            sessionId={sessionId}
            open={modelIoOpen}
            onClose={closeModelIo}
          />
        )}
      </>
    );
  }

  return (
    <>
      <aside
        className="relative flex h-full flex-col border-l border-border bg-muted/40"
        style={{ width: `${width}px` }}
      >
        {/* 拖拽抓手：左边缘 4px 透明区域，hover 时显示一条细线 */}
        <div
          onMouseDown={onDragStart}
          className="absolute left-0 top-0 z-10 h-full w-1 cursor-ew-resize hover:bg-primary/30"
          title="拖动改宽度"
          aria-label="调整工作台宽度"
        />

        {/* 顶栏：tab 切换 + 折叠按钮。
           - tab 列表始终显示完整中文标签（不缩成图标）
           - 容器宽度不够时整条横向滚动
           - 鼠标在 tab 区上下滚动会被 onWheel 转成横向滚动，免按 Shift */}
        <div className="flex h-9 shrink-0 items-stretch border-b border-border bg-background/50">
          <TabScroller>
            <SidebarTab
              id="tasks"
              current={tab}
              onClick={setTab}
              icon={<Terminal className="h-3.5 w-3.5" />}
              label="后台任务"
            />
            <SidebarTab
              id="edits"
              current={tab}
              onClick={setTab}
              icon={<FilePenLine className="h-3.5 w-3.5" />}
              label="修改文件"
            />
            <SidebarTab
              id="todos"
              current={tab}
              onClick={setTab}
              icon={<ListChecks className="h-3.5 w-3.5" />}
              label="任务清单"
            />
            <SidebarTab
              id="plans"
              current={tab}
              onClick={setTab}
              icon={<ClipboardList className="h-3.5 w-3.5" />}
              label="计划"
            />
          </TabScroller>
          <div className="flex shrink-0 items-center gap-0.5 border-l border-border/40 bg-background/50 pl-1 pr-1">
            {debugEnabled && sessionId && (
              <button
                type="button"
                onClick={() => setModelIoOpen(true)}
                className="grid h-6 w-6 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
                title="Model I/O"
                aria-label="Model I/O"
              >
                <FileJson className="h-3.5 w-3.5" />
              </button>
            )}
            <button
              type="button"
              onClick={() => setCollapsed(true)}
              className="grid h-6 w-6 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
              title="折叠工作台"
              aria-label="折叠工作台"
            >
              <ChevronsRight className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>

        {/* tab 内容区 */}
        <div className="min-h-0 flex-1 overflow-auto">
          {tab === "tasks" && <BackgroundTaskTab />}
          {tab === "edits" && <EditTreeTab />}
          {tab === "todos" && <TodoTab />}
          {tab === "plans" && <PlanTab />}
        </div>
      </aside>
      {sessionId && (
        <ModelIoInspector
          sessionId={sessionId}
          open={modelIoOpen}
          onClose={closeModelIo}
        />
      )}
    </>
  );
}

function SidebarTab({
  id,
  current,
  onClick,
  icon,
  label,
}: {
  id: TabId;
  current: TabId;
  onClick: (id: TabId) => void;
  icon: ReactNode;
  label: string;
}) {
  const active = id === current;
  return (
    <button
      type="button"
      onClick={() => onClick(id)}
      title={label}
      aria-label={label}
      className={cn(
        "inline-flex h-7 shrink-0 items-center gap-1 whitespace-nowrap rounded px-2 text-[12px] transition-colors",
        active
          ? "bg-background text-foreground shadow-sm"
          : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
      )}
    >
      {icon}
      <span>{label}</span>
    </button>
  );
}

/**
 * Tab 列表的横向滚动容器。
 *
 * - 不够宽时整条 tab 横向滚动；不裁切文字
 * - 滚动条做隐式（仅 hover 出现，靠 [data-tab-scroller] 全局样式控制）
 * - 鼠标滚轮（垂直）→ 横向滚动：让用户在窄 sidebar 里不必按 Shift 也能切 tab
 */
function TabScroller({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      // 只把垂直滚动转横向；用户主动按 shift 已经天然是横向，不重复处理
      if (e.shiftKey) return;
      // 如果横向 delta 已经不为 0（触摸板横滑 / 横向滚轮），交给浏览器原生处理
      if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
      if (e.deltaY === 0) return;
      // 滚到尽头时让事件冒泡（页面继续滚），避免在边界吞掉用户输入
      const maxScroll = el.scrollWidth - el.clientWidth;
      const next = el.scrollLeft + e.deltaY;
      if ((e.deltaY > 0 && el.scrollLeft >= maxScroll) ||
          (e.deltaY < 0 && el.scrollLeft <= 0)) {
        return;
      }
      e.preventDefault();
      el.scrollLeft = Math.max(0, Math.min(maxScroll, next));
    };
    // passive: false 才能 preventDefault
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  return (
    <div
      ref={ref}
      className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto px-1.5 [scrollbar-width:thin]"
      style={{ scrollbarColor: "transparent transparent" }}
      onMouseEnter={(e) => {
        // hover 时显示滚动条颜色（仅本元素，避免侵入全局样式）
        (e.currentTarget as HTMLDivElement).style.scrollbarColor =
          "rgb(0 0 0 / 0.2) transparent";
      }}
      onMouseLeave={(e) => {
        (e.currentTarget as HTMLDivElement).style.scrollbarColor =
          "transparent transparent";
      }}
    >
      {children}
    </div>
  );
}

function SidebarIconButton({
  icon,
  label,
  onClick,
  active,
}: {
  icon: ReactNode;
  label: string;
  onClick: () => void;
  active: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "grid h-9 w-full place-items-center text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
        active && "bg-accent/40 text-foreground"
      )}
      title={label}
      aria-label={label}
    >
      {icon}
    </button>
  );
}
