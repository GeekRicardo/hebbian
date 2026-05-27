import { useCallback, useEffect, useRef, useState } from "react";

export interface NotchPayload {
  type: "pending" | "info";
  kind: string;
  title: string;
  summary: string;
}

interface Props {
  payload: NotchPayload;
  onDismiss: () => void;
  onClick: () => void;
}

export default function NotificationCard({ payload, onDismiss, onClick }: Props) {
  const [collapsed, setCollapsed] = useState(false);
  const [position, setPosition] = useState({ x: 0, y: 0 });
  const dragging = useRef(false);
  const dragStart = useRef({ x: 0, y: 0 });
  const posRef = useRef({ x: 0, y: 0 });
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // pending 类：折叠后 60s 自动展开
  useEffect(() => {
    if (payload.type === "pending" && collapsed) {
      intervalRef.current = setInterval(() => {
        setCollapsed(false);
      }, 60_000);
    }
    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    };
  }, [payload.type, collapsed]);

  // info 类：3s 自动关闭
  useEffect(() => {
    if (payload.type === "info") {
      const timer = setTimeout(() => onDismiss(), 3_000);
      return () => clearTimeout(timer);
    }
  }, [payload.type, onDismiss]);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    dragging.current = true;
    dragStart.current = { x: e.clientX - posRef.current.x, y: e.clientY - posRef.current.y };
  }, []);

  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    if (!dragging.current) return;
    posRef.current = {
      x: e.clientX - dragStart.current.x,
      y: e.clientY - dragStart.current.y,
    };
    setPosition({ ...posRef.current });
  }, []);

  const handleMouseUp = useCallback(() => {
    dragging.current = false;
  }, []);

  if (collapsed) {
    return (
      <div
        className="fixed top-4 right-4 z-[9999] cursor-pointer select-none"
        style={{
          transform: `translate(${position.x}px, ${position.y}px)`,
        }}
        onClick={() => setCollapsed(false)}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        role="button"
        tabIndex={0}
      >
        <div className="w-10 h-10 bg-black/80 backdrop-blur-xl rounded-full flex items-center justify-center shadow-lg border border-white/10 transition-transform hover:scale-105">
          <span className="text-lg">{payload.kind === "permission_requested" || payload.kind === "user_question" ? "⚠️" : "✓"}</span>
        </div>
      </div>
    );
  }

  return (
    <div
      className="fixed z-[9999] cursor-pointer select-none"
      style={{
        top: 0,
        left: 0,
        transform: `translate(${16 + position.x}px, ${16 + position.y}px)`,
      }}
      onClick={onClick}
      onMouseDown={handleMouseDown}
      onMouseMove={handleMouseMove}
      onMouseUp={handleMouseUp}
      role="button"
      tabIndex={0}
    >
      <div
        className="w-[360px] bg-black/70 backdrop-blur-2xl rounded-2xl border border-white/10 shadow-2xl overflow-hidden transition-all"
        onClick={(e) => e.stopPropagation()}
      >
        {/* header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-white/5">
          <div className="flex items-center gap-2">
            <span className="text-base">{payload.kind === "permission_requested" || payload.kind === "user_question" ? "⚠️" : "✓"}</span>
            <span className="text-sm font-medium text-white/90">{payload.title}</span>
          </div>
          <div className="flex items-center gap-1">
            {payload.type === "pending" && (
              <button
                className="w-6 h-6 flex items-center justify-center rounded-full hover:bg-white/10 text-white/40 hover:text-white/70 transition-colors"
                onClick={(e) => {
                  e.stopPropagation();
                  setCollapsed(true);
                }}
                title="收起"
              >
                ◀
              </button>
            )}
            <button
              className="w-6 h-6 flex items-center justify-center rounded-full hover:bg-white/10 text-white/40 hover:text-white/70 transition-colors"
              onClick={(e) => {
                e.stopPropagation();
                onDismiss();
              }}
              title="关闭"
            >
              ✕
            </button>
          </div>
        </div>
        {/* body */}
        <div className="px-4 py-3">
          <p className="text-sm text-white/70 leading-relaxed line-clamp-2">{payload.summary}</p>
        </div>
        {/* footer hint */}
        <div className="px-4 py-2 border-t border-white/5">
          <span className="text-xs text-white/30">点击打开 Hebbian</span>
        </div>
      </div>
    </div>
  );
}
