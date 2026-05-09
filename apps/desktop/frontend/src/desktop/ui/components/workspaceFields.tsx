import { useMemo } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Folder, Lock, Plus, X } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label } from "@/desktop/ui/components/ui/input";

/** 单选目录输入：path + 「选择」按钮，placeholder 通常用来显示全局默认值。 */
export function DirPicker({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  async function pick() {
    try {
      const dir = await openDialog({ directory: true, multiple: false });
      if (typeof dir === "string") onChange(dir);
    } catch (e: any) {
      toast.error(e.message ?? String(e));
    }
  }
  return (
    <div className="flex items-center gap-2">
      <Input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="flex-1 font-mono text-xs"
      />
      <Button
        variant="outline"
        type="button"
        onClick={pick}
        className="shrink-0"
      >
        <Folder className="w-4 h-4" />
        选择
      </Button>
    </div>
  );
}

/**
 * 一组目录路径管理：增 / 删 / 行内显示。
 *
 * - `dirs`：当前生效的目录列表。
 * - `inheritedDirs`：当 `dirs.length === 0` 时显示这些灰色条目，提示"用全局默认"。
 * - `lockedDirs`：这些条目仅展示，不可删除（用于"对话已开始后锁定的目录"等场景）。
 *   `dirs` 里凡是出现在 `lockedDirs` 中的项，会渲染锁图标且没有移除按钮。
 * - `lockedHint`：当存在 locked 条目时，在列表上方显示的一行提示。
 */
export function DirListField({
  label,
  dirs,
  onChange,
  inheritedDirs,
  emptyHint,
  trailing,
  lockedDirs,
  lockedHint,
}: {
  label: string;
  dirs: string[];
  onChange: (dirs: string[]) => void;
  inheritedDirs?: string[];
  emptyHint?: string;
  trailing?: React.ReactNode;
  lockedDirs?: string[];
  lockedHint?: string;
}) {
  const showingInherited = dirs.length === 0 && (inheritedDirs?.length ?? 0) > 0;
  const lockedSet = useMemo(() => new Set(lockedDirs ?? []), [lockedDirs]);
  const hasLocked = (lockedDirs?.length ?? 0) > 0;

  async function add() {
    try {
      const dir = await openDialog({ directory: true, multiple: true });
      if (!dir) return;
      const arr = Array.isArray(dir) ? dir : [dir];
      const merged = [...dirs];
      for (const d of arr) {
        if (typeof d === "string" && !merged.includes(d)) merged.push(d);
      }
      onChange(merged);
    } catch (e: any) {
      toast.error(e.message ?? String(e));
    }
  }
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <Label>{label}</Label>
        <div className="flex items-center gap-1">
          {trailing}
          <Button variant="outline" size="sm" type="button" onClick={add}>
            <Plus className="w-3.5 h-3.5" />
            添加
          </Button>
        </div>
      </div>
      {hasLocked && lockedHint && !showingInherited && (
        <div className="text-[11px] text-muted-foreground/80 px-1">{lockedHint}</div>
      )}
      {dirs.length === 0 && !showingInherited ? (
        <div className="text-xs text-muted-foreground/70 italic px-2 py-3 border border-dashed border-border rounded-md text-center">
          {emptyHint ?? "暂无"}
        </div>
      ) : (
        <ul className="space-y-1">
          {(showingInherited ? inheritedDirs! : dirs).map((d, i) => {
            const inherited = showingInherited;
            const locked = !inherited && lockedSet.has(d);
            return (
              <li
                key={`${d}-${i}`}
                className={cn(
                  "flex items-center gap-2 px-2 py-1 rounded-md group",
                  inherited
                    ? "bg-muted/20 text-muted-foreground italic"
                    : locked
                      ? "bg-muted/30"
                      : "bg-muted/40"
                )}
                title={locked ? "对话已开始，此目录不能再移除" : undefined}
              >
                {locked ? (
                  <Lock className="w-3.5 h-3.5 shrink-0 text-muted-foreground/70" />
                ) : (
                  <Folder
                    className={cn(
                      "w-3.5 h-3.5 shrink-0",
                      inherited
                        ? "text-muted-foreground/60"
                        : "text-muted-foreground"
                    )}
                  />
                )}
                <span
                  className={cn(
                    "flex-1 truncate text-xs font-mono",
                    locked && "text-muted-foreground"
                  )}
                >
                  {d}
                  {inherited && (
                    <span className="ml-1.5 not-italic font-sans text-[10px] text-muted-foreground/70">
                      （全局默认）
                    </span>
                  )}
                </span>
                {!inherited && !locked && (
                  <button
                    type="button"
                    onClick={() => onChange(dirs.filter((_, j) => j !== i))}
                    className="text-muted-foreground hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity"
                    aria-label="移除"
                  >
                    <X className="w-3.5 h-3.5" />
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/**
 * 工具开关列表。`inherited`：当 `enabled === null` 时（即继承全局）显示这些灰色项作为占位。
 */
export function ToolToggleList({
  label,
  availableTools,
  enabled,
  onChange,
  inheritedEnabled,
  trailing,
}: {
  label: string;
  availableTools: { name: string; description: string }[];
  enabled: string[] | null;
  onChange: (next: string[] | null) => void;
  inheritedEnabled?: string[];
  trailing?: React.ReactNode;
}) {
  const inheriting = enabled === null;
  const effective = inheriting ? (inheritedEnabled ?? []) : enabled!;
  const enabledSet = useMemo(() => new Set(effective), [effective]);

  function toggle(name: string) {
    // 若当前继承全局，先把"全局快照"复制到本地，再修改
    const base = inheriting ? new Set(inheritedEnabled ?? []) : new Set(enabled);
    if (base.has(name)) base.delete(name);
    else base.add(name);
    onChange(Array.from(base));
  }

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <Label>{label}</Label>
        {trailing}
      </div>
      {availableTools.length === 0 ? (
        <div className="text-xs text-muted-foreground/70 italic px-2 py-3 border border-dashed border-border rounded-md text-center">
          没有可选工具
        </div>
      ) : (
        <ul className="space-y-1">
          {availableTools.map((t) => (
            <li
              key={t.name}
              className={cn(
                "flex items-start gap-2 px-2 py-1.5 rounded-md hover:bg-muted/30",
                inheriting && "bg-muted/10"
              )}
            >
              <input
                type="checkbox"
                id={`toolchk-${label}-${t.name}`}
                checked={enabledSet.has(t.name)}
                onChange={() => toggle(t.name)}
                className="h-4 w-4 mt-0.5 rounded"
              />
              <label
                htmlFor={`toolchk-${label}-${t.name}`}
                className="flex-1 cursor-pointer"
              >
                <div
                  className={cn(
                    "text-sm font-medium",
                    inheriting && "text-muted-foreground"
                  )}
                >
                  {t.name}
                  {inheriting && enabledSet.has(t.name) && (
                    <span className="ml-1.5 text-[10px] font-normal text-muted-foreground/70">
                      （全局默认启用）
                    </span>
                  )}
                </div>
                {t.description && (
                  <div className="text-xs text-muted-foreground">
                    {t.description}
                  </div>
                )}
              </label>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function cn(...parts: Array<string | false | null | undefined>) {
  return parts.filter(Boolean).join(" ");
}
