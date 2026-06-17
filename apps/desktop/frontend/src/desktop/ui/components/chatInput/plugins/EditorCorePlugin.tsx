import { useEffect, useRef } from "react";
import { useLexicalComposerContext } from "@lexical/react/LexicalComposerContext";
import {
  COMMAND_PRIORITY_LOW,
  COMPOSITION_END_COMMAND,
  KEY_ARROW_DOWN_COMMAND,
  KEY_ARROW_UP_COMMAND,
  KEY_ENTER_COMMAND,
} from "lexical";
import { shouldSubmitChatInput } from "@/desktop/ui/components/chatInputKeyboard";

export interface EditorCoreHandlers {
  /**
   * Enter 语义裁决。返回 true=已消费（发送/入队，阻止默认换行）；
   * false=不消费（让编辑器插入换行）。Shift/Cmd+Enter 也走这里，由它按 streaming/队列状态决定。
   */
  onEnter: (event: KeyboardEvent) => boolean;
  /** 上/下方向键历史导航；返回 true 表示已消费（有草稿内容时返回 false 让光标正常移动）。 */
  onArrow: (direction: "older" | "newer") => boolean;
}

/**
 * 把 ChatInput 的键盘语义接到 Lexical 命令系统（COMMAND_PRIORITY_LOW）：
 * - 纯 Enter（无 Shift/Cmd）先过 IME 防误触：组字中 / keyCode 229 / compositionend 宽限窗内，
 *   这个 Enter 是输入法确认键 → 吞掉（既不发送也不插换行）。否则交给 onEnter。
 * - Shift/Cmd+Enter 是显式意图，跳过 IME 防护直接交 onEnter（它返回 false 时由编辑器插换行）。
 * - 上/下方向键 → 交给 onArrow，由历史草稿逻辑决定是否消费。
 *
 * Lexical 在组字途中（editor.isComposing()）本就不派发 KEY_ENTER_COMMAND，但 compositionend
 * 紧邻的那个确认 Enter 会漏过；shouldSubmitChatInput 的三道防线（isComposing / keyCode 229 /
 * lastCompositionEndAt 宽限窗）补齐这层边界，与原 textarea 版一致。lastCompositionEndAt 由
 * COMPOSITION_END_COMMAND 维护。typeahead 菜单用 COMMAND_PRIORITY_HIGH 注册同类命令、菜单打开时
 * 先消费，故这里无需感知菜单状态。
 */
export function EditorCorePlugin({
  handlers,
}: {
  handlers: EditorCoreHandlers;
}) {
  const [editor] = useLexicalComposerContext();
  const lastCompositionEndAtRef = useRef(0);

  useEffect(() => {
    const cleanups = [
      editor.registerCommand<CompositionEvent>(
        COMPOSITION_END_COMMAND,
        (event) => {
          lastCompositionEndAtRef.current =
            event?.timeStamp ?? performance.now();
          return false;
        },
        COMMAND_PRIORITY_LOW
      ),
      editor.registerCommand<KeyboardEvent>(
        KEY_ENTER_COMMAND,
        (event) => {
          if (!event) return false;
          const isPlainEnter = !event.shiftKey && !event.metaKey;
          if (
            isPlainEnter &&
            !shouldSubmitChatInput(
              {
                key: event.key,
                shiftKey: event.shiftKey,
                isComposing: event.isComposing,
                keyCode: event.keyCode,
                timeStamp: event.timeStamp,
              },
              {
                isComposing: editor.isComposing(),
                lastCompositionEndAt: lastCompositionEndAtRef.current,
              }
            )
          ) {
            // 输入法确认键：吞掉，既不发送也不插换行。
            event.preventDefault();
            return true;
          }
          if (handlers.onEnter(event)) {
            event.preventDefault();
            return true;
          }
          return false;
        },
        COMMAND_PRIORITY_LOW
      ),
      editor.registerCommand<KeyboardEvent>(
        KEY_ARROW_UP_COMMAND,
        (event) => {
          if (handlers.onArrow("older")) {
            event.preventDefault();
            return true;
          }
          return false;
        },
        COMMAND_PRIORITY_LOW
      ),
      editor.registerCommand<KeyboardEvent>(
        KEY_ARROW_DOWN_COMMAND,
        (event) => {
          if (handlers.onArrow("newer")) {
            event.preventDefault();
            return true;
          }
          return false;
        },
        COMMAND_PRIORITY_LOW
      ),
    ];
    return () => cleanups.forEach((fn) => fn());
  }, [editor, handlers]);

  return null;
}


