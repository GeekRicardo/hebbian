# 内置浏览器多元素注释 + 交互/视觉 + 防丢失 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把内置浏览器注释从「单元素」升级为「多元素 + @N 引用 + 真实结构操作 + 浏览器交互 + 截图视觉回传 + 统一汇总浮窗 + 未提交防丢失」。

**Architecture:** 严格落在架构 §8.5「注释 = 普通 user message + 信号工具实时回灌预览」框架内。protocol / agent-core 主路径 / prompt cache 零改动。新增 3 个旁支会话专属信号工具（`PreviewMutate` / `PreviewAct` / `PreviewCapture`），`PreviewStyle` 加 `target` 参数；inspector.js 重构成多元素 draft 状态模型；统一汇总浮窗替代旧「修改队列」；截图复用既有 `ToolOutput.attachments` + `VisionBridgeClient` 管线（零新建）。

**Tech Stack:** Rust（agent-core Tool trait / Tauri command / tauri webview）、vanilla JS（inspector.js 注入脚本，含 html2canvas）、React + TS（BrowserPanel 工具栏 / App.tsx 消费端）。

**设计依据：** [docs/superpowers/specs/2026-06-12-multi-element-annotation-design.md](../specs/2026-06-12-multi-element-annotation-design.md)

---

## 关键约束（每个 task 都要守）

- **命名规范（架构 §4.4.7）**：工具名 PascalCase（`PreviewMutate`）；工具参数 camelCase（JSON schema）/ Rust 内部 snake_case；`browser://` 事件 + `heb:*` 消息沿用现有风格。
- **信号工具机制**：agent-core 工具 `execute` 只返回确认句（不碰 webview）；真正改页面由 `mod.rs` 的 `route_aside_event` 观察工具调用后下发 inspector。`PreviewCapture` 例外——`execute_rich` 要 await 截图回传。
- **旁支会话工具白名单**：`aside_send_args` 的 `restrict_tools` 必须把新工具列进去，**绝不让旁支会话拿到 Bash/Edit**（危险 + hitl=None 会挂死）。
- **UI 文案纪律（CLAUDE.md 步骤 3.1）**：所有给用户看的字是人话，不暴露 `@N` 以外的内部命名 / 路径 / 字段名。
- **代码注释纪律**：注释只写「这是什么 + 为什么必须这样」，不出现外部项目名 / 历史对比（那些写 changelog）。
- **每个 task 完成后 commit**；只 `git add` 本 task 实际改的文件（工作区有他处未完成改动，绝不 `git add -A`）。
- **验证命令**：Rust `cargo check -p agent-core --tests` + `cargo test -p agent-core --lib`；TS `pnpm exec tsc --noEmit`；inspector 纯函数 `node` 单测。

## 文件结构总览

| 文件 | 职责 | 本计划改动 |
|---|---|---|
| `crates/agent-core/src/tools/preview_style.rs` | PreviewStyle 信号工具 | 加 `target` 参数（Task 1） |
| `crates/agent-core/src/tools/preview_mutate.rs` | 结构操作信号工具（新建） | Task 2 |
| `crates/agent-core/src/tools/preview_act.rs` | 交互信号工具（新建） | Task 8 |
| `crates/agent-core/src/tools/mod.rs` | 工具注册 | 注册 2 个新工具（Task 2/8） |
| `apps/desktop/src/browser/mod.rs` | Tauri 后端：旁支会话、上行分发、合并总结、dirty/防丢失 | Task 6/9/11/12 |
| `apps/desktop/src/browser/inspector.js` | 注入页面：多元素状态、小方块、@N、结构/交互执行、浮窗、防丢失 | Task 3/4/5/7/9/10/12 |
| `apps/desktop/frontend/src/desktop/ui/components/BrowserPanel.tsx` | 工具栏 React | Task 12（防丢失拦截） |
| `apps/desktop/frontend/src/desktop/ui/lib/browserHost.ts` | 承载层契约 | Task 11/12 |
| `apps/desktop/frontend/src/desktop/ui/lib/annotation.ts` | 消息组装 | Task 11 |
| `apps/desktop/frontend/src/App.tsx` | 注释 → 主对话消费端 | Task 11 |
| `apps/desktop/frontend/src/desktop/ui/lib/annotation.test.ts` | inspector 纯函数单测 | Task 3 |

> **功能 F（PreviewCapture 截图视觉回传）推迟单独立项**（2026-06-12 决策）：截图回传跨 agent-core / Desktop 边界，`ToolCtx` 无 app handle，需先给 agent-core 加截图通道抽象——在交互链路（E）跑通前硬上风险高。本期不含 `preview_capture.rs` / `html2canvas.min.js`。

---

## 阶段一：agent-core 工具层（PreviewStyle 加 target + PreviewMutate）

最底层、可独立单测，不依赖 webview。

### Task 1: PreviewStyle 加 `target` 参数

**Files:**
- Modify: `crates/agent-core/src/tools/preview_style.rs`

- [ ] **Step 1: 写失败测试**

在 `preview_style.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_target_and_returns_ack() {
        let tool = PreviewStyleTool;
        let out = tool
            .execute(serde_json::json!({
                "prop": "color",
                "value": "#fff",
                "target": "@2"
            }))
            .await
            .unwrap();
        assert!(out.contains("color"), "确认句应含属性名，实际: {out}");
    }

    #[tokio::test]
    async fn target_is_optional() {
        let tool = PreviewStyleTool;
        let out = tool
            .execute(serde_json::json!({ "prop": "color", "value": "#fff" }))
            .await
            .unwrap();
        assert!(out.contains("#fff"));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p agent-core --lib preview_style`
Expected: FAIL（`PreviewStyleInput` 无 `target` 字段，serde 默认 `deny_unknown_fields` 行为或编译期不识别 → 测试编译/运行失败）

- [ ] **Step 3: 给 `PreviewStyleInput` 加 `target` 字段 + schema**

把 `PreviewStyleInput` 改为：

```rust
#[derive(Debug, Deserialize)]
pub struct PreviewStyleInput {
    /// CSS 属性名，如 `border-radius` / `color` / `font-size`。
    pub prop: String,
    /// CSS 值，如 `12px` / `#1f2328` / `600`。
    pub value: String,
    /// 改哪个选中元素（`@2`）；缺省主元素 `@1`。多元素注释框里指定目标用。
    #[serde(default)]
    pub target: Option<String>,
}
```

`parameters_schema` 的 `properties` 加一项（`required` 不加 target）：

```rust
"target": { "type": "string", "description": "Which selected element to style, like @2. Defaults to the primary element @1." }
```

`description` 末尾补一句（让模型知道多元素能力）：

```rust
"... Both `prop` and `value` are required. In a multi-element annotation, pass `target` like @2 to style a specific selected element; defaults to @1."
```

`execute` 体保持不变（仍只返回确认句，`target` 由 Desktop 侧 `route_aside_event` 透传下发，工具本身不需要用它）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p agent-core --lib preview_style`
Expected: PASS（2 个测试）

- [ ] **Step 5: Commit**

```bash
git add crates/agent-core/src/tools/preview_style.rs
git commit -m "PreviewStyle 加 target 参数：多元素注释框里指定改哪个选中元素

- Why: 多元素注释框升级，助手要能对 @N 任一选中元素调样式
- target 可选，缺省主元素；execute 仍只返确认，实际下发由 Desktop route_aside_event 透传
- 影响范围: agent-core 工具 schema additive，旧旁支会话不传 target 行为不变"
```

### Task 2: 新建 PreviewMutate 工具（结构操作信号）

**Files:**
- Create: `crates/agent-core/src/tools/preview_mutate.rs`
- Modify: `crates/agent-core/src/tools/mod.rs:12`（加 `pub mod`）、`:247`（注册）

- [ ] **Step 1: 写工具 + 失败测试**

创建 `crates/agent-core/src/tools/preview_mutate.rs`：

```rust
//! PreviewMutate：内置浏览器「元素对话」旁支会话专用的结构操作信号工具（架构 §8.5）。
//!
//! 与 PreviewStyle 同源：agent-core 不碰 webview，execute 只返回确认；真正在预览页
//! 新增/删除/改文本由 Desktop 观察事件流里的本工具调用、下发 inspector 执行。
//! 预览改动是草稿态（刷新即失），最终由「提交到主对话」让主对话改源码落地。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use common::{AppError, AppResult};

use crate::tools::Tool;

pub const PREVIEW_MUTATE_TOOL_NAME: &str = "PreviewMutate";

#[derive(Debug, Deserialize)]
pub struct PreviewMutateInput {
    /// 操作类型：`append`（在目标内追加）/ `remove`（删目标）/ `setText`（改目标文本）。
    pub op: String,
    /// 操作哪个选中元素（`@2`）；缺省主元素 `@1`。
    #[serde(default)]
    pub target: Option<String>,
    /// `op=append` 时：要追加的 HTML 片段。
    #[serde(default)]
    pub html: Option<String>,
    /// `op=setText` 时：新文本内容。
    #[serde(default)]
    pub text: Option<String>,
}

pub struct PreviewMutateTool;

#[async_trait]
impl Tool for PreviewMutateTool {
    fn name(&self) -> &str {
        PREVIEW_MUTATE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Mutate the DOM structure of a selected web element live in the page preview. \
         op=append adds an HTML fragment inside the target; op=remove deletes the target; \
         op=setText replaces the target's text. target is @N (defaults to @1). \
         This is a DRAFT in the preview only (lost on reload) — the user will later submit \
         it so the main conversation edits the real source. Keep appended `html` semantically \
         clean so it maps back to JSX easily."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["op"],
            "properties": {
                "op": { "type": "string", "enum": ["append", "remove", "setText"], "description": "append | remove | setText" },
                "target": { "type": "string", "description": "Which selected element, like @2. Defaults to @1." },
                "html": { "type": "string", "description": "HTML fragment to append (op=append)" },
                "text": { "type": "string", "description": "New text content (op=setText)" }
            }
        })
    }

    async fn execute(&self, input: Value) -> AppResult<String> {
        let parsed: PreviewMutateInput = serde_json::from_value(input)
            .map_err(|e| AppError::msg(format!("invalid PreviewMutate input: {e}")))?;
        let target = parsed.target.as_deref().unwrap_or("@1");
        Ok(format!("已对预览元素 {target} 执行结构操作：{}", parsed.op))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_returns_ack() {
        let out = PreviewMutateTool
            .execute(serde_json::json!({ "op": "append", "target": "@1", "html": "<button>x</button>" }))
            .await
            .unwrap();
        assert!(out.contains("append"));
        assert!(out.contains("@1"));
    }

    #[tokio::test]
    async fn target_defaults_to_primary() {
        let out = PreviewMutateTool
            .execute(serde_json::json!({ "op": "remove" }))
            .await
            .unwrap();
        assert!(out.contains("@1"), "缺省 target 应为 @1，实际: {out}");
    }

    #[tokio::test]
    async fn invalid_input_errors() {
        let r = PreviewMutateTool.execute(serde_json::json!({ "foo": "bar" })).await;
        assert!(r.is_err(), "缺 op 应报错");
    }
}
```

- [ ] **Step 2: 注册工具到 mod.rs**

`crates/agent-core/src/tools/mod.rs:12` 附近（`pub mod preview_style;` 同处）加：

```rust
pub mod preview_mutate;
```

`mod.rs:247`（`Box::new(preview_style::PreviewStyleTool),` 那行后）加：

```rust
        Box::new(preview_mutate::PreviewMutateTool),
```

注意：**不要**加进 `BUILTIN_TOOL_NAMES`（与 PreviewStyle 一样，旁支会话专属，普通会话看不到）。

- [ ] **Step 3: 跑测试确认通过**

Run: `cargo test -p agent-core --lib preview_mutate`
Expected: PASS（3 个测试）

- [ ] **Step 4: 编译全量确认无破坏**

Run: `cargo check -p agent-core --tests`
Expected: 通过

- [ ] **Step 5: Commit**

```bash
git add crates/agent-core/src/tools/preview_mutate.rs crates/agent-core/src/tools/mod.rs
git commit -m "新增 PreviewMutate 工具：旁支会话在预览里真实新增/删除/改文本

- Why: 助手原来只能改样式不能改结构，新增按钮/删冗余元素/改文案预览看不到
- 信号工具范式（同 PreviewStyle）：execute 只返确认，实际 DOM 操作由 Desktop 下发 inspector
- 草稿态，最终由提交到主对话改源码落地；不进 BUILTIN_TOOL_NAMES，旁支会话专属
- 影响范围: agent-core 工具注册 additive"
```

---

## 阶段二：inspector.js 多元素状态模型 + 小方块（功能 A）

> 集成层。inspector.js 是注入页面的 vanilla JS。可纯单测的部分（@N 解析、draft 序列化）走 `__hebCore` + node 单测；DOM 渲染部分给锚点 + hebweb 复现验证。

### Task 3: draft 状态模型 + @N 纯函数（TDD）

**Files:**
- Modify: `apps/desktop/src/browser/inspector.js`（`__hebCore` 区 + 状态变量区）
- Test: `apps/desktop/frontend/src/desktop/ui/lib/annotation.test.ts`（node 测 `__hebCore`）

**契约：** 把现状游离的 `selectedTarget` / `cardSnapshot` / `styleDiff` 收拢成一个 `draft` 对象（spec §5.1）：

```
draft = {
  elements: [ { key, el, snapshot, styleDiff } ],  // 选中元素，按顺序
  activeIndex: 0,
  asideSession: null,
  asideMessages: [],
  structuralChanges: [],
}
```

`@N` 是 1-based：`@1` → `elements[0]`。

- [ ] **Step 1: 在 `__hebCore` 加两个纯函数 + 失败测试**

`inspector.js` 的 `__hebCore` 对象加两个纯函数（在 `parseInMsg` 附近）：

```javascript
// "@2" -> 1（0-based index）；非法返回 -1。注释框 @N 引用解析用。
function refToIndex(ref) {
  var m = /^@(\d+)$/.exec(String(ref || "").trim());
  if (!m) return -1;
  var n = parseInt(m[1], 10);
  return n >= 1 ? n - 1 : -1;
}

// contenteditable 输入框的子节点序列 → 发送给助手的纯文本。
// nodes: [{type:"text",value} | {type:"ref",ref:"@2",locator}]。
// chip 还原成「元素2: <locator>」让助手拿到元素定位。
function composeAsideText(nodes) {
  var out = "";
  for (var i = 0; i < nodes.length; i++) {
    var n = nodes[i];
    if (n.type === "ref") {
      var idx = refToIndex(n.ref);
      out += "「元素" + (idx + 1) + (n.locator ? ": " + n.locator : "") + "」";
    } else {
      out += n.value || "";
    }
  }
  return out;
}
```

挂进 `__hebCore`（`buildXPath: buildXPath,` 那块对象字面量里加）：

```javascript
    refToIndex: refToIndex,
    composeAsideText: composeAsideText,
```

`annotation.test.ts` 加（文件已 import inspector 的 `__hebCore`，参考现有测试写法）：

```typescript
import { describe, it, expect } from "vitest";
// 现有文件已有 require inspector 的方式，沿用它取 core
const core = require("../../../../../../src/browser/inspector.js");

describe("inspector __hebCore @N", () => {
  it("refToIndex 解析 @N 为 0-based", () => {
    expect(core.refToIndex("@1")).toBe(0);
    expect(core.refToIndex("@3")).toBe(2);
    expect(core.refToIndex("@0")).toBe(-1);
    expect(core.refToIndex("x")).toBe(-1);
  });
  it("composeAsideText 把 chip 还原成元素定位", () => {
    const txt = core.composeAsideText([
      { type: "text", value: "让 " },
      { type: "ref", ref: "@1", locator: "button.btn" },
      { type: "text", value: " 和 " },
      { type: "ref", ref: "@2", locator: "div.card" },
      { type: "text", value: " 对齐" },
    ]);
    expect(txt).toBe("让 「元素1: button.btn」 和 「元素2: div.card」 对齐");
  });
});
```

> 注：`annotation.test.ts` 现有的 inspector 引入路径以文件里已有的为准（若已 `require`，复用同一行）。先看文件顶部现有 import 再决定 `core` 怎么取。

- [ ] **Step 2: 跑测试确认失败**

Run: `cd apps/desktop/frontend && pnpm exec vitest run annotation.test.ts`
Expected: FAIL（`refToIndex is not a function`）

- [ ] **Step 3: 已在 Step 1 实现，跑测试确认通过**

Run: `cd apps/desktop/frontend && pnpm exec vitest run annotation.test.ts`
Expected: PASS

- [ ] **Step 4: 引入 draft 状态对象（DOM 层，无单测，给锚点）**

在 inspector.js 注释卡片区（`var cardEl = null; var cardSnapshot = null;` 附近，spec §5.1）：
- 新增 `var draft = null;`（打开注释框时初始化）
- `elementKeyOf` 复用现状
- 保留 `cardSnapshot` 作为 `draft.elements[draft.activeIndex].snapshot` 的别名读取点，减少改动面（`currentTarget()` / `styleSet` 等仍能工作）
- `styleDiff` 全局改为读写 `draft.elements[draft.activeIndex].styleDiff`

- [ ] **Step 5: 验证编译 + 既有功能不回归**

Run: `pnpm exec tsc --noEmit`（inspector.js 是 JS 不参与 tsc，但确保 test 文件类型通过）
Run: `cd apps/desktop/frontend && pnpm exec vitest run annotation.test.ts`
Expected: 全 PASS

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/browser/inspector.js apps/desktop/frontend/src/desktop/ui/lib/annotation.test.ts
git commit -m "inspector: 引入多元素 draft 状态模型 + @N 纯函数(TDD)

- Why: 注释框从绑单元素升级多元素，@N 引用 + per-element styleDiff 的地基
- refToIndex/composeAsideText 进 __hebCore 走 node 单测；draft 对象收拢游离全局态
- 影响范围: inspector 内部状态重构，对外消息协议未变"
```

### Task 4: 小方块 chip 行 + ➕ 追加选取（DOM 层）

**Files:**
- Modify: `apps/desktop/src/browser/inspector.js`（`showAnnotationCard` 头部区 + picker 状态机）

**锚点与契约（spec §5.2）：**

- [ ] **Step 1: 头部加 ➕ 按钮**

`showAnnotationCard` 的 `head` 区（`badge` 与 `closeBtn` 之间）加一个 ➕ 图标按钮，点击 → 设 `pickerMode = "append"` 后 `startPicker()`。新增模块级 `var pickerMode = "new";`。

- [ ] **Step 2: 标题下方渲染小方块行 `renderChips()`**

新增函数 `renderChips(container)`：遍历 `draft.elements`，每个渲染一个圆角小方块（序号居中）：
- 激活态（`i === draft.activeIndex`）填充 `#2f81f7` 白字，其余 `#f1f3f5` 描边
- `mouseenter` → `positionOverlay(selectedOverlay, draft.elements[i].el)`（复用现有 overlay）；`mouseleave` → 恢复高亮 activeIndex 那个
- `click` → 设 `draft.activeIndex = i`，重渲染样式编辑器区（盒模型/CARD_FIELDS/全部 CSS 切到 `draft.elements[i].el`）
- 每个方块右上角 `×`：`draft.elements.length > 1` 时才显示，点击移除该元素后 `renderChips` 重排
- 小方块行插入到 `head` 之后、`styleCard` 之前

- [ ] **Step 3: picker 追加模式接线**

`onClick`（picker 状态机）末尾：`pickerMode === "append"` 且已有打开的 draft 时，把新选中元素 `push` 进 `draft.elements`（构造 `{key, el, snapshot: collectSnapshot(el), styleDiff:{}}`），`renderChips` 刷新，**不** `showAnnotationCard`（不新建框）；`pickerMode` 复位 `"new"`。

- [ ] **Step 4: hebweb 复现验证**

```bash
cargo build -p hebbian-web-server
cd apps/desktop/frontend && pnpm build && cd -
./target/debug/hebweb --port 38080 &
# Playwright: 打开页面 → 选元素 → 点 ➕ → 再选一个 → 断言出现 [1][2] 两个小方块
# hover [2] → 断言页面高亮切到第二个元素；点 [1] → 样式编辑器切回第一个
```
Expected: 小方块正确渲染、hover 高亮、点击切换、➕ 追加生效

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/browser/inspector.js
git commit -m "inspector: 注释框头部小方块[1][2][3] + ➕ 追加选取(功能 A)

- Why: 一个注释框选中多元素，可视化切换 + hover 高亮 + 追加/移除
- renderChips 复用现有 overlay 做 hover 高亮；➕ 进 append picker 模式追加不新建框
- 影响范围: inspector DOM 层，hebweb 已验证多元素选取链路"
```

---

## 阶段三：@N contenteditable 输入框（功能 B）

### Task 5: contenteditable 输入框 + chip + @ 弹层

**Files:**
- Modify: `apps/desktop/src/browser/inspector.js`（`showAnnotationCard` 的 chatInput 区 + sendChat）

**锚点与契约（spec §5.3）：** 把 `chatInput` 从 `<textarea>` 换成 `contenteditable` div。

- [ ] **Step 1: chatInput 改 contenteditable**

`chatInput` 改为 `document.createElement("div")` + `contentEditable = "true"`，保留 placeholder（用 `:empty:before` 或 data 属性模拟）、样式对齐原 textarea。

- [ ] **Step 2: @ 触发选择弹层**

监听 `input` 事件：检测光标前刚输入 `@` → 在光标位置弹小浮层列出 `draft.elements` 的 `1/2/3 + badge`，方向键/点击选择 → 在光标处插入 chip 节点：

```javascript
// chip = 不可编辑 span，蓝底白字
var chip = document.createElement("span");
chip.setAttribute("data-heb-ref", "@" + (i + 1));
chip.contentEditable = "false";
chip.textContent = "@" + (i + 1);
chip.style.cssText = "display:inline-block;background:#2f81f7;color:#fff;border-radius:4px;padding:0 5px;margin:0 1px;font-size:11px;";
chip.addEventListener("mouseenter", function () { positionOverlay(selectedOverlay, draft.elements[i].el); });
```

直接键入 `@2` 文本：失焦 / 输入空格时扫描文本节点把 `@\d+` 转 chip。

- [ ] **Step 3: IME 合成防误触**

`compositionstart` 置 `composing=true`、`compositionend` 置 `false`；`input` handler 里 `composing` 为真时不触发 @ 弹层（中文输入「@」不误判）。

- [ ] **Step 4: sendChat 用 composeAsideText 还原**

`sendChat` 改为：遍历 `chatInput.childNodes`，文本节点 → `{type:"text",value}`、chip 节点 → `{type:"ref", ref: chip 的 data-heb-ref, locator: elementLocator(draft.elements[idx].snapshot)}`，调 `__hebCore.composeAsideText(nodes)` 得最终文本。元素 detach / 已移除时 locator 拼「(已移除)」。`heb:aside:send` 载荷的 `element` 字段改为带全部选中元素定位（见 Task 8 后端配合）。

- [ ] **Step 5: hebweb 复现验证**

```bash
# Playwright: 选两元素 → 输入框打 "@" → 断言弹层列出 1/2 → 选 @1 → 断言出现蓝色 chip
# 打中文 "颜色@2" → 断言 @2 成 chip 不误触；hover chip → 页面高亮对应元素
# 发送 → 看 model_io.jsonl 里 user content 含「元素2: ...locator」
```
Expected: chip 渲染、@ 弹层、IME 不误触、发送还原定位

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/browser/inspector.js
git commit -m "inspector: 对话输入框 @N 引用(contenteditable chip + @弹层)(功能 B)

- Why: 对话里 @1 @2 引用选中元素，助手据此做元素之间的修改
- chip 不可编辑蓝块 + hover 高亮；IME 合成期不触发 @ 弹层避免中文误判
- sendChat 用 __hebCore.composeAsideText 把 chip 还原成元素定位喂助手
- 影响范围: inspector DOM 层 + 上行 element 载荷扩展"
```

---

## 阶段四：旁支会话多元素绑定 + PreviewMutate 下发（功能 ①③）

### Task 6: 旁支会话绑定多元素 + PreviewStyle target 透传

**Files:**
- Modify: `apps/desktop/src/browser/mod.rs`（`aside_system_prompt` / `handle_aside_send` / `route_aside_event`）

**契约（spec §5.4）：** 元素定位从「创建会话时固化 system prompt」改为「每轮 user content 前缀」，保 prompt cache + 支持追加元素。

- [ ] **Step 1: 改 `aside_system_prompt` 为通用规则**

`aside_system_prompt`（mod.rs:878）改为不接收具体 element，只讲通用规则：

```rust
fn aside_system_prompt() -> String {
    "你是内置浏览器的预览样式/结构助手。用户会用 @1 @2 等标号引用页面上选中的元素。\
     你可以：用 PreviewStyle 调任一元素样式（target 传 @N）、用 PreviewMutate 改结构、\
     用 PreviewAct 操作页面、用 PreviewCapture 截图查看效果。每轮消息开头会给出当前所有\
     选中元素的定位。改动是预览草稿，用户满意后会提交到主对话改源码落地。".to_string()
}
```

调用处（`create` 会话那行）改为 `aside_system_prompt()`。

- [ ] **Step 2: `handle_aside_send` 每轮拼元素定位前缀**

`heb:aside:send` 载荷新增 `elements`（数组：`[{ref, locator}]`）。`handle_aside_send` 取出后，把 user `text` 前缀拼上：

```rust
// 每轮带当前全部选中元素定位（追加元素后下一轮助手立刻可见）
let elements_block = /* 从 payload.elements 拼成 "当前选中：\n@1: ...\n@2: ..." */;
let user_content = if elements_block.is_empty() { text } else { format!("{elements_block}\n\n{text}") };
```

（兼容：旧载荷仍传 `element` 单字符串时回退原行为。）

- [ ] **Step 3: `route_aside_event` 透传 PreviewStyle 的 target**

`route_aside_event`（mod.rs:931，`name == "PreviewStyle"` 分支）取 `input.get("target")`，连同 prop/value 下发：

```rust
let target = input.get("target").and_then(|v| v.as_str()).unwrap_or("@1").to_string();
eval_aside_down(app, host_session, surface, "heb:aside:apply",
    serde_json::json!({ "sessionId": aside_session, "prop": prop, "value": value, "target": target }));
```

- [ ] **Step 4: 编译验证**

Run: `cargo check -p hebbian-desktop`（或 workspace）
Expected: 通过

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/browser/mod.rs
git commit -m "browser: 旁支会话绑定多元素 + PreviewStyle target 透传(功能①)

- Why: 助手要能对 @N 任一选中元素调样式；追加元素后下一轮要立刻可见
- 元素定位从 system prompt 固化改为每轮 user content 前缀（保 prompt cache + 支持追加）
- route_aside_event 把 PreviewStyle.target 透传给 inspector
- 影响范围: mod.rs 旁支会话链路，向后兼容旧单元素载荷"
```

### Task 7: inspector 按 target 应用 + PreviewMutate 执行

**Files:**
- Modify: `apps/desktop/src/browser/inspector.js`（`handleIn` 的 `heb:aside:apply` + 新增 `heb:aside:mutate`）

**契约（spec §5.4 / §5.5）：**

- [ ] **Step 1: `heb:aside:apply` 按 target 应用**

`handleIn` 的 `case "heb:aside:apply"`：解析 `msg.payload.target`（`__hebCore.refToIndex`），应用到 `draft.elements[idx].el`（而非全局 currentTarget）；越界回退 activeIndex。tool 气泡显示 `🎨 @N prop → value`。

- [ ] **Step 2: 新增 `heb:aside:mutate` 分支**

`handleIn` 加 `case "heb:aside:mutate"`，按 `op`：
- `append`：`el.insertAdjacentHTML("beforeend", html)`，try/catch 失败 → tool 气泡「⚠️ 新增失败：HTML 不合法」；成功 → 把新元素 push 进 `draft.elements`、`renderChips`、记 `draft.structuralChanges.push({op:"append", target, html})`、tool 气泡「🔧 在 @N 新增元素」
- `remove`：`el.remove()`，从 `draft.elements` 删对应项、`renderChips`、记 structuralChange、tool 气泡「🔧 删除 @N」
- `setText`：`el.textContent = text`，记 structuralChange、tool 气泡「🔧 改 @N 文本」

- [ ] **Step 3: hebweb 复现验证**

```bash
# Playwright: 选一元素 → 对话「给这个加个按钮」→ 等助手调 PreviewMutate
# 看 model_io.jsonl 有 PreviewMutate 调用；DOM 断言新按钮出现 + 多个小方块
# 选两元素 → 「把 @2 背景改蓝」→ 断言第二个元素背景变蓝（不是第一个）
```
Expected: target 正确路由、结构操作真实生效

- [ ] **Step 4: Commit**

```bash
git add apps/desktop/src/browser/inspector.js
git commit -m "inspector: 按 target 应用样式 + PreviewMutate 结构操作执行(功能①③)

- Why: 助手改 @N 指定元素样式 / 新增删除改文本，预览真实生效
- heb:aside:apply 按 refToIndex 路由到目标元素；heb:aside:mutate 三种 op 执行 + 记草稿
- append 新元素自动纳入小方块可继续 @它；失败 try/catch 静默提示
- 影响范围: inspector DOM 层"
```

---

## 阶段五：浏览器交互（功能 E，PreviewAct）

### Task 8: 新建 PreviewAct 工具 + 注册

**Files:**
- Create: `crates/agent-core/src/tools/preview_act.rs`
- Modify: `crates/agent-core/src/tools/mod.rs`（`pub mod` + 注册）

**契约（spec §5.8）：** 信号工具，参数 `action`（click/type/scroll/hover/press）+ target/text/key/delta。结构同 Task 2 的 PreviewMutate（参考它写，含完整 `execute` + 3 个单测：action 解析、target 缺省 @1、非法报错）。

- [ ] **Step 1: 写工具文件**（结构、Tool impl、tests 仿 Task 2 PreviewMutate，把 op→action、html/text→text/key/delta；description 说明「触发交互态：弹窗/hover 菜单/表单」）
- [ ] **Step 2: 注册到 mod.rs**（`pub mod preview_act;` + `Box::new(preview_act::PreviewActTool),`，不进 BUILTIN_TOOL_NAMES）
- [ ] **Step 3: 跑测试** `cargo test -p agent-core --lib preview_act` → PASS
- [ ] **Step 4: 编译** `cargo check -p agent-core --tests` → 通过
- [ ] **Step 5: Commit**

```bash
git add crates/agent-core/src/tools/preview_act.rs crates/agent-core/src/tools/mod.rs
git commit -m "新增 PreviewAct 工具：旁支会话操作预览页面(点击/输入/滚动/hover/按键)

- Why: 前端不全死样式，弹窗/hover菜单/表单交互态光调 CSS 测不出，要能操作触发
- 信号工具范式，execute 只返确认；不进 BUILTIN_TOOL_NAMES，旁支会话专属
- 影响范围: agent-core 工具注册 additive"
```

### Task 9: PreviewAct 下发 + inspector 执行

**Files:**
- Modify: `apps/desktop/src/browser/mod.rs`（`route_aside_event` 加 `PreviewAct` 分支）
- Modify: `apps/desktop/src/browser/inspector.js`（`handleIn` 加 `heb:aside:act`）
- Modify: `apps/desktop/src/browser/mod.rs`（`aside_send_args` 的 `restrict_tools` 白名单加 `PreviewMutate`/`PreviewAct`）

- [ ] **Step 1: restrict_tools 白名单扩容**

`aside_send_args`（mod.rs:968）的 `restrict_tools` 从 `vec!["PreviewStyle"]` 扩为 `vec!["PreviewStyle", "PreviewMutate", "PreviewAct"]`；`enabled_tools` 同步（`handle_aside_send` 传的 `vec!["PreviewStyle".to_string()]` 改为含这 3 个）。（PreviewCapture 推迟单独立项，本期不含。）

- [ ] **Step 2: route_aside_event 加 PreviewAct 下发**

仿 PreviewStyle 分支，`name == "PreviewAct"` → 下发 `heb:aside:act`，透传 action/target/text/key/delta。

- [ ] **Step 3: inspector heb:aside:act 执行**

`handleIn` 加 `case "heb:aside:act"`，按 action 在 `draft.elements[refToIndex(target)].el` 上执行（spec §5.8）：
- `click`：`el.click()`
- `type`：`el.focus()` + 设 value + `dispatchEvent(new Event("input",{bubbles:true}))`（触发 React 受控）
- `hover`：`dispatchEvent(new MouseEvent("mouseover",{bubbles:true}))` + mouseenter
- `press`：`dispatchEvent(new KeyboardEvent("keydown",{key, bubbles:true}))` + keyup
- `scroll`：`window.scrollBy(0, delta)` 或 `el.scrollTop += delta`
- 每个动作 tool 气泡（🖱/⌨）+ 记 `draft.structuralChanges`

- [ ] **Step 4: hebweb 复现验证**

```bash
# Playwright: 页面有个点击展开的菜单 → 选触发按钮 → 对话「点开看看」
# 等助手调 PreviewAct{click} → DOM 断言菜单展开
```
Expected: 交互真实触发

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/browser/mod.rs apps/desktop/src/browser/inspector.js
git commit -m "browser: PreviewAct 下发 + inspector 执行交互(功能 E)

- Why: 助手操作预览页触发弹窗/菜单/表单交互态
- restrict_tools 白名单纳入 3 个 preview 工具(绝不暴露 Bash/Edit)；act 按 target 路由
- 影响范围: mod.rs 旁支链路 + inspector DOM 层"
```

---

## 阶段六：统一汇总浮窗（功能 D）

### Task 10: 重做浮窗为注释列表 + 加入列表 + 单条直提

**Files:**
- Modify: `apps/desktop/src/browser/inspector.js`（`editQueue`/`renderQueuePanel` → `annotationList`/`renderAnnotationPanel`，注释框底部按钮）

**契约（spec §5.6）：** 一个注释项 = draft 快照 `{ elements:[{snapshot, styleDiff}], asideMessages, structuralChanges }`。

- [ ] **Step 1: 重命名 + 扩展数据结构**

`editQueue` → `annotationList`（项含完整 draft 快照，不只 styleDiff）；`renderQueuePanel` → `renderAnnotationPanel`，标题「注释列表 (N)」。

- [ ] **Step 2: 注释框底部两个出口**

`showAnnotationCard` 底部（现 `submitMain` 区）改为两个按钮：
- **「加入列表」**：当前 draft（样式 diff / 对话 / 结构改动任意非空）→ `annotationList.push(snapshotOfDraft())`、`removeCard()`、`renderAnnotationPanel()`、上行 `heb:annotation:dirty {count: annotationList.length}`（Task 14 用）
- **「提交到主对话」**：保留现有单条直提（走 `heb:aside:submit` 或现有 annotation 提交）

- [ ] **Step 3: 浮窗每项可展开 + 移除**

`renderAnnotationPanel` 每项：元素小方块组 + 摘要（有对话取对话末条要点／纯样式「调了 N 项样式」／结构「新增/删除 N 处」）；点击项 → `showAnnotationCard` 用该项 draft 快照重建（恢复 elements 选中态 + 对话 + 高亮）；每项 `×` 移除后 `renderAnnotationPanel` + 上行 dirty。

- [ ] **Step 4: 底部「全部提交到主对话」**

底部按钮 → 上行 `heb:annotation:submit-all {items: annotationList}`（Task 12 后端合并总结）；成功后清空 `annotationList`、`renderAnnotationPanel`、上行 dirty=0。

- [ ] **Step 5: hebweb 复现验证**

```bash
# Playwright: 选元素调样式 → 加入列表 → 断言浮窗出现 1 项；再做一条 → 2 项
# 点第一项 → 断言注释框重新展开且元素高亮恢复；× 移除 → 断言项消失
```
Expected: 浮窗收集/展开/移除/计数正确

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/browser/inspector.js
git commit -m "inspector: 统一汇总浮窗(注释列表)替代旧修改队列(功能 D)

- Why: 旧队列只收样式 diff、纯对话不触发，用户看不见；统一成收完整注释项
- 项含多元素+对话+结构改动，可展开恢复全部状态；底部全部提交 + 单条直提两出口
- 上行 heb:annotation:dirty 供防丢失拦截用
- 影响范围: inspector DOM 层，旧 editQueue 消息名迁移"
```

### Task 11: 多注释合并总结链路

**Files:**
- Modify: `apps/desktop/src/browser/mod.rs`（`forward_inspector_message` 加 `heb:annotation:submit-all` + `handle_annotation_submit_all`）
- Modify: `apps/desktop/frontend/src/App.tsx`（消费 `browser://annotation-summary`）
- Modify: `apps/desktop/frontend/src/desktop/ui/lib/browserHost.ts`（事件契约）

**契约（spec §5.7）：** N 条注释打包 → 旁支会话一起总结 → 一条 user message 进主对话。

- [ ] **Step 1: 后端 `handle_annotation_submit_all`**

`forward_inspector_message` 的 `match ty` 加 `"heb:annotation:submit-all" => handle_annotation_submit_all(app, session_id, &payload),`。新函数：把 N 条注释（元素定位 + 样式 diff + 对话要点 + 结构改动）拼成一个总 prompt，复用 `aside_send_args(.., enabled_tools=[])` 纯文本总结（仿 `handle_aside_submit`），结果 `app.emit("browser://annotation-summary", {summary, boundSessionId, items})`。

- [ ] **Step 2: browserHost.ts 加事件**

`BrowserHost` 接口加 `onAnnotationSummary(cb)`，监听 `browser://annotation-summary`（仿现有 `onAnnotationBatch`）。

- [ ] **Step 3: App.tsx 消费**

加一个 `useEffect` 监听 `onAnnotationSummary`，组装 user message（导语 + summary + 各元素 element-N.json 附件，复用 `buildBatchAnnotationMessage` 思路）→ `store.sendUserMessage(.., target)`（仿现有 onAnnotationBatch useEffect）。

- [ ] **Step 4: 编译验证**

Run: `cargo check -p hebbian-desktop` + `cd apps/desktop/frontend && pnpm exec tsc --noEmit`
Expected: 通过

- [ ] **Step 5: hebweb 端到端复现验证**

```bash
# Playwright: 攒 2 条注释 → 全部提交 → 看主对话收到一条整合 user message
# 含两条注释的修改要点 + element-N.json 附件
```
Expected: 合并总结进主对话

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/browser/mod.rs apps/desktop/frontend/src/App.tsx apps/desktop/frontend/src/desktop/ui/lib/browserHost.ts
git commit -m "browser: 多注释合并总结 → 一条整合 user message 进主对话(功能 D)

- Why: 浮窗攒的多条注释一起提交，让助手整合成一份连贯修改需求
- handle_annotation_submit_all 复用旁支总结机制；App.tsx 组装带 element-N.json 附件
- 影响范围: mod.rs + App.tsx + browserHost.ts 消费端"
```

---

## 阶段七：未提交防丢失（功能 G）

### Task 12: dirty 上行 + 工具栏拦截 + beforeunload 兜底

**Files:**
- Modify: `apps/desktop/src/browser/mod.rs`（`forward_inspector_message` 加 `heb:annotation:dirty` → emit `browser://annotation-dirty`）
- Modify: `apps/desktop/frontend/src/desktop/ui/lib/browserHost.ts`（`onAnnotationDirty` 事件 + `allowUnload` 命令）
- Modify: `apps/desktop/frontend/src/desktop/ui/components/BrowserPanel.tsx`（刷新/导航前拦截）
- Modify: `apps/desktop/src/browser/inspector.js`（`beforeunload` + 一次性放行标志）

**契约（spec §5.10）：** 两类入口各管不重叠 + 一次性放行去重。

- [ ] **Step 1: dirty 上行透传**

`forward_inspector_message` 的 `match ty` 加 `"heb:annotation:dirty" => { app.emit("browser://annotation-dirty", with_session(payload)); }`。

- [ ] **Step 2: BrowserPanel 存 dirty count + 工具栏拦截**

`Inst` 加字段 `dirtyCount: number`；订阅 `onAnnotationDirty` 更新。刷新按钮 / 后退 / 前进 / 地址栏提交的 handler，执行前若 `cur.dirtyCount > 0` → 弹自定义确认 dialog（中文人话：「你有 N 条注释还没提交，刷新就没了，确定吗？」），用户确认才继续；确认后先调 `host.allowUnload(sid)`（下发 `heb:unload:allow` 置一次性放行）再执行 reload/navigate。

> 弹窗组件复用项目现有 confirm dialog 模式（参考 RightSidebar / 现有 toast 确认）；文案守 CLAUDE.md 步骤 3.1，不暴露内部命名。

- [ ] **Step 3: inspector beforeunload 兜底 + 放行标志**

inspector 加模块级 `var unloadAllowOnce = false;`；`handleIn` 加 `case "heb:unload:allow": unloadAllowOnce = true; break;`。注册 `window.addEventListener("beforeunload", fn)`：`annotationList.length > 0 && !unloadAllowOnce` 时 `e.preventDefault(); e.returnValue = "";`（原生确认框）；每次进入回调后 `unloadAllowOnce = false`（一次性，用完即清）。

- [ ] **Step 4: browserHost + 命令接线**

`browserHost.ts` 加 `onAnnotationDirty(cb)` + `allowUnload(sid)`（后者新增 Tauri command `browser_allow_unload` → `send_down(inst, "heb:unload:allow", {})`，参考现有 `browser_clear_selection`）。

- [ ] **Step 5: hebweb 复现验证**

```bash
# Playwright:
# (a) 队列非空 → 点工具栏刷新 → 断言弹自定义中文确认框；确认 → 只刷一次(不双弹)
# (b) 队列空 → 点刷新 → 无拦截直接刷
# (c) 页面内 location.reload() → 断言 beforeunload 原生框
# (d) 全部提交后 → 刷新无拦截(dirty 归零)
```
Expected: 四种路径都对、不双弹

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/browser/mod.rs apps/desktop/src/browser/inspector.js apps/desktop/frontend/src/desktop/ui/components/BrowserPanel.tsx apps/desktop/frontend/src/desktop/ui/lib/browserHost.ts apps/desktop/frontend/src/desktop/bridge/tauri.ts
git commit -m "browser: 未提交注释刷新防丢失(功能 G)

- Why: 汇总浮窗草稿态，刷新/离开一刷没，拦一下避免白丢
- 两类入口各管+一次性放行去重避免双弹：工具栏走自定义中文框，页面自身跳转 beforeunload 兜底
- dirty count 上行供前端拦截判断；allowUnload 下发让 beforeunload 跳过不二次弹
- 影响范围: mod.rs + inspector + BrowserPanel + browserHost"
```

---

## 阶段八：架构.md 同步 + 端到端验证

### Task 13: 架构.md / changelog 登记 + 全链路复现

**Files:**
- Modify: `docs/架构.md`（§8.5 / §4.4 / §13）
- Modify: `docs/changelog.md`

- [ ] **Step 1: 架构.md §8.5 第 2 条补充**

「注释支持多元素选取（@N 引用）+ 结构草稿操作（PreviewMutate）+ 浏览器交互（PreviewAct）+ 统一汇总浮窗 + 未提交防丢失。截图视觉回传（PreviewCapture）推迟单独立项。」

- [ ] **Step 2: §4.4 工具表登记**

`PreviewMutate` / `PreviewAct` 与 `PreviewStyle` 并列登记为旁支会话专属信号工具（不进 BUILTIN_TOOL_NAMES）。

- [ ] **Step 3: §13 决策表追加**

「预览结构操作 / 交互 = 草稿态 + 提交时由主对话落地源码（不直接写源码）」。

- [ ] **Step 4: changelog 追加一条**

按 changelog 模板（日期 / 总结 / Why / 改动列表 / 影响范围 / 留尾巴：F 截图推迟单独立项）。

- [ ] **Step 5: 全量验证**

```bash
cargo check --workspace
cargo test -p agent-core --lib
cd apps/desktop/frontend && pnpm exec tsc --noEmit && cd -
# hebweb 端到端：选多元素 → @N 对话让助手改 @2 样式 + 新增元素 + 点击交互
#   → 加入列表 → 攒 2 条 → 全部提交 → 主对话收到整合需求
#   → 刷新前弹防丢失警告
```
Expected: 全绿 + 端到端链路通

- [ ] **Step 6: Commit**

```bash
git add docs/架构.md docs/changelog.md
git commit -m "docs: 登记内置浏览器多元素注释 + 交互 + 防丢失(架构.md §8.5/§4.4/§13 + changelog)

- 多元素注释/PreviewMutate/PreviewAct/统一浮窗/防丢失落地，截图 F 推迟单独立项
- 影响范围: 文档"
```

---

## 自检（写完计划后的 fresh-eyes 检查）

- **spec 覆盖**：A(Task4) / B(Task5) / ①(Task6-7) / C(Task2,7) / D(Task10-11) / E(Task8-9) / G(Task12) / F(推迟，spec 已标注) — 全覆盖
- **类型一致**：`draft` / `annotationList` / `refToIndex` / `composeAsideText` / `heb:aside:apply|mutate|act` / `heb:annotation:dirty|submit-all` / `heb:unload:allow` 在各 task 命名一致
- **无 placeholder**：agent-core 三个工具给完整代码 + TDD；inspector/React 集成层给锚点 + 契约 + hebweb 复现（用户已确认分层粒度）
- **白名单一致**：Task 9 restrict_tools = 3 个 preview 工具（不含推迟的 Capture）

## 风险与留尾巴

- **F 截图推迟**：需 agent-core 加「截图通道」async trait（Desktop 实现），A-E 跑通后单独立项
- **contenteditable + IME**：@N chip 的中文输入法兼容是 Task 5 最大风险点，hebweb 必须测中文
- **inspector.js 体积**：本计划后 inspector 更大，新功能组织成独立段落（draft 状态区 / chip 区 / 浮窗区 / 防丢失区）
- **html2canvas 打包**：随 F 一起推迟，本期 inspector 不引入外部库


