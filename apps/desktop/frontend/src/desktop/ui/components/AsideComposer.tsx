import { useCallback, useEffect, useRef, useState } from "react";
import { CornerDownLeft, Loader2, Square } from "lucide-react";
import { LexicalComposer } from "@lexical/react/LexicalComposer";
import { toast } from "sonner";
import { api } from "@/desktop/bridge/tauri";
import { cn } from "@/desktop/ui/lib/utils";
import type { MessageAttachment } from "@/desktop/ui/types";
import { AttachmentPreviewStrip } from "./AttachmentPreviewStrip";
import { chatInputEditorConfig } from "./chatInput/editorConfig";
import { EditorSurface, type EditorController } from "./chatInput/EditorSurface";
import {
  MAX_IMAGE_BYTES,
  MAX_TEXT_FILE_BYTES,
  imageAttachmentFromFile,
  isTextFile,
  mediaTypeFromName,
} from "./chatInput/attachments";

/**
 * 旁支 / 浏览器注释等「次级对话」共用的轻量输入框。
 *
 * 复用主对话输入框的 Lexical 富文本内核（[`EditorSurface`]）：粘贴/拖拽图片成附件、
 * 路径粘贴成可整块删除的引用 chip、IME 安全的回车发送。但**不依赖主对话全局状态**
 * （workdir / 项目 / 队列 / token 统计那些都不要）——次级对话只需要「写字 + 贴图 +
 * 引用 + 发送/停止」。模型选择器等场景特有的控件通过 `leftSlot` 注入。
 *
 * 受控：正文与附件由外部持有（旁支存 branch store / 注释存页面内存），本组件只管编辑与回调。
 */
export interface AsideComposerProps {
  /** 正文草稿（受控）。 */
  value: string;
  onChange: (value: string) => void;
  attachments: MessageAttachment[];
  onAttachmentsChange: (next: MessageAttachment[]) => void;
  /** 正在跑一轮：发送按钮变「停止」。 */
  busy: boolean;
  /** 发送：把当前正文 + 附件交出去（外部负责清空 value/attachments）。 */
  onSend: (text: string, attachments: MessageAttachment[]) => void;
  /** 停止当前轮（busy 时点发送按钮触发）。不传则 busy 时按钮禁用。 */
  onStop?: () => void;
  placeholder?: string;
  /** 底部操作行左侧插槽（如模型选择器）。 */
  leftSlot?: React.ReactNode;
  disabled?: boolean;
}

export function AsideComposer(props: AsideComposerProps) {
  return (
    <LexicalComposer initialConfig={chatInputEditorConfig}>
      <AsideComposerInner {...props} />
    </LexicalComposer>
  );
}

function AsideComposerInner({
  value,
  onChange,
  attachments,
  onAttachmentsChange,
  busy,
  onSend,
  onStop,
  placeholder = "问点什么",
  leftSlot,
  disabled,
}: AsideComposerProps) {
  const editorRef = useRef<EditorController | null>(null);
  const [isEmpty, setIsEmpty] = useState(value.trim().length === 0);

  const onEditorReady = useCallback(
    (controller: EditorController) => {
      editorRef.current = controller;
      // 外部草稿回填（切换旁支 / 重建时）：仅在与编辑器当前内容不一致时写入。
      if (value && controller.read().text !== value) controller.setText(value);
    },
    // 仅初始化时回填一次，后续由用户编辑驱动；value 变化不强行覆盖（避免打断输入）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
    []
  );

  const onEditorChange = useCallback(
    (state: { isEmpty: boolean }) => {
      setIsEmpty(state.isEmpty);
      const text = editorRef.current?.read().text ?? "";
      onChange(text);
    },
    [onChange]
  );

  function clearEditor() {
    editorRef.current?.clear();
    onChange("");
  }

  function doSend() {
    const text = editorRef.current?.read().text.trim() ?? "";
    if ((!text && attachments.length === 0) || busy || disabled) return;
    const sending = attachments;
    clearEditor();
    onAttachmentsChange([]);
    onSend(text, sending);
  }

  const handleEnter = useCallback((event: KeyboardEvent): boolean => {
    // Shift/⌘ + Enter 换行；纯 Enter 发送（IME 安全由 EditorCorePlugin 兜底）。
    if (event.shiftKey || event.metaKey) return false;
    doSend();
    return true;
    // doSend 读最新 ref，无需进依赖；attachments/busy 通过闭包刷新由父重渲染保证。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [attachments, busy, disabled]);

  async function addFiles(files: File[]) {
    const next: MessageAttachment[] = [];
    for (const file of files) {
      try {
        if (file.type.startsWith("image/")) {
          if (file.size > MAX_IMAGE_BYTES) {
            toast.error(`${file.name} 超过 12MB`);
            continue;
          }
          next.push(await imageAttachmentFromFile(file));
        } else if (isTextFile(file)) {
          if (file.size > MAX_TEXT_FILE_BYTES) {
            toast.error(`${file.name} 超过 1MB`);
            continue;
          }
          next.push({
            kind: "text_file",
            name: file.name,
            media_type: file.type || mediaTypeFromName(file.name),
            content: await file.text(),
          });
        } else {
          toast.error(`${file.name} 不是支持的文本或图片文件`);
        }
      } catch (e: any) {
        toast.error(e?.message || `${file.name} 读取失败`);
      }
    }
    if (next.length) onAttachmentsChange([...attachments, ...next]);
  }

  async function insertPathReferences(paths: string[]) {
    const controller = editorRef.current;
    if (!controller) return;
    let inserted = 0;
    for (const p of paths) {
      try {
        const res = await api.attachPath(p);
        if (res.kind === "missing") continue;
        controller.insertReference({ path: res.path, kind: "path" });
        inserted += 1;
      } catch (e: any) {
        toast.error(e?.message ?? String(e));
      }
    }
    if (inserted > 0) controller.focus();
  }

  function removeAttachment(index: number) {
    onAttachmentsChange(attachments.filter((_, i) => i !== index));
  }

  useEffect(() => {
    editorRef.current?.setText(value === "" ? "" : value);
    // 仅当外部把 value 清空时同步清编辑器（发送后父置空）；非空变化不回灌避免打断输入。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value === ""]);

  const sendDisabled = !busy && (disabled || (isEmpty && attachments.length === 0));

  return (
    <div className="shrink-0 border-t border-border px-2.5 pb-2 pt-2">
      <div
        className={cn(
          "rounded-lg border border-border bg-background focus-within:ring-1 focus-within:ring-ring transition",
          disabled && "opacity-60"
        )}
      >
        {attachments.length > 0 && (
          <AttachmentPreviewStrip
            attachments={attachments}
            variant="composer"
            onRemove={removeAttachment}
            className="px-2 pt-2"
          />
        )}
        <div className="px-1 py-1">
          <EditorSurface
            onReady={onEditorReady}
            onChange={onEditorChange}
            disabled={disabled}
            placeholder={placeholder}
            slashCatalog={[]}
            onSlashPick={() => {}}
            onEnter={handleEnter}
            onArrow={() => false}
            onPasteFiles={(files) => void addFiles(files)}
            onPastePaths={(paths) => void insertPathReferences(paths)}
            mentionFromMenu={false}
            onMentionClose={() => {}}
          />
        </div>
        <div className="flex items-center justify-between gap-2 px-2 pb-1.5 pt-0.5">
          <div className="min-w-0">{leftSlot}</div>
          <button
            type="button"
            onClick={busy ? onStop : doSend}
            disabled={busy ? !onStop : sendDisabled}
            className={cn(
              "grid h-7 w-7 shrink-0 place-items-center rounded-md transition",
              busy
                ? "text-foreground hover:bg-accent"
                : "text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40"
            )}
            title={busy ? "停止" : "发送（Enter）"}
            aria-label={busy ? "停止" : "发送"}
          >
            {busy ? (
              <Square className="h-3.5 w-3.5 fill-current" />
            ) : (
              <CornerDownLeft className="h-3.5 w-3.5" />
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
