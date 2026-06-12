# 内置浏览器：多元素注释 + 统一汇总浮窗 + 真实结构操作

> 设计准则锚点：[架构.md §8.5 内置浏览器与页面注释](../../架构.md)。本设计不改 protocol / agent-core 主路径 / prompt cache，新增能力全部 additive。

## 1. 背景与动机

现状（`apps/desktop/src/browser/inspector.js`）一个注释框只能绑定**单个**元素：选中 → 调样式 / 和助手对话 → 提交到主对话。三个痛点：

1. **无法做元素之间的修改**：用户想表达「让 A 和 B 对齐」「把 C 移到 D 下面」时，注释框只认一个元素，助手拿不到第二个元素的定位。
2. **汇总浮窗形同虚设**：现有的「修改队列框」（`editQueue` / `renderQueuePanel`）只在**手动样式 diff 非空**时才出现，纯对话不触发——用户从没见过它，等于不存在。
3. **助手只能改样式，不能改结构**：`PreviewStyle` 信号工具只能改 CSS。用户想新增一个按钮、删除多余分隔线、改文案，预览里看不到效果，只能凭空想象。

## 2. 目标

- **A. 多元素注释框**：一个注释框可选中多个元素，标题下方以小方块 `[1][2][3]` 列出；hover 高亮、点击切换、可移除。
- **B. 对话 @N 引用**：contenteditable 输入框里用 `@1`/`@2` 引用选中元素（高亮 chip），助手据此做元素之间的修改，且能调任意被引用元素的样式。
- **C. 真实结构操作**：助手能在预览里真实新增 / 删除元素、改文本内容（草稿态），用户所见即所得。
- **D. 统一汇总浮窗**：一个浮窗收集多条注释（每条 = N 元素 + 样式 diff + 对话 + 结构改动，任意非空即可加入），可逐条展开、一键全部提交到主对话由助手一起总结。

## 3. 非目标

- **不改源码落地**：预览里的样式 / 结构改动都是**草稿态**（刷新即失）。真正进产品靠「提交到主对话」让主对话改源码 JSX。预览永不直接写源码（§8.5 铁律）。
- **不动 hebweb 降级路径的新增能力**：本期聚焦 Desktop 子 webview；iframe 路径沿用同一份 inspector.js，传输层已抽象，天然兼容。
- **不做 @N 的跨注释框引用**：@N 只在当前注释框的选中列表范围内。

## 4. 架构定位

仍是 §8.5 三条铁律内的纯 surface 能力：

| 层 | 职责 | 改动 |
|---|---|---|
| `inspector.js`（注入页面） | 多元素状态机、小方块、@N chip、结构操作执行、统一浮窗 | 主战场 |
| `apps/desktop/src/browser/mod.rs` | 旁支会话绑定多元素、观察 `PreviewMutate` 下发、多注释合并总结 | 中等 |
| `crates/agent-core/src/tools/preview_style.rs` + 新增 `preview_mutate.rs` | `PreviewStyle` 加 `target`；`PreviewMutate` 信号工具 | 小 |
| `App.tsx` / `annotation.ts` / `browserHost.ts` | 消费端 + 上行契约 | 小 |
| `docs/架构.md §8.5` + changelog | 登记新工具 `PreviewMutate` | 文档 |

**协议 / prompts / model-gateway 零改动。** 新增的 `browser://` 内部消息和旁支会话工具均 additive，旧客户端无感。

## 5. 详细设计

### 5.1 多元素状态模型（inspector.js）

现状用两个游离全局 `selectedTarget` / `styleDiff`。改成一个**注释框会话对象**，承载多元素：

```
draft = {
  elements: [ { key, el, snapshot, styleDiff: {prop:{before,after}} } ],  // 选中元素，按选中顺序
  activeIndex: 0,        // 当前激活元素（样式编辑器 + 盒模型图作用对象）
  asideSession: null,    // 旁支会话 id（整个 draft 共享一个，绑定全部 elements）
  asideMessages: [],     // 对话历史
  structuralChanges: [], // 结构改动记录（新增/删除/改文本），提交时描述给主对话
}
```

- `elements[0]` = 主元素（首次选中）。`@1` = elements[0]，`@2` = elements[1]……（1-based，对齐用户心智）
- 每个元素**独立的 styleDiff**（手动样式编辑器「每个都能调」——点小方块切 activeIndex，编辑器读写 `elements[activeIndex].styleDiff`）
- `key` 复用现有 `elementKeyOf(el)`（`__hebAsideKey__`），保证 DOM 重渲染后仍能定位

### 5.2 元素选择器与小方块（功能 A）

**头部右上角**（× 左边）加 ➕ 图标按钮：
- 点击 → `startPicker()` 进入「追加模式」（新增标志 `pickerMode = "append"`）
- 追加模式下 `onClick` 选中的元素 **push 进 `draft.elements`** 而非新建注释框；选完自动退出 picker，刷新小方块

**标题下方小方块行** `renderChips(draft)`：
- 每个元素一个圆角小方块，内容 = 序号；激活态填充 `#2f81f7`、其余 `#f1f3f5` 描边
- `mouseenter` → 复用 `positionOverlay` 在页面高亮该元素 + badge tooltip；`mouseleave` 收起
- `click` → 设 `activeIndex`，重渲染样式编辑器（盒模型图 / CARD_FIELDS / 全部 CSS 切到该元素）
- 每个方块右上角 `×`：移除该元素，`elements.length === 1` 时禁止移除（至少留主元素），移除后重排显示序号

### 5.3 @N 引用输入框（功能 B）

`chatInput` 从 `<textarea>` 改 `contenteditable` div：
- 输入 `@` → 弹出小浮层列出 `1/2/3 + badge`，方向键 / 点击选择 → 插入 chip
- 直接键入 `@2` → 失焦 / 空格时自动识别成 chip
- chip = 不可编辑的 `<span data-heb-ref="2">@2</span>`，蓝底白字小圆角；`mouseenter` 高亮对应元素
- 退格删 chip 整体删除（contenteditable 原生行为）
- **发送时**：遍历 contenteditable 子节点，文本原样、chip 还原成 `「元素2: <elementLocator(snapshot)>」`，拼成喂给助手的 user content。于是助手能感知多个元素的定位、做元素之间的修改

**IME 处理**：监听 `compositionstart`/`compositionend`，合成期间不触发 `@` 弹层（避免中文输入「@」误判）。

### 5.4 助手改任意 @N 元素的样式（功能 ①）

旁支会话从「绑定 1 元素」升级为「绑定整个 draft 的 N 元素」：

- `heb:aside:send` 载荷带 `elements: [{ref:1, locator}, {ref:2, locator}…]` 而非单个 element
- **元素定位放每轮 user content，不放 system prompt**：现状 `aside_system_prompt` 在创建会话那一刻用单个 `element_desc` 固化（mod.rs:1061）。多元素场景用户可能「先对话建会话、之后再 ➕ 追加元素」，固化的 system prompt 接不到新元素。改为——system prompt 只讲「你是预览样式/结构助手，可对带 @N 标号的元素调样式/改结构」的通用规则；每轮 `heb:aside:send` 把**当前全部选中元素的 @N + locator** 拼进 user content 前缀。这样追加元素后下一轮对话助手立刻可见，且不破坏 prompt（system 固定 → 仍命中缓存）
- `PreviewStyle` 工具加可选参数 `target`（`@N`，默认 `@1`）：

```rust
// preview_style.rs
pub struct PreviewStyleInput {
    pub prop: String,
    pub value: String,
    #[serde(default)]
    pub target: Option<String>,  // "@2" 指定改哪个选中元素，缺省主元素
}
```

- `route_aside_event` 观察 `PreviewStyle` 时把 `target` 一并下发 `heb:aside:apply`
- inspector 收到后按 `target` 解析到 `draft.elements[N-1].el` 应用样式（而非全局 currentTarget）

### 5.5 真实结构操作（功能 C）—— 新增工具 PreviewMutate

新增信号工具 `crates/agent-core/src/tools/preview_mutate.rs`，与 `PreviewStyle` **同源机制**（agent-core 不碰 webview，只发信号，Desktop 观察后下发 inspector 执行）：

```rust
pub const PREVIEW_MUTATE_TOOL_NAME: &str = "PreviewMutate";

pub struct PreviewMutateInput {
    pub op: String,                  // "append" | "remove" | "setText"
    #[serde(default)]
    pub target: Option<String>,      // "@N" 操作哪个选中元素，默认 @1
    #[serde(default)]
    pub html: Option<String>,        // op=append 时：新元素的 HTML 片段
    #[serde(default)]
    pub text: Option<String>,        // op=setText 时：新文本
}
```

- description 明确告诉模型：这是**预览草稿**，最终要在提交后由主对话改源码实现；append 的 html 要语义干净（便于映射回 JSX）
- `execute` 返回确认句（信号工具范式）
- `route_aside_event` 加 `name == "PreviewMutate"` 分支 → 下发 `heb:aside:mutate`
- inspector `handleIn` 加 `heb:aside:mutate`：
  - `append`：`target.insertAdjacentHTML("beforeend", html)`，新元素**自动 push 进 draft.elements**（多一个小方块，可继续 @它）
  - `remove`：`target.remove()`，从 draft.elements 删除对应项 + 记一条结构改动
  - `setText`：`target.textContent = text`
  - 每次操作在对话流里显示一条 tool 气泡（🔧 新增/删除/改文本），并记进 `draft.structuralChanges[]` 供提交时描述给主对话

**安全**：`html` 用 `insertAdjacentHTML` 注入到**目标页面**（本就是用户自己的预览页，非 hebbian UI），不经过 hebbian 特权上下文，无提权风险；旁支会话 `restrict_tools` 白名单加 `PreviewMutate`（与 PreviewStyle 并列，绝不暴露 Bash/Edit）。

### 5.6 统一汇总浮窗（功能 D）

重做 `editQueue` / `renderQueuePanel` 为 `annotationList` / `renderAnnotationPanel`：

- 一个注释项 = 一条完整 draft 快照：`{ elements:[{snapshot, styleDiff}], asideMessages, structuralChanges }`
- 注释框底部**两个出口**（用户已定）：
  - **「加入列表」**：当前 draft（样式 diff / 对话 / 结构改动**任意非空**即可）存进浮窗，关闭注释框
  - **「提交到主对话」**：单条直接提交（保留快捷路径），走现有单条总结
- 浮窗（视觉同注释框：白底圆角阴影、可拖动，默认右下角，标题「注释列表 (N)」）：
  - 每项：元素小方块组 + 一句摘要（有对话取对话要点，纯样式显示「调了 N 项样式」，有结构改动显示「新增/删除 N 处」）
  - 点击项 → **重新展开**该注释框（恢复 draft.elements 选中态、对话历史、结构改动、高亮）。实现：把注释项的 draft 快照塞回 `showAnnotationCard`
  - 每项 `×` 移除
  - 底部 **「全部提交到主对话」**：所有注释项打包发 `heb:annotation:submit-all`

### 5.7 多注释合并总结（功能 D 提交链路）

`heb:annotation:submit-all` 上行 → `mod.rs` 新增 `handle_annotation_submit_all`：

- 把 N 条注释（每条的元素定位 + 样式 diff + 对话要点 + 结构改动）拼成一个总 prompt
- 复用旁支会话总结机制（`aside_send_args` enabled_tools=[] 纯文本总结），让助手**一起总结**成一份整合的、连贯的修改需求
- 总结结果 emit `browser://annotation-summary` → `App.tsx` 组装成 user message 发主对话（带各元素 element-N.json 附件）

纯样式 / 纯结构 / 纯对话的项都并入同一次总结，主对话拿到的是一份完整连贯需求。

## 6. 数据流时序

```
选元素 → ➕ 追加更多 → 小方块[1][2][3]
  ↓
对话框 @1 @2 「让这俩对齐」→ heb:aside:send(elements) → 旁支会话
  ↓ 助手调 PreviewStyle{target:@2} / PreviewMutate{op:append,target:@1}
  ↓ route_aside_event 观察 → heb:aside:apply / heb:aside:mutate
  ↓ inspector 实时改预览（多一个小方块）
  ↓
「加入列表」→ annotationList push → 浮窗 [注释1][注释2]
  ↓ 点注释项 → 重新展开注释框（恢复全部状态）
  ↓
「全部提交到主对话」→ heb:annotation:submit-all → mod.rs 合并总结
  ↓ browser://annotation-summary → App.tsx → sendUserMessage → 主对话改源码
```

## 7. 错误处理与边界

- **元素 detach**（React 重渲染换 DOM 节点）：每个 element 复用现有 `currentTarget()` 的 selector/xpath 找回逻辑，扩展成 per-element
- **@N 引用了已移除的元素**：发送时该 chip 还原成「元素2(已移除)」，助手知情
- **append 的 html 非法**：inspector try/catch 静默，对话流提示「新增失败：HTML 不合法」
- **浮窗为空**：N=0 时浮窗自动隐藏（沿用现有 `renderQueuePanel` 空态逻辑）
- **draft 至少 1 个元素**：最后一个元素禁止移除

## 8. 测试策略

- **inspector.js 纯函数核心**（`__hebCore`，node 可测）：新增 `@N` 解析（contenteditable → locator 还原）、draft 序列化/反序列化（浮窗存取）写单测，落 `annotation.test.ts` 同级
- **preview_mutate.rs**：`execute` 返回确认 + 参数解析单测（`cargo test -p agent-core --lib`）
- **复现验证**（按 CLAUDE.md 修 bug 流程）：hebweb + Playwright 跑全链路——选多元素 → 小方块 hover 高亮 → @N 对话 → 助手 append → 加入列表 → 展开 → 全部提交，DOM/截图核对
- **回归**：`PreviewStyle` 无 target 时默认主元素（向后兼容旧旁支会话）

## 9. 架构.md 同步（实施时必做）

按 CLAUDE.md「引入新工具必须先更新架构.md」：

- §8.5 第 2 条补充「注释支持多元素 + 结构草稿操作」
- §4.4 工具表 / BUILTIN_TOOL_NAMES 说明区登记 `PreviewMutate`（与 `PreviewStyle` 同为旁支会话专属信号工具，不进通用工具集）
- §13 决策表追加一行：预览结构操作 = 草稿态 + 提交时由主对话落地源码（不直接写源码）
- changelog 追加一条

## 10. 实施顺序建议

1. agent-core：`PreviewStyle` 加 target + 新增 `preview_mutate.rs`（最底层，可独立单测）
2. inspector.js：多元素状态模型 + 小方块（功能 A 骨架）
3. inspector.js：@N contenteditable 输入框（功能 B）
4. mod.rs + inspector.js：旁支多元素绑定 + PreviewMutate 下发执行（功能 ①③）
5. inspector.js + mod.rs + App.tsx：统一浮窗 + 合并总结（功能 D）
6. 架构.md + changelog + 端到端复现验证
