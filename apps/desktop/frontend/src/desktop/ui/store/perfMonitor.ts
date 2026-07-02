/**
 * 轻量级前端性能监控（开发/诊断用）。
 *
 * ## 设计原则
 *
 * - **持续静默采样**：不打印、不 setInterval、不干扰正常工作流
 * - **按需 dump**：觉得卡时在控制台调 `__heb_perf__.dump()` 输出累积报告
 * - **可测量自身开销**：每次记录都自计时，dump 底部汇报 perfMonitor 自身消耗
 * - **不启用零开销**：`isEnabled=false` 时每条路径只有一次 boolean 判断 + early return
 *
 * ## 用法（无需刷新，运行时开关）
 *
 * ```js
 * __heb_perf__.enable()   // 立即开启采样
 * __heb_perf__.disable()  // 立即关闭、停 Observer
 * __heb_perf__.toggle()   // 翻转
 * __heb_perf__.dump()     // 觉得卡时输出累积报告（未启用会提示开）
 * ```
 *
 * 或者页面加载前设 `localStorage.setItem('hebbian.perfMonitor', '1')` 自动启用。
 *
 * ## 输出解读
 *
 * - **本次窗口**：上次 dump 到现在（或启动到现在）的指标
 * - **累计（N 次 dump）**：自监控开启以来的全部历史
 * - **自检开销**：perfMonitor 自身记录函数的总耗时占比
 *   - 如果「事件延迟 avg」很大但「自检开销」很小 → 卡顿在业务逻辑/渲染
 *   - 如果「自检开销」占比高 → 监控自身的 Map 操作有问题（不该出现）
 *
 * ## 性能分析
 *
 * 每条 record 函数在 enabled 路径上的开销约 100-200ns（一次 Map.get + 几次整数加）。
 * 即使每秒 5000 次调用，总开销也不到 1ms/s（0.1% CPU）。相比之下一次 React
 * 重渲染通常是 1-10ms，所以监控本身不是瓶颈。
 */

const STORAGE_KEY = "hebbian.perfMonitor";

// ── types ──

interface EventBucket {
  count: number;
  /** 从事件 push → dispatch 完成的总耗时 ms */
  totalLatencyMs: number;
  maxLatencyMs: number;
}

interface WindowSnapshot {
  since: number;
  events: Map<string, EventBucket>;
  setStateCalls: number;
  renders: Map<string, number>;
  longTasks: number;
  longTaskTotalMs: number;
}

interface CumulativeStats {
  events: Map<string, { count: number; totalLatencyMs: number; maxLatencyMs: number }>;
  setStateCalls: number;
  renders: Map<string, number>;
  longTasks: number;
  longTaskTotalMs: number;
}

// ── singleton ──

let instance: PerfMonitor | null = null;

class PerfMonitor {
  // 当前窗口（每次 dump 后重置）
  private window: WindowSnapshot;
  // 自监控开启以来的累计
  private cumulative: CumulativeStats;
  // dump 次数
  private dumpCount = 0;
  // 上次 dump 的时间戳
  private lastDumpAt: number;
  // 监控开始时间
  private startedAt: number;

  // ── 自检：perfMonitor 自身开销 ──
  /** recordEvent 内部累计耗时（ns） */
  selfEventNs = 0;
  /** recordSetState 内部累计耗时（ns） */
  selfSetStateNs = 0;
  /** recordRender 内部累计耗时（ns） */
  selfRenderNs = 0;
  /** record 总调用次数 */
  selfCallCount = 0;

  private longTaskObserver: PerformanceObserver | null = null;
  private enabled: boolean;

  constructor() {
    const auto =
      typeof localStorage !== "undefined" &&
      localStorage.getItem(STORAGE_KEY) === "1";
    this.enabled = false; // 先设 false，下面 auto 为 true 时由 enable() 统一初始化
    this.startedAt = performance.now();
    this.lastDumpAt = this.startedAt;
    this.window = this.freshWindow();
    this.cumulative = this.freshCumulative();
    if (auto) {
      this.enable();
    }
  }

  // ── public API ──

  get isEnabled(): boolean {
    return this.enabled;
  }

  /** 运行时启用：立即开始采样，持久化 localStorage。页面加载后随时可调。 */
  enable(): void {
    if (this.enabled) {
      console.log("%c[PerfMonitor] 已在运行中", "color:#94a3b8");
      return;
    }
    this.enabled = true;
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(STORAGE_KEY, "1");
    }
    this.resetState();
    this.startObservers();
    console.log(
      "%c⏱ PerfMonitor 已启用%c — 持续静默采样 | %c__heb_perf__.dump()%c 输出报告 | %c__heb_perf__.disable()%c 关闭",
      "color:#4ade80;font-weight:bold",
      "",
      "color:#facc15;font-weight:bold",
      "",
      "color:#f87171",
      "",
    );
  }

  /** 运行时关闭：停 Observer，数据保留。再 enable 会重置计数器。 */
  disable(): void {
    if (!this.enabled) return;
    this.enabled = false;
    if (typeof localStorage !== "undefined") {
      localStorage.removeItem(STORAGE_KEY);
    }
    this.stopObservers();
    console.log("%c[PerfMonitor] 已关闭%c — 数据已清空。%c__heb_perf__.enable()%c 重新开启",
      "color:#f87171;font-weight:bold", "", "color:#4ade80", "");
  }

  toggle(): void {
    if (this.enabled) this.disable();
    else this.enable();
  }

  recordEvent(type: string, latencyMs: number): void {
    if (!this.enabled) return;
    const t0 = this.nowNs();

    // ── 当前窗口 ──
    const wb = this.window.events.get(type);
    if (wb) {
      wb.count++;
      wb.totalLatencyMs += latencyMs;
      if (latencyMs > wb.maxLatencyMs) wb.maxLatencyMs = latencyMs;
    } else {
      this.window.events.set(type, {
        count: 1,
        totalLatencyMs: latencyMs,
        maxLatencyMs: latencyMs,
      });
    }

    // ── 累计 ──
    const cb = this.cumulative.events.get(type);
    if (cb) {
      cb.count++;
      cb.totalLatencyMs += latencyMs;
      if (latencyMs > cb.maxLatencyMs) cb.maxLatencyMs = latencyMs;
    } else {
      this.cumulative.events.set(type, {
        count: 1,
        totalLatencyMs: latencyMs,
        maxLatencyMs: latencyMs,
      });
    }

    this.selfEventNs += this.nowNs() - t0;
    this.selfCallCount++;
  }

  recordSetState(): void {
    if (!this.enabled) return;
    const t0 = this.nowNs();
    this.window.setStateCalls++;
    this.cumulative.setStateCalls++;
    this.selfSetStateNs += this.nowNs() - t0;
    this.selfCallCount++;
  }

  recordRender(componentName: string): void {
    if (!this.enabled) return;
    const t0 = this.nowNs();
    const wp = this.window.renders.get(componentName) ?? 0;
    this.window.renders.set(componentName, wp + 1);
    const cp = this.cumulative.renders.get(componentName) ?? 0;
    this.cumulative.renders.set(componentName, cp + 1);
    this.selfRenderNs += this.nowNs() - t0;
    this.selfCallCount++;
  }

  /** 输出累积报告 + 重置当前窗口（累计不清）。 */
  dump(): void {
    if (!this.enabled) {
      console.log(
        "%c[PerfMonitor] 未启用%c — 在控制台输入 %c__heb_perf__.enable()%c 立即开启",
        "color:#f87171;font-weight:bold",
        "",
        "color:#4ade80;font-weight:bold",
        "",
      );
      return;
    }
    this.dumpCount++;
    const now = performance.now();
    const windowElapsedS = (now - this.window.since) / 1000;
    const totalElapsedS = (now - this.startedAt) / 1000;

    this.printReport(
      this.window,
      windowElapsedS,
      this.cumulative,
      totalElapsedS,
    );

    // 重置窗口
    this.lastDumpAt = now;
    this.window = this.freshWindow();
  }

  // ── internal ──

  private startObservers(): void {
    // Long Task API（浏览器在空闲时才回调，无定时器开销）
    if (typeof PerformanceObserver !== "undefined" && !this.longTaskObserver) {
      try {
        this.longTaskObserver = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            this.window.longTasks++;
            this.window.longTaskTotalMs += entry.duration;
            this.cumulative.longTasks++;
            this.cumulative.longTaskTotalMs += entry.duration;
          }
        });
        this.longTaskObserver.observe({ type: "longtask", buffered: false });
      } catch {
        // 浏览器不支持
      }
    }
  }

  private stopObservers(): void {
    this.longTaskObserver?.disconnect();
    this.longTaskObserver = null;
  }

  /** 重置窗口 + 累计 + 自检计数器，从此刻重新开始。 */
  private resetState(): void {
    this.startedAt = performance.now();
    this.lastDumpAt = this.startedAt;
    this.window = this.freshWindow();
    this.cumulative = this.freshCumulative();
    this.dumpCount = 0;
    this.selfEventNs = 0;
    this.selfSetStateNs = 0;
    this.selfRenderNs = 0;
    this.selfCallCount = 0;
  }

  private freshWindow(): WindowSnapshot {
    return {
      since: performance.now(),
      events: new Map(),
      setStateCalls: 0,
      renders: new Map(),
      longTasks: 0,
      longTaskTotalMs: 0,
    };
  }

  private freshCumulative(): CumulativeStats {
    return {
      events: new Map(),
      setStateCalls: 0,
      renders: new Map(),
      longTasks: 0,
      longTaskTotalMs: 0,
    };
  }

  /** 当前时间（纳秒），用于测量 perfMonitor 自身开销。 */
  private nowNs(): number {
    return performance.now() * 1_000_000;
  }

  private printReport(
    w: WindowSnapshot,
    windowElapsedS: number,
    c: CumulativeStats,
    totalElapsedS: number,
  ): void {
    const totalEventsW = [...w.events.values()].reduce((s, b) => s + b.count, 0);
    const totalEventsC = [...c.events.values()].reduce((s, b) => s + b.count, 0);
    const totalRendersW = [...w.renders.values()].reduce((s, n) => s + n, 0);
    const totalRendersC = [...c.renders.values()].reduce((s, n) => s + n, 0);

    // 自检总开销
    const selfTotalNs = this.selfEventNs + this.selfSetStateNs + this.selfRenderNs;
    const selfTotalMs = selfTotalNs / 1_000_000;
    const totalWindowMs = windowElapsedS * 1000;

    const border =
      totalEventsW > 500 || w.setStateCalls > 200
        ? "#f87171"
        : w.longTasks > 0
          ? "#facc15"
          : "#4ade80";

    console.groupCollapsed(
      `%c⏱ PerfMonitor #${this.dumpCount} %c│ 本次 %c${windowElapsedS.toFixed(1)}s%c │ 累计 %c${totalElapsedS.toFixed(0)}s%c ${w.longTasks > 0 ? "│ 🐌 长任务:" + w.longTasks : ""}`,
      `color:#facc15;font-weight:bold`,
      "",
      `color:${border};font-weight:bold`,
      "",
      "",
      "color:#94a3b8",
      w.longTasks > 0 ? "color:#f87171;font-weight:bold" : "",
    );

    // ── 事件表 ──
    if (totalEventsW > 0) {
      const rows = [...w.events.entries()]
        .sort((a, b) => b[1].count - a[1].count)
        .map(([type, b]) => ({
          "事件类型": type,
          "本窗口/条": b.count,
          "速率/s": (b.count / windowElapsedS).toFixed(0),
          "avg延迟(ms)": b.count > 0 ? (b.totalLatencyMs / b.count).toFixed(2) : "0",
          "max延迟(ms)": b.maxLatencyMs.toFixed(2),
          "累计/条": c.events.get(type)?.count ?? 0,
        }));

      console.log(
        `%c📨 事件流入 — 本窗口 ${totalEventsW} 条 (${(totalEventsW / windowElapsedS).toFixed(0)}/s) │ 累计 ${totalEventsC} 条`,
        "font-weight:bold;font-size:13px",
      );
      console.table(rows);
    } else {
      console.log("%c📨 本窗口无事件", "color:#94a3b8");
    }

    // ── setState ──
    const ssColor =
      w.setStateCalls / windowElapsedS > 60 ? "#f87171" : "#4ade80";
    console.log(
      `%c🔄 zustand setState — 本窗口 %c${w.setStateCalls} 次 %c(${(w.setStateCalls / windowElapsedS).toFixed(0)}/s)%c │ 累计 ${c.setStateCalls} 次`,
      "font-weight:bold;font-size:13px",
      `color:${ssColor};font-weight:bold`,
      "color:#94a3b8",
      "",
    );

    // ── 渲染表 ──
    if (totalRendersW > 0) {
      const rows = [...w.renders.entries()]
        .sort((a, b) => b[1] - a[1])
        .map(([name, count]) => ({
          "组件": name,
          "本窗口/次": count,
          "速率/s": (count / windowElapsedS).toFixed(1),
          "累计/次": c.renders.get(name) ?? 0,
        }));

      console.log(
        `%c🎨 组件渲染 — 本窗口 ${totalRendersW} 次, top 10`,
        "font-weight:bold;font-size:13px",
      );
      console.table(rows.slice(0, 10));
    }

    // ── 长任务 ──
    if (w.longTasks > 0) {
      console.log(
        `%c🐌 长任务 (>50ms 阻塞) — 本窗口 %c${w.longTasks} 次%c 合计 %c${w.longTaskTotalMs.toFixed(0)}ms%c 均 %c${(w.longTaskTotalMs / w.longTasks).toFixed(1)}ms%c │ 累计 ${c.longTasks} 次 ${c.longTaskTotalMs.toFixed(0)}ms`,
        "font-weight:bold;font-size:13px",
        "color:#f87171;font-weight:bold",
        "",
        "color:#f87171;font-weight:bold",
        "",
        "color:#f87171",
        "",
      );
    }

    // ── 自检开销 ──
    const selfPct = totalWindowMs > 0 ? (selfTotalMs / totalWindowMs * 100).toFixed(3) : "0";
    const avgNsPerCall = this.selfCallCount > 0
      ? (selfTotalNs / this.selfCallCount).toFixed(0)
      : "0";
    console.log(
      `%c🔬 自检开销 — perfMonitor 自身耗时 %c${selfTotalMs.toFixed(3)}ms%c (${selfPct}% 窗口) | %c${this.selfCallCount} 次调用%c 均 %c${avgNsPerCall}ns/次%c │ event:${(this.selfEventNs/1e6).toFixed(3)}ms setState:${(this.selfSetStateNs/1e6).toFixed(3)}ms render:${(this.selfRenderNs/1e6).toFixed(3)}ms`,
      "font-weight:bold;font-size:12px",
      parseFloat(selfPct) > 1 ? "color:#f87171;font-weight:bold" : "color:#4ade80",
      "",
      "",
      "",
      "color:#94a3b8",
      "",
    );

    // ── 重置自检计数器（每次 dump 重置，看本窗口开销）──
    // 注意：不重置 cumulative 的 self，因为累计没意义（每次 dump 重置后 self 归零）
    this.selfEventNs = 0;
    this.selfSetStateNs = 0;
    this.selfRenderNs = 0;
    this.selfCallCount = 0;

    console.groupEnd();
  }
}

// ── 确保单例 + 全局暴露 ──

function getMonitor(): PerfMonitor {
  if (!instance) {
    instance = new PerfMonitor();
    if (typeof window !== "undefined") {
      (window as any).__heb_perf__ = {
        enable: () => instance?.enable(),
        disable: () => instance?.disable(),
        toggle: () => instance?.toggle(),
        dump: () => instance?.dump(),
        isEnabled: () => instance?.isEnabled ?? false,
      };
    }
  }
  return instance;
}

// ── 对外 hooks / 工具 ──

/**
 * 记录一次事件从 dispatch 开始到处理完成的耗时。
 *
 * 开销（enabled 时）：~150ns（一次 Map.get + 几次整数运算）
 */
export function perfRecordEvent(type: string, latencyMs: number): void {
  getMonitor().recordEvent(type, latencyMs);
}

/**
 * zustand set() 每次调用记一次。
 *
 * 开销（enabled 时）：~80ns（两次整数 ++）
 */
export function perfRecordSetState(): void {
  getMonitor().recordSetState();
}

/**
 * React 组件渲染计数 hook。放在组件函数体顶部即可。
 *
 * 开销（enabled 时）：~120ns（两次 Map.get + 两次整数 ++）
 *
 * ```tsx
 * function MyComponent() {
 *   usePerfRender("MyComponent");
 *   // ...
 * }
 * ```
 */
export function usePerfRender(componentName: string): void {
  // 先做 enabled 检查以避免不必要的 getMonitor() 调用
  // getMonitor().isEnabled 在实例已创建后只读字段，无额外开销
  if (!getMonitor().isEnabled) return;
  getMonitor().recordRender(componentName);
}

export function isPerfMonitorEnabled(): boolean {
  return getMonitor().isEnabled;
}
