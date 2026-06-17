import { useEffect, useState } from "react";
import { useLexicalComposerContext } from "@lexical/react/LexicalComposerContext";
import { $getSelection, $isRangeSelection, $isTextNode } from "lexical";

/**
 * 监听编辑器选区变化，提取「光标所在文本节点内、光标之前的纯文本」。
 * typeahead 触发检测基于它——只看当前 TextNode 的局部文本，chip 节点天然隔断触发词，
 * 与原 textarea 基于 selectionStart 取 before 的语义对齐（chip 等价于一个不可分割边界）。
 */
export function useTextBeforeCursor(): string {
  const [editor] = useLexicalComposerContext();
  const [before, setBefore] = useState("");

  useEffect(() => {
    return editor.registerUpdateListener(({ editorState }) => {
      editorState.read(() => {
        const selection = $getSelection();
        if (!$isRangeSelection(selection) || !selection.isCollapsed()) {
          setBefore("");
          return;
        }
        const node = selection.anchor.getNode();
        if (!$isTextNode(node)) {
          setBefore("");
          return;
        }
        setBefore(node.getTextContent().slice(0, selection.anchor.offset));
      });
    });
  }, [editor]);

  return before;
}
