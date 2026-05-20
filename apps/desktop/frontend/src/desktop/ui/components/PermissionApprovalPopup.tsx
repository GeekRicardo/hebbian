import { useEffect, useMemo, useState } from "react";
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
 * 例：`"ls -la"`                  → `{ sub: null, root: "ls" }`
 *
 * 切 token 后过滤掉 flag（`-` 开头），只保留位置参数；用户选 sub 前缀时记 `(root, sub)`，
 * 选 root 前缀时记单 token，两者都靠 HitlGate 的空白 token 边界匹配命中后续命令。
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
  return {
    sub: tokens.length >= 2 ? `${tokens[0]} ${tokens[1]}` : null,
    root: tokens[0],
  };
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
    <div className="px-4 pb-2">
      <div className="max-w-3xl mx-auto pr-[50px]">
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
              {bashPrefixes ? (
                <>
                  {segmentRoots.length >= 2 && (
                    <button
                      type="button"
                      onClick={() =>
                        send({
                          kind: "allow_and_remember",
                          pattern: segmentRoots[0],
                          extraPatterns: segmentRoots.slice(1),
                          scope: "session",
                        })
                      }
                      disabled={submitting}
                      className={cn(
                        "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                        "bg-primary/10 text-primary hover:bg-primary/20 disabled:opacity-50"
                      )}
                      title={`本会话内放行 compound 命令的全部 ${segmentRoots.length} 段：${segmentRoots.join(", ")}`}
                    >
                      <Check className="w-3.5 h-3.5" />
                      整条都允许（{segmentRoots.length} 段）
                    </button>
                  )}
                  {bashPrefixes.sub && (
                    <button
                      type="button"
                      onClick={() =>
                        send({
                          kind: "allow_and_remember",
                          pattern: bashPrefixes.sub,
                          scope: "session",
                        })
                      }
                      disabled={submitting}
                      className={cn(
                        "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                        "bg-muted hover:bg-muted/80 disabled:opacity-50"
                      )}
                      title={`本会话内 ${bashPrefixes.sub}* 都不再询问`}
                    >
                      当前对话{" "}
                      <code className="font-mono text-[12px]">
                        {bashPrefixes.sub} *
                      </code>
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() =>
                      send({
                        kind: "allow_and_remember",
                        pattern: bashPrefixes.root,
                        scope: "session",
                      })
                    }
                    disabled={submitting}
                    className={cn(
                      "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                      "bg-muted hover:bg-muted/80 disabled:opacity-50"
                    )}
                    title={`本会话内所有 ${bashPrefixes.root}* 都不再询问（含子命令）`}
                  >
                    当前对话{" "}
                    <code className="font-mono text-[12px]">
                      {bashPrefixes.root} *
                    </code>
                  </button>
                  <button
                    type="button"
                    onClick={() =>
                      send({
                        kind: "allow_and_remember",
                        pattern: bashPrefixes.root,
                        scope: "project",
                      })
                    }
                    disabled={submitting}
                    className={cn(
                      "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                      "bg-muted hover:bg-muted/80 disabled:opacity-50"
                    )}
                    title={`当前项目（workdir）所有对话放行 ${bashPrefixes.root}*`}
                  >
                    <FolderTree className="w-3.5 h-3.5" />
                    本项目{" "}
                    <code className="font-mono text-[12px]">
                      {bashPrefixes.root} *
                    </code>
                  </button>
                  <button
                    type="button"
                    onClick={() =>
                      send({
                        kind: "allow_and_remember",
                        pattern: bashPrefixes.root,
                        scope: "global",
                      })
                    }
                    disabled={submitting}
                    className={cn(
                      "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                      "bg-muted hover:bg-muted/80 disabled:opacity-50"
                    )}
                    title={`写入 ~/.hebbian/permissions.json，全局放行 ${bashPrefixes.root}*`}
                  >
                    <Globe className="w-3.5 h-3.5" />
                    始终允许{" "}
                    <code className="font-mono text-[12px]">
                      {bashPrefixes.root} *
                    </code>
                  </button>
                </>
              ) : pending.toolName !== "Bash" ? (
                <>
                  <button
                    type="button"
                    onClick={() =>
                      send({ kind: "allow_and_remember", scope: "session" })
                    }
                    disabled={submitting}
                    className={cn(
                      "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                      "bg-muted hover:bg-muted/80 disabled:opacity-50"
                    )}
                    title="本会话内不再询问此工具"
                  >
                    当前对话不再询问
                  </button>
                  <button
                    type="button"
                    onClick={() =>
                      send({ kind: "allow_and_remember", scope: "project" })
                    }
                    disabled={submitting}
                    className={cn(
                      "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                      "bg-muted hover:bg-muted/80 disabled:opacity-50"
                    )}
                    title="当前项目（workdir）所有对话不再询问此工具"
                  >
                    <FolderTree className="w-3.5 h-3.5" />
                    本项目不再询问
                  </button>
                  <button
                    type="button"
                    onClick={() =>
                      send({ kind: "allow_and_remember", scope: "global" })
                    }
                    disabled={submitting}
                    className={cn(
                      "h-8 px-3 rounded-md text-sm inline-flex items-center gap-1.5 transition-colors",
                      "bg-muted hover:bg-muted/80 disabled:opacity-50"
                    )}
                    title="写入 ~/.hebbian/permissions.json，所有对话生效"
                  >
                    <Globe className="w-3.5 h-3.5" />
                    始终允许
                  </button>
                </>
              ) : null}
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
