import { Fragment, useEffect, useMemo, useRef, useState } from "react";

import { cn } from "@/desktop/ui/lib/utils";
import type { SlashCommandMeta } from "@/desktop/ui/lib/slashCommands";

interface Props {
  disabled?: boolean;
  /** 由上层装配（内置 + 动态 skills）；空数组时按钮仍可点开但 popup 显示提示。 */
  commands: SlashCommandMeta[];
  /** 用户点中一条命令时回调；调用方把 `//<name> ` 写入输入框并 focus 末尾。 */
  onPick: (cmd: SlashCommandMeta) => void;
}

const SOURCE_LABEL: Record<string, string> = {
  global: "global",
  project: "project",
  project_code: "code",
};

/**
 * 工具栏按钮：点击弹出已注册的 `//` 命令清单。
 *
 * 按 §8 的"前端拦截"原则，本组件只负责把命令模板写到输入框——真正派发由
 * `dispatchSlashCommand` 在 ChatInput 的 submit 路径完成。Popup 分两组：
 * - 内置控制命令（builtin）
 * - Skills（按 source 角标区分 global / project / project_code）
 */
export function SlashCommandButton({ disabled, commands, onPick }: Props) {
  const [open, setOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onClick(event: MouseEvent) {
      if (
        wrapperRef.current &&
        !wrapperRef.current.contains(event.target as Node)
      ) {
        setOpen(false);
      }
    }
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [open]);

  const { builtins, skills } = useMemo(() => {
    const builtins: SlashCommandMeta[] = [];
    const skills: SlashCommandMeta[] = [];
    for (const c of commands) {
      if (c.kind === "skill") skills.push(c);
      else builtins.push(c);
    }
    return { builtins, skills };
  }, [commands]);

  return (
    <div className="relative" ref={wrapperRef}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        disabled={disabled}
        className={cn(
          "h-8 w-8 rounded-md inline-flex items-center justify-center bg-transparent text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:pointer-events-none",
          open && "bg-muted text-foreground"
        )}
        title="插入命令"
        aria-label="插入命令"
      >
        <span
          className={cn(
            "inline-flex h-4 w-4 items-center justify-center rounded-[3px]",
            "border border-current text-[9px] leading-none font-medium"
          )}
        >
          /
        </span>
      </button>
      {open && (
        <div
          onClick={(e) => e.stopPropagation()}
          className="absolute bottom-full left-0 mb-1 min-w-[300px] max-w-[420px] max-h-[60vh] overflow-y-auto rounded-lg border border-border bg-card shadow-lg z-[90] animate-slide-up"
        >
          {builtins.length === 0 && skills.length === 0 ? (
            <div className="px-3 py-2 text-xs text-muted-foreground">
              暂无可用命令
            </div>
          ) : (
            <>
              {builtins.length > 0 && (
                <CommandSection title="命令" items={builtins} onPick={(c) => { setOpen(false); onPick(c); }} />
              )}
              {skills.length > 0 && (
                <CommandSection
                  title="Skills"
                  items={skills}
                  onPick={(c) => { setOpen(false); onPick(c); }}
                />
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

function CommandSection({
  title,
  items,
  onPick,
}: {
  title: string;
  items: SlashCommandMeta[];
  onPick: (cmd: SlashCommandMeta) => void;
}) {
  return (
    <div>
      <div className="px-3 pt-2 pb-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/80">
        {title}
      </div>
      {/* hairline 用 `mx-3` 缩进，左右离 popup 边界留 12px；hover bg 仍然全宽到边 */}
      {items.map((cmd, i) => (
        <Fragment key={`${cmd.kind}:${cmd.name}`}>
          {i > 0 && <div className="h-px bg-border mx-3" />}
        <button
          type="button"
          onClick={() => onPick(cmd)}
          className="w-full flex items-center justify-between gap-3 px-3 py-2 text-sm hover:bg-accent text-left"
        >
          <div className="flex flex-col min-w-0 flex-1">
            <span className="font-mono text-foreground truncate">
              //{cmd.name}{cmd.args ? " " : ""}
              {cmd.args && (
                <span className="text-muted-foreground">{cmd.args}</span>
              )}
            </span>
            {cmd.desc && (
              <span className="text-xs text-muted-foreground truncate">
                {cmd.desc}
              </span>
            )}
          </div>
          {cmd.kind === "skill" && cmd.skillSource && (
            <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
              {SOURCE_LABEL[cmd.skillSource] ?? cmd.skillSource}
            </span>
          )}
        </button>
        </Fragment>
      ))}
    </div>
  );
}
