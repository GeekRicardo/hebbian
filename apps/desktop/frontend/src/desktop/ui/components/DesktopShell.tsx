import { useState, type CSSProperties } from "react";
import { ChatView } from "@/desktop/ui/components/ChatView";
import { RightSidebar } from "@/desktop/ui/components/RightSidebar";
import { DesktopSidebar } from "@/desktop/ui/components/DesktopSidebar";
import { BrowserPanel } from "@/desktop/ui/components/BrowserPanel";
import { useBrowserPanel } from "@/desktop/ui/store/browserPanel";
import "./desktopShell.css";

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

function hueStyle(hue: number): CSSProperties {
  const accent = `hsl(${hue} 92% 55%)`;
  const accent2 = `hsl(${(hue + 28) % 360} 92% 64%)`;
  const baseMain = hslToRgb(208, 0.92, 0.58);
  const currentMain = hslToRgb(hue, 0.92, 0.58);
  const delta = {
    r: currentMain.r - baseMain.r,
    g: currentMain.g - baseMain.g,
    b: currentMain.b - baseMain.b,
  };
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
            <span className="dsp-hero-logo">H</span>
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

export function DesktopShell() {
  const [hue, setHue] = useState(208);
  const browserOpen = useBrowserPanel((s) => s.open);
  return (
    <div className="dsp-shell" style={hueStyle(hue)}>
      <DesktopSidebar hue={hue} setHue={setHue} />
      <DesktopChat />
      {browserOpen && <BrowserPanel />}
      <RightSidebar
        defaultWidth={640}
        minWidth={500}
        maxWidth={960}
        storagePrefix="hebbian.desktopShell.rightSidebar.wide"
      />
    </div>
  );
}
