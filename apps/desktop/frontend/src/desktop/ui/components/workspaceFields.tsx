import { useLayoutEffect, useMemo, useRef } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  File,
  FileArchive,
  FileAudio,
  FileCode,
  FileCog,
  FileImage,
  FileJson,
  FilePlus,
  FileSpreadsheet,
  FileText,
  FileVideoCamera,
  Folder,
  FolderPlus,
  Lock,
  Plus,
  X,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { toast } from "sonner";
import { PathHint } from "@/desktop/ui/components/PathHint";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label } from "@/desktop/ui/components/ui/input";
import { cn, pathLeaf } from "@/desktop/ui/lib/utils";

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
  const inputRef = useRef<HTMLInputElement | null>(null);
  useLayoutEffect(() => {
    const input = inputRef.current;
    if (!input || document.activeElement === input) return;
    input.scrollLeft = input.scrollWidth;
  }, [value]);
  return (
    <div className="flex items-center gap-2">
      <Input
        ref={inputRef}
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

const CODE_EXTENSIONS = new Set([
  "c",
  "cc",
  "cpp",
  "cs",
  "css",
  "go",
  "h",
  "hpp",
  "html",
  "java",
  "js",
  "jsx",
  "kt",
  "lua",
  "mjs",
  "php",
  "py",
  "rb",
  "rs",
  "scss",
  "sh",
  "swift",
  "ts",
  "tsx",
  "vue",
]);

const TEXT_EXTENSIONS = new Set([
  "csv",
  "log",
  "md",
  "rst",
  "text",
  "toml",
  "txt",
  "yaml",
  "yml",
]);

const IMAGE_EXTENSIONS = new Set(["avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp"]);
const AUDIO_EXTENSIONS = new Set(["aac", "aiff", "flac", "m4a", "mp3", "ogg", "wav"]);
const VIDEO_EXTENSIONS = new Set(["avi", "m4v", "mov", "mp4", "mpeg", "mpg", "webm"]);
const ARCHIVE_EXTENSIONS = new Set(["7z", "bz2", "dmg", "gz", "rar", "tar", "tgz", "xz", "zip"]);
const SPREADSHEET_EXTENSIONS = new Set(["ods", "tsv", "xls", "xlsm", "xlsx"]);
const CONFIG_FILE_NAMES = new Set([
  ".env",
  ".gitignore",
  ".npmrc",
  "dockerfile",
  "makefile",
  "package.json",
  "pnpm-lock.yaml",
  "tsconfig.json",
]);
const TEXT_FILE_NAMES = new Set(["license", "readme"]);

function pathExtension(path: string) {
  const leaf = pathLeaf(path).toLowerCase();
  const dot = leaf.lastIndexOf(".");
  if (dot <= 0 || dot === leaf.length - 1) return "";
  return leaf.slice(dot + 1);
}

function looksLikeFile(path: string) {
  const leaf = pathLeaf(path).toLowerCase();
  return CONFIG_FILE_NAMES.has(leaf) || TEXT_FILE_NAMES.has(leaf) || pathExtension(path) !== "";
}

function pathIconFor(path: string, forceDirectory: boolean): LucideIcon {
  if (forceDirectory || !looksLikeFile(path)) return Folder;
  const leaf = pathLeaf(path).toLowerCase();
  const ext = pathExtension(path);
  if (leaf === "package.json" || ext === "json" || ext === "jsonl") return FileJson;
  if (CONFIG_FILE_NAMES.has(leaf) || ["lock", "conf", "config", "ini"].includes(ext)) return FileCog;
  if (CODE_EXTENSIONS.has(ext)) return FileCode;
  if (IMAGE_EXTENSIONS.has(ext)) return FileImage;
  if (AUDIO_EXTENSIONS.has(ext)) return FileAudio;
  if (VIDEO_EXTENSIONS.has(ext)) return FileVideoCamera;
  if (ARCHIVE_EXTENSIONS.has(ext)) return FileArchive;
  if (SPREADSHEET_EXTENSIONS.has(ext)) return FileSpreadsheet;
  if (TEXT_EXTENSIONS.has(ext) || TEXT_FILE_NAMES.has(leaf)) return FileText;
  return File;
}

export function PathTypeIcon({
  path,
  forceDirectory = false,
  className,
}: {
  path: string;
  forceDirectory?: boolean;
  className?: string;
}) {
  const Icon = pathIconFor(path, forceDirectory);
  return <Icon className={className} />;
}

/**
 * 一组路径管理：增 / 删 / 行内显示。
 *
 * - `paths`：当前生效的路径列表。
 * - `inheritedPaths`：当 `paths.length === 0` 时显示这些灰色条目，提示"用全局默认"。
 * - `lockedPaths`：这些条目仅展示，不可删除（用于"对话已开始后锁定的路径"等场景）。
 *   `paths` 里凡是出现在 `lockedPaths` 中的项，会渲染锁图标且没有移除按钮。
 * - `lockedHint`：当存在 locked 条目时，在列表上方显示的一行提示。
 */
export function PathListField({
  label,
  paths,
  onChange,
  inheritedPaths,
  emptyHint,
  trailing,
  lockedPaths,
  lockedHint,
  allowFiles = false,
  maxVisibleRows,
}: {
  label: string;
  paths: string[];
  onChange: (paths: string[]) => void;
  inheritedPaths?: string[];
  emptyHint?: string;
  trailing?: React.ReactNode;
  lockedPaths?: string[];
  lockedHint?: string;
  allowFiles?: boolean;
  maxVisibleRows?: number;
}) {
  const showingInherited = paths.length === 0 && (inheritedPaths?.length ?? 0) > 0;
  const lockedSet = useMemo(() => new Set(lockedPaths ?? []), [lockedPaths]);
  const hasLocked = (lockedPaths?.length ?? 0) > 0;
  const visiblePaths = showingInherited ? inheritedPaths! : paths;
  const scrollable = maxVisibleRows != null && visiblePaths.length > maxVisibleRows;

  function merge(nextPaths: string[]) {
    const merged = [...paths];
    for (const path of nextPaths) {
      if (typeof path === "string" && !merged.includes(path)) merged.push(path);
    }
    onChange(merged);
  }

  async function addFolder() {
    try {
      const dir = await openDialog({ directory: true, multiple: true });
      if (!dir) return;
      const arr = Array.isArray(dir) ? dir : [dir];
      merge(arr);
    } catch (e: any) {
      toast.error(e.message ?? String(e));
    }
  }

  async function addFile() {
    try {
      const file = await openDialog({ directory: false, multiple: true });
      if (!file) return;
      const arr = Array.isArray(file) ? file : [file];
      merge(arr);
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
          {allowFiles && (
            <Button
              variant="outline"
              size="icon"
              type="button"
              onClick={addFile}
              title="添加文件"
              aria-label="添加文件"
            >
              <FilePlus className="w-3.5 h-3.5" />
            </Button>
          )}
          <Button
            variant="outline"
            size={allowFiles ? "icon" : "sm"}
            type="button"
            onClick={addFolder}
            title="添加文件夹"
            aria-label="添加文件夹"
          >
            {allowFiles ? (
              <FolderPlus className="w-3.5 h-3.5" />
            ) : (
              <>
                <Plus className="w-3.5 h-3.5" />
                添加
              </>
            )}
          </Button>
        </div>
      </div>
      {hasLocked && lockedHint && !showingInherited && (
        <div className="text-[11px] text-muted-foreground/80 px-1">{lockedHint}</div>
      )}
      {paths.length === 0 && !showingInherited ? (
        <div className="text-xs text-muted-foreground/70 italic px-2 py-3 border border-dashed border-border rounded-md text-center">
          {emptyHint ?? "暂无"}
        </div>
      ) : (
        <ul
          className={cn("space-y-1", scrollable && "overflow-y-auto pr-1")}
          style={scrollable ? { maxHeight: `${maxVisibleRows! * 30}px` } : undefined}
        >
          {visiblePaths.map((d) => {
            const inherited = showingInherited;
            const locked = !inherited && lockedSet.has(d);
            return (
              <li key={d}>
                <PathHint path={d} className="w-full">
                  <span
                    className={cn(
                      "flex items-center gap-2 px-2 py-1 rounded-md group w-full",
                      inherited
                        ? "bg-muted/20 text-muted-foreground italic"
                        : locked
                          ? "bg-muted/30"
                          : "bg-muted/40"
                    )}
                    title={locked ? "对话已开始，此路径不能再移除" : undefined}
                  >
                    {locked ? (
                      <Lock className="w-3.5 h-3.5 shrink-0 text-muted-foreground/70" />
                    ) : (
                      <PathTypeIcon
                        path={d}
                        forceDirectory={!allowFiles}
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
                      {pathLeaf(d)}
                      {inherited && (
                        <span className="ml-1.5 not-italic font-sans text-[10px] text-muted-foreground/70">
                          （全局默认）
                        </span>
                      )}
                    </span>
                    {!inherited && !locked && (
                      <button
                        type="button"
                        onClick={() => onChange(paths.filter((path) => path !== d))}
                        className="text-muted-foreground hover:text-destructive opacity-0 group-hover:opacity-100 transition-opacity"
                        aria-label="移除路径"
                      >
                        <X className="w-3.5 h-3.5" />
                      </button>
                    )}
                  </span>
                </PathHint>
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
