# 内置终端（TerminalPanel）Spec

> **文档状态**：已实现（2026-06-11，见 changelog 同日条目）。代码编译三态全绿，GUI 交互验收（§8）待 desktop dev 眼验。仍需按 §10 清单把核心契约并入 [架构.md](架构.md) §8，本文件保留为详设附录。
> **调研基础**：fanbox（本机 `~/code/ricardo/other/fanbox`，Electron + xterm.js 5.5 + node-pty，源码级调研）；stagewise（本机 `~/code/ricardo/other/stagewise`，node-pty + xterm.js，源码级调研，见 §1.4）；hebbian 现有 RightSidebar 多 tab 框架、browser popout、`main.tsx` 多 surface 路由、全局快捷键代码（源码级核对）。
> **范围澄清**：本期**不与后台任务（BgTaskRegistry）融合**——终端是纯 Desktop surface 的用户工具，agent 不感知、不读输出、不进协议。后台 Bash PTY 化的融合方案另行讨论（见 §11 留尾巴）。这一「用户终端与 agent shell 两套独立生态」的取舍，被 stagewise 同款印证（§1.4）。

---

## 0. 一句话总结

加一个**全局单例**的终端面板（不随会话切换、不绑 session）：右侧 sidebar 第「终端」tab 是它的内嵌视图，**内部可开多个子终端**，并能**像内置浏览器一样弹出成独立窗口**（popout）。前端 xterm.js 渲染、Rust 端 portable-pty 起真实 shell；**终端聚焦时除 Cmd 系外所有按键透传 PTY**（Ctrl-C/B/F、Alt+←/→ 等终端惯用键不被应用快捷键截胡），选中文本自动复制。

PTY 进程在 Rust 端是**单一真理源**，sidebar 内嵌视图与 popout 窗口都是同一组 PTY 的 xterm 视图（同一时刻只有一个视图活跃，另一个让位，避免双 resize 打架）。

不碰 agent-core、不碰协议、不碰 storage。hebweb / heb CLI 无此面板（与内置浏览器同先例：Tauri native 能力，surface 不对称已有先例）。

---

## 1. 调研结论摘要（fanbox）

fanbox 的终端 = `@xterm/xterm 5.5` + `node-pty`，Electron IPC 通信。架构可平移，PTY 库需换（node-pty 是 Node 原生模块，Tauri 用不了）。

### 1.1 可直接照搬的设计

| 设计点 | fanbox 出处 | hebbian 对应 |
|---|---|---|
| IPC 协议五件套：spawn / input / resize / kill + data / exit 推流 | `electron/main.js:260-310` | Tauri 命令 + 事件，§4 |
| spawn env 兜底：`TERM=xterm-256color` + locale 非 UTF-8 时强设 `LANG=zh_CN.UTF-8`（GUI app 不继承 shell locale，中文路径变 `\M-^@` 乱码） | `electron/main.js:264-266` | 同款，Tauri app 同样不继承 |
| shell 选择：`$SHELL` 兜底 `/bin/zsh`，不传 args（interactive 非 login） | `electron/main.js:262` | 同款 |
| addon 组合：fit（自适应）+ unicode11（CJK 宽字符，必配 `unicode.activeVersion = '11'`）+ webgl（加速，contextLoss 时 dispose 回退 DOM） | `app.js:~250-295` | 同款 |
| `display:none` 期间滚动区算矮一屏（xterm 5.x upstream #5339）：切回时 `viewport.syncScrollArea(true)` + `scrollToBottom()` | `app.js:~430` | 我们 sidebar 切 tab 正是 display:none，必踩 |
| 多行粘贴用 bracketed paste 包裹（`\x1b[200~…\x1b[201~`），防 shell 逐行执行 | `app.js:~133` | 同款 |
| 字体读 CSS 变量 `--font-mono`，主题对象随应用皮肤切换 | `app.js:~252` | 同款 |
| 多 tab session 模型：`{id, xterm, fit, host, dead, title, startDir}` | `app.js:~281` | §6 |

### 1.2 不照搬的部分

- node-pty → **portable-pty**（wezterm 作者维护，macOS/Linux openpty + Windows ConPTY 同一 API，Tauri 生态事实标准）
- fanbox 的 agent 态势感知（busy/idle 检测、完成通知）、路径链接化、终端跟随浏览：本期不做，留 §11
- fanbox 无「选中自动复制」，该特性参考 iTerm2 copy-on-select 自行实现（§5.4）

### 1.3 技术选型定论

- **前端 xterm.js（`@xterm/xterm` 5.x）而非 ghostty-web**：`frontend/package.json:17` 里躺着一个零引用的 `ghostty-web ^0.4.0`，它太早期（无 fit/unicode/search addon 生态、无 `attachCustomKeyEventHandler` 对等物——§5 的键盘策略强依赖这个 API）。**实施时顺手移除 ghostty-web 依赖**。
- **Rust 端 portable-pty**，放 `apps/desktop` 层（不进 agent-core：终端不是 agent 能力，是窗口部件，与 browser 模块同层同理）。

### 1.4 stagewise 调研结论（佐证「不与 agent 融合」的取舍）

stagewise（AGPL-3.0，仅借鉴设计思路不抄代码）2026-05 发布了内置终端，与本设计高度同构，关键发现：

- **它的用户终端是真实 PTY 交互 shell**（node-pty + xterm.js + `@xterm/headless` 做后端序列化），不是日志展示——和本 spec 选型一致。
- **它有意把「用户终端」与「agent shell」做成两套完全独立的 PTY 生态**：
  - 用户终端（`TerminalService`）：用户打字/Ctrl+C 直接写 PTY，输出只进 UI 缓冲，**agent 看不到**。
  - agent shell（`ShellService` + agent 的 `createShellSession` 工具）：agent 自己创建、自己读结果，**与用户终端零交叉**。
  - 用户终端可选地「绑定」到某个 agent 实例，但**仅用于 UI 分组**（决定终端 tab 显示在哪个 agent 名下），**不是执行/数据通道**——agent 读不到用户在终端里输入或输出的任何内容。
- **结论对本设计的意义**：业界先行者验证了「用户终端独立于 agent」是成熟的产品取舍——用户要的是「不离开 app 就能起 dev server / 看日志 / 跑命令」，不是「让 agent 接管我的终端」。这正是本期方案。hebbian 的 agent 命令执行已有自己的 Bash 工具 + BgTaskRegistry 体系（对应 stagewise 的 agent shell），本终端就是对应它的「用户终端」一侧。融合（让 agent 读用户终端 / 后台 Bash PTY 化进终端显示）是另一个更大的命题，stagewise 都没做，本期同样不做（§11）。
- **一处差异**：stagewise 终端是 window-scoped（每个 stagewise 窗口一份）；本设计是 **app 全局单例**（§3）——因为 hebbian 主体是单窗口多会话，终端作为开发者工具应跨会话复用（A 会话起的 dev server 切到 B 会话仍在跑、仍可见），而不是每会话一份。

---

## 2. 范围

### 做

- **全局单例终端**：整个 app 一组终端，不随会话切换、不绑 session（§3）
- RightSidebar 第「终端」tab 作为内嵌视图，内部多子终端 tab（新建 / 切换 / 关闭）
- **popout 独立窗口**：像内置浏览器一样把终端弹出成独立可缩放窗口；PTY 单一真理源，内嵌与 popout 共享同一组终端（§4.5 / §6.5）
- 真实 PTY shell（用户的 `$SHELL`，interactive），新建子终端时 cwd 默认当前会话 workdir（无则 `$HOME`）
- 终端键盘焦点策略（§5，本 spec 核心）
- 选中自动复制、粘贴安全（bracketed paste）
- Rust 端 raw 输出 ring buffer，前端重挂载 / webview reload / popout 切换后回放恢复画面

### 不做（本期）

- 与 BgTaskRegistry / 后台 Bash 融合、agent 读终端输出（§11，stagewise 也不做，见 §1.4）
- 终端内容持久化落盘（app 退出即终止所有 PTY，无恢复）
- 内嵌视图与 popout 窗口**同时**各显一个活跃 xterm 镜像同一 PTY（双 resize 会打架）——本期用「让位」模型，同一时刻一个活跃（§4.5）
- 终端内搜索（search addon）、路径点击跳转、分屏 —— P1 候选
- hebweb 对应面板（无 PTY 通道；浏览器场景另需 ws 推流，融合期一并考虑）

---

## 3. 架构定位与模块归属

```
apps/desktop/src/terminal/mod.rs        ← PTY 管理（portable-pty），全局 TerminalState，Tauri 命令 + 事件 + popout 窗口
apps/desktop/frontend/.../TerminalSurface.tsx ← 终端 UI 主体（xterm 渲染 + 键盘策略 + 多子终端 tab），内嵌与 popout 共用
apps/desktop/frontend/.../RightSidebar.tsx    ← 接「终端」tab，内嵌挂 TerminalSurface
apps/desktop/frontend/src/main.tsx            ← +`?terminal-popout=1` surface 路由（popout 窗口加载它）
```

### 3.1 全局单例（核心理念）

终端**不绑 session**，整个 app 一份 `TerminalState`（Rust 端全局 `manage`）。理由：

- 终端是开发者工具，使用直觉是「跨会话长存」——A 会话里起的 `pnpm dev` 切到 B 会话仍在跑、仍能看输出，而不是切会话就没了。
- 与现有 **browser 面板（session-scoped，每对话一个浏览器实例）刻意不同**：浏览器内容是「这个对话在讨论的页面」，属于会话上下文；终端是「我这台机器上的活儿」，属于工作台。两者归属语义不同，不强行对齐。
- 全局单例也简化实现：Rust 端不需要 session 路由表（对比 BgTaskRegistry 的 `registry_for_session`），就一个 `HashMap<term_id, TerminalInstance>`。

### 3.2 PTY 单一真理源（内嵌 ↔ popout）

PTY 进程只在 Rust 端存在一份。前端无论是 sidebar 内嵌视图还是 popout 独立窗口，都是「连到某个 `term_id` 的 xterm 视图」。切换形态（收进 sidebar / 弹出 popout）不重启 PTY、不丢 scrollback——新视图 `terminal_attach` 回放 ring buffer 即可重建画面。这与 browser popout「在新窗口重新加载同一 URL」本质不同（浏览器无状态，终端有 PTY 状态），故**不复用 browser 的 popout 代码，但复用其交互范式与 `WebviewUrl::App` 多窗口加载先例**（`lib.rs:2587` log-viewer 窗口同款）。

设计影响评估（按 CLAUDE.md 5 问）：

1. **与架构.md 相悖？** 否。纯 surface 部件，不触 §0 / §12 / §13 任何原则。
2. **符合既定设计？** 是。与 §8 browser 模块同层同模式（Rust 模块 + Tauri 命令 + `xxx://` 事件 + 前端面板 + popout 窗口）；多 surface 路由复用 `main.tsx` 既有 `?log-viewer` 机制。
3. **需改架构.md？** 需要：§8 追加「内置终端」一节（本 spec §10 列并入点）。不新增协议字段 / 工具 / 模式。
4. **影响其他模块？** 无。不碰 protocol / EventPayload / agent-core / storage。前端仅 RightSidebar 加 tab + 新组件 + main.tsx 加一个 surface 分支。
5. **取舍**：PTY 进程随 app 退出终止、不持久化——接受，用户终端的合理预期；与后台任务彻底解耦换来零架构风险，代价是 agent 起的 dev server 看不进终端（融合期再解，stagewise 同样不做）。全局单例 vs session-scoped 的选择见 §3.1，与 browser 刻意不对齐。

---

## 4. Rust 端设计（apps/desktop/src/terminal/mod.rs）

### 4.1 状态（全局单例）

```rust
struct TerminalInstance {
    id: String,                      // "term_001" 自增
    master: Box<dyn MasterPty>,      // resize 用
    writer: Box<dyn Write + Send>,   // 前端输入写入
    child: Box<dyn Child + Send>,    // kill 用
    scrollback: VecDeque<u8>,        // raw ring buffer（上限 1 MiB），attach 回放用
    cwd: String,
    last_size: (u16, u16),           // 让位模型下记住最近一次 resize（§4.5）
}

/// app 全局唯一（tauri manage），不按 session 路由——见 §3.1。
pub struct TerminalState {
    terminals: Mutex<HashMap<String, TerminalInstance>>,
    order: Mutex<Vec<String>>,       // 子终端 tab 顺序（全局一份）
    active_view: Mutex<ViewOwner>,   // 当前活跃视图：Embedded | Popout（§4.5）
    counter: AtomicU64,
}

enum ViewOwner { Embedded, Popout }
```

### 4.2 Tauri 命令

| 命令 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `terminal_open` | `cwd: Option<String>, cols, rows` | `term_id` | openpty + spawn `$SHELL`；env 按 §1.1 兜底；追加进 `order` |
| `terminal_write` | `id, data: String` | — | 写 PTY stdin。每键一次 invoke，Tauri invoke 开销可忽略 |
| `terminal_resize` | `id, cols, rows` | — | `master.resize`；记入 `last_size` |
| `terminal_close` | `id` | — | kill child + 从 map/order 移除 |
| `terminal_attach` | `id` | `{ scrollback_b64, alive }` | 视图（重）挂载时拉 ring buffer 回放（内嵌切回 / popout 新窗口 / reload 共用） |
| `terminal_list` | — | `{ terminals: [{id, cwd, alive}], order, active_view }` | 任一视图初始化时重建 tab 列表 + 知道自己该不该活跃 |
| `terminal_popout` | — | — | 开独立窗口（§4.5），置 `active_view=Popout`，emit `terminal://view` |
| `terminal_close_popout` | — | — | 关独立窗口，置 `active_view=Embedded`，emit `terminal://view` |

### 4.3 事件（Rust → 前端）

| 事件 | payload | 说明 |
|---|---|---|
| `terminal://output` | `{id, data_b64}` | reader 线程阻塞读 PTY，**8ms 或 4 KiB 聚合**后 emit（避免每字节一事件）。base64 是因为 PTY 输出可能含无效 UTF-8 截断字节，JSON string 会损坏；前端解成 `Uint8Array` 直接喂 `xterm.write`。**emit 全窗口广播**——内嵌与 popout 两个窗口都收得到，但仅活跃视图实际渲染（§4.5） |
| `terminal://exit` | `{id, exit_code}` | shell 退出。实例保留在 map（alive=false），让前端展示退出态；关 tab 时才移除 |
| `terminal://view` | `{owner: "embedded"\|"popout"}` | 活跃视图归属变更。内嵌面板据此切「显示终端 / 显示『已弹出』占位」；popout 窗口据此知道自己已接管 |

输出同时追加进 `scrollback` ring buffer（驱逐最老字节）。

**为什么用事件不用轮询**：现有 bg 任务面板是轮询模型，但终端打字回显走轮询会有 ≥75ms 平均延迟，打字手感不可接受；事件推送是终端的硬要求。

### 4.4 生命周期

- app 退出：`TerminalState` drop 时 kill 全部 child（zombie 防护：portable-pty 的 child wait 由 reader 线程退出时收割）
- shell 自然退出：发 `terminal://exit`，保留实例供前端显示退出态
- 不随 session 切换做任何事（终端全局单例，子终端 cwd 只在创建那一刻取自当时的 workdir）
- popout 窗口被 OS 关闭：监听 `WindowEvent::Destroyed` → 同 `terminal_close_popout` 逻辑，置回 `Embedded` + emit `terminal://view`（照搬 browser popout 的窗口事件处理，`browser/mod.rs:506`）

### 4.5 popout 独立窗口与「让位」模型

**目标**：像内置浏览器一样，把终端弹成独立可缩放窗口；同一组 PTY，不重启、不丢 scrollback。

**与 browser popout 的本质差异**：browser popout 用 `WebviewUrl::External(目标URL)` 在新窗口重新加载页面（浏览器无状态）；终端有 PTY 状态，新窗口必须加载 **hebbian 自己的前端**渲染 xterm 再 attach 现有 PTY。故用 `WebviewUrl::App("/?terminal-popout=1")`（先例：`lib.rs:2587` log-viewer 窗口）。

**让位模型（为什么不双屏镜像）**：同一 PTY 被两个窗口的 xterm 同时 attach 时，两个 `fit()` 会算出不同 cols/rows 各自 `terminal_resize`，PTY 尺寸来回抖动、重绘错乱。本期规避：**同一时刻只有一个视图活跃**，由 `active_view` 决定。

- `terminal_popout`：建 `TERMINAL_POPOUT_LABEL` 窗口加载 `/?terminal-popout=1` → 置 `active_view=Popout` → emit `terminal://view{owner:"popout"}`
- 内嵌面板收到 `owner:"popout"` → 隐藏 xterm，显示「终端已弹出，点此收回」占位（照搬 browser 的让位占位交互）
- popout 窗口初始化 → `terminal_list` 得知 `active_view=Popout`（自己该活跃）→ 对每个 term `terminal_attach` 回放 → 正常渲染、接管输入输出
- 收回（popout 内点「收回」按钮 / OS 关窗）→ `terminal_close_popout` → `active_view=Embedded` → emit → 内嵌面板 re-attach 回放、恢复渲染

**输入**：仅活跃视图的 xterm `onData` 写 PTY；让位视图的 xterm 被隐藏（无焦点，不产生输入），无需额外拦截。

**resize**：仅活跃视图跑 `fit()` + `terminal_resize`；切换视图时新活跃方 attach 后做一次 `fit()`，把 PTY 调到自己尺寸。

---

## 5. 键盘焦点策略（核心）

### 5.1 已核实的冲突清单

hebbian 现有全局监听（源码核对过）：

| 监听 | 位置 | 冲突 | 处理 |
|---|---|---|---|
| Cmd/Ctrl+F → 拉起聊天内查找 | `ChatView.tsx:392`，`isLocalFindShortcut` 用 `metaKey \|\| ctrlKey` | **Ctrl+F 是 readline forward-char，必被误吃** | 终端聚焦豁免（§5.3） |
| Cmd/Ctrl+N → 新对话 | `isNewConversationShortcut` 同 primary 判定 | **Ctrl+N 是 readline next-history** | 同上 |
| Cmd/Ctrl+Shift+F → 全局搜索 | `isGlobalSearchShortcut` | Ctrl+Shift+F 归终端 | 同上 |
| bare Enter 抑制（capture） | `ChatInput.tsx:378` → `shouldSuppressBareEnterOnDocument` | 无：`isKeyboardInteractiveTarget` 认 textarea，xterm 的隐藏 `.xterm-helper-textarea` 天然豁免 | 仅验收时验证 |
| Enter 聚焦聊天输入框 | `ChatView.tsx:422` | 同上，textarea 豁免 | 仅验收时验证 |
| 各弹窗 Escape | Popup / dialog 等 | 弹窗打开时焦点本就不在终端 | 不处理 |

> 注意：今后**任何新增的全局快捷键**只要走 `hasPrimaryModifier`（含 ctrlKey），都必须过 §5.3 的豁免函数。在 `keyboardShortcuts.ts` 文件头加注释立此规矩。

### 5.2 总规则（macOS 终端惯例，iTerm2 对齐）

终端聚焦时：

- **不带 Cmd 的一切按键 → 进 PTY**。包括且不限于：Ctrl+C（SIGINT）/B/F/A/E/N/P/R/W/U/K/T/Z、Tab 补全、Esc、方向键、F1-F12。应用层不得拦截。
- **Option(Alt) 系 → 进 PTY**，xterm 配 `macOptionIsMeta: true`；另显式映射（iTerm 默认 profile 同款，写 PTY 的字节）：

  | 按键 | 写入序列 | 语义 |
  |---|---|---|
  | Alt+← | `ESC b` | 词左跳 |
  | Alt+→ | `ESC f` | 词右跳 |
  | Alt+Backspace | `ESC DEL` | 删一词 |
  | Cmd+← | `\x01` (Ctrl+A) | 行首 |
  | Cmd+→ | `\x05` (Ctrl+E) | 行尾 |
  | Cmd+Backspace | `\x15` (Ctrl+U) | 删整行 |

- **Cmd 系白名单由终端自己处理**，其余 Cmd 组合放行给应用（菜单 / 全局快捷键照常）：

  | 组合 | 行为 |
  |---|---|
  | Cmd+C | 有选区 → 复制选区；无选区 → 什么都不做（**绝不发 SIGINT**，那是 Ctrl+C 的事） |
  | Cmd+V | 粘贴，bracketed paste 包裹 |
  | Cmd+K | 清屏（`term.clear()`） |
  | Cmd+N / Cmd+F / Cmd+, 等 | 放行给应用（Cmd+F 预留给 P1 终端内搜索，届时再收回） |

- **IME 组合输入**：`event.isComposing` 为 true 时一律不拦，交 xterm 原生 composition 处理（中文输入法依赖）。

### 5.3 实现方式（双向豁免）

**终端侧**——`term.attachCustomKeyEventHandler(e)`：

```
isComposing            → return true（xterm 正常处理）
e.metaKey:
  命中 §5.2 Cmd 白名单  → 自己处理（写序列/复制/粘贴/清屏），return false
  其他 Cmd             → return false（xterm 不吃，冒泡给应用）
Alt+Arrow/Backspace    → 写 §5.2 映射序列，preventDefault，return false
其余                   → return true（xterm 编码后写 PTY）
```

**应用侧**——`keyboardShortcuts.ts` 新增：

```ts
export function isTerminalFocusTarget(el: KeyboardFocusTarget | null | undefined): boolean
// 判定 activeElement 是否在终端内（className 含 "xterm-helper-textarea"，
// 或 closest("[data-terminal-root]") 命中）
```

`ChatView` 的 Cmd/Ctrl+F、新对话、全局搜索等 handler 入口处：`if (isTerminalFocusTarget(document.activeElement)) return;`。豁免逻辑收口在这一个函数，不在各 handler 里散写焦点判断。

### 5.4 选中自动复制（copy-on-select）

- `term.onSelectionChange` → 100ms debounce → `term.getSelection()` 非空则写剪贴板
- debounce 是因为拖选过程中 selectionChange 高频触发，不能每次都写剪贴板
- 剪贴板 API：`navigator.clipboard.writeText`（Tauri webview 是 secure context）；失败 fallback Tauri clipboard 插件（实施时确认项目是否已引入，未引入且 navigator 可用则不加依赖）
- 选区复制**不清除选区**（iTerm 行为：选完还高亮着，便于视觉确认）

### 5.5 鼠标

- 滚轮：xterm 默认（normal buffer 滚 scrollback；vim/htop 等 alt-screen 转发给程序）
- Option+Click 定位光标（iTerm 同款）：P1 可选，本期不做
- 右键：浏览器默认菜单先禁掉（`contextmenu` preventDefault），P1 再考虑自定义菜单（复制/粘贴/清屏）

---

## 6. 前端设计（TerminalSurface.tsx）

终端 UI 主体抽成 `TerminalSurface` 组件，**内嵌（sidebar）与 popout（独立窗口）共用同一份**——两者唯一差别是「自己是不是当前活跃视图」（由 `terminal://view` / `terminal_list` 的 `active_view` 决定）。组件本身无 session 概念（全局单例）。

### 6.1 RightSidebar 接入（内嵌视图）

- `TabId` 增加 `"terminal"`；图标 `SquareTerminal`（区别于 tasks 在用的 `Terminal`）；默认宽 480（终端需要 ≥80 列可用）
- 内嵌挂 `<TerminalSurface variant="embedded" />`
- 挂载模式照抄 browser tab：**lazy mount + 切走 `display:none` 不卸载**（卸载会丢 xterm 实例；PTY 在 Rust 端不死，但重建 xterm 需走 attach 回放，能避则避）
- 切回时序：`display` 恢复 → `fit()` → `syncScrollArea(true)` → `scrollToBottom()`（§1.1 的坑）
- 顶栏加 popout 按钮（图标，照 browser 工具栏）；当 `active_view=popout` 时整个面板内容换成「终端已弹出，点此收回」占位（点击 → `terminal_close_popout`）

### 6.2 popout surface（独立窗口）

- `main.tsx` 加分支：`params.has("terminal-popout")` → 渲染 `<TerminalSurface variant="popout" />`（不挂整个 App，纯终端窗口），照搬 `?log-viewer` 既有写法
- popout 窗口由 `terminal_popout` 命令用 `WebviewUrl::App("/?terminal-popout=1")` 创建
- popout 窗口初始化：`terminal_list` → 得知 `active_view=Popout`（该活跃）+ 拿到 order/terminals → 逐个 `terminal_attach` 回放 → 渲染、接管输入输出

### 6.3 多子终端 tab（全局，两视图共享）

- 面板顶部：tab 条（标题 = cwd 目录名）+ 新建按钮 + 每 tab 关闭按钮
- 子终端模型（前端）：`{ termId, xterm, fit, hostEl, alive }`；所有实例共存于 DOM，非激活子 tab `display:none`
- tab 列表是**全局的**：内嵌与 popout 看到同一组子终端、同一顺序（都来自 Rust `terminal_list` 的 `order`）。在内嵌里新建/关闭的子终端，收进/弹出后在另一视图里一致
- 新建：`terminal_open(cwd = 当前会话 workdir ?? $HOME)`；shell 退出：tab 标灰 + 显示 `[进程已退出]`，保留 scrollback 可滚，仅可关闭
- 视图（重）挂载 / webview reload / popout 切换：`terminal_list` → 对每个 alive 实例 `terminal_attach` 回放 scrollback → 重新订阅 `terminal://output`

### 6.3 xterm 配置

```ts
new Terminal({
  fontFamily: var(--font-mono) 回退 monospace,
  fontSize: 13, lineHeight: 1.2,
  cursorBlink: true,
  scrollback: 5000,
  allowProposedApi: true,        // unicode11 必需
  macOptionIsMeta: true,         // Option 作 Meta
  minimumContrastRatio: 4.5,
  theme: 跟应用深色主题的 ANSI 16 色映射,
})
// addons: FitAddon + Unicode11Addon(activeVersion='11') + WebglAddon(try/contextLoss 回退)
```

- resize：host 容器挂 `ResizeObserver` → `fit()` → `terminal_resize`（sidebar 拖宽时跟手）
- 输出背压：`xterm.write(chunk, callback)` 自带流控，事件 handler 直接喂即可

### 6.4 依赖变更

```
frontend/package.json:
  + @xterm/xterm @xterm/addon-fit @xterm/addon-unicode11 @xterm/addon-webgl
  - ghostty-web        （零引用，移除）
apps/desktop/Cargo.toml:
  + portable-pty
```

---

## 7. UI 文案

按 CLAUDE.md 步骤 3.1：tab 名「终端」；新建按钮 tooltip「新终端」；退出态显示「进程已退出」；不出现 PTY / shell path / term_id 等内部词。

---

## 8. 验收清单（手动，hebweb 不适用，必须 Desktop dev 模式）

键盘（核心）：

- [ ] 终端聚焦按 Ctrl+C 中断前台进程；Ctrl+B/F/A/E/P 光标移动生效
- [ ] **Ctrl+N 不新建对话**、**Ctrl+F 不拉起聊天查找**、Ctrl+Shift+F 不开全局搜索（修复前应能复现误触，修复后消失——A/B 对照）
- [ ] Alt+← / Alt+→ 按词跳；Alt+Backspace 删词；Cmd+←/→ 行首行尾
- [ ] 终端聚焦按 Enter 不被聊天输入框抢焦点；Tab 补全不跳焦点
- [ ] 中文输入法组合输入不漏字、CJK 对齐不错列（`ls` 中文文件名目录）
- [ ] Cmd+N（带 Cmd）仍能新建对话——白名单外 Cmd 正常放行

复制粘贴：

- [ ] 拖选文本 → 松手后剪贴板里就是选中内容；选区保持高亮
- [ ] Cmd+C 复制选区；无选区时 Cmd+C 不中断进程
- [ ] Cmd+V 粘贴多行脚本 → shell 不逐行执行（bracketed paste 生效）

渲染与生命周期：

- [ ] vim / htop 正常渲染与退出；窗口/侧栏拖宽 resize 跟手
- [ ] 切到别的 tab 再切回：scrollback 完整、能滚到底（#5339 坑不复现）
- [ ] 开 2+ 个子终端 tab 互不串台；关 tab 进程被杀（`ps` 验证无残留）
- [ ] shell `exit` 后 tab 显示退出态，scrollback 仍可读
- [ ] 中文路径目录下开终端，提示符不乱码（locale 兜底生效）

全局单例与 popout：

- [ ] 切换会话：终端及其所有子 tab、运行中的进程不变（不随 session 切换重建）
- [ ] 在会话 A 起 `pnpm dev`，切到会话 B，终端里它仍在跑、输出继续
- [ ] 点 popout：独立窗口出现、scrollback 完整重现、可继续输入；内嵌面板变「已弹出」占位
- [ ] popout 里新建/关闭子终端、跑命令，收回后内嵌视图状态一致
- [ ] 收回（popout 内按钮 / 直接关窗）：内嵌面板恢复渲染、scrollback 完整、能继续输入
- [ ] popout 期间内嵌占位不接收键盘输入（无双写 PTY）

---

## 9. 实施文件清单

| 文件 | 动作 |
|---|---|
| `apps/desktop/src/terminal/mod.rs` | 新建：全局 PTY 管理 + 8 命令 + 3 事件 + popout 窗口 |
| `apps/desktop/src/lib.rs`（或 main 注册处） | `mod terminal` + 命令注册 + `TerminalState` manage（全局，非 session 路由） |
| `apps/desktop/Cargo.toml` | + portable-pty |
| `frontend/.../components/TerminalSurface.tsx` | 新建：内嵌与 popout 共用主体 |
| `frontend/.../components/RightSidebar.tsx` | TabId / 图标 / 挂载内嵌视图 + popout 按钮 + 让位占位 |
| `frontend/src/main.tsx` | + `?terminal-popout` surface 分支（照 `?log-viewer`） |
| `frontend/.../lib/keyboardShortcuts.ts` | + `isTerminalFocusTarget` + 文件头规矩注释 |
| `frontend/.../components/ChatView.tsx` | 3 处 handler 入口豁免 |
| `frontend/.../bridge/tauri.ts` | 命令包装（含 popout） |
| `frontend/package.json` | + xterm 系；- ghostty-web |
| `docs/架构.md` §8 | 追加「内置终端」一节 |
| `docs/changelog.md` | 追加一条 |

## 10. 架构.md 并入点

§8 追加小节，内容收敛为：模块归属（desktop surface 部件、**全局单例**、不进 agent-core 的理由）、命令 / 事件契约表（§4.2 / §4.3）、PTY 单一真理源 + popout 让位模型（§3.2 / §4.5）、键盘焦点总规则（§5.2 的三条）、与 browser 面板的「同构但 session 归属相反」关系（§3.1）。详设留在本文件。

## 11. 留尾巴

- **与后台任务融合**（本轮讨论搁置的方案，stagewise 同样未做，见 §1.4）：后台 Bash PTY 化、模型读输出需 strip ANSI。本期的 `terminal/mod.rs` 设计已为此留余地——PTY spawn / ring buffer / 推流三件套未来可下沉 agent-core 复用，Tauri 层只剩转发。注意：融合后终端是「全局单例」与 bg 任务「session-scoped」的归属冲突需先解（可能演化为「全局终端 + 每会话一组绑定子终端」两层）
- **内嵌 + popout 双屏镜像**（本期用让位模型规避）：若将来要两个视图同时活跃，需引入「主从尺寸协商」——一个视图为 resize 主，另一个只读跟随，类似 tmux `aggressive-resize`。本期不做
- P1 候选：终端内搜索（search addon + 收回 Cmd+F）、Option+Click 光标定位、右键菜单、路径点击跳转（fanbox 有完整实现可抄）、终端跟随会话 workdir 的 `cd` 同步
- P1 候选：终端内搜索（search addon + 收回 Cmd+F）、Option+Click 光标定位、右键菜单、路径点击跳转（fanbox 有完整实现可抄）、终端跟随会话 workdir 的 `cd` 同步
- Windows 适配（ConPTY 路径 portable-pty 已封装，但快捷键表是 macOS 惯例，需另做 Ctrl/Alt 映射表）——hebbian 当前只跑 macOS，不阻塞
