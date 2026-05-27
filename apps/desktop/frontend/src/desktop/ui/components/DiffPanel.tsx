import { Fragment, useEffect, useMemo, useRef, useState, useCallback } from "react";
import { createPortal } from "react-dom";
import {
  X,
  Columns2,
  Rows3,
  Maximize2,
  Minimize2,
  ArrowLeft,
  ArrowRight,
  UnfoldVertical,
} from "lucide-react";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import { PathHint } from "@/desktop/ui/components/PathHint";
import type { DiffPayload, EditEntry } from "@/desktop/ui/types";

/**
 * 把全屏内容渲到 ChatView 末尾的 `#chat-fullscreen-anchor`。
 * 锚点是 ChatView 内 `absolute inset-0 pointer-events-none` 的覆盖层——
 * 这样放大只覆盖 chat 区域（不挡 sidebar），且需要交互时自己开 pointer-events-auto。
 * 锚点不存在时（未渲染 ChatView）回退到 document.body。
 */
export function FullscreenPortal({ children }: { children: React.ReactNode }) {
  const [host, setHost] = useState<HTMLElement | null>(null);
  useEffect(() => {
    setHost(
      typeof document !== "undefined"
        ? document.getElementById("chat-fullscreen-anchor") ?? document.body
        : null,
    );
  }, []);
  if (!host) return null;
  return createPortal(children, host);
}

/**
 * Diff 渲染布局（架构 §4.13.9）：
 * - inline: 单列 unified view（行号 + +/- 符号）
 * - split:  左右两栏分屏
 *
 * 注：是否「放大」由调用方独立管理（`expanded` state + FullscreenPortal 包装），
 * 不再揉进 DiffMode——拆开后切换布局不会误关掉放大框。
 */
export type DiffMode = "inline" | "split";

interface DiffRow {
  left: string;
  right: string;
  kind: "same" | "add" | "remove";
}

/** LCS-based diff: computes aligned rows marking added/removed/same lines. */
function computeDiff(beforeLines: string[], afterLines: string[]): DiffRow[] {
  const m = beforeLines.length;
  const n = afterLines.length;
  const dp = new Uint16Array((m + 1) * (n + 1));
  const idx = (i: number, j: number) => i * (n + 1) + j;

  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (beforeLines[i - 1] === afterLines[j - 1]) {
        dp[idx(i, j)] = dp[idx(i - 1, j - 1)] + 1;
      } else {
        dp[idx(i, j)] = Math.max(dp[idx(i - 1, j)], dp[idx(i, j - 1)]);
      }
    }
  }

  const rev: DiffRow[] = [];
  let i = m;
  let j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && beforeLines[i - 1] === afterLines[j - 1]) {
      rev.push({ left: beforeLines[i - 1], right: afterLines[j - 1], kind: "same" });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || dp[idx(i, j - 1)] >= dp[idx(i - 1, j)])) {
      rev.push({ left: "", right: afterLines[j - 1], kind: "add" });
      j--;
    } else {
      rev.push({ left: beforeLines[i - 1], right: "", kind: "remove" });
      i--;
    }
  }
  return rev.reverse();
}

interface DiffViewerProps {
  /** 修改前的全文文本。Edit 流式时是 `args.old_string` 已收部分；Write 时为 ""。 */
  beforeText: string;
  /** 修改后的全文文本。Edit 流式时是 `args.new_string`；Write 时为 `args.content`。 */
  afterText: string;
  /** 文件路径（顶栏文件名 + 全路径 hover）。空字符串则用 fallback 文案。 */
  filePath?: string;
  /** 顶栏右上角动作标签："修改文件" / "创建文件" / "覆盖文件"… */
  actionLabel?: string;
  /** 当前布局（仅 inline / split）。 */
  mode: DiffMode;
  onCycleMode: () => void;
  /** 流式态：在 after 末尾追加光标占位，提示参数仍在写入。 */
  streaming?: boolean;
  /** 提供则在顶栏右侧渲染关闭按钮（放大 / 浮层场景）。 */
  onClose?: () => void;
  /** 顶栏左侧的额外标签（例如「审批前预览」）。 */
  badge?: string;
  /**
   * 当前是否处于放大态。仅影响顶栏切换按钮图标（Maximize2 ↔ Minimize2）。
   * 实际放大渲染由调用方包 FullscreenPortal 完成。
   */
  expanded?: boolean;
  /** 提供则在顶栏渲染放大/缩小按钮。 */
  onToggleExpanded?: () => void;
  /** 顶栏右侧追加渲染：留给将来自定义按钮。 */
  rightExtras?: React.ReactNode;
  /** 容器额外类名。 */
  className?: string;
  /** 滚动区域最多显示行数。超过自动滚动，未传则不限。 */
  maxRows?: number;
  /**
   * GitHub PR review 风格折叠：每个 change 行外保留 ±N 行 same 上下文，
   * 中间没被任何 change 邻居覆盖的 same 段折叠成「展开 K 行」按钮可点开。
   * 未传 / `0` / 没有任何 same 行可折叠时全展开。
   * 仅在拿到完整文件（含未改动上下文）时有意义，调用方在 detail+expanded 时传 3。
   */
  collapseContext?: number;
  /**
   * 行号起点：`beforeText` 实际对应原文件的第几行（1-based）。默认 1。
   * 流式 / 审批 / 非放大 detail 态拿到的 beforeText 只是 `old_string` 局部，
   * 调用方在原文中 indexOf 出起始行号传进来，行号槽才能显示真实位置。
   */
  baseLineBefore?: number;
  /** afterText 的起点行号。一般跟 baseLineBefore 一致（同一处替换），默认 1。 */
  baseLineAfter?: number;
}

/**
 * GitHub 风格的折叠视图：返回每个"展示单元"——可能是单行，也可能是一个可点开的折叠段。
 *
 * - 任何非 same 行（add / remove）和它周围 ±contextLines 的 same 行都直接展示
 * - 中间的纯 same 段折叠成一个 group，渲一个"展开 K 行原文"按钮
 * - `contextLines = 0` 表示不展示任何 same 上下文（仅 change 行）
 */
type DiffViewItem =
  | { kind: "row"; row: DiffRow; index: number }
  | { kind: "collapsed"; start: number; end: number; rows: DiffRow[] };

/**
 * 预计算每行 before/after 行号。避免在 React map 里 mutable 累加导致折叠段
 * 展开时闭包行号错乱。
 */
interface RenderRow {
  kind: "same" | "add" | "remove";
  textLeft: string;
  textRight: string;
  beforeNum: number | null;
  afterNum: number | null;
  diffIdx: number;
}

function buildRenderRows(
  diffRows: DiffRow[],
  baseBefore = 1,
  baseAfter = 1,
): RenderRow[] {
  let bn = baseBefore - 1;
  let an = baseAfter - 1;
  return diffRows.map((row, i) => {
    let beforeNum: number | null = null;
    let afterNum: number | null = null;
    if (row.kind === "same") {
      bn++;
      an++;
      beforeNum = bn;
      afterNum = an;
    } else if (row.kind === "remove") {
      bn++;
      beforeNum = bn;
    } else {
      an++;
      afterNum = an;
    }
    return {
      kind: row.kind,
      textLeft: row.left,
      textRight: row.right,
      beforeNum,
      afterNum,
      diffIdx: i,
    };
  });
}

function buildCollapsibleView(
  diffRows: DiffRow[],
  contextLines: number,
): DiffViewItem[] {
  if (contextLines < 0) contextLines = 0;
  const n = diffRows.length;
  const visible = new Array<boolean>(n).fill(false);
  for (let i = 0; i < n; i++) {
    if (diffRows[i].kind === "same") continue;
    const lo = Math.max(0, i - contextLines);
    const hi = Math.min(n - 1, i + contextLines);
    for (let k = lo; k <= hi; k++) visible[k] = true;
  }
  const result: DiffViewItem[] = [];
  let i = 0;
  while (i < n) {
    if (visible[i]) {
      result.push({ kind: "row", row: diffRows[i], index: i });
      i++;
    } else {
      const start = i;
      while (i < n && !visible[i]) i++;
      result.push({
        kind: "collapsed",
        start,
        end: i,
        rows: diffRows.slice(start, i),
      });
    }
  }
  return result;
}

/** 行高约 18px（text-[11px] leading-relaxed），统一用于 maxRows → maxHeight 换算 */
const DIFF_LINE_PX = 18;
const DIFF_VIEWPORT_PADDING_PX = 16;

/**
 * 流式态下让 diff 视口自动粘底：
 * - 新内容到达且当前粘底 → scrollTop = scrollHeight
 * - 用户主动向上滚 → 解除粘底，新内容不再强拉到底
 * - 用户重新滚回底部 → 自动恢复粘底
 *
 * 仅在 `streaming = true` 时生效；非流式态行为不变。
 */
function useStickyBottomScroll(streaming: boolean, signal: unknown) {
  const ref = useRef<HTMLDivElement | null>(null);
  const stickRef = useRef(true);

  const onScroll = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    stickRef.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 4;
  }, []);

  useEffect(() => {
    if (!streaming) return;
    const el = ref.current;
    if (!el || !stickRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [streaming, signal]);

  // streaming 由 false → true 时重置粘底意图（新一轮 edit 默认跟随）
  useEffect(() => {
    if (streaming) stickRef.current = true;
  }, [streaming]);

  return { ref, onScroll };
}

function maxRowsToStyle(
  maxRows: number | undefined,
): React.CSSProperties | undefined {
  if (!maxRows) return undefined;
  return {
    maxHeight: `${maxRows * DIFF_LINE_PX + DIFF_VIEWPORT_PADDING_PX}px`,
  };
}

/**
 * 纯渲染组件：架构 §4.13.9 三态共用入口。父组件只需提供 before/after 文本即可。
 *
 * 三态接入点：
 * - streaming（消息卡片）：MessageBubble Edit/Write 流式展开
 * - approval（审批弹窗）：PermissionApprovalPopup Edit/Write
 * - detail（消息卡片）：MessageBubble Edit/Write 已落盘后展开
 */
export function DiffViewer({
  beforeText,
  afterText,
  filePath,
  actionLabel,
  mode,
  onCycleMode,
  streaming,
  onClose,
  badge,
  expanded,
  onToggleExpanded,
  rightExtras,
  className,
  maxRows,
  collapseContext,
  baseLineBefore = 1,
  baseLineAfter = 1,
}: DiffViewerProps) {
  const diffRows = useMemo(
    () => computeDiff(beforeText.split("\n"), afterText.split("\n")),
    [beforeText, afterText],
  );
  const renderRows = useMemo(
    () => buildRenderRows(diffRows, baseLineBefore, baseLineAfter),
    [diffRows, baseLineBefore, baseLineAfter],
  );
  const items = useMemo<DiffViewItem[] | null>(() => {
    if (collapseContext && collapseContext > 0) {
      return buildCollapsibleView(diffRows, collapseContext);
    }
    return null;
  }, [diffRows, collapseContext]);
  // 折叠段展开 state：key = `${start}-${end}`，跨切换 inline↔split 保留
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());
  const toggleGroup = useCallback((key: string) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const addCount = diffRows.filter((r) => r.kind === "add").length;
  const removeCount = diffRows.filter((r) => r.kind === "remove").length;

  const isEmpty = !beforeText && !afterText;
  // 新建文件场景：old_string 为空、after 非空 → 没有"差异"语义，单栏纯 add 视图最直观
  const isCreate = !beforeText && !!afterText;
  const heightStyle = maxRowsToStyle(maxRows);

  return (
    <div className={cn("flex min-h-0 flex-col", className)}>
      <DiffHeader
        filePath={filePath ?? ""}
        actionLabel={actionLabel ?? ""}
        badge={badge}
        mode={mode}
        addCount={addCount}
        removeCount={removeCount}
        onCycleMode={onCycleMode}
        hideModeToggle={isCreate}
        expanded={expanded}
        onToggleExpanded={onToggleExpanded}
        onClose={onClose}
        rightExtras={rightExtras}
      />
      {isEmpty ? (
        <div className="flex-1 px-3 py-6 text-center text-[12px] text-muted-foreground">
          {streaming ? "等待参数…" : "文件为空（无变更）"}
        </div>
      ) : isCreate || mode === "inline" ? (
        // 新建文件没有差异语义，强走 inline 模式：全 add 行 + 绿底 + 行号，
        // 视觉跟 split 的右侧栏一致但去掉空白左栏
        <InlineDiff
          renderRows={renderRows}
          items={items}
          streaming={!!streaming}
          heightStyle={heightStyle}
          expandedGroups={expandedGroups}
          toggleGroup={toggleGroup}
        />
      ) : (
        <SplitDiff
          renderRows={renderRows}
          items={items}
          streaming={!!streaming}
          heightStyle={heightStyle}
          expandedGroups={expandedGroups}
          toggleGroup={toggleGroup}
        />
      )}
    </div>
  );
}

function DiffHeader({
  filePath,
  actionLabel,
  badge,
  mode,
  addCount,
  removeCount,
  onCycleMode,
  hideModeToggle,
  expanded,
  onToggleExpanded,
  onClose,
  rightExtras,
}: {
  filePath: string;
  actionLabel: string;
  badge?: string;
  mode: DiffMode;
  addCount: number;
  removeCount: number;
  onCycleMode: () => void;
  /** create / 单栏 add-only 场景下隐藏 split↔inline 切换按钮 */
  hideModeToggle?: boolean;
  expanded?: boolean;
  onToggleExpanded?: () => void;
  onClose?: () => void;
  rightExtras?: React.ReactNode;
}) {
  // 顶栏循环按钮：split ↔ inline；放大/缩小走独立按钮，不再混进 mode
  const modeLabel = mode === "split" ? "分栏" : "行内";
  const ModeIcon = mode === "split" ? Columns2 : Rows3;

  return (
    <div className="flex items-center justify-between gap-2 border-b border-border bg-muted/30 px-3 py-2 shrink-0">
      <div className="min-w-0 flex items-center gap-2">
        {filePath && (
          <PathHint path={filePath}>
            <span className="truncate text-[12px] font-medium font-mono">
              {pathLeaf(filePath)}
            </span>
          </PathHint>
        )}
        {actionLabel && (
          <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
            {actionLabel}
          </span>
        )}
        {badge && (
          <span className="shrink-0 rounded bg-amber-500/10 px-1.5 py-0.5 text-[10px] text-amber-600 dark:text-amber-400">
            {badge}
          </span>
        )}
        {/* GitHub PR 风格：+N -M 分开渲染，绿/红 token；无变更则隐藏 */}
        {(addCount > 0 || removeCount > 0) && (
          <span className="shrink-0 inline-flex items-center gap-1.5 font-mono text-[10px] tabular-nums">
            {addCount > 0 && (
              <span className="text-green-700 dark:text-green-400">
                +{addCount}
              </span>
            )}
            {removeCount > 0 && (
              <span className="text-rose-600 dark:text-rose-400">
                −{removeCount}
              </span>
            )}
          </span>
        )}
      </div>
      <div className="flex items-center gap-1">
        {!hideModeToggle && (
          <button
            type="button"
            onClick={onCycleMode}
            className="inline-flex items-center gap-1 rounded px-2 py-1 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground"
            title={`当前：${modeLabel}。点击切换 split ↔ inline。`}
          >
            <ModeIcon className="h-3.5 w-3.5" />
            <span>{modeLabel}</span>
          </button>
        )}
        {onToggleExpanded && (
          <button
            type="button"
            onClick={onToggleExpanded}
            className="grid h-6 w-6 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
            title={expanded ? "退出放大 (Esc)" : "放大查看"}
          >
            {expanded ? (
              <Minimize2 className="h-3.5 w-3.5" />
            ) : (
              <Maximize2 className="h-3.5 w-3.5" />
            )}
          </button>
        )}
        {rightExtras}
        {onClose && (
          <button
            type="button"
            onClick={onClose}
            className="grid h-6 w-6 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
            title="关闭 (Esc)"
          >
            <X className="h-4 w-4" />
          </button>
        )}
      </div>
    </div>
  );
}

/**
 * 单行渲染：受控行号槽 + +/- 符号 + 文本。
 *
 * - 行号槽：传 `undefined` 不渲该槽，传 `null` 渲空字符串（占位保持对齐）。
 *   inline 模式同时渲 before/after 两个槽；split 模式只渲一个。
 * - sign：VSCode 风格的 +/-/空格符号；颜色跟随符号变化。`undefined` 不渲符号槽。
 * - text 用 `whitespace-pre-wrap break-all`，长行换行从文本起点缩进。
 */
function DiffLine({
  beforeNum,
  afterNum,
  sign,
  text,
  rowClass,
  showCursor,
}: {
  beforeNum?: number | null;
  afterNum?: number | null;
  sign?: "+" | "-" | " ";
  text: string;
  rowClass?: string;
  showCursor?: boolean;
}) {
  const renderGutter = (n: number | null | undefined) =>
    n === undefined ? null : (
      <span className="select-none shrink-0 w-8 mr-1 text-right text-[9px] text-muted-foreground tabular-nums">
        {n ?? ""}
      </span>
    );

  return (
    <div className={cn("flex min-h-[1.4em] items-start", rowClass)}>
      {renderGutter(beforeNum)}
      {renderGutter(afterNum)}
      {sign !== undefined && (
        <span
          className={cn(
            "select-none shrink-0 w-3 mr-1 text-center text-[12px] font-mono leading-[1.4em]",
            sign === "+" && "text-green-600 dark:text-green-400",
            sign === "-" && "text-destructive",
          )}
        >
          {sign === " " ? "" : sign}
        </span>
      )}
      <span className="min-w-0 flex-1 whitespace-pre-wrap break-all">
        {text || " "}
        {showCursor && <StreamCursor />}
      </span>
    </div>
  );
}

/**
 * Unified inline diff（VSCode 风格）：单列、同时显示 before 和 after 行号、行首 +/-/空格符号。
 * - 删除行：左行号 + 空右行号 + `-`，红底
 * - 新增行：空左行号 + 右行号 + `+`，绿底
 * - 不变行：两行号都有 + 空格符
 */
interface DiffListProps {
  renderRows: RenderRow[];
  items: DiffViewItem[] | null;
  streaming: boolean;
  heightStyle?: React.CSSProperties;
  expandedGroups: Set<string>;
  toggleGroup: (key: string) => void;
}

/** 找出 renderRows 最后一个非 remove 行，做流式光标位置 */
function findLastNonRemoveIdx(renderRows: RenderRow[]): number {
  for (let i = renderRows.length - 1; i >= 0; i--) {
    if (renderRows[i].kind !== "remove") return i;
  }
  return -1;
}

function InlineDiff({
  renderRows,
  items,
  streaming,
  heightStyle,
  expandedGroups,
  toggleGroup,
}: DiffListProps) {
  const lastNonRemoveIdx = findLastNonRemoveIdx(renderRows);
  const { ref: scrollRef, onScroll } = useStickyBottomScroll(streaming, renderRows);

  const renderRow = (r: RenderRow) => {
    let sign: "+" | "-" | " ";
    let text: string;
    let rowClass: string;
    if (r.kind === "remove") {
      sign = "-";
      text = r.textLeft;
      rowClass = "bg-destructive/10 text-destructive px-1";
    } else if (r.kind === "add") {
      sign = "+";
      text = r.textRight;
      rowClass = "bg-green-500/10 text-green-700 dark:text-green-400 px-1";
    } else {
      sign = " ";
      text = r.textLeft;
      rowClass = "px-1";
    }
    return (
      <DiffLine
        key={r.diffIdx}
        beforeNum={r.beforeNum}
        afterNum={r.afterNum}
        sign={sign}
        text={text}
        rowClass={rowClass}
        showCursor={streaming && r.diffIdx === lastNonRemoveIdx}
      />
    );
  };

  return (
    <div
      ref={scrollRef}
      onScroll={onScroll}
      className="flex-1 overflow-auto font-mono text-[11px] leading-relaxed"
      style={heightStyle}
    >
      <div className="p-2">
        {items
          ? items.map((item) => {
              if (item.kind === "row") {
                return renderRow(renderRows[item.index]);
              }
              const groupKey = `${item.start}-${item.end}`;
              const open = expandedGroups.has(groupKey);
              if (open) {
                return (
                  <Fragment key={groupKey}>
                    {Array.from(
                      { length: item.end - item.start },
                      (_, k) => renderRows[item.start + k],
                    ).map((r) => renderRow(r))}
                  </Fragment>
                );
              }
              return (
                <CollapsedToggle
                  key={groupKey}
                  rowCount={item.end - item.start}
                  startLine={item.start + 1}
                  endLine={item.end}
                  onClick={() => toggleGroup(groupKey)}
                />
              );
            })
          : renderRows.map((r) => renderRow(r))}
      </div>
    </div>
  );
}

function SplitDiff({
  renderRows,
  items,
  streaming,
  heightStyle,
  expandedGroups,
  toggleGroup,
}: DiffListProps) {
  const lastNonRemoveIdx = findLastNonRemoveIdx(renderRows);
  const { ref: scrollRef, onScroll } = useStickyBottomScroll(streaming, renderRows);

  const PlaceholderCell = (
    <div className="min-h-[1.4em] flex-1 min-w-0 bg-muted/10" />
  );

  // row-first 渲染：每一行用一个 flex container 装左右两个 cell；flex stretch 自动等高
  const renderRow = (r: RenderRow) => {
    let leftCell: React.ReactNode;
    let rightCell: React.ReactNode;

    if (r.kind === "add") {
      leftCell = PlaceholderCell;
    } else {
      leftCell = (
        <div className="flex-1 min-w-0">
          <DiffLine
            beforeNum={r.beforeNum ?? undefined}
            sign={r.kind === "remove" ? "-" : " "}
            text={r.textLeft}
            rowClass={cn(
              "px-1",
              r.kind === "remove" && "bg-destructive/10 text-destructive",
            )}
          />
        </div>
      );
    }

    if (r.kind === "remove") {
      rightCell = PlaceholderCell;
    } else {
      rightCell = (
        <div className="flex-1 min-w-0">
          <DiffLine
            afterNum={r.afterNum ?? undefined}
            sign={r.kind === "add" ? "+" : " "}
            text={r.textRight}
            showCursor={streaming && r.diffIdx === lastNonRemoveIdx}
            rowClass={cn(
              "px-1",
              r.kind === "add" && "bg-green-500/10 text-green-700 dark:text-green-400",
            )}
          />
        </div>
      );
    }

    return (
      <div key={r.diffIdx} className="flex divide-x divide-border">
        {leftCell}
        {rightCell}
      </div>
    );
  };

  return (
    <div
      ref={scrollRef}
      onScroll={onScroll}
      className="flex-1 overflow-auto font-mono text-[11px] leading-relaxed"
      style={heightStyle}
    >
      {/* 双列 sticky header */}
      <div className="sticky top-0 z-10 flex divide-x divide-border border-b border-border bg-muted/40">
        <div className="flex-1 min-w-0 flex items-center gap-1 px-2 py-1 text-[10px] text-muted-foreground">
          <ArrowLeft className="h-3 w-3" />
          修改前
        </div>
        <div className="flex-1 min-w-0 flex items-center gap-1 px-2 py-1 text-[10px] text-muted-foreground">
          <ArrowRight className="h-3 w-3" />
          修改后
        </div>
      </div>

      <div className="p-2">
        {items
          ? items.map((item) => {
              if (item.kind === "row") {
                return renderRow(renderRows[item.index]);
              }
              const groupKey = `${item.start}-${item.end}`;
              const open = expandedGroups.has(groupKey);
              if (open) {
                return (
                  <Fragment key={groupKey}>
                    {Array.from(
                      { length: item.end - item.start },
                      (_, k) => renderRows[item.start + k],
                    ).map((r) => renderRow(r))}
                  </Fragment>
                );
              }
              return (
                <CollapsedToggle
                  key={groupKey}
                  rowCount={item.end - item.start}
                  startLine={item.start + 1}
                  endLine={item.end}
                  onClick={() => toggleGroup(groupKey)}
                />
              );
            })
          : renderRows.map((r) => renderRow(r))}
      </div>
    </div>
  );
}

/** GitHub PR review 风格"展开 N 行原文"按钮——跨整行渲染，点击展开该折叠段 */
function CollapsedToggle({
  rowCount,
  startLine,
  endLine,
  onClick,
}: {
  rowCount: number;
  startLine: number;
  endLine: number;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="my-px flex w-full items-center justify-center gap-1.5 rounded-sm border border-dashed border-border/60 bg-muted/30 px-2 py-0.5 text-[10px] text-muted-foreground transition-colors hover:border-border hover:bg-muted hover:text-foreground"
      title={`展开第 ${startLine}—${endLine} 行原文`}
    >
      <UnfoldVertical className="h-3 w-3" />
      <span>展开 {rowCount} 行原文（#{startLine}—#{endLine}）</span>
    </button>
  );
}

function StreamCursor() {
  return (
    <span
      aria-hidden="true"
      className="ml-0.5 inline-block h-3 w-[2px] -translate-y-[1px] animate-pulse rounded-sm bg-foreground/70 align-middle"
    />
  );
}

/* ───────────────── DiffPanel：detail 态浮层（基于 EditEntry / api.diffEdit）─────── */

interface DiffPanelProps {
  sessionId: string;
  entry: EditEntry;
  onClose: () => void;
}

export function DiffPanel({ sessionId, entry, onClose }: DiffPanelProps) {
  const [payload, setPayload] = useState<DiffPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [viewMode, setViewMode] = useState<DiffMode>("split");
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .diffEdit(sessionId, entry.snapshot_id)
      .then((p) => {
        if (!cancelled) setPayload(p);
      })
      .catch((e) => {
        if (!cancelled) setError(e?.message ?? String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, entry.snapshot_id]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (expanded) setExpanded(false);
        else onClose();
      }
    },
    [expanded, onClose],
  );

  useEffect(() => {
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [handleKeyDown]);

  const cycleMode = () =>
    setViewMode((prev) => (prev === "split" ? "inline" : "split"));
  const toggleExpanded = () => setExpanded((e) => !e);

  const actionLabel =
    entry.action === "create"
      ? "创建文件"
      : entry.action === "overwrite"
        ? "覆盖文件"
        : "修改文件";

  const content = (
    <>
      {loading && payload == null ? (
        <div className="flex-1 flex items-center justify-center text-sm text-muted-foreground py-16">
          加载差异…
        </div>
      ) : error ? (
        <div className="flex-1 flex items-center justify-center text-sm text-destructive py-16 px-4 text-center">
          {error}
        </div>
      ) : payload ? (
        <DiffViewer
          beforeText={payload.before_text}
          afterText={payload.after_text}
          filePath={payload.file_path}
          actionLabel={actionLabel}
          mode={viewMode}
          onCycleMode={cycleMode}
          maxRows={expanded ? undefined : 20}
          expanded={expanded}
          onToggleExpanded={toggleExpanded}
          collapseContext={3}
          onClose={onClose}
          className="min-h-0 flex-1"
        />
      ) : (
        <div className="flex-1 flex items-center justify-center text-sm text-muted-foreground py-16">
          无差异数据
        </div>
      )}
    </>
  );

  if (expanded) {
    return (
      <FullscreenPortal>
        <div
          className="pointer-events-auto absolute inset-0 bg-foreground/30"
          onClick={() => setExpanded(false)}
        />
        <div
          className="pointer-events-auto absolute inset-3 flex flex-col overflow-hidden border border-border bg-background shadow-2xl"
          onClick={(e) => e.stopPropagation()}
        >
          {content}
        </div>
      </FullscreenPortal>
    );
  }

  // 非阻塞浮层：portal 到 chat 区域；仅卡片本身接收点击，背景透传到聊天界面。
  return (
    <FullscreenPortal>
      <div className="pointer-events-auto absolute right-4 top-4 w-[85%] max-w-[960px] max-h-[calc(100%-2rem)] flex flex-col border border-border bg-background/95 shadow-xl backdrop-blur">
        {content}
      </div>
    </FullscreenPortal>
  );
}

function pathLeaf(filePath: string): string {
  const parts = filePath.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || filePath;
}
