import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import {
  ArrowLeft,
  ArrowRight,
  ExternalLink,
  Globe2,
  Loader2,
  MousePointerSquareDashed,
  RefreshCw,
  Sparkles,
} from "lucide-react";
import { toast } from "sonner";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useStore } from "@/desktop/ui/store/useStore";
import { getBrowserHost, type BrowserStateEvent } from "@/desktop/ui/lib/browserHost";
import {
  extractPreviewUrls,
  formatPreviewUrlLabel,
  messagesToDetectSources,
  normalizePreviewUrlInput,
} from "@/desktop/ui/lib/previewUrl";
import type { HebElementSnapshot } from "@/desktop/ui/lib/annotation";
import { AnnotationCard } from "@/desktop/ui/components/AnnotationCard";

interface SelectedState {
  snapshot: HebElementSnapshot;
  /** 选中元素在主窗口的屏幕位置 + 浏览器视口左边界（卡片要避开原生 webview，落到它左侧的 DOM 区） */
  anchor: { elementTop: number; panelLeft: number };
}

/**
 * 内置浏览器（架构 §4 / §8.5）——RightSidebar 的一个 tab。
 *
 * 承载是原生子 webview（浮在 viewportRef 占位区上方）。本组件负责：地址栏/导航工具栏、
 * 占位区 bounds 同步（ResizeObserver）、选取按钮、注释卡片锚定。webview 内容不在 React
 * 树里——viewportRef 只是一块"留白"，真实页面由 Rust 侧 set_bounds 定位覆盖上去。
 *
 * `active`：是否当前显示的 tab。常驻挂载、切走只隐藏不卸载（保住页面/登录态/滚动）；
 * active=false 时 setVisible(false) 让原生 webview 不盖住别的 tab 内容。
 */
export function BrowserPanel({ active }: { active: boolean }) {
  const host = getBrowserHost();

  const viewportRef = useRef<HTMLDivElement | null>(null);
  const activeRef = useRef(active);
  activeRef.current = active;
  const [draftUrl, setDraftUrl] = useState("");
  const [state, setState] = useState<BrowserStateEvent>({
    url: "",
    can_go_back: false,
    can_go_forward: false,
    loading: false,
  });
  const [title, setTitle] = useState("");
  const [selected, setSelected] = useState<SelectedState | null>(null);
  const [pickerActive, setPickerActive] = useState(false);
  const [autoFollow, setAutoFollow] = useState(true);

  // 聊天流里检测到的本地 dev server 地址（架构 §4.2）。
  const messages = useStore((s) => s.currentSession?.messages);
  const sources = useMemo(() => messagesToDetectSources(messages ?? []), [messages]);
  const detectedUrls = useMemo(() => extractPreviewUrls(sources, "card"), [sources]);
  const autoOpenUrl = useMemo(() => extractPreviewUrls(sources, "autoOpen")[0] ?? null, [sources]);

  // 占位区 → 子 webview bounds 同步。active=false（隐藏 tab）时不同步，避免把 webview
  // 定位到 0×0 或别的 tab 区域。
  const syncBounds = useCallback(() => {
    if (!activeRef.current) return;
    const el = viewportRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    void host.setBounds({ x: r.left, y: r.top, width: r.width, height: r.height });
  }, [host]);

  // 订阅 webview 事件
  useEffect(() => {
    const unlistens: Array<() => void> = [];
    let alive = true;
    const track = (p: Promise<() => void>) =>
      void p.then((fn) => {
        if (alive) unlistens.push(fn);
        else fn();
      });

    track(host.onState((s) => setState(s)));
    track(
      host.onTitle((t) => {
        setTitle(t.title);
        setState((prev) => (prev.url === t.url ? prev : { ...prev, url: t.url }));
      })
    );
    track(
      host.onElement((snapshot) => {
        setPickerActive(false);
        const el = viewportRef.current;
        const base = el ? el.getBoundingClientRect() : { left: 0, top: 0 };
        // 原生 webview 盖在 DOM 之上 → 注释卡片不能落在 webview 区域（会被盖住）。
        // 落到 sidebar 左侧的聊天区（纯 DOM）；元素本身的高亮框由 inspector.js 画在页面内。
        setSelected({
          snapshot,
          anchor: { elementTop: base.top + snapshot.boundingClientRect.y, panelLeft: base.left },
        });
      })
    );
    track(host.onPickerOff(() => setPickerActive(false)));
    track(
      host.onEscaped((info) => {
        toast.warning(info.reason || "该地址无法打开");
      })
    );

    return () => {
      alive = false;
      unlistens.forEach((fn) => fn());
    };
  }, [host]);

  // bounds 同步：窗口 resize + 占位区 resize（sidebar 拖宽/折叠都会触发）
  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => syncBounds());
    ro.observe(el);
    window.addEventListener("resize", syncBounds);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", syncBounds);
    };
  }, [syncBounds]);

  // active 切换：显示 tab → 重新定位 + 显示 webview；切走 → 隐藏 webview（不盖别的 tab）
  useEffect(() => {
    if (active) {
      void host.setVisible(true);
      // 等 DOM 完成布局再取 rect（hidden→显示这一帧 rect 才有效）
      const raf = requestAnimationFrame(() => syncBounds());
      return () => cancelAnimationFrame(raf);
    }
    void host.setVisible(false);
    setSelected(null); // 切走时收起注释卡片
    return undefined;
  }, [active, host, syncBounds]);

  // 卸载（折叠 sidebar / 关闭对话窗口）：关掉子 webview
  useEffect(() => {
    return () => {
      void host.close();
    };
  }, [host]);

  // 地址栏 url 跟随 state（用户聚焦编辑时不抢）
  const addrFocusedRef = useRef(false);
  useEffect(() => {
    if (!addrFocusedRef.current && state.url) {
      setDraftUrl(formatPreviewUrlLabel(state.url));
    }
  }, [state.url]);

  // auto-follow：聊天流里冒出新的 dev server 地址且开关打开时自动跟随。
  // 用户手动输地址（loadUrl user 档）会关掉它，把控制权交还用户。
  const followedRef = useRef<string | null>(null);
  useEffect(() => {
    if (!autoFollow || !autoOpenUrl || followedRef.current === autoOpenUrl) return;
    followedRef.current = autoOpenUrl;
    setSelected(null);
    if (!state.url) void host.open(autoOpenUrl, "auto", currentBounds(viewportRef.current));
    else void host.navigate(autoOpenUrl).catch(() => undefined);
  }, [autoFollow, autoOpenUrl, state.url, host]);

  const loadUrl = (raw: string, origin: "auto" | "user") => {
    const norm = normalizePreviewUrlInput(raw);
    if (!norm) {
      toast.error("这个地址没法打开");
      return;
    }
    if (origin === "user") setAutoFollow(false);
    setSelected(null);
    if (!state.url)
      void host.open(norm, origin, currentBounds(viewportRef.current)).catch((err) => toast.error(String(err)));
    else void host.navigate(norm).catch((err) => toast.error(String(err)));
  };

  const submitUrl = (e: FormEvent) => {
    e.preventDefault();
    loadUrl(draftUrl, "user");
  };

  const togglePicker = () => {
    const next = !pickerActive;
    setPickerActive(next);
    setSelected(null);
    void host.setPicker(next);
  };

  const openExternal = () => {
    if (state.url) void openUrl(state.url).catch(() => undefined);
  };

  return (
    <div className="flex h-full min-h-0 w-full flex-col bg-background">
      <form onSubmit={submitUrl} className="flex h-10 shrink-0 items-center gap-1 border-b border-border px-1.5">
        <button
          type="button"
          onClick={() => void host.back()}
          disabled={!state.can_go_back}
          className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
          title="后退"
        >
          <ArrowLeft className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={() => void host.forward()}
          disabled={!state.can_go_forward}
          className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
          title="前进"
        >
          <ArrowRight className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={() => void host.reload()}
          disabled={!state.url}
          className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
          title="刷新"
        >
          {state.loading ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCw className="h-4 w-4" />}
        </button>
        <div className="flex h-7 min-w-0 flex-1 items-center gap-1.5 rounded-full border border-border bg-muted/50 px-2.5 focus-within:border-primary">
          <Globe2 className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <input
            value={draftUrl}
            onChange={(e) => setDraftUrl(e.target.value)}
            onFocus={() => (addrFocusedRef.current = true)}
            onBlur={() => (addrFocusedRef.current = false)}
            placeholder="输入网址，回车打开"
            spellCheck={false}
            className="h-full w-full min-w-0 bg-transparent text-[12px] outline-none"
          />
        </div>
        <button
          type="button"
          onClick={() => setAutoFollow((v) => !v)}
          className={`grid h-7 w-7 place-items-center rounded hover:bg-accent ${
            autoFollow ? "text-primary" : "text-muted-foreground"
          }`}
          title={autoFollow ? "自动跟随助手打开的地址（已开）" : "自动跟随助手打开的地址（已关）"}
          aria-pressed={autoFollow}
        >
          <Sparkles className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={togglePicker}
          disabled={!state.url}
          className={`grid h-7 w-7 place-items-center rounded hover:bg-accent disabled:opacity-30 ${
            pickerActive ? "bg-primary/15 text-primary" : "text-muted-foreground"
          }`}
          title="选取页面元素标注"
        >
          <MousePointerSquareDashed className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={openExternal}
          disabled={!state.url}
          className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
          title="在系统浏览器打开"
        >
          <ExternalLink className="h-4 w-4" />
        </button>
      </form>

      {detectedUrls.length > 0 ? (
        <div className="flex shrink-0 gap-1.5 overflow-x-auto border-b border-border/60 px-2 py-1.5">
          {detectedUrls.map((url) => (
            <button
              key={url}
              type="button"
              onClick={() => loadUrl(url, "user")}
              className="shrink-0 rounded-full border border-border bg-muted/50 px-2.5 py-0.5 text-[11px] text-muted-foreground hover:border-primary hover:text-foreground"
              title={url}
            >
              {formatPreviewUrlLabel(url)}
            </button>
          ))}
        </div>
      ) : null}

      {/* 占位区：原生子 webview 浮在它上面。空态给引导。 */}
      <div ref={viewportRef} className="relative min-h-0 flex-1 bg-muted/20">
        {!state.url && (
          <div className="pointer-events-none absolute inset-0 grid place-items-center px-4 text-center">
            <div>
              <Globe2 className="mx-auto h-10 w-10 text-muted-foreground/40" />
              <div className="mt-3 text-[13px] font-medium text-foreground">内置浏览器</div>
              <div className="mt-1 text-[12px] leading-5 text-muted-foreground">
                输入网址，或让助手启动开发服务器后自动打开预览
              </div>
            </div>
          </div>
        )}
      </div>

      {selected && (
        <AnnotationCard
          snapshot={selected.snapshot}
          anchor={selected.anchor}
          onClose={() => setSelected(null)}
        />
      )}
    </div>
  );
}

function currentBounds(el: HTMLElement | null) {
  if (!el) return { x: 0, y: 0, width: 1, height: 1 };
  const r = el.getBoundingClientRect();
  return { x: r.left, y: r.top, width: r.width, height: r.height };
}
