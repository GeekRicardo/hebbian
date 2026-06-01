import { useState } from "react";
import { toast } from "sonner";
import { Check, Copy, Loader2 } from "lucide-react";
import { Dialog } from "./ui/dialog";
import { Button } from "./ui/button";
import { api, type ClaudeResumeResult } from "@/desktop/bridge/tauri";

interface ExportClaudeDialogProps {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  sessionId: string;
}

/**
 * 把当前对话导出成一个 Claude 会话，给出可直接粘进终端恢复的命令。
 *
 * 思维链开关：带上时上下文更完整，但 Claude 那边对历史思维链有签名校验，
 * 续聊的第一条可能被拒——默认关掉，需要再开。
 */
export function ExportClaudeDialog({
  open,
  onOpenChange,
  sessionId,
}: ExportClaudeDialogProps) {
  const [includeThinking, setIncludeThinking] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [result, setResult] = useState<ClaudeResumeResult | null>(null);
  const [copied, setCopied] = useState(false);

  async function handleExport() {
    setExporting(true);
    try {
      const r = await api.exportSessionToClaude(sessionId, includeThinking);
      setResult(r);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      setExporting(false);
    }
  }

  async function handleCopy() {
    if (!result) return;
    await navigator.clipboard.writeText(result.resume_command);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  // 关掉时重置，下次打开是干净状态。
  function handleOpenChange(v: boolean) {
    if (!v) {
      setResult(null);
      setCopied(false);
    }
    onOpenChange(v);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={handleOpenChange}
      title="导出到 Claude"
      description="把这段对话变成一个 Claude 会话，在终端里继续聊，上下文照旧。"
      size="md"
    >
      {result ? (
        <div className="space-y-3">
          <p className="text-sm text-muted-foreground">
            导出好了。复制下面这行，粘到终端回车即可继续这段对话：
          </p>
          <div className="flex items-center gap-2 rounded-lg border border-border bg-muted/40 px-3 py-2">
            <code className="flex-1 select-text font-mono text-xs break-all">
              {result.resume_command}
            </code>
            <Button size="sm" variant="ghost" onClick={handleCopy}>
              {copied ? (
                <Check className="h-4 w-4 text-green-500" />
              ) : (
                <Copy className="h-4 w-4" />
              )}
            </Button>
          </div>
        </div>
      ) : (
        <label className="flex items-start gap-3 cursor-pointer select-none">
          <input
            type="checkbox"
            checked={includeThinking}
            onChange={(e) => setIncludeThinking(e.target.checked)}
            className="mt-0.5 h-4 w-4 accent-primary"
          />
          <span className="text-sm">
            连思维链一起带过去
            <span className="block text-xs text-muted-foreground mt-0.5">
              上下文更全，但终端里续聊的第一条可能报错；不确定就别勾。
            </span>
          </span>
        </label>
      )}

      <div className="mt-5 flex justify-end gap-2">
        {result ? (
          <Button variant="ghost" onClick={() => handleOpenChange(false)}>
            完成
          </Button>
        ) : (
          <>
            <Button variant="ghost" onClick={() => handleOpenChange(false)}>
              取消
            </Button>
            <Button onClick={handleExport} disabled={exporting}>
              {exporting && <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />}
              导出
            </Button>
          </>
        )}
      </div>
    </Dialog>
  );
}
