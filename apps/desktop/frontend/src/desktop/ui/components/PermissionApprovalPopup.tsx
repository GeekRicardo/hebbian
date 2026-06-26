import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  Check,
  FolderOpen,
  FolderTree,
  Globe,
  MessageSquareWarning,
  Shield,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/desktop/bridge/tauri";
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
import {
  lineOfOldString,
  useOriginalFileText,
} from "@/desktop/ui/lib/useDiffBaseLine";
import type { ApprovalSegmentStatus } from "@/desktop/ui/types";

/** 段级白名单状态的展示样式（架构 §4.4.2.3）。 */
const SEG_META: Record<
  ApprovalSegmentStatus,
  { label: string; badge: string; text: string }
> = {
  readonly: {
    label: "只读",
    badge: "bg-muted text-muted-foreground",
    text: "text-muted-foreground/70",
  },
  whitelisted: {
    label: "✓ 已允许",
    badge: "bg-green-500/15 text-green-600 dark:text-green-500",
    text: "text-muted-foreground line-through decoration-green-600/40",
  },
  unmemorable: {
    label: "危险·不可记",
    badge: "bg-destructive/15 text-destructive",
    text: "text-destructive",
  },
  needs_approval: {
    label: "待批",
    badge: "bg-amber-500/15 text-amber-600 dark:text-amber-500",
    text: "",
  },
};

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

type JudgeSegment = { index: string; fingerprint: string; detail: string };

/**
 * 把 AutoMode judge 的单行 reason 拆成「结论 + 逐段影响」。
 *
 * judge prompt 强制单行输出（上游 regex 解析多行会判失败），格式固定为
 * `<一句话结论>. Segments: [1] <指纹>: <影响>；[2] ...`。这里按 `[N]` 边界切段，
 * 给人看的换行/层级全在前端做。没有 `Segments:` 段（如 DENY 的单句）时整段当结论。
 */
function parseAutoJudgeReason(raw: string): {
  headline: string;
  segments: JudgeSegment[];
} {
  const text = raw.trim();
  const marker = text.match(/\bSegments\s*[:：]\s*/i);
  if (!marker || marker.index === undefined) {
    return { headline: text, segments: [] };
  }
  const headline = text.slice(0, marker.index).trim();
  const body = text.slice(marker.index + marker[0].length);
  const segments: JudgeSegment[] = [];
  const re = /\[(\d+)\]\s*([\s\S]*?)(?=\s*[;；]\s*\[\d+\]|$)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(body)) !== null) {
    const chunk = m[2].trim().replace(/[;；]\s*$/, "").trim();
    if (!chunk) continue;
    // 指纹与影响以首个「：」或「: 」分隔——避免命中 `https://` 这类无空格冒号。
    const sep = chunk.search(/：|:\s/);
    const fingerprint = sep >= 0 ? chunk.slice(0, sep).trim() : chunk;
    const detail = sep >= 0 ? chunk.slice(sep + 1).trim() : "";
    segments.push({ index: m[1], fingerprint, detail });
  }
  if (segments.length === 0) return { headline: text, segments: [] };
  return { headline, segments };
}

/** AutoMode judge 危险原因展示：结论一行、逐段缩进列表，整体限高可滚动。 */
function AutoJudgeReason({ reason }: { reason: string }) {
  const { headline, segments } = useMemo(
    () => parseAutoJudgeReason(reason),
    [reason]
  );
  return (
    <div className="border-t border-border/60 text-[12px] text-amber-700 dark:text-amber-400">
      <div className="px-3 py-2 max-h-48 overflow-y-auto space-y-1.5">
        <div className="flex items-start gap-1.5">
          <MessageSquareWarning className="w-3.5 h-3.5 shrink-0 mt-0.5" />
          <span className="font-medium leading-relaxed">{headline}</span>
        </div>
        {segments.length > 0 && (
          <ul className="pl-5 space-y-1">
            {segments.map((seg) => (
              <li key={seg.index} className="flex gap-1.5 leading-relaxed">
                <span className="shrink-0 font-mono text-amber-600/70 dark:text-amber-500/70">
                  {seg.index}.
                </span>
                <span className="min-w-0">
                  <code className="font-mono break-all text-amber-800 dark:text-amber-300">
                    {seg.fingerprint}
                  </code>
                  {seg.detail && (
                    <span className="text-amber-700/90 dark:text-amber-400/90">
                      {" — "}
                      {seg.detail}
                    </span>
                  )}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

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
  // hover 某个 MemoryOption 时，在 Bash 命令预览里把对应 pattern 段高亮（cross-link）
  const [hoveredPattern, setHoveredPattern] = useState<string | null>(null);

  // ⚠️ hook 必须在组件顶层、所有提前 return 之前调用——把 useMemo 放在 `if (!pending)`
  // 之后会让 hook 调用次数随 pending 变化，React 抛"Rendered more hooks than during
  // the previous render"，弹窗会被 unmount，用户根本看不到按钮 → 看起来"卡住等返回"。
  // 记忆选项 list：用户在 popup 里勾选要记的 pattern。
  // - Bash：后端已把 command_segments 过滤成「会写且可记忆」段（只读命令、rm 等
  //   不可记忆命令都不在内）。逐段切前缀——dispatcher（git/cargo/kubectl…）默认勾
  //   「精确子命令」（git commit），root（git *）作为放宽档默认不勾；unitary
  //   命令（touch/cp…）没有子命令，默认勾 root。这样既"审过不再审"又保住子命令粒度。
  // - Edit / Write：从 input.file_path 切「精确文件 / 父目录」两档路径前缀
  // - 其它工具：仅一档「工具 X」（pattern=null = Any matcher）兜底
  const memoryOptions: MemoryOption[] = useMemo(() => {
    if (!pending) return [];
    const opts: MemoryOption[] = [];
    if (pending.toolName === "Bash") {
      const segs = pending.commandSegments ?? [];
      const seenSub = new Set<string>();
      const seenRoot = new Set<string>();
      for (const fp of segs) {
        const parsed = parseBashPrefixes(fp);
        if (!parsed) continue;
        const hasSub = !!parsed.sub && parsed.sub !== parsed.root;
        if (hasSub && !seenSub.has(parsed.sub!)) {
          seenSub.add(parsed.sub!);
          opts.push({
            key: `sub:${parsed.sub}`,
            pattern: parsed.sub!,
            label: parsed.sub!,
            hint: "精确子命令（推荐）",
            defaultChecked: true,
          });
        }
        if (!seenRoot.has(parsed.root)) {
          seenRoot.add(parsed.root);
          opts.push({
            key: `root:${parsed.root}`,
            pattern: parsed.root,
            label: `${parsed.root} *`,
            hint: hasSub ? "该命令的所有子命令（更宽）" : "该命令的所有用法",
            defaultChecked: !hasSub,
          });
        }
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
    } else {
      opts.push({
        key: `tool:${pending.toolName}`,
        pattern: null,
        label: `工具 ${pending.toolName}`,
        hint: "工具名级允许（粒度较粗）",
        defaultChecked: true,
      });
    }
    return opts;
  }, [pending?.toolName, pending?.input, pending?.commandSegments]);

  if (!pending) return null;

  const isPathAccess = pending.kind === "path_access";

  // Plan 审批不在输入框上方弹窗：展示与审批操作整体下沉到右侧「计划」栏
  // （架构 §4.4.5）。HITL 通路不变，仅 UI 承载位置迁移。
  if (pending.kind === "plan") return null;

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
    <div className="max-w-3xl mx-auto pb-2">
      <div className="pr-[50px]">
        <div
          className={cn(
            "w-full rounded-lg border border-border bg-card text-card-foreground shadow-lg overflow-hidden pointer-events-auto",
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
        ) : !isPathAccess &&
          (pending.toolName === "Bash" || pending.toolName === "PowerShell") ? (
          // Bash / PowerShell 用结构化预览：command / description / timeout / background
          // 比原始 JSON 直观；hoveredPattern 命中时高亮对应命令段（跟 MemoryRecallPanel 联动）
          <BashArgsPreview args={pending.input} highlight={hoveredPattern} />
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

        {pending.autoJudgeReason && !feedbackOpen && (
          <AutoJudgeReason reason={pending.autoJudgeReason} />
        )}

        {/* 反馈输入框（按需展开，仅 tool_call 有） */}
        {feedbackOpen && (
          <div className="px-3 py-2 border-t border-border">
            <textarea
              value={feedback}
              onChange={(e) => setFeedback(e.target.value)}
              onKeyDown={(e) => {
                if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                  e.preventDefault();
                  if (!submitting && feedback.trim()) {
                    send({ kind: "deny_with_feedback", feedback: feedback.trim() });
                  }
                }
              }}
              placeholder="告诉Hebbian如何改进（⌘/Ctrl+Enter 提交）"
              rows={2}
              className="w-full resize-none rounded-md border border-input bg-background px-2 py-1.5 text-sm outline-none focus:ring-2 focus:ring-ring"
              autoFocus
            />
          </div>
        )}

        {/* 段级白名单状态：复合命令拆段后逐段展示——已白名单段 ✓ 划掉（本次无需处理）、
            待批段正常、rm 等不可记段红色（每次必审、不可勾选）、只读段灰显。
            让用户一眼看清「为什么这条还要批」「哪几段其实已经放行了」（架构 §4.4.2.3）。 */}
        {!isPathAccess && !feedbackOpen && (pending.segments?.length ?? 0) > 0 && (
          <div className="px-3 py-2 border-t border-border/60 space-y-1">
            {pending.segments!.map((seg, i) => {
              const meta = SEG_META[seg.status];
              return (
                <div key={i} className="flex items-center gap-2 text-[12px]">
                  <span
                    className={cn(
                      "shrink-0 px-1.5 py-0.5 rounded text-[10px] font-medium",
                      meta.badge
                    )}
                  >
                    {meta.label}
                  </span>
                  <code className={cn("font-mono truncate", meta.text)}>
                    {seg.fingerprint}
                  </code>
                </div>
              );
            })}
          </div>
        )}

        {/* 危险复合模式：任何作用域都记不住，明确告诉用户别白点（架构 §4.4.2.2）。 */}
        {!isPathAccess && !feedbackOpen && pending.refuseRemember && (
          <div className="px-3 py-2 border-t border-border/60 text-[12px] text-amber-600 dark:text-amber-500 flex items-start gap-1.5">
            <MessageSquareWarning className="w-3.5 h-3.5 shrink-0 mt-0.5" />
            <span>此命令含危险复合模式，出于安全每次都需确认，无法加入白名单。</span>
          </div>
        )}

        {/* 二级区：「记忆 pattern 多选 list + scope 按钮」（架构 §4.4.2 段级判定）。
            Bash 列出 sub / root / compound 各段 root；其它工具只暴露工具名级。
            用户勾选要记的 pattern（默认全选）→ 点 scope 按钮一次性写多条规则。
            路径审批走主按钮区的 4 档，不进二级区。
            refuse_remember（危险复合）时隐藏——点了也写不进去，别让按钮骗人。 */}
        {!isPathAccess &&
          !feedbackOpen &&
          !pending.refuseRemember &&
          memoryOptions.length > 0 && (
          <MemoryRecallPanel
            options={memoryOptions}
            disabled={submitting}
            onHoverPattern={setHoveredPattern}
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
                拒绝并说明
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
  const workdir = useStore((s) => s.currentSession?.workdir ?? null);
  const { beforeText, afterText } = diffSidesFromArgs(toolName, args);
  const action = inferDiffAction(toolName, args);
  const actionLabel =
    action === "create" ? "创建文件" : action === "overwrite" ? "覆盖文件" : "修改文件";

  // 审批弹窗里 before = old_string 是文件片段；读盘拿到原文 indexOf 出真实起始行号
  const enableBaseLookup = action === "modify" && !!args.old_string;
  const originalText = useOriginalFileText(args.file_path, workdir, enableBaseLookup);
  const baseLine = enableBaseLookup && originalText
    ? lineOfOldString(originalText, args.old_string ?? "")
    : 1;

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
            baseLineBefore={baseLine}
            baseLineAfter={baseLine}
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
        maxRows={14}
        baseLineBefore={baseLine}
        baseLineAfter={baseLine}
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
/**
 * Bash / PowerShell 审批的结构化参数预览：解析 args.command / description / timeout_secs /
 * run_in_background，替代原始 JSON 字符串，让人一眼看清「跑什么、要不要后台、多久 timeout」。
 *
 * highlight 来自 MemoryRecallPanel 的 hover 状态——hover 某个 `git *` / `git status` 类
 * pattern 时，命令文本里对应段会被 mark 染色（cross-link 帮用户对照"勾这个 pattern 等于允许命令里这一段"）
 */
function BashArgsPreview({
  args,
  highlight,
}: {
  args: unknown;
  highlight: string | null;
}) {
  const data =
    args && typeof args === "object"
      ? (args as Record<string, unknown>)
      : null;
  const command = typeof data?.command === "string" ? data.command : "";
  const description =
    typeof data?.description === "string" ? data.description : "";
  const timeoutSecs =
    typeof data?.timeout_secs === "number"
      ? data.timeout_secs
      : typeof data?.timeout === "number"
        ? data.timeout
        : null;
  const background = data?.run_in_background === true;

  if (!command) {
    return (
      <pre className="text-[11px] text-muted-foreground/90 px-3 py-2 max-h-32 overflow-auto bg-background/50 font-mono whitespace-pre-wrap break-all">
        {JSON.stringify(args, null, 2)}
      </pre>
    );
  }

  return (
    <div className="px-3 py-2 max-h-40 overflow-auto bg-background/50 text-[12px] space-y-1.5">
      {description && (
        <div className="text-muted-foreground/85">{description}</div>
      )}
      <div className="font-mono text-foreground whitespace-pre-wrap break-all">
        <span className="text-muted-foreground/60 select-none">$ </span>
        {highlightCommand(command, highlight)}
      </div>
      {(timeoutSecs !== null || background) && (
        <div className="flex items-center gap-3 text-[11px] text-muted-foreground/80">
          {background && <span className="font-mono">run_in_background</span>}
          {timeoutSecs !== null && (
            <span className="font-mono">timeout {timeoutSecs}s</span>
          )}
        </div>
      )}
    </div>
  );
}

function highlightCommand(command: string, highlight: string | null): ReactNode {
  if (!highlight || !command) return command;
  // pattern 后缀 ` *` 是 compound 段的通配符（如 `git *`）；高亮时只用根命令字面量
  const needle = highlight.replace(/\s*\*\s*$/, "").trim();
  if (!needle) return command;
  // 多处出现都高亮（cd /tmp && cd /foo 这种 `cd` 命中两次都染色）
  const parts: ReactNode[] = [];
  let cursor = 0;
  let key = 0;
  while (cursor <= command.length) {
    const idx = command.indexOf(needle, cursor);
    if (idx < 0) {
      parts.push(command.slice(cursor));
      break;
    }
    if (idx > cursor) parts.push(command.slice(cursor, idx));
    parts.push(
      <mark
        key={key++}
        className="rounded bg-primary/20 px-0.5 text-primary"
      >
        {command.slice(idx, idx + needle.length)}
      </mark>,
    );
    cursor = idx + needle.length;
  }
  return <>{parts}</>;
}

function MemoryRecallPanel({
  options,
  disabled,
  onApply,
  onHoverPattern,
}: {
  options: MemoryOption[];
  disabled?: boolean;
  onApply: (
    picked: MemoryOption[],
    scope: "session" | "project" | "global",
  ) => void;
  /** hover label 时回调 pattern（leave 时回调 null）—— 让外层 Bash 预览高亮对应段 */
  onHoverPattern?: (pattern: string | null) => void;
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
        {options.map((opt) => {
          const isChecked = !!checked[opt.key];
          return (
            <label
              key={opt.key}
              onMouseEnter={() => onHoverPattern?.(opt.pattern)}
              onMouseLeave={() => onHoverPattern?.(null)}
              className={cn(
                "flex items-center gap-2 text-[12px] cursor-pointer px-1.5 py-1 rounded select-none transition-colors",
                isChecked
                  ? "hover:bg-muted/40"
                  : "opacity-60 hover:opacity-100 hover:bg-muted/30",
              )}
              data-testid={`memory-option-${opt.key}`}
            >
              <input
                type="checkbox"
                checked={isChecked}
                onChange={() => toggle(opt.key)}
                className="accent-primary"
                disabled={disabled}
              />
              <code
                className={cn(
                  "font-mono text-[12px] truncate flex-1 min-w-0",
                  isChecked ? "text-foreground" : "text-muted-foreground",
                )}
              >
                {opt.label}
              </code>
            </label>
          );
        })}
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
          "当前项目的所有对话都自动放行",
        )}
        {scopeBtn(
          "global",
          "全局",
          <Globe className="w-3.5 h-3.5" />,
          "所有项目、所有对话都自动放行",
        )}
      </div>
    </div>
  );
}
