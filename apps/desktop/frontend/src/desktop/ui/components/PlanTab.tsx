import { useCallback, useEffect, useState } from "react";
import { ClipboardList, Loader2, RotateCcw } from "lucide-react";
import { api } from "@/desktop/bridge/tauri";
import { useStore } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import type { PlanMeta } from "@/desktop/ui/types";

/**
 * 右侧工作台「计划」栏：只列 plan 标题（架构 §4.4.5）。
 *
 * - 按 mtime 倒序，活跃那份标「当前」；顶部一个刷新按钮
 * - 点标题 → `store.openPlan(planId, title)` 在中间编辑区打开 plan 详情
 *   （markdown 正文 + 选区评论 + 评论列表 + 待审批操作条都在编辑区里）
 */
export function PlanTab() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const activePlan = useStore((s) => s.activePlan);
  const openPlan = useStore((s) => s.openPlan);
  const activeTabId = useStore((s) =>
    sessionId ? s.activeTabBySession[sessionId] ?? null : null,
  );

  const [plans, setPlans] = useState<PlanMeta[]>([]);
  const [loading, setLoading] = useState(false);

  const refreshPlans = useCallback(async () => {
    if (!sessionId) return;
    setLoading(true);
    try {
      setPlans(await api.listSessionPlans(sessionId));
    } catch (e) {
      console.warn("listSessionPlans failed", e);
    } finally {
      setLoading(false);
    }
  }, [sessionId]);

  useEffect(() => {
    refreshPlans();
  }, [refreshPlans]);

  // 新 ExitPlanMode 落盘（activePlan 变）→ 刷新列表，让新 plan 露在栏里。
  // 自动打开编辑区交给 RightSidebar 的待审批 effect。
  useEffect(() => {
    if (activePlan) refreshPlans();
  }, [activePlan, refreshPlans]);

  if (!sessionId) {
    return <div className="p-4 text-sm text-muted-foreground">打开一个对话再查看 plan。</div>;
  }
  if (plans.length === 0) {
    return (
      <div className="p-4 text-sm text-muted-foreground">
        本对话还没有 plan。切到「计划模式」让 agent 出一份。
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-8 shrink-0 items-center justify-between border-b border-border px-3 text-xs text-muted-foreground">
        <span className="flex items-center gap-1.5">
          <ClipboardList className="h-3.5 w-3.5" />
          计划 {plans.length}
        </span>
        <button
          type="button"
          onClick={refreshPlans}
          title="刷新"
          aria-label="刷新"
          className="grid h-5 w-5 place-items-center rounded hover:bg-accent hover:text-foreground"
        >
          {loading ? (
            <Loader2 className="h-3 w-3 animate-spin" />
          ) : (
            <RotateCcw className="h-3 w-3" />
          )}
        </button>
      </div>
      <ul className="min-h-0 flex-1 overflow-auto py-1">
        {plans.map((p) => {
          const isOpen = activeTabId === `plan:${p.plan_id}`;
          return (
            <li key={p.plan_id}>
              <button
                type="button"
                onClick={() => openPlan(p.plan_id, p.title)}
                title={p.title}
                className={cn(
                  "flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs transition-colors",
                  isOpen ? "bg-accent text-foreground" : "hover:bg-accent/50",
                )}
              >
                <span className="min-w-0 flex-1 truncate">{p.title}</span>
                {p.is_active && (
                  <span className="shrink-0 rounded bg-primary/10 px-1 text-[10px] text-primary">
                    当前
                  </span>
                )}
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
