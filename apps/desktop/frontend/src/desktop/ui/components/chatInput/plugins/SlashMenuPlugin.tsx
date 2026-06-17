import { useEffect, useMemo, useRef, useState } from "react";
import { useLexicalComposerContext } from "@lexical/react/LexicalComposerContext";
import {
  $getRoot,
  COMMAND_PRIORITY_HIGH,
  KEY_ARROW_DOWN_COMMAND,
  KEY_ARROW_UP_COMMAND,
  KEY_ENTER_COMMAND,
  KEY_ESCAPE_COMMAND,
  KEY_TAB_COMMAND,
} from "lexical";
import { cn } from "@/desktop/ui/lib/utils";
import type { SlashCommandMeta } from "@/desktop/ui/lib/slashCommands";
import { useTextBeforeCursor } from "./useTriggerQuery";

/**
 * `//` 命令实时联想：编辑器整体文本以 `//` 开头且首 token 未输入空格时弹出，
 * 与原 textarea 版判定一致（首 token 出现空格 = 进入参数输入，联想关闭）。
 * 选中后由 onPick 写入命令模板（外层用 $setPlainText 重置编辑器内容）。
 */
export function SlashMenuPlugin({
  catalog,
  onPick,
}: {
  catalog: SlashCommandMeta[];
  onPick: (cmd: SlashCommandMeta) => void;
}) {
  const [editor] = useLexicalComposerContext();
  const before = useTextBeforeCursor();
  const [activeIdx, setActiveIdx] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  const suggestions = useMemo(() => {
    const trimmed = before.trimStart();
    if (!trimmed.startsWith("//")) return [];
    const afterSlash = trimmed.slice(2);
    if (/\s/.test(afterSlash)) return [];
    const query = afterSlash.toLowerCase();
    if (!query) return catalog;
    return catalog.filter(
      (c) =>
        c.name.toLowerCase().includes(query) ||
        c.desc.toLowerCase().includes(query)
    );
  }, [before, catalog]);

  const open = suggestions.length > 0;

  useEffect(() => {
    setActiveIdx(0);
  }, [suggestions.length]);

  useEffect(() => {
    const container = listRef.current;
    if (!container || !open) return;
    const active = container.children[activeIdx] as HTMLElement | undefined;
    active?.scrollIntoView({ block: "nearest" });
  }, [activeIdx, open]);

  useEffect(() => {
    if (!open) return;
    const pick = () => {
      const cmd = suggestions[activeIdx];
      if (cmd) onPick(cmd);
      return true;
    };
    const cleanups = [
      editor.registerCommand(
        KEY_ARROW_DOWN_COMMAND,
        (event) => {
          event.preventDefault();
          setActiveIdx((i) => Math.min(i + 1, suggestions.length - 1));
          return true;
        },
        COMMAND_PRIORITY_HIGH
      ),
      editor.registerCommand(
        KEY_ARROW_UP_COMMAND,
        (event) => {
          event.preventDefault();
          setActiveIdx((i) => Math.max(i - 1, 0));
          return true;
        },
        COMMAND_PRIORITY_HIGH
      ),
      editor.registerCommand<KeyboardEvent>(
        KEY_ENTER_COMMAND,
        (event) => {
          if (event?.shiftKey) return false;
          event?.preventDefault();
          return pick();
        },
        COMMAND_PRIORITY_HIGH
      ),
      editor.registerCommand<KeyboardEvent>(
        KEY_TAB_COMMAND,
        (event) => {
          event.preventDefault();
          return pick();
        },
        COMMAND_PRIORITY_HIGH
      ),
      editor.registerCommand(
        KEY_ESCAPE_COMMAND,
        () => {
          // 与原行为对齐：Esc 清空 `//` 草稿。
          editor.update(() => {
            $getRoot().clear().selectEnd();
          });
          setActiveIdx(0);
          return true;
        },
        COMMAND_PRIORITY_HIGH
      ),
    ];
    return () => cleanups.forEach((fn) => fn());
  }, [editor, open, suggestions, activeIdx, onPick]);

  if (!open) return null;

  return (
    <div
      ref={listRef}
      className="absolute bottom-full left-0 right-0 mb-1 max-h-[40vh] overflow-y-auto rounded-lg border border-border bg-card shadow-lg z-[100]"
    >
      {suggestions.map((cmd, i) => (
        <button
          key={`${cmd.kind}:${cmd.name}`}
          type="button"
          onMouseDown={(e) => {
            e.preventDefault();
            onPick(cmd);
          }}
          onMouseEnter={() => setActiveIdx(i)}
          className={cn(
            "w-full flex items-center justify-between gap-3 px-3 py-1.5 text-sm text-left border-l-2 transition-colors",
            i === activeIdx
              ? "bg-primary/10 border-l-primary"
              : "hover:bg-accent/50 border-l-transparent"
          )}
        >
          <div className="flex flex-col min-w-0 flex-1">
            <span className="font-mono text-foreground truncate">
              //{cmd.name}
              {cmd.args && (
                <span className="text-muted-foreground ml-1">{cmd.args}</span>
              )}
            </span>
            {cmd.desc && (
              <span className="text-[11px] text-muted-foreground truncate">
                {cmd.desc}
              </span>
            )}
          </div>
          {cmd.kind === "skill" && cmd.skillSource && (
            <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
              {cmd.skillSource}
            </span>
          )}
        </button>
      ))}
    </div>
  );
}
