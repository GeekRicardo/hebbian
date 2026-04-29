import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Bot, Plus, Star, Trash2, User } from "lucide-react";
import { nanoid } from "nanoid";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label, Textarea } from "@/desktop/ui/components/ui/input";
import {
  AvatarField,
  AvatarPreview,
} from "@/desktop/ui/components/AvatarField";
import { useStore } from "@/desktop/ui/store/useStore";
import type { Prompt } from "@/desktop/ui/types";
import { cn } from "@/desktop/ui/lib/utils";

const AGENT_AVATAR_SUGGESTIONS = [
  "🤖", "💻", "🌐", "✍️", "📚", "🎨", "🧠", "🔬", "💡", "🎯",
  "🧑‍🏫", "🧑‍💼", "🧑‍🎨", "🧑‍💻", "👨‍🔬", "👩‍⚕️", "📝", "🔍", "⚡", "🪄",
];

const USER_AVATAR_SUGGESTIONS = [
  "🙂", "😎", "🧑", "👩", "👨", "🧑‍💻", "🧑‍🎨", "🧑‍🔬", "🧑‍🏫", "✨",
];

export function PromptsDialog() {
  const {
    promptsDialogOpen,
    setPromptsDialogOpen,
    promptsFile,
    prompts,
    userAvatar,
    setUserAvatar,
    upsertPrompt,
    deletePrompt,
    setDefaultPrompt,
  } = useStore();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<Prompt | null>(null);
  const [dirty, setDirty] = useState(false);

  const defaultPromptId = promptsFile.default_prompt_id ?? null;

  useEffect(() => {
    if (!promptsDialogOpen) return;
    const first =
      (selectedId
        ? prompts.find((prompt) => prompt.id === selectedId)
        : undefined) ??
      prompts.find((prompt) => prompt.id === defaultPromptId) ??
      prompts[0];
    if (first) {
      setSelectedId(first.id);
      setDraft({ ...first });
    } else {
      setSelectedId(null);
      setDraft(null);
    }
    setDirty(false);
  }, [promptsDialogOpen, prompts, defaultPromptId]);

  function selectPrompt(p: Prompt) {
    if (dirty && !confirm("当前未保存的修改将丢失，确定切换？")) return;
    setSelectedId(p.id);
    setDraft({ ...p });
    setDirty(false);
  }

  function addPrompt() {
    const p: Prompt = {
      id: nanoid(),
      name: "新 Agent",
      avatar: "✨",
      content: "",
      created_at: Date.now(),
      updated_at: Date.now(),
    };
    setSelectedId(p.id);
    setDraft(p);
    setDirty(true);
  }

  async function handleSave() {
    if (!draft) return;
    try {
      await upsertPrompt(draft);
      toast.success("已保存");
      setDirty(false);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  async function handleDelete() {
    if (!draft || !selectedId) return;
    if (!confirm(`删除 Agent "${draft.name}"？`)) return;
    try {
      await deletePrompt(selectedId);
      toast.success("已删除");
      setSelectedId(null);
      setDraft(null);
      setDirty(false);
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  async function handleSetDefault(id: string) {
    try {
      await setDefaultPrompt(id);
      toast.success("默认 Agent 已更新");
    } catch (e: any) {
      toast.error(e.message || String(e));
    }
  }

  function patch<K extends keyof Prompt>(key: K, value: Prompt[K]) {
    if (!draft) return;
    setDraft({ ...draft, [key]: value });
    setDirty(true);
  }

  return (
    <Dialog
      open={promptsDialogOpen}
      onOpenChange={setPromptsDialogOpen}
      title="Agent 管理"
      description="管理默认 Agent、角色指令和头像"
      size="xl"
      footer={
        <>
          <Button variant="outline" onClick={() => setPromptsDialogOpen(false)}>
            关闭
          </Button>
          {draft && (
            <Button onClick={handleSave} disabled={!dirty}>
              保存
            </Button>
          )}
        </>
      }
    >
      <div className="space-y-4">
        <div className="border-b border-border pb-4">
          <AvatarField
            label="我的头像"
            value={userAvatar}
            onChange={setUserAvatar}
            suggestions={USER_AVATAR_SUGGESTIONS}
            previewFallback={<User className="h-5 w-5" />}
          />
        </div>

        <div className="flex gap-4 min-h-[420px]">
          <div className="w-60 shrink-0 border-r border-border pr-3">
            <div className="flex items-center justify-between mb-2">
              <span className="text-xs font-medium text-muted-foreground">
                Agents
              </span>
              <button
                onClick={addPrompt}
                className="h-6 w-6 inline-flex items-center justify-center rounded hover:bg-accent text-muted-foreground"
                title="添加"
              >
                <Plus className="w-4 h-4" />
              </button>
            </div>
            <ul className="space-y-0.5">
              {prompts.map((p) => {
                const isDefault = defaultPromptId === p.id;
                return (
                  <li
                    key={p.id}
                    onClick={() => selectPrompt(p)}
                    className={cn(
                      "px-3 py-2 rounded-md cursor-pointer flex items-center gap-2",
                      selectedId === p.id
                        ? "bg-accent text-accent-foreground"
                        : "hover:bg-accent/50"
                    )}
                  >
                    <AvatarPreview
                      value={p.avatar}
                      fallback={<Bot className="h-3.5 w-3.5" />}
                      className="h-6 w-6 shrink-0 text-sm"
                      title={p.name}
                    />
                    <span className="text-sm truncate flex-1">{p.name}</span>
                    <button
                      type="button"
                      onClick={(event) => {
                        event.stopPropagation();
                        handleSetDefault(p.id);
                      }}
                      className={cn(
                        "h-6 w-6 inline-flex items-center justify-center rounded text-muted-foreground hover:bg-background",
                        isDefault && "text-amber-500"
                      )}
                      title={isDefault ? "默认 Agent" : "设为默认 Agent"}
                    >
                      <Star
                        className={cn(
                          "h-3.5 w-3.5",
                          isDefault && "fill-current"
                        )}
                      />
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>

          <div className="flex-1 min-w-0 space-y-4">
            {!draft ? (
              <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
                左侧选择或新增一个 Agent
              </div>
            ) : (
              <>
                <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 items-end">
                  <div className="space-y-1.5">
                    <Label>名称</Label>
                    <Input
                      value={draft.name}
                      spellCheck={false}
                      autoCorrect="off"
                      onChange={(e) => patch("name", e.target.value)}
                    />
                  </div>
                  <Button
                    type="button"
                    variant={
                      defaultPromptId === draft.id ? "secondary" : "outline"
                    }
                    onClick={() => handleSetDefault(draft.id)}
                    disabled={dirty}
                    title={dirty ? "保存后可设为默认 Agent" : undefined}
                  >
                    <Star
                      className={cn(
                        "h-3.5 w-3.5",
                        defaultPromptId === draft.id && "fill-current"
                      )}
                    />
                    默认
                  </Button>
                </div>

                <AvatarField
                  label="Agent 头像"
                  value={draft.avatar}
                  onChange={(value) => patch("avatar", value)}
                  suggestions={AGENT_AVATAR_SUGGESTIONS}
                  previewFallback={<Bot className="h-5 w-5" />}
                />

                <div className="space-y-1.5">
                  <Label>系统指令</Label>
                  <Textarea
                    rows={10}
                    value={draft.content}
                    spellCheck={false}
                    autoCorrect="off"
                    onChange={(e) => patch("content", e.target.value)}
                    placeholder="描述这个 Agent 的能力、语气、约束…"
                  />
                </div>
                <div className="pt-2 flex items-center justify-end border-t border-border">
                  <Button variant="destructive" size="sm" onClick={handleDelete}>
                    <Trash2 className="w-3.5 h-3.5" />
                    删除
                  </Button>
                </div>
              </>
            )}
          </div>
        </div>
      </div>
    </Dialog>
  );
}
