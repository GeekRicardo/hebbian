import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface NotchPayload {
  type: "pending" | "info";
  /** "permission_requested" | "user_question" | "turn_completed" */
  kind: string;
  title: string;
  summary: string;
  /** pending 类携带，用于卡片内联审批 / 撤销。 */
  request_id?: string;
  /** 审批子类型："tool_call" | "path_access" | "plan"。仅 tool_call 可内联放行。 */
  perm_kind?: string;
}

interface Props {
  payload: NotchPayload;
  onDismiss: () => void;
  onOpen: () => void;
}

/** 三种状态的强调色（编译期固定，避免动态拼 tailwind class 失效）。 */
const ACCENT = {
  approval: { chip: "bg-amber-400/15 text-amber-300 ring-1 ring-amber-400/25", glyph: "⚠" },
  question: { chip: "bg-sky-400/15 text-sky-300 ring-1 ring-sky-400/25", glyph: "？" },
  done: { chip: "bg-emerald-400/15 text-emerald-300 ring-1 ring-emerald-400/25", glyph: "✓" },
} as const;

export default function NotificationCard({ payload, onDismiss, onOpen }: Props) {
  const cardRef = useRef<HTMLDivElement>(null);
  const [busy, setBusy] = useState(false);

  const isApproval = payload.kind === "permission_requested";
  const isQuestion = payload.kind === "user_question";
  const canInlineApprove = isApproval && payload.perm_kind === "tool_call";
  const accent = isApproval ? ACCENT.approval : isQuestion ? ACCENT.question : ACCENT.done;

  // 内容高度随状态变化 → 通知后端把窗口高度贴合卡片，避免裁切 / 留黑边。
  useEffect(() => {
    const el = cardRef.current;
    if (!el) return;
    const report = () => {
      const h = Math.ceil(el.getBoundingClientRect().height) + 32;
      invoke("notify_resize", { height: h }).catch(() => {});
    };
    report();
    const ro = new ResizeObserver(report);
    ro.observe(el);
    return () => ro.disconnect();
  }, [payload]);

  // info 类：3s 自动关闭
  useEffect(() => {
    if (payload.type === "info") {
      const t = setTimeout(onDismiss, 3_000);
      return () => clearTimeout(t);
    }
  }, [payload.type, onDismiss]);

  async function approve(decision: "allow_once" | "deny") {
    if (!payload.request_id || busy) return;
    setBusy(true);
    try {
      await invoke("approve_permission", {
        requestId: payload.request_id,
        decision,
        feedback: null,
        pattern: null,
        scope: "session",
        extraPatterns: [],
      });
      onDismiss();
    } catch {
      setBusy(false);
    }
  }

  return (
    <div className="fixed inset-0 flex items-start justify-end p-2.5 select-none">
      <div
        ref={cardRef}
        className="w-[336px] rounded-[24px] bg-[#0b0b0e]/95 backdrop-blur-2xl border border-white/10 shadow-[0_12px_50px_rgba(0,0,0,0.6)] overflow-hidden"
      >
        {/* header */}
        <div className="flex items-center gap-2.5 px-4 pt-3.5 pb-2">
          <span className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-[13px] ${accent.chip}`}>
            {accent.glyph}
          </span>
          <span className="flex-1 truncate text-[13px] font-semibold text-white/90">{payload.title}</span>
          <button
            onClick={onDismiss}
            className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-white/35 hover:bg-white/10 hover:text-white/70 transition-colors"
            title="关闭"
          >
            ✕
          </button>
        </div>

        {/* body */}
        <div className="px-4 pb-3">
          <p className="text-[12.5px] leading-relaxed text-white/55 line-clamp-3 break-words">
            {payload.summary}
          </p>
        </div>

        {/* actions（仅 pending 类）*/}
        {payload.type === "pending" && (
          <div className="flex items-center gap-1.5 px-4 pb-3.5">
            {canInlineApprove ? (
              <>
                <button
                  disabled={busy}
                  onClick={() => approve("allow_once")}
                  className="flex-1 h-8 rounded-xl bg-emerald-500/90 hover:bg-emerald-500 text-[12.5px] font-medium text-white transition-colors disabled:opacity-50"
                >
                  允许本次
                </button>
                <button
                  disabled={busy}
                  onClick={() => approve("deny")}
                  className="flex-1 h-8 rounded-xl bg-white/10 hover:bg-white/15 text-[12.5px] font-medium text-white/80 transition-colors disabled:opacity-50"
                >
                  拒绝
                </button>
                <button
                  onClick={onOpen}
                  className="h-8 px-3 rounded-xl bg-white/[0.06] hover:bg-white/10 text-[12.5px] text-white/55 transition-colors"
                  title="在主窗口处理（可选范围 / 反馈）"
                >
                  打开
                </button>
              </>
            ) : (
              <button
                onClick={onOpen}
                className="flex-1 h-8 rounded-xl bg-white/10 hover:bg-white/15 text-[12.5px] font-medium text-white/80 transition-colors"
              >
                打开处理
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
