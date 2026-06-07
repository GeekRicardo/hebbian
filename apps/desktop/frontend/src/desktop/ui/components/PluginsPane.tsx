import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import {
  ChevronDown,
  ChevronRight,
  Download,
  Package,
  Plus,
  Store,
  Trash2,
} from "lucide-react";
import { Button } from "@/desktop/ui/components/ui/button";
import { Input, Label } from "@/desktop/ui/components/ui/input";
import { api } from "@/desktop/bridge/tauri";
import type { PluginListItem } from "@/desktop/ui/types";

type MarketplaceRow = { name: string; source: string };

/**
 * 插件管理面板（架构 §6.1.4）。
 *
 * 两段布局：
 * 1. 已添加的 Marketplace——展开后显示该 marketplace 下可安装的 plugin 列表
 * 2. 已安装的 Plugins——名称、版本、组件摘要、卸载按钮
 *
 * 入口在 AppSettingsDialog 的 "插件" tab。
 */
export function PluginsPane() {
  const [marketplaces, setMarketplaces] = useState<MarketplaceRow[]>([]);
  const [plugins, setPlugins] = useState<PluginListItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [addSource, setAddSource] = useState("");
  const [adding, setAdding] = useState(false);
  // marketplace 展开状态：name → 该 marketplace 下的 catalog 条目
  const [expanded, setExpanded] = useState<
    Map<string, { name: string; description?: string | null }[]>
  >(new Map());
  const [expandLoading, setExpandLoading] = useState<string | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const [mkts, plgs] = await Promise.all([
        api.pluginMarketplaceList(),
        api.pluginList(),
      ]);
      setMarketplaces(mkts.map(([name, source]) => ({ name, source })));
      setPlugins(plgs);
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  const installedNames = new Set(plugins.map((p) => p.name));

  async function handleAddMarketplace() {
    const source = addSource.trim();
    if (!source) return;
    setAdding(true);
    try {
      const name = await api.pluginMarketplaceAdd(source);
      toast.success(`已添加：${name}`);
      setAddSource("");
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setAdding(false);
    }
  }

  async function handleRemoveMarketplace(name: string) {
    try {
      await api.pluginMarketplaceRemove(name);
      toast.success(`已删除：${name}`);
      setExpanded((prev) => {
        const next = new Map(prev);
        next.delete(name);
        return next;
      });
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  async function toggleMarketplace(name: string) {
    if (expanded.has(name)) {
      setExpanded((prev) => {
        const next = new Map(prev);
        next.delete(name);
        return next;
      });
      return;
    }
    setExpandLoading(name);
    try {
      // 调后端列出 marketplace 下的 plugin 目录
      const catalog = await api.pluginMarketplaceListPlugins(name);
      setExpanded((prev) => new Map(prev).set(name, catalog));
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setExpandLoading(null);
    }
  }

  async function handleInstall(pluginName: string, marketplace?: string) {
    setInstalling(pluginName);
    try {
      const result = await api.pluginInstall(pluginName, marketplace);
      const parts: string[] = [];
      if (result.skills_count > 0) parts.push(`${result.skills_count} skills`);
      if (result.agents_count > 0) parts.push(`${result.agents_count} agents`);
      if (result.has_hooks) parts.push("hooks");
      if (result.mcp_servers_count > 0)
        parts.push(`${result.mcp_servers_count} MCP servers`);
      const summary = parts.length > 0 ? `（${parts.join("、")}）` : "";
      toast.success(
        `已安装：${result.display_name ?? result.name}${summary}`,
      );
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    } finally {
      setInstalling(null);
    }
  }

  async function handleUninstall(name: string) {
    try {
      await api.pluginUninstall(name);
      toast.success(`已卸载：${name}`);
      await reload();
    } catch (e: any) {
      toast.error(e?.message ?? String(e));
    }
  }

  return (
    <div className="space-y-6">
      {/* ── 已添加的 Marketplace ── */}
      <section className="space-y-2">
        <div className="flex items-center justify-between">
          <Label>插件市场</Label>
          {loading && (
            <span className="text-xs text-muted-foreground">加载中…</span>
          )}
        </div>
        <p className="text-xs text-muted-foreground">
          添加后展开可浏览和安装其中的插件。支持 Claude Code 兼容格式。
        </p>

        {marketplaces.length === 0 && !loading && (
          <p className="text-xs text-muted-foreground">
            还没有添加任何市场
          </p>
        )}

        <div className="space-y-2">
          {marketplaces.map((m) => {
            const isExpanded = expanded.has(m.name);
            const catalog = expanded.get(m.name);
            const isLoadingThis = expandLoading === m.name;
            return (
              <div key={m.name} className="rounded border">
                <div className="flex items-center justify-between gap-2 px-2 py-1.5 bg-muted/30">
                  <button
                    type="button"
                    onClick={() => toggleMarketplace(m.name)}
                    className="flex items-center gap-1.5 min-w-0 flex-1 text-left hover:opacity-80"
                  >
                    {isExpanded ? (
                      <ChevronDown className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                    ) : (
                      <ChevronRight className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                    )}
                    <Store className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                    <div className="min-w-0 flex-1">
                      <div className="text-sm font-medium truncate">
                        {m.name}
                      </div>
                      <div className="text-[10px] text-muted-foreground truncate">
                        {m.source}
                      </div>
                    </div>
                    {isLoadingThis && (
                      <span className="text-xs text-muted-foreground">
                        加载中…
                      </span>
                    )}
                  </button>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => handleRemoveMarketplace(m.name)}
                    title="删除市场"
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                  </Button>
                </div>

                {isExpanded && catalog && (
                  <ul className="divide-y border-t">
                    {catalog.length === 0 && (
                      <li className="px-3 py-2 text-xs text-muted-foreground">
                        暂无插件
                      </li>
                    )}
                    {catalog.map((entry) => {
                      const installed = installedNames.has(entry.name);
                      const isInstallingThis = installing === entry.name;
                      return (
                        <li
                          key={entry.name}
                          className="flex items-center justify-between gap-2 px-3 py-2"
                        >
                          <div className="min-w-0 flex-1">
                            <div className="flex items-center gap-1.5">
                              <Package className="w-3 h-3 text-muted-foreground shrink-0" />
                              <span className="text-sm font-medium truncate">
                                {entry.name}
                              </span>
                              {installed && (
                                <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-500/10 text-green-600 dark:text-green-400">
                                  已安装
                                </span>
                              )}
                            </div>
                            {entry.description && (
                              <p className="text-[11px] text-muted-foreground mt-0.5 line-clamp-2">
                                {entry.description}
                              </p>
                            )}
                          </div>
                          {!installed && (
                            <Button
                              variant="ghost"
                              size="sm"
                              disabled={isInstallingThis}
                              onClick={() =>
                                handleInstall(entry.name, m.name)
                              }
                              title="安装"
                            >
                              <Download className="w-3.5 h-3.5" />
                              <span className="ml-1 text-xs">
                                {isInstallingThis ? "安装中…" : "安装"}
                              </span>
                            </Button>
                          )}
                        </li>
                      );
                    })}
                  </ul>
                )}
              </div>
            );
          })}
        </div>

        {/* 添加 marketplace */}
        <div className="flex items-end gap-2">
          <div className="flex-1 space-y-1">
            <Input
              value={addSource}
              onChange={(e) => setAddSource(e.target.value)}
              placeholder="owner/repo 或 git URL"
              onKeyDown={(e) => {
                if (e.key === "Enter") handleAddMarketplace();
              }}
            />
          </div>
          <Button
            onClick={handleAddMarketplace}
            disabled={adding || !addSource.trim()}
          >
            <Plus className="w-3.5 h-3.5 mr-1" />
            {adding ? "添加中…" : "添加"}
          </Button>
        </div>
      </section>

      {/* ── 已安装的 Plugins ── */}
      <section className="space-y-2">
        <Label>已安装的插件</Label>
        <p className="text-xs text-muted-foreground">
          从市场安装的插件。卸载会清理该插件注入的所有 skills、agents、hooks、MCP servers。
        </p>

        {plugins.length === 0 ? (
          <p className="text-xs text-muted-foreground">暂无</p>
        ) : (
          <div className="space-y-2">
            {plugins.map((p) => (
              <div
                key={p.name}
                className="flex items-center justify-between gap-2 rounded border px-3 py-2"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <Package className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                    <span className="text-sm font-medium truncate">
                      {p.display_name ?? p.name}
                    </span>
                    {p.version && (
                      <span className="text-[10px] text-muted-foreground">
                        v{p.version}
                      </span>
                    )}
                  </div>
                  {p.description && (
                    <p className="text-[11px] text-muted-foreground mt-0.5 line-clamp-2">
                      {p.description}
                    </p>
                  )}
                  <div className="flex items-center gap-2 mt-1">
                    {p.skills_count > 0 && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-blue-500/10 text-blue-600 dark:text-blue-400">
                        {p.skills_count} skills
                      </span>
                    )}
                    {p.agents_count > 0 && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-purple-500/10 text-purple-600 dark:text-purple-400">
                        {p.agents_count} agents
                      </span>
                    )}
                    {p.has_hooks && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/10 text-amber-600 dark:text-amber-400">
                        hooks
                      </span>
                    )}
                    {p.mcp_servers_count > 0 && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-500/10 text-green-600 dark:text-green-400">
                        {p.mcp_servers_count} MCP
                      </span>
                    )}
                  </div>
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => handleUninstall(p.name)}
                  title="卸载"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  <span className="ml-1 text-xs">卸载</span>
                </Button>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}
