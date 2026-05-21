import { useState } from "react";
import { toast } from "sonner";
import { isTauri } from "@/desktop/bridge/transport";
import { openUrl as openExternalUrl } from "@tauri-apps/plugin-opener";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label } from "@/desktop/ui/components/ui/input";
import {
  Loader2,
  ExternalLink,
  Copy,
  Download,
  ArrowRight,
} from "lucide-react";
import { api } from "@/desktop/bridge/tauri";
import type {
  AuthMode,
  AuthUrlResult,
  ImportedToken,
} from "@/desktop/ui/types";

interface Props {
  open: boolean;
  mode: AuthMode | null;
  onOpenChange: (v: boolean) => void;
  onSuccess: (info: {
    api_key: string;
    refresh_token?: string;
    account_id?: string;
    token_expires_at?: number;
  }) => void;
}

type ProviderResult = {
  api_key: string;
  refresh_token?: string;
  account_id?: string;
  token_expires_at?: number;
};

const MODE_TITLE: Record<AuthMode, string> = {
  api_key: "API Key",
  oauth_codex: "OpenAI (ChatGPT / Codex)",
  oauth_claude_code: "Claude Code",
  oauth_gemini_cli: "Gemini CLI",
};

export function OAuthDialog({ open, mode, onOpenChange, onSuccess }: Props) {
  if (!open || !mode || mode === "api_key") return null;

  const title = `OAuth 登录 — ${MODE_TITLE[mode]}`;

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      description="在浏览器中完成授权后，token 将自动写入当前供应商"
      size="md"
    >
      {mode === "oauth_codex" && (
        <PkceFlow
          kind="openai"
          onDone={(info) => {
            onSuccess(info);
            onOpenChange(false);
          }}
        />
      )}
      {mode === "oauth_claude_code" && (
        <PkceFlow
          kind="claude"
          onDone={(info) => {
            onSuccess(info);
            onOpenChange(false);
          }}
        />
      )}
      {mode === "oauth_gemini_cli" && (
        <PkceFlow
          kind="gemini"
          onDone={(info) => {
            onSuccess(info);
            onOpenChange(false);
          }}
        />
      )}
    </Dialog>
  );
}

// ===================================================================
// OpenAI / Claude Code / Gemini —— PKCE 浏览器复制回调码流程
// ===================================================================
type PkceKind = "openai" | "claude" | "gemini";

function PkceFlow({
  kind,
  onDone,
}: {
  kind: PkceKind;
  onDone: (info: ProviderResult) => void;
}) {
  const [auth, setAuth] = useState<AuthUrlResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [exchanging, setExchanging] = useState(false);
  const [code, setCode] = useState("");
  const [importing, setImporting] = useState(false);

  const start = async () => {
    setError(null);
    setStarting(true);
    try {
      const res =
        kind === "openai"
          ? await api.oauthOpenAIStart()
          : kind === "claude"
          ? await api.oauthClaudeStart()
          : await api.oauthGeminiStart();
      setAuth(res);
      await openSystemBrowser(res.auth_url);
    } catch (e: any) {
      setError(e.message || String(e));
    } finally {
      setStarting(false);
    }
  };

  const exchange = async () => {
    if (!auth) return;
    const trimmed = code.trim();
    if (!trimmed) {
      toast.error("请粘贴回调地址中的 code 参数");
      return;
    }
    setExchanging(true);
    setError(null);
    try {
      const token: ImportedToken =
        kind === "openai"
          ? await api.oauthOpenAIExchange(auth.session_id, trimmed, auth.state)
          : kind === "claude"
          ? await api.oauthClaudeExchange(auth.session_id, trimmed)
          : await api.oauthGeminiExchange(auth.session_id, trimmed);
      toast.success("OAuth 登录成功");
      onDone(toProviderResult(token));
    } catch (e: any) {
      setError(e.message || String(e));
    } finally {
      setExchanging(false);
    }
  };

  const importFromCli = async () => {
    if (kind === "openai") return;
    setImporting(true);
    setError(null);
    try {
      const token =
        kind === "claude"
          ? await api.oauthClaudeCodeImport()
          : await api.oauthGeminiCliImport();
      toast.success("已从本地凭据导入");
      onDone(toProviderResult(token));
    } catch (e: any) {
      setError(e.message || String(e));
    } finally {
      setImporting(false);
    }
  };

  const redirectHint =
    kind === "openai"
      ? "授权完成后浏览器会跳转到 localhost:1455/auth/callback，完整复制地址栏里的回调 URL 粘贴回来最稳；如果只复制 code，也会使用本次登录的 state 校验。"
      : kind === "claude"
      ? "授权完成后浏览器会跳转到 platform.claude.com/oauth/code/callback，URL 中的 code 形如 `xxx#state`，完整复制粘贴即可。"
      : "授权完成后浏览器会跳转到 codeassist.google.com/authcode，页面会显示一段授权码，复制粘贴即可。";

  return (
    <div className="space-y-4">
      {error && <ErrorBox message={error} />}

      <div className="rounded-lg border border-border p-3 bg-accent/30 text-sm">
        <div className="font-medium mb-1">流程</div>
        <ol className="list-decimal pl-5 space-y-1 text-muted-foreground">
          <li>点击「打开授权页」并在浏览器中登录、同意授权</li>
          <li>{redirectHint}</li>
          <li>将 code 粘贴到下方输入框，点击「完成登录」</li>
        </ol>
      </div>

      {!auth && (
        <Button onClick={start} disabled={starting} className="w-full">
          {starting ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <ExternalLink className="w-4 h-4" />
          )}
          打开授权页
        </Button>
      )}

      {auth && (
        <>
          <div className="rounded-lg border border-border p-3 bg-muted/30">
            <div className="text-xs text-muted-foreground mb-1.5">授权链接</div>
            <div className="flex items-center gap-2">
              <code className="flex-1 text-xs font-mono text-muted-foreground truncate">
                {auth.auth_url}
              </code>
              <button
                onClick={() => {
                  navigator.clipboard
                    .writeText(auth.auth_url)
                    .then(() => toast.success("授权链接已复制"));
                }}
                className="h-7 w-7 inline-flex items-center justify-center rounded-md hover:bg-background text-muted-foreground flex-shrink-0"
                title="复制链接"
              >
                <Copy className="w-3.5 h-3.5" />
              </button>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => openSystemBrowser(auth.auth_url)}
              className="flex-1"
            >
              <ExternalLink className="w-3.5 h-3.5" />
              重新打开授权页
            </Button>
          </div>

          <div>
            <Label>授权码 (code)</Label>
            <Input
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder={
                kind === "openai"
                  ? "完整回调 URL，或 URL 中的 code"
                  : kind === "claude"
                  ? "xxxxx#state  — 或仅 code"
                  : "浏览器页面显示的 code"
              }
              className="mt-1.5 font-mono"
            />
          </div>

          <Button
            onClick={exchange}
            disabled={exchanging || !code.trim()}
            className="w-full"
          >
            {exchanging ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <ArrowRight className="w-4 h-4" />
            )}
            完成登录
          </Button>
        </>
      )}

      {kind !== "openai" && (
        <div className="pt-2 border-t border-border">
          <div className="text-xs text-muted-foreground mb-2">
            或直接从本机 CLI 凭据导入：
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={importFromCli}
            disabled={importing}
            className="w-full"
          >
            {importing ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Download className="w-4 h-4" />
            )}
            {kind === "claude"
              ? "从 ~/.claude/.credentials.json 导入"
              : "从 ~/.gemini/oauth_creds.json 导入"}
          </Button>
        </div>
      )}
    </div>
  );
}

// ===================================================================
// 工具
// ===================================================================

function toProviderResult(token: ImportedToken): ProviderResult {
  return {
    api_key: token.access_token,
    refresh_token: token.refresh_token ?? undefined,
    account_id: token.account_id ?? undefined,
    token_expires_at: token.expires_at ?? undefined,
  };
}

function ErrorBox({ message }: { message: string }) {
  return (
    <div className="rounded-md border border-destructive/40 bg-destructive/10 text-destructive text-sm p-3 whitespace-pre-wrap">
      {message}
    </div>
  );
}

async function openSystemBrowser(url: string) {
  try {
    await openExternalUrl(url);
    return;
  } catch (error) {
    if (isTauri()) {
      throw error;
    }
  }
  window.open(url, "_blank");
}
