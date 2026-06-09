import { useEffect, useLayoutEffect, useMemo, useRef, useState, useCallback } from "react";
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
import { toast } from "sonner";
import { animations } from "@/assets/animations";
import {
  getHistoryDraft,
  type ChatInputHistoryState,
} from "@/desktop/ui/components/chatInputHistory";
import { shouldSubmitChatInput } from "@/desktop/ui/components/chatInputKeyboard";
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
import { AttachmentPreviewStrip } from "@/desktop/ui/components/AttachmentPreviewStrip";
import { PathTypeIcon } from "@/desktop/ui/components/workspaceFields";
import { shouldSuppressBareEnterOnDocument } from "@/desktop/ui/lib/keyboardShortcuts";
import {
  buildSlashCommandCatalog,
  dispatchSlashCommand,
  type SlashCommandMeta,
} from "@/desktop/ui/lib/slashCommands";
import { cn, pathLeaf } from "@/desktop/ui/lib/utils";
import { useStore } from "@/desktop/ui/store/useStore";
import { api } from "@/desktop/bridge/tauri";
import type { MessageAttachment, SkillItem } from "@/desktop/ui/types";

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
  const chipScrollRef = useRef<HTMLDivElement | null>(null);
  const draggingRef = useRef<{ startY: number; startH: number } | null>(null);
  const compositionRef = useRef({
    isComposing: false,
    lastCompositionEndAt: 0,
  });

  const compacting = useStore((s) => s.compacting);
  const compactCurrentSession = useStore((s) => s.compactCurrentSession);
  const enqueueInput = useStore((s) => s.enqueueInput);
  const flushQueuedItem = useStore((s) => s.flushQueuedItem);
  const currentInputQueue = useStore((s) => s.currentInputQueue);
  const composerDraft = useStore((s) => s.composerDraft);
  const clearComposerDraft = useStore((s) => s.clearComposerDraft);
  const tokenStats = useStore(
    (s) => s.currentSession?.token_stats ?? null
  );
  const contextUsage = useStore((s) => s.contextUsage);
  const pendingWorkdir = useStore((s) => s.pendingWorkdir);
  const pendingAllowedPaths = useStore((s) => s.pendingAllowedPaths);
  const setPendingWorkdir = useStore((s) => s.setPendingWorkdir);
  const setPendingAllowedPaths = useStore((s) => s.setPendingAllowedPaths);
  const currentSession = useStore((s) => s.currentSession);
  const projects = useStore((s) => s.projects);

  // activeWorkdir 用 pending 即可：openSession 会同步 pending 值。
  const activeWorkdir = pendingWorkdir;
  const activeAllowedPaths = pendingAllowedPaths;
  const activeProject = currentSession?.project_id
    ? (projects.find((p) => p.id === currentSession.project_id) ?? null)
    : null;

  // 输入框文本 (value) 与附件 (attachments) 故意不绑定 currentSession：
  // 这是用户当前的"草稿"，跨对话保留，切到老对话也不会被清空（老对话已发送的消息
  // 仍在历史里，与草稿互不干扰）。发送时由 submit() 自行清空。

  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const addMenuRef = useRef<HTMLDivElement>(null);
  // 二级设置行（RunMode / Reasoning / 状态）固定展开；不再提供底部折叠小按钮。
  const drawerOpen = true;

  // 架构 §6.1.3 / §8：当前 workdir 下加载的三层 skills，驱动 `//<skill-name>` 命令注册表
  // 和 SlashCommandButton 的 popup 列表。workdir 变化时刷新；失败时退回空数组（仍可用
  // 内置命令）。SkillsPane 在用户导入新 skill 后没有跨组件通知，这里只做 best-effort 刷新——
  // 用户重启 / 切对话 / 改 workdir 都能拿到最新列表，足够低成本场景。
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

  // ── // 命令实时联想 ──────────────────────────────────────────────────────
  const [slashActiveIdx, setSlashActiveIdx] = useState(0);
  const slashSuggestions = useMemo(() => {
    const trimmed = value.trimStart();
    if (!trimmed.startsWith("//")) return [];
    const afterSlash = trimmed.slice(2);
    // 已有空格 = 用户正在输入参数，联想关闭
    if (/\s/.test(afterSlash)) return [];
    const query = afterSlash.toLowerCase();
    if (!query) return slashCatalog;
    return slashCatalog.filter(
      (c) =>
        c.name.toLowerCase().includes(query) ||
        c.desc.toLowerCase().includes(query),
    );
  }, [value, slashCatalog]);

  useEffect(() => {
    setSlashActiveIdx(0);
  }, [slashSuggestions.length]);

  // 滚动 active item 到可视区域
  useEffect(() => {
    const container = slashListRef.current;
    if (!container || slashSuggestions.length === 0) return;
    const active = container.children[slashActiveIdx] as HTMLElement | undefined;
    active?.scrollIntoView({ block: "nearest" });
  }, [slashActiveIdx, slashSuggestions.length]);

  const pickSlashSuggestion = useCallback(
    (cmd: SlashCommandMeta) => {
      const trailingSpace = cmd.args.length > 0 ? " " : "";
      setValue(`//${cmd.name}${trailingSpace}`);
      setHistoryState({ index: null });
      requestAnimationFrame(() => {
        const el = textareaRef.current;
        if (!el) return;
        el.focus();
        const end = el.value.length;
        el.setSelectionRange(end, end);
      });
    },
    [],
  );
  const slashListRef = useRef<HTMLDivElement>(null);

  // ── @ 对话引用联想 ──────────────────────────────────────────────────────
  const [atRefOpen, setAtRefOpen] = useState(false);
  const [atActiveIdx, setAtActiveIdx] = useState(0);
  const atQuery = useMemo(() => {
    if (!atRefOpen) return "";
    // 从光标位置向前找 @，提取 @ 后的文字作为 query
    const el = textareaRef.current;
    if (!el) return "";
    const before = value.slice(0, el.selectionStart);
    const atPos = before.lastIndexOf("@");
    if (atPos === -1) return "";
    return before.slice(atPos + 1);
  }, [atRefOpen, value]);

  useEffect(() => {
    setAtActiveIdx(0);
  }, [atQuery]);

  /** 从 + 菜单打开对话引用弹窗（不依赖 @ 触发）。 */
  const [atRefFromMenu, setAtRefFromMenu] = useState(false);

  const pickConversationRef = useCallback(
    (item: ConversationItem) => {
      const el = textareaRef.current;
      if (atRefFromMenu) {
        // 从加号菜单触发：直接插入路径到光标位置
        const cursor = el ? el.selectionStart : value.length;
        const before = value.slice(0, cursor);
        const after = value.slice(cursor);
        const insertion = item.path + " ";
        setValue(before + insertion + after);
        setAtRefFromMenu(false);
        setAtRefOpen(false);
        // 把路径加入 allowedPaths
        if (!activeAllowedPaths.includes(item.path)) {
          void setPendingAllowedPaths([...activeAllowedPaths, item.path]);
        }
        requestAnimationFrame(() => {
          if (!el) return;
          el.focus();
          const pos = cursor + insertion.length;
          el.setSelectionRange(pos, pos);
        });
        return;
      }
      // 从 @ 触发：替换 @ 及后续 query 为路径
      if (!el) return;
      const before = value.slice(0, el.selectionStart);
      const atPos = before.lastIndexOf("@");
      if (atPos === -1) return;
      const pre = value.slice(0, atPos);
      const post = value.slice(el.selectionStart);
      const insertion = item.path + " ";
      setValue(pre + insertion + post);
      setAtRefOpen(false);
      // 把路径加入 allowedPaths
      if (!activeAllowedPaths.includes(item.path)) {
        void setPendingAllowedPaths([...activeAllowedPaths, item.path]);
      }
      requestAnimationFrame(() => {
        el.focus();
        const pos = atPos + insertion.length;
        el.setSelectionRange(pos, pos);
      });
    },
    [value, activeAllowedPaths, setPendingAllowedPaths, atRefFromMenu],
  );

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

  async function clearWorkspaceSelections() {
    try {
      await setPendingWorkdir(null);
      await setPendingAllowedPaths([]);
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

  // chip 行展开时鼠标垂直滚轮 → 横向滚动；要 active listener 才能 preventDefault
  // 阻止页面跟着滚。dep 上 path 数量保证 chip 行 mount/unmount 时重新绑定。
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
    // `//` 命令系统（架构 §8）：
    // - 内置控制命令（如 //force-automode）→ 本地派发，不发给模型
    // - skill 命令（如 //commit）→ 通过 sendPrompt 改写成 `/<name> [args]` 走正常发送路径
    // - 未知命令 → 错误 toast，绝不降级成 prompt 发给模型（fail-closed）
    if (v.startsWith("//")) {
      const result = await dispatchSlashCommand(
        v,
        {
          sessionId: currentSession?.id ?? null,
          toast,
          sendPrompt: async (text) => {
            setSending(true);
            const queuedAttachments = attachments;
            setAttachments([]);
            try {
              void onSend(text, queuedAttachments).catch((e: any) => {
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
          setValue("");
          setHistoryState({ index: null });
        }
        return;
      }
    }
    setSending(true);
    setValue("");
    const queuedAttachments = attachments;
    setAttachments([]);
    setHistoryState({ index: null });
    try {
      void onSend(v, queuedAttachments).catch((e: any) => {
        toast.error(e?.message || String(e));
      });
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

  /**
   * Shift+Enter / Cmd+Enter：入队首 + 立即引导。
   * 输入框有内容时先入队首再 flush；输入框为空但队列非空时直接 flush 队首——
   * 这样消息发出去后按快捷键仍然有效。
   */
  async function enqueueHeadAndFlush() {
    const v = value.trim();
    const hasDraft = v || attachments.length > 0;
    if (!hasDraft && currentInputQueue.length === 0) return;
    if (hasDraft) {
      enqueueInput(v, attachments, "head");
      setValue("");
      setAttachments([]);
      setHistoryState({ index: null });
    }
    try {
      await flushQueuedItem();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  // composerDraft：「放回输入框」按钮把队列项内容写到 store，这里消费并清掉。
  // 文本以换行追加（避免覆盖正在打的内容），附件直接合并。
  useEffect(() => {
    if (!composerDraft) return;
    const { content, attachments: incoming } = composerDraft;
    if (content) {
      setValue((prev) => (prev ? `${prev}\n${content}` : content));
      setHistoryState({ index: null });
    }
    if (incoming.length > 0) {
      setAttachments((prev) => [...prev, ...incoming]);
    }
    clearComposerDraft();
    requestAnimationFrame(() => textareaRef.current?.focus());
  }, [composerDraft, clearComposerDraft]);

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
    const newValue = e.target.value;
    setValue(newValue);
    setHistoryState({ index: null });
    // @ 触发对话引用：输入 @ 且前面是空格/行首/空 → 打开弹窗
    const cursor = e.target.selectionStart;
    const before = newValue.slice(0, cursor);
    const atPos = before.lastIndexOf("@");
    if (atPos !== -1) {
      const charBeforeAt = atPos > 0 ? before[atPos - 1] : " ";
      if (/[\s]/.test(charBeforeAt) || atPos === 0) {
        const afterAt = before.slice(atPos + 1);
        // @ 后面不能有空格（空格表示 @ 引用结束）
        if (!afterAt.includes(" ") && !afterAt.includes("\n")) {
          if (!atRefOpen) {
            setAtRefOpen(true);
            setAtRefFromMenu(false);
          }
          return;
        }
      }
    }
    if (atRefOpen && !atRefFromMenu) setAtRefOpen(false);
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
   *    判断是文件还是目录，文件追加到 attachments，目录追加到 pendingAllowedPaths
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
    const nonEmptyLines = text.split(/\r?\n/).filter((l) => l.trim()).length;
    // 只有粘贴内容全部由路径行组成才拦截；混有普通文本（日志、代码等）时放行
    if (candidates.length === 0 || candidates.length < nonEmptyLines) return;
    e.preventDefault();
    await attachPathCandidates(candidates);
  }

  /** 把每行（或 file:// 列表）当作潜在路径，全部丢给后端探测；UI 同步刷新 chip / attachments。 */
  async function attachPathCandidates(paths: string[]) {
    const newAttachments: MessageAttachment[] = [];
    const newDirs: string[] = [];
    const missingPaths: string[] = [];
    for (const p of paths) {
      try {
        const res = await api.attachPath(p);
        switch (res.kind) {
          case "file":
            newAttachments.push(res.attachment);
            break;
          case "dir":
            if (
              !activeAllowedPaths.includes(res.path) &&
              !newDirs.includes(res.path)
            ) {
              newDirs.push(res.path);
            }
            break;
          case "missing":
            missingPaths.push(p);
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
        await setPendingAllowedPaths([...activeAllowedPaths, ...newDirs]);
        toast.success(`已添加 ${newDirs.length} 个目录`);
      } catch (e: any) {
        toast.error(e?.message ?? String(e));
      }
    }
    // 找不到的路径当普通文本插入 textarea
    if (missingPaths.length > 0) {
      const insertText = missingPaths.join("\n");
      const el = textareaRef.current;
      if (el) {
        const start = el.selectionStart;
        const end = el.selectionEnd;
        const before = value.slice(0, start);
        const after = value.slice(end);
        const next = before + insertText + after;
        setValue(next);
        requestAnimationFrame(() => {
          const cursor = start + insertText.length;
          el.setSelectionRange(cursor, cursor);
          el.focus();
        });
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
    // ── @ 对话引用键盘导航 ──
    if (atRefOpen) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setAtActiveIdx((i) => i + 1); // 上限由 popup 控制
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setAtActiveIdx((i) => Math.max(i - 1, 0));
        return;
      }
      if (e.key === "Enter" && !e.shiftKey && !e.metaKey) {
        e.preventDefault();
        // 通过 ref 让 popup 拿 items 不合适；改为 dispatching 自定义事件
        // 这里用 setTimeout 0 等 popup 里的 activeIndex 稳定后再触发 pick
        // 我们改为在 popup 上发一个自定义 Event。
        // 简单方案：doc 上 dispatch，popup 里 listen。
        const event = new CustomEvent("conversation-ref-pick-active");
        document.dispatchEvent(event);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setAtRefOpen(false);
        return;
      }
    }

    // ── // 命令联想键盘导航 ──
    if (slashSuggestions.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSlashActiveIdx((i) => Math.min(i + 1, slashSuggestions.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSlashActiveIdx((i) => Math.max(i - 1, 0));
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !e.metaKey)) {
        const picked = slashSuggestions[slashActiveIdx];
        if (picked) {
          e.preventDefault();
          pickSlashSuggestion(picked);
          return;
        }
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setValue("");
        return;
      }
    }

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

    // Shift+Enter / Cmd+Enter = 入队首 + 立即引导（走 PendingInputs，
    // 当前 model_call+tool_call 完成后插队）。
    // 触发条件：streaming 中，或输入框为空但队列里已有待发消息。
    if (
      (isStreaming || currentInputQueue.length > 0) &&
      e.key === "Enter" &&
      (e.shiftKey || e.metaKey) &&
      !compositionRef.current.isComposing &&
      !e.nativeEvent.isComposing &&
      e.nativeEvent.keyCode !== 229
    ) {
      e.preventDefault();
      void enqueueHeadAndFlush();
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

  // 窗口被快捷键唤起到前台时，后端会 emit 此事件，自动聚焦到 chat 输入框。
  useEffect(() => {
    const unlisten = listen("hebbian://focus-chat-input", () => {
      textareaRef.current?.focus();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // streaming 时仍允许输入（Enter 入队 / Shift+Enter 立即入队队首），
  // 只有外部显式 disabled（如未配置 provider）时才禁用。
  const inputDisabled = !!disabled;
  const canSubmit =
    isStreaming ||
    (!disabled && !sending && (!!value.trim() || attachments.length > 0));

  return (
    <div className={cn("pl-2 pr-4 pt-0 pb-3", isStreaming && "chat-input-streaming")}>
      <div className="pt-2 relative">
        {/* 上边框拖拽热区：贴在外壳顶 border 外侧 ~6px 区域，光标变 ns-resize 暗示可拖；
            双击恢复自适应高度。不画可见手柄——保持视觉干净。 */}
        <div
          onPointerDown={onGripPointerDown}
          onPointerMove={onGripPointerMove}
          onPointerUp={onGripPointerUp}
          onPointerCancel={onGripPointerUp}
          onDoubleClick={onGripDoubleClick}
          className="absolute -top-1 left-6 right-6 h-2 cursor-ns-resize z-10"
          title="拖动调整高度（双击恢复自适应）"
          aria-label="拖动调整输入框高度"
        />
        {/* 白色输入卡片：保留独立 rounded-3xl border 完整圆角。`relative z-10` 让它
            盖住下方抽屉的负 margin 钻入部分——视觉上抽屉从白色卡片下端"伸出"。 */}
        <div
          onDrop={onDrop}
          onDragOver={onDragOver}
          onDragLeave={() => setDraggingFiles(false)}
          className={cn(
            // 主投影朝上散得多（投到消息区之上）；副投影 Y=0、spread 收紧——只在卡片四周
            // 烘出薄薄一圈光晕，不向下延伸，避免视觉底比 sidebar 主体卡片低几像素。
            "relative z-10 rounded-3xl border border-input bg-background shadow-[0_-10px_28px_-10px_rgba(0,0,0,0.28),0_0_12px_-6px_rgba(0,0,0,0.12)] focus-within:ring-2 focus-within:ring-ring transition",
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
          {activeProject ? (
            /* activeProject 模式：项目名是高频可见信息，单 chip 不折叠 */
            <div className="flex flex-wrap gap-1.5 px-3 pt-2">
              <HoverHint
                hint={
                  <span className="flex max-w-[320px] flex-col gap-1 font-mono">
                    {activeWorkdir && (
                      <span className="break-words">{activeWorkdir}</span>
                    )}
                    {activeWorkdir && activeAllowedPaths.length > 0 && (
                      <span className="flex flex-col gap-0.5 py-0.5" aria-hidden="true">
                        <span className="h-px bg-border" />
                        <span className="h-px bg-border" />
                      </span>
                    )}
                    {activeAllowedPaths.map((dir) => (
                      <span key={dir} className="break-words">
                        {dir}
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
            /* 散装路径模式：折叠态低调图标 + 数量；hover 整组向右展开。
               用嵌套 grid-cols 0fr→1fr + flex-1 min-w-0 让 chip 容器**充满 chip 行剩余空间**，
               不再被 max-w-[520px] 卡在卡片左半边；内层 flex-nowrap + overflow-x-auto + 鼠标
               滚轮转横滚（在上方 useEffect 里）。 */
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
                    // 隐藏滚动条但保留滚动能力
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
          {/* ── @ 对话引用 popup ── */}
          {atRefOpen && (
            <ConversationRefPopup
              query={atQuery}
              onPick={pickConversationRef}
              onClose={() => setAtRefOpen(false)}
              activeIndex={atActiveIdx}
              onActiveIndexChange={setAtActiveIdx}
            />
          )}
          {/* ── // 命令实时联想 popup ── */}
          {slashSuggestions.length > 0 && (
            <div
              ref={slashListRef}
              className="absolute bottom-full left-0 right-0 mb-1 max-h-[40vh] overflow-y-auto rounded-lg border border-border bg-card shadow-lg z-[100]"
            >
              {slashSuggestions.map((cmd, i) => (
                <button
                  key={`${cmd.kind}:${cmd.name}`}
                  type="button"
                  onMouseDown={(e) => {
                    e.preventDefault();
                    pickSlashSuggestion(cmd);
                  }}
                  onMouseEnter={() => setSlashActiveIdx(i)}
                  className={cn(
                    "w-full flex items-center justify-between gap-3 px-3 py-1.5 text-sm text-left border-l-2 transition-colors",
                    i === slashActiveIdx
                      ? "bg-primary/10 border-l-primary"
                      : "hover:bg-accent/50 border-l-transparent",
                  )}
                >
                  <div className="flex flex-col min-w-0 flex-1">
                    <span className="font-mono text-foreground truncate">
                      //{cmd.name}
                      {cmd.args && (
                        <span className="text-muted-foreground ml-1">{cmd.args}</span>
                      )}
                    </span>
                    {cmd.desc && (
                      <span className="text-[11px] text-muted-foreground truncate">
                        {cmd.desc}
                      </span>
                    )}
                  </div>
                  {cmd.kind === "skill" && cmd.skillSource && (
                    <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
                      {cmd.skillSource}
                    </span>
                  )}
                </button>
              ))}
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
            placeholder={
              isStreaming
                ? "正在生成…Enter 排队，Shift+Enter 立即引导"
                : "输入消息，Enter 发送，Shift+Enter 换行…"
            }
            rows={1}
            style={manual ? { height } : undefined}
            className="chat-input-textarea w-full resize-none bg-transparent px-3 py-3 text-sm outline-none placeholder:text-muted-foreground min-h-[56px] overflow-y-auto"
          />

          {/* 底部工具条：左 = + 菜单 / `//` 命令 / 模型选择，右 = 发送。
              pb-0 让按钮紧贴 DrawerToggle / 白色卡片底边——视觉重心下沉。 */}
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
                      setAtRefFromMenu(true);
                      setAtRefOpen(true);
                      setAtActiveIdx(0);
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
                const insertion = `//${cmd.name}${trailingSpace}`;
                setValue((prev) => {
                  if (!prev || prev.endsWith("\n")) return prev + insertion;
                  return prev + "\n" + insertion;
                });
                setHistoryState({ index: null });
                requestAnimationFrame(() => {
                  const el = textareaRef.current;
                  if (!el) return;
                  el.focus();
                  const end = el.value.length;
                  el.setSelectionRange(end, end);
                });
              }}
            />
            <ModelPickerButton />
            </div>

            <div className="flex items-center gap-1">
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

        {/* 二级抽屉：紧贴白色卡片下方的独立反色卡片。
            左侧运行设置（RunMode / Reasoning），右侧只读状态（工作目录末段 / token / 上下文环）。
            折叠时整个卡片 unmount——这样里面的 popup（Reasoning 上拉菜单等）不会被任何
            overflow-hidden 祖先裁切。 */}
        <InputDrawer
          open={drawerOpen}
          left={
            <>
              <RunModeChip
                sessionId={currentSession?.id ?? null}
                disabled={inputDisabled}
              />
              <ReasoningEffortPill />
            </>
          }
          right={
            <>
              {activeWorkdir && (
                <HoverHint hint={activeWorkdir} align="end">
                  <span className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs text-muted-foreground">
                    <FolderOpen className="w-3 h-3" />
                    <span className="truncate max-w-[160px] font-mono">
                      {pathLeaf(activeWorkdir)}
                    </span>
                  </span>
                </HoverHint>
              )}
              <div className="flex items-center gap-0.5">
                <TokenStatsPanel
                  stats={tokenStats}
                  contextUsage={contextUsage}
                  onCompact={contextUsage ? () => {
                    if (compacting) return;
                    void runCompact("");
                  } : undefined}
                />
              </div>
            </>
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
