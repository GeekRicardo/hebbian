import { useEffect, useMemo, useRef } from "react";
import {
  Loader2,
  MessageSquarePlus,
  MessagesSquare,
  Plus,
  X,
} from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import {
  useBranchStore,
  type Branch,
} from "@/desktop/ui/store/useBranchStore";
import { MessageBubble } from "./MessageBubble";
import { AsideComposer } from "./AsideComposer";
import { cn } from "@/desktop/ui/lib/utils";

/**
 * 右侧工作台「旁支对话」tab（架构 §8.5 QuickChat）。
 *
 * 旁支 = 从主对话 fork 出来的临时只读讨论：继承主对话此刻的聊天记录作上下文，只挂
 * Read / Grep，能读代码、查实现、解释调用，但改不了任何文件。后端纯内存、不落盘、关掉即消失。
 *
 * 一个主对话下可开多条旁支，顶部子 tab 横条切换 / 新建 / 关闭；
 * 底部保留轻量输入区继续追问。
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
        <BranchConversation key={active.branchId} branch={active} />
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
        "group relative box-border inline-flex h-6 min-w-[96px] max-w-[132px] flex-[1_1_96px] items-center gap-0 overflow-hidden rounded px-2 pr-[13px] text-[12px] transition-colors",
        active
          ? "bg-background text-foreground shadow-sm"
          : "text-muted-foreground hover:bg-accent hover:text-accent-foreground"
      )}
    >
      <button
        type="button"
        onClick={onSelect}
        className="inline-flex w-0 min-w-0 flex-[1_1_auto] items-center gap-1 overflow-hidden bg-transparent pr-1"
        title={branch.title}
      >
        <MessagesSquare className="h-3 w-3 shrink-0" />
        <span className="block min-w-0 flex-1 truncate text-left">{branch.title}</span>
        {branch.busy ? (
          <Loader2 className="h-3 w-3 shrink-0 animate-spin" />
        ) : null}
      </button>
      <button
        type="button"
        onClick={onClose}
        className="absolute right-px top-1/2 z-[1] grid h-[14px] w-[14px] shrink-0 -translate-y-1/2 place-items-center rounded-[3px] bg-transparent text-inherit opacity-0 transition group-hover:opacity-100"
        title="关闭这条旁支"
        aria-label="关闭这条旁支"
      >
        <X className="h-3 w-3" />
      </button>
    </div>
  );
}

function BranchConversation({ branch }: { branch: Branch }) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const setBranchInput = useBranchStore((s) => s.setBranchInput);
  const setBranchAttachments = useBranchStore((s) => s.setBranchAttachments);
  const sendBranchMessage = useBranchStore((s) => s.sendBranchMessage);
  const cancelBranch = useBranchStore((s) => s.cancelBranch);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [
    branch.messages,
    branch.liveText,
    branch.liveParts,
    branch.busy,
  ]);

  const empty =
    branch.messages.length === 0 &&
    !branch.busy &&
    !branch.liveText &&
    branch.liveParts.length === 0;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="border-b border-border/40 px-3 py-1.5 text-[11px] text-muted-foreground">
        基于主对话 {branch.inheritedCount} 条记录 · 只读（不改文件、不跑命令）
      </div>

      <div ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 py-3">
        {empty ? (
          <EmptyHint
            icon={<MessagesSquare className="h-5 w-5 opacity-60" />}
            text="这条旁支还没有消息"
          />
        ) : null}

        {/* 与主对话同源：直接复用 MessageBubble 渲染 storage Message（reasoning 折叠、
            工具卡片展开 / 实时输出、附件全自动）。旁支不挂 fork/编辑/重生成等重交互回调。 */}
        {branch.messages.map((m) => (
          <MessageBubble key={m.id} message={m} />
        ))}

        {branch.busy ? (
          <MessageBubble
            key="live"
            streaming
            message={{
              id: "live",
              role: "assistant",
              content: branch.liveText,
              created_at: Date.now(),
            }}
            streamingParts={branch.liveParts}
          />
        ) : null}

        {branch.error ? (
          <div className="rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-[12px] text-destructive">
            {branch.error}
          </div>
        ) : null}
      </div>

      <AsideComposer
        value={branch.input}
        onChange={(value) => setBranchInput(branch.branchId, value)}
        attachments={branch.attachments}
        onAttachmentsChange={(attachments) =>
          setBranchAttachments(branch.branchId, attachments)
        }
        busy={branch.busy}
        onSend={(text, attachments) =>
          void sendBranchMessage(branch.branchId, text, attachments)
        }
        onStop={() => void cancelBranch(branch.branchId)}
        placeholder="继续问这条旁支"
      />
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
