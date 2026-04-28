import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  GripHorizontal,
  Loader2,
  Paperclip,
  Search,
  Globe,
  Image as ImageIcon,
  ChevronDown,
  Wrench,
} from "lucide-react";
import { toast } from "sonner";
import { animations } from "@/assets/animations";
import {
  getHistoryDraft,
  type ChatInputHistoryState,
} from "@/desktop/ui/components/chatInputHistory";
import { shouldSubmitChatInput } from "@/desktop/ui/components/chatInputKeyboard";
import { LoopingWebm } from "@/desktop/ui/components/LoopingWebm";
import { AttachmentPreviewStrip } from "@/desktop/ui/components/AttachmentPreviewStrip";
import { shouldSuppressBareEnterOnDocument } from "@/desktop/ui/lib/keyboardShortcuts";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";
import type { MessageAttachment } from "@/desktop/ui/types";

interface Props {
  onSend: (content: string, attachments: MessageAttachment[]) => Promise<void>;
  onCancel?: () => Promise<void> | void;
  disabled?: boolean;
  isStreaming?: boolean;
  userMessageHistory?: string[];
}

const MIN_H = 48;
const MAX_H = 480;
const KEY = "chatInputHeight";
const MAX_TEXT_FILE_BYTES = 1024 * 1024;
const MAX_IMAGE_BYTES = 12 * 1024 * 1024;

// 工具名称 → lucide 图标的映射
const TOOL_ICONS: Record<string, React.ReactNode> = {
  web_search: <Search className="w-3.5 h-3.5" />,
  web_fetch: <Globe className="w-3.5 h-3.5" />,
  image_generation: <ImageIcon className="w-3.5 h-3.5" />,
};

export function ChatInput({
  onSend,
  onCancel,
  disabled,
  isStreaming,
  userMessageHistory = [],
}: Props) {
  const [value, setValue] = useState("");
  const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
  const [draggingFiles, setDraggingFiles] = useState(false);
  const [sending, setSending] = useState(false);
  const [canceling, setCanceling] = useState(false);
  const [historyState, setHistoryState] = useState<ChatInputHistoryState>({
    index: null,
  });
  const [height, setHeight] = useState<number>(() => {
    const raw = localStorage.getItem(KEY);
    const n = raw ? parseInt(raw, 10) : NaN;
    return Number.isFinite(n) && n >= MIN_H && n <= MAX_H ? n : 120;
  });
  const [manual, setManual] = useState(false);
  // 控制工具下拉面板的显示
  const [toolMenuOpen, setToolMenuOpen] = useState(false);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const draggingRef = useRef<{ startY: number; startH: number } | null>(null);
  const toolMenuRef = useRef<HTMLDivElement>(null);
  const compositionRef = useRef({
    isComposing: false,
    lastCompositionEndAt: 0,
  });

  const availableTools = useStore((s) => s.availableTools);
  const enabledTools = useStore((s) => s.enabledTools);
  const toggleTool = useStore((s) => s.toggleTool);

  // 点击工具菜单外部时关闭
  useEffect(() => {
    function onClickOutside(e: MouseEvent) {
      if (toolMenuRef.current && !toolMenuRef.current.contains(e.target as Node)) {
        setToolMenuOpen(false);
      }
    }
    if (toolMenuOpen) document.addEventListener("mousedown", onClickOutside);
    return () => document.removeEventListener("mousedown", onClickOutside);
  }, [toolMenuOpen]);

  // 未手动调整高度时，按内容自适应
  useLayoutEffect(() => {
    if (manual) return;
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    const h = Math.min(Math.max(el.scrollHeight, MIN_H), 200);
    el.style.height = `${h}px`;
  }, [value, manual]);

  useEffect(() => {
    function onWindowKeyDown(event: KeyboardEvent) {
      if (shouldSuppressBareEnterOnDocument(event, document.activeElement)) {
        event.preventDefault();
      }
    }

    window.addEventListener("keydown", onWindowKeyDown, { capture: true });
    return () =>
      window.removeEventListener("keydown", onWindowKeyDown, { capture: true });
  }, []);

  async function submit() {
    const v = value.trim();
    if ((!v && attachments.length === 0) || sending || isStreaming) return;
    setSending(true);
    setValue("");
    const queuedAttachments = attachments;
    setAttachments([]);
    setHistoryState({ index: null });
    try {
      await onSend(v, queuedAttachments);
    } finally {
      setSending(false);
    }
  }

  async function cancel() {
    if (!isStreaming || canceling) return;
    setCanceling(true);
    try {
      await onCancel?.();
    } finally {
      setCanceling(false);
    }
  }

  function onChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
    setValue(e.target.value);
    setHistoryState({ index: null });
  }

  async function addFiles(files: FileList | File[]) {
    const next: MessageAttachment[] = [];
    for (const file of Array.from(files)) {
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
    if (next.length) {
      setAttachments((current) => [...current, ...next]);
    }
  }

  async function onPaste(e: React.ClipboardEvent<HTMLTextAreaElement>) {
    const files = Array.from(e.clipboardData.files).filter((file) =>
      file.type.startsWith("image/")
    );
    if (!files.length) return;
    e.preventDefault();
    await addFiles(files);
  }

  function onDrop(e: React.DragEvent<HTMLDivElement>) {
    if (!e.dataTransfer.files.length) return;
    e.preventDefault();
    setDraggingFiles(false);
    addFiles(e.dataTransfer.files);
  }

  function onDragOver(e: React.DragEvent<HTMLDivElement>) {
    if (!e.dataTransfer.types.includes("Files")) return;
    e.preventDefault();
    setDraggingFiles(true);
  }

  function removeAttachment(index: number) {
    setAttachments((current) => current.filter((_, i) => i !== index));
  }

  function navigateHistory(direction: "older" | "newer") {
    const next = getHistoryDraft({
      direction,
      currentValue: value,
      history: userMessageHistory,
      state: historyState,
    });
    if (!next.handled) return false;
    setValue(next.value);
    setHistoryState(next.state);
    return true;
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (
      !compositionRef.current.isComposing &&
      !e.nativeEvent.isComposing &&
      (e.key === "ArrowUp" || e.key === "ArrowDown")
    ) {
      const handled = navigateHistory(e.key === "ArrowUp" ? "older" : "newer");
      if (handled) {
        e.preventDefault();
        return;
      }
    }

    if (
      shouldSubmitChatInput(
        {
          key: e.key,
          shiftKey: e.shiftKey,
          isComposing: e.nativeEvent.isComposing,
          keyCode: e.nativeEvent.keyCode,
          timeStamp: e.timeStamp,
        },
        compositionRef.current
      )
    ) {
      e.preventDefault();
      submit();
    }
  }

  function onCompositionStart() {
    compositionRef.current.isComposing = true;
  }

  function onCompositionEnd(e: React.CompositionEvent<HTMLTextAreaElement>) {
    compositionRef.current.isComposing = false;
    compositionRef.current.lastCompositionEndAt = e.timeStamp;
  }

  // 拖拽调整高度——向上拖增高
  function onGripPointerDown(e: React.PointerEvent<HTMLDivElement>) {
    e.preventDefault();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    draggingRef.current = { startY: e.clientY, startH: height };
    setManual(true);
  }
  function onGripPointerMove(e: React.PointerEvent<HTMLDivElement>) {
    if (!draggingRef.current) return;
    const delta = draggingRef.current.startY - e.clientY;
    const next = Math.min(Math.max(draggingRef.current.startH + delta, MIN_H), MAX_H);
    setHeight(next);
  }
  function onGripPointerUp(e: React.PointerEvent<HTMLDivElement>) {
    if (!draggingRef.current) return;
    (e.target as HTMLElement).releasePointerCapture(e.pointerId);
    draggingRef.current = null;
    localStorage.setItem(KEY, String(height));
  }
  function onGripDoubleClick() {
    setManual(false);
    localStorage.removeItem(KEY);
  }

  useEffect(() => {
    if (!manual) return;
    if (textareaRef.current) {
      textareaRef.current.style.height = `${height}px`;
    }
  }, [height, manual]);

  // 当前是否有任何工具启用
  const hasEnabledTools = enabledTools.size > 0;
  const inputDisabled = disabled || isStreaming;
  const canSubmit =
    isStreaming ||
    (!disabled && !sending && (!!value.trim() || attachments.length > 0));

  return (
    <div className="border-t border-border bg-background/80 backdrop-blur-md px-4 pt-0 pb-3">
      <div className="max-w-3xl mx-auto">
        {/* 拖拽手柄 */}
        <div
          onPointerDown={onGripPointerDown}
          onPointerMove={onGripPointerMove}
          onPointerUp={onGripPointerUp}
          onPointerCancel={onGripPointerUp}
          onDoubleClick={onGripDoubleClick}
          className="h-3 flex items-center justify-center cursor-ns-resize group"
          title="拖拽调整高度（双击恢复自适应）"
        >
          <GripHorizontal className="w-4 h-4 text-muted-foreground/60 group-hover:text-muted-foreground transition-colors" />
        </div>

        <div
          onDrop={onDrop}
          onDragOver={onDragOver}
          onDragLeave={() => setDraggingFiles(false)}
          className={cn(
            "relative rounded-xl border border-input bg-background shadow-sm focus-within:ring-2 focus-within:ring-ring transition",
            draggingFiles && "border-primary ring-2 ring-primary/30",
            disabled && "opacity-60"
          )}
        >
          {attachments.length > 0 && (
            <AttachmentPreviewStrip
              attachments={attachments}
              variant="composer"
              onRemove={removeAttachment}
              className="px-3 pt-2"
            />
          )}
          <div className="flex items-end gap-2">
            <input
              ref={fileInputRef}
              type="file"
              multiple
              className="hidden"
              accept="text/*,application/json,application/xml,application/javascript,application/typescript,.txt,.md,.markdown,.json,.jsonl,.csv,.ts,.tsx,.js,.jsx,.rs,.py,.go,.java,.c,.cpp,.h,.hpp,.css,.html,.xml,.yaml,.yml,.toml,.sql,image/*"
              onChange={(e) => {
                if (e.currentTarget.files) addFiles(e.currentTarget.files);
                e.currentTarget.value = "";
              }}
            />
          {/* ── 工具选择下拉按钮（输入框左侧） ── */}
          {availableTools.length > 0 && (
            <div className="relative self-end pb-2 pl-2" ref={toolMenuRef}>
              <button
                type="button"
                onClick={() => setToolMenuOpen((v) => !v)}
                title={hasEnabledTools ? "工具已启用，点击管理" : "点击启用 AI 工具"}
                className={cn(
                  "h-8 px-2 rounded-md inline-flex items-center gap-1 text-xs font-medium transition-colors",
                  hasEnabledTools
                    ? "bg-primary/10 text-primary hover:bg-primary/20"
                    : "text-muted-foreground hover:bg-muted hover:text-foreground"
                )}
              >
                <Wrench className="w-3.5 h-3.5" />
                {/* 显示启用数量徽标 */}
                {hasEnabledTools && (
                  <span className="leading-none">{enabledTools.size}</span>
                )}
                <ChevronDown
                  className={cn(
                    "w-3 h-3 transition-transform",
                    toolMenuOpen && "rotate-180"
                  )}
                />
              </button>

              {/* ── 工具下拉菜单 ── */}
              {toolMenuOpen && (
                <div className="absolute bottom-full left-0 mb-2 w-52 rounded-lg border border-border bg-popover shadow-lg z-50 overflow-hidden">
                  <div className="px-3 py-2 text-xs font-semibold text-muted-foreground border-b border-border">
                    Agent 工具
                  </div>
                  {availableTools.map((tool) => {
                    const enabled = enabledTools.has(tool.name);
                    return (
                      <button
                        key={tool.name}
                        type="button"
                        onClick={() => toggleTool(tool.name)}
                        className={cn(
                          "w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left transition-colors",
                          enabled
                            ? "bg-primary/5 text-foreground"
                            : "text-muted-foreground hover:bg-muted hover:text-foreground"
                        )}
                      >
                        {/* 工具图标 */}
                        <span className="shrink-0 text-muted-foreground">
                          {TOOL_ICONS[tool.name] ?? <Wrench className="w-3.5 h-3.5" />}
                        </span>
                        <span className="flex-1 min-w-0">
                          <div className="font-medium truncate">{tool.description}</div>
                          <div className="text-[11px] text-muted-foreground/70 mt-0.5 truncate">
                            {tool.name}
                          </div>
                        </span>
                        {/* 开关指示点 */}
                        <span
                          className={cn(
                            "w-2 h-2 rounded-full shrink-0",
                            enabled ? "bg-primary" : "bg-muted-foreground/30"
                          )}
                        />
                      </button>
                    );
                  })}
                  <div className="px-3 py-1.5 text-[11px] text-muted-foreground border-t border-border">
                    启用后 AI 可自动调用工具
                  </div>
                </div>
              )}
            </div>
          )}

          <textarea
            ref={textareaRef}
            value={value}
            onChange={onChange}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            onCompositionStart={onCompositionStart}
            onCompositionEnd={onCompositionEnd}
            disabled={inputDisabled}
            spellCheck={false}
            autoCorrect="off"
            autoCapitalize="off"
            autoComplete="off"
            placeholder="输入消息，Enter 发送，Shift+Enter 换行…"
            rows={1}
            style={manual ? { height } : undefined}
            className="flex-1 resize-none bg-transparent px-3 py-3 text-sm outline-none placeholder:text-muted-foreground min-h-[48px] overflow-y-auto"
          />

          <div className="flex items-center gap-1 pr-2 pb-2">
            <button
              type="button"
              onClick={() => fileInputRef.current?.click()}
              disabled={inputDisabled}
              className="h-8 w-8 rounded-md inline-flex items-center justify-center bg-transparent text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:pointer-events-none"
              title="添加附件"
            >
              <Paperclip className="w-4 h-4" />
            </button>
            <button
              type="button"
              onClick={isStreaming ? cancel : submit}
              disabled={
                isStreaming ? canceling : !canSubmit
              }
              className={cn(
                "h-8 w-8 rounded-md inline-flex items-center justify-center bg-transparent text-primary hover:bg-muted disabled:opacity-40 disabled:pointer-events-none",
                isStreaming && "bg-background text-primary hover:bg-background"
              )}
              title={isStreaming ? "中断生成" : "发送 (Enter)"}
            >
              {isStreaming ? (
                <LoopingWebm
                  src={animations.sendInterrupt}
                  className="h-7 w-7 rounded"
                />
              ) : sending ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <img
                  src={animations.assistantThinkingStatic}
                  alt=""
                  className="h-7 w-7 object-contain"
                  draggable={false}
                />
              )}
            </button>
          </div>
          </div>
        </div>

        <div className="flex items-center justify-between mt-1.5 px-1 text-[11px] text-muted-foreground">
          <span>
            {hasEnabledTools ? (
              <>
                <span className="text-primary font-medium">Agent 模式</span>
                {" · "}已启用 {enabledTools.size} 个工具
              </>
            ) : (
              attachments.length > 0
                ? `已添加 ${attachments.length} 个附件`
                : "支持 Markdown · Cmd/Ctrl+F 搜索当前对话"
            )}
          </span>
          <span>Enter 发送 · Shift+Enter 换行</span>
        </div>
      </div>
    </div>
  );
}

function isTextFile(file: File) {
  if (file.type.startsWith("text/")) return true;
  return /\.(txt|md|markdown|json|jsonl|csv|ts|tsx|js|jsx|rs|py|go|java|c|cpp|h|hpp|css|html|xml|yaml|yml|toml|sql)$/i.test(
    file.name
  );
}

function mediaTypeFromName(name: string) {
  const lower = name.toLowerCase();
  if (lower.endsWith(".json")) return "application/json";
  if (lower.endsWith(".xml")) return "application/xml";
  if (lower.endsWith(".html")) return "text/html";
  if (lower.endsWith(".css")) return "text/css";
  if (lower.endsWith(".csv")) return "text/csv";
  if (lower.endsWith(".md") || lower.endsWith(".markdown")) return "text/markdown";
  return "text/plain";
}

async function imageAttachmentFromFile(file: File): Promise<MessageAttachment> {
  const dataUrl = await readFileAsDataUrl(file);
  const comma = dataUrl.indexOf(",");
  const data = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
  return {
    kind: "image",
    name: file.name || "pasted-image.png",
    media_type: file.type || mediaTypeFromDataUrl(dataUrl) || "image/png",
    data,
  };
}

function readFileAsDataUrl(file: File) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.onerror = () => reject(new Error(`${file.name} 读取失败`));
    reader.readAsDataURL(file);
  });
}

function mediaTypeFromDataUrl(dataUrl: string) {
  const match = /^data:([^;,]+)[;,]/.exec(dataUrl);
  return match?.[1] ?? null;
}
