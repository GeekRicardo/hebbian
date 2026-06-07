import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/desktop/ui/components/ui/button";
import { Label, Textarea } from "@/desktop/ui/components/ui/input";
import { api } from "@/desktop/bridge/tauri";

/**
 * Hooks 设置面板（架构 §4.8）。
 *
 * 用 JSON 编辑器直接展示和编辑 `~/.hebbian/hooks.json`。
 * hooks 配置结构（事件 → 规则数组）天然适合 JSON 编辑：
 *
 * ```json
 * {
 *   "PreToolUse": [
 *     { "matcher": { "tool": "Bash" }, "command": "python guard.py" }
 *   ],
 *   "Stop": [
 *     { "command": "cargo check 2>&1 | tail -50", "mode": "sync", "timeout_secs": 60 }
 *   ]
 * }
 * ```
 */
export function HooksPane() {
  const [raw, setRaw] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const text = await api.getHooksRaw();
      // 格式化展示
      try {
        const parsed = JSON.parse(text);
        setRaw(JSON.stringify(parsed, null, 2));
      } catch {
        setRaw(text);
      }
      setDirty(false);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  function handleChange(value: string) {
    setRaw(value);
    setDirty(true);
  }

  async function handleSave() {
    // 前端先校验 JSON
    try {
      JSON.parse(raw);
    } catch (e: any) {
      toast.error(`JSON 格式错误：${e.message}`);
      return;
    }
    setSaving(true);
    try {
      await api.saveHooksRaw(raw);
      toast.success("Hooks 已保存");
      setDirty(false);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="space-y-4">
      <div>
        <Label>全局 Hooks 配置</Label>
        <p className="text-xs text-muted-foreground mt-1">
          编辑 <code className="text-[11px]">~/.hebbian/hooks.json</code>。
          每个事件点位（如 PreToolUse、Stop）对应一组规则。
          新 session 启动时自动加载。
        </p>
      </div>

      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">
            {loading ? "加载中…" : dirty ? "有未保存的修改" : "已同步"}
          </span>
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="sm" onClick={reload} disabled={loading}>
              刷新
            </Button>
            <Button size="sm" onClick={handleSave} disabled={saving || !dirty}>
              {saving ? "保存中…" : "保存"}
            </Button>
          </div>
        </div>
        <Textarea
          value={raw}
          onChange={(e) => handleChange(e.target.value)}
          rows={20}
          className="font-mono text-xs leading-relaxed"
          placeholder='{\n  "Stop": [\n    { "command": "cargo check 2>&1 | tail -50", "mode": "sync", "timeout_secs": 60 }\n  ]\n}'
          spellCheck={false}
        />
      </div>

      <div className="space-y-2 text-xs text-muted-foreground">
        <p className="font-medium text-foreground">可用事件点位</p>
        <div className="grid grid-cols-2 gap-x-4 gap-y-1">
          <span><code>SessionStart</code> — 会话开始</span>
          <span><code>SessionEnd</code> — 会话结束</span>
          <span><code>UserPromptSubmit</code> — 用户提交消息</span>
          <span><code>PreToolUse</code> — 工具调用前</span>
          <span><code>PostToolUse</code> — 工具调用后</span>
          <span><code>PostToolUseFailure</code> — 工具调用失败</span>
          <span><code>PermissionRequest</code> — 权限请求</span>
          <span><code>PreCompact</code> — 压缩前</span>
          <span><code>PostCompact</code> — 压缩后</span>
          <span><code>Notification</code> — 通知</span>
          <span><code>Stop</code> — 模型回复结束</span>
        </div>
        <p className="mt-2 font-medium text-foreground">规则字段</p>
        <div className="space-y-0.5">
          <span><code>command</code> — 要执行的 shell 命令</span><br/>
          <span><code>matcher</code> — 可选，<code>{`{ "tool": "Bash" }`}</code> 按工具名过滤</span><br/>
          <span><code>mode</code> — <code>"sync"</code>（默认）或 <code>"async"</code>（fire-and-forget）</span><br/>
          <span><code>timeout_secs</code> — 超时秒数（默认 5，Stop 建议 30-60）</span>
        </div>
      </div>
    </div>
  );
}
