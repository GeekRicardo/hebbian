import {
  $createLineBreakNode,
  $createParagraphNode,
  $createTextNode,
  $getRoot,
  $getSelection,
  $isParagraphNode,
  $isRangeSelection,
  type ParagraphNode,
} from "lexical";
import { $createReferenceNode, type ReferencePayload } from "./ReferenceNode";

/** 把多行文本逐行 append 进段落（行间用 LineBreakNode），与 textarea 多行语义对齐。 */
function appendLinesToParagraph(paragraph: ParagraphNode, text: string) {
  text.split("\n").forEach((line, i) => {
    if (i > 0) paragraph.append($createLineBreakNode());
    if (line) paragraph.append($createTextNode(line));
  });
}

/** 用纯文本整体替换编辑器内容（历史导航 / 命令补全 / 草稿回填），并把光标移到末尾。 */
export function $setPlainText(text: string) {
  const root = $getRoot();
  root.clear();
  const paragraph = $createParagraphNode();
  appendLinesToParagraph(paragraph, text);
  root.append(paragraph);
  paragraph.selectEnd();
}

/** 在末尾追加一段文本（composerDraft「放回输入框」按已有内容换行追加）。 */
export function $appendText(text: string) {
  const root = $getRoot();
  const last = root.getLastChild();
  const paragraph = $isParagraphNode(last) ? last : $createParagraphNode();
  if (!paragraph.getParent()) root.append(paragraph);
  const merged = (root.getTextContent() ? "\n" : "") + text;
  appendLinesToParagraph(paragraph, merged);
  paragraph.selectEnd();
}

/** 在光标处插入一个引用 chip，后补一个空格避免与相邻文本粘连。 */
export function $insertReference(payload: ReferencePayload) {
  let selection = $getSelection();
  if (!$isRangeSelection(selection)) {
    const root = $getRoot();
    const last = root.getLastChild();
    const paragraph = $isParagraphNode(last) ? last : $createParagraphNode();
    if (!paragraph.getParent()) root.append(paragraph);
    paragraph.selectEnd();
    selection = $getSelection();
  }
  if ($isRangeSelection(selection)) {
    selection.insertNodes([$createReferenceNode(payload), $createTextNode(" ")]);
  }
}

