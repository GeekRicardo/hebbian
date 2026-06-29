import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  ChevronsRight,
  ChevronsLeft,
  FolderTree,
  Terminal,
  FilePenLine,
  FileJson,
  Globe2,
  MessagesSquare,
  ListChecks,
  ClipboardList,
  SquareTerminal,
  GitBranch,
} from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";
import { isEmbeddedPreview, isTauri } from "@/desktop/bridge/transport";
import { useStore } from "@/desktop/ui/store/useStore";
import { BackgroundTaskTab } from "./BackgroundTaskPanel";
import { EditTreeTab } from "./EditTreePanel";
import { FileTreeTab } from "./FileTreePanel";
import { GitPanel } from "./GitPanel";
import { ModelIoInspector } from "./ModelIoInspector";
import { TodoTab } from "./TodoTab";
import { PlanTab } from "./PlanTab";
import { BrowserPanel } from "./BrowserPanel";
import { TerminalSurface } from "./TerminalSurface";
import { BranchChatTab } from "./BranchChatTab";

/**
 * 右侧工作台：固定列布局（被 ChatView 的 grid 让位），承载多个 tab。
 *
 * 设计要点：
 * - **不浮动**：作为 ChatView 的 grid 第三列存在，挤压 chat 区域（不是 overlay）
 * - **可折叠**：右上角箭头按钮收起到 36px 宽，只剩 tab 图标列；再点恢复
 * - **左边缘可拖**：用户拖动左侧 4px 抓手改 sidebar 宽度
 * - **宽度不持久化**：拖动宽度只在本次运行内记忆（模块级 Map），重启回各 tab 默认；
 *   折叠态 / 当前 tab 走 localStorage 跨会话保留
 *
 * 内容数据全部走对应 tab 组件自己 fetch；sidebar 不持有业务数据，仅管布局。
 */

type TabId = "files" | "tasks" | "edits" | "git" | "todos" | "plans" | "branches" | "browser" | "terminal";

const TAB_IDS: TabId[] = ["files", "tasks", "edits", "git", "todos", "plans", "branches", "browser", "terminal"];

const STORAGE_PREFIX = "hebbian.rightSidebar";

const DEFAULT_WIDTH = 320;
const MIN_WIDTH = 240;
const MAX_WIDTH = 720;
const COLLAPSED_WIDTH = 36;

// 各 tab 打开时的默认宽度（px）。注意：实际生效值会被外部传入的 [minWidth, maxWidth]
// clamp——DesktopShell 传 minWidth=200，故这里所有值需 ≥200 才能原样生效。
const TAB_DEFAULT_WIDTH: Record<TabId, number> = {
  files: 250,
  tasks: 250,
  edits: 250,
  git: 320,
  todos: 250,
  plans: 250,
  branches: 500,
  browser: 750,
  terminal: 500,
};

// 拖动宽度只在本次 App 运行内记忆（模块级内存）；重启回各 tab 默认宽度。
// 之前持久化进 localStorage，导致调过一次默认值就永远看不到新默认。
const sessionWidths = new Map<TabId, number>();

interface RightSidebarProps {
  defaultWidth?: number;
  minWidth?: number;
  maxWidth?: number;
  storagePrefix?: string;
}

function loadInitial<T>(key: string, fallback: T, parse: (raw: string) => T): T {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    return parse(raw);
  } catch {
    return fallback;
  }
}

export function RightSidebar({
  defaultWidth = DEFAULT_WIDTH,
  minWidth = MIN_WIDTH,
  maxWidth = MAX_WIDTH,
  storagePrefix = STORAGE_PREFIX,
}: RightSidebarProps = {}) {
  const storageCollapsedKey = `${storagePrefix}.collapsed`;
  const storageTabKey = `${storagePrefix}.tab`;

  // 浏览器 / 终端 tab 依赖 Tauri 原生窗口能力：web surface（hebweb）没有、自举（内置浏览器
  // 嵌套加载本前端）时套娃。两种情况都不显示这两个 tab，避免点了触发 not implemented / ACL 报错。
  const nativeTabsAvailable = isTauri() && !isEmbeddedPreview();

  const clampWidthForTab = useCallback(
    (_id: TabId, value: number) => Math.min(maxWidth, Math.max(minWidth, value)),
    [minWidth, maxWidth],
  );
  const loadWidthForTab = useCallback(
    (id: TabId) => {
      const tabDefaultWidth = clampWidthForTab(id, TAB_DEFAULT_WIDTH[id] ?? defaultWidth);
      const remembered = sessionWidths.get(id);
      return remembered !== undefined ? clampWidthForTab(id, remembered) : tabDefaultWidth;
    },
    [defaultWidth, clampWidthForTab],
  );

  // 首次打开默认折叠（仅显示 36px 图标列），用户主动点开。
  // localStorage 有记录则用记录值。
  const [collapsed, setCollapsed] = useState(() =>
    loadInitial(storageCollapsedKey, true, (s) => s === "1")
  );
  const [tab, setTab] = useState<TabId>(() =>
    loadInitial<TabId>(storageTabKey, "tasks", (s) => {
      const valid = (TAB_IDS as string[]).includes(s) ? (s as TabId) : "tasks";
      // 浏览器/终端是 Tauri 原生窗口能力：web surface（hebweb）无此能力、自举时套娃，
      // 两种情况都纠正掉残留的旧 tab 值，否则会挂出不可用面板 / 触发命令报错。
      if (!nativeTabsAvailable && (valid === "browser" || valid === "terminal")) {
        return "tasks";
      }
      return valid;
    })
  );
  const [width, setWidth] = useState(() => loadWidthForTab(tab));
  const [resizing, setResizing] = useState(false);

  // 用户主动停在浏览器/终端 tab 时，agent 更新（todos/edits）不该抢走焦点——否则原生子
  // webview 被切走隐藏，正在加载的慢页面（如 baidu）会黑屏。tabRef 供下面不依赖 tab 的
  // 自动切 tab effect 读最新值。
  const tabRef = useRef(tab);
  tabRef.current = tab;
  const autoSwitchBlocked = () => tabRef.current === "browser" || tabRef.current === "terminal";

  // 浏览器 tab 懒挂载：首次切到它才创建子 webview（没人看就不起浏览器）。
  // 一旦挂上就保留（切走靠 hidden + setVisible(false)），直到 sidebar 折叠卸载整个展开视图。
  const [browserMounted, setBrowserMounted] = useState(tab === "browser");
  useEffect(() => {
    if (tab === "browser") setBrowserMounted(true);
  }, [tab]);

  // 终端 tab 同样懒挂载 + 切走只隐藏（保住 xterm 实例与输出订阅，PTY 在 Rust 端常驻）。
  const [terminalMounted, setTerminalMounted] = useState(tab === "terminal");
  useEffect(() => {
    if (tab === "terminal") setTerminalMounted(true);
  }, [tab]);

  // Model I/O Drawer 由本 sidebar 持有：debug 开启时多一个入口，点击打开覆盖式查看器。
  // 不放进 tab 内嵌是因为 Inspector 信息密度极大（RequestDetail/N 条 MessageRow/嵌套 PrettyJson），
  // 320px tab 容不下。
  const debugEnabled = useStore((s) => s.debugEnabled);
  const settingsOpen = useStore((s) => s.settingsOpen);
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const sessionWorkdir = useStore((s) => s.currentSession?.workdir ?? null);
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
    if (autoSwitchBlocked()) return; // 用户在浏览器/终端 tab，不抢焦点
    setCollapsed(false);
    setTab("todos");
  }, [sessionId, todosKey, todos.length]);

  // 自动聚焦只在「模型刚提交一次修改」那一下触发——由 store 的一次性
  // expandEditsRunId 信号驱动（store 仅在 run_edits_committed 落到当前会话时设值）。
  // 加载历史 / 回退 / 切对话都不会设这个信号，故重启后打开任意对话不会误弹。
  // 用户原话："只有跑完那一下会自动跳到修改文件 sidebar，后面切换都不会自动了"。
  const expandEditsRunId = useStore((s) => s.expandEditsRunId);
  const prevExpandRunIdRef = useRef(expandEditsRunId);
  useEffect(() => {
    if (expandEditsRunId === prevExpandRunIdRef.current) return;
    prevExpandRunIdRef.current = expandEditsRunId;
    if (!expandEditsRunId) return;
    if (autoSwitchBlocked()) return; // 用户在浏览器/终端 tab，不抢焦点
    setCollapsed(false);
    setTab("edits");
    window.setTimeout(() => {
      const node = document.getElementById(`run-edits-${expandEditsRunId}`);
      node?.scrollIntoView({ block: "nearest", behavior: "smooth" });
      node?.classList.add("ring-2", "ring-emerald-400", "ring-offset-2", "ring-offset-background");
      window.setTimeout(() => {
        node?.classList.remove("ring-2", "ring-emerald-400", "ring-offset-2", "ring-offset-background");
      }, 1500);
    }, 50);
  }, [expandEditsRunId]);

  // 用户发送消息 → 缓慢折叠工作台（store 一次性 tick 信号驱动；与上面「Run 跑完
  // 自动展开」配对）。首帧不折叠，只对真实的 tick 自增响应。
  const collapseTick = useStore((s) => s.collapseRightSidebarTick);
  const prevCollapseTickRef = useRef(collapseTick);
  useEffect(() => {
    if (collapseTick === prevCollapseTickRef.current) return;
    prevCollapseTickRef.current = collapseTick;
    // 用户停在终端 / 浏览器 tab 时不自动折叠——这俩是用户主动盯着的工作区（终端在跑、
    // 网页在看），自动收起会打断他，且浏览器原生子 webview 折叠后还要额外收可见性。
    if (tabRef.current === "terminal" || tabRef.current === "browser") return;
    setCollapsed(true);
  }, [collapseTick]);

  // plan 待审批（架构 §4.4.5）：HITL 决策搬进计划栏后，待审批 plan 出现时自动展开
  // sidebar + 切到计划 tab，确保用户看到决策入口。plan 审批是阻塞性的（agent 在等），
  // 比 todos 更需要露出，故无视 autoSwitchBlocked。
  const pendingApprovalKind = useStore((s) => s.pendingApproval?.kind ?? null);
  const pendingPlanId = useStore((s) =>
    s.pendingApproval?.kind === "plan" ? s.pendingApproval.plan?.plan_id ?? null : null,
  );
  const pendingPlanSummary = useStore((s) =>
    s.pendingApproval?.kind === "plan" ? s.pendingApproval.plan?.summary ?? "" : "",
  );
  const openPlan = useStore((s) => s.openPlan);
  const prevPendingPlanIdRef = useRef<string | null>(null);
  useEffect(() => {
    const isPlanPending = pendingApprovalKind === "plan" && pendingPlanId !== null;
    if (isPlanPending && pendingPlanId !== prevPendingPlanIdRef.current) {
      prevPendingPlanIdRef.current = pendingPlanId;
      setCollapsed(false);
      setTab("plans");
      // 同时在中间编辑区开 plan tab：审批条 + 正文都在编辑区里，用户直接看到决策入口
      openPlan(pendingPlanId, pendingPlanSummary || "待审批计划");
    }
    if (!isPlanPending) prevPendingPlanIdRef.current = null;
  }, [pendingApprovalKind, pendingPlanId, pendingPlanSummary, openPlan]);

  // 点链接选了「内置浏览器」打开（架构 §8.5）→ 切到 browser tab 并展开。实际导航由
  // BrowserPanel 监听同一信号执行；这里只负责把 tab 露出来，否则用户看不到打开了哪。
  const browserNavTick = useStore((s) => s.browserNavigateRequest.tick);
  const prevBrowserNavTickRef = useRef(browserNavTick);
  useEffect(() => {
    if (browserNavTick === prevBrowserNavTickRef.current) return;
    prevBrowserNavTickRef.current = browserNavTick;
    setCollapsed(false);
    setTab("browser");
  }, [browserNavTick]);

  useEffect(() => {
    setWidth(loadWidthForTab(tab));
  }, [tab, loadWidthForTab]);

  // 持久化折叠状态
  useEffect(() => {
    localStorage.setItem(storageCollapsedKey, collapsed ? "1" : "0");
  }, [storageCollapsedKey, collapsed]);
  useEffect(() => {
    if (!collapsed) sessionWidths.set(tab, width);
  }, [tab, width, collapsed]);
  useEffect(() => {
    localStorage.setItem(storageTabKey, tab);
  }, [storageTabKey, tab]);

  // 拖拽逻辑：mousedown 在左边缘 → 固定右边缘，只移动左边缘更新宽度。
  const dragRef = useRef<{ startX: number; startWidth: number } | null>(null);
  const onDragStart = useCallback(
    (e: React.MouseEvent) => {
      if (collapsed) return;
      e.preventDefault();
      dragRef.current = { startX: e.clientX, startWidth: width };
      setResizing(true);
      document.body.style.cursor = "ew-resize";
      document.body.style.userSelect = "none";

      const onMove = (ev: MouseEvent) => {
        const drag = dragRef.current;
        if (!drag) return;
        const deltaX = ev.clientX - drag.startX;
        const next = clampWidthForTab(tab, drag.startWidth - deltaX);
        setWidth(next);
      };
      const onUp = () => {
        dragRef.current = null;
        setResizing(false);
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [collapsed, width, tab, clampWidthForTab]
  );

  // 折叠态图标列（折叠后展示，可点图标直接展开到对应 tab）。
  const collapsedIcons = (
    <div className="flex h-full w-9 shrink-0 flex-col">
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
        icon={<FolderTree className="h-4 w-4" />}
        label="文件目录"
        onClick={() => {
          setTab("files");
          setCollapsed(false);
        }}
        active={tab === "files"}
      />
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
        icon={<GitBranch className="h-4 w-4" />}
        label="源代码管理"
        onClick={() => {
          setTab("git");
          setCollapsed(false);
        }}
        active={tab === "git"}
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
      <SidebarIconButton
        icon={<MessagesSquare className="h-4 w-4" />}
        label="旁支对话"
        onClick={() => {
          setTab("branches");
          setCollapsed(false);
        }}
        active={tab === "branches"}
      />
      {/* 浏览器 / 终端是 Tauri 原生窗口专属功能：web surface（hebweb）无此能力、自举
         （本前端被内置浏览器嵌套加载）时套娃，两种情况都隐藏，避免触发命令报错 / ACL 报错。 */}
      {nativeTabsAvailable && (
        <>
          <SidebarIconButton
            icon={<Globe2 className="h-4 w-4" />}
            label="内置浏览器"
            onClick={() => {
              setTab("browser");
              setCollapsed(false);
            }}
            active={tab === "browser"}
          />
          <SidebarIconButton
            icon={<SquareTerminal className="h-4 w-4" />}
            label="终端"
            onClick={() => {
              setTab("terminal");
              setCollapsed(false);
            }}
            active={tab === "terminal"}
          />
        </>
      )}
      {debugEnabled && sessionId && (
        <SidebarIconButton
          icon={<FileJson className="h-4 w-4" />}
          label="Model I/O"
          onClick={() => setModelIoOpen(true)}
          active={false}
        />
      )}
    </div>
  );

  return (
    <>
      {/*
        单 aside 外壳：width 在 36px（折叠）↔ width px（展开）之间走 500ms 过渡，
        实现「缓慢折叠」。内部两套内容（折叠图标列 / 展开完整面板）按 collapsed 切换并
        各自固定宽度，靠外壳 overflow-hidden 裁切，宽度收缩时内容不被挤压变形。
      */}
      <aside
        className={cn(
          "relative flex h-full shrink-0 justify-self-end flex-col overflow-hidden border-l border-border bg-muted/40",
          resizing ? "" : "transition-[width] duration-500 ease-in-out"
        )}
        style={{ width: `${collapsed ? COLLAPSED_WIDTH : width}px` }}
      >
        {collapsed ? (
          collapsedIcons
        ) : (
          <div
            className="flex h-full flex-col"
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
                  id="files"
                  current={tab}
                  onClick={setTab}
                  icon={<FolderTree className="h-3.5 w-3.5" />}
                  label="文件目录"
                />
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
                  id="git"
                  current={tab}
                  onClick={setTab}
                  icon={<GitBranch className="h-3.5 w-3.5" />}
                  label="源代码管理"
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
                <SidebarTab
                  id="branches"
                  current={tab}
                  onClick={setTab}
                  icon={<MessagesSquare className="h-3.5 w-3.5" />}
                  label="旁支对话"
                />
                <SidebarTab
                  id="browser"
                  current={tab}
                  onClick={setTab}
                  icon={<Globe2 className="h-3.5 w-3.5" />}
                  label="浏览器"
                />
                <SidebarTab
                  id="terminal"
                  current={tab}
                  onClick={setTab}
                  icon={<SquareTerminal className="h-3.5 w-3.5" />}
                  label="终端"
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

            {/* tab 内容区。浏览器 tab 例外：常驻挂载、切走只隐藏（hidden）不卸载——
               原生子 webview 重建代价大且会丢页面/登录态。其余 tab 是纯 React，条件渲染即可。 */}
            <div className="relative min-h-0 flex-1">
              <div
                className={cn(
                  "h-full overflow-auto",
                  (tab === "browser" || tab === "terminal") && "hidden",
                )}
              >
                {tab === "files" && <FileTreeTab />}
                {tab === "tasks" && <BackgroundTaskTab />}
                {tab === "edits" && <EditTreeTab />}
                {tab === "git" && <GitPanel />}
                {tab === "todos" && <TodoTab />}
                {tab === "plans" && <PlanTab />}
                {tab === "branches" && <BranchChatTab />}
              </div>
              {browserMounted && (
                <div className={cn("absolute inset-0", tab !== "browser" && "hidden")}>
                  <BrowserPanel active={tab === "browser"} obscured={modelIoOpen || settingsOpen} />
                </div>
              )}
              {terminalMounted && (
                <div className={cn("absolute inset-0", tab !== "terminal" && "hidden")}>
                  <TerminalSurface
                    variant="embedded"
                    active={tab === "terminal"}
                    defaultCwd={sessionWorkdir}
                  />
                </div>
              )}
            </div>
          </div>
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
