import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  FilePlus2,
  Folder,
  FolderOpen,
  GripHorizontal,
  Loader2,
  Plus,
  X,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { animations } from "@/assets/animations";
import {
  getHistoryDraft,
  type ChatInputHistoryState,
} from "@/desktop/ui/components/chatInputHistory";
import { shouldSubmitChatInput } from "@/desktop/ui/components/chatInputKeyboard";
import { ContextRing } from "@/desktop/ui/components/ContextRing";
import { LoopingWebm } from "@/desktop/ui/components/LoopingWebm";
import { ModelPickerButton } from "@/desktop/ui/components/ModelPickerButton";
import { TokenStatsPanel } from "@/desktop/ui/components/TokenStatsPanel";
import { AttachmentPreviewStrip } from "@/desktop/ui/components/AttachmentPreviewStrip";
import { shouldSuppressBareEnterOnDocument } from "@/desktop/ui/lib/keyboardShortcuts";
import { cn } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
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

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const draggingRef = useRef<{ startY: number; startH: number } | null>(null);
  const compositionRef = useRef({
    isComposing: false,
    lastCompositionEndAt: 0,
  });

  const compacting = useStore((s) => s.compacting);
  const compactCurrentSession = useStore((s) => s.compactCurrentSession);
  const enqueueInput = useStore((s) => s.enqueueInput);
  const tokenStats = useStore(
    (s) => s.currentSession?.token_stats ?? null
  );
  const contextUsage = useStore((s) => s.contextUsage);
  const pendingWorkdir = useStore((s) => s.pendingWorkdir);
  const pendingAllowedDirs = useStore((s) => s.pendingAllowedDirs);
  const setPendingWorkdir = useStore((s) => s.setPendingWorkdir);
  const setPendingAllowedDirs = useStore((s) => s.setPendingAllowedDirs);
  const currentSession = useStore((s) => s.currentSession);

  // chip 数据源直接用 pending：openSession 会把 pending 同步成目标对话的实际值，
  // setPending* 也会同步更新当前 session，二者保持一致——所以这里只看一边即可。
  const activeWorkdir = pendingWorkdir;
  const activeAllowedDirs = pendingAllowedDirs;

  // 输入框文本 (value) 与附件 (attachments) 故意不绑定 currentSession：
  // 这是用户当前的"草稿"，跨对话保留，切到老对话也不会被清空（老对话已发送的消息
  // 仍在历史里，与草稿互不干扰）。发送时由 submit() 自行清空。

  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const addMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!addMenuOpen) return;
    function onClick(event: MouseEvent) {
      if (
        addMenuRef.current &&
        !addMenuRef.current.contains(event.target as Node)
      ) {
        setAddMenuOpen(false);
      }
    }
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [addMenuOpen]);

  async function pickProject() {
    setAddMenuOpen(false);
    try {
      const dir = await openDialog({ directory: true, multiple: false });
      if (typeof dir === "string") {
        await setPendingWorkdir(dir);
        toast.success(`已设为项目：${dir}`);
      }
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function pickAllowedDir() {
    setAddMenuOpen(false);
    try {
      const dir = await openDialog({ directory: true, multiple: true });
      if (!dir) return;
      const arr = Array.isArray(dir) ? dir : [dir];
      const merged = [...activeAllowedDirs];
      for (const d of arr) {
        if (typeof d === "string" && !merged.includes(d)) merged.push(d);
      }
      await setPendingAllowedDirs(merged);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function clearWorkdir() {
    try {
      await setPendingWorkdir(null);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function removeAllowedDir(path: string) {
    try {
      await setPendingAllowedDirs(activeAllowedDirs.filter((d) => d !== path));
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function runCompact(customInstructions: string) {
    if (compacting) return;
    setSending(true);
    setValue("");
    setHistoryState({ index: null });
    try {
      await compactCurrentSession(customInstructions || undefined);
      toast.success("上下文已压缩");
    } catch (e: any) {
      toast.error(e?.message || "压缩失败");
    } finally {
      setSending(false);
    }
  }

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
    if ((!v && attachments.length === 0) || sending) return;
    // streaming 时回车不再直接发送，而是入队（FIFO 自动消费）。
    if (isStreaming) {
      enqueueAndClear("tail");
      return;
    }
    if (v.startsWith("/compact")) {
      const args = v.slice("/compact".length).trim();
      await runCompact(args);
      return;
    }
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

  /** 把当前输入加入队列；position='head' 用于 Shift+Enter / 立即发送。 */
  function enqueueAndClear(position: "tail" | "head") {
    const v = value.trim();
    if (!v && attachments.length === 0) return;
    enqueueInput(v, attachments, position);
    setValue("");
    setAttachments([]);
    setHistoryState({ index: null });
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

  /**
   * 粘贴时按 [文件 → 路径文本] 顺序处理：
   * 1. 剪切板里有 File（截图、Finder 复制图片）→ 直接走 addFiles，原样支持图片预览
   * 2. 文本是文件路径列表（按行拆，支持 file:// URI）→ 调 attach_path 让后端
   *    判断是文件还是目录，文件追加到 attachments，目录追加到 pendingAllowedDirs
   * 3. 普通文本 → 不拦截，让浏览器按默认行为插入到 textarea
   */
  async function onPaste(e: React.ClipboardEvent<HTMLTextAreaElement>) {
    const files = Array.from(e.clipboardData.files);
    if (files.length > 0) {
      e.preventDefault();
      await addFiles(files);
      return;
    }
    const text = e.clipboardData.getData("text/plain");
    const candidates = parsePathCandidates(text);
    if (candidates.length === 0) return;
    e.preventDefault();
    await attachPathCandidates(candidates);
  }

  /** 把每行（或 file:// 列表）当作潜在路径，全部丢给后端探测；UI 同步刷新 chip / attachments。 */
  async function attachPathCandidates(paths: string[]) {
    const newAttachments: MessageAttachment[] = [];
    const newDirs: string[] = [];
    for (const p of paths) {
      try {
        const res = await api.attachPath(p);
        switch (res.kind) {
          case "file":
            newAttachments.push(res.attachment);
            break;
          case "dir":
            if (
              !activeAllowedDirs.includes(res.path) &&
              !newDirs.includes(res.path)
            ) {
              newDirs.push(res.path);
            }
            break;
          case "missing":
            toast.error(`找不到路径：${res.path}`);
            break;
          case "unsupported":
            toast.error(res.reason);
            break;
        }
      } catch (e: any) {
        toast.error(e?.message ?? String(e));
      }
    }
    if (newAttachments.length > 0) {
      setAttachments((current) => [...current, ...newAttachments]);
    }
    if (newDirs.length > 0) {
      try {
        await setPendingAllowedDirs([...activeAllowedDirs, ...newDirs]);
        toast.success(`已添加 ${newDirs.length} 个目录`);
      } catch (e: any) {
        toast.error(e?.message ?? String(e));
      }
    }
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

    // streaming 中 Shift+Enter = 立即入队（队首），让它最先被消费；
    // 非 streaming 时保持浏览器默认换行行为。
    if (
      isStreaming &&
      e.key === "Enter" &&
      e.shiftKey &&
      !compositionRef.current.isComposing &&
      !e.nativeEvent.isComposing &&
      e.nativeEvent.keyCode !== 229
    ) {
      e.preventDefault();
      enqueueAndClear("head");
      return;
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

  // streaming 时仍允许输入（Enter 入队 / Shift+Enter 立即入队队首），
  // 只有外部显式 disabled（如未配置 provider）时才禁用。
  const inputDisabled = !!disabled;
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

        <div className="flex items-end gap-1">
        <div
          onDrop={onDrop}
          onDragOver={onDragOver}
          onDragLeave={() => setDraggingFiles(false)}
          className={cn(
            "flex-1 min-w-0 relative rounded-xl border border-input bg-background shadow-sm focus-within:ring-2 focus-within:ring-ring transition",
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
          {(activeWorkdir || activeAllowedDirs.length > 0) && (
            <div className="flex flex-wrap gap-1.5 px-3 pt-2">
              {activeWorkdir && (
                <span
                  className="inline-flex items-center gap-1 rounded-md bg-primary/10 text-primary px-2 py-0.5 text-[11px] font-mono group"
                  title={`项目：${activeWorkdir}`}
                >
                  <FolderOpen className="w-3 h-3" />
                  <span className="truncate max-w-[200px]">
                    {basename(activeWorkdir)}
                  </span>
                  <button
                    type="button"
                    onClick={clearWorkdir}
                    className="opacity-50 hover:opacity-100"
                    aria-label="移除项目"
                  >
                    <X className="w-3 h-3" />
                  </button>
                </span>
              )}
              {activeAllowedDirs.map((d) => (
                <span
                  key={d}
                  className="inline-flex items-center gap-1 rounded-md bg-muted text-muted-foreground px-2 py-0.5 text-[11px] font-mono group"
                  title={d}
                >
                  <Folder className="w-3 h-3" />
                  <span className="truncate max-w-[200px]">{basename(d)}</span>
                  <button
                    type="button"
                    onClick={() => removeAllowedDir(d)}
                    className="opacity-50 hover:opacity-100"
                    aria-label="移除目录"
                  >
                    <X className="w-3 h-3" />
                  </button>
                </span>
              ))}
            </div>
          )}
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
            placeholder={
              isStreaming
                ? "正在生成…Enter 排队，Shift+Enter 立即排到队首"
                : "输入消息，Enter 发送，Shift+Enter 换行…"
            }
            rows={1}
            style={manual ? { height } : undefined}
            className="w-full resize-none bg-transparent px-3 py-3 text-sm outline-none placeholder:text-muted-foreground min-h-[56px] overflow-y-auto"
          />

          {/* 底部工具条：左 = + 菜单（文件 / 项目 / 目录），右 = 模型选择 + 发送 */}
          <div className="flex items-center justify-between px-2 pb-2">
            <div className="relative" ref={addMenuRef}>
              <button
                type="button"
                onClick={() => setAddMenuOpen((v) => !v)}
                disabled={inputDisabled}
                className="h-8 w-8 rounded-md inline-flex items-center justify-center bg-transparent text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:pointer-events-none"
                title="添加文件 / 项目 / 目录"
              >
                <Plus className="w-4 h-4" />
              </button>
              {addMenuOpen && (
                <div
                  onClick={(e) => e.stopPropagation()}
                  className="absolute bottom-full left-0 mb-1 w-44 rounded-lg border border-border bg-card shadow-lg z-[90] overflow-hidden animate-slide-up"
                >
                  <button
                    type="button"
                    onClick={() => {
                      setAddMenuOpen(false);
                      fileInputRef.current?.click();
                    }}
                    className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                  >
                    <FilePlus2 className="w-4 h-4 text-muted-foreground" />
                    添加文件
                  </button>
                  <button
                    type="button"
                    onClick={pickProject}
                    className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                    title={
                      activeWorkdir
                        ? "项目（workdir）只能有一个，选择新目录会替换当前项目"
                        : undefined
                    }
                  >
                    <FolderOpen className="w-4 h-4 text-muted-foreground" />
                    {activeWorkdir ? "更换项目" : "添加项目"}
                  </button>
                  <button
                    type="button"
                    onClick={pickAllowedDir}
                    className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                  >
                    <Folder className="w-4 h-4 text-muted-foreground" />
                    添加目录
                  </button>
                </div>
              )}
            </div>

            <div className="flex items-center gap-1">
              <ModelPickerButton />
              {(() => {
                // streaming 时：输入框有内容 → 按钮做入队（同 Enter）；否则做中断生成。
                const hasDraft = !!value.trim() || attachments.length > 0;
                const enqueueMode = isStreaming && hasDraft;
                const onClick = enqueueMode
                  ? () => enqueueAndClear("tail")
                  : isStreaming
                    ? cancel
                    : submit;
                const buttonDisabled = enqueueMode
                  ? false
                  : isStreaming
                    ? canceling
                    : !canSubmit;
                const title = enqueueMode
                  ? "排队（Enter）"
                  : isStreaming
                    ? "中断生成"
                    : "发送 (Enter)";
                return (
                  <button
                    type="button"
                    onClick={onClick}
                    disabled={buttonDisabled}
                    className={cn(
                      "h-8 w-8 rounded-md inline-flex items-center justify-center bg-transparent text-primary hover:bg-muted disabled:opacity-40 disabled:pointer-events-none",
                      isStreaming &&
                        !enqueueMode &&
                        "bg-background text-primary hover:bg-background"
                    )}
                    title={title}
                  >
                    {enqueueMode ? (
                      <Plus className="w-4 h-4" />
                    ) : isStreaming ? (
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
                );
              })()}
            </div>
          </div>
        </div>

          {/* 紧贴输入框右侧的状态条：左 = TokenStats（hover 浮出统计），
              右 = ContextRing（hover 显示百分比，点击运行 /compact）。
              默认无边框，hover 才出方形大圆角边框，让视觉重量给输入框本身。 */}
          <div className="flex items-center gap-0.5 pb-2 shrink-0">
            <TokenStatsPanel stats={tokenStats} />
            {contextUsage && (
              <ContextRing
                used={contextUsage.used_tokens}
                budget={contextUsage.budget_tokens}
                onClick={() => {
                  if (compacting) return;
                  void runCompact("");
                }}
              />
            )}
          </div>
        </div>

        <div className="flex items-center justify-between mt-1.5 px-1 text-[11px] text-muted-foreground">
          <span>
            {attachments.length > 0
              ? `已添加 ${attachments.length} 个附件`
              : ""}
          </span>
        </div>
      </div>
    </div>
  );
}

/**
 * 把粘贴的文本拆解为"看起来像路径"的候选列表。
 *
 * 启发式：先按换行拆，再 trim；每行剥掉 macOS / GNOME 拖拽常见的引号，
 * 然后只保留以 `/`、`~/`、`file://` 或 Windows 盘符（`C:\` / `C:/`）开头的项。
 * 普通文本（哪怕里面带几个空格）一概不当路径，避免误吃用户消息。
 */
function parsePathCandidates(raw: string): string[] {
  if (!raw || raw.length > 4096) return [];
  const lines = raw.split(/\r?\n/);
  const out: string[] = [];
  for (const line of lines) {
    let s = line.trim();
    if (!s) continue;
    // 去掉 Finder / 终端拖拽常见的两端引号
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

/** 路径 basename：剥掉尾部 `/` 后取最后一段；空串回退为整段。 */
function basename(p: string): string {
  const trimmed = p.replace(/[\\/]+$/, "");
  const idx = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  const name = idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
  return name || trimmed || p;
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
