import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronsRight, ChevronsLeft, Terminal, FilePenLine, FileJson } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";
import { BackgroundTaskTab } from "./BackgroundTaskPanel";
import { EditTreeTab } from "./EditTreePanel";
import { ModelIoInspector } from "./ModelIoInspector";

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

type TabId = "tasks" | "edits";

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
      s === "edits" ? "edits" : "tasks"
    )
  );

  // Model I/O Drawer 由本 sidebar 持有：debug 开启时多一个入口，点击打开覆盖式查看器。
  // 不放进 tab 内嵌是因为 Inspector 信息密度极大（RequestDetail/N 条 MessageRow/嵌套 PrettyJson），
  // 320px tab 容不下。
  const debugEnabled = useStore((s) => s.debugEnabled);
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const [modelIoOpen, setModelIoOpen] = useState(false);
  const closeModelIo = useCallback(() => setModelIoOpen(false), []);

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

        {/* 顶栏：tab 切换 + 折叠按钮 */}
        <div className="flex h-9 shrink-0 items-center border-b border-border bg-background/50 pl-1.5 pr-1">
          <SidebarTab id="tasks" current={tab} onClick={setTab} icon={<Terminal className="h-3.5 w-3.5" />}>
            后台任务
          </SidebarTab>
          <SidebarTab id="edits" current={tab} onClick={setTab} icon={<FilePenLine className="h-3.5 w-3.5" />}>
            修改文件
          </SidebarTab>
          <div className="flex-1" />
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

        {/* tab 内容区 */}
        <div className="min-h-0 flex-1 overflow-auto">
          {tab === "tasks" && <BackgroundTaskTab />}
          {tab === "edits" && <EditTreeTab />}
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
  children,
}: {
  id: TabId;
  current: TabId;
  onClick: (id: TabId) => void;
  icon: ReactNode;
  children: ReactNode;
}) {
  const active = id === current;
  return (
    <button
      type="button"
      onClick={() => onClick(id)}
      className={cn(
        "inline-flex h-7 items-center gap-1.5 rounded px-2 text-[12px] transition-colors",
        active
          ? "bg-background text-foreground shadow-sm"
          : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
      )}
    >
      {icon}
      <span>{children}</span>
    </button>
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
