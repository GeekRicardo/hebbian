import { Suspense, lazy, useCallback, useRef, useState, type CSSProperties } from "react";
import { ChatView } from "@/desktop/ui/components/ChatView";
import { RightSidebar } from "@/desktop/ui/components/RightSidebar";
import { DesktopSidebar } from "@/desktop/ui/components/DesktopSidebar";
import { useStore, selectCurrentEditorTabs } from "@/desktop/ui/store/useStore";
import { cn } from "@/desktop/ui/lib/utils";
import { animations } from "@/assets/animations";
import "./desktopShell.css";

// Monaco 体量大：编辑区整列懒加载，没人打开 tab 就不进主 bundle 路径。
const EditorPane = lazy(() => import("@/desktop/ui/components/EditorPane"));

function clampColor(value: number) {
  return Math.max(0, Math.min(255, Math.round(value)));
}

function hslToRgb(hue: number, saturation: number, lightness: number) {
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
  const segment = hue / 60;
  const x = chroma * (1 - Math.abs((segment % 2) - 1));
  const [r1, g1, b1] =
    segment < 1 ? [chroma, x, 0] :
    segment < 2 ? [x, chroma, 0] :
    segment < 3 ? [0, chroma, x] :
    segment < 4 ? [0, x, chroma] :
    segment < 5 ? [x, 0, chroma] :
    [chroma, 0, x];
  const m = lightness - chroma / 2;
  return {
    r: clampColor((r1 + m) * 255),
    g: clampColor((g1 + m) * 255),
    b: clampColor((b1 + m) * 255),
  };
}

function shiftedRgba(
  base: { r: number; g: number; b: number },
  delta: { r: number; g: number; b: number },
  alpha: number
) {
  return `rgba(${clampColor(base.r + delta.r)}, ${clampColor(base.g + delta.g)}, ${clampColor(base.b + delta.b)}, ${alpha})`;
}

function hueStyle(hue: number, themeId: string): CSSProperties {
  const accent = `hsl(${hue} 92% 55%)`;
  const accent2 = `hsl(${(hue + 28) % 360} 92% 64%)`;
  const baseMain = hslToRgb(208, 0.92, 0.58);
  const currentMain = hslToRgb(hue, 0.92, 0.58);
  const delta = {
    r: currentMain.r - baseMain.r,
    g: currentMain.g - baseMain.g,
    b: currentMain.b - baseMain.b,
  };

  if (themeId === "abyss") {
    return {
      "--dsp-bg": "#07111c",
      "--dsp-canvas": "#091522",
      "--dsp-sidebar": "rgb(7 15 25 / 0.9)",
      "--dsp-card": "rgb(17 31 48 / 0.72)",
      "--dsp-card-strong": "rgb(16 29 45 / 0.94)",
      "--dsp-line": "rgb(142 180 218 / 0.14)",
      "--dsp-line-strong": "rgb(151 196 237 / 0.24)",
      "--dsp-text": "#e8f2ff",
      "--dsp-muted": "#93a8bf",
      "--dsp-faint": "#61748a",
      "--dsp-accent": `hsl(${hue} 94% 66%)`,
      "--dsp-accent-2": `hsl(${(hue + 42) % 360} 86% 72%)`,
      "--dsp-accent-soft": `hsl(${hue} 94% 66% / 0.18)`,
      "--dsp-chat-wash": `hsl(${hue} 92% 64% / 0.08)`,
      "--dsp-chat-bubble-a": `hsl(${hue} 90% 66% / 0.16)`,
      "--dsp-chat-bubble-b": `hsl(${(hue + 42) % 360} 82% 70% / 0.13)`,
      "--dsp-chat-bubble-c": `hsl(${(hue + 142) % 360} 58% 64% / 0.1)`,
      "--dsp-chat-bubble-d": `hsl(${(hue + 308) % 360} 72% 68% / 0.1)`,
      "--dsp-chat-panel": "rgb(8 18 29 / 0.72)",
      "--dsp-right-bg": "linear-gradient(180deg, rgb(9 20 32 / 0.92), rgb(6 14 23 / 0.82))",
      "--dsp-right-card": "rgb(15 29 45 / 0.9)",
      "--dsp-user-bubble": `linear-gradient(135deg, hsl(${hue} 92% 62% / 0.2), rgb(14 28 44 / 0.95))`,
      "--dsp-user-line": `hsl(${hue} 92% 66% / 0.26)`,
      "--dsp-orb-shadow": `0 24px 70px hsl(${hue} 92% 62% / 0.22)`,
      "--dsp-hero-strip": "rgb(42 93 137 / 0.5)",
      "--dsp-hero-orb": `hsl(${hue} 92% 62% / 0.2)`,
      "--dsp-hero-panel-a": "rgb(14 29 46 / 0.96)",
      "--dsp-hero-panel-b": "rgb(15 38 61 / 0.88)",
      "--dsp-hero-panel-c": "rgb(21 31 58 / 0.78)",
      "--primary": `${hue} 94% 64%`,
      "--ring": `${hue} 94% 64%`,
      "--dsp-green": "#48c78e",
      "--dsp-amber": "#d6a44c",
      "--dsp-danger": "#f06f72",
      "--dsp-shadow": "0 22px 70px rgb(0 0 0 / 0.42)",
      "--dsp-shadow-soft": "0 12px 34px rgb(0 0 0 / 0.28)",
      "--dsp-shell-bg": `radial-gradient(circle at 20% 0%, hsl(${hue} 92% 62% / 0.16), transparent 32%), radial-gradient(circle at 78% 12%, hsl(${(hue + 52) % 360} 86% 70% / 0.12), transparent 30%), linear-gradient(135deg, #050b13 0%, var(--dsp-bg) 48%, #0a1320 100%)`,
      "--dsp-sidebar-fade": "rgb(6 13 22 / 0.5)",
      "--dsp-chat-panel-end": "rgb(6 14 23 / 0.38)",
      "--dsp-hue-popover-bg": "rgb(10 21 34 / 0.96)",
      "--dsp-hue-button-bg": "rgb(18 33 50 / 0.72)",
      "--dsp-theme-preset-bg": "rgb(18 33 50 / 0.66)",
      "--dsp-ring-core": "rgb(10 21 34 / 0.96)",
    } as CSSProperties;
  }

  return {
    "--dsp-accent": accent,
    "--dsp-accent-2": accent2,
    "--dsp-accent-soft": `hsl(${hue} 92% 55% / 0.12)`,
    "--dsp-chat-wash": `hsl(${hue} 72% 58% / 0.045)`,
    "--dsp-chat-bubble-a": `hsl(${hue} 80% 62% / 0.12)`,
    "--dsp-chat-bubble-b": `hsl(${(hue + 42) % 360} 76% 64% / 0.1)`,
    "--dsp-chat-bubble-c": `hsl(${(hue + 148) % 360} 58% 66% / 0.08)`,
    "--dsp-chat-bubble-d": `hsl(${(hue + 318) % 360} 70% 68% / 0.08)`,
    "--dsp-chat-panel": `hsl(${hue} 58% 99% / 0.7)`,
    "--dsp-right-bg": `linear-gradient(180deg, hsl(${hue} 52% 98% / 0.82), hsl(${hue} 42% 95% / 0.72))`,
    "--dsp-right-card": `hsl(${hue} 44% 99% / 0.88)`,
    "--dsp-user-bubble": `linear-gradient(135deg, hsl(${hue} 92% 55% / 0.1), hsl(${hue} 52% 99% / 0.94))`,
    "--dsp-user-line": `hsl(${hue} 92% 55% / 0.18)`,
    "--dsp-orb-shadow": `0 20px 50px hsl(${hue} 92% 55% / 0.16)`,
    "--dsp-hero-strip": shiftedRgba({ r: 224, g: 235, b: 247 }, delta, 0.58),
    "--dsp-hero-orb": shiftedRgba({ r: 92, g: 150, b: 255 }, delta, 0.14),
    "--dsp-hero-panel-a": shiftedRgba({ r: 255, g: 255, b: 255 }, delta, 0.96),
    "--dsp-hero-panel-b": shiftedRgba({ r: 235, g: 246, b: 255 }, delta, 0.82),
    "--dsp-hero-panel-c": shiftedRgba({ r: 223, g: 245, b: 232 }, delta, 0.72),
    "--primary": `${hue} 92% 58%`,
    "--ring": `${hue} 92% 58%`,
    "--dsp-sidebar": `hsl(${hue} 48% 94% / 0.86)`,
    "--dsp-bg": `hsl(${hue} 42% 97%)`,
    "--dsp-canvas": `hsl(${hue} 52% 99%)`,
    "--dsp-line": `hsl(${hue} 36% 26% / 0.09)`,
    "--dsp-line-strong": `hsl(${hue} 44% 28% / 0.16)`,
    "--dsp-shadow": `0 18px 50px hsl(${hue} 34% 26% / 0.13)`,
    "--dsp-shadow-soft": `0 10px 26px hsl(${hue} 34% 26% / 0.08)`,
  } as CSSProperties;
}


function DesktopEmptyState() {
  return (
    <div className="dsp-empty-state">
      <div className="dsp-welcome-brand">
        <div className="dsp-brand-mark">
          <span className="dsp-hero-traffic" aria-hidden="true" />
          <span className="dsp-hero-corner" aria-hidden="true" />
          <span className="dsp-hero-sidebar" aria-hidden="true" />
          <span className="dsp-hero-grid" aria-hidden="true" />
          <span className="dsp-hero-cardlet" aria-hidden="true">
            <img className="dsp-hero-logo" src={animations.brandMark} alt="" draggable={false} />
          </span>
          <span className="dsp-hero-line is-one" aria-hidden="true" />
          <span className="dsp-hero-line is-two" aria-hidden="true" />
          <span className="dsp-hero-line is-three" aria-hidden="true" />
          <span className="dsp-hero-bubble is-a" aria-hidden="true" />
          <span className="dsp-hero-bubble is-b" aria-hidden="true" />
        </div>
      </div>
      <h3>你想用 Hebbian 做什么</h3>
    </div>
  );
}

function DesktopChat() {
  return (
    <main className="dsp-chat dsp-chat-host">
      <ChatView emptyState={<DesktopEmptyState />} />
    </main>
  );
}

const VIEWER_DEFAULT_WIDTH = 700;
const VIEWER_MIN_WIDTH = 360;
const VIEWER_MAX_WIDTH = 1100;

/**
 * 工作区编辑区列：夹在 chat 与右侧工作台之间，仅在有打开的 tab 时出现，把 chat 挤窄。
 *
 * 左边缘可拖改宽度；宽度只在本次运行内记忆（模块外不存），刷新/重启回默认——
 * 与右侧工作台的「宽度不持久化」一致。
 */
function EditorColumn() {
  const hasTabs = useStore((s) => selectCurrentEditorTabs(s).length > 0);
  const [width, setWidth] = useState(VIEWER_DEFAULT_WIDTH);
  const [resizing, setResizing] = useState(false);
  const dragRef = useRef<{ startX: number; startWidth: number } | null>(null);

  const clamp = useCallback(
    (v: number) => Math.min(VIEWER_MAX_WIDTH, Math.max(VIEWER_MIN_WIDTH, v)),
    [],
  );

  const onDragStart = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      dragRef.current = { startX: e.clientX, startWidth: width };
      setResizing(true);
      document.body.style.cursor = "ew-resize";
      document.body.style.userSelect = "none";
      const onMove = (ev: MouseEvent) => {
        const drag = dragRef.current;
        if (!drag) return;
        // 左边缘拖动：右边缘固定，往左拖变宽。
        setWidth(clamp(drag.startWidth - (ev.clientX - drag.startX)));
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
    [width, clamp],
  );

  if (!hasTabs) return null;

  return (
    <div className="relative h-full shrink-0" style={{ width: `${width}px` }}>
      <div
        onMouseDown={onDragStart}
        className={cn(
          "absolute left-0 top-0 z-10 h-full w-1 cursor-ew-resize hover:bg-primary/30",
          resizing && "bg-primary/40",
        )}
        title="拖动改宽度"
        aria-label="调整编辑区宽度"
      />
      <Suspense fallback={<div className="grid h-full place-items-center text-sm text-muted-foreground">加载编辑器…</div>}>
        <EditorPane />
      </Suspense>
    </div>
  );
}

export function DesktopShell() {
  const [hue, setHue] = useState(208);
  const [themeId, setThemeId] = useState("glacier");
  return (
    <div className="dsp-shell" data-dsp-theme={themeId} style={hueStyle(hue, themeId)}>
      <DesktopSidebar hue={hue} setHue={setHue} themeId={themeId} setThemeId={setThemeId} />
      <DesktopChat />
      <EditorColumn />
      <RightSidebar
        defaultWidth={640}
        minWidth={200}
        maxWidth={960}
        storagePrefix="hebbian.desktopShell.rightSidebar.wide"
      />
    </div>
  );
}
