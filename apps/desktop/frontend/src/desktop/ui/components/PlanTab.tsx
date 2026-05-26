import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, MessageSquarePlus, History, FileText, Loader2 } from "lucide-react";
import { toast } from "sonner";
import { api } from "@/desktop/bridge/tauri";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import { MarkdownRenderer } from "./MarkdownRenderer";
import type { PlanComment, PlanMeta } from "@/desktop/ui/types";

/**
 * 右侧工作台第四个 tab：Plan 预览 + 评论流（架构 §4.4.5）。
 *
 * - 顶栏下拉切换历史 plan（按 mtime 倒序，活跃那份标识 "当前"）
 * - 主区 ReactMarkdown 渲染 plan markdown（与 chat 文本同款渲染器）
 * - 选中 markdown 一段 → 出 "💬 加评论" 按钮 → 弹评论输入；anchor 自动填选段首段文字
 * - 底部按钮组：全局加评论 / 刷新；comments 在主区下方按时间序列表
 *
 * **评论的去处**：评论落盘 `plans/<plan_id>.comments.jsonl`；下一轮 user message
 * 发送时 agent_core 把 unconsumed comments 拼到 SEMI 段，agent 据此改 plan。
 */
export function PlanTab() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const activePlan = useStore((s) => s.activePlan);
  const planComments = useStore((s) => s.planComments);
  const setActivePlan = useStore((s) => s.setSessionActivePlan);
  const replaceComments = useStore((s) => s.replaceSessionPlanComments);
  const appendComment = useStore((s) => s.appendSessionPlanComment);

  const [plans, setPlans] = useState<PlanMeta[]>([]);
  const [loadingPlans, setLoadingPlans] = useState(false);
  const [selectedPlanId, setSelectedPlanId] = useState<string | null>(null);
  const [planMd, setPlanMd] = useState<string>("");
  const [loadingMd, setLoadingMd] = useState(false);

  // 拉历史 plan 列表
  const refreshPlans = useCallback(async () => {
    if (!sessionId) return;
    setLoadingPlans(true);
    try {
      const list = await api.listSessionPlans(sessionId);
      setPlans(list);
      // 默认选中 active 那份；没 active 选最新一份
      const active = list.find((p) => p.is_active) ?? list[0];
      if (active) {
        setSelectedPlanId((cur) => cur ?? active.plan_id);
      } else {
        setSelectedPlanId(null);
      }
    } catch (e) {
      console.warn("listSessionPlans failed", e);
    } finally {
      setLoadingPlans(false);
    }
  }, [sessionId]);

  useEffect(() => {
    refreshPlans();
  }, [refreshPlans]);

  // store 里 activePlan 变了（新 ExitPlanMode 落盘）→ 自动刷新列表 + 选中它
  useEffect(() => {
    if (activePlan && activePlan.plan_id !== selectedPlanId) {
      refreshPlans();
      setSelectedPlanId(activePlan.plan_id);
    }
  }, [activePlan, refreshPlans, selectedPlanId]);

  // 读 markdown + comments
  useEffect(() => {
    if (!sessionId || !selectedPlanId) {
      setPlanMd("");
      return;
    }
    let cancelled = false;
    setLoadingMd(true);
    Promise.all([
      api.readPlanMarkdown(sessionId, selectedPlanId),
      api.listPlanComments(sessionId, selectedPlanId),
    ])
      .then(([md, cmts]) => {
        if (cancelled) return;
        setPlanMd(md);
        replaceComments(sessionId, selectedPlanId, cmts);
        // 同步把当前选中的 plan 写进 activePlan 镜像，方便其他组件读
        const meta = plans.find((p) => p.plan_id === selectedPlanId);
        if (meta && meta.is_active) {
          setActivePlan(sessionId, {
            plan_id: meta.plan_id,
            plan_path: meta.plan_path,
            markdown: md,
            summary: meta.title,
          });
        }
      })
      .catch((e) => {
        console.warn("readPlanMarkdown failed", e);
        if (!cancelled) toast.error(`读取 plan 失败：${e}`);
      })
      .finally(() => {
        if (!cancelled) setLoadingMd(false);
      });
    return () => {
      cancelled = true;
    };
    // plans 故意不进 deps：plans 变化由 refreshPlans 触发已经刷过一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, selectedPlanId, replaceComments, setActivePlan]);

  // 选区评论：监听 markdown 容器的 selectionchange
  const markdownContainerRef = useRef<HTMLDivElement | null>(null);
  const [selection, setSelection] = useState<{ text: string } | null>(null);

  useEffect(() => {
    function onSelect() {
      const sel = window.getSelection();
      if (!sel || sel.rangeCount === 0) {
        setSelection(null);
        return;
      }
      const range = sel.getRangeAt(0);
      const container = markdownContainerRef.current;
      if (!container) return;
      // 仅识别落在 plan markdown 区域内的选区
      if (!container.contains(range.commonAncestorContainer)) {
        setSelection(null);
        return;
      }
      const text = sel.toString().trim();
      if (text.length < 3) {
        setSelection(null);
        return;
      }
      setSelection({ text });
    }
    document.addEventListener("selectionchange", onSelect);
    return () => document.removeEventListener("selectionchange", onSelect);
  }, []);

  // 添加评论
  const [showCommentBox, setShowCommentBox] = useState(false);
  const [commentBody, setCommentBody] = useState("");
  const [commentAnchor, setCommentAnchor] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const openCommentBox = (preset?: { anchor: string }) => {
    setCommentAnchor(preset?.anchor ?? "");
    setCommentBody("");
    setShowCommentBox(true);
  };
  const submitComment = async () => {
    if (!sessionId || !selectedPlanId) return;
    const body = commentBody.trim();
    if (!body) {
      toast.error("评论内容不能为空");
      return;
    }
    const anchor = commentAnchor.trim() || "(global)";
    setSubmitting(true);
    try {
      const saved = await api.addPlanComment(sessionId, selectedPlanId, anchor, body);
      appendComment(sessionId, selectedPlanId, saved);
      setShowCommentBox(false);
      setCommentBody("");
      setCommentAnchor("");
    } catch (e) {
      toast.error(`添加评论失败：${e}`);
    } finally {
      setSubmitting(false);
    }
  };

  if (!sessionId) {
    return (
      <div className="p-4 text-sm text-muted-foreground">打开一个对话再查看 plan。</div>
    );
  }
  if (plans.length === 0) {
    return (
      <div className="p-4 text-sm text-muted-foreground">
        本对话还没有 plan。切到「计划模式」让 agent 出一份。
      </div>
    );
  }

  const currentMeta = plans.find((p) => p.plan_id === selectedPlanId);
  const comments = selectedPlanId ? planComments[selectedPlanId] ?? [] : [];
  const unconsumed = comments.filter((c) => !c.consumed);

  return (
    <div className="flex h-full flex-col">
      {/* 顶栏：plan 切换 + 标记 */}
      <div className="border-b border-border px-3 py-2">
        <div className="flex items-center gap-2 text-xs">
          <FileText className="h-3.5 w-3.5 text-muted-foreground" />
          <PlanPicker plans={plans} selected={selectedPlanId} onSelect={setSelectedPlanId} />
          {loadingPlans && <Loader2 className="h-3 w-3 animate-spin text-muted-foreground" />}
        </div>
        {currentMeta && (
          <div className="mt-1 flex items-center gap-2 text-[11px] text-muted-foreground">
            {currentMeta.is_active && (
              <span className="rounded bg-primary/10 px-1.5 py-0.5 text-primary">当前</span>
            )}
            <span>{new Date(currentMeta.updated_at_ms).toLocaleString()}</span>
          </div>
        )}
      </div>

      {/* 选区浮动操作条 */}
      {selection && (
        <div className="border-b border-border bg-amber-500/10 px-3 py-1.5 text-xs">
          <button
            type="button"
            onClick={() =>
              openCommentBox({
                anchor: selection.text.slice(0, 40).replace(/\s+/g, " "),
              })
            }
            className="inline-flex items-center gap-1 rounded bg-amber-500 px-2 py-1 text-white"
          >
            <MessageSquarePlus className="h-3 w-3" /> 给选中段加评论
          </button>
          <span className="ml-2 text-muted-foreground">
            "{selection.text.slice(0, 30)}…"
          </span>
        </div>
      )}

      {/* 主区：markdown */}
      <div
        ref={markdownContainerRef}
        className="min-h-0 flex-1 overflow-auto px-3 py-2"
      >
        {loadingMd ? (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" /> 读取中…
          </div>
        ) : (
          <MarkdownRenderer
            markdown={planMd}
            className="prose prose-sm max-w-none dark:prose-invert"
          />
        )}
      </div>

      {/* 评论区 */}
      <div className="shrink-0 border-t border-border bg-muted/30">
        <div className="flex items-center justify-between px-3 py-1.5 text-xs">
          <span className="flex items-center gap-1 text-muted-foreground">
            <History className="h-3 w-3" />
            评论 {comments.length}
            {unconsumed.length > 0 && (
              <span className="text-amber-600">（{unconsumed.length} 条待发送）</span>
            )}
          </span>
          <button
            type="button"
            onClick={() => openCommentBox()}
            className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <MessageSquarePlus className="h-3 w-3" /> 添加
          </button>
        </div>
        <ul className="max-h-40 overflow-auto divide-y divide-border">
          {comments.map((c) => (
            <CommentRow key={c.id} comment={c} />
          ))}
        </ul>
        {showCommentBox && (
          <div className="border-t border-border px-3 py-2">
            <input
              value={commentAnchor}
              onChange={(e) => setCommentAnchor(e.target.value)}
              placeholder="锚点（可选，例如 L12-15 或 选段头部 30 字）"
              className="mb-1 w-full rounded border border-border bg-background px-2 py-1 text-xs"
            />
            <textarea
              value={commentBody}
              onChange={(e) => setCommentBody(e.target.value)}
              placeholder="评论内容"
              rows={3}
              className="w-full rounded border border-border bg-background px-2 py-1 text-xs"
            />
            <div className="mt-1 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setShowCommentBox(false)}
                className="rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent"
              >
                取消
              </button>
              <button
                type="button"
                onClick={submitComment}
                disabled={submitting}
                className="rounded bg-primary px-2 py-1 text-xs text-primary-foreground disabled:opacity-50"
              >
                {submitting ? "提交中…" : "提交"}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function PlanPicker({
  plans,
  selected,
  onSelect,
}: {
  plans: PlanMeta[];
  selected: string | null;
  onSelect: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const cur = plans.find((p) => p.plan_id === selected);
  return (
    <div className="relative flex-1">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between gap-1 rounded border border-border bg-background px-2 py-1 text-left text-xs hover:bg-accent"
      >
        <span className="truncate">{cur?.title ?? "选择一份 plan"}</span>
        <ChevronDown className="h-3 w-3 text-muted-foreground" />
      </button>
      {open && (
        <div className="absolute left-0 right-0 top-full z-30 mt-1 max-h-60 overflow-auto rounded border border-border bg-popover shadow-lg">
          {plans.map((p) => (
            <button
              key={p.plan_id}
              type="button"
              onClick={() => {
                onSelect(p.plan_id);
                setOpen(false);
              }}
              className={cn(
                "flex w-full items-center justify-between gap-2 px-2 py-1.5 text-left text-xs hover:bg-accent",
                p.plan_id === selected && "bg-accent/50"
              )}
            >
              <span className="truncate">{p.title}</span>
              {p.is_active && (
                <span className="shrink-0 rounded bg-primary/10 px-1 text-[10px] text-primary">
                  当前
                </span>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function CommentRow({ comment }: { comment: PlanComment }) {
  return (
    <li
      className={cn(
        "px-3 py-1.5 text-xs",
        comment.consumed ? "text-muted-foreground" : "text-foreground"
      )}
    >
      <div className="flex items-center gap-2">
        <span className="rounded bg-muted px-1 py-0.5 text-[10px] text-muted-foreground">
          {comment.anchor}
        </span>
        <span className="text-[10px] text-muted-foreground">
          {new Date(comment.created_at_ms).toLocaleString()}
        </span>
        {!comment.consumed && (
          <span className="text-[10px] text-amber-600">待发送</span>
        )}
      </div>
      <p className="mt-0.5 whitespace-pre-wrap">{comment.body}</p>
    </li>
  );
}
