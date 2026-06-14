import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import {
  ChevronDown,
  CornerDownLeft,
  Loader2,
  MessageSquarePlus,
  MessagesSquare,
  Plus,
  Wrench,
  X,
} from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import {
  useBranchStore,
  type Branch,
  type BranchMessage,
  type BranchToolCall,
} from "@/desktop/ui/store/useBranchStore";
import { MarkdownRenderer } from "./MarkdownRenderer";
import { shouldSubmitChatInput } from "./chatInputKeyboard";
import { cn } from "@/desktop/ui/lib/utils";
import type { Provider } from "@/desktop/ui/types";

/**
 * 右侧工作台「旁支对话」tab（架构 §8.5 QuickChat）。
 *
 * 旁支 = 从主对话 fork 出来的临时只读讨论：继承主对话此刻的聊天记录作上下文，只挂
 * Read / Grep，能读代码、查实现、解释调用，但改不了任何文件。后端纯内存、不落盘、关掉即消失。
 *
 * 体验对齐主对话：输入框走和主对话同一套 IME 合成判断（输入法回车只上屏、不提交），
 * 右下角带模型选择器（默认继承主对话，可临时切换，不影响主对话）。一个主对话下可开多条旁支，
 * 顶部子 tab 横条切换 / 新建 / 关闭。
 */
export function BranchChatTab() {
  const sessionId = useStore((s) => s.currentSession?.id ?? null);
  const providerId = useStore((s) => s.currentSession?.provider_id ?? null);
  const model = useStore((s) => s.currentSession?.model ?? null);

  const branches = useBranchStore((s) => s.branches);
  const activeBranchId = useBranchStore((s) => s.activeBranchId);
  const createBranch = useBranchStore((s) => s.createBranch);
  const selectBranch = useBranchStore((s) => s.selectBranch);
  const discardBranch = useBranchStore((s) => s.discardBranch);
  const setBranchInput = useBranchStore((s) => s.setBranchInput);
  const setBranchModel = useBranchStore((s) => s.setBranchModel);
  const sendBranchMessage = useBranchStore((s) => s.sendBranchMessage);

  const sessionBranches = useMemo(
    () =>
      Object.values(branches)
        .filter((b) => b.boundSessionId === sessionId)
        .sort((a, b) => a.createdAt - b.createdAt),
    [branches, sessionId]
  );

  const active =
    activeBranchId && branches[activeBranchId]?.boundSessionId === sessionId
      ? branches[activeBranchId]
      : sessionBranches[0] ?? null;

  useEffect(() => {
    if (!active && activeBranchId && sessionBranches.length > 0) {
      selectBranch(sessionBranches[0].branchId);
    }
  }, [active, activeBranchId, sessionBranches, selectBranch]);

  const newBranch = () => void createBranch(sessionId!, providerId, model);

  if (!sessionId) {
    return (
      <EmptyHint
        icon={<MessagesSquare className="h-5 w-5 opacity-60" />}
        text="先打开一个对话，再从这里开旁支讨论"
      />
    );
  }

  return (
    <div className="flex h-full flex-col">
      {/* 子 tab 横条：每条旁支一个标签 + 新建按钮 */}
      <div className="flex h-8 shrink-0 items-stretch border-b border-border bg-background/50">
        <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto px-1.5 [scrollbar-width:thin]">
          {sessionBranches.map((b) => (
            <BranchSubTab
              key={b.branchId}
              branch={b}
              active={b.branchId === active?.branchId}
              onSelect={() => selectBranch(b.branchId)}
              onClose={() => void discardBranch(b.branchId)}
            />
          ))}
        </div>
        <button
          type="button"
          onClick={newBranch}
          className="grid w-8 shrink-0 place-items-center border-l border-border/40 text-muted-foreground hover:bg-accent hover:text-foreground"
          title="新建旁支讨论"
          aria-label="新建旁支讨论"
        >
          <Plus className="h-3.5 w-3.5" />
        </button>
      </div>

      {active ? (
        <BranchConversation
          key={active.branchId}
          branch={active}
          onInput={(v) => setBranchInput(active.branchId, v)}
          onPickModel={(pid, m) => setBranchModel(active.branchId, pid, m)}
          onSend={() => void sendBranchMessage(active.branchId, active.input)}
        />
      ) : (
        <EmptyHint
          icon={<MessagesSquare className="h-5 w-5 opacity-60" />}
          text="还没有旁支讨论。从当前对话分一条出来，挂只读工具读代码、查实现，不影响主线。"
          action={
            <button
              type="button"
              onClick={newBranch}
              className="mt-1 inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-3 py-1.5 text-[12px] text-foreground hover:bg-accent"
            >
              <MessageSquarePlus className="h-3.5 w-3.5" />
              开一条旁支
            </button>
          }
        />
      )}
    </div>
  );
}

function BranchSubTab({
  branch,
  active,
  onSelect,
  onClose,
}: {
  branch: Branch;
  active: boolean;
  onSelect: () => void;
  onClose: () => void;
}) {
  return (
    <div
      className={cn(
        "group inline-flex h-6 shrink-0 items-center gap-1 rounded px-2 text-[12px] transition-colors",
        active
          ? "bg-background text-foreground shadow-sm"
          : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
      )}
    >
      <button
        type="button"
        onClick={onSelect}
        className="inline-flex items-center gap-1"
        title={branch.title}
      >
        <MessagesSquare className="h-3 w-3 shrink-0" />
        <span className="max-w-[88px] truncate">{branch.title}</span>
        {branch.busy ? (
          <Loader2 className="h-3 w-3 shrink-0 animate-spin" />
        ) : null}
      </button>
      <button
        type="button"
        onClick={onClose}
        className="grid h-4 w-4 place-items-center rounded text-muted-foreground/60 opacity-0 transition hover:bg-destructive/15 hover:text-destructive group-hover:opacity-100"
        title="关闭这条旁支"
        aria-label="关闭这条旁支"
      >
        <X className="h-3 w-3" />
      </button>
    </div>
  );
}

function BranchConversation({
  branch,
  onInput,
  onPickModel,
  onSend,
}: {
  branch: Branch;
  onInput: (value: string) => void;
  onPickModel: (providerId: string, model: string) => void;
  onSend: () => void;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [
    branch.messages,
    branch.liveText,
    branch.liveReasoning,
    branch.liveTools,
    branch.busy,
  ]);

  const empty =
    branch.messages.length === 0 &&
    !branch.busy &&
    !branch.liveText &&
    branch.liveTools.length === 0;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="border-b border-border/40 px-3 py-1.5 text-[11px] text-muted-foreground">
        基于主对话 {branch.inheritedCount} 条记录 · 只读（Read / Grep）
      </div>

      <div ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 py-3">
        {empty ? (
          <EmptyHint
            icon={<MessagesSquare className="h-5 w-5 opacity-60" />}
            text="问点什么——比如「这个函数在哪调用」「解释下这段实现」"
          />
        ) : null}

        {branch.messages.map((m) => (
          <BranchBubble key={m.id} message={m} />
        ))}

        {branch.busy ? (
          <BranchBubble
            message={{
              kind: "assistant",
              id: "live",
              text: branch.liveText,
              reasoning: branch.liveReasoning,
              tools: branch.liveTools,
            }}
            streaming
          />
        ) : null}

        {branch.error ? (
          <div className="rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-[12px] text-destructive">
            {branch.error}
          </div>
        ) : null}
      </div>

      <BranchComposer
        value={branch.input}
        busy={branch.busy}
        providerId={branch.providerId}
        model={branch.model}
        onChange={onInput}
        onPickModel={onPickModel}
        onSend={onSend}
      />
    </div>
  );
}

function BranchBubble({
  message,
  streaming = false,
}: {
  message: BranchMessage;
  streaming?: boolean;
}) {
  if (message.kind === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[88%] whitespace-pre-wrap break-words rounded-xl bg-primary/10 px-3 py-2 text-[13px] leading-5 text-foreground">
          {message.text}
        </div>
      </div>
    );
  }

  const hasBody = message.text.length > 0;
  return (
    <div className="space-y-1.5">
      {message.reasoning ? (
        <div className="rounded-lg border border-border/60 bg-muted/40 px-2.5 py-2 text-[12px] leading-5 text-muted-foreground">
          <MarkdownRenderer markdown={message.reasoning} className="markdown-body" />
        </div>
      ) : null}

      {message.tools.map((t) => (
        <BranchToolChip key={t.id} tool={t} />
      ))}

      {hasBody ? (
        <MarkdownRenderer
          markdown={message.text}
          className="markdown-body text-[13px] leading-6 text-foreground"
        />
      ) : streaming && message.tools.length === 0 ? (
        <div className="flex items-center gap-2 text-[12px] text-muted-foreground">
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
          思考中…
        </div>
      ) : null}
    </div>
  );
}

function BranchToolChip({ tool }: { tool: BranchToolCall }) {
  return (
    <div className="flex items-center gap-2 rounded-full border border-border/60 bg-muted/40 px-3 py-1 text-[12px] text-muted-foreground">
      <Wrench className="h-3 w-3 shrink-0" />
      <span className="font-medium text-foreground/80">{tool.name}</span>
      <span className="min-w-0 flex-1 truncate font-mono text-[11px] opacity-70">
        {tool.argsPreview}
      </span>
      {tool.status === "running" ? (
        <Loader2 className="h-3 w-3 shrink-0 animate-spin" />
      ) : tool.status === "error" ? (
        <span className="shrink-0 text-destructive">失败</span>
      ) : null}
    </div>
  );
}

function BranchComposer({
  value,
  busy,
  providerId,
  model,
  onChange,
  onPickModel,
  onSend,
}: {
  value: string;
  busy: boolean;
  providerId: string | null;
  model: string | null;
  onChange: (value: string) => void;
  onPickModel: (providerId: string, model: string) => void;
  onSend: () => void;
}) {
  // 与主对话输入框同款 IME 合成判断：输入法组合期间回车只上屏，组合刚结束的回车也不误提交。
  const compositionRef = useRef({ isComposing: false, lastCompositionEndAt: 0 });

  const sendDisabled = busy || value.trim().length === 0;
  const handleKeyDown = (e: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    const submit = shouldSubmitChatInput(
      {
        key: e.key,
        shiftKey: e.shiftKey,
        isComposing: e.nativeEvent.isComposing,
        keyCode: e.nativeEvent.keyCode,
        timeStamp: e.timeStamp,
      },
      compositionRef.current
    );
    if (!submit) return;
    e.preventDefault();
    if (!sendDisabled) onSend();
  };

  return (
    <div className="shrink-0 border-t border-border px-2.5 pb-2 pt-2">
      <div className="rounded-lg border border-border bg-background px-2 py-1.5">
        <textarea
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={handleKeyDown}
          onCompositionStart={() => {
            compositionRef.current.isComposing = true;
          }}
          onCompositionEnd={(e) => {
            compositionRef.current.isComposing = false;
            compositionRef.current.lastCompositionEndAt = e.timeStamp;
          }}
          disabled={busy}
          rows={1}
          placeholder="问点什么（旁支只读，不改文件）"
          className="max-h-28 min-h-[30px] w-full resize-none bg-transparent px-1 py-1 text-[13px] leading-5 text-foreground outline-none placeholder:text-muted-foreground/70 disabled:opacity-60"
        />
        <div className="flex items-center justify-between gap-2 pt-0.5">
          <BranchModelPicker
            providerId={providerId}
            model={model}
            onPick={onPickModel}
          />
          <button
            type="button"
            onClick={onSend}
            disabled={sendDisabled}
            className="grid h-7 w-7 shrink-0 place-items-center rounded-md text-muted-foreground transition hover:bg-accent hover:text-foreground disabled:opacity-40"
            title="发送（Enter）"
            aria-label="发送"
          >
            {busy ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <CornerDownLeft className="h-3.5 w-3.5" />
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * 旁支专用的轻量模型选择器：受控（值存在 branch store），只切本旁支的模型，
 * 不碰主对话。不复用 ModelPickerButton——那个绑定 currentSession、点选即改主对话，
 * 语义不符。
 */
function BranchModelPicker({
  providerId,
  model,
  onPick,
}: {
  providerId: string | null;
  model: string | null;
  onPick: (providerId: string, model: string) => void;
}) {
  const providers = useStore((s) => s.providersFile.providers);
  const [open, setOpen] = useState(false);

  const enabled = useMemo(
    () => providers.filter((p) => p.enabled !== false),
    [providers]
  );

  useEffect(() => {
    if (!open) return;
    const onClick = () => setOpen(false);
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [open]);

  const label = model ?? "选择模型";

  return (
    <div className="relative">
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
        aria-expanded={open}
        className="inline-flex max-w-[180px] items-center gap-1 rounded-full px-2 py-1 text-[11px] leading-none text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        title={label}
      >
        <span className="truncate">{label}</span>
        <ChevronDown className="h-3 w-3 opacity-60" />
      </button>
      {open ? (
        <div
          onClick={(e) => e.stopPropagation()}
          className="absolute bottom-full left-0 z-[90] mb-1 max-h-[320px] w-64 overflow-y-auto rounded-lg border border-border bg-card py-1 shadow-lg"
        >
          {enabled.length === 0 ? (
            <div className="px-3 py-2 text-[12px] text-muted-foreground">
              先在设置里启用一个供应商
            </div>
          ) : (
            enabled.map((p) => (
              <BranchProviderModels
                key={p.id}
                provider={p}
                currentProviderId={providerId}
                currentModel={model}
                onPick={(m) => {
                  onPick(p.id, m);
                  setOpen(false);
                }}
              />
            ))
          )}
        </div>
      ) : null}
    </div>
  );
}

function BranchProviderModels({
  provider,
  currentProviderId,
  currentModel,
  onPick,
}: {
  provider: Provider;
  currentProviderId: string | null;
  currentModel: string | null;
  onPick: (model: string) => void;
}) {
  const models =
    provider.models.length > 0
      ? provider.models
      : provider.default_model
      ? [provider.default_model]
      : [];
  if (models.length === 0) return null;
  return (
    <div className="py-0.5">
      <div className="px-3 py-1 text-[10px] font-semibold uppercase text-muted-foreground/70">
        {provider.name}
      </div>
      {models.map((m) => {
        const active = provider.id === currentProviderId && m === currentModel;
        return (
          <button
            key={`${provider.id}-${m}`}
            type="button"
            onClick={() => onPick(m)}
            className={cn(
              "flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-[12px] transition-colors hover:bg-accent",
              active && "bg-primary/10 text-primary"
            )}
          >
            <span className="min-w-0 flex-1 truncate">{m}</span>
            {active ? <span className="shrink-0 text-[11px]">✓</span> : null}
          </button>
        );
      })}
    </div>
  );
}

function EmptyHint({
  icon,
  text,
  action,
}: {
  icon: React.ReactNode;
  text: string;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center text-[12.5px] leading-5 text-muted-foreground">
      {icon}
      <p>{text}</p>
      {action}
    </div>
  );
}
