import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { ChevronDown, ChevronRight, Trash2 } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { invoke } from "@/desktop/bridge/transport";
import { Button } from "@/desktop/ui/components/ui/button";
import { Dialog } from "@/desktop/ui/components/ui/dialog";
import { Input, Label } from "@/desktop/ui/components/ui/input";
import { cn } from "@/desktop/ui/lib/utils";

/** 跳过 SKILL.md 头部的 YAML frontmatter（`---\n...\n---\n`），只返回正文。 */
function stripFrontmatter(text: string): string {
  if (!text.startsWith("---")) return text;
  const lines = text.split("\n");
  // 第 0 行是 `---`；找下一个 `---` 行作为结束
  let end = -1;
  for (let i = 1; i < lines.length; i++) {
    if (lines[i].trim() === "---") {
      end = i;
      break;
    }
  }
  if (end < 0) return text;
  return lines.slice(end + 1).join("\n").replace(/^\n+/, "");
}

export type SkillItem = {
  name: string;
  description: string;
  path: string;
  source: "global" | "project" | "project_code";
  enabled: boolean;
};

type ScannedSkill = {
  name: string;
  relative_path: string;
  description: string;
  dir_path: string;
};

/**
 * Skills 管理面板：列出当前 workdir 加载的三层 skills，提供三种导入入口
 *  - 本地目录（Tauri dialog 选目录）
 *  - Git 仓库（`git clone --depth=1` 到临时目录后拷贝）
 *  - `~/.claude/skills`（一次性迁移）
 *
 * Scope 由调用方写死：AppSettingsDialog 永远 global、SessionSettingsDialog 永远 project。
 * 无 workdir 时 project scope 自动降级提示，不允许导入到项目层。
 */
export function SkillsPane({
  workdir,
  scope,
}: {
  workdir: string | null;
  scope: "global" | "project";
}) {
  const [skills, setSkills] = useState<SkillItem[]>([]);
  const [claudeSkills, setClaudeSkills] = useState<string[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState(false);
  const [loading, setLoading] = useState(false);
  const [githubUrl, setGithubUrl] = useState("");
  const [githubSubpath, setGithubSubpath] = useState("");
  const [previewSkill, setPreviewSkill] = useState<SkillItem | null>(null);
  const [previewContent, setPreviewContent] = useState<string | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  // 扫描结果选择 modal 状态
  const [scanResults, setScanResults] = useState<ScannedSkill[] | null>(null);
  const [scanSelected, setScanSelected] = useState<Set<string>>(new Set());
  const [scanSource, setScanSource] = useState<
    { kind: "dir"; srcDir: string } | { kind: "github"; repoUrl: string; subpath: string | null } | null
  >(null);
  const [scanLoading, setScanLoading] = useState(false);
  const [scanImporting, setScanImporting] = useState(false);
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());

  const effectiveWorkdir = workdir ?? "";
  const projectUnavailable = scope === "project" && !effectiveWorkdir;

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const wd = effectiveWorkdir || ".";
      const [list, claudeList] = await Promise.all([
        invoke<SkillItem[]>("list_skills", { workdir: wd }),
        invoke<string[]>("list_claude_skills"),
      ]);
      setSkills(list);
      setClaudeSkills(claudeList);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, [effectiveWorkdir]);

  useEffect(() => {
    reload();
  }, [reload]);

  // 同名 skill 是否已存在于当前 scope（→ 已导入）。用 source 区分：
  //  - scope=global → 看 skills 里 source=="global"
  //  - scope=project → 看 skills 里 source=="project"
  const installedNames = useMemo(() => {
    const want = scope === "global" ? "global" : "project";
    return new Set(skills.filter((s) => s.source === want).map((s) => s.name));
  }, [skills, scope]);

  function toggle(name: string) {
    if (installedNames.has(name)) return; // 已导入禁选
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  }

  async function doImport() {
    if (selected.size === 0) {
      toast.error("请选择至少一个 skill");
      return;
    }
    if (projectUnavailable) {
      toast.error("当前对话没绑定项目，先去「目录与工具」选一个项目再来");
      return;
    }
    setImporting(true);
    try {
      const imported = await invoke<{ name: string; overwritten: boolean }[]>(
        "import_claude_skills",
        {
          scope,
          workdir: scope === "project" ? effectiveWorkdir : null,
          names: Array.from(selected),
          overwrite: true,
        }
      );
      toast.success(`已导入 ${imported.length} 个 skill`);
      setSelected(new Set());
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setImporting(false);
    }
  }

  async function doDelete(s: SkillItem) {
    if (s.source === "project_code") {
      toast.error("这条 skill 在你的项目代码里，去源文件改");
      return;
    }
    try {
      const wd = s.source === "project" ? effectiveWorkdir : null;
      await invoke<boolean>("delete_skill", {
        source: s.source,
        name: s.name,
        workdir: wd,
      });
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function doScanFromDir() {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const result = await open({
      directory: true,
      multiple: false,
      title: "选一个目录开始扫描",
    });
    if (!result || Array.isArray(result)) return;
    if (projectUnavailable) {
      toast.error("当前对话没绑定项目，先去「目录与工具」选一个项目再来");
      return;
    }
    setScanLoading(true);
    try {
      const scanned = await invoke<ScannedSkill[]>("scan_skill_dir", {
        srcDir: result,
      });
      if (scanned.length === 0) {
        toast.error("目录里没找到 SKILL.md");
        return;
      }
      setScanResults(scanned);
      setScanSelected(new Set(scanned.map((s) => s.dir_path)));
      setScanSource({ kind: "dir", srcDir: result });
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setScanLoading(false);
    }
  }

  async function doScanFromGithub() {
    const url = githubUrl.trim();
    if (!url) {
      toast.error("请输入 git 仓库 URL");
      return;
    }
    if (projectUnavailable) {
      toast.error("当前对话没绑定项目，先去「目录与工具」选一个项目再来");
      return;
    }
    setScanLoading(true);
    try {
      const subpath = githubSubpath.trim() || null;
      const scanned = await invoke<ScannedSkill[]>("scan_skill_github", {
        repoUrl: url,
        subpath,
      });
      if (scanned.length === 0) {
        toast.error("仓库里没找到 SKILL.md");
        return;
      }
      setScanResults(scanned);
      setScanSelected(new Set(scanned.map((s) => s.dir_path)));
      setScanSource({ kind: "github", repoUrl: url, subpath });
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setScanLoading(false);
    }
  }

  async function confirmScanImport() {
    if (!scanResults || !scanSource) return;
    if (scanSelected.size === 0) {
      toast.error("请至少选一个 skill");
      return;
    }
    // 找到选中的 ScannedSkill 取 dir_path 作为 selectedPaths（后端用 dir_path 做 key）
    const chosen = scanResults.filter((s) => scanSelected.has(s.dir_path));
    const selectedPaths = chosen.map((s) => s.dir_path);
    setScanImporting(true);
    try {
      let imported: { name: string; overwritten: boolean }[];
      if (scanSource.kind === "dir") {
        imported = await invoke("import_skills_from_dir", {
          scope,
          srcDir: scanSource.srcDir,
          workdir: scope === "project" ? effectiveWorkdir : null,
          selectedPaths,
          overwrite: true,
        });
      } else {
        imported = await invoke("import_skills_from_github", {
          scope,
          repoUrl: scanSource.repoUrl,
          subpath: scanSource.subpath,
          workdir: scope === "project" ? effectiveWorkdir : null,
          selectedPaths,
          overwrite: true,
        });
      }
      toast.success(`已导入 ${imported.length} 个 skill`);
      setScanResults(null);
      setScanSource(null);
      setScanSelected(new Set());
      if (scanSource.kind === "github") {
        setGithubUrl("");
        setGithubSubpath("");
      }
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setScanImporting(false);
    }
  }

  function toggleScanItem(dirPath: string) {
    setScanSelected((prev) => {
      const next = new Set(prev);
      if (next.has(dirPath)) next.delete(dirPath);
      else next.add(dirPath);
      return next;
    });
  }

  async function toggleSkillEnabled(s: SkillItem) {
    try {
      await invoke("set_skill_enabled", {
        name: s.name,
        enabled: !s.enabled,
      });
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function openPreview(s: SkillItem) {
    setPreviewSkill(s);
    setPreviewContent(null);
    setPreviewLoading(true);
    try {
      const text = await invoke<string>("read_skill_md", { path: s.path });
      setPreviewContent(stripFrontmatter(text));
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
      setPreviewSkill(null);
    } finally {
      setPreviewLoading(false);
    }
  }

  return (
    <>
    <div className="space-y-5">
      {projectUnavailable && (
        <div className="text-xs px-2 py-1.5 rounded border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300">
          当前对话没绑定项目；要装到「当前项目」需要先去「目录与工具」选一个项目，或换到应用全局设置里管理 Skills。
        </div>
      )}

      <section className="space-y-2">
        <div className="flex items-center justify-between">
          <Label>已加载的 Skills</Label>
          {loading && <span className="text-xs text-muted-foreground">加载中…</span>}
        </div>
        <p className="text-xs text-muted-foreground">
          点击任一条预览内容；勾选框控制是否启用，禁用后模型不会看到这个 skill。
        </p>
        {skills.length === 0 ? (
          <p className="text-xs text-muted-foreground">暂无</p>
        ) : (
          <ul className="space-y-1">
            {skills.map((s) => (
              <li key={`${s.source}:${s.name}`}>
                <div
                  className={cn(
                    "flex items-start gap-2 px-2 py-1.5 rounded border hover:bg-accent/40 transition-colors",
                    !s.enabled && "opacity-50"
                  )}
                >
                  <input
                    type="checkbox"
                    checked={s.enabled}
                    onChange={() => toggleSkillEnabled(s)}
                    className="mt-1 h-3.5 w-3.5 rounded shrink-0"
                    aria-label={s.enabled ? `禁用 ${s.name}` : `启用 ${s.name}`}
                    title={s.enabled ? "启用中（取消勾选 = 禁用）" : "已禁用"}
                  />
                  <span
                    className={cn(
                      "shrink-0 mt-0.5 px-1.5 py-0.5 text-[10px] rounded font-medium",
                      s.source === "global" && "bg-blue-500/15 text-blue-700 dark:text-blue-300",
                      s.source === "project" && "bg-purple-500/15 text-purple-700 dark:text-purple-300",
                      s.source === "project_code" && "bg-zinc-500/15 text-zinc-700 dark:text-zinc-300"
                    )}
                  >
                    {s.source}
                  </span>
                  <button
                    type="button"
                    onClick={() => openPreview(s)}
                    className="min-w-0 flex-1 text-left cursor-pointer"
                  >
                    <div className="font-mono text-sm">{s.name}</div>
                    <div className="text-xs text-muted-foreground line-clamp-2">
                      {s.description}
                    </div>
                  </button>
                  {s.source !== "project_code" && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => doDelete(s)}
                      aria-label={`删除 ${s.name}`}
                    >
                      <Trash2 className="w-3.5 h-3.5" />
                    </Button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2">
        <Label>从本地目录导入</Label>
        <p className="text-xs text-muted-foreground">
          选一个目录，自动扫描里面所有 skill，让你挑哪些导入。
        </p>
        <Button onClick={doScanFromDir} disabled={scanLoading || projectUnavailable}>
          {scanLoading ? "扫描中…" : "选择目录…"}
        </Button>
      </section>

      <section className="space-y-2">
        <Label>从 Git 仓库导入</Label>
        <p className="text-xs text-muted-foreground">
          需要本机装了 git。下载下来扫描完，未导入的部分会自动清理。
        </p>
        <Input
          value={githubUrl}
          onChange={(e) => setGithubUrl(e.target.value)}
          placeholder="https://github.com/user/repo.git"
        />
        <Input
          value={githubSubpath}
          onChange={(e) => setGithubSubpath(e.target.value)}
          placeholder="子路径（可选）例如 skills/ 或 .claude/skills/"
        />
        <Button
          onClick={doScanFromGithub}
          disabled={scanLoading || !githubUrl.trim() || projectUnavailable}
        >
          {scanLoading ? "扫描中…" : "扫描仓库"}
        </Button>
      </section>

      <section className="space-y-2">
        <Label>从 ~/.claude/skills 导入</Label>
        <p className="text-xs text-muted-foreground">
          一次性拷贝到 hebbian；已导入的会自动勾选并禁用。
        </p>
        {claudeSkills.length === 0 ? (
          <p className="text-xs text-muted-foreground">没有可导入的 skill（~/.claude/skills 为空或不存在）</p>
        ) : (
          <>
            <ul className="space-y-1 max-h-48 overflow-y-auto rounded border p-1">
              {claudeSkills.map((name) => {
                const installed = installedNames.has(name);
                const checked = installed || selected.has(name);
                return (
                  <li key={name}>
                    <label
                      className={cn(
                        "flex items-center gap-2 px-2 py-1 rounded text-sm",
                        installed
                          ? "text-muted-foreground cursor-not-allowed"
                          : "hover:bg-accent/40 cursor-pointer"
                      )}
                    >
                      <input
                        type="checkbox"
                        checked={checked}
                        disabled={installed}
                        onChange={() => toggle(name)}
                        className="h-3.5 w-3.5 rounded"
                      />
                      <span className="font-mono">{name}</span>
                      {installed && (
                        <span className="ml-auto text-[10px] text-muted-foreground">
                          已导入
                        </span>
                      )}
                    </label>
                  </li>
                );
              })}
            </ul>
            <Button
              onClick={doImport}
              disabled={importing || selected.size === 0 || projectUnavailable}
            >
              {importing ? "导入中…" : `导入 ${selected.size} 个`}
            </Button>
          </>
        )}
      </section>
    </div>

    {previewSkill && (
      <div className="fixed inset-0 z-[110]">
        <div
          className="absolute inset-0 bg-foreground/30"
          onClick={() => setPreviewSkill(null)}
        />
        <div
          className="absolute inset-3 grid grid-rows-[auto_minmax(0,1fr)] overflow-hidden rounded-xl border border-border bg-background shadow-2xl"
          onClick={(e) => e.stopPropagation()}
        >
          <div className="flex min-h-10 items-center justify-between gap-3 border-b border-border bg-muted/30 px-3">
            <div className="min-w-0 truncate text-[13px]">
              <strong className="font-mono">{previewSkill.name}</strong>
              <span className="ml-2 text-xs text-muted-foreground font-normal">
                {previewSkill.path}
              </span>
            </div>
            <button
              type="button"
              onClick={() => setPreviewSkill(null)}
              className="inline-flex h-7 w-7 items-center justify-center rounded-md border border-border bg-background text-muted-foreground hover:text-foreground"
              aria-label="关闭"
            >
              ×
            </button>
          </div>
          <div className="min-h-0 overflow-auto p-6">
            {previewLoading ? (
              <div className="text-sm text-muted-foreground">加载中…</div>
            ) : previewContent ? (
              <div className="markdown-preview text-[14px] leading-relaxed max-w-3xl mx-auto">
                <ReactMarkdown
                  remarkPlugins={[remarkGfm]}
                  components={{
                    h1: ({ children }) => (
                      <h1 className="mt-4 mb-3 text-2xl font-semibold">{children}</h1>
                    ),
                    h2: ({ children }) => (
                      <h2 className="mt-4 mb-2 text-xl font-semibold">{children}</h2>
                    ),
                    h3: ({ children }) => (
                      <h3 className="mt-3 mb-2 text-lg font-semibold">{children}</h3>
                    ),
                    h4: ({ children }) => (
                      <h4 className="mt-3 mb-2 text-base font-semibold">{children}</h4>
                    ),
                    p: ({ children }) => <p className="my-2">{children}</p>,
                    ul: ({ children }) => (
                      <ul className="list-disc pl-5 my-2 space-y-1">{children}</ul>
                    ),
                    ol: ({ children }) => (
                      <ol className="list-decimal pl-5 my-2 space-y-1">{children}</ol>
                    ),
                    li: ({ children }) => <li className="my-0.5">{children}</li>,
                    code: ({ children, className }) => {
                      const inline = !className;
                      return inline ? (
                        <code className="px-1 py-0.5 rounded bg-muted text-[12.5px] font-mono">
                          {children}
                        </code>
                      ) : (
                        <code className={className}>{children}</code>
                      );
                    },
                    pre: ({ children }) => (
                      <pre className="my-3 p-3 rounded bg-muted overflow-x-auto text-[12.5px] font-mono">
                        {children}
                      </pre>
                    ),
                    blockquote: ({ children }) => (
                      <blockquote className="my-3 pl-3 border-l-2 border-border text-muted-foreground">
                        {children}
                      </blockquote>
                    ),
                    a: ({ children, href }) => (
                      <a
                        href={href}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-primary underline underline-offset-2"
                      >
                        {children}
                      </a>
                    ),
                    hr: () => <hr className="my-4 border-border" />,
                    table: ({ children }) => (
                      <table className="my-3 border-collapse">{children}</table>
                    ),
                    th: ({ children }) => (
                      <th className="border border-border px-2 py-1 bg-muted font-semibold">
                        {children}
                      </th>
                    ),
                    td: ({ children }) => (
                      <td className="border border-border px-2 py-1">{children}</td>
                    ),
                  }}
                >
                  {previewContent}
                </ReactMarkdown>
              </div>
            ) : (
              <div className="text-sm text-muted-foreground">（空）</div>
            )}
          </div>
        </div>
      </div>
    )}

    {scanResults && scanSource && (
      <div className="fixed inset-0 z-[110]">
        <div
          className="absolute inset-0 bg-foreground/30"
          onClick={() => {
            if (!scanImporting) {
              setScanResults(null);
              setScanSource(null);
              setScanSelected(new Set());
            }
          }}
        />
        <div
          className="absolute inset-3 grid grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden rounded-xl border border-border bg-background shadow-2xl"
          onClick={(e) => e.stopPropagation()}
        >
          <div className="flex min-h-10 items-center justify-between gap-3 border-b border-border bg-muted/30 px-3">
            <div className="min-w-0 truncate text-[13px]">
              <strong>选择要导入的 skill</strong>
              <span className="ml-2 text-xs text-muted-foreground font-normal">
                {scanSource.kind === "dir"
                  ? scanSource.srcDir
                  : `${scanSource.repoUrl}${scanSource.subpath ? ` / ${scanSource.subpath}` : ""}`}
                · 共 {scanResults.length} 个
              </span>
            </div>
            <button
              type="button"
              onClick={() => {
                if (!scanImporting) {
                  setScanResults(null);
                  setScanSource(null);
                  setScanSelected(new Set());
                }
              }}
              className="inline-flex h-7 w-7 items-center justify-center rounded-md border border-border bg-background text-muted-foreground hover:text-foreground"
              aria-label="关闭"
              disabled={scanImporting}
            >
              ×
            </button>
          </div>
          <div className="min-h-0 overflow-auto p-4">
            {(() => {
              // 按 relative_path 第一段分组；relative_path 为空（顶层就是单 skill）归到 "."
              const groups = new Map<string, ScannedSkill[]>();
              for (const s of scanResults) {
                const first = s.relative_path.split(/[\\/]/)[0] || ".";
                const arr = groups.get(first) ?? [];
                arr.push(s);
                groups.set(first, arr);
              }
              const entries = Array.from(groups.entries()).sort((a, b) =>
                a[0].localeCompare(b[0])
              );

              const renderItem = (s: ScannedSkill, indent: boolean) => (
                <li key={s.dir_path}>
                  <label
                    className={cn(
                      "flex items-start gap-2 px-2 py-1.5 rounded border hover:bg-accent/40 cursor-pointer",
                      indent && "ml-5"
                    )}
                  >
                    <input
                      type="checkbox"
                      checked={scanSelected.has(s.dir_path)}
                      onChange={() => toggleScanItem(s.dir_path)}
                      className="mt-1 h-3.5 w-3.5 rounded"
                    />
                    <div className="min-w-0 flex-1">
                      <div className="font-mono text-sm">{s.name}</div>
                      {s.description && (
                        <div className="text-xs text-muted-foreground line-clamp-2">
                          {s.description}
                        </div>
                      )}
                    </div>
                  </label>
                </li>
              );

              return (
                <ul className="space-y-1.5">
                  {entries.map(([group, items]) => {
                    // 单条 skill 直接展示，不需要分组头
                    if (items.length === 1) {
                      return renderItem(items[0], false);
                    }
                    const open = expandedGroups.has(group);
                    const allOn = items.every((s) => scanSelected.has(s.dir_path));
                    const someOn = items.some((s) => scanSelected.has(s.dir_path));
                    return (
                      <li key={group} className="space-y-1">
                        <div className="flex items-center gap-2 px-2 py-1.5 rounded border bg-muted/20">
                          <button
                            type="button"
                            onClick={() => {
                              const next = new Set(expandedGroups);
                              if (open) next.delete(group);
                              else next.add(group);
                              setExpandedGroups(next);
                            }}
                            className="shrink-0 text-muted-foreground hover:text-foreground"
                            aria-label={open ? "收起" : "展开"}
                          >
                            {open ? (
                              <ChevronDown className="w-4 h-4" />
                            ) : (
                              <ChevronRight className="w-4 h-4" />
                            )}
                          </button>
                          <input
                            type="checkbox"
                            checked={allOn}
                            ref={(el) => {
                              if (el) el.indeterminate = !allOn && someOn;
                            }}
                            onChange={() => {
                              const next = new Set(scanSelected);
                              for (const s of items) {
                                if (allOn) next.delete(s.dir_path);
                                else next.add(s.dir_path);
                              }
                              setScanSelected(next);
                            }}
                            className="h-3.5 w-3.5 rounded shrink-0"
                            aria-label={allOn ? `取消勾选 ${group} 全部` : `勾选 ${group} 全部`}
                          />
                          <button
                            type="button"
                            onClick={() => {
                              const next = new Set(expandedGroups);
                              if (open) next.delete(group);
                              else next.add(group);
                              setExpandedGroups(next);
                            }}
                            className="min-w-0 flex-1 text-left"
                          >
                            <span className="font-mono text-sm">{group}</span>
                            <span className="ml-2 text-[11px] text-muted-foreground">
                              {items.length} 个
                            </span>
                          </button>
                        </div>
                        {open && (
                          <ul className="space-y-1">
                            {items.map((s) => renderItem(s, true))}
                          </ul>
                        )}
                      </li>
                    );
                  })}
                </ul>
              );
            })()}
          </div>
          <div className="flex items-center justify-between gap-3 border-t border-border bg-muted/30 px-3 py-2">
            <div className="text-xs text-muted-foreground">
              已选 {scanSelected.size} / {scanResults.length}（写入
              {scope === "project" ? "当前项目" : "全局"}）
            </div>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                onClick={() => {
                  if (!scanImporting) {
                    setScanResults(null);
                    setScanSource(null);
                    setScanSelected(new Set());
                  }
                }}
                disabled={scanImporting}
              >
                取消
              </Button>
              <Button
                onClick={confirmScanImport}
                disabled={scanImporting || scanSelected.size === 0}
              >
                {scanImporting ? "导入中…" : `导入 ${scanSelected.size} 个`}
              </Button>
            </div>
          </div>
        </div>
      </div>
    )}
    </>
  );
}
