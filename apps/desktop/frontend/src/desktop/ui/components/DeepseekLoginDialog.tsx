import { useState } from "react";
import { Loader2, LogIn, Mail, Smartphone } from "lucide-react";
import { toast } from "sonner";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label, SecretInput } from "@/desktop/ui/components/ui/input";
import { api, type DeepseekLoginToken } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";

type LoginKind = "email" | "mobile";

interface Props {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  /** 登录成功后回调；token 写回 provider.api_key，login 作为账号显示。 */
  onSuccess: (result: DeepseekLoginToken) => void;
}

export function DeepseekLoginDialog({ open, onOpenChange, onSuccess }: Props) {
  const [kind, setKind] = useState<LoginKind>("email");
  const [email, setEmail] = useState("");
  const [mobile, setMobile] = useState("");
  const [areaCode, setAreaCode] = useState("+86");
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(false);

  function reset() {
    setEmail("");
    setMobile("");
    setAreaCode("+86");
    setPassword("");
  }

  async function handleLogin() {
    if (!password.trim()) {
      toast.error("请填写密码");
      return;
    }
    if (kind === "email" && !email.trim()) {
      toast.error("请填写邮箱");
      return;
    }
    if (kind === "mobile" && !mobile.trim()) {
      toast.error("请填写手机号");
      return;
    }

    setLoading(true);
    try {
      const result = await api.deepseekLogin({
        email: kind === "email" ? email.trim() : null,
        mobile: kind === "mobile" ? mobile.trim() : null,
        area_code: kind === "mobile" ? areaCode.trim() : null,
        password,
      });
      onSuccess(result);
      toast.success(`已登录：${result.login}`);
      reset();
      onOpenChange(false);
    } catch (e: any) {
      toast.error(e?.message || String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        if (!loading) onOpenChange(v);
      }}
      title="用 DeepSeek 账号登录"
      description="使用 chat.deepseek.com 的账号密码登录，成功后 token 自动写入 API Key 字段。"
      size="md"
      footer={
        <>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={loading}
          >
            取消
          </Button>
          <Button onClick={handleLogin} disabled={loading}>
            {loading ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin mr-1" />
            ) : (
              <LogIn className="w-3.5 h-3.5 mr-1" />
            )}
            登录
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="flex gap-2">
          <button
            type="button"
            onClick={() => setKind("email")}
            className={cn(
              "flex-1 inline-flex items-center justify-center gap-1.5 rounded-md border px-3 py-1.5 text-sm",
              kind === "email"
                ? "border-primary bg-primary/10 text-primary"
                : "border-border text-muted-foreground hover:bg-accent"
            )}
          >
            <Mail className="w-3.5 h-3.5" /> 邮箱
          </button>
          <button
            type="button"
            onClick={() => setKind("mobile")}
            className={cn(
              "flex-1 inline-flex items-center justify-center gap-1.5 rounded-md border px-3 py-1.5 text-sm",
              kind === "mobile"
                ? "border-primary bg-primary/10 text-primary"
                : "border-border text-muted-foreground hover:bg-accent"
            )}
          >
            <Smartphone className="w-3.5 h-3.5" /> 手机号
          </button>
        </div>

        {kind === "email" ? (
          <div className="space-y-1.5">
            <Label>邮箱</Label>
            <Input
              value={email}
              spellCheck={false}
              autoCorrect="off"
              autoCapitalize="off"
              placeholder="you@example.com"
              onChange={(e) => setEmail(e.target.value)}
            />
          </div>
        ) : (
          <div className="grid grid-cols-[80px_1fr] gap-2">
            <div className="space-y-1.5">
              <Label>区号</Label>
              <Input
                value={areaCode}
                onChange={(e) => setAreaCode(e.target.value)}
                spellCheck={false}
                autoCorrect="off"
                placeholder="+86"
              />
            </div>
            <div className="space-y-1.5">
              <Label>手机号</Label>
              <Input
                value={mobile}
                spellCheck={false}
                autoCorrect="off"
                inputMode="numeric"
                placeholder="13xxxxxxxxx"
                onChange={(e) => setMobile(e.target.value)}
              />
            </div>
          </div>
        )}

        <div className="space-y-1.5">
          <Label>密码</Label>
          <SecretInput
            value={password}
            spellCheck={false}
            autoCorrect="off"
            placeholder="DeepSeek 账号密码"
            onChange={(e) => setPassword(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleLogin();
            }}
          />
        </div>

        <p className="text-[11px] text-muted-foreground leading-relaxed">
          凭据仅会发送给 chat.deepseek.com 官方端点，不会上传到任何第三方。
          登录后获得的 token 会写入当前 provider 的 API Key。
        </p>
      </div>
    </Dialog>
  );
}
