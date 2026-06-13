import { useCallback, useEffect, useRef, useState } from "react";
import { Loader2, LogIn, Power, RefreshCw, Smartphone } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/desktop/ui/components/ui/button";
import { api, type WeChatStatus } from "@/desktop/bridge/tauri";

type Phase = "idle" | "loading_qr" | "waiting" | "scanned";

export function WeChatPane({ active }: { active: boolean }) {
  const [status, setStatus] = useState<WeChatStatus | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [qrSvg, setQrSvg] = useState<string | null>(null);
  const pollTimer = useRef<number | null>(null);

  const clearPoll = useCallback(() => {
    if (pollTimer.current !== null) {
      window.clearTimeout(pollTimer.current);
      pollTimer.current = null;
    }
  }, []);

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await api.wechatStatus());
    } catch (e: any) {
      toast.error(`读取微信状态失败：${e?.message ?? e}`);
    }
  }, []);

  useEffect(() => {
    if (active) void refreshStatus();
  }, [active, refreshStatus]);

  useEffect(() => () => clearPoll(), [clearPoll]);

  const pollOnce = useCallback(
    async (qrcodeId: string) => {
      try {
        const result = await api.wechatLoginPoll(qrcodeId);
        switch (result.status) {
          case "waiting":
            pollTimer.current = window.setTimeout(() => void pollOnce(qrcodeId), 2000);
            break;
          case "scanned":
            setPhase("scanned");
            pollTimer.current = window.setTimeout(() => void pollOnce(qrcodeId), 2000);
            break;
          case "confirmed":
            setPhase("idle");
            setQrSvg(null);
            toast.success("微信已登录，开始接收消息");
            void refreshStatus();
            break;
          case "expired":
            setPhase("idle");
            setQrSvg(null);
            toast.error("二维码已过期，请重新获取");
            break;
        }
      } catch (e: any) {
        setPhase("idle");
        setQrSvg(null);
        toast.error(`扫码登录失败：${e?.message ?? e}`);
      }
    },
    [refreshStatus],
  );

  const startLogin = useCallback(async () => {
    clearPoll();
    setPhase("loading_qr");
    setQrSvg(null);
    try {
      const { svg, qrcode_id } = await api.wechatLoginStart();
      setQrSvg(svg);
      setPhase("waiting");
      pollTimer.current = window.setTimeout(() => void pollOnce(qrcode_id), 2000);
    } catch (e: any) {
      setPhase("idle");
      toast.error(`获取二维码失败：${e?.message ?? e}`);
    }
  }, [clearPoll, pollOnce]);

  const toggleRunning = useCallback(async () => {
    if (!status) return;
    try {
      if (status.running) {
        await api.wechatStop();
        toast.success("已停止微信渠道");
      } else if (status.bot_id) {
        await api.wechatStart(status.bot_id);
        toast.success("已启动微信渠道");
      }
      void refreshStatus();
    } catch (e: any) {
      toast.error(`操作失败：${e?.message ?? e}`);
    }
  }, [status, refreshStatus]);

  const loggedIn = status?.logged_in ?? false;
  const running = status?.running ?? false;

  return (
    <div className="flex flex-col gap-6">
      <header className="flex items-start justify-between gap-4">
        <div>
          <h3 className="text-sm font-medium text-zinc-100">微信</h3>
          <p className="mt-1 text-xs leading-relaxed text-zinc-400">
            扫码把微信连到 hebbian，之后在微信里发消息就能直接跟 AI 对话。
            关闭主窗口后仍在后台收发，退出 App 才会断开。
          </p>
        </div>
        <span
          className={`shrink-0 rounded-full px-2 py-0.5 text-[11px] font-medium ${
            running
              ? "bg-emerald-500/15 text-emerald-400"
              : loggedIn
                ? "bg-amber-500/15 text-amber-400"
                : "bg-zinc-700/40 text-zinc-400"
          }`}
        >
          {running ? "运行中" : loggedIn ? "已登录·未运行" : "未登录"}
        </span>
      </header>

      {loggedIn ? (
        <div className="flex items-center gap-3 rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
          <Smartphone className="h-5 w-5 text-emerald-400" />
          <div className="flex-1 text-sm text-zinc-200">
            {running ? "正在接收微信消息" : "已登录，当前未在接收"}
          </div>
          <Button variant={running ? "outline" : "default"} size="sm" onClick={toggleRunning}>
            <Power className="mr-1.5 h-3.5 w-3.5" />
            {running ? "停止" : "启动"}
          </Button>
          <Button variant="outline" size="sm" onClick={startLogin}>
            <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
            重新扫码
          </Button>
        </div>
      ) : (
        <div className="flex flex-col items-center gap-4 rounded-lg border border-zinc-800 bg-zinc-900/40 p-6">
          {phase === "idle" && (
            <Button onClick={startLogin}>
              <LogIn className="mr-1.5 h-4 w-4" />
              扫码登录微信
            </Button>
          )}
          {phase === "loading_qr" && (
            <div className="flex items-center gap-2 text-sm text-zinc-400">
              <Loader2 className="h-4 w-4 animate-spin" />
              正在获取二维码…
            </div>
          )}
          {(phase === "waiting" || phase === "scanned") && qrSvg && (
            <>
              <div
                className="h-[220px] w-[220px] overflow-hidden rounded-lg bg-white p-2 [&>svg]:h-full [&>svg]:w-full"
                dangerouslySetInnerHTML={{ __html: qrSvg }}
              />
              <p className="text-sm text-zinc-300">
                {phase === "scanned" ? "已扫码，请在手机上确认登录" : "请用微信扫描二维码"}
              </p>
              <Button variant="outline" size="sm" onClick={startLogin}>
                <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
                刷新二维码
              </Button>
            </>
          )}
        </div>
      )}

      <div className="rounded-lg border border-zinc-800/60 bg-zinc-900/20 p-4 text-xs leading-relaxed text-zinc-400">
        <p className="mb-1.5 font-medium text-zinc-300">在微信里能做什么</p>
        <p>直接发文字 → 跟当前对话的 AI 聊天。也能用斜杠命令：</p>
        <p className="mt-1 font-mono text-[11px] text-zinc-500">
          /new 新建对话 · /threads 切换对话 · /providers 供应商 · /models 模型 · /status 当前状态 · /help 帮助
        </p>
      </div>
    </div>
  );
}
