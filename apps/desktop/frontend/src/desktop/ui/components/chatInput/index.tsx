import {
  type CSSProperties,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  BriefcaseBusiness,
  FilePlus2,
  FolderOpen,
  Loader2,
  MessageSquare,
  Plus,
  X,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { toast } from "sonner";
import { LexicalComposer } from "@lexical/react/LexicalComposer";
import { animations } from "@/assets/animations";
import {
  getHistoryDraft,
  type ChatInputHistoryState,
} from "@/desktop/ui/components/chatInputHistory";
import {
  ConversationRefPopup,
  type ConversationItem,
} from "@/desktop/ui/components/ConversationRefPopup";
import { InputDrawer } from "@/desktop/ui/components/InputDrawer";
import { HoverHint } from "@/desktop/ui/components/HoverHint";
import { LoopingWebm } from "@/desktop/ui/components/LoopingWebm";
import { ModelPickerButton } from "@/desktop/ui/components/ModelPickerButton";
import { PathHint } from "@/desktop/ui/components/PathHint";
import { ReasoningEffortPill } from "@/desktop/ui/components/ReasoningEffortPill";
import { RunModeChip } from "@/desktop/ui/components/RunModeChip";
import { SlashCommandButton } from "@/desktop/ui/components/SlashCommandButton";
import { TokenStatsPanel } from "@/desktop/ui/components/TokenStatsPanel";
import { isSessionCompacting } from "@/desktop/ui/components/compactingState";
import { ProviderUsageIndicator } from "@/desktop/ui/components/ProviderUsageIndicator";
import { AttachmentPreviewStrip } from "@/desktop/ui/components/AttachmentPreviewStrip";
import { PathTypeIcon } from "@/desktop/ui/components/workspaceFields";
import { projectInputWithoutAllowedPath } from "@/desktop/ui/lib/projectFolders";
import { shouldSuppressBareEnterOnDocument } from "@/desktop/ui/lib/keyboardShortcuts";
import {
  buildSlashCommandCatalog,
  dispatchSlashCommand,
  type SlashCommandMeta,
} from "@/desktop/ui/lib/slashCommands";
import { cn, pathLeaf } from "@/desktop/ui/lib/utils";
import { useStore, type EditorSelectionRef } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import type { MessageAttachment, SkillItem } from "@/desktop/ui/types";
import { chatInputEditorConfig } from "./editorConfig";
import { EditorSurface, type EditorController } from "./EditorSurface";
import {
  MAX_IMAGE_BYTES,
  MAX_TEXT_FILE_BYTES,
  imageAttachmentFromFile,
  isTextFile,
  mediaTypeFromName,
} from "./attachments";

interface Props {
  onSend: (content: string, attachments: MessageAttachment[]) => Promise<void>;
  onCancel?: () => Promise<void> | void;
  disabled?: boolean;
  isStreaming?: boolean;
  userMessageHistory?: string[];
}

/** 选区引用渲染成 `path:line` 或 `path:start-end` 文本。 */
function formatSelectionRef(ref: EditorSelectionRef): string {
  return ref.startLine === ref.endLine
    ? `${ref.path}:${ref.startLine}`
    : `${ref.path}:${ref.startLine}-${ref.endLine}`;
}

export function ChatInput(props: Props) {
  return (
    <LexicalComposer initialConfig={chatInputEditorConfig}>
      <ChatInputInner {...props} />
    </LexicalComposer>
  );
}

function ChatInputInner({
  onSend,
  onCancel,
  disabled,
  isStreaming,
  userMessageHistory = [],
}: Props) {
  const [attachments, setAttachments] = useState<MessageAttachment[]>([]);
  const [isEmpty, setIsEmpty] = useState(true);
  const [draggingFiles, setDraggingFiles] = useState(false);
  const [sending, setSending] = useState(false);
  const [canceling, setCanceling] = useState(false);
  const [historyState, setHistoryState] = useState<ChatInputHistoryState>({
    index: null,
  });

  const dropCardRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const chipScrollRef = useRef<HTMLDivElement | null>(null);
  const nativeDropRef = useRef<(paths: string[]) => void>(() => {});
  const editorRef = useRef<EditorController | null>(null);

  const compactingSessionId = useStore((s) => s.compactingSessionId);
  const currentSessionId = useStore((s) => s.currentSession?.id ?? null);
  const compacting = isSessionCompacting(compactingSessionId, currentSessionId);
  const compactCurrentSession = useStore((s) => s.compactCurrentSession);
  const enqueueInput = useStore((s) => s.enqueueInput);
  const flushQueuedItem = useStore((s) => s.flushQueuedItem);
  const currentInputQueue = useStore((s) => s.currentInputQueue);
  const composerDraft = useStore((s) => s.composerDraft);
  const clearComposerDraft = useStore((s) => s.clearComposerDraft);
  const tokenStats = useStore((s) => s.currentSession?.token_stats ?? null);
  const contextUsage = useStore((s) => s.contextUsage);
  const pendingWorkdir = useStore((s) => s.pendingWorkdir);
  const pendingAllowedPaths = useStore((s) => s.pendingAllowedPaths);
  const setPendingWorkdir = useStore((s) => s.setPendingWorkdir);
  const setPendingAllowedPaths = useStore((s) => s.setPendingAllowedPaths);
  const currentSession = useStore((s) => s.currentSession);
  const editorSelectionRef = useStore((s) => s.editorSelectionRef);
  const setEditorSelectionRef = useStore((s) => s.setEditorSelectionRef);
  const projects = useStore((s) => s.projects);
  const saveProject = useStore((s) => s.saveProject);
  const providersFile = useStore((s) => s.providersFile);

  const activeWorkdir = pendingWorkdir;
  const activeAllowedPaths = pendingAllowedPaths;
  const activeProject = currentSession?.project_id
    ? (projects.find((p) => p.id === currentSession.project_id) ?? null)
    : null;

  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const addMenuRef = useRef<HTMLDivElement>(null);
  const drawerOpen = true;

  // 架构 §6.1.3 / §8：当前 workdir 下的三层 skills，驱动 `//` 命令注册表。
  const [skills, setSkills] = useState<SkillItem[]>([]);
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await api.listSkills(activeWorkdir || ".");
        if (!cancelled) setSkills(list);
      } catch {
        if (!cancelled) setSkills([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeWorkdir]);
  const slashCatalog = useMemo(() => buildSlashCommandCatalog(skills), [skills]);

  // `@` 引用菜单是否从 + 菜单主动打开（区别于编辑器内 `@` 自动触发）。
  const [mentionFromMenu, setMentionFromMenu] = useState(false);

  const onEditorReady = useCallback((controller: EditorController) => {
    editorRef.current = controller;
  }, []);

  const onEditorChange = useCallback(
    (state: { isEmpty: boolean; isUserEdit: boolean }) => {
      setIsEmpty(state.isEmpty);
      // 仅用户真实编辑才退出历史浏览；程序性写入（含历史导航自身的 setText）不重置，
      // 否则连续上键会被自己触发的更新冲掉索引、翻不到更早的历史。
      if (state.isUserEdit) setHistoryState({ index: null });
    },
    []
  );

  // ── 发送 / 入队 ────────────────────────────────────────────────────────
  /** 读取编辑器内容并把引用路径并入 allowed_paths，返回纯文本。 */
  async function drainEditor(): Promise<string> {
    const controller = editorRef.current;
    if (!controller) return "";
    const { text, references } = controller.read();
    // 来自文件查看器的实时选区引用（path:line）也算一条引用：路径并入 allowed_paths，
    // 引用文本前置到消息。
    const selRef = editorSelectionRef;
    const refPaths = selRef ? [...references, selRef.path] : references;
    if (refPaths.length > 0) {
      const merged = [...activeAllowedPaths];
      for (const p of refPaths) if (!merged.includes(p)) merged.push(p);
      if (merged.length !== activeAllowedPaths.length) {
        try {
          await setPendingAllowedPaths(merged);
        } catch (e: any) {
          toast.error(e?.message ?? String(e));
        }
      }
    }
    if (selRef) {
      const ref = formatSelectionRef(selRef);
      return text ? `${ref}\n${text}` : ref;
    }
    return text;
  }

  function clearEditor() {
    editorRef.current?.clear();
    setHistoryState({ index: null });
    setEditorSelectionRef(null);
  }

  async function submit() {
    const controller = editorRef.current;
    if (!controller) return;
    const { text } = controller.read();
    const v = text.trim();
    if ((!v && attachments.length === 0 && !editorSelectionRef) || sending) return;
    if (isStreaming) {
      await enqueueAndClear("tail");
      return;
    }
    if (v.startsWith("/compact")) {
      const args = v.slice("/compact".length).trim();
      await runCompact(args);
      return;
    }
    // `//` 命令系统（架构 §8）。
    if (v.startsWith("//")) {
      const result = await dispatchSlashCommand(
        v,
        {
          sessionId: currentSession?.id ?? null,
          toast,
          sendPrompt: async (sendText) => {
            setSending(true);
            const queuedAttachments = attachments;
            setAttachments([]);
            try {
              void onSend(sendText, queuedAttachments).catch((e: any) => {
                toast.error(e?.message || String(e));
              });
            } finally {
              setSending(false);
            }
          },
        },
        skills
      );
      if (result.handled) {
        if (result.error) {
          toast.error(result.error);
        } else {
          clearEditor();
        }
        return;
      }
    }
    setSending(true);
    const content = await drainEditor();
    clearEditor();
    const queuedAttachments = attachments;
    setAttachments([]);
    try {
      void onSend(content.trim(), queuedAttachments).catch((e: any) => {
        toast.error(e?.message || String(e));
      });
    } finally {
      setSending(false);
    }
  }

  /** 把当前输入加入队列；position='head' 用于 Shift+Enter / 立即引导。 */
  async function enqueueAndClear(position: "tail" | "head") {
    const content = await drainEditor();
    const v = content.trim();
    if (!v && attachments.length === 0) return;
    enqueueInput(v, attachments, position);
    clearEditor();
    setAttachments([]);
  }

  /** Shift+Enter / Cmd+Enter：入队首 + 立即引导。 */
  async function enqueueHeadAndFlush() {
    const content = await drainEditor();
    const v = content.trim();
    const hasDraft = !!v || attachments.length > 0;
    if (!hasDraft && currentInputQueue.length === 0) return;
    if (hasDraft) {
      enqueueInput(v, attachments, "head");
      clearEditor();
      setAttachments([]);
    }
    try {
      await flushQueuedItem();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function runCompact(customInstructions: string) {
    if (compacting) return;
    clearEditor();
    try {
      await compactCurrentSession(customInstructions || undefined);
      toast.success("上下文已压缩");
    } catch (e: any) {
      toast.error(e?.message || "压缩失败");
    }
  }

  // ── 键盘：Enter 发送 / 入队，方向键历史 ──────────────────────────────────
  const handleEnter = useCallback(
    (event: KeyboardEvent): boolean => {
      // Shift+Enter / Cmd+Enter 在 streaming 或有队列时 = 入队首 + 引导。
      if (
        (isStreaming || currentInputQueue.length > 0) &&
        (event.shiftKey || event.metaKey)
      ) {
        void enqueueHeadAndFlush();
        return true;
      }
      // 普通态的 Shift+Enter（非 streaming、无队列）= 换行：不消费，让编辑器插入换行。
      if (event.shiftKey || event.metaKey) return false;
      // 纯 Enter：发送（streaming 时 submit 内部自动转入队）。已过 IME 防护。
      void submit();
      return true;
    },
    // submit / enqueueHeadAndFlush 用最新闭包：依赖项覆盖其读到的状态。
    [isStreaming, currentInputQueue.length, attachments, sending, skills, currentSession]
  );

  const handleArrow = useCallback(
    (direction: "older" | "newer"): boolean => {
      const controller = editorRef.current;
      if (!controller) return false;
      const { text } = controller.read();
      const next = getHistoryDraft({
        direction,
        currentValue: text,
        history: userMessageHistory,
        state: historyState,
      });
      if (!next.handled) return false;
      controller.setText(next.value);
      setHistoryState(next.state);
      return true;
    },
    [userMessageHistory, historyState]
  );

  // ── 附件 / 路径引用 ──────────────────────────────────────────────────────
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

  /** 把粘贴/拖拽的路径当引用 chip 插入编辑器（存在的才插）。 */
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

  /**
   * Desktop 原生拖拽落地：后端按磁盘路径分流——支持的小图片 / 文本读成附件，
   * 其余（目录 / 大文件 / 二进制 / 未知类型）作为引用 chip 插入编辑器。
   */
  async function handleNativeDrop(paths: string[]) {
    if (paths.length === 0) return;
    let outcomes;
    try {
      outcomes = await api.dropPaths(paths);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
      return;
    }
    const controller = editorRef.current;
    const newAttachments: MessageAttachment[] = [];
    const missing: string[] = [];
    let refs = 0;
    for (const o of outcomes) {
      if (o.kind === "image") {
        newAttachments.push({
          kind: "image",
          name: o.name,
          media_type: o.media_type,
          data: o.data,
        });
      } else if (o.kind === "text_file") {
        newAttachments.push({
          kind: "text_file",
          name: o.name,
          media_type: o.media_type,
          content: o.content,
        });
      } else if (o.kind === "reference") {
        controller?.insertReference({ path: o.path, kind: "path" });
        refs += 1;
      } else {
        missing.push(o.path);
      }
    }
    if (newAttachments.length) {
      setAttachments((current) => [...current, ...newAttachments]);
    }
    if (refs > 0) controller?.focus();
    if (missing.length) {
      toast.error(`${missing.length} 个文件无法访问`);
    }
  }

  function removeAttachment(index: number) {
    setAttachments((current) => current.filter((_, i) => i !== index));
  }

  // ── + 菜单的目录 / 文件 / 项目操作 ───────────────────────────────────────
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

  async function pickAllowedFolder() {
    setAddMenuOpen(false);
    try {
      const dir = await openDialog({ directory: true, multiple: true });
      if (!dir) return;
      const arr = Array.isArray(dir) ? dir : [dir];
      const merged = [...activeAllowedPaths];
      for (const d of arr) {
        if (typeof d === "string" && !merged.includes(d)) merged.push(d);
      }
      await setPendingAllowedPaths(merged);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function pickAllowedFiles() {
    setAddMenuOpen(false);
    try {
      const file = await openDialog({ directory: false, multiple: true });
      if (!file) return;
      const arr = Array.isArray(file) ? file : [file];
      const merged = [...activeAllowedPaths];
      for (const path of arr) {
        if (typeof path === "string" && !merged.includes(path)) merged.push(path);
      }
      await setPendingAllowedPaths(merged);
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

  async function removeAllowedPath(path: string) {
    try {
      await setPendingAllowedPaths(activeAllowedPaths.filter((d) => d !== path));
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  /**
   * 项目标签浮层里删除允许路径：既从当前对话移除（立即生效），也从项目配置
   * 永久移除（影响该项目以后新建的对话）。
   */
  async function removeProjectAllowedPath(path: string) {
    try {
      await setPendingAllowedPaths(activeAllowedPaths.filter((d) => d !== path));
      if (activeProject) {
        await saveProject(projectInputWithoutAllowedPath(activeProject, path));
      }
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function clearWorkspaceSelections() {
    try {
      await setPendingWorkdir(null);
      await setPendingAllowedPaths([]);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
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

  // ── 副作用 ───────────────────────────────────────────────────────────────
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

  // chip 行展开时鼠标垂直滚轮 → 横向滚动。
  useEffect(() => {
    const el = chipScrollRef.current;
    if (!el) return;
    function onWheel(e: WheelEvent) {
      if (e.deltaY === 0) return;
      e.preventDefault();
      el!.scrollLeft += e.deltaY;
    }
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [activeWorkdir, activeAllowedPaths.length, activeProject]);

  // composerDraft：「放回输入框」按钮把队列项内容写到 store，这里消费并清掉。
  useEffect(() => {
    if (!composerDraft) return;
    const { content, attachments: incoming } = composerDraft;
    if (content) {
      editorRef.current?.appendText(content);
      setHistoryState({ index: null });
    }
    if (incoming.length > 0) {
      setAttachments((prev) => [...prev, ...incoming]);
    }
    clearComposerDraft();
    requestAnimationFrame(() => editorRef.current?.focus());
  }, [composerDraft, clearComposerDraft]);

  // 窗口被快捷键唤起到前台时聚焦输入框。
  useEffect(() => {
    const unlisten = listen("hebbian://focus-chat-input", () => {
      editorRef.current?.focus();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Desktop 原生拖拽：Tauri 默认拦截 webview HTML5 file drop 改发原生事件。
  nativeDropRef.current = handleNativeDrop;
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    const hitInputCard = (x: number, y: number) => {
      const rect = dropCardRef.current?.getBoundingClientRect();
      if (!rect) return false;
      const dpr = window.devicePixelRatio || 1;
      const px = x / dpr;
      const py = y / dpr;
      return px >= rect.left && px <= rect.right && py >= rect.top && py <= rect.bottom;
    };
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === "over") {
          setDraggingFiles(hitInputCard(payload.position.x, payload.position.y));
        } else if (payload.type === "leave") {
          setDraggingFiles(false);
        } else if (payload.type === "drop") {
          const hit = hitInputCard(payload.position.x, payload.position.y);
          setDraggingFiles(false);
          if (hit) nativeDropRef.current(payload.paths);
        }
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const inputDisabled = !!disabled;
  const canSubmit =
    isStreaming || (!disabled && !sending && (!isEmpty || attachments.length > 0));

  return (
    <div className={cn("pl-2 pr-4 pt-0 pb-3 text-sm", isStreaming && "chat-input-streaming")}>
      <div className="pt-0 relative">
        {/* 白色输入卡片 */}
        <div
          ref={dropCardRef}
          className={cn(
            "relative z-10 w-full rounded-3xl border border-input bg-background shadow-[0_-10px_28px_-10px_rgba(0,0,0,0.28),0_0_12px_-6px_rgba(0,0,0,0.12)] focus-within:ring-2 focus-within:ring-ring transition",
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
          {editorSelectionRef && (
            <div className="flex flex-wrap gap-1.5 px-3 pt-2">
              <span
                className="inline-flex max-w-[260px] items-center gap-1 rounded-md bg-primary/10 px-1.5 py-0.5 text-[12px] font-medium text-primary"
                title={formatSelectionRef(editorSelectionRef)}
              >
                <PathTypeIcon path={editorSelectionRef.path} className="h-3 w-3 shrink-0" />
                <span className="truncate">
                  {pathLeaf(editorSelectionRef.path) || editorSelectionRef.path}
                  {editorSelectionRef.startLine === editorSelectionRef.endLine
                    ? `:${editorSelectionRef.startLine}`
                    : `:${editorSelectionRef.startLine}-${editorSelectionRef.endLine}`}
                </span>
                <button
                  type="button"
                  onClick={() => setEditorSelectionRef(null)}
                  className="shrink-0 opacity-50 transition hover:opacity-100"
                  aria-label="移除选区引用"
                  tabIndex={-1}
                >
                  <X className="h-3 w-3" />
                </button>
              </span>
            </div>
          )}
          {activeProject ? (
            <div className="flex flex-wrap gap-1.5 px-3 pt-2">
              <HoverHint
                hint={
                  <span className="flex min-w-[200px] flex-col gap-1 font-mono">
                    {activeWorkdir && (
                      <span className="break-all">{activeWorkdir}</span>
                    )}
                    {activeWorkdir && activeAllowedPaths.length > 0 && (
                      <span className="my-0.5 h-px bg-border" aria-hidden="true" />
                    )}
                    {activeAllowedPaths.map((dir) => (
                      <span
                        key={dir}
                        className="group flex items-center gap-1.5 rounded px-1 -mx-1 hover:bg-muted/60"
                      >
                        <span className="min-w-0 flex-1 break-all">{dir}</span>
                        <button
                          type="button"
                          onClick={() => removeProjectAllowedPath(dir)}
                          className="shrink-0 opacity-0 transition-opacity group-hover:opacity-60 hover:!opacity-100"
                          aria-label="从项目移除该路径"
                        >
                          <X className="h-3 w-3" />
                        </button>
                      </span>
                    ))}
                  </span>
                }
                align="start"
              >
                <span className="inline-flex items-center gap-1 rounded-md bg-primary/10 text-primary px-2 py-0.5 text-[11px] font-medium">
                  <BriefcaseBusiness className="w-3 h-3" />
                  <span className="truncate max-w-[220px]">
                    {activeProject.name}
                  </span>
                </span>
              </HoverHint>
            </div>
          ) : (activeWorkdir || activeAllowedPaths.length > 0) ? (
            <div className="group/chips flex items-center gap-1.5 px-3 pt-2 min-w-0">
              <span
                className="inline-flex items-center gap-1 text-primary text-[11px] font-mono shrink-0 transition-colors"
                title="项目和允许路径（hover 展开）"
                aria-label="项目和允许路径"
              >
                <FolderOpen className="w-3.5 h-3.5" />
                <span className="tabular-nums">
                  {(activeWorkdir ? 1 : 0) + activeAllowedPaths.length}
                </span>
              </span>
              <div
                className={cn(
                  "grid flex-1 min-w-0",
                  "grid-cols-[0fr] group-hover/chips:grid-cols-[1fr]",
                  "transition-[grid-template-columns] duration-200 ease-out"
                )}
              >
                <div
                  ref={chipScrollRef}
                  className={cn(
                    "min-w-0 overflow-x-auto",
                    "[scrollbar-width:none] [&::-webkit-scrollbar]:hidden",
                    "opacity-0 pointer-events-none transition-opacity duration-200",
                    "group-hover/chips:opacity-100 group-hover/chips:pointer-events-auto"
                  )}
                >
                  <div className="flex items-center gap-1.5 flex-nowrap whitespace-nowrap">
                    <HoverHint hint="清空所有路径选择" align="start">
                      <button
                        type="button"
                        onClick={clearWorkspaceSelections}
                        className="inline-flex h-5 w-5 items-center justify-center rounded-md bg-muted text-muted-foreground hover:bg-destructive/10 hover:text-destructive shrink-0"
                        aria-label="清空所有路径选择"
                      >
                        <X className="w-3 h-3" />
                      </button>
                    </HoverHint>
                    {activeWorkdir && (
                      <PathHint path={activeWorkdir}>
                        <span className="inline-flex items-center gap-1 rounded-md bg-primary/10 text-primary px-2 py-0.5 text-[11px] font-mono group shrink-0">
                          <FolderOpen className="w-3 h-3" />
                          <span className="truncate max-w-[200px]">
                            {pathLeaf(activeWorkdir)}
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
                      </PathHint>
                    )}
                    {activeAllowedPaths.map((d) => (
                      <PathHint key={d} path={d}>
                        <span className="inline-flex items-center gap-1 rounded-md bg-muted text-muted-foreground px-2 py-0.5 text-[11px] font-mono group shrink-0">
                          <PathTypeIcon path={d} className="w-3 h-3" />
                          <span className="truncate max-w-[200px]">
                            {pathLeaf(d)}
                          </span>
                          <button
                            type="button"
                            onClick={() => removeAllowedPath(d)}
                            className="opacity-50 hover:opacity-100"
                            aria-label="移除路径"
                          >
                            <X className="w-3 h-3" />
                          </button>
                        </span>
                      </PathHint>
                    ))}
                  </div>
                </div>
              </div>
            </div>
          ) : null}
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
          {/* 富文本编辑器（含 slash / mention / paste 插件，popup 相对本卡片定位） */}
          <div className="relative px-0 py-0">
            <EditorSurface
              onReady={onEditorReady}
              onChange={onEditorChange}
              disabled={inputDisabled}
              placeholder={
                isStreaming
                  ? "正在生成…Enter 排队，Shift+Enter 立即引导"
                  : "输入消息，Enter 发送，Shift+Enter 换行…"
              }
              slashCatalog={slashCatalog}
              onSlashPick={(cmd) => {
                const trailingSpace = cmd.args.length > 0 ? " " : "";
                editorRef.current?.setText(`//${cmd.name}${trailingSpace}`);
                editorRef.current?.focus();
                setHistoryState({ index: null });
              }}
              onEnter={handleEnter}
              onArrow={handleArrow}
              onPasteFiles={(files) => void addFiles(files)}
              onPastePaths={(paths) => void insertPathReferences(paths)}
              mentionFromMenu={mentionFromMenu}
              onMentionClose={() => setMentionFromMenu(false)}
            />
          </div>

          {/* 底部工具条 */}
          <div className="flex items-center justify-between px-2 pb-0">
            <div className="flex items-center gap-1">
              <div className="relative" ref={addMenuRef}>
                <button
                  type="button"
                  onClick={() => setAddMenuOpen((v) => !v)}
                  disabled={inputDisabled}
                  className="h-8 w-8 rounded-md inline-flex items-center justify-center bg-transparent text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:pointer-events-none"
                  title="添加文件 / 项目 / 路径"
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
                      onClick={pickAllowedFiles}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                    >
                      <FilePlus2 className="w-4 h-4 text-muted-foreground" />
                      允许访问文件
                    </button>
                    <button
                      type="button"
                      onClick={pickAllowedFolder}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                    >
                      <FolderOpen className="w-4 h-4 text-muted-foreground" />
                      允许访问文件夹
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setAddMenuOpen(false);
                        setMentionFromMenu(true);
                        editorRef.current?.focus();
                      }}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm hover:bg-accent text-left"
                    >
                      <MessageSquare className="w-4 h-4 text-muted-foreground" />
                      引用对话
                    </button>
                  </div>
                )}
              </div>
              <SlashCommandButton
                disabled={inputDisabled}
                commands={slashCatalog}
                onPick={(cmd) => {
                  const trailingSpace = cmd.args.length > 0 ? " " : "";
                  editorRef.current?.appendText(`//${cmd.name}${trailingSpace}`);
                  editorRef.current?.focus();
                  setHistoryState({ index: null });
                }}
              />
              <ModelPickerButton />
            </div>

            <div className="flex items-center gap-1">
              {(() => {
                const hasDraft = !isEmpty || attachments.length > 0;
                const enqueueMode = isStreaming && hasDraft;
                const onClick = enqueueMode
                  ? () => void enqueueAndClear("tail")
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
                        src={animations.assistantThinking}
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

        <InputDrawer
          open={drawerOpen}
          left={
            <>
              <RunModeChip
                sessionId={currentSession?.id ?? null}
                compact={isStreaming}
              />
              <ReasoningEffortPill compact={isStreaming} />
            </>
          }
          right={
            <div className="flex items-center gap-0.5">
              <ProviderUsageIndicator
                provider={
                  currentSession?.provider_id
                    ? (providersFile.providers.find((p) => p.id === currentSession.provider_id) ?? null)
                    : null
                }
                compact={isStreaming}
                tokenStats={tokenStats}
                model={currentSession?.model ?? ""}
              />
              <TokenStatsPanel
                stats={tokenStats}
                contextUsage={contextUsage}
                compact={isStreaming}
                compacting={compacting}
                onCompact={contextUsage ? () => {
                  if (compacting) return;
                  void runCompact("");
                } : undefined}
              />
            </div>
          }
        />

        {attachments.length > 0 && (
          <div className="mt-1.5 px-1 text-[11px] text-muted-foreground">
            已添加 {attachments.length} 个附件
          </div>
        )}
      </div>
    </div>
  );
}



