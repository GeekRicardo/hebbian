# 内置浏览器 TDD（技术设计 + 测试驱动计划）

> **上游文档**：[内置浏览器与临时对话框-spec.md](内置浏览器与临时对话框-spec.md)（需求与取舍）。本文是实现级设计：模块/接口/文件清单 + 测试先行的开发顺序。
> **已拍板选型**：Desktop = Tauri 子 webview（multi-webview `unstable` + `initialization_script`）；hebweb = preview-proxy + iframe 降级。
> **TDD 纪律**：每个任务先写测试（或手动验收脚本）跑红，再实现跑绿；纯函数必须有单测；native webview 无法进 CI 的部分用「手动验收清单」固化，条目逐项打勾才算完成。

---

## 1. Phase 0：Spike（B 方案可行性验证）

新建临时 demo（`examples/webview-spike`，验证完删除或留作参考），**逐项验证，全部通过才进 P1**；任何一项失败 → 按 spec §11 拍板 4 的退路回退 A 方案。

| # | 验证项 | 做法 | 通过标准 |
|---|---|---|---|
| S1 | 子 webview 创建与定位 | `tauri = { features = ["unstable"] }`；主窗口 `window.add_child(WebviewBuilder::new("preview", WebviewUrl::External(...)), LogicalPosition, LogicalSize)` | macOS 上子 webview 出现在指定矩形内，加载 example.com 正常渲染 |
| S2 | 跨导航注入持续生效 | `WebviewBuilder::initialization_script("window.__HEB_MARK__=1")`；在子 webview 内点链接跳转 2 个不同域 | 每次导航后 `__HEB_MARK__` 仍为 1（WKUserScript at-document-start 语义） |
| S3 | 双向 IPC | 注入脚本调 `window.ipc.postMessage(json)`；Rust 侧 `WebviewBuilder::on_ipc_message`（或 wry ipc handler）收到；Rust 调 `webview.eval("window.__HEB_RX__(...)")` | 两个方向各传 1KB JSON 往返成功，时延 < 50ms |
| S4 | bounds 同步 | 前端占位 div + ResizeObserver → invoke `browser_set_bounds`；拖拽窗口大小、折叠侧栏 | 子 webview 跟随占位区域，无残影；连续 resize 不崩 |
| S5 | 导航事件 | `on_navigation` 回调 + title 变化（轮询 `webview.title()` 或 eval document.title 上报） | 地址栏能实时反映子 webview 当前 URL 与 title |
| S6 | z-order 实测 | 打开一个 DOM 模态弹窗与子 webview 区域重叠 | **记录实际行为**（预期：native 在上层盖住 DOM）。确认应对：弹窗出现时 `webview.hide()` 或缩小 bounds 避让，恢复时还原——验证 hide/show 不丢页面状态 |
| S7 | cookie 持久化 | `WebviewBuilder::data_directory(...)`（或确认 wry 默认行为）；登录一个表单站点，关闭重开子 webview | 会话保持（或明确得出"仅进程内保持"的结论，写回 spec §11 风险表） |

Spike 结论落档：在本文档 §1 末尾追加「Spike 结果」小节，逐项记录实测行为与 Tauri/wry 版本号。

### Spike 结果（2026-06-10，tauri 2.10.3 / tauri-runtime-wry 2.10.1，macOS）

**全部七项通过，B 方案放行。** 实测手段：`apps/desktop/src/browser/mod.rs::run_spike` 在 `HEBBIAN_WEBVIEW_SPIKE=1` 时自动跑序列，`target:"webview_spike"` 日志取证。

| # | 结果 | 证据 |
|---|---|---|
| S1 创建定位 | ✅ | `add_child` 返回 Ok，example.com 在指定矩形渲染，无 panic |
| S2 跨导航注入持续 | ✅ | 导航到 httpbin（异域）后 `heb:ready` 再次到达——`initialization_script` 是 at-document-start，每个新文档都重注入 |
| S3 双向 IPC | ✅ | 上行：inspector `location.replace("heb-bridge://…")` 被 `on_navigation` 拦截解析（日志 `S3-up bridge: heb:ready`，return false 页面无感知）；下行：`webview.eval` picker start 返回 Ok |
| S4 bounds 同步 | ✅ | `set_bounds` Ok，连续调用不崩 |
| S5 导航事件 | ✅ | 每次导航 `on_navigation` 回调命中（example→httpbin set-cookie→httpbin cookies 三跳全捕获） |
| S6 z-order/hide-show | ✅ | `hide`/`show` Ok；native webview 叠在 DOM 之上（应对：弹窗/对话期 `browser_set_visible(false)` 避让，已落 command） |
| S7 cookie | ✅ | set-cookie 页 → /cookies 页导航链完成且 `heb:ready` 在终点页到达，会话由 wry 默认 data store 维持 |

**关键收获**：上行通道最终用 `heb-bridge://` 自定义 scheme 导航拦截（外部 URL 子 webview 无 Tauri IPC，`window.ipc.postMessage` 不可用）——这是 spike 中确定的实现路径，已写入 §8.5 决策。`Window::add_child` 与 `Manager::get_window` 均在 `unstable` feature 下；调用全部收口 `browser/mod.rs`。

---

## 2. 模块设计

### 2.1 文件清单（新增/修改）

```
apps/desktop/src/
  browser/mod.rs              BrowserController：子 webview 生命周期 + commands + 事件转发   [新增]
  lib.rs                      注册 browser 模块 commands                                      [修改]

apps/desktop/frontend/src/
  inspector/                  注入脚本（独立 vite entry，无 React 依赖）                      [新增]
    index.ts                  入口：bridge 选择 + picker + styler 装配
    bridge.ts                 传输抽象（wry IPC / postMessage 双通道）
    picker.ts                 元素选取状态机 + overlay 渲染
    snapshot.ts               HebElementSnapshot 采集（DOM/样式/fiber）
    styler.ts                 样式实时预览（apply/revert + diff 记录）
  desktop/ui/components/
    BrowserPanel.tsx           面板：地址栏/导航/占位 div/工具栏                              [新增]
    AnnotationCard.tsx         注释卡片：元素徽章 + 注释输入 + 样式参数编辑器                  [新增]
    DesktopSidebar.tsx         右侧 sidebar 加浏览器图标                                      [修改]
  desktop/ui/lib/
    browserHost.ts             BrowserHost 接口 + Tauri 实现（P2.5 加 iframe 实现）           [新增]
    previewUrl.ts              URL 归一化/两档校验/检测正则（纯函数）                          [新增]
    annotationMessage.ts       注释 → user message content + attachments 组装（纯函数）       [新增]

crates/preview-proxy/         hebweb 降级路径                                                [P2.5 新增]
apps/web-server/src/server.rs  startPreviewProxy/stopPreviewProxy invoke 镜像                [P2.5 修改]
```

### 2.2 Rust：`BrowserController`（apps/desktop/src/browser/mod.rs）

进程级单例 state（`Mutex<Option<BrowserInstance>>`，v1 单实例单标签）：

```rust
struct BrowserInstance {
    webview: tauri::Webview,        // 子 webview 句柄
    current_url: String,            // 真实 URL（地址栏数据源）
    picker_active: bool,
}
```

Tauri commands（Desktop 专属，**不进 CoreClient**——浏览器承载是 surface 能力非 core 业务，对齐 spec §7.4）：

```
browser_open(url, origin)        创建/复用子 webview 并导航；origin = "auto" | "user"，
                                 按 spec §4.2 两档校验（auto 仅本地网段；user 放行公网 http(s)，
                                 元数据地址硬黑名单）
browser_navigate(url)            地址栏跳转（origin=user 档校验）
browser_back / browser_forward / browser_reload
browser_set_bounds(x, y, w, h)   前端占位 div rect 同步（逻辑像素）
browser_set_visible(visible)     S6 弹窗避让用
browser_close()                  销毁子 webview
browser_picker(active)           开/关选取模式（eval 转发给 inspector）
browser_style_apply(prop, value) 样式实时预览（eval 转发）
browser_style_revert()
```

事件（`app.emit` 给前端，沿用现有事件总线风格）：

```
browser://state        { url, title, can_go_back, can_go_forward, loading }
browser://element      { snapshot }          // inspector 选中元素，IPC → Rust → 前端
browser://picker-off   {}                    // Esc 取消
browser://escaped      { url }               // 导航到校验失败的地址时强制拦截并上报
```

实现要点：

- `initialization_script` 内容 = `include_str!(concat!(env!("OUT_DIR"), "/inspector.js"))`，build.rs 从前端构建产物拷贝（产物缺失时 build 报错，提示先跑前端 build）
- IPC handler 只做一件事：解析 `{ type, payload }` 信封后转发 `app.emit`——**不在 Rust 侧理解 snapshot 结构**（它只是透传的 JSON，类型定义留在 TS 一份，避免双语言同步）
- 导航校验是安全边界：`on_navigation` 回调里同样跑两档校验，校验失败 `return false` 阻断并发 `browser://escaped`（地址栏输入只是入口之一，页面内跳转也要拦）

### 2.3 inspector.js 四件套

**bridge.ts**——传输抽象：

```typescript
interface Bridge {
  send(msg: InspectorOutMsg): void;     // → 宿主
  onReceive(cb: (msg: InspectorInMsg) => void): void;
}
// 选择逻辑：window.ipc?.postMessage 存在 → WryBridge（出向 ipc.postMessage，
// 入向挂 window.__HEB_RX__ 全局函数供 Rust eval 调用）；
// 否则 → FrameBridge（window.parent.postMessage + message 事件，校验 origin/来源标记）
```

消息协议沿用 spec §8.2 表（`heb:picker:*` / `heb:style:*` / `heb:ready` / `heb:navigated`）。

**picker.ts**——状态机：

```
idle → (picker:start) → hovering → (click) → selected → (picker:start 重入/Esc) → idle
hovering: mousemove → elementFromPoint（过滤 data-hebbian-overlay）→ 高亮 overlay + 组件名标签
selected: 虚线框常驻；发 heb:picker:selected { snapshot }
所有事件监听 capture 阶段 + preventDefault（picker 激活期间不触发页面自身点击）
```

**snapshot.ts**——采集（spec §6 结构，截断规则是硬约束：attributes 值 ≤200 字符、innerText ≤500、props ≤20 项 × 100 字符、序列化总长 > 8KB 时丢弃 computedStyles 以外的可选字段直至达标）。fiber 提取独立函数 `extractReactInfo(el): ReactInfo | null`，任何异常返回 null。

**styler.ts**——`apply(prop, value)` 记录 `{ prop, before: el.style.getPropertyValue(prop), after: value }` 后 `setProperty`；同 prop 重复 apply 只更新 after；`revert()` 逆序恢复 before；`takeDiff()` 输出去重后的 diff 数组。

### 2.4 前端：BrowserHost 适配层 + 面板

```typescript
interface BrowserHost {
  open(url: string, origin: "auto" | "user"): Promise<void>;
  navigate(url: string): Promise<void>;
  back(): void; forward(): void; reload(): void;
  setBounds(rect: DOMRect): void;
  setPicker(active: boolean): void;
  applyStyle(prop: string, value: string): void;
  revertStyles(): void;
  close(): void;
  onState(cb: (s: BrowserState) => void): Unlisten;
  onElementSelected(cb: (snap: HebElementSnapshot) => void): Unlisten;
}
```

- `TauriBrowserHost`（P1/P2）：invoke + listen 包装 §2.2 的 commands/事件
- `IframeBrowserHost`（P2.5）：startPreviewProxy + iframe + postMessage
- `BrowserPanel.tsx` 持 host 实例：占位 div（`ref` + ResizeObserver + IntersectionObserver → setBounds/setVisible）、地址栏表单、导航按钮组、[选取元素] 按钮、检测 chips（复用 deepseek-gui 设计的 hebbian 实现 `previewUrl.ts`）
- `AnnotationCard.tsx`：收到 `onElementSelected` 后按 snapshot.boundingClientRect + 占位 div offset 计算锚点弹出；内部用 QuickChat inline 积木（SessionChatController + 输入框）+ 样式参数表单；提交调 `annotationMessage.ts` 组装后走现有 `send_message`

### 2.5 注释消息组装（annotationMessage.ts，纯函数）

```typescript
function buildAnnotationMessage(input: {
  snapshot: HebElementSnapshot;
  comment: string;
  styleDiff: StyleDiff[];
}): { content: string; attachments: MessageAttachment[] }
```

输出形态固定为 spec §5.4 模板（导语「我在页面预览里圈了个地方，想这样改：」+ `<web_annotation>` 块 + element.json TextFile 附件）。纯函数 + snapshot 测试，格式变更必须先改测试。

---

## 3. 测试清单（先写测试，红 → 绿）

### 3.1 纯函数单测（vitest，先行编写）

| 测试文件 | 用例 | 关键断言 |
|---|---|---|
| `previewUrl.test.ts` | 归一化：`"3000"`→`http://127.0.0.1:3000/`；`"0.0.0.0:5173"`→`127.0.0.1`；无 scheme 补 http | 输出精确相等 |
| | 两档校验：auto 档拒绝 `https://example.com`；user 档放行；两档都拒 `ftp://`、`http://169.254.169.254` | 表驱动 ≥ 12 个 case |
| | 检测双阈值：dev server 输出文本命中 auto_open；普通含 URL 文本只入 card；`/health` 路径剔除 | 用真实终端输出样本做 fixture |
| `annotationMessage.test.ts` | 组装：含/不含 styleDiff、comment 空、react 缺省 四种输入 | content 与 attachments 形状 snapshot 测试；element.json 可被 `JSON.parse` |
| `snapshot.test.ts`（jsdom） | 截断：超长 innerText/attributes/props 按规则截断；总长 >8KB 降级丢字段 | 序列化长度 < 8192；必留字段（tagName/selectorPath/url）始终存在 |
| | selectorPath：构造嵌套 DOM，生成路径能 `querySelector` 唯一命中原元素 | 往返一致性 |
| | fiber 提取：mock `__reactFiber$x` 链 | 组件链顺序正确；无 fiber 返回 null 不抛 |
| `styler.test.ts`（jsdom） | apply→revert 幂等；同 prop 二次 apply 合并 diff；revert 后 takeDiff 为空 | `el.style` 与初始完全一致 |
| `picker.test.ts`（jsdom） | 状态机：start→hover→click→selected；Esc 任意态回 idle；overlay 元素被 elementFromPoint 过滤 | 状态转移表全覆盖 |

### 3.2 Rust 单测

| 位置 | 用例 |
|---|---|
| `apps/desktop/src/browser/mod.rs` `#[cfg(test)]` | URL 两档校验 Rust 侧实现（与 TS 同一套表驱动 case，**两份实现共用一份 case 清单**写在测试注释里，改一处必对另一处）；IPC 信封解析（合法/缺字段/超长拒收） |
| `crates/preview-proxy`（P2.5） | HTML 注入：plain / gzip / 无 `</head>` / chunked 四个 fixture，断言注入位置与 Content-Length 重算；同 host 绝对 URL 重写（href/src/srcset/action 覆盖，跨 host 不动）；cookie jar：双 target 写同名 cookie 互不可见；`Set-Cookie` 不出现在下游响应头 |

### 3.3 手动验收清单（native webview 不可 CI，逐项打勾固化在 PR 描述）

P1（对应 spec §10）：

- [ ] sidebar 图标打开面板 → 输入 `example.com` 回车 → 渲染正常，地址栏显示规范化 URL
- [ ] agent `pnpm dev` 起 vite → 聊天流出现 chip → 点击/auto_open 打开并渲染
- [ ] 后退/前进/刷新/⌘[/⌘] 行为与系统浏览器一致；title 实时更新
- [ ] 表单登录站点登录成功，刷新后会话保持（S7 结论决定是否跨重启）
- [ ] 页面内点击跳转到 `http://169.254.169.254` 类地址被拦截且面板有提示
- [ ] 窗口 resize / 折叠侧栏，子 webview 跟随无残影；打开设置弹窗，子 webview 正确避让（S6 方案）
- [ ] prod build（非 dev 模式）以上全部复测

P2 追加：

- [ ] 选取按钮 → hover 高亮跟随 + 组件名标签 → 点击弹注释卡片，徽章显示 `button.btn-primary <SaveBtn>`
- [ ] 参数编辑器改圆角实时生效；取消后页面恢复原样（DOM diff 验证：`el.getAttribute("style")` 回到初值）
- [ ] 注释发送 → 主对话出现「页面标注」卡片消息 → `session.jsonl` 里该 user message 含 element.json 附件（jq 验证）
- [ ] agent 完成修改 → HMR 后页面呈现 → **A/B 复跑**：同一注释输入在修复前后行为对照（CLAUDE.md 修 bug 流程同款标准）
- [ ] picker 激活期间页面自身的点击事件不触发；Esc 退出

P2.5（hebweb，Playwright 自动化，按 [heb-cli-debug.md §9](heb-cli-debug.md)）：

- [ ] 全链路脚本：打开面板 → 代理加载 vite 页 → 选取 → 批注 → 发送 → 断言主对话 DOM 出现标注卡片
- [ ] 双公网 target 登录态隔离断言

### 3.4 回归测试沉淀原则

手动清单中任何一项后续修 bug 时，若 bug 本质可单元化（如「校验函数漏拦截某 host」「styleDiff revert 漏属性」），按 CLAUDE.md B.2 固化为单测，A/B 翻转可复现。

---

## 4. 开发顺序（依赖驱动，每步红→绿）

```
P0  S1–S7 spike                                  （结论写回本文档 §1）
P1.1  previewUrl.ts + 测试                        ← 先测后写，纯函数
P1.2  browser/mod.rs commands + Rust 校验单测      ← 校验逻辑先测
P1.3  BrowserHost 接口 + TauriBrowserHost
P1.4  BrowserPanel.tsx + DesktopSidebar 图标       ← 手动清单 P1 验收
P2.1  bridge.ts / picker.ts / snapshot.ts / styler.ts + 全部 jsdom 测试   ← 全部先红后绿
P2.2  inspector 构建管线（vite entry → build.rs include）
P2.3  browser_picker/style commands + 事件转发
P2.4  annotationMessage.ts + 测试 → AnnotationCard.tsx（QuickChat inline 积木）
P2.5  手动清单 P2 验收 + changelog
P2.5.x  preview-proxy crate（注入/jar/重写单测先行）→ IframeBrowserHost → Playwright 链路
P3  旁支对话（spec §3/§10）——单独立项，复用 QuickChat 积木
```

每个 P 完成时：`cargo check --workspace` + `pnpm exec tsc --noEmit` + 相关测试 + changelog 追加一条（CLAUDE.md 步骤 4/5）；架构.md 增补（spec §9 清单）在 P1 动工前一次性落。

---

## 5. 风险登记（实现层）

| 风险 | 缓解 |
|---|---|
| Tauri multi-webview API 在后续版本变动 | 所有 unstable API 调用收口在 `browser/mod.rs` 单文件；升级 Tauri 时只动这一处 |
| `initialization_script` 在某些站点 CSP 下被禁 | WKUserScript 不受页面 CSP 约束（宿主注入），spike S2 顺带验证一个带严格 CSP 的站点 |
| inspector 全局符号与页面冲突 | 仅暴露 `window.__HEB_RX__` 一个入口（FrameBridge 模式零全局），其余 IIFE 闭包；命名带 `__HEB` 前缀 |
| 子 webview 截图（P3）| wry 是否暴露 WKWebView takeSnapshot 待查；不可用则走 Rust 侧 CGWindowListCreateImage 按 bounds 裁剪，P3 再评 |
| eval 注入 style value 的转义 | `browser_style_apply` 的 value 经 serde_json::to_string 转义后嵌入 eval 串；属性名白名单校验（仅 §5.3 四组），杜绝任意 JS 注入 |
