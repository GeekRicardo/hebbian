import { useCallback, useEffect, useMemo, useRef } from "react";
import { useLexicalComposerContext } from "@lexical/react/LexicalComposerContext";
import { PlainTextPlugin } from "@lexical/react/LexicalPlainTextPlugin";
import { ContentEditable } from "@lexical/react/LexicalContentEditable";
import { HistoryPlugin } from "@lexical/react/LexicalHistoryPlugin";
import { LexicalErrorBoundary } from "@lexical/react/LexicalErrorBoundary";
import { $getRoot } from "lexical";
import type { SlashCommandMeta } from "@/desktop/ui/lib/slashCommands";
import {
  $appendText,
  $insertReference,
  $setPlainText,
} from "./editorState";
import { $serializeInput, type SerializedInput } from "./serialize";
import type { ReferencePayload } from "./ReferenceNode";
import { EditorCorePlugin } from "./plugins/EditorCorePlugin";
import { SlashMenuPlugin } from "./plugins/SlashMenuPlugin";
import { MentionMenuPlugin } from "./plugins/MentionMenuPlugin";
import { PastePlugin } from "./plugins/PastePlugin";

/** 外层命令式操作编辑器的句柄。 */
export interface EditorController {
  /** 序列化当前内容（chip → 路径文本 + 引用列表）。 */
  read: () => SerializedInput;
  /** 整体替换为纯文本并把光标移到末尾。 */
  setText: (text: string) => void;
  /** 末尾追加文本（已有内容时换行追加）。 */
  appendText: (text: string) => void;
  /** 在光标处插入引用 chip。 */
  insertReference: (payload: ReferencePayload) => void;
  /** 清空内容。 */
  clear: () => void;
  /** 聚焦编辑器。 */
  focus: () => void;
}

interface Props {
  onReady: (controller: EditorController) => void;
  /** 内容变化：isEmpty 驱动按钮态；isUserEdit=false 表示程序性写入，不应重置历史索引。 */
  onChange: (state: { isEmpty: boolean; isUserEdit: boolean }) => void;
  disabled?: boolean;
  placeholder: string;
  slashCatalog: SlashCommandMeta[];
  onSlashPick: (cmd: SlashCommandMeta) => void;
  onEnter: (event: KeyboardEvent) => boolean;
  onArrow: (direction: "older" | "newer") => boolean;
  onPasteFiles: (files: File[]) => void;
  onPastePaths: (paths: string[]) => void;
  mentionFromMenu: boolean;
  onMentionClose: () => void;
}

const MIN_H = 30;
const MAX_H = 20 * 20; // 20 行 × 20px 行高

/**
 * 程序性写入（历史导航 / 命令补全 / 草稿回填 / 插引用）统一打这个 tag。
 * onChange 监听见到它就跳过——只有真实用户编辑才重置历史索引、刷新按钮态由它自然驱动。
 */
const PROGRAMMATIC_TAG = "chat-input-programmatic";

export function EditorSurface({
  onReady,
  onChange,
  disabled,
  placeholder,
  slashCatalog,
  onSlashPick,
  onEnter,
  onArrow,
  onPasteFiles,
  onPastePaths,
  mentionFromMenu,
  onMentionClose,
}: Props) {
  const [editor] = useLexicalComposerContext();

  const controller = useMemo<EditorController>(
    () => ({
      read: () => {
        let result: SerializedInput = { text: "", references: [] };
        editor.getEditorState().read(() => {
          result = $serializeInput();
        });
        return result;
      },
      setText: (text) =>
        editor.update(() => $setPlainText(text), { tag: PROGRAMMATIC_TAG }),
      appendText: (text) =>
        editor.update(() => $appendText(text), { tag: PROGRAMMATIC_TAG }),
      insertReference: (payload) =>
        editor.update(() => $insertReference(payload), {
          tag: PROGRAMMATIC_TAG,
        }),
      clear: () =>
        editor.update(
          () => {
            $getRoot().clear();
          },
          { tag: PROGRAMMATIC_TAG }
        ),
      focus: () => editor.focus(),
    }),
    [editor]
  );

  useEffect(() => {
    onReady(controller);
  }, [onReady, controller]);

  useEffect(() => {
    editor.setEditable(!disabled);
  }, [editor, disabled]);

  const handlers = useMemo(
    () => ({ onEnter, onArrow }),
    [onEnter, onArrow]
  );

  const handleChange = useCallback(
    (tags: Set<string>) => {
      const isUserEdit = !tags.has(PROGRAMMATIC_TAG);
      editor.getEditorState().read(() => {
        const text = $getRoot().getTextContent().trim();
        onChange({ isEmpty: text.length === 0, isUserEdit });
      });
    },
    [editor, onChange]
  );

  useEffect(() => {
    return editor.registerUpdateListener(({ tags }) => handleChange(tags));
  }, [editor, handleChange]);

  return (
    <>
      <PlainTextPlugin
        contentEditable={
          <ContentEditable
            spellCheck={false}
            autoCorrect="off"
            autoCapitalize="off"
            aria-label="消息输入框"
            className="chat-input-editor w-full resize-none bg-transparent px-3 py-1 text-sm outline-none overflow-y-auto"
            style={{
              lineHeight: "20px",
              minHeight: MIN_H,
              maxHeight: MAX_H,
            }}
          />
        }
        placeholder={
          <div className="pointer-events-none absolute left-3 top-1 text-sm text-muted-foreground">
            {placeholder}
          </div>
        }
        ErrorBoundary={LexicalErrorBoundary}
      />
      <HistoryPlugin />
      <EditorCorePlugin handlers={handlers} />
      <SlashMenuPlugin catalog={slashCatalog} onPick={onSlashPick} />
      <PastePlugin onFiles={onPasteFiles} onPaths={onPastePaths} />
      <MentionMenuPlugin fromMenu={mentionFromMenu} onClose={onMentionClose} />
    </>
  );
}
