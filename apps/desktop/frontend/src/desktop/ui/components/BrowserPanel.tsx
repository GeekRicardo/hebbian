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
import { ipcConfirm } from "@/desktop/ui/lib/utils";
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
 * 多对话多实例：每个对话一个子 webview，状态（url/标题/历史/选取/弹出/自动跟随）各存一份，
 * 按 currentSession 渲染。webview 在「输入网址打开」那一刻才在后端懒创建——没碰过浏览器的
 * 对话不占实例。切对话先 hideOthers 把别的对话的 webview 收起，再显示当前对话的那个。
 *
 * `active`：是否当前显示的 tab。常驻挂载、切走只隐藏不卸载（保住页面/登录态/滚动）。
 */
interface Inst {
  state: BrowserStateEvent;
  title: string;
  pickerActive: boolean;
  poppedOut: boolean;
  autoFollow: boolean;
  /** 本实例已自动跟随过的地址（同一地址不重复跟随） */
  followed: string | null;
  /** 已在后端创建子 webview（懒创建标记）——false 时下次导航走 open，true 走 navigate */
  opened: boolean;
  /** 页面里还没提交的注释条数（>0 时刷新/导航先确认，防丢） */
  dirtyCount: number;
}

const BLANK_STATE: BrowserStateEvent = {
  url: "",
  can_go_back: false,
  can_go_forward: false,
  loading: false,
};
const BLANK_INST: Inst = {
  state: BLANK_STATE,
  title: "",
  pickerActive: false,
  poppedOut: false,
  autoFollow: true,
  followed: null,
  opened: false,
  dirtyCount: 0,
};

export function BrowserPanel({ active, obscured = false }: { active: boolean; obscured?: boolean }) {
  const host = getBrowserHost();

  const viewportRef = useRef<HTMLDivElement | null>(null);
  const activeRef = useRef(active);
  activeRef.current = active;

  const currentSessionId = useStore((s) => s.currentSession?.id ?? null);
  const currentSessionIdRef = useRef(currentSessionId);
  currentSessionIdRef.current = currentSessionId;

  // 每个对话一份浏览器状态，按 session_id 索引；渲染当前对话的那份。
  const [insts, setInsts] = useState<Record<string, Inst>>({});
  const instsRef = useRef(insts);
  instsRef.current = insts;
  const cur = (currentSessionId ? insts[currentSessionId] : undefined) ?? BLANK_INST;

  const [draftUrl, setDraftUrl] = useState("");

  const patchInst = useCallback((sid: string, patch: Partial<Inst>) => {
    setInsts((prev) => ({ ...prev, [sid]: { ...(prev[sid] ?? BLANK_INST), ...patch } }));
  }, []);

  // 聊天流里检测到的本地 dev server 地址（架构 §4.2）。
  const messages = useStore((s) => s.currentSession?.messages);
  const sources = useMemo(() => messagesToDetectSources(messages ?? []), [messages]);
  const detectedUrls = useMemo(() => extractPreviewUrls(sources, "card"), [sources]);
  const autoOpenUrl = useMemo(() => extractPreviewUrls(sources, "autoOpen")[0] ?? null, [sources]);

  // 占位区 → 当前对话子 webview bounds 同步。隐藏 tab / 弹出 / 无实例时不同步。
  const syncBounds = useCallback(() => {
    const sid = currentSessionIdRef.current;
    if (!sid || !activeRef.current) return;
    const inst = instsRef.current[sid];
    if (!inst?.opened || inst.poppedOut) return;
    const el = viewportRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    void host.setBounds(sid, { x: r.left, y: r.top, width: r.width, height: r.height });
  }, [host]);

  // 订阅 webview 事件——回调带 session_id，落到对应实例；当前对话才弹 toast。
  useEffect(() => {
    const unlistens: Array<() => void> = [];
    let alive = true;
    const track = (p: Promise<() => void>) =>
      void p.then((fn) => {
        if (alive) unlistens.push(fn);
        else fn();
      });

    track(host.onState((sid, s) => patchInst(sid, { state: s })));
    track(
      host.onTitle((sid, t) =>
        setInsts((prev) => {
          const base = prev[sid] ?? BLANK_INST;
          return {
            ...prev,
            [sid]: {
              ...base,
              title: t.title,
              state: base.state.url === t.url ? base.state : { ...base.state, url: t.url },
            },
          };
        })
      )
    );
    track(host.onPickerOff((sid) => patchInst(sid, { pickerActive: false })));
    track(
      host.onEscaped((sid, info) => {
        if (sid === currentSessionIdRef.current) toast.warning(info.reason || "该地址无法打开");
      })
    );
    track(host.onPopout((sid, open) => patchInst(sid, { poppedOut: open })));
    track(host.onAnnotationDirty((sid, count) => patchInst(sid, { dirtyCount: count })));

    return () => {
      alive = false;
      unlistens.forEach((fn) => fn());
    };
  }, [host, patchInst]);

  // bounds 同步：占位区尺寸/位置变化时把原生子 webview 跟过去。
  //
  // 子 webview 是 Rust 侧绝对定位的独立层，不参与 CSS 布局/动画，必须显式下发坐标。
  // 难点：侧边栏折叠/自动调宽走外层 <aside> 的 500ms width 过渡，占位区被外壳裁切
  // （自身 width 固定），动画期间它的布局尺寸不变 → ResizeObserver 不触发，webview
  // 停在旧位置。解法：RO/resize 负责"边沿"（拖拽、窗口缩放、动画起止会报点），每次
  // 报点后启动一段封顶 rAF 跟随，把过渡中间帧补上；rect 连续两帧不变即停，不常驻空转。
  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    let raf = 0;
    let last = "";
    let still = 0;
    const follow = () => {
      const node = viewportRef.current;
      if (!node) return;
      const r = node.getBoundingClientRect();
      const key = `${Math.round(r.left)},${Math.round(r.top)},${Math.round(r.width)},${Math.round(r.height)}`;
      if (key === last) {
        // 连续 ~5 帧无变化 → 过渡结束，停止跟随
        if (++still > 5) return;
      } else {
        last = key;
        still = 0;
        syncBounds();
      }
      raf = requestAnimationFrame(follow);
    };
    const kick = () => {
      cancelAnimationFrame(raf);
      still = 0;
      raf = requestAnimationFrame(follow);
    };
    const ro = new ResizeObserver(kick);
    ro.observe(el);
    window.addEventListener("resize", kick);
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      window.removeEventListener("resize", kick);
    };
  }, [syncBounds]);

  // 卸载兜底：侧边栏折叠时 RightSidebar 把整个展开面板（含本组件）从 DOM 卸载，
  // 但原生子 webview 是 Rust 侧独立层、不随 React 卸载消失，会残留在屏幕上盖住其它
  // 内容。本组件 unmount 时无条件 hideOthers("") 把所有实例收起（""=不保留任何）。
  // 重新展开时 mount 回来，下面的可见性 effect 会按当前 tab/对话重新 setVisible。
  useEffect(() => {
    return () => {
      void host.hideOthers("");
    };
  }, [host]);

  // 挂载恢复：insts 是组件内 state，折叠卸载会丢，重挂载后变空 → cur.opened=false →
  // 以为"没开"不 setVisible，webview 在后端还活着却显示不出来（用户报的"内嵌打不开"）。
  // mount 时向后端查实际还开着的实例，恢复 opened + url。
  useEffect(() => {
    let alive = true;
    void host.listOpen().then((list) => {
      if (!alive || !list.length) return;
      setInsts((prev) => {
        const next = { ...prev };
        for (const [sid, url] of list) {
          const base = next[sid] ?? BLANK_INST;
          next[sid] = { ...base, opened: true, state: { ...base.state, url } };
        }
        return next;
      });
    });
    return () => {
      alive = false;
    };
  }, [host]);

  // 切对话 / 切 tab / 实例懒创建后：收起别的对话的 webview，按可见性显示当前对话的那个。
  useEffect(() => {
    if (!currentSessionId) {
      if (active) void host.hideOthers(""); // 没绑定对话——全部收起
      return;
    }
    void host.hideOthers(currentSessionId); // 切对话先把别的对话的 webview 收起
    // obscured：全屏覆盖层（Model I/O / 设置）打开时，子视图是独立 OS 层、z-order
    // 永远盖在覆盖层之上，必须显式隐藏，否则网页压在覆盖层上。覆盖层关闭后恢复。
    const visible = active && cur.opened && !cur.poppedOut && !obscured;
    void host.setVisible(currentSessionId, visible);
    if (visible) {
      // 等 DOM 完成布局再取 rect（hidden→显示这一帧 rect 才有效）
      const raf = requestAnimationFrame(() => syncBounds());
      return () => cancelAnimationFrame(raf);
    }
    if (!obscured) void host.clearSelection(currentSessionId); // 切走/弹出时收注释卡片（obscured 只是临时遮挡，不清选中）
    return undefined;
  }, [active, currentSessionId, cur.opened, cur.poppedOut, obscured, host, syncBounds]);

  // 地址栏跟随当前对话实例的 url（用户聚焦编辑时不抢）；切到没开浏览器的对话则清空。
  const addrFocusedRef = useRef(false);
  useEffect(() => {
    if (addrFocusedRef.current) return;
    setDraftUrl(cur.state.url ? formatPreviewUrlLabel(cur.state.url) : "");
  }, [cur.state.url, currentSessionId]);

  // 有未提交注释时先确认；确认后给页面发一次性放行，避免 beforeunload 再弹一次。
  const confirmDiscardAnnotations = useCallback(
    async (sid: string): Promise<boolean> => {
      const inst = instsRef.current[sid];
      if (!inst || inst.dirtyCount <= 0) return true;
      const ok = await ipcConfirm(
        `页面里还有 ${inst.dirtyCount} 条注释没提交，离开后会丢失。确定继续吗？`,
        "注释还没提交"
      );
      if (ok) {
        await host.allowUnload(sid).catch(() => {});
        patchInst(sid, { dirtyCount: 0 });
      }
      return ok;
    },
    [host, patchInst]
  );

  const loadUrl = useCallback(
    (raw: string, origin: "auto" | "user") => {
      const sid = currentSessionIdRef.current;
      if (!sid) {
        toast.error("先打开一个对话，再开浏览器");
        return;
      }
      const norm = normalizePreviewUrlInput(raw);
      if (!norm) {
        toast.error("这个地址没法打开");
        return;
      }
      void (async () => {
        if (origin === "user" && !(await confirmDiscardAnnotations(sid))) return;
        if (origin === "user") patchInst(sid, { autoFollow: false });
        const inst = instsRef.current[sid];
        if (!inst?.opened) {
          // 懒创建：这个对话第一次开浏览器，才在后端建子 webview
          patchInst(sid, { opened: true });
          void host
            .open(sid, norm, origin, currentBounds(viewportRef.current))
            .catch((err) => {
              toast.error(String(err));
              patchInst(sid, { opened: false });
            });
        } else {
          void host.navigate(sid, norm).catch((err) => toast.error(String(err)));
        }
      })();
    },
    [host, patchInst, confirmDiscardAnnotations]
  );

  // auto-follow：聊天流里冒出新的 dev server 地址且当前对话开关打开时自动跟随。
  // 用户手动输地址（loadUrl user 档）会关掉它，把控制权交还用户。
  useEffect(() => {
    if (!currentSessionId || !cur.autoFollow || !autoOpenUrl || cur.followed === autoOpenUrl) return;
    patchInst(currentSessionId, { followed: autoOpenUrl });
    loadUrl(autoOpenUrl, "auto");
  }, [currentSessionId, cur.autoFollow, cur.followed, autoOpenUrl, patchInst, loadUrl]);

  const submitUrl = (e: FormEvent) => {
    e.preventDefault();
    loadUrl(draftUrl, "user");
  };

  const togglePicker = () => {
    if (!currentSessionId) return;
    const next = !cur.pickerActive;
    patchInst(currentSessionId, { pickerActive: next });
    void host.setPicker(currentSessionId, next);
  };

  const openExternal = () => {
    if (cur.state.url) void openUrl(cur.state.url).catch(() => undefined);
  };

  const popout = () => {
    // 空浏览器也允许弹出——popout 自带地址栏，可在新窗口里输网址
    if (currentSessionId) void host.popout(currentSessionId).catch((err) => toast.error(String(err)));
  };

  return (
    <div className="flex h-full min-h-0 w-full flex-col bg-background">
      <form onSubmit={submitUrl} className="flex h-10 shrink-0 items-center gap-1 border-b border-border px-1.5">
        <button
          type="button"
          onClick={() => {
            const sid = currentSessionId;
            if (!sid) return;
            void confirmDiscardAnnotations(sid).then((ok) => {
              if (ok) void host.back(sid);
            });
          }}
          disabled={!cur.state.can_go_back}
          className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
          title="后退"
        >
          <ArrowLeft className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={() => {
            const sid = currentSessionId;
            if (!sid) return;
            void confirmDiscardAnnotations(sid).then((ok) => {
              if (ok) void host.forward(sid);
            });
          }}
          disabled={!cur.state.can_go_forward}
          className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
          title="前进"
        >
          <ArrowRight className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={() => {
            const sid = currentSessionId;
            if (!sid) return;
            void confirmDiscardAnnotations(sid).then((ok) => {
              if (ok) void host.reload(sid);
            });
          }}
          disabled={!cur.state.url}
          className="grid h-7 w-7 place-items-center rounded text-muted-foreground hover:bg-accent disabled:opacity-30"
          title="刷新"
        >
          {cur.state.loading ? <Loader2 className="h-4 w-4 animate-spin" /> : <RefreshCw className="h-4 w-4" />}
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
          onClick={() => currentSessionId && patchInst(currentSessionId, { autoFollow: !cur.autoFollow })}
          className={`grid h-7 w-7 place-items-center rounded hover:bg-accent ${
            cur.autoFollow ? "text-primary" : "text-muted-foreground"
          }`}
          title={cur.autoFollow ? "自动跟随助手打开的地址（已开）" : "自动跟随助手打开的地址（已关）"}
          aria-pressed={cur.autoFollow}
        >
          <Sparkles className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={togglePicker}
          disabled={!cur.state.url}
          className={`grid h-7 w-7 place-items-center rounded transition-transform hover:bg-accent active:scale-90 disabled:opacity-30 ${
            cur.pickerActive ? "bg-primary/20 text-primary ring-1 ring-primary/40" : "text-muted-foreground"
          }`}
          title={cur.pickerActive ? "选取中…点页面元素，或再点这里退出" : "选取页面元素标注"}
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
          disabled={!cur.state.url}
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
        {cur.poppedOut ? (
          <div className="absolute inset-0 grid place-items-center px-4 text-center">
            <div>
              <PictureInPicture2 className="mx-auto h-10 w-10 text-muted-foreground/50" />
              <div className="mt-3 text-[13px] font-medium text-foreground">已在新窗口打开</div>
              <div className="mt-1 text-[12px] leading-5 text-muted-foreground">
                页面在独立窗口里，可缩放测样式、选元素标注
              </div>
              <button
                type="button"
                onClick={() => currentSessionId && void host.closePopout(currentSessionId)}
                className="mt-4 inline-flex h-7 items-center rounded-md bg-primary px-3 text-[12px] font-medium text-primary-foreground"
              >
                收回到这里
              </button>
            </div>
          </div>
        ) : (
          !cur.state.url && (
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
