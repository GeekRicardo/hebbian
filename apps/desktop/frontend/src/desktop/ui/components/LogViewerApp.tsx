import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Pin, PinOff } from "lucide-react";
import LogConsole from "@/desktop/ui/components/LogConsole";

// 独立日志窗口：标题栏 + 置顶开关 + 共用的 LogConsole（搜索/过滤/虚拟滚动都在里面）。
export default function LogViewerApp() {
  const [alwaysOnTop, setAlwaysOnTop] = useState(true);

  const toggleAlwaysOnTop = useCallback(async () => {
    const next = !alwaysOnTop;
    setAlwaysOnTop(next);
    try {
      await invoke("set_log_viewer_always_on_top", { alwaysOnTop: next });
    } catch {}
  }, [alwaysOnTop]);

  return (
    <div className="flex h-screen flex-col bg-[#0b0b0c] text-foreground">
      {/* 标题栏（可拖拽） */}
      <div
        className="flex h-9 shrink-0 items-center justify-between border-b border-white/10 bg-[#121214] px-3"
        style={{ WebkitAppRegion: "drag" } as React.CSSProperties}
      >
        <span className="text-xs font-medium text-white/55">日志查看器</span>
        <div className="flex items-center gap-1" style={{ WebkitAppRegion: "no-drag" } as React.CSSProperties}>
          <button
            type="button"
            onClick={toggleAlwaysOnTop}
            className={`rounded p-1 transition-colors ${
              alwaysOnTop ? "text-sky-400 hover:bg-white/10" : "text-white/50 hover:bg-white/10 hover:text-white/80"
            }`}
            title={alwaysOnTop ? "取消置顶" : "永远置顶"}
          >
            {alwaysOnTop ? <Pin className="h-3.5 w-3.5" /> : <PinOff className="h-3.5 w-3.5" />}
          </button>
        </div>
      </div>

      {/* 独立窗口字号给大一点，看着更像专门的日志工具 */}
      <div className="min-h-0 flex-1 p-2">
        <LogConsole fontSize={13.5} rowHeight={24} />
      </div>
    </div>
  );
}
