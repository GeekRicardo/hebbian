# 右上角事件弹窗提醒

## 目标

hebbian **在后台时**（用户在其他 app 前台），通过屏幕右上角弹窗通知发生的事件。纯通知，不内嵌交互操作。

- **需等待类事件**（approval / question）：持续显示直到用户回到 hebbian 处理，不自动消失
- **即时通知类事件**（turn_completed / 其他）：显示后 3s 自动关闭

## 行为规则

| 条件 | 行为 |
|------|------|
| hebbian 在前台 | **不弹** 通知（用户已能看到） |
| hebbian 在后台 + approval | 弹通知，不消失；可折叠/拖拽 |
| hebbian 在后台 + question | 弹通知，不消失；可折叠/拖拽 |
| hebbian 在后台 + turn_completed | 弹通知，3s 消失 |
| 用户点击已显示的 notification | 调起 hebbian 窗口（bring to front） |
| pending 卡片折叠 | 缩成小条（仅图标），60s 后自动展开一次（循环，直到消失） |
| 拖拽 | 任意位置拖动，位置持久化到该通知生命周期 |

## 技术方案

Tauri 2 创建无边框、置顶、透明 webview 窗口，定位屏幕右上角。窗口不抢焦点、不在任务栏显示。纯单向通知，无需 rebuild 前端审批交互。

### 为什么不用其他方案

- **系统原生通知**：不可固定右上角、不可自定义样式
- **前端 toast**：只在 hebbian 窗口内可见

## 文件变更

### 新建文件

| 文件 | 职责 |
|------|------|
| `apps/desktop/src/notch.rs` | 管理 NotchWindow 生命周期、通知队列、Tauri 事件监听 |
| `apps/desktop/frontend/src/notch.html` | NotchWindow 的极简 HTML 入口 |
| `apps/desktop/frontend/src/notch.tsx` | NotchWindow 的 React 入口，管理通知卡片渲染 |
| `apps/desktop/frontend/src/desktop/ui/components/NotificationCard.tsx` | 通知卡片：图标 + 标题 + 摘要 + 关闭按钮 |

### 修改文件

| 文件 | 改动 |
|------|------|
| `apps/desktop/src/lib.rs` | setup 中初始化 NotchManager；注册 `notify_dismiss` 命令 |
| `apps/desktop/src/chat.rs` | PermissionRequested / UserQuestionRequested / TurnFinished → emit Tauri `notification` 事件 |
| `apps/desktop/vite.config.ts` | 多入口：main + notch |

## 任务拆解

### Task 1: NotchManager 后端（notch.rs）

**文件**：`apps/desktop/src/notch.rs`（新建）

```
pub struct NotchManager {
    app: AppHandle,
    window_label: &'static str, // "notch"
    current_type: Option<NotchType>,
    queue: VecDeque<NotchEntry>,
}

enum NotchType {
    Pending,
    Info,
}
```

- `new(app: &AppHandle)` → 注册 `notification` Tauri 事件监听（`listen_global`）
- 事件回调：反序列化 NotificationPayload → `push()` 入队 → `flush()` 如果空闲
- `flush()`：检入队首 → 创建或更新 NotchWindow → 通过 emit 把 payload 发给前端
- `dismiss()`：隐藏窗口（`hide()` 而非 `close()`，避免重建开销）
- `bring_hebbian_to_front()`：`app.get_webview_window("main")?.set_focus()`
- 前台检测：监听 `window.on_focus` / `on_blur` 事件决定是否出 popup
- 窗口配置：`decorations: false`、`always_on_top: true`、`focus: false`、`transparent: true`、`skip_taskbar: true`、`visible: false`（初始隐藏）、宽 360px 高 auto、右上角定位

**验证**：`cargo check`

### Task 2: chat.rs 事件桥接

**文件**：`apps/desktop/src/chat.rs`

在 `agent_event_to_engine_event` 调用之后：

```rust
// PermissionRequested → emit notification
if matches!(engine_event, EngineEvent::PermissionRequested { .. }) {
    let _ = app_handle.emit("notification", serde_json::json!({
        "type": "pending",
        "kind": "permission_requested",
        "title": "工具运行等待审批",
        "summary": tool_summary,
    }));
}
// UserQuestionRequested → emit notification
if matches!(engine_event, EngineEvent::UserQuestionRequested { .. }) {
    let _ = app_handle.emit("notification", serde_json::json!({
        "type": "pending",
        "kind": "user_question",
        "title": "需要你的回答",
        "summary": question_text,
    }));
}
// TurnFinished → emit notification (info)
if matches!(engine_event, EngineEvent::TurnFinished) {
    let _ = app_handle.emit("notification", serde_json::json!({
        "type": "info",
        "kind": "turn_completed",
        "title": "回答完成",
        "summary": "Agent 已完成当前回合",
    }));
}
```

### Task 3: 前端 NotchWindow + NotificationCard

**文件**：
- `apps/desktop/frontend/src/notch.html`：`<div id="root"></div>` + vite 入口
- `apps/desktop/frontend/src/notch.tsx`：React root，监听 `notification` 事件，管理 NotificationCard 渲染
- `apps/desktop/frontend/src/desktop/ui/components/NotificationCard.tsx`：

```
展开态：
┌─────────────────────────────────────┐
│ ⚠️ 工具运行等待审批            ◀  ✕│
│ mkdir /etc/config                   │
│   点此查看                          │
└─────────────────────────────────────┘

折叠态（点击 ◀ 后）：
┌───┐
│ ⚠️│
└───┘
（60s 后自动重新展开）
```

卡片功能：
- 图标（⚠️ pending / ✓ info）
- 标题行 + 折叠按钮（◀ 收起 / ▶ 展开）+ 关闭按钮（✕）
- 摘要文本（最多两行，截断）
- **拖拽**：`onMouseDown/onMouseMove/onMouseUp` 原生实现，通过 position state 自由移动，不依赖第三方库
- 点击卡片主体 → 调起 hebbian 主窗口
- info 类：3s setTimeout 自动 dismiss
- pending 类：不自动消失；可折叠；折叠后 60s setTimeout 自动展开一次（循环直到 dismiss）
- 窗口配置要求不透传鼠标事件到底层应用（`hit_test` 允许拖拽和点击）

CSS 用 tailwind，玻璃态 (`backdrop-blur-lg bg-black/70`)，圆角 `rounded-2xl`，过渡动画。

### Task 4: Vite 多入口 + tauri.conf 配置

**文件**：
- `apps/desktop/vite.config.ts`：加 notch 入口
- `apps/desktop/tauri.conf.json`：无需改（与现有配置兼容）

### Task 5: lib.rs 注册

**文件**：`apps/desktop/src/lib.rs`

```rust
mod notch;

// in run():
.manage(notch::NotchManager::new(&app.handle())?)
```

注册 `notify_dismiss` Tauri 命令。

## 优先级队列（精简版）

```
pending > info
```

- pending（approval/question）：立即显示，不自动消失
- info（turn_completed）：加入队列；如果队列为空立即显示并 3s 计时；如果有 pending 则排队，pending 消失后显示
- 同一类型连续到达 → dismiss 旧卡片再显示新卡片

## 注意事项

- 窗口不要 `close()` 而是 `show()`/`hide()`——避免反复创建重建 webview 开销
- `focus: false` 是关键——不能抢其他 app 焦点
- 点击卡片 → 调起**对应进程**的 hebbian 主窗口（`get_webview_window("main")` 进程内查找，多进程天然隔离不串）
- 多 hebbian 进程天然隔离：各自 notch 窗口 label 在进程内唯一
- 不使用 `notify_result` 回调——审批/回答仍在 hebbian 主窗口内完成
