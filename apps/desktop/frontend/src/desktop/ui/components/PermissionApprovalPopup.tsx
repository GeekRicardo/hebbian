import { useEffect, useMemo, useState, type ReactNode } from "react";
import {
  Check,
  FolderOpen,
  FolderTree,
  Globe,
  Maximize2,
  MessageSquareWarning,
  Minimize2,
  Shield,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";
import {
  DiffViewer,
  FullscreenPortal,
  type DiffMode,
} from "@/desktop/ui/components/DiffPanel";
import {
  diffSidesFromArgs,
  inferDiffAction,
  parsePartialEditArgs,
  type PartialEditArgs,
} from "@/desktop/ui/lib/parsePartialEditArgs";

/**
 * 把 BashTool 推送的命令指纹切成"前缀按钮"。
 *
 * 例：`"git status -uno README"` → `{ sub: "git status", root: "git" }`
 * 例：`"touch /tmp/x.txt"`        → `{ sub: null, root: "touch" }` (路径参数不算"子命令")
 * 例：`"ls -la"`                  → `{ sub: null, root: "ls" }`
 *
 * 切 token 后过滤掉 flag（`-` 开头），只保留位置参数；用户选 sub 前缀时记 `(root, sub)`，
 * 选 root 前缀时记单 token，两者都靠 HitlGate 的空白 token 边界匹配命中后续命令。
 *
 * 路径参数过滤：第二个 token 看起来是路径（`/` / `~` / `./` / `../` 开头）时 sub = null
 * ——避免用户被误导："点 sub" 等同于写一条只匹配该确切文件的规则，下次同前缀
 * 不同文件名不命中。路径参数命令应当用 root（含 `*`）或 PathAccess 审批走分级路径。
 */
function parseBashPrefixes(
  fingerprint: string | null | undefined
): { sub: string | null; root: string } | null {
  if (!fingerprint) return null;
  const tokens = fingerprint
    .trim()
    .split(/\s+/)
    .filter((t) => t && !t.startsWith("-"));
  if (tokens.length === 0) return null;
  const isPathLike = (t: string) =>
    t.startsWith("/") || t.startsWith("~") || t.startsWith("./") || t.startsWith("../");
  const sub =
    tokens.length >= 2 && !isPathLike(tokens[1])
      ? `${tokens[0]} ${tokens[1]}`
      : null;
  return { sub, root: tokens[0] };
}

const RISK_STYLE: Record<
  "low" | "medium" | "high" | "critical",
  { label: string; className: string }
> = {
  low: {
    label: "低风险",
    className: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
  },
  medium: {
    label: "中风险",
    className: "bg-amber-500/10 text-amber-600 dark:text-amber-400",
  },
  high: {
    label: "高风险",
    className: "bg-orange-500/15 text-orange-600 dark:text-orange-400",
  },
  critical: {
    label: "高危",
    className: "bg-red-500/15 text-red-600 dark:text-red-400",
  },
};

/**
 * HITL 审批弹窗。挂在 ChatInput 上方。
 *
 * 两类审批：
 * - tool_call：destructive 工具（Bash/Write）请求执行
 * - path_access：工具想访问 workspace 之外的路径，
 *   按钮分 「仅此次 / 加入本对话 / 加入全局」
 */
export function PermissionApprovalPopup() {
  const pending = useStore((s) => s.pendingApproval);
  const resolveApproval = useStore((s) => s.resolveApproval);
  const resolvePathAccess = useStore((s) => s.resolvePathAccess);
  const [feedbackOpen, setFeedbackOpen] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [submitting, setSubmitting] = useState(false);

  // ⚠️ hook 必须在组件顶层、所有提前 return 之前调用——把 useMemo 放在 `if (!pending)`
  // 之后会让 hook 调用次数随 pending 变化，React 抛"Rendered more hooks than during
  // the previous render"，弹窗会被 unmount，用户根本看不到按钮 → 看起来"卡住等返回"。
  const bashPrefixes = useMemo(
    () =>
      pending && pending.toolName === "Bash"
        ? parseBashPrefixes(pending.fingerprint)
        : null,
    [pending?.toolName, pending?.fingerprint]
  );
  // Bash compound 命令的全部段 root 列表（架构 §4.4.2）。
  // 例：`cd /tmp && touch foo` → ["cd", "touch"]，去重保序。
  // segmentRoots.length >= 2 时弹窗展示"整条命令一次性允许"按钮，
  // 一次写入多条规则避免段级判定"全段 allow"条件单点放行不够。
  const segmentRoots = useMemo(() => {
    if (!pending || pending.toolName !== "Bash") return [] as string[];
    const segs = pending.commandSegments ?? [];
    const roots: string[] = [];
    for (const fp of segs) {
      const parsed = parseBashPrefixes(fp);
      if (parsed && !roots.includes(parsed.root)) {
        roots.push(parsed.root);
      }
    }
    return roots;
  }, [pending?.toolName, pending?.commandSegments]);

  // 记忆选项 list：用户在 popup 里勾选要记的 pattern。
  // - Bash：sub（如可拆二级子命令）/ 各段 root（含 compound）；默认全选段 root
  // - Edit / Write：从 input.file_path 切出「精确文件 / 父目录」两档路径前缀
  //   后端 build_rule 对非 Bash 工具把 pattern 当 path_prefix（FilePath matcher），
  //   下次同前缀的路径自动放行；这是用户的"审批路径"心理模型
  // - 其它工具：仅一档「工具 X」（pattern=null = Any matcher）兜底
  const memoryOptions: MemoryOption[] = useMemo(() => {
    if (!pending) return [];
    const opts: MemoryOption[] = [];
    if (pending.toolName === "Bash" && bashPrefixes) {
      if (bashPrefixes.sub && bashPrefixes.sub !== bashPrefixes.root) {
        opts.push({
          key: `sub:${bashPrefixes.sub}`,
          pattern: bashPrefixes.sub,
          label: bashPrefixes.sub,
          hint: "精确二级子命令",
          defaultChecked: false,
        });
      }
      const roots = segmentRoots.length > 0 ? segmentRoots : [bashPrefixes.root];
      for (const r of roots) {
        opts.push({
          key: `root:${r}`,
          pattern: r,
          label: `${r} *`,
          hint: roots.length >= 2 ? "compound 段" : "该根命令的所有子命令",
          defaultChecked: true,
        });
      }
    } else if (
      (pending.toolName === "Edit" || pending.toolName === "Write") &&
      typeof (pending.input as { file_path?: unknown })?.file_path === "string"
    ) {
      const filePath = (pending.input as { file_path: string }).file_path;
      // 精确文件路径：仅放行同一个文件
      opts.push({
        key: `path:${filePath}`,
        pattern: filePath,
        label: filePath,
        hint: "精确文件",
        defaultChecked: false,
      });
      // 父目录前缀：放行该目录下所有同工具操作
      const slash = filePath.lastIndexOf("/");
      if (slash > 0) {
        const parentDir = filePath.slice(0, slash + 1);
        opts.push({
          key: `dir:${parentDir}`,
          pattern: parentDir,
          label: `${parentDir}*`,
          hint: "整个目录",
          defaultChecked: true,
        });
      }
    } else if (pending.toolName !== "Bash") {
      opts.push({
        key: `tool:${pending.toolName}`,
        pattern: null,
        label: `工具 ${pending.toolName}`,
        hint: "工具名级允许（粒度较粗）",
        defaultChecked: true,
      });
    }
    return opts;
  }, [pending?.toolName, pending?.input, bashPrefixes, segmentRoots]);

  if (!pending) return null;

  const isPathAccess = pending.kind === "path_access";

  async function send(decision: Parameters<typeof resolveApproval>[0]) {
    setSubmitting(true);
    try {
      await resolveApproval(decision);
      setFeedbackOpen(false);
      setFeedback("");
    } catch (e: any) {
      toast.error(e?.message ?? "审批回应失败");
    } finally {
      setSubmitting(false);
    }
  }

  async function sendPath(
    scope: "once" | "this_session" | "this_project" | "global"
  ) {
    setSubmitting(true);
    try {
      await resolvePathAccess(scope);
    } catch (e: any) {
      toast.error(e?.message ?? "审批回应失败");
    } finally {
      setSubmitting(false);
    }
  }

  const risk = RISK_STYLE[pending.risk] ?? RISK_STYLE.medium;
  const inputPreview =
    typeof pending.input === "string"
      ? pending.input
      : pending.input
        ? JSON.stringify(pending.input, null, 2)
        : "";

  // 架构 §4.13.9 approval 态：Edit/Write 渲 DiffViewer，不再原始 JSON。
  // 入参 input 此时已是完整 JSON（PermissionRequested 携带）；统一走容错解析
  // 走流式同一个入口，避免两份代码。
  const isEditLike = !isPathAccess && (pending.toolName === "Edit" || pending.toolName === "Write");
  const editArgs: PartialEditArgs | null = isEditLike
    ? parsePartialEditArgs(
        typeof pending.input === "string"
          ? pending.input
          : pending.input
            ? JSON.stringify(pending.input)
            : "",
      )
    : null;

  return (
    // max-w + mx-auto 提到最外层：原来外层 `px-4 pb-2` 没限宽，左右两侧虽透明但
    // 仍是 div 节点，会拦截鼠标事件挡住下面的 chat 消息。提到外层后只有居中那块
    // max-w-3xl 区域是 div，左右空白真的"不存在"，点击穿透到下面。
    <div className="max-w-3xl mx-auto px-4 pb-2">
      <div className="pr-[50px]">
        <div
          className={cn(
            "w-full rounded-lg border border-border bg-card text-card-foreground shadow-lg overflow-hidden",
            "animate-in fade-in slide-in-from-bottom-2 duration-150"
          )}
        >
        {/* 头部 */}
        <div className="flex items-center gap-2 px-3 py-2 border-b border-border bg-muted/40">
          <Shield className="w-4 h-4 text-primary shrink-0" />
          <span className="text-sm font-medium flex-1 truncate">
            {isPathAccess ? (
              <>
                <code className="font-mono">{pending.toolName}</code> 想访问越界路径
              </>
            ) : (
              <>
                AI 请求执行 <code className="font-mono">{pending.toolName}</code>
              </>
            )}
          </span>
          <span
            className={cn(
              "text-[11px] px-1.5 py-0.5 rounded font-medium",
              risk.className
            )}
          >
            {risk.label}
          </span>
        </div>

        {/* 路径列表（PathAccess） */}
        {isPathAccess && pending.paths && pending.paths.length > 0 && (
          <ul className="text-[12px] px-3 py-2 max-h-32 overflow-auto bg-background/50 font-mono space-y-0.5">
            {pending.paths.map((p) => (
              <li key={p} className="break-all text-muted-foreground/90">
                · {p}
              </li>
            ))}
          </ul>
        )}

        {/* 工具输入参数预览（tool_call） */}
        {isEditLike && editArgs ? (
          <ApprovalEditDiff toolName={pending.toolName} args={editArgs} />
        ) : (
          !isPathAccess &&
          inputPreview &&
          inputPreview !== "null" && (
            <pre className="text-[11px] text-muted-foreground/90 px-3 py-2 max-h-32 overflow-auto bg-background/50 font-mono whitespace-pre-wrap break-all">
              {inputPreview.slice(0, 800)}
              {inputPreview.length > 800 ? "…" : ""}
            </pre>
          )
        )}

        {/* 反馈输入框（按需展开，仅 tool_call 有） */}
        {feedbackOpen && (
          <div className="px-3 py-2 border-t border-border">
            <textarea
              value={feedback}
              onChange={(e) => setFeedback(e.target.value)}
              placeholder="告诉Hebbian如何改进"
              rows={2}
              className="w-full resize-none rounded-md border border-input bg-background px-2 py-1.5 text-sm outline-none focus:ring-2 focus:ring-ring"
              autoFocus
            />
          </div>
        )}

        {/* 二级区：「记忆 pattern 多选 list + scope 按钮」（架构 §4.4.2 段级判定）。
            Bash 列出 sub / root / compound 各段 root；其它工具只暴露工具名级。
            用户勾选要记的 pattern（默认全选）→ 点 scope 按钮一次性写多条规则。
            路径审批走主按钮区的 4 档，不进二级区。 */}
        {!isPathAccess && !feedbackOpen && memoryOptions.length > 0 && (
          <MemoryRecallPanel
            options={memoryOptions}
            disabled={submitting}
            onApply={(picked, scope) => {
              // picked 至少 1 条：第一条做 pattern，其余进 extra_patterns。
              // 工具名级（pattern=null）走 picked[0].pattern === null 单独分支。
              const first = picked[0];
              if (first.pattern === null) {
                send({ kind: "allow_and_remember", scope });
                return;
              }
              const patterns = picked
                .filter((p) => p.pattern !== null)
                .map((p) => p.pattern!);
              send({
                kind: "allow_and_remember",
                pattern: patterns[0],
                extraPatterns: patterns.slice(1),
                scope,
              });
            }}
          />
        )}

        {/* 按钮组 */}
        <div className="flex flex-wrap items-center gap-1.5 px-2 py-2 border-t border-border bg-background/60">
          {isPathAccess ? (
            <>
              <button
                type="button"
                onClick={() => sendPath("once")}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm font-medium inline-flex items-center gap-1.5 transition-colors",
                  "bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
                )}
              >
                <Check className="w-3.5 h-3.5" />
                仅此次
              </button>
              <button
                type="button"
                onClick={() => sendPath("this_session")}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                  "bg-muted hover:bg-muted/80 disabled:opacity-50"
                )}
                title="加入本对话的允许路径"
              >
                <FolderOpen className="w-3.5 h-3.5" />
                加入本对话
              </button>
              <button
                type="button"
                onClick={() => sendPath("this_project")}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                  "bg-muted hover:bg-muted/80 disabled:opacity-50"
                )}
                title="加入本项目允许路径（当前 workdir 下任何对话生效）"
              >
                <FolderTree className="w-3.5 h-3.5" />
                加入本项目
              </button>
              <button
                type="button"
                onClick={() => sendPath("global")}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                  "bg-muted hover:bg-muted/80 disabled:opacity-50"
                )}
                title="加入全局允许路径（所有对话生效）"
              >
                <Globe className="w-3.5 h-3.5" />
                加入全局
              </button>
              <div className="flex-1" />
              <button
                type="button"
                onClick={() => send({ kind: "deny" })}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm font-medium inline-flex items-center gap-1.5 transition-colors",
                  "bg-destructive/10 text-destructive hover:bg-destructive/20 disabled:opacity-50"
                )}
              >
                <X className="w-3.5 h-3.5" />
                拒绝
              </button>
            </>
          ) : !feedbackOpen ? (
            <>
              <button
                type="button"
                onClick={() => send({ kind: "allow_once" })}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm font-medium inline-flex items-center gap-1.5 transition-colors",
                  "bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
                )}
              >
                <Check className="w-3.5 h-3.5" />
                允许此次
              </button>
              {/*
                Bash 工具有命令指纹时只展示前缀按钮，避免工具名级"总是允许"
                让后续 `rm -rf /` 也免审批。其他工具退回工具名级"总是允许"。
              */}
              <div className="flex-1" />
              <button
                type="button"
                onClick={() => setFeedbackOpen(true)}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                  "text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"
                )}
              >
                <MessageSquareWarning className="w-3.5 h-3.5" />
                拒绝并反馈
              </button>
              <button
                type="button"
                onClick={() => send({ kind: "deny" })}
                disabled={submitting}
                className={cn(
                  "h-8 px-3 rounded-md text-sm font-medium inline-flex items-center gap-1.5 transition-colors",
                  "bg-destructive/10 text-destructive hover:bg-destructive/20 disabled:opacity-50"
                )}
              >
                <X className="w-3.5 h-3.5" />
                拒绝
              </button>
            </>
          ) : (
            <>
              <button
                type="button"
                onClick={() => {
                  setFeedbackOpen(false);
                  setFeedback("");
                }}
                disabled={submitting}
                className="h-8 px-3 rounded-md text-sm text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"
              >
                取消
              </button>
              <div className="flex-1" />
              <button
                type="button"
                onClick={() =>
                  send({ kind: "deny_with_feedback", feedback: feedback.trim() })
                }
                disabled={submitting || !feedback.trim()}
                className="h-8 px-3 rounded-md text-sm font-medium bg-destructive text-destructive-foreground hover:bg-destructive/90 disabled:opacity-50"
              >
                提交反馈
              </button>
            </>
          )}
        </div>
        </div>
      </div>
    </div>
  );
}

/**
 * 架构 §4.13.9 approval 态：Edit/Write 在审批弹窗里用 DiffViewer 替代原始 JSON。
 *
 * before/after 直接来自 args（与 streaming 态同源）：
 * - Edit:  before = old_string,  after = new_string
 * - Write: before = "",          after = content
 *
 * 顶栏右上 [放大 / inline↔split / 关闭] 与详情卡片保持一致。
 */
function ApprovalEditDiff({
  toolName,
  args,
}: {
  toolName: string;
  args: PartialEditArgs;
}) {
  const [viewMode, setViewMode] = useState<DiffMode>("split");
  const [expanded, setExpanded] = useState(false);
  const { beforeText, afterText } = diffSidesFromArgs(toolName, args);
  const action = inferDiffAction(toolName, args);
  const actionLabel =
    action === "create" ? "创建文件" : action === "overwrite" ? "覆盖文件" : "修改文件";

  // 放大态接管 Esc，避免直接关掉审批弹窗
  useEffect(() => {
    if (!expanded) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setExpanded(false);
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [expanded]);

  const cycleMode = () => setViewMode((m) => (m === "split" ? "inline" : "split"));
  const toggleExpanded = () => setExpanded((e) => !e);

  if (expanded) {
    return (
      <FullscreenPortal>
        <div
          className="pointer-events-auto absolute inset-0 bg-foreground/30"
          onClick={() => setExpanded(false)}
        />
        <div
          className="pointer-events-auto absolute inset-3 flex flex-col overflow-hidden rounded-xl border border-border bg-background shadow-2xl"
          onClick={(e) => e.stopPropagation()}
        >
          <DiffViewer
            beforeText={beforeText}
            afterText={afterText}
            filePath={args.file_path ?? ""}
            actionLabel={actionLabel}
            badge="待审批"
            mode={viewMode}
            onCycleMode={cycleMode}
            expanded={expanded}
            onToggleExpanded={toggleExpanded}
            onClose={() => setExpanded(false)}
            className="min-h-0 flex-1"
          />
        </div>
      </FullscreenPortal>
    );
  }

  return (
    <div className="border-t border-border">
      <DiffViewer
        beforeText={beforeText}
        afterText={afterText}
        filePath={args.file_path ?? ""}
        actionLabel={actionLabel}
        badge="待审批"
        mode={viewMode}
        onCycleMode={cycleMode}
        expanded={expanded}
        onToggleExpanded={toggleExpanded}
        maxRows={20}
      />
    </div>
  );
}

/**
 * 一项可勾选的记忆 pattern。
 * - `pattern: string` → Bash 命令前缀（写 `Bash{commandPrefix}` 规则）
 * - `pattern: null` → 工具名级 wildcard（写 `Any` matcher 规则）
 */
type MemoryOption = {
  key: string;
  pattern: string | null;
  label: string;
  hint: string;
  defaultChecked: boolean;
};

/**
 * 记忆面板：多选 pattern checkbox list + 3 scope 一键写入按钮。
 *
 * 用户先勾选要"记住"的 pattern（默认全选段 root），再点对应 scope 按钮，
 * 一次性把 N 条勾选转成 N 条 PermissionRule 落盘。比"每行 × 3 chip"信息密度更高，
 * 且 compound 命令的 segment roots 可逐段细调。
 */
function MemoryRecallPanel({
  options,
  disabled,
  onApply,
}: {
  options: MemoryOption[];
  disabled?: boolean;
  onApply: (
    picked: MemoryOption[],
    scope: "session" | "project" | "global",
  ) => void;
}) {
  const [checked, setChecked] = useState<Record<string, boolean>>(() => {
    const m: Record<string, boolean> = {};
    for (const o of options) m[o.key] = o.defaultChecked;
    return m;
  });
  // pending key set 变更时（不同审批弹窗复用同一组件实例）重置默认勾选
  useEffect(() => {
    const m: Record<string, boolean> = {};
    for (const o of options) m[o.key] = o.defaultChecked;
    setChecked(m);
  }, [options.map((o) => o.key).join("|")]);

  const allChecked = options.length > 0 && options.every((o) => checked[o.key]);
  const noneChecked = options.every((o) => !checked[o.key]);
  const pickedCount = options.filter((o) => checked[o.key]).length;

  const toggleAll = () => {
    const next = !allChecked;
    setChecked(Object.fromEntries(options.map((o) => [o.key, next])));
  };
  const toggle = (key: string) =>
    setChecked((prev) => ({ ...prev, [key]: !prev[key] }));

  const apply = (scope: "session" | "project" | "global") => {
    const picked = options.filter((o) => checked[o.key]);
    if (picked.length === 0) return;
    onApply(picked, scope);
  };

  const scopeBtn = (
    scope: "session" | "project" | "global",
    text: string,
    icon: ReactNode,
    hint: string,
  ) => (
    <button
      type="button"
      data-testid={`memory-scope-${scope}`}
      onClick={() => apply(scope)}
      disabled={disabled || noneChecked}
      title={hint}
      className={cn(
        "h-7 px-2.5 rounded-md text-[12px] inline-flex items-center gap-1.5 transition-colors",
        "bg-muted hover:bg-primary/15 hover:text-primary disabled:opacity-40 disabled:cursor-not-allowed",
      )}
    >
      {icon}
      {text}
    </button>
  );

  return (
    <div
      className="flex flex-col gap-1 px-3 py-2 border-t border-border bg-background/30"
      data-testid="memory-recall-panel"
    >
      <div className="flex items-center justify-between text-[11px] text-muted-foreground/80 mb-1">
        <span>勾选要一起记忆的前缀，再点应用范围：</span>
        <span className="font-mono">{pickedCount}/{options.length}</span>
      </div>

      {/* 全选 */}
      {options.length > 1 && (
        <label
          className="flex items-center gap-2 text-[12px] cursor-pointer pb-1 mb-1 border-b border-border/40 select-none"
          data-testid="memory-toggle-all"
        >
          <input
            type="checkbox"
            checked={allChecked}
            onChange={toggleAll}
            className="accent-primary"
            disabled={disabled}
          />
          <span className="font-medium">
            {allChecked ? "取消全选" : "全选"}
          </span>
        </label>
      )}

      {/* pattern checkboxes */}
      <div className="flex flex-col gap-0.5 max-h-44 overflow-auto">
        {options.map((opt) => (
          <label
            key={opt.key}
            className="flex items-center gap-2 text-[12px] cursor-pointer hover:bg-muted/40 px-1.5 py-1 rounded select-none"
            data-testid={`memory-option-${opt.key}`}
          >
            <input
              type="checkbox"
              checked={!!checked[opt.key]}
              onChange={() => toggle(opt.key)}
              className="accent-primary"
              disabled={disabled}
            />
            <code className="font-mono text-[12px] truncate flex-1 min-w-0">
              {opt.label}
            </code>
            <span className="text-muted-foreground text-[10px] shrink-0">
              {opt.hint}
            </span>
          </label>
        ))}
      </div>

      {/* scope 按钮：把当前勾选项写成规则 */}
      <div className="flex items-center gap-1.5 pt-1.5 mt-1 border-t border-border/40">
        <span className="text-[11px] text-muted-foreground mr-1">
          应用到：
        </span>
        {scopeBtn(
          "session",
          "本对话",
          <FolderOpen className="w-3.5 h-3.5" />,
          "写到当前对话的 in-memory 规则",
        )}
        {scopeBtn(
          "project",
          "本项目",
          <FolderTree className="w-3.5 h-3.5" />,
          "写到 ~/.hebbian/permissions.json，限当前 workdir",
        )}
        {scopeBtn(
          "global",
          "全局",
          <Globe className="w-3.5 h-3.5" />,
          "写到 ~/.hebbian/permissions.json，所有对话生效",
        )}
      </div>
    </div>
  );
}
