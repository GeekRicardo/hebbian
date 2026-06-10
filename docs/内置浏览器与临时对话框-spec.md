# 内置浏览器 + 页面注释 + 通用临时对话框（QuickChat）Spec

> **文档状态**：提案（2026-06-10），待批准。批准后按 §9 清单并入 [架构.md](架构.md) 对应章节，本文件保留为详设附录。
> **调研基础**：OpenAI Codex Desktop（官方文档 + 截图）、stagewise 1.6.0（AGPL-3.0，`/tmp/stagewise` 源码级调研）、deepseek-gui（本机源码级调研）。三方调研结论见 §1。

---

## 0. 一句话总结

给 hebbian 加三个部件：

1. **QuickChat**——通用临时对话能力：后端是「**旁支对话**」（aside session：可从当前对话分叉也可全新创建、可全工具 agent 可纯对话、结果可回灌主对话），前端是可接到任意位置的多形态渲染（独立浮层框、嵌入式混合输入），支撑页面注释对话、基于当前对话的临时分支讨论、未来任意"就地起一段对话"的场景（`//btw` 只是其入口形式之一，暂不排期）
2. **PreviewPanel**——内置浏览器面板：iframe 嵌入本地 dev server 或公网页面，聊天流 URL 自动检测 + auto-follow
3. **preview-proxy + inspector.js**——注入式本地反代：让本地与公网页面获得元素选取、样式实时编辑、注释采集能力

注释数据走**现有** `send_message(attachments)` 通道进入主对话，**agent-core / protocol / model-gateway 零改动**。

---

## 1. 调研结论摘要

### 1.1 三方对比

| | Codex Desktop | stagewise 1.6.0 | deepseek-gui |
|---|---|---|---|
| 宿主 | 闭源桌面 app（`InAppBrowser` flag） | 自研 Electron 浏览器 | Electron `<webview>`，降级 iframe |
| 元素选取 | 有（annotation mode） | CDP（`webContents.debugger`）hit-test | 无（沙箱全关，无法注入） |
| React 感知 | 未知 | CDP 注入 fiber 分析（组件名+props） | 无 |
| 样式编辑 | 有（参数面板+实时预览） | 无 UI（但采集 computedStyles） | 无 |
| 注释→LLM | 注释+元素上下文进对话 | `.swdomelement` JSON 文件作聊天附件 | 无回流（单向 LLM→浏览器） |
| URL 自动检测 | — | — | 双阈值正则启发式（最精细） |

### 1.2 对 hebbian 的路线判断

- **CDP 宿主路线（stagewise/Codex）效果最强但走不了**：依赖 Electron `webContents.debugger`，Tauri WKWebView 无 CDP 对等物
- **代理注入路线是正解**：本地反代包住 dev server、改写 HTML 注入脚本。Desktop 与 hebweb 共享同一份 React 前端，iframe 方案两边天然可用，保住三 surface 对称
- **协议形态判断被双重印证**：codex 开源协议层 `UserInput` 只有 Text/Image（无任何 DOM 字段）、stagewise 用"文件附件+prompt 说明"——注释就是**普通带附件的 user message**，agent 核心对"注释"概念无感知
- **licence 红线**：stagewise 是 AGPL-3.0。只借鉴数据结构设计与交互思路，**不抄任何代码**；fiber 遍历等属公开常识，独立重写

### 1.3 deepseek-gui 可直接照搬的设计（源码已核）

| 设计点 | 出处 |
|---|---|
| 本地 URL 白名单（localhost / *.localhost / host.docker.internal / *.local / ::1 / 10.x / 127.x / 172.16-31.x / 192.168.x / 169.254.x；0.0.0.0→重写 127.0.0.1；仅 http/https） | `src/shared/dev-preview-url.ts` |
| 纯数字输入自动补全 `http://127.0.0.1:<port>`，无 scheme 补 `http://` | 同上 |
| **双阈值 URL 检测**：`card` 模式（宽松，展示候选 chips）与 `auto_open` 模式（严格，只认 dev server 输出特征）分开，避免自动打开误伤 | `src/renderer/src/lib/dev-preview-detection.ts` |
| 三层判定：block 级（assistant 文本需命中动作/状态词；tool block 需是命令执行且命令或输出像 dev server）→ URL ±120 字符上下文复判 → 路径过滤（/health /metrics /readyz /livez /v\d+） | 同上 |
| 自动打开只看"最近一条 user 消息之后"的内容（本轮新产生的 URL 才触发） | `extractLatestTurnAutoOpenDevPreviewUrls` |
| auto-follow 状态机：默认开、用户手动输 URL 即关（用户接管）、外部指定 preferredUrl 强制重开 | `DevBrowserPanel.tsx:172-190, 275` |
| iframe 模式手动历史栈（上限 30）、加载 10s 超时标失败、reload 用 nonce 重建、`errorCode -3`（导航中断）忽略 | `DevBrowserPanel.tsx:157-267` |
| 候选 URL chips 横向滚动条 + 空态引导按钮 + "在系统浏览器打开"逃生门 | `DevBrowserPanel.tsx:533-579` |

### 1.4 stagewise 可借鉴的设计（源码已核，仅借鉴设计）

| 设计点 | 出处 |
|---|---|
| 选中元素快照 schema（§6 的蓝本）：xpath/attributes/innerText/react 信息/boundingRect/computedStyles/伪元素/交互态/父子兄弟轻量摘要/frame 上下文/codeMetadata 预留 | `src/shared/selected-elements/swdomelement.ts` |
| React fiber 提取要点：遍历 DOM 节点 `__reactFiber$*` 属性找 fiber；组件名取 `type.displayName ?? type.name`；props 截断（最多 20 个、每值 100 字符）；分析器挂 `Symbol.for(...)` 键降低可探测性 | `selected-element-tracker/react-component-tracker.ts` |
| 高亮 overlay：rAF 跟踪 boundingRect 节流 10fps；hovered（蓝色填充+实线）与 selected（透明+虚线）两种样式；截图时统一隐藏 overlay | `web-content-preload/components/hovered-element-tracker.tsx` |
| 截图压缩到 5MB 以下再给模型（Claude API 限制） | `browsing-tab-controller.ts:1734` |
| 元素快照作为附件文件 + 在 prompt 里说明文件格式 | `agents/chat/prompts/environment-preamble.md` |

### 1.5 Codex Desktop 交互形态（官方文档 + 截图）

- Annotation mode：点选元素；Shift 圈区域；Cmd+点击立即发送
- **Advanced annotations**（用户截图所示）：选中元素后弹参数面板（字重/字号/字体/边框圆角/边框颜色/边框宽度…），改动**实时在页面上预览**，确认后把"注释+精确目标值"发给 agent——把模糊意图（"圆角大一点"）变成精确目标（`border-radius: 6px → 12px`），这是"LLM 快速能改对"收益最大的交互
- 支持 localhost dev server、文件预览、免登录公网页

---

## 2. 总体设计

```
┌─ hebbian 前端（同一份 React 代码）────────────────────────────────────────────┐
│  ChatView（主对话）        BrowserPanel（内置浏览器，右侧 sidebar 图标进入）    │
│      ▲                      ├─ URL 栏 / 导航 / auto-follow / 候选 chips       │
│      │ send_message         ├─ BrowserHost 适配层（两路实现，面板无感知）      │
│      │ (含注释附件)          └─ 工具栏：[选取元素] 按钮                        │
│      │                            │                                          │
│  QuickChat（旁支对话渲染层）◄─────┘                                           │
│   ├─ inline：注释输入 + 样式参数编辑器（锚定选中元素旁，P2）                   │
│   └─ floating：branch / fresh 旁支讨论（P3）                                  │
└──────────────────────────────────────────────────────────────────────────────┘
   BrowserHost 两路实现：
   ┌─ Desktop（主路径，B 方案）──────────┐  ┌─ hebweb（降级路径，A 方案）─────┐
   │ Tauri 子 webview 直接加载目标 URL    │  │ iframe → preview-proxy 反代    │
   │ initialization_script 注入          │  │ 代理改写 HTML 注入              │
   │ wry IPC / webview.eval 双向通信     │  │ postMessage 双向通信            │
   │ 真 cookie / 登录 / 任意公网          │  │ 代理端 cookie jar（§7）        │
   └─────────────────────────────────────┘  └────────────────────────────────┘
         │ 两路注入同一份脚本
         ▼
  inspector.js（注入页面内，传输层抽象）
   ├─ picker：hover 高亮 + 点击选中
   ├─ 采集：DOM/样式/React fiber → HebElementSnapshot
   └─ 实时预览：改 inline style，cancel 时恢复
```

数据流（注释场景）：

```
用户点[选取元素] → postMessage 开 picker → 页面内 hover 高亮 → 点击选中
→ inspector.js 采集 HebElementSnapshot → postMessage 回前端
→ 前端在元素旁弹 QuickChat(annotation)：写注释 + 调样式参数（实时预览）
→ 提交 → 组装 user message（注释文字 + CSS diff + element.json 附件 + 可选截图）
→ send_message(主 session)  ……主对话正常走 agent loop，agent 用 Grep/Read/Edit 改代码
→ dev server HMR 热更 → iframe 内立即看到效果
```

**架构定位**：全部落在 surface 层（§7/§8 范畴）+ 一个新 crate。agent-core / protocol / model-gateway 不动（例外见 §9 的 ephemeral 标记，additive）。

---

## 3. QuickChat：通用临时对话能力

### 3.1 设计定位

QuickChat 不是一个固定的弹窗组件，而是**两层东西**：

- **后端：旁支对话（aside session）**——一段独立于主对话的会话，可从主对话 fork（继承上下文）也可全新创建；可以是带完整工具集的 agent，也可以是不带工具的纯 LLM 对话；结束后可丢弃、可保留、可把结论回灌主对话。`//btw` 命令、消息气泡右键、全局快捷键都只是它的**入口形式**，不是概念本身
- **前端：多形态渲染层**——同一套会话控制逻辑，按宿主场景渲染成不同形态，能临时接到界面任何位置

核心抽象是把"聊天能力"从 ChatView 的页面布局里解耦出来，后端则完全复用现有 session 机制。

### 3.2 四个正交配置维度

每个 QuickChat 实例由四个维度组合定义：

| 维度 | 取值 | 实现 |
|---|---|---|
| **会话来源** | `attach-main`（不开新会话，发进当前主 session）/ `branch`（fork 当前对话 → 旁支）/ `fresh`（全新旁支，可带 contextSeed） | 现有 `send_message` / `fork_session`（desktop `apps/desktop/src/lib.rs:594` + hebweb 镜像已齐）/ `create_session` |
| **能力档** | `full-agent`（完整工具集，走 agent loop，能改代码）/ `chat-only`（无工具，纯 LLM 对话） | 现有 `send_message` 的 `enabled_tools` 参数：chat-only 即传 `[]`，**零后端改动** |
| **呈现形态** | `floating`（独立浮层对话框）/ `inline`（嵌入宿主 UI 的混合形态，§3.4） | 前端渲染层 |
| **结果去向** | 留在旁支（默认）/ `returnToChat`（结论回灌主对话，§3.7） | §3.7 |

模型选择：branch / fresh 旁支**继承主会话的 provider + model**，QuickChat 头部提供模型选择器可随时切换（复用现有选择器组件）——已拍板。

典型组合：

- 页面注释（P2）= attach-main + full-agent（要改代码）+ inline（注释输入与参数编辑器混合，§5.2）
- 旁支讨论（P3，入口：消息气泡右键 / 未来 `//btw`）= branch + 默认 chat-only（可切 full-agent）+ floating + 可选 returnToChat
- 未来"全局问一下" = fresh + chat-only + floating

### 3.3 旁支会话（aside session）语义

`branch` / `fresh` 产生的会话标记为**旁支对话**：

- `Session` meta 增加 `aside: bool`（默认 false），经现有 `MetaUpdate` 机制 append 落盘（与 `run_mode` 同路径，§8.2-3 既有机制）
- `listSessions` 默认过滤 aside=true；filter 增加 `include_aside` 显式包含（§3.2 同步 API 的 additive 改动）
- QuickChat 关闭时弹两选项：**丢弃**（调现有 `deleteSession`）或**保留**（置 aside=false，会话升级进列表，可在主界面继续）
- 崩溃兜底：启动时扫到 aside 且 7 天无更新的会话，设置页提供一键清理（不自动删，文件是用户的）

这是本 spec 中**唯一**触碰 agent-core storage 的改动，纯 additive：老 jsonl 无此字段按 false 读，向前向后兼容。

### 3.4 两种呈现形态

**floating（独立浮层对话框）**——branch / fresh 场景：

- `position: fixed`，可锚定到任意 DOMRect（branch 锚到触发消息气泡；无锚点时右下角），可拖动，可钉住（pin 后不随点击外部关闭）
- 结构：头部（来源徽章 + 能力档开关「仅对话 / 完整 agent」+ pin/关闭）/ 正文（消息流，复用现有消息渲染）/ 底部（输入框）
- 尺寸默认 420×520，可拖拽调整；同屏最多一个 floating 实例（再开则替换，多开是非目标）

**inline（嵌入式混合形态）**——页面注释等场景：

- QuickChat 不渲染成"完整对话框"，而是把**输入框 + 会话控制器**嵌进宿主自己的 UI 里，与宿主的其他控件（注释场景：元素徽章 + 样式参数编辑器）混合排布
- 消息流区域按需出现：发送前只有输入区；发送后展开一块轻量消息流展示本次往返（attach-main 模式下即主对话里这条消息的镜像）
- 宿主通过 props 决定布局，QuickChat 提供的是「会话控制 + 输入框 + 可选消息流」三个可拼积木，而不是固定外壳

### 3.5 实现要点：从 ChatView 解耦

现状 `ChatView`（`apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx`）与活跃 session、页面布局耦合。QuickChat 不复制 ChatView，而是抽出两个可复用单元：

1. **`SessionChatController`**（hook）：参数化 `session_id`，封装 send_message 调用 + EngineEvent channel 订阅 + 消息列表 state。ChatView 与 QuickChat 都用它（ChatView 改为消费此 hook 属于顺带重构，**允许分两步**：先给 QuickChat 写 hook，ChatView 迁移另开任务，避免一次 PR 不可审）
2. **`MessageList`（裁剪渲染）**：复用现有 MessageBubble 渲染消息/工具卡片，QuickChat 里禁用重交互（无 fork/rollback 按钮、工具卡片默认折叠）

HITL 注意：QuickChat 绑定的 session 触发审批/提问时，审批 UI 必须在 QuickChat 内呈现（复用现有审批气泡组件），不能丢到主 ChatView——事件按 session_id 路由，天然隔离。

### 3.6 旁支对话的入口形式（均为表现层，概念在 §3.1-3.3）

| 入口 | 形态 | 排期 |
|---|---|---|
| 消息气泡右键「开旁支讨论」 | branch + floating，锚到该气泡 | P3 |
| `//btw` 命令 | 同上，以最新消息为分叉点；届时登记架构.md §8.2 表 A，前端本地派发不进主对话 transcript | **暂不排期**（已拍板：先只做 web 注释） |
| 全局快捷键"问一下" | fresh + floating | 远期 |

旁支会话自己的 jsonl 由 agent-core 正常落盘，与入口形式无关。

### 3.7 returnToChat：把旁支结论回灌主对话

branch / fresh 的 QuickChat 提供「返回主对话」选项（头部开关或关闭时询问）。三种实现形态评估：

| 方案 | 形态 | 评估 |
|---|---|---|
| A. 伪造 tool_use/tool_result 对 | 假装主 agent 调过一次 Task 工具 | **否决**：模型从未发起过该调用，伪造破坏 transcript 真实性；部分 provider 校验 tool_use/tool_result 配对；伪造内容会进后续 model request 误导推理 |
| B. 结构化 user message 注入 | 把旁支结论包成一条 user message 发/注入主对话 | **v1 拍板**：走现有 `send_message` / `InjectUserMessage`（主对话有活跃 run 时插队，无则直接发起），零协议改动 |
| C. 后台任务 notification 形态 | 对齐 §4.12 wakeup notification 的注入风格 | 作为 B 的**格式细则**而非独立方案：让模型把它理解成"一个后台分支任务的结果"，与 subagent 后台完成通知的心智一致 |

v1 = B（格式参考 C）。注入内容两档：

- **结论**（默认）：旁支对话最后一条 assistant 消息
- **总结**（已拍板"用输出总结"）：向旁支会话追加一条总结请求（如"把这次讨论的结论和依据总结成给主对话参考的一段话"），由旁支模型**自己输出总结**，取该输出注入——不复用 L3 压缩 prompt，总结质量随旁支模型走

**导语按场景定制**：`<aside_result>` 的外壳结构统一，但开头那句导语由各宿主场景提供模板（composer 机制）——这句话同时给模型和用户看（主对话里会渲染出来），必须是自然的人话，不写"系统从 XX 模块注入"式的机器腔。内置模板：

```
旁支讨论（branch）回灌：
<aside_result kind="branch" forked_from="<message_id>">
我刚才就着这段对话单独开了个小讨论，把结论带回来，后面可以参考：
…（结论 / 总结内容）…
</aside_result>
```

未来新场景接入时各自提供导语模板，不复用不贴切的措辞。注意：**页面注释不走 aside_result**——它是 attach-main 直发主对话，导语见 §5.4。

主对话 UI 侧该消息渲染成可折叠的"旁支结论"卡片，点击可跳转查看完整旁支会话（保留时）。

**远期**（不在本 spec 排期）：若要做"主 agent 主动等待用户旁支结果"的强语义（真正的工具形态），需引入用户侧委托任务概念，属 §4.4.11 subagent 体系的扩展，到时单独评估。

---

## 4. PreviewPanel：内置浏览器面板

### 4.1 布局与入口

- **右侧 sidebar 常驻浏览器图标**（DesktopSidebar 加一项）：用户随时主动点开，打开后是主界面右侧分屏面板（与 ChatView 左右并排，可折叠收回），不是独立窗口——保证 hebweb 行为一致
- 用户主动使用路径是一等公民：点图标 → 面板展开（空态有地址栏）→ 输入网址直接浏览，与会话无关也能用
- 其余入口：(a) 聊天流检测到 dev server URL 时输入框上方弹"打开预览"chip；(b) auto_open 模式命中时自动展开
- 导航体验按真浏览器标准做：地址栏（回车跳转、显示原始 URL）、后退/前进（含键盘快捷键 ⌘[ / ⌘]）、刷新、加载进度条、页面 title 显示、新标签重置（v1 单标签，多标签 P3）

### 4.2 URL 检测、auto-follow 与访问范围

**自动检测与跟随（照搬 deepseek-gui，§1.3，仅本地地址）**：

- 检测源：`TextDone` 后的 assistant 消息文本 + Bash 工具的 command/output
- 双阈值：候选 chips 用宽松模式；**自动打开仅在严格模式命中**（dev server 输出特征 + 本轮新产生）
- auto-follow 状态机：默认开；用户手动输 URL 即关；agent 新起 server 输出新 URL → preferredUrl 机制强制跟随
- URL 归一化照搬 §1.3 规则（纯数字补端口、无 scheme 补 http、0.0.0.0→127.0.0.1）

**访问范围（两档）**：

| 来源 | 允许范围 | 理由 |
|---|---|---|
| 自动检测 / auto-follow / agent 触发 | **仅本地网段**（§1.3 白名单） | 自动通道永不指向公网——防止对话内容（含 prompt injection）把预览面板带去任意恶意站点 |
| 用户在地址栏主动输入 / 点击页面内链接 | 本地网段 + **公网 http(s)** | 用户主动行为，与在普通浏览器里打开同级；对齐 Codex「支持免登录公网页」 |

公网页面同样经 preview-proxy 加载（注入 inspector.js，批注能力一致，§7.2）。登录支持：用户可在内置浏览器里**直接登录**（表单登录经代理端 cookie jar 维持会话，§7.2）；不继承系统浏览器已有登录态；OAuth popup 流暂不支持（P3）。面板提供「在系统浏览器打开」逃生门。

校验实现为前端 `previewUrl.ts` + proxy 端 Rust 同规则**双重校验**（前端是 UX，proxy 是安全边界；proxy 按「调用方声明的来源档」执行对应范围）。

### 4.3 承载层（选型已定 §11 拍板 4：B 方案）

| surface | 承载 | 注入 | 通信 |
|---|---|---|---|
| Desktop | Tauri 子 webview（multi-webview `unstable`），直接加载目标 URL，前端用占位 div 圈定区域、ResizeObserver 同步 bounds | `initialization_script`（每次导航自动注入 inspector.js） | inspector → Rust：wry IPC；Rust → inspector：`webview.eval` |
| hebweb | iframe，`src` 指向 preview-proxy 地址（§7 降级路径）；`sandbox="allow-forms allow-modals allow-popups allow-same-origin allow-scripts"` | 代理改写 HTML 注入 | `window.postMessage` 双向 |

- Desktop 路径下导航历史用 webview 原生栈（`can_go_back`/`go_back`）；hebweb 沿用 deepseek-gui 的手动栈方案（30 条上限、10s 超时、nonce 重建刷新）
- 地址栏永远显示**真实页面 URL**（hebweb 的代理地址是实现细节，不给用户看——CLAUDE.md UI 文案纪律）
- "在系统浏览器打开"逃生门：打开真实 URL
- 两路差异封装在前端 `BrowserHost` 适配层（接口：navigate/back/forward/reload/setBounds + 事件 navigated/loadState/elementSelected），面板 UI 与注释流对两路无感知

### 4.4 Phase 0 Spike（动工前置，详见 [内置浏览器-tdd.md](内置浏览器-tdd.md) §1）

B 方案依赖 Tauri multi-webview unstable API，动工前必须用最小 demo 验证：子 webview 创建/定位、跨导航注入持续生效、双向 IPC、bounds 同步、导航事件、z-order 实测、cookie 持久化。任何一项不可行 → 回退 A 方案（iframe+代理全量，spec 旧版 §7 升回主路径）。

---

## 5. 页面注释流（核心场景）

### 5.1 进入选取模式

1. PreviewPanel 工具栏 **[选取元素]** 按钮（快捷键 `⌘⇧E`）→ 前端 postMessage `picker:start` 给 iframe
2. inspector.js 进入 picker 模式：`mousemove` → `elementFromPoint` → 高亮 overlay（蓝色半透明填充+实线边框+左上角组件名/标签名标签；rAF 跟踪 rect，10fps 节流）；`Esc` 或再点按钮退出
3. 点击元素 → 该元素转"选中"态（虚线框常驻）→ 采集 `HebElementSnapshot`（§6）→ postMessage `picker:selected` 回前端

### 5.2 注释 UI = QuickChat 的 inline 混合形态

前端收到 snapshot 后，把元素 viewport rect 换算成 iframe 在主窗口中的绝对坐标，在元素旁弹出注释卡片。这张卡片是 QuickChat 的 **inline 呈现**（§3.4）：QuickChat 只贡献「输入框 + attach-main 会话控制」两块积木，与注释场景自己的控件（元素徽章、样式参数编辑器）混合排布——它不是"对话框里塞了个表单"，而是"注释表单里嵌了对话能力"：

```
┌──────────────────────────────────┐
│ ◉ button.btn-primary   <SaveBtn> │ ← 选中元素徽章（标签名 + React 组件名）
│ ┌──────────────────────────────┐ │
│ │ 描述这些更改…                 │ │ ← 自然语言注释输入
│ └──────────────────────────────┘ │
│ ▾ 样式调整                        │ ← 参数编辑器（§5.3，默认折叠一组常用项）
│   字号    [16  ] px              │
│   字重    [400 ▾]                │
│   文字颜色 [■ #1f2937]           │
│   圆角    [6   ] px              │
│   …                              │
│              [取消]  [发送到对话] │
└──────────────────────────────────┘
```

### 5.3 样式参数编辑器（对齐 Codex advanced annotations）

- 数据源：snapshot 的 `computedStyles`（白名单属性，§6）
- 分组：**文字**（font-family/font-size/font-weight/line-height/color/text-align）、**盒模型**（margin/padding/width/height/gap）、**边框背景**（border-radius/border-width/border-color/background-color/box-shadow）、**布局**（display/flex-direction/justify-content/align-items）
- 每次改动 → postMessage `style:apply {prop, value}` → inspector.js 对选中元素 `el.style.setProperty(prop, value)` → **页面实时预览**
- inspector.js 记录 `styleDiff: [{prop, before, after}]`；**取消** → `style:revert` 逐项恢复原 inline 值；**发送** → diff 并入注释 payload，inline 改动保留（HMR 改完源码后刷新自然归位）
- 颜色用取色器、数值用步进输入、枚举用下拉——全部标准表单控件，无自由文本 CSS（防注入也防拼错）

### 5.4 提交：组装 user message

点击 [发送到对话] → `attach-main` 模式走现有 `send_message(session_id, content, attachments, ...)`：

`content`（文本，模型可读自描述，不依赖 system prompt 教学——避免动 STABLE 段破坏 prompt cache，这是与 stagewise preamble 方式的有意分歧）。导语同时给模型和用户看（主对话里渲染出来），写成自然的第一人称，不说"从内置浏览器注释"这种机器腔：

```
我在页面预览里圈了个地方，想这样改：

<web_annotation url="http://localhost:3000/settings" viewport="1280x800">
  <comment>这个按钮改成右对齐，hover 时加个阴影</comment>
  <target>button.btn-primary（React: SettingsPage > Card > SaveBtn）</target>
  <style_changes>
    border-radius: 6px → 12px
    font-weight: 400 → 600
  </style_changes>
</web_annotation>

元素完整快照在附件 element.json。style_changes 里是我在预览上实时调过、确认了效果的精确值，改源码时请原样采用。
```

UI 渲染：主对话里这条消息显示为「页面标注」卡片（缩略 comment + 目标元素徽章，可展开看全文），用户视角干净；模型视角拿到完整结构化文本。

`attachments`：

- `MessageAttachment::TextFile { name: "element.json", media_type: "application/json", content: <HebElementSnapshot 序列化> }`
- （P3）`MessageAttachment::Image { ... }` 元素区域截图

发送后 QuickChat 切到消息流视图展示本次往返；agent 改完代码 → dev server HMR → iframe 即时可见，形成闭环。

### 5.5 多注释与区域注释（P3）

- 多注释：选中后不立即发送，pin 成编号标记（①②③），底部"发送全部"合并为一条 message（`<web_annotation>` 数组形态）
- 区域注释：Shift 拖拽圈 rect，采集 rect 内顶层元素列表摘要
- Cmd+点击：跳过对话框直接用空注释+默认文案发送（Codex 同款加速）

---

## 6. HebElementSnapshot 数据结构

蓝本为 stagewise `.swdomelement` schema（§1.4），按 hebbian 需要裁剪。前端 TS 类型 + 注入脚本共用一份定义；Rust 侧**不定义此类型**（它只是附件 JSON 的内容，agent-core 不感知）：

```typescript
interface HebElementSnapshot {
  // 上下文
  url: string;                    // 页面原始 URL（非代理地址）
  viewport: { width: number; height: number };
  capturedAt: string;             // ISO 时间戳

  // 元素身份（agent 的 grep 锚点，最重要）
  tagName: string;
  id?: string;
  classList: string[];
  selectorPath: string;           // 最短可定位 CSS 路径（含 nth-child 兜底）
  xpath: string;
  attributes: Record<string, string>;   // 截断：每值 ≤200 字符
  innerText?: string;             // 截断 ≤500 字符

  // 框架信息（React 优先，Vue 后置）
  react?: {
    componentChain: string[];     // 由近及远的组件名链，如 ["SaveBtn","Card","SettingsPage"]
    props: Record<string, string>; // 截断：≤20 项、每值 ≤100 字符，序列化失败标 "[NOT SERIALIZABLE]"
    source?: { file: string; line: number };  // 仅当页面存在 data-source 类构建插件标记时
  };

  // 视觉与样式
  boundingClientRect: { x: number; y: number; width: number; height: number };
  computedStyles: Record<string, string>;  // 白名单属性（§5.3 四组，约 30 个）
  styleDiff?: { prop: string; before: string; after: string }[];  // 参数编辑器的改动

  // 层级摘要（轻量，不递归）
  parent?: { tagName: string; classList: string[] };
  childrenSummary?: string[];     // 子元素 tagName 列表，≤10 个
}
```

截断纪律：单个 snapshot 序列化后目标 < 8KB——它会进 transcript 与每轮 model request，必须克制（工具输出阈值精神同 §4.1.3）。

---

## 7. preview-proxy（新 crate，**hebweb 降级路径专用**——选型拍板 4 后 Desktop 不再使用）

### 7.1 位置与形态

- `crates/preview-proxy`：纯库 crate（axum/hyper 反代 + HTML 注入），不落任何 `~/.hebbian` 数据
- 依赖方向：仅 `apps/web-server` → `preview-proxy`（apps 层组件，**agent-core 不依赖它**，DAG 不变）
- 生命周期：每个 target 一个实例，监听 `127.0.0.1:0`（随机端口），同时存活 LRU 上限 4；hebweb server 内部起停；进程退出即销毁，无持久状态
- 排期：P2 只做 Desktop（子 webview 路径）；本 crate 随 hebweb 镜像放 P2.5/P3（§10）

### 7.2 行为

| 行为 | 细节 |
|---|---|
| 转发 | 保留 method/headers/body；改写 `Host`；上游为 https 时由 proxy client 持 TLS（页面侧统一是 `http://127.0.0.1:port`，无混合内容问题）；同 host 重定向改写为代理地址 |
| HTML 注入 | 仅 `Content-Type: text/html`；gzip/br/deflate 先解压；`</head>` 前插入 `<script src="/__hebbian__/inspector.js" data-hebbian></script>`（无 head 则文档首部）；重算 `Content-Length`，去 `Content-Encoding` |
| 绝对 URL 重写 | HTML 改写时把**同 host** 的绝对 URL（`href`/`src`/`srcset`/`action`）重写为代理相对路径，防止页面内导航/资源加载脱出代理；跨 host 的 JS 动态请求（fetch/XHR 绝对地址）不处理——浏览器直连原站即可工作，只是不经代理（无注入需求） |
| 跨 host 导航 | iframe 内点击跨 host 链接会脱代理（丢注入）；inspector.js 经 `heb:navigated` 上报，前端提示并可一键"以新地址重新代理"（对该 host 起新 proxy 实例，同时存活实例 LRU 上限 4 个） |
| Cookie / 登录态 | **代理端 cookie jar**：`Set-Cookie` 不透传给浏览器，全部存 proxy 进程内（按 target host 隔离），后续请求由 proxy 自动附加。用户可以**在内置浏览器里直接登录**（表单提交经代理，会话由 jar 维持）。选择 jar 而非透传的原因：cookie 不区分端口，透传会让多个 target 在 `127.0.0.1` 下互相串；且原站 `Domain`/`Secure`/`__Host-` 前缀属性在代理域下会被浏览器拒收，WKWebView 的 ITP 还会限制 iframe 第三方 cookie——jar 一并绕开。代价：页面 JS 读 `document.cookie` 拿不到值（依赖此的前端逻辑会异常）。**不**继承系统浏览器已有登录态（Chrome cookie 加密存储，本就拿不到）；OAuth popup 登录流不经代理，P3 评估 |
| 静态资源 | `/__hebbian__/inspector.js` 由 proxy 自答（`include_str!` 编译期内嵌，见 §8.1） |
| WS 透传 | Upgrade 请求双向裸转发（vite/next HMR 必需） |
| 头处理 | 剥响应头 `X-Frame-Options`、CSP 中的 `frame-ancestors`；其余 CSP 指令保留但追加 `script-src` 放行自身脚本（若原页面有严格 CSP） |
| SSE | `text/event-stream` 直通不缓冲 |

### 7.3 安全

- target 校验在 **proxy 端强制**（前端校验只是 UX），按来源档执行（§4.2）：自动通道仅本地网段；用户主动导航放行公网 http(s)，但**永远拒绝**非 http(s) scheme 与本机敏感端口段以外的内网探测式地址（如 `169.254.169.254` 云元数据地址列入硬黑名单）
- 只监听 `127.0.0.1`；不做鉴权（威胁模型：本地 target 本机进程本就可直连；公网 target 相当于本机发起的普通 http 请求。需要意识到代理对局域网其他进程暴露了"借道访问公网"的通路——监听 loopback 已排除局域网，本机恶意进程自身就能直接联网，无新增面）
- inspector.js 的 postMessage 通信双向校验：父侧校验 `event.source === iframe.contentWindow`；子侧记录首次握手的 `event.origin` 后续比对
- 公网页面是不可信内容：inspector.js 不向页面暴露任何可调用接口（只监听 postMessage 且校验来源）；snapshot 内容进对话前在前端做长度与字段白名单清洗（页面可控的 innerText/attributes 视为不可信输入，截断规则 §6 同时是安全边界）

### 7.4 API 增量

```
【预览代理】（hebweb 内部 WS invoke，不进 CoreClient——浏览器承载是 surface 能力非 core 业务）
  startPreviewProxy(targetUrl, origin) → { proxyUrl }   origin = Auto | UserNavigation，
                                                        按 §4.2 两档执行范围校验，越界直接报错
  stopPreviewProxy(proxyUrl) → Result
```

Desktop 不需要这两个接口（子 webview 直连）；Desktop 自己的浏览器 Tauri commands 见 [内置浏览器-tdd.md](内置浏览器-tdd.md) §2。

---

## 8. inspector.js（注入脚本）

### 8.1 构建与分发

- 源码在前端仓库 `apps/desktop/frontend/src/inspector/`（独立 vite entry，无 React 依赖，目标 gzip < 15KB）
- 构建产物 `inspector.js` 由 build 脚本同时供给两路：Desktop 侧 `include_str!` 进 `initialization_script`；hebweb 侧拷入 `crates/preview-proxy/assets/` 内嵌自答——均为单二进制分发，无运行时文件依赖

### 8.2 消息协议（前端 ↔ inspector，传输层抽象）

inspector.js 内部一个 `bridge` 适配层选传输：检测到 `window.ipc.postMessage`（wry 注入）走 Tauri 子 webview 通道（出向 wry IPC、入向由 Rust `webview.eval` 调 `window.__HEB_RX__(msg)`）；否则走 iframe `postMessage` 通道。消息体两路完全一致：

| 方向 | 消息 | 载荷 |
|---|---|---|
| → | `heb:picker:start` / `heb:picker:stop` | — |
| ← | `heb:picker:selected` | `HebElementSnapshot` |
| ← | `heb:picker:cancelled` | —（Esc） |
| → | `heb:style:apply` | `{ prop, value }` |
| → | `heb:style:revert` | —（恢复全部 inline 改动） |
| → | `heb:overlay:hide` / `heb:overlay:show` | —（截图前后，P3） |
| ← | `heb:ready` | `{ url, title }`（脚本加载完成握手） |
| ← | `heb:navigated` | `{ url, title }`（SPA 路由变化，监听 history API） |

消息一律带 `source: "hebbian-inspector"` 字段过滤无关 postMessage。

### 8.3 实现要点

- picker：`mousemove` → `document.elementFromPoint`（跳过自身 overlay：`pointer-events: none` + `data-hebbian-overlay` 过滤）；overlay 为两个绝对定位 div（hovered/selected），rAF + 10fps 节流跟踪 rect
- fiber 提取（独立重写，要点为公开常识）：遍历元素自有属性找 `__reactFiber$` 前缀键 → 沿 `fiber.return` 上行收集函数/类组件的 `displayName ?? name`（≤8 层）→ 最近组件的 `memoizedProps` 按 §6 截断规则序列化；找不到 fiber 则 `react` 字段缺省，**不报错**
- `source.file` 来源：仅识别元素上 `data-source` / `data-inspector-*` 类属性（用户项目自装了相关构建插件才有）；hebbian **不要求**用户改构建配置——没有 file:line 时组件链+class+innerText 已够 agent Grep 定位
- computedStyles：`getComputedStyle` 按白名单逐项读取（不可枚举全量——2000+ 属性会爆 snapshot 体积）
- 防御：全部逻辑包在 try/catch + IIFE 私有作用域；任何异常静默降级为"无该项数据"，**绝不影响宿主页面运行**

---

## 9. 架构影响评估（CLAUDE.md 5 问）

1. **是否与架构.md 相悖？** 否。Surface 是壳（§0-1）：三部件全在 surface 层；注释 = 普通 user message，agent loop 无新分支；不动 STABLE prompt（§0-9）：注释格式自描述于 user message 内
2. **是否符合既定设计？** 复用 §3.1 Op（StartRun/InjectUserMessage/Fork）、§3.2 同步 API 风格（additive 追加）、§4.4 工具系统（agent 用既有 Grep/Read/Edit 完成修改）、§8 命令系统（`//btw` 进表 A）
3. **是否引入新设计 / 需修改架构.md？** 是，批准后增补：
   - §3.2 追加【预览代理】两个方法 + listSessions filter 的 `include_aside`
   - §4.1 Session meta 追加 `aside` 字段说明（旁支对话）
   - 新增 §7.8（或 §8.5）「内置浏览器与旁支对话（QuickChat）」概要节，正文引用本 spec
   - §8.2 表 A 的 `//btw` 待该入口排期时再登记
   - §13 决策表追加：「内置浏览器 Desktop 用 Tauri 子 webview（multi-webview unstable + initialization_script 注入），hebweb 降级代理+iframe——理由：真浏览器体验（真 cookie / 登录 / 任意公网）是核心诉求，代理模拟（cookie jar、绝对 URL 重写）是补丁堆；代价：unstable API 风险（P0 spike 把关，留回退）+ 两路 BrowserHost 实现」
   - workspace 布局图追加 `crates/preview-proxy`
4. **影响哪些模块？** apps/desktop（commands + 前端三组件 + DesktopSidebar 入口）、apps/web-server（镜像 invoke）、新 crate preview-proxy、agent-core 仅 storage 的 aside 字段（additive，老 jsonl 兼容）。protocol / model-gateway / prompts 零改动；prompt cache 不受影响（system prompt 完全不动）
5. **取舍是否清楚？** Desktop 子 webview 换来真浏览器体验，代价是 unstable API 风险（P0 spike 把关、保留回退 A 的退路）与 Desktop/hebweb 两路实现（差异封装在 BrowserHost 适配层与 inspector 传输层，业务代码两路共用）。hebweb 降级路径的公网支持是 best-effort：表单登录经代理 jar 可用，OAuth popup/重 CSP 页明确降级提示（fail-closed 不静默）。自动通道永不指向公网是两路共同的安全底线。returnToChat 用 user message 注入而非伪造 tool_result，牺牲了"工具结果"的强语义，换取 transcript 真实性与零协议改动。ephemeral 是唯一 core 触点，权衡过纯前端方案（localStorage 记临时会话列表）：会破坏"jsonl 是唯一事实"原则，否决

---

## 10. 分期与验收

### P0：Spike（B 方案可行性，详见 [内置浏览器-tdd.md](内置浏览器-tdd.md) §1）

- 最小 demo 验证 multi-webview 七项能力（创建定位/跨导航注入/双向 IPC/bounds 同步/导航事件/z-order/cookie 持久化）
- 任何一项不可行 → 回退 A 方案并更新本 spec

### P1：BrowserPanel（Desktop 子 webview，纯浏览，无注释）

- 范围：§4 全部——sidebar 图标入口、子 webview 创建与 bounds 同步、地址栏/导航/title/进度、URL 检测/auto-follow/访问两档、逃生门
- 验收：heb 起一个 session 让 agent `pnpm dev` 一个 vite 项目 → Desktop 聊天流出现预览 chip → 自动打开面板并渲染页面；sidebar 图标手动打开输入公网址正常浏览、表单登录直接可用（真 cookie）；prod build 实测
- 不含：选取、注释、QuickChat、hebweb

### P2：inspector.js 注入 + 注释流（已拍板：只做 web 注释，旁支对话整体后移）

- 范围：§5（注释流，单注释，attach-main + inline 形态）、§6、§8；QuickChat 只实现注释所需的 inline 积木（SessionChatController + 输入框）
- 验收（修 bug 流程同款 A/B 标准）：
  1. 选取 vite demo 页上一个按钮 → 注释卡片弹出且徽章显示正确组件名
  2. 参数编辑器改 `border-radius` → 页面实时变化 → 取消 → 恢复
  3. 写注释"把这个按钮文案改成保存并加粗"发送 → 主对话收到「页面标注」消息（含 element.json 附件）→ agent Edit 源码 → HMR 后页面呈现改动，全程无人工指路
  4. 公网页（如 example.com）选取元素批注 → snapshot 的 url/innerText 正确
- 回归测试：见 [内置浏览器-tdd.md](内置浏览器-tdd.md) §3 测试清单

### P2.5：hebweb 降级路径

- 范围：§7 preview-proxy 全部（HTML 注入/WS 透传/cookie jar/绝对 URL 重写）+ BrowserHost iframe 实现 + hebweb invoke 镜像
- 验收：hebweb 上 Playwright 全链路（选取→批注→发送→agent 改码→HMR）；表单登录经 jar 会话保持、双 target 不串扰
- 回归测试：proxy HTML 注入单测（gzip/无 head/分块/绝对 URL 重写）、公网档校验单测（含元数据地址硬黑名单）

### P3：旁支对话 + 注释增强

- **旁支对话完整落地**：§3 四维度（branch/fresh + floating 形态 + aside 标记与过滤 + returnToChat）、消息气泡右键入口；验收含：开旁支聊题外话 → 主对话 transcript 无新增行（diff session.jsonl 验证）→ 关闭选丢弃 → 会话目录被删；旁支（chat-only 档）讨论后点「返回主对话」→ 主对话出现 `<aside_result>` 且渲染为旁支结论卡片；aside 过滤单测
- 注释增强：多注释合并发送、Shift 区域注释、Cmd 快发、元素截图附件（子 webview 路径可用 WKWebView 快照能力，另评）、Vue 支持
- 浏览器增强：多标签、OAuth popup 登录流（子 webview 路径下 popup 可在新 webview 承载，可行性高）

---

## 11. 风险与开放问题

| 风险 | 应对 |
|---|---|
| WKWebView prod 模式 iframe-http 策略未实测 | P1 验收前置项（§4.4），有备选路径 |
| 复杂 CSP 页面注入后脚本被拒执行 | 目标场景 dev server 几乎无 CSP；有 CSP 时按 §7.2 改写；仍失败则面板提示"该页面无法标注"（fail-closed，不静默） |
| React 19 拿不到源码位置 | 设计上不依赖 file:line（grep 锚点足够）；`data-source` 属性是 opportunistic 增强 |
| 绝对 URL 资源绕过代理 | HTML 属性级同 host 重写（§7.2）覆盖主要场景；JS 动态拼接的导航靠 `heb:navigated` 检测脱代理并提示一键"重新代理" |
| 公网页面 = 不可信内容进对话 | snapshot 字段白名单 + 截断是安全边界（§7.3）；自动通道永不指向公网；云元数据等探测地址硬黑名单 |
| 公网重 CSP 页面注入失败 | 明确降级提示"该页面无法标注"，逃生门开系统浏览器；不追求全网可批注 |
| 登录流局限 | 表单登录可用（代理 jar）；OAuth popup / 依赖 `document.cookie` 的页面会异常——降级提示 + 逃生门；jar 默认进程内不落盘（关浏览器面板即登出），持久化作 P3 选项 |
| AGPL 污染 | 仅借鉴 schema 设计与交互（本 spec 已转述为自有描述），实现全部独立编写；inspector.js 不引用 stagewise 任何代码 |
| QuickChat 与主 ChatView 双处订阅同一 session 事件（attach-main 模式） | 事件本就广播给前端，双视图渲染同一数据流无写冲突；输入侧 request_id 隔离 |

已拍板（2026-06-10）：

1. 旁支会话继承主会话 provider+model，QuickChat 头部模型选择器可切换（§3.2）
2. `//btw` 暂不做，P2 只做 web 注释，旁支对话整体移 P3（§3.6 / §10）
3. returnToChat 总结档 = 让旁支模型自己输出总结（§3.7），不复用 L3 压缩 prompt
4. **浏览器承载选型 = B 方案**：Desktop 用 Tauri 原生子 webview（multi-webview `unstable` feature + `initialization_script` 注入，真浏览器体验：真实 origin / 真 cookie / 登录 / 任意公网站点，无需代理改写）；hebweb 降级用 preview-proxy + iframe（§7 整节降级为 hebweb 专用路径，代理端 cookie jar / 绝对 URL 重写等补丁仅存在于这条降级路径）。inspector.js 本体两路共用，传输层抽象（wry IPC / postMessage）。实现级设计见 [内置浏览器-tdd.md](内置浏览器-tdd.md)，其 Phase 0 spike 通过前 B 方案保留回退到 A 的退路
