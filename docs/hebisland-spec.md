# HebIsland 实现规格（macOS native / Swift）

> 来源：`docs/hebisland.md` 设计决策 + `docs/hebisland-design.html` 视觉原型。
> 本文档是 `apps/island-mac/` 实现的**唯一锚**。代码实现委托 codex 按本文档完成。
>
> **路线变更（2026-06-04）**：放弃 Tauri 多窗口方案，改为 macOS native（Swift + AppKit/SwiftUI）。
> 原因见 §0。

---

## 0. 为什么从 Tauri 切到 native

旧方案用 Tauri 为每条通知动态创建一个无边框 webview 窗口。在 macOS 上撞到架构性死路：

- 通知窗口需要在收到 socket 消息时**运行时动态创建**，而 socket 监听跑在后台线程。
- macOS 的 `WKWebView` **必须在主线程创建并加载**。从后台线程 `build()`，窗口框架能出现（能看到一个空框），但 webview 内容永远不 attach、不加载 —— 表现为**纯透明空窗 / 白屏**，连注入脚本都不执行。
- 即便用 `run_on_main_thread` 把创建搬回主线程，仍要绕开 query 传参（`WebviewUrl::App` 把整串当资源路径）、透明窗口显示、嵌入资源协议等一连串坑。

native 路线用 `NSPanel` 直接渲染 SwiftUI，无 webview、无嵌入资源、无线程契约陷阱，且更轻量、启动更快、内存更省。参考实现：CodeIsland（`other/CodeIsland`，纯 Swift 刘海面板）。

---

## 1. 概述

HebIsland 是**独立 macOS native 二进制** (`hebisland`)，通过 Unix socket (`~/.hebbian/island.sock`) 与外部通信。

- **菜单栏后台 agent**（`LSUIElement = true`，无 Dock 图标），常驻监听 socket。
- **每条通知一个无边框 `NSPanel`**，在「当前焦点窗口所在屏幕」的右上角堆叠。
- 通知可指定自动消失时间、自定义操作按钮。
- 用户操作（点击按钮 / 关闭）通过**同一 socket 连接回传给调用方**。
- 不持有 `agent_core`，不读写 session，不执行 shell，不访问网络。

定位：`hebisland` 是一个独立的本地 UI 外设，不是第四个 `agent_core` surface。

---

## 2. 技术栈与项目结构

| 项 | 选择 |
|---|---|
| 语言 | Swift 5.9+ |
| UI | SwiftUI（卡片视图）+ AppKit（`NSPanel` / `NSHostingView` / 菜单栏） |
| 构建 | Swift Package Manager（`Package.swift`，可执行 target） |
| 最低系统 | macOS 14 |
| socket | `Network` framework（`NWListener` + `NWEndpoint.unix`） |
| JSON | `Codable` |
| CLI 解析 | 手写参数解析或 `ArgumentParser`（轻量优先，手写即可） |

目录布局：

```
apps/island-mac/
├── Package.swift
├── Sources/
│   └── HebIsland/
│       ├── main.swift              # CLI 入口：分发 daemon / notify
│       ├── AppDelegate.swift       # NSApplication 生命周期 + 菜单栏 item + 启动 socket
│       ├── SocketServer.swift      # NWListener 监听 + 每连接读写 + action 回写
│       ├── Protocol.swift          # Codable 消息类型（见 §4）
│       ├── NotificationManager.swift # 通知生命周期：show/dismiss/堆叠/超时计时器
│       ├── PanelController.swift   # 单条通知的 NSPanel 创建/定位/关闭
│       ├── ScreenResolver.swift    # 焦点窗口所在屏幕判定 + 切换监听
│       ├── CardView.swift          # SwiftUI 卡片（暗色，对齐 design.html）
│       └── NotifyClient.swift      # `notify` 子命令：连 socket 写一行 JSON（可 --wait）
└── Tests/
    └── HebIslandTests/
        └── ProtocolTests.swift     # JSON 编解码 / 堆叠定位 / action 路由
```

---

## 3. App 形态

- **菜单栏 agent**：`Info.plist` / `NSApplication` 设 `LSUIElement`（`setActivationPolicy(.accessory)`），无 Dock 图标、不抢焦点。
- 菜单栏 item 提供：状态文案（监听中 / socket 路径）、退出。
- `daemon` 子命令启动 `NSApplication` 主循环 + socket server。
- `notify` 子命令是纯 CLI：连 socket 写一行 JSON 后退出（不启 NSApplication），见 §5。
- **daemon 单例**：`daemon` 启动时先探测 socket，若已有活 daemon 则打印提示并 `exit(0)` 复用，**绝不 unlink 抢占**在跑的 socket（否则会踢掉 Desktop 的长连接）。探测用同步 POSIX `connect`（见 `DaemonProbe.swift`）。
- **自动拉起**：任何调用方推送前确保 daemon 在跑——没有就 spawn 一个、有就复用（`ensureDaemonRunning`：探测不通 → `Process` 启动自身 `daemon` 子进程并切断 stdio → 轮询等 socket ready，约 5s）。`notify` 已内置；Desktop 的 `hebisland_client.rs` 后续也应在连接失败时走同一逻辑。

---

## 4. Socket 协议

> ⚠️ **本节是硬契约**：Desktop 的 `apps/desktop/src/hebisland_client.rs` 已经在用这套协议，native 端必须逐字对齐，否则 Desktop 通知 / 审批回传会断。

### 4.1 连接

```
~/.hebbian/island.sock   （Unix domain socket, stream）
```

- daemon 启动时：先 `unlink` 旧 socket 文件 → 设 `umask 0o077` → `bind` → 恢复 umask → `chmod 0o700`。保证 socket 文件只当前用户可读写（关闭 TOCTOU 窗口）。
- 每行一个 JSON，`\n` 分隔。
- 连接是**双工长连接**：调用方写入 → island 执行；island 在**同一连接**上回写 action 事件。
- 同时支持多条并发连接（Desktop 一条长连接 + CLI `notify` 各自短连接）。

### 4.2 调用方 → island

```jsonc
// 展示通知（id 与 card.id 相同）
{"type":"show","id":"<id>","card":{NotificationCard}}

// 关闭通知
{"type":"dismiss","id":"<id>"}
```

### 4.3 island → 调用方（action 回传）

```jsonc
{"msg_id":"<id>","action":"<action>"}
```

- **回传写回「展示该通知的那条连接」**。Desktop 的长连接据此落地 HITL。
- `notify --wait` 的短连接：保持连接直到收到首个 action，打印后退出。
- `notify`（无 `--wait`）：写完即断开，收不到回传。
- 字段名 `msg_id` 是 **snake_case**（既成契约，勿改）。

### 4.4 NotificationCard

```jsonc
{
  "id":         "<string>",              // 通知唯一 id（与外层 id 相同）
  "cardType":   "info | approval | question | success",
  "title":      "<string>",
  "body":       "<string>",
  "sessionId":  "<string>?",             // 可选
  "durationMs": 5000,                    // 可选；null/省略=按 cardType 默认
  "actions":    ["知道了","打开"],         // 可选；null/省略=按 cardType 默认按钮
  "options":    [{"label":"右上角","desc":"经典位置"}],  // 可选；question 卡的可选项
  "multiSelect": false,                  // 可选；question 卡是否多选（默认单选）
  "subcommands": [{"tool":"Bash","detail":"cargo test","checked":true}]  // 可选；approval 卡的子命令勾选列表
}
```

字段命名：外部 JSON 用 camelCase（`cardType` / `sessionId` / `durationMs` / `multiSelect`），唯一例外是回传的 `msg_id`（历史契约）。Swift 侧用 `CodingKeys` 映射。

**向后兼容**：Desktop 当前只发 `id/cardType/title/body/sessionId`，**不发** `durationMs/actions/options/multiSelect/subcommands`。native 端必须把这些当可选，缺省走默认。

### 4.5 durationMs 语义

| 值 | 行为 |
|----|------|
| `null` / 省略 | 按 cardType 默认：`info`/`success` = 5000ms 后自动消失，`approval`/`question` = 常驻 |
| `0` | 不自动消失（常驻） |
| `> 0` | 指定毫秒后自动消失，消失时回传 `action: "dismiss"` |

鼠标 hover 卡片时暂停倒计时（对齐 design.html 行为），移开继续。

### 4.6 actions 与回传值

> **关键**：按钮上**显示**的文字可以是中文，但**回传给 Desktop 的 `action` 值必须是英文枚举** `allow` / `deny` / `open` / `dismiss`。Desktop 的 reader 只认这四个：`allow`/`deny` → 落 HITL，`open`/`dismiss` → 不动作。回传别的值 Desktop 会忽略。

**cardType 默认按钮**（显示文字 → 回传 action）：

| cardType | 默认按钮 | 回传 action |
|----------|---------|------------|
| `info` | 无按钮（超时自动消失） | 超时回传 `dismiss` |
| `success` | 无按钮（超时自动消失） | 超时回传 `dismiss` |
| `approval` | 拒绝 / 一次 / 对话 / 项目 / 全局 | `deny` / `allow` / `allow_conversation` / `allow_project` / `allow_global` |
| `question` | 跳过 / 提交 | `skip` / `submit` |

**右上角 ✕ 关闭**（所有卡片）→ 回传 `dismiss`。

**自定义 `actions`（仅供 `notify --wait` 等自助调用方）**：当 `card.actions` 非空时，按数组渲染按钮，点击**回传按钮名本身**（此时调用方自己约定语义，Desktop 不使用该路径）。

### 4.7 问答选项与子命令勾选

**问答选项**（`card.options`）：

- `question` 卡可携带 `options: [{label, desc?}]` 数组，渲染为选项列表
- `multiSelect: true` 时多选（方框），`false` 或省略时单选（圆点）
- 用户选中后点「提交」→ 回传 `{"action":"submit","selected":[0,2],"input":"自由输入文本"}`
- 点「跳过」→ 回传 `{"action":"skip"}`

**子命令勾选**（`card.subcommands`）：

- `approval` 卡可携带 `subcommands: [{tool, detail?, checked?}]` 数组，渲染为「待审批队列」
- 用户可切换勾选状态，点审批按钮时回传 `{"action":"allow","checked":[0,1]}`
- `checked` 数组为空表示用户取消了所有勾选

**回传格式**：

```jsonc
// 按钮 action（无额外数据）
{"msg_id":"perm-1","action":"allow"}

// 问答提交（带选中项 + 输入）
{"msg_id":"q-1","action":"submit","selected":[0],"input":"右上角"}

// 审批带勾选
{"msg_id":"perm-2","action":"allow","checked":[0,1]}
```

字段 `selected`/`input`/`checked` 为可选，缺省为 `null`。

---

## 5. CLI

```bash
hebisland daemon                          # 启动菜单栏 agent（常驻）+ socket 监听
hebisland notify --msg '<json>'           # 单次推送（fire-and-forget，不等回传）
hebisland notify --msg '<json>' --wait    # 推送并等待首个 action 回传，打印后退出
hebisland notify --msg '<json>' --wait --timeout 30   # 等待超时秒数（默认 60）
```

`--wait` 行为：连 socket → 写 `show` → 阻塞读取直到收到一行 action JSON → 打印到 stdout、退出 0；超时则打印错误到 stderr、退出 1。

示例：

```bash
hebisland daemon &

# 5s 自动消失的信息通知
hebisland notify --msg '{"type":"show","id":"n1","card":{"id":"n1","cardType":"info","title":"完成","body":"编译通过","durationMs":5000}}'

# 常驻审批通知，等待用户操作
hebisland notify --wait --msg '{"type":"show","id":"n2","card":{"id":"n2","cardType":"approval","title":"审批","body":"运行 cargo check?"}}'
# 用户点"允许" → stdout: {"msg_id":"n2","action":"allow"}
```

---

## 6. 窗口规格（NSPanel）

每条通知一个 `NSPanel`，由 `PanelController` 管理。关键属性（参考 CodeIsland `PanelWindowController`）：

```swift
let panel = NSPanel(
    contentRect: rect,
    styleMask: [.borderless, .nonactivatingPanel],
    backing: .buffered,
    defer: false
)
panel.level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.mainMenuWindow)) + 2) // 浮于菜单栏之上
panel.backgroundColor = .clear
panel.isOpaque = false
panel.hasShadow = false                  // 阴影由 SwiftUI 卡片自己画，避免方角阴影
panel.isMovableByWindowBackground = false
panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
panel.contentView = NSHostingView(rootView: CardView(...))
```

- **首次点击即生效**：`nonactivatingPanel` 默认吞掉激活点击。需子类化 `NSPanel` 重写 `canBecomeKey` 返回 `true`，并让按钮点击不依赖窗口先激活（参考 CodeIsland 的 `KeyablePanel`），保证用户一下点中「允许」就触发，无需先点一下激活。
- 窗口尺寸固定宽 **420px**（对齐 design.html），高度按 cardType 内容给定（info 矮、approval 高）。SwiftUI 内容用 `.fixedSize` 或测量后回设 panel frame，避免透明区误吞点击。
- 全屏 Space 下不抢占（`fullScreenAuxiliary`）。

### 6.1 堆叠定位

- 锚点：当前焦点屏幕（§7）的右上角，距右 / 上边距各 20px，卡片间距 10px。
- 自上而下：最早的在最上，新通知追加到下方。
- 某卡关闭后，下方卡片上移填补空隙（带动画）。
- y 坐标按各卡片实际高度累加（不要假设等高）。
- 坐标用 AppKit 的屏幕坐标系（注意 macOS 原点在左下角，右上角 = `screen.frame.maxY - margin - cardHeight`）。

### 6.2 超出屏幕（第一版简单策略）

- 普通（info）通知最多同时显示 5 条；超出时折叠最旧的为「还有 N 条」。
- 审批 / 问答通知不被自动挤掉、不自动关闭。

### 6.3 折叠 / 展开（对齐 design.html）

每张卡右上角 hover 显示两个控制按钮：折叠 ⌄、关闭 ✕。

- 点折叠 ⌄：卡片动画缩成 **48×48 圆角方块**（圆角 18px），**右边缘与顶边不动**（向左收起），内容切换为单字符折叠图标（按主题着色：info=✦/✓、approval=!、question=?）。
- 折叠态点击方块：动画**向左展开**回 420×内容高（右边缘仍不动）。
- 折叠 / 展开走约 0.35s 弹性过渡（`cubic-bezier(0.34,1.56,0.64,1)`），结束后重排堆叠。
- 折叠卡在堆叠里占 `FOLDED_SIZE = 48` 高，其余卡按各自高度顺延；折叠卡的 x 贴右（`maxX - margin - 48`）。

native 实现：折叠 / 关闭按钮在 SwiftUI 卡片内（hover 显示）；点击经回调让 `PanelController` 用 `NSAnimationContext` 动画 `setFrame`（保持 `maxX` / `maxY` 不变），并把 `NSHostingView` 的 rootView 切到折叠态视图；`NotificationManager` 重排时折叠卡按 48×48、展开卡按 420×内容高，都右对齐。

### 6.4 拖拽与吸附（对齐 design.html）

- 在卡片**背景**（非按钮 / 交互区）按下拖动：移动超过 `DRAG_THRESHOLD = 5`px 才算拖拽（区分拖拽 vs 点击）；拖拽中窗口跟随鼠标。
- 松手：比较窗口当前位置与其堆叠锚点（home）的距离，**dx < `SNAP_DISTANCE`(48) 且 dy < 48** → 动画吸附回 home；否则留在拖到的位置。
- 拖拽产生位移的这次操作**吃掉随后的 click**，不触发折叠 / 展开 / 审批按钮。

native 实现：`NSPanel` 自定义 `mouseDragged` 移动 `setFrameOrigin`（或 `isMovableByWindowBackground`，但要保证按钮 / 交互区点击优先、不误触发拖动）；`mouseUp` 后比较与 home 锚点距离做 snap（`NSAnimationContext` 动画 `setFrameOrigin`）。home 锚点由 `NotificationManager` 每次重排时记录到对应 `PanelController`。

---

## 7. 跟随焦点窗口所在屏幕

需求：通知弹出后，**用户把焦点切到另一块屏幕的窗口时，已有通知整体迁移到新屏幕**的右上角（screen hop）。

判定「焦点窗口在哪块屏」（参考 CodeIsland `ScreenDetector.frontmostApplicationWindowBounds`）：

1. `NSWorkspace.shared.frontmostApplication` 拿到前台 app 的 pid（排除自己）。
2. `CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)` 取窗口列表。
3. 过滤 `kCGWindowLayer == 0`（普通窗口层）、属于前台 app pid、`width/height > 0` 的第一个窗口，取其 `kCGWindowBounds`。
4. 该 bounds 中心点落在哪个 `NSScreen` → 即焦点屏幕。
5. 拿不到前台窗口（如桌面）→ 回退到 `NSScreen.main`。

触发重定位的时机：

- `NSWorkspace.shared.notificationCenter` 的 `didActivateApplicationNotification`（切换前台 app）。
- `NSApplication` 的 `didChangeScreenParametersNotification`（接显示器 / 分辨率变化）。
- 兜底：低频 `Timer`（如 500ms）比对焦点屏幕签名，变了才 hop（避免每帧重排）。

hop 时对所有 panel 做带动画的 `setFrame`（参考 CodeIsland `animateScreenHop`）。

---

## 8. 卡片视觉（SwiftUI，对齐 design.html）

暗色基底，等宽字体，绿/红/蓝/琥珀主题色。完整视觉规格见 `docs/hebisland-design.html`，要点：

| 项 | 值 |
|---|---|
| 卡片背景 | 纯黑 `#000`，圆角 16px，1px 描边 `rgba(255,255,255,0.10)` |
| 阴影 | `0 12px 40px rgba(0,0,0,0.6)` |
| 字体 | `SF Mono` / 等宽 |
| 宽度 | 420px |
| 主题色 | green `rgb(77,217,102)` / red `rgb(255,102,102)` / blue `rgb(71,122,209)` / amber `rgb(255,179,71)` / cyan `rgb(102,179,255)` |

卡片结构（自上而下）：

```
┌────────────────────────────────────────┐
│ ⚡ 标题                       source  ✕ │   topline：图标 + 标题 + 来源徽章 + 关闭
│ 描述文本，最多两行                       │   description
│ ──────────────────────────────────────  │   分隔线（虚线，可选）
│ [拒绝]        [允许]        [打开]       │   actions
└────────────────────────────────────────┘
```

- `approval` / `question` 卡片边框做 2s 呼吸闪烁（warning=琥珀，question=青）。
- `info` 自动消失时向右弹出动画（`dismiss-bounce-right`）。
- 主题映射：`info`→cyan，`approval`→amber(warning)，`question`→cyan，外加可选 `success`/`danger`。
- 按钮配色：允许=绿底，拒绝=红底，打开/忽略=灰底（见 design.html `.btn-*`）。
- **不展示完整 JSON input**；完整内容仍在 Hebbian 主 UI / CLI 看。

折叠态（§6.3）、拖拽与吸附（§6.4）按 design.html 行为实现。

第一版仍不做：工具命令详情 / diff / 子命令勾选 / 问答选项列表 / 文本输入。这些是 design.html 里需要协议扩展才能传数据的进阶形态，留到协议扩展后再做。

---

## 9. 安全边界

- 不执行 shell、不读 session、不访问远程 URL、不持有 `agent_core`。
- socket 只监听本机 Unix socket，文件权限 `0o700`（仅当前用户）。
- 通知内容作为纯文本渲染（SwiftUI `Text`），不做任何富文本 / HTML 解析。
- 图标第一版只支持内置符号（SF Symbols / 字符），不加载远程图片。

---

## 10. 错误处理

| 场景 | 行为 |
|---|---|
| daemon 未运行时 `notify` | 自动 spawn 一个 daemon（`ensureDaemonRunning`，§3），等 socket ready 后推送；spawn 后仍连不上才报错退出 1 |
| 同时启动两个 daemon | 后启的探测到已有活 daemon → `exit(0)` 复用，不抢占 socket |
| socket 断连 | 当前通知窗口继续显示；该连接的待回传 action 丢弃（窗口仍可手动关） |
| 重复通知 id 的 `show` | 视为 update，更新已有 panel 内容，不新建窗口 |
| 审批卡片被关闭 | 回传 `dismiss`；**不自动拒绝**，原审批仍挂在 Desktop 主 UI |
| 多屏 | 默认焦点窗口所在屏（§7） |

---

## 11. 测试验收

### 11.1 单元 / 集成

- `Protocol` 的 JSON 编解码（含缺省 `durationMs/actions`、含 `sessionId`）。
- 堆叠 y 坐标计算：单条 / 多条不同高度 / 关闭中间一条后重排。
- 焦点屏幕判定：给定 window bounds → 命中的 screen。
- action 回传路由：show 在连接 A → 用户点按钮 → action 只回到连接 A。
- 重复 id = update。

### 11.2 手动验收

```bash
swift run hebisland daemon &

# info 5s 自动消失
swift run hebisland notify --msg '{"type":"show","id":"m1","card":{"id":"m1","cardType":"info","title":"完成","body":"编译通过"}}'

# 审批，等待回传
swift run hebisland notify --wait --msg '{"type":"show","id":"m2","card":{"id":"m2","cardType":"approval","title":"Bash 需要你的允许","body":"运行 cargo check"}}'
# 点"允许" → stdout: {"msg_id":"m2","action":"allow"}
```

验收点：

- 卡片出现在**焦点窗口所在屏**右上角，暗色等宽，观感接近 design.html。
- 多条自上而下堆叠不重叠；关闭一条，下方上移。
- info 5s 后向右弹出消失；审批 / 问答常驻。
- 鼠标 hover info 卡片暂停倒计时。
- 焦点切到另一屏的窗口 → 已有卡片整体 hop 过去。
- 点「允许 / 拒绝」首次点击即生效，回传英文 action。

### 11.3 与 Desktop 联调（最终验收）

1. `swift run hebisland daemon &`
2. 启动 Desktop（`pnpm tauri dev`），触发一次工具审批。
3. hebisland 弹出审批卡片；点「允许」→ Desktop 的 `hitl::resolve_hitl_from_island` 收到 `allow` 并放行。
4. 点「拒绝」→ Desktop 收到 `deny` 并拒绝。

---

## 12. 给 codex 的实现里程碑

按顺序交付，每步可独立验证：

1. **M1 — 骨架**：`Package.swift` + `main.swift` 分发 `daemon`/`notify`；`daemon` 起 `NSApplication`（accessory）+ 菜单栏 item；`notify` 连 socket 写一行。
2. **M2 — socket**：`SocketServer`（NWListener + 权限 + 多连接 + 逐行 JSON）；`Protocol` Codable；收到 `show` 先只 log。
3. **M3 — 单卡片**：`PanelController` 在主屏右上角弹一个固定内容的 `NSPanel`；`CardView` 暗色基底（先 info 卡）。
4. **M4 — 三种卡片 + 按钮 + 回传**：approval/question 按钮，点击在原连接回写英文 action；`--wait` 打通。
5. **M5 — 堆叠 + 超时 + dismiss + 重复 id update**：多卡堆叠、info 自动消失、hover 暂停、关闭重排。
6. **M6 — 焦点屏幕跟随**：`ScreenResolver` + 通知监听 + screen hop 动画。
7. **M7 — 视觉打磨**：完整对齐 design.html（边框呼吸、弹出动画、主题色、来源徽章、✕ 关闭）。
8. **M8 — Desktop 联调**：§11.3 全链路验收。
