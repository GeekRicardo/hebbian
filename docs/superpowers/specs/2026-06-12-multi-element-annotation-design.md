# 内置浏览器：多元素注释 + 统一汇总浮窗 + 真实结构操作 + 浏览器交互/视觉

> 设计准则锚点：[架构.md §8.5 内置浏览器与页面注释](../../架构.md)。本设计不改 protocol / agent-core 主路径 / prompt cache，新增能力全部 additive。

## 1. 背景与动机

现状（`apps/desktop/src/browser/inspector.js`）一个注释框只能绑定**单个**元素：选中 → 调样式 / 和助手对话 → 提交到主对话。四个痛点：

1. **无法做元素之间的修改**：用户想表达「让 A 和 B 对齐」「把 C 移到 D 下面」时，注释框只认一个元素，助手拿不到第二个元素的定位。
2. **汇总浮窗形同虚设**：现有的「修改队列框」（`editQueue` / `renderQueuePanel`）只在**手动样式 diff 非空**时才出现，纯对话不触发——用户从没见过它，等于不存在。
3. **助手只能改样式，不能改结构**：`PreviewStyle` 信号工具只能改 CSS。用户想新增一个按钮、删除多余分隔线、改文案，预览里看不到效果，只能凭空想象。
4. **前端不全是死样式**：弹窗、hover 菜单、表单交互态光调 CSS 测不出来；助手既不能**操作**页面（点击/输入/滚动/hover/按键）触发这些状态，也**看不见**操作后的真实效果。

## 2. 目标

- **A. 多元素注释框**：一个注释框可选中多个元素，标题下方以小方块 `[1][2][3]` 列出；hover 高亮、点击切换、可移除。
- **B. 对话 @N 引用**：contenteditable 输入框里用 `@1`/`@2` 引用选中元素（高亮 chip），助手据此做元素之间的修改，且能调任意被引用元素的样式。
- **C. 真实结构操作**：助手能在预览里真实新增 / 删除元素、改文本内容（草稿态），用户所见即所得。
- **D. 统一汇总浮窗**：一个浮窗收集多条注释（每条 = N 元素 + 样式 diff + 对话 + 结构改动，任意非空即可加入），可逐条展开、一键全部提交到主对话由助手一起总结。
- **E. 浏览器交互**：助手能在预览里真实操作页面——点击、输入文字、滚动、hover、按键——触发弹窗 / 菜单 / 表单等交互态。
- **F. 让助手「看见」**：助手能截取交互后的页面渲染图，回传作为视觉输入，自己判断效果对不对（复用现有 Image 附件 + VisionBridge 管线）。
- **G. 未提交防丢失**：汇总浮窗里有未提交注释时刷新 / 离开页面，弹窗警告，避免草稿白丢。

## 3. 非目标

- **不改源码落地**：预览里的样式 / 结构改动都是**草稿态**（刷新即失）。真正进产品靠「提交到主对话」让主对话改源码 JSX。预览永不直接写源码（§8.5 铁律）。
- **不动 hebweb 降级路径的新增能力**：本期聚焦 Desktop 子 webview；iframe 路径沿用同一份 inspector.js，传输层已抽象，天然兼容。
- **不做 @N 的跨注释框引用**：@N 只在当前注释框的选中列表范围内。
- **截图不追求像素级保真**：用 DOM 渲染（html2canvas），阴影/滤镜/渐变/跨域图等复杂 CSS 还原近似。够用于验证「交互后状态变了没、布局对不对」，工具 description 会诚实告诉模型这是近似渲染，避免它对颜色/阴影过度自信。不做 OS 级屏幕截图（避开 DPI / 窗口遮挡的工程黑洞）。

## 4. 架构定位

仍是 §8.5 三条铁律内的纯 surface 能力：

| 层 | 职责 | 改动 |
|---|---|---|
| `inspector.js`（注入页面） | 多元素状态机、小方块、@N chip、结构操作执行、统一浮窗、交互动作执行、html2canvas 截图 | 主战场 |
| `apps/desktop/src/browser/mod.rs` | 旁支会话绑定多元素、观察 `PreviewMutate` / `PreviewAct` / `PreviewCapture` 下发、回传截图作为工具结果、多注释合并总结 | 中等 |
| `crates/agent-core/src/tools/preview_style.rs` + 新增 `preview_mutate.rs` / `preview_act.rs` / `preview_capture.rs` | `PreviewStyle` 加 `target`；新增结构 / 交互 / 截图信号工具 | 小 |
| `App.tsx` / `annotation.ts` / `browserHost.ts` | 消费端 + 上行契约 | 小 |
| `docs/架构.md §8.5` + changelog | 登记新工具 | 文档 |

**协议 / prompts / model-gateway 零改动。** 新增的 `browser://` 内部消息和旁支会话工具均 additive，旧客户端无感。截图回传复用既有 `MessageAttachment::Image` + `VisionBridgeClient`（弱文本模型自动降级转文字），**视觉管线零新建**。

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

### 5.8 浏览器交互（功能 E）—— 新增工具 PreviewAct

新增信号工具 `crates/agent-core/src/tools/preview_act.rs`，与 `PreviewStyle` 同源机制：

```rust
pub const PREVIEW_ACT_TOOL_NAME: &str = "PreviewAct";

pub struct PreviewActInput {
    pub action: String,              // "click" | "type" | "scroll" | "hover" | "press"
    #[serde(default)]
    pub target: Option<String>,      // "@N" 操作哪个选中元素（click/type/hover 用），默认 @1
    #[serde(default)]
    pub text: Option<String>,        // action=type 时：要输入的文字
    #[serde(default)]
    pub key: Option<String>,         // action=press 时：按键名（Enter/Escape/ArrowDown…）
    #[serde(default)]
    pub delta: Option<i32>,          // action=scroll 时：滚动量（px，正=向下）
}
```

- `route_aside_event` 加 `name == "PreviewAct"` 分支 → 下发 `heb:aside:act`
- inspector `handleIn` 加 `heb:aside:act`，按 action 在目标元素上执行：
  - `click`：`el.click()`（合成完整 pointer 序列，触发框架事件监听）
  - `type`：聚焦后逐字 `dispatchEvent(InputEvent)` + 设 `value`，触发 React 受控组件 onChange
  - `hover`：`dispatchEvent(MouseEvent("mouseover"/"mouseenter"))`，触发 hover 菜单 / tooltip
  - `press`：`dispatchEvent(KeyboardEvent("keydown"/"keyup"))`
  - `scroll`：`window.scrollBy` 或目标元素 `scrollTop += delta`
  - 每个动作对话流显示一条 tool 气泡（🖱 点击 @1 / ⌨ 输入…），并把动作记进 `draft.structuralChanges[]`（提交时一并描述给主对话，说明"这个效果是在 X 交互后出现的"）
- **交互后新元素不自动选中**：交互后若 DOM 冒出新元素（弹窗 / 菜单），不自动选中——保持简单；助手要标注它再用 `PreviewMutate` 或让用户 ➕ 选取

**安全**：交互只作用于目标预览页（用户自己的页面），旁支会话 `restrict_tools` 白名单加 `PreviewAct`（与其他 preview 工具并列，绝不暴露 Bash/Edit）。

### 5.9 让助手「看见」（功能 F）—— 新增工具 PreviewCapture

新增信号工具 `crates/agent-core/src/tools/preview_capture.rs`。**与前面工具的关键不同：它要等截图回传才能返回工具结果**（前面的工具 `execute` 立即返回确认即可）。

```rust
pub const PREVIEW_CAPTURE_TOOL_NAME: &str = "PreviewCapture";

pub struct PreviewCaptureInput {
    #[serde(default)]
    pub target: Option<String>,      // "@N" 只截某个元素，缺省截整个可视区
}
```

**截图链路**（打破单向下发模式）：

1. 模型调 `PreviewCapture` → `route_aside_event` 观察到 → 下发 `heb:aside:capture`，**同时记一个 pending（capture_id → oneshot sender）**
2. inspector 收到 `heb:aside:capture`：html2canvas 渲染目标 → `canvas.toDataURL("image/png")` → 上行 `heb:capture:result {captureId, dataUrl}`
3. mod.rs 收到 `heb:capture:result` → 解出 base64 PNG → 通过 oneshot 唤醒 pending
4. `PreviewCapture` 工具的 `execute` 一直 await 这个 oneshot（带超时），拿到 PNG 后**作为工具结果的 Image 附件返回**：

```rust
// execute 返回带图的 ToolResult
Ok(ToolOutput {
    content: "已截取当前预览（近似渲染，颜色/阴影可能不精确）".into(),
    attachments: vec![MessageAttachment::image_from_bytes("preview.png", "image/png", &png_bytes)],
})
```

5. 旁支会话下一轮把这个 Image 附件喂回模型——目标模型支持图片就直接看；不支持，`VisionBridgeClient` 自动用视觉辅助模型转文字描述（既有管线，`tool_result_image_replaced_with_vision_notes` 已覆盖）

**html2canvas 引入**（用户已定：打包进 inspector）：把 html2canvas min 源码内联进 inspector 注入链路（或注入时附加 `<script>`），离线可用、任何页面可截。inspector 体积增 ~200KB，仅 Desktop 子 webview 注入一次。

**工具结果通道（已核实，零改造）**：`Tool` trait 已有 `execute_rich(ctx, input) -> AppResult<ToolOutput>`，`ToolOutput { content, attachments: Vec<MessageAttachment> }`，且 dispatcher 会把 attachments 透传进 `ToolResult.attachments` 再进模型上下文（`Read` 读图片就走这条）。`PreviewCapture` 只实现 `execute_rich` 返回带 Image 的 ToolOutput 即可——**不动 Tool trait、不影响任何现有工具**。

**超时与失败**：截图 10s 没回（页面卡 / html2canvas 异常）→ execute 返回文字「截图超时，无法查看当前渲染」，不挂死会话。

### 5.10 未提交防丢失（功能 G）

汇总浮窗 `annotationList` 非空（有未提交注释）时，拦截"页面要离开"——刷新 / 导航 / 点链接都会让草稿白丢。

**根因**：会丢草稿的入口分两类，拦法不同。关键是**避免双重弹窗**（先自定义框、navigate 又触发 beforeunload 原生框，用户连点两次）。两者各管不重叠的入口：

**① 工具栏入口（React 发起）→ 自定义中文美观弹窗**：
- 刷新按钮 / 后退 / 前进 / 地址栏回车这几个 React handler，调 `browser_reload` / `browser_navigate` 等命令**前**先查当前对话浏览器有没有未提交注释
- 有 → 弹自定义确认 dialog（中文、可写「你有 N 条注释没提交，刷新就没了，确定吗？」、配项目设计语言），用户确认才继续执行命令
- "未提交注释数"怎么拿到：inspector 在 `annotationList` 变化时上行 `heb:annotation:dirty {count}`，mod.rs emit `browser://annotation-dirty` → 前端按对话存一份 dirty count（类似现有 pickerActive 等状态）。React 弹窗读它，不必每次现问 inspector
- 用户确认后，React 给 inspector 下发 `heb:unload:allow`（置一个一次性放行标志），再执行 reload/navigate——这样接下来 navigate 触发的 beforeunload 看到放行标志直接跳过，**不二次弹**

**② 页面自身入口（React 不知情）→ beforeunload 兜底**：
- inspector 监听 `beforeunload`：`annotationList` 非空**且**没有一次性放行标志时，`e.preventDefault()` + `e.returnValue=""` → 浏览器原生确认框（文案不可定制，浏览器安全限制）
- 兜住页面里的 `location.reload()`、点 `<a>` 跳转、表单提交跳转——这些 React 完全不知情，只能靠页面内的 beforeunload
- 放行标志用完即清（一次性），避免误放后续真正的页面自发跳转

**为什么这样分**：自定义框体验好但只能拦 React 发起的入口；beforeunload 能拦页面自身但文案丑。各管各的入口 + 一次性放行标志去重，既覆盖全、又不双弹。「关对话 / 关 App」超出"刷新页面"范畴，本期不做（用户已确认"其他都可以"）。

## 6. 数据流时序

```
选元素 → ➕ 追加更多 → 小方块[1][2][3]
  ↓
对话框 @1 @2 「点开 @1 的菜单看看对齐对不对」→ heb:aside:send(elements) → 旁支会话
  ↓ 助手调 PreviewAct{action:click,target:@1} → heb:aside:act → inspector el.click()
  ↓ 助手调 PreviewCapture → heb:aside:capture → inspector html2canvas → heb:capture:result
  ↓ mod.rs oneshot 唤醒 → 工具结果带 PNG 附件 → 下一轮喂回模型（VisionBridge 自动降级）
  ↓ 助手「看见」效果 → 调 PreviewStyle{target:@2} / PreviewMutate 调整
  ↓ route_aside_event 观察 → heb:aside:apply / heb:aside:mutate → inspector 实时改预览
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
- **未提交防丢失去重**：自定义弹窗确认后下发的一次性放行标志必须用完即清，否则会误放后续真正的页面自发跳转
- **提交成功后队列清空**：「全部提交到主对话」成功 → `annotationList` 清空 → dirty count 归零 → beforeunload 自动解除

## 8. 测试策略

- **inspector.js 纯函数核心**（`__hebCore`，node 可测）：新增 `@N` 解析（contenteditable → locator 还原）、draft 序列化/反序列化（浮窗存取）、`target` 解析（@N → element index）写单测，落 `annotation.test.ts` 同级
- **preview_mutate.rs / preview_act.rs / preview_capture.rs**：`execute` / `execute_rich` 返回 + 参数解析单测（`cargo test -p agent-core --lib`）；capture 的 oneshot 超时路径单测
- **复现验证**（按 CLAUDE.md 修 bug 流程）：hebweb + Playwright 跑全链路——选多元素 → 小方块 hover 高亮 → @N 对话 → 助手 PreviewAct 点击触发弹窗 → PreviewCapture 截图回传 → append → 加入列表 → 展开 → 全部提交，DOM/截图核对
- **未提交防丢失**：队列非空时点工具栏刷新 → 断言弹自定义中文框；确认后只刷一次（不双弹）；队列空时刷新无拦截；提交成功后 dirty 归零
- **回归**：`PreviewStyle` 无 target 时默认主元素（向后兼容旧旁支会话）

## 9. 架构.md 同步（实施时必做）

按 CLAUDE.md「引入新工具必须先更新架构.md」：

- §8.5 第 2 条补充「注释支持多元素 + 结构草稿操作 + 浏览器交互 + 截图视觉回传」
- §4.4 工具表 / BUILTIN_TOOL_NAMES 说明区登记 `PreviewMutate` / `PreviewAct` / `PreviewCapture`（与 `PreviewStyle` 同为旁支会话专属信号工具，不进通用工具集）
- §13 决策表追加两行：① 预览结构操作 / 交互 = 草稿态 + 提交时由主对话落地源码（不直接写源码）；② 截图用 DOM 渲染（html2canvas 打包进 inspector）而非 OS 屏幕截图，取舍 = 避开 DPI/遮挡黑洞，代价 = 复杂 CSS 近似
- changelog 追加一条

## 10. 实施顺序建议

1. agent-core：`PreviewStyle` 加 target + 新增 `preview_mutate.rs`（最底层，可独立单测）
2. inspector.js：多元素状态模型 + 小方块（功能 A 骨架）
3. inspector.js：@N contenteditable 输入框（功能 B）
4. mod.rs + inspector.js：旁支多元素绑定 + PreviewMutate 下发执行（功能 ①③）
5. agent-core + mod.rs + inspector.js：PreviewAct 交互动作集（功能 E）
6. agent-core + mod.rs + inspector.js：PreviewCapture + html2canvas 截图回传链路（功能 F，最复杂——含 oneshot 等待 + Image 工具结果）
7. inspector.js + mod.rs + App.tsx：统一浮窗 + 合并总结（功能 D）
8. inspector.js + 工具栏 React + mod.rs：未提交防丢失（功能 G——dirty 上行 + 自定义弹窗 + beforeunload 兜底 + 一次性放行去重）
9. 架构.md + changelog + 端到端复现验证
