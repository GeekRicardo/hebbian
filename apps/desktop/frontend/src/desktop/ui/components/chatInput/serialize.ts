import {
  $getRoot,
  $isElementNode,
  $isLineBreakNode,
  $isParagraphNode,
  $isTextNode,
  type LexicalNode,
} from "lexical";
import { $isReferenceNode } from "./ReferenceNode";

export interface SerializedInput {
  /** 还原后的纯文本：chip 替换成其路径原文，段落间用 \n 衔接。 */
  text: string;
  /** 文中出现的所有引用路径（去重，保持出现顺序）。 */
  references: string[];
}

/**
 * 把当前 editor state 序列化成发送用的纯文本 + 引用路径列表。
 * 必须在 editor.read / editor.update 回调里调用（依赖 $ 系列 API）。
 *
 * 段落 → 之间补 \n；LineBreakNode → \n；TextNode → 原文；ReferenceNode → 其路径原文。
 */
export function $serializeInput(): SerializedInput {
  const refs: string[] = [];
  const seen = new Set<string>();
  const blocks: string[] = [];

  for (const child of $getRoot().getChildren()) {
    blocks.push(serializeNode(child, refs, seen));
  }

  return {
    text: blocks.join("\n").trim(),
    references: refs,
  };
}

function serializeNode(
  node: LexicalNode,
  refs: string[],
  seen: Set<string>
): string {
  if ($isReferenceNode(node)) {
    const path = node.getPath();
    if (!seen.has(path)) {
      seen.add(path);
      refs.push(path);
    }
    return path;
  }
  if ($isLineBreakNode(node)) return "\n";
  if ($isTextNode(node)) return node.getTextContent();
  if ($isParagraphNode(node) || $isElementNode(node)) {
    return node
      .getChildren()
      .map((child) => serializeNode(child, refs, seen))
      .join("");
  }
  return node.getTextContent();
}
