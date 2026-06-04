# HebIsland 设计规格

> 来源：hebisland-design.html 原型 + 对话确认的所有设计要点。
> 本文档是实现的唯一锚。

---

## 1. 概述

HebIsland 是 Hebbian Desktop 的桌面级通知 / 快捷操作浮层，替代原 Notch 系统。

- **一个全屏透明窗口**（always-on-top / click-through / 无装饰 / 不抢焦点），卡片区域拦截鼠标事件，其余穿透。
- 同时显示**多张卡片**，按 zone 分组排列。
- 每张卡片可独立**折叠 / 展开 / 拖拽 / 关闭**。

---

## 2. Zone 与布局

### 2.1 两个 zone

| Zone | 默认位置 | 堆叠方向 |
|------|---------|---------|
| `top-right` | 屏幕右上角 | 从顶向下 |
| `bottom-right` | 屏幕右下角 | 从底向上 |

### 2.2 布局常量

```
MARGIN  = 20px   // 屏幕边距
GAP     = 12px   // 卡片间距
CARD_W  = 336px  // 展开宽度
FOLDED  = 48px   // 折叠正方形边长
```

### 2.3 行槽与折叠紧凑

- 展开时：行槽高 = 卡片自然高度（expandedH），宽 = CARD_W。
- 折叠时：行槽高 = FOLDED（48px），宽 = FOLDED。
- **折叠后行槽收缩**，相邻卡片会挤过来紧凑排列。

### 2.4 折叠锚点方向

| Zone | 折叠方向 | 展开方向 |
|------|---------|---------|
| `top-right` | 贴右上角（top-left 不变，向右上收缩） | 向左下展开 |
| `bottom-right` | 贴右下角（bottom-right 不变，向右下收缩） | 向左上展开 |

具体实现：
- **top-right**：折叠时 x = screenW - MARGIN - FOLDED，y = slotY（顶对齐）。
- **bottom-right**：从 `y = screenH - MARGIN` 向上推进；折叠时 slotH = FOLDED，卡片顶边 = y - FOLDED，x = screenW - MARGIN - FOLDED。

### 2.5 layoutAll 伪代码

```
top-right:
  y = MARGIN
  for card in sorted_by_order:
    slotH = folded ? FOLDED : expandedH
    x = folded ? (W - MARGIN - FOLDED) : (W - MARGIN - CARD_W)
    card.pos = (x, y)        // 顶对齐
    y += slotH + GAP

bottom-right:
  y = H - MARGIN
  for card in sorted_by_order:
    slotH = folded ? FOLDED : expandedH
    y -= slotH
    x = folded ? (W - MARGIN - FOLDED) : (W - MARGIN - CARD_W)
    card.pos = (x, y)        // 底对齐自然成立
    y -= GAP
```

---

## 3. 卡片状态

每张卡片 3 种视觉态：

| 状态 | 尺寸 | 外观 |
|------|------|------|
| 展开（默认） | CARD_W × auto | 圆角卡片，header + body + actions |
| 折叠 | FOLDED × FOLDED | 圆形方块，显示类型图标 |
| 拖拽中 | 同当前态 | 跟随鼠标，带阴影提升 |

### 3.1 折叠 / 展开

- 双击卡片 → toggle 折叠。
- 折叠态双击 → 展开。
- 带 CSS transition（0.35s ease）平滑过渡尺寸和位置。

### 3.2 拖拽

- mousedown + mousemove 启动拖拽（3px 死区防误触）。
- 拖拽中卡片跟随鼠标。
- mouseup 释放：snap 回布局位置（layoutAll 重算）。
- 拖拽期间 `pointer-events: none` 在非拖拽卡片上。

### 3.3 关闭

- 展开态 header 有 ✕ 按钮。
- 点击 → 卡片移除 + 通知后端 dismiss。
- 带 fade-out 动画。

---

## 4. 窗口配置

```
label:         "island"
decorations:   false
always_on_top: true
transparent:   true
skip_taskbar:  true
focusable:     false          // 不抢焦点
inner_size:    全屏（primary monitor 尺寸）
visible:       false          // 初始隐藏，有卡片时 show
```

关键点：
- 窗口 body 背景 `transparent`。
- 卡片区域通过正常 HTML 元素拦截鼠标事件。
- 非卡片区域 `pointer-events: none` 穿透到下层。
- macOS 需要 `ignore_cursor_events` 配合，由前端通过 `setIgnoreCursorEvents` 动态切换。

---

## 5. 通知类型与行为

继承原 Notch 系统的三种通知，但改为多卡片共存：

| 类型 | kind | 行为 |
|------|------|------|
| 审批请求 | `permission_requested` | 持续显示，可内联批准/拒绝（仅 tool_call），被 resolve 后自动撤除 |
| 用户提问 | `user_question` | 持续显示，点击打开主窗口回答，被回答后自动撤除 |
| 回答完成 | `turn_completed` | 3s 自动消失 |

### 5.1 弹出策略

- `ISLAND_ALWAYS_POP = true`（调试）：始终弹出。
- `ISLAND_ALWAYS_POP = false`（生产）：仅主窗口不在前台时弹出。

---

## 6. 动画

所有位置 / 尺寸变化走 CSS transition：
```css
.island-card {
  transition: left 0.35s ease, top 0.35s ease,
              width 0.35s ease, height 0.35s ease,
              opacity 0.35s ease, border-radius 0.35s ease;
}
```

拖拽中暂停 transition（直接跟鼠标）。

---

## 7. 架构映射

- Rust 后端：`apps/desktop/src/island.rs`（替代 notch.rs）
- 前端入口：`IslandApp.tsx`（替代 NotchApp.tsx）
- 前端卡片：`IslandCard.tsx`（替代 NotificationCard.tsx）
- 路由入口：`/?island=1`（替代 `/?notch=1`）
- lib.rs：`island::initialize_island` / `island::create_island_state`
- 窗口 label：`"island"`（替代 `"notch"`）

---

## 8. 后端 → 前端协议

### 8.1 推送通知

后端通过 `window.eval` 派发 CustomEvent：

```
event: "island-push"
detail: {
  id: string,           // 唯一标识（后端生成）
  zone: "top-right" | "bottom-right",
  type: "pending" | "info",
  kind: "permission_requested" | "user_question" | "turn_completed",
  title: string,
  summary: string,
  request_id?: string,
  perm_kind?: string,
}
```

### 8.2 撤除通知

```
event: "island-remove"
detail: { id: string }
```

### 8.3 前端 → 后端 Tauri 命令

| 命令 | 参数 | 说明 |
|------|------|------|
| `island_dismiss` | `id: string` | 用户关闭卡片 |
| `island_click` | `id: string` | 点击卡片 → 打开主窗口 |
| `island_approve` | `requestId, decision` | 内联审批 |

---

## 9. 鼠标穿透策略

全屏透明窗口必须在「非卡片区域」穿透鼠标事件。

### macOS (Tauri)

- 窗口初始 `setIgnoreCursorEvents(true)`。
- 前端在 `mousemove` / `mouseenter` 时检测光标是否在卡片区域内：
  - 进入卡片 → `setIgnoreCursorEvents(false)`
  - 离开卡片 → `setIgnoreCursorEvents(true)`
- 通过 `document.elementFromPoint` 判断是否命中卡片元素。

---

## 10. 从 Notch 迁移

1. `notch.rs` → `island.rs`：状态从单条队列改为 `HashMap<id, IslandEntry>`
2. `NotchApp.tsx` → `IslandApp.tsx`：从单卡片改为多卡片 + 布局引擎
3. `NotificationCard.tsx` → `IslandCard.tsx`：基本保留，增加折叠态渲染
4. 路由参数 `?notch=1` → `?island=1`
5. lib.rs 注册改为 `island::*` 命令
6. chat.rs 调用点从 `notch::emit_*` 改为 `island::emit_*`
