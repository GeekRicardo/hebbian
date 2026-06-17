import { useCallback, useEffect, useState } from "react";
import { useLexicalComposerContext } from "@lexical/react/LexicalComposerContext";
import {
  $getSelection,
  $isRangeSelection,
  $isTextNode,
  COMMAND_PRIORITY_HIGH,
  KEY_ARROW_DOWN_COMMAND,
  KEY_ARROW_UP_COMMAND,
  KEY_ENTER_COMMAND,
  KEY_ESCAPE_COMMAND,
} from "lexical";
import {
  ConversationRefPopup,
  type ConversationItem,
} from "@/desktop/ui/components/ConversationRefPopup";
import { $createReferenceNode } from "../ReferenceNode";
import { useTextBeforeCursor } from "./useTriggerQuery";

/**
 * `@` 对话引用：光标前出现 `@` 且其前是空白/行首、`@` 后无空格时弹出对话列表，
 * 与原 textarea 版触发判定一致。选中后把 `@query` 文本替换成一个 conversation chip。
 *
 * 也支持从 + 菜单主动打开（fromMenu=true，不依赖 `@` 触发，选中后在光标处插 chip）。
 */
export function MentionMenuPlugin({
  fromMenu,
  onClose,
}: {
  fromMenu: boolean;
  onClose: () => void;
}) {
  const [editor] = useLexicalComposerContext();
  const before = useTextBeforeCursor();
  const [activeIdx, setActiveIdx] = useState(0);
  // Escape 临时关闭自动触发的弹窗，直到 `@` 上下文变化再重新打开。
  const [dismissed, setDismissed] = useState(false);

  // `@` 自动触发：光标前最后一个 `@`，其前为空白或行首，且 `@` 后无空白。
  const autoQuery = (() => {
    const atPos = before.lastIndexOf("@");
    if (atPos === -1) return null;
    const charBeforeAt = atPos > 0 ? before[atPos - 1] : " ";
    if (!/\s/.test(charBeforeAt) && atPos !== 0) return null;
    const afterAt = before.slice(atPos + 1);
    if (/\s/.test(afterAt)) return null;
    return afterAt;
  })();

  useEffect(() => {
    setDismissed(false);
  }, [autoQuery]);

  const open = fromMenu || (autoQuery !== null && !dismissed);
  const query = fromMenu ? "" : (autoQuery ?? "");

  useEffect(() => {
    setActiveIdx(0);
  }, [query, fromMenu]);

  const pick = useCallback(
    (item: ConversationItem) => {
      editor.update(() => {
        const selection = $getSelection();
        if (!$isRangeSelection(selection)) return;
        if (!fromMenu) {
          // 删除已输入的 `@query`：从锚点向前回删 query.length + 1 个字符。
          const node = selection.anchor.getNode();
          if ($isTextNode(node)) {
            const offset = selection.anchor.offset;
            const removeLen = (autoQuery ?? "").length + 1;
            const start = Math.max(0, offset - removeLen);
            const text = node.getTextContent();
            const nextText = text.slice(0, start) + text.slice(offset);
            node.setTextContent(nextText);
            node.select(start, start);
          }
        }
        const sel = $getSelection();
        if ($isRangeSelection(sel)) {
          sel.insertNodes([
            $createReferenceNode({ path: item.path, kind: "conversation" }),
          ]);
        }
      });
      onClose();
    },
    [editor, fromMenu, autoQuery, onClose]
  );

  useEffect(() => {
    if (!open) return;
    const cleanups = [
      editor.registerCommand(
        KEY_ARROW_DOWN_COMMAND,
        (event) => {
          event.preventDefault();
          setActiveIdx((i) => i + 1);
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
          document.dispatchEvent(
            new CustomEvent("conversation-ref-pick-active")
          );
          return true;
        },
        COMMAND_PRIORITY_HIGH
      ),
      editor.registerCommand(
        KEY_ESCAPE_COMMAND,
        () => {
          if (fromMenu) onClose();
          else setDismissed(true);
          return true;
        },
        COMMAND_PRIORITY_HIGH
      ),
    ];
    return () => cleanups.forEach((fn) => fn());
  }, [editor, open, onClose]);

  if (!open) return null;

  return (
    <ConversationRefPopup
      query={query}
      onPick={pick}
      onClose={onClose}
      activeIndex={activeIdx}
      onActiveIndexChange={setActiveIdx}
    />
  );
}
