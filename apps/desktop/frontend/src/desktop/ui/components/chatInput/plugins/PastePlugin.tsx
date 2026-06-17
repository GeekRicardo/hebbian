import { useEffect } from "react";
import { useLexicalComposerContext } from "@lexical/react/LexicalComposerContext";
import { COMMAND_PRIORITY_HIGH, PASTE_COMMAND } from "lexical";

/**
 * 粘贴处理（与原 textarea onPaste 同序）：
 * 1. 剪贴板有 File（截图 / Finder 复制）→ 交给 onFiles 上传内容，拦截默认。
 * 2. 纯文本全部由路径行组成 → 交给 onPaths 插引用 chip，拦截默认。
 * 3. 其余（普通文本 / 混合内容）→ 不拦截，Lexical 默认插入。
 */
export function PastePlugin({
  onFiles,
  onPaths,
}: {
  onFiles: (files: File[]) => void;
  onPaths: (paths: string[]) => void;
}) {
  const [editor] = useLexicalComposerContext();

  useEffect(() => {
    return editor.registerCommand(
      PASTE_COMMAND,
      (event) => {
        if (!(event instanceof ClipboardEvent) || !event.clipboardData) {
          return false;
        }
        const files = Array.from(event.clipboardData.files);
        if (files.length > 0) {
          event.preventDefault();
          onFiles(files);
          return true;
        }
        const text = event.clipboardData.getData("text/plain");
        const candidates = parsePathCandidates(text);
        const nonEmptyLines = text.split(/\r?\n/).filter((l) => l.trim()).length;
        if (candidates.length > 0 && candidates.length >= nonEmptyLines) {
          event.preventDefault();
          onPaths(candidates);
          return true;
        }
        return false;
      },
      COMMAND_PRIORITY_HIGH
    );
  }, [editor, onFiles, onPaths]);

  return null;
}

/**
 * 把粘贴文本拆成"看起来像路径"的候选。按行 trim、剥引号，只留 `/` `~/` `file://`
 * 或 Windows 盘符开头的项；普通文本一律不当路径，避免误吃用户消息。
 */
function parsePathCandidates(raw: string): string[] {
  if (!raw || raw.length > 4096) return [];
  const out: string[] = [];
  for (const line of raw.split(/\r?\n/)) {
    let s = line.trim();
    if (!s) continue;
    if (
      (s.startsWith('"') && s.endsWith('"')) ||
      (s.startsWith("'") && s.endsWith("'"))
    ) {
      s = s.slice(1, -1);
    }
    if (
      s.startsWith("/") ||
      s.startsWith("~/") ||
      s.startsWith("file://") ||
      /^[A-Za-z]:[\\/]/.test(s)
    ) {
      out.push(s);
    }
  }
  return out;
}
