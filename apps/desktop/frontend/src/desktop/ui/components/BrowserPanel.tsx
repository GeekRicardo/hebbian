import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import {
  ArrowLeft,
  ArrowRight,
  ExternalLink,
  Globe2,
  Loader2,
  MousePointerSquareDashed,
  PictureInPicture2,
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

/**
 * 内置浏览器（架构 §4 / §8.5）——RightSidebar 的一个 tab。
 *
 * 承载是原生子 webview（浮在 viewportRef 占位区上方）。本组件负责：地址栏/导航工具栏、
 * 占位区 bounds 同步、选取按钮、弹出独立窗口。注释卡片由注入页面的 inspector.js 在页面内
 * 渲染（embedded 与 popout 共用），提交经上行通道 → App 级监听 → 发进对话。
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
  const [pickerActive, setPickerActive] = useState(false);
  const [autoFollow, setAutoFollow] = useState(true);
  // 已弹出到独立窗口：内嵌 webview 让位，显示占位
  const [poppedOut, setPoppedOut] = useState(false);
  const poppedOutRef = useRef(false);
  poppedOutRef.current = poppedOut;

  // 元素对话的上下文：把当前对话 + provider/model + 可选模型列表喂给后端
  // （旁支会话建会话 / 卡片模型选择器 / 提交总结要用）
  const providers = useStore((s) => s.providersFile.providers);
  const modelOptions = useMemo(
    () =>
      providers
        .filter((p) => p.enabled !== false)
        .flatMap((p) =>
          (p.models ?? []).map((m) => ({ providerId: p.id, model: m, label: `${m} · ${p.name}` }))
        ),
    [providers]
  );
  // 把当前对话绑定给浏览器——注释/队列/旁支结论都提交回这个对话，不随之后切到的别的对话变（否则会串）。
  // 绑定时机：首次显示浏览器 tab + 用户/agent 每次导航浏览器（见 loadUrl）——即「最后操作浏览器的对话」。
  const bindContext = useCallback(() => {
    const s = useStore.getState().currentSession;
    if (s?.id && s.provider_id && s.model) {
      void host.setContext(s.id, s.provider_id, s.model, modelOptions).catch(() => undefined);
    }
  }, [host, modelOptions]);
  const boundOnceRef = useRef(false);
  useEffect(() => {
    if (active && !boundOnceRef.current) {
      boundOnceRef.current = true;
      bindContext();
    }
  }, [active, bindContext]);

  // 聊天流里检测到的本地 dev server 地址（架构 §4.2）。
  const messages = useStore((s) => s.currentSession?.messages);
  const sources = useMemo(() => messagesToDetectSources(messages ?? []), [messages]);
  const detectedUrls = useMemo(() => extractPreviewUrls(sources, "card"), [sources]);
  const autoOpenUrl = useMemo(() => extractPreviewUrls(sources, "autoOpen")[0] ?? null, [sources]);

  // 占位区 → 子 webview bounds 同步。active=false（隐藏 tab）时不同步，避免把 webview
  // 定位到 0×0 或别的 tab 区域。
  const syncBounds = useCallback(() => {
    if (!activeRef.current || poppedOutRef.current) return;
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
    track(host.onPickerOff(() => setPickerActive(false)));
    track(
      host.onEscaped((info) => {
        toast.warning(info.reason || "该地址无法打开");
      })
    );
    track(host.onPopout((open) => setPoppedOut(open)));

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
    // 可见 = 当前 tab 且未弹出到独立窗口
    if (active && !poppedOut) {
      void host.setVisible(true);
      // 等 DOM 完成布局再取 rect（hidden→显示这一帧 rect 才有效）
      const raf = requestAnimationFrame(() => syncBounds());
      return () => cancelAnimationFrame(raf);
    }
    void host.setVisible(false);
    void host.clearSelection(); // 切走 / 弹出时收起页面内注释卡片
    return undefined;
  }, [active, poppedOut, host, syncBounds]);

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
    bindContext(); // 导航浏览器即把当前对话绑定为提交目标
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
    void host.setPicker(next);
  };

  const openExternal = () => {
    if (state.url) void openUrl(state.url).catch(() => undefined);
  };

  const popout = () => {
    // 空浏览器也允许弹出——popout 自带地址栏，可在新窗口里输网址
    void host.popout().catch((err) => toast.error(String(err)));
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
          className={`grid h-7 w-7 place-items-center rounded transition-transform hover:bg-accent active:scale-90 disabled:opacity-30 ${
            pickerActive ? "bg-primary/20 text-primary ring-1 ring-primary/40" : "text-muted-foreground"
          }`}
          title={pickerActive ? "选取中…点页面元素，或再点这里退出" : "选取页面元素标注"}
        >
          <MousePointerSquareDashed className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={popout}
          className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-accent"
          title="弹出独立窗口（可缩放测样式，同样能标注）"
        >
          <PictureInPicture2 className="h-4 w-4" />
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
        {poppedOut ? (
          <div className="absolute inset-0 grid place-items-center px-4 text-center">
            <div>
              <PictureInPicture2 className="mx-auto h-10 w-10 text-muted-foreground/50" />
              <div className="mt-3 text-[13px] font-medium text-foreground">已在新窗口打开</div>
              <div className="mt-1 text-[12px] leading-5 text-muted-foreground">
                页面在独立窗口里，可缩放测样式、选元素标注
              </div>
              <button
                type="button"
                onClick={() => void host.closePopout()}
                className="mt-4 inline-flex h-7 items-center rounded-md bg-primary px-3 text-[12px] font-medium text-primary-foreground"
              >
                收回到这里
              </button>
            </div>
          </div>
        ) : (
          !state.url && (
            <div className="pointer-events-none absolute inset-0 grid place-items-center px-4 text-center">
              <div>
                <Globe2 className="mx-auto h-10 w-10 text-muted-foreground/40" />
                <div className="mt-3 text-[13px] font-medium text-foreground">内置浏览器</div>
                <div className="mt-1 text-[12px] leading-5 text-muted-foreground">
                  输入网址，或让助手启动开发服务器后自动打开预览
                </div>
              </div>
            </div>
          )
        )}
      </div>
    </div>
  );
}

function currentBounds(el: HTMLElement | null) {
  if (!el) return { x: 0, y: 0, width: 1, height: 1 };
  const r = el.getBoundingClientRect();
  return { x: r.left, y: r.top, width: r.width, height: r.height };
}
