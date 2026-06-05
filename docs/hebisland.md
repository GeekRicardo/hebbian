# hebisland — 独立通知与审批岛设计

## 目标

`hebisland` 是 Hebbian 的独立通知器二进制：

- 可以被 Hebbian Desktop / heb CLI / hebweb 调用，也可以被用户直接从 CLI 调用。
- 每条通知是一个独立的无边框窗口，支持右上角或右下角堆叠。
- 支持自定义图标、主题、标题、描述、动作按钮。
- 支持把 Hebbian 的审批请求渲染成可选择的通知卡片，替代 Desktop 进程内现有的后台审批通知。
- 通知器只负责展示与采集用户决定，不直接持有 `agent_core`，审批决定回传给发起方 surface，由发起方落到 `agent_core`。

一句话定位：`hebisland` 是一个独立的本地 UI 外设，不是第四个 `agent_core` surface。

## 已确认设计决策

| 决策 | 结论 | 原因 |
|---|---|---|
| 二进制命名 | `hebisland` | 呼应 CodeIsland 的「岛」隐喻，同时与 `heb` / `hebweb` 同系列 |
| 技术栈 | macOS native（Swift + AppKit/SwiftUI） | Tauri 多窗口在 macOS 后台线程创建 webview 不可靠（白屏 / 透明空窗），改用 `NSPanel` 直接渲染 SwiftUI；详见 [hebisland-spec.md §0](hebisland-spec.md) |
| 通信方式 | Unix socket 双向 | 支持 CLI 单次推送，也支持审批决定回传；与 `heb` CLI 现有 IPC 方向一致 |
| 审批落点 | 决定回传，hebbian 落地 | `hebisland` 不碰 `agent_core`，避免职责重叠和状态分叉 |
| 多通知形态 | 每条通知一个无边框窗口 | 支持独立生命周期、独立动画、独立点击与审批 |

## 架构影响评估

### 1. 是否与架构.md 相悖？

不相悖，但需要在正式实现前补充架构文档。

现有 `docs/架构.md §7.5` 定义了三类持有 `agent_core` 的 surface：Desktop、heb CLI、hebweb。`hebisland` 不应该加入这张「持有 core」拓扑图；它是外部 UI companion，只通过本地 socket 与上述 surface 通信。

这避免违反「三个 surface 都是 in-process 复用 agent_core」的既定设计。

### 2. 是否符合既定设计？

符合。

- 工具 / 协议字段命名继续遵循 `§4.4.7`：外部 JSON 字段使用 `camelCase`，Rust 内部使用 `snake_case`。
- 审批仍由 surface 走现有 `approve_permission` / `IpcCommand::Allow` 等路径进入 `agent_core`，不绕过 HITL。
- 本地 socket 可以复用 `~/.hebbian/cli-sockets/` 的目录理念，但建议新建 `~/.hebbian/island.sock` 或 `~/.hebbian/island/<profile>.sock`，避免与 session socket 混淆。

### 3. 是否引入新设计 / 需修改架构.md？

正式实现时需要修改 `docs/架构.md`：

- `§4.5 HITL`：补充「审批通知可由外部 companion 展示，决定仍由原 surface resolve」。
- `§7.5 surface 拓扑`：新增旁路 companion 图，明确 `hebisland` 不持有 `agent_core`。
- `§6 Storage / 文件锁`：登记 socket 与配置落盘位置。
- `§13 设计决策表`：追加 `hebisland` 的职责边界决策。

本设计文档本身是方案草案，不修改 runtime 协议；后续进入实现计划时再同步架构文档。

### 4. 会影响哪些其他模块？

- `apps/desktop/src/notch.rs`：现有单窗口串行通知后续会被抽离或降级为 fallback。
- `apps/desktop/frontend/src/desktop/ui/components/NotchApp.tsx` / `NotificationCard.tsx`：视觉可迁移到 `hebisland` 前端。
- `apps/cli`：可新增 `heb notify` 或让 daemon 在审批事件出现时调用 `hebisland` socket。
- `apps/web-server`：可在后台事件出现时接入同一 notification bridge。
- `crates/protocol`：如需要跨进程共享通知 schema，可新增轻量协议类型；审批本体仍复用现有 `PermissionRequested` / `ApprovalDecision` 语义。

### 5. 修改取舍

最小改动方案：保留 Desktop 内 `notch.rs`，只新增 `hebisland` CLI 推送能力。优点是风险低；缺点是审批通知仍有两套 UI。

更彻底方案：新增 `hebisland` 后，让 Desktop / heb CLI / hebweb 的通知都统一走 socket，`notch.rs` 仅作为未启动 `hebisland` 时的 fallback。优点是体验一致、边界清楚；缺点是要处理 companion 进程发现、启动失败和 socket 断连。

推荐采用更彻底方案，但分阶段实施：先把 `hebisland` 做成可直接 CLI 调用的独立通知器，再接入 Hebbian 审批事件，最后替换 Desktop 后台审批通知。

## 现状与问题

当前 Desktop 内置的 `notch.rs` 有几个限制：

- 只有一个窗口，通知排队串行展示。
- pending 类型通知会替换当前通知，不适合多个审批请求并存。
- 绑定 Desktop 进程，heb CLI 和 hebweb 不能自然复用。
- 审批 UI 与通知生命周期混在 Desktop Tauri command 里，无法独立调用。

`hebisland` 的核心改进是：把「通知展示」从 Desktop surface 中拆出来，成为所有 surface 都能复用的独立 UI companion。

## 进程模型

```text
                    agent_core + ~/.hebbian/
                            │
       ┌────────────────────┼────────────────────┐
       │                    │                    │
  Desktop surface       heb daemon           hebweb server
  持有 agent_core       持有 agent_core       持有 agent_core
       │                    │                    │
       └──────────────┬─────┴──────────────┬─────┘
                      │                    │
              Unix socket 双向 IPC          │
                      │                    │
                 hebisland companion ◄─────┘
                 不持有 agent_core
                 只展示通知 / 回传用户选择
```

关键约束：

- `hebisland` 可以常驻，也可以被首次通知拉起。
- `hebisland` 不读写 session，不 resolve approval，不调用 `LocalCoreClient`。
- 发起通知的 surface 必须保存 `request_id` 与回调上下文。
- 用户点击审批按钮后，`hebisland` 只发回 `NotificationAction`，由发起 surface 转为 `ApprovalDecision`。

## 二进制与 CLI

```bash
# 启动常驻通知守护进程
hebisland daemon

# fire-and-forget 推送（不等待回传）
hebisland notify --msg '<json>'

# 推送并等待用户操作回传（阻塞直到收到 action 或超时）
hebisland notify --msg '<json>' --wait
hebisland notify --msg '<json>' --wait --timeout 30
```

实现位置：`apps/island-mac/`（独立 Swift Package）。
二进制入口：`Sources/HebIsland/main.swift`。
UI：SwiftUI 卡片（`CardView.swift`）经 `NSHostingView` 装进 `NSPanel`。

## Socket 协议

### 连接位置

建议：

```text
~/.hebbian/island.sock
```

如果未来要支持多配置或多用户 profile，可扩展为：

```text
~/.hebbian/island/default.sock
```

不要放进 `~/.hebbian/cli-sockets/<session>.sock`，那个目录表达的是「heb daemon session socket」。`hebisland` 是全局 companion，不属于某个 session。

### Framing

沿用 `heb` CLI 现有风格：一行一个 JSON，换行分隔。

```json
{"type":"show","notification":{...}}
{"type":"action","id":"n_01","action":"approve"}
{"type":"dismiss","id":"n_01","reason":"timeout"}
```

### 实际消息格式

```jsonc
// 调用方 → island
{"type":"show","id":"msg-001","card":{
  "id":"msg-001",
  "cardType":"approval",
  "title":"工具审批",
  "body":"Bash 想执行 rm -rf /tmp/test",
  "sessionId":"abc",
  "durationMs":0,            // 可选。0=常驻；>0=自动消失毫秒；省略=按 cardType 默认
  "actions":["拒绝","允许"]   // 可选。自定义按钮；省略=按 cardType 默认按钮
}}
{"type":"dismiss","id":"msg-001"}

// island → 调用方 (action 回传，仅限保持连接的调用方)
{"msg_id":"msg-001","action":"允许"}
```

### durationMs 与 actions

调用方可以通过 `durationMs` 精确控制消失时间（默认 info=5s, approval/question=0即常驻）。
通过 `actions` 自定义按钮列表，按钮名即是回传的 action 值。

- `hebisland notify --msg '...'`：fire-and-forget，写完即断开。
- `hebisland notify --msg '...' --wait`：保持连接，等首个 action 回传后打印 JSON 并退出。

详见 `docs/hebisland-spec.md` §2。

### Action 回传

```json
{"msg_id":"msg-001","action":"允许"}
```

`action` 值：
- 用户自定义 `actions` 数组中的按钮名（如 `"知道了"` / `"允许"` / `"拒绝"`）
- 卡片默认按钮：`"allow"` / `"deny"` / `"open"` / `"dismiss"`
- info 卡片超时自动消失：`"dismiss"`

发起 surface 收到后按需转为自己领域的动作（如 `allow` → `ApprovalDecision::AllowOnce`）。

### Dismiss

```json
{"type":"dismiss","id":"msg-001"}
```

由 surface 发送，island 收到后关闭对应窗口。

审批类通知被关闭时，不自动拒绝。发起 surface 应保持原审批挂起，并继续在主 UI 中可处理。

## 窗口堆叠规则

每条通知一个无边框窗口。窗口大小由前端内容测量后回传后端，再由后端设置真实窗口尺寸。

### 右上角

从旧到新，自上而下排列：

```text
┌──────────────┐
│ 旧通知        │
└──────────────┘
      gap
┌──────────────┐
│ 较新通知      │
└──────────────┘
      gap
┌──────────────┐
│ 最新通知      │
└──────────────┘
```

定位公式：

```text
x = monitor.right - margin - width
y = monitor.top + margin + sum(previous_heights + gap)
```

### 右下角

从旧到新，自下而上排列：

```text
┌──────────────┐
│ 最新通知      │
└──────────────┘
      gap
┌──────────────┐
│ 较新通知      │
└──────────────┘
      gap
┌──────────────┐
│ 旧通知        │
└──────────────┘
```

定位公式：

```text
x = monitor.right - margin - width
y = monitor.bottom - margin - sum(current_and_previous_heights + gap)
```

这样满足：右上角「旧 → 新」从上到下；右下角「旧 → 新」从下到上。

### 超出屏幕

第一版使用简单策略：

- 最多同时显示 5 条普通通知。
- 审批通知不被自动挤掉。
- 普通通知超出时，优先折叠最旧的普通通知，显示为「还有 N 条通知」。
- 如果全是审批通知且超出屏幕，只压缩卡片高度，不自动关闭。

## 视觉设计

整体风格：玻璃态 + 暗色半透明 + 细描边 + 柔和阴影。它应该像一个系统级悬浮卡片，而不是网页 toast。

### 卡片结构

```text
┌────────────────────────────────────┐
│ icon  标题                 source  │
│       描述文本，最多两行           │
│       detail / tool input 摘要      │
│       [拒绝]              [允许]   │
└────────────────────────────────────┘
```

### 尺寸

| 项 | 值 |
|---|---|
| 宽度 | 360px 默认，审批类 390px |
| 最小高度 | 92px |
| 最大高度 | 240px |
| 圆角 | 22px |
| 屏幕边距 | 24px |
| 卡片间距 | 12px |

### 主题

| theme | 用途 | 主色 |
|---|---|---|
| `neutral` | 普通提示 | slate |
| `info` | 信息 | blue |
| `success` | 成功 | emerald |
| `warning` | 审批 / 等待 | amber |
| `danger` | 错误 / 拒绝 | rose |

审批卡片默认 `warning`，允许按钮用 emerald，拒绝按钮用 translucent rose。

### 图标

第一版支持两类：

1. 内置图标名：`spark` / `terminal` / `check` / `warning` / `error` / `lock` / `tool`
2. 本地图片路径：只允许本机绝对路径或配置目录内相对路径，不拉远程图片，避免 SSRF 和隐私泄漏。

## 审批通知设计

审批卡片的信息分层：

1. 用户要决定什么：`Bash 需要你的允许`
2. 这次会做什么：`运行 cargo check --workspace`
3. 风险提示：按工具 effect 显示短标签，比如 `会执行本地命令` / `会修改文件` / `会访问网络`
4. 动作：`拒绝` / `允许`

示例：

```text
Bash 需要你的允许
运行：cargo check --workspace
会执行本地命令
[拒绝] [允许]
```

不在通知卡片里展示完整 JSON input。完整内容仍在 Hebbian 主 UI / CLI 事件流里看。

## 迁移状态

### 旧 Tauri 实现（已废弃，2026-06-04 删除）

- 旧 `apps/island`（Tauri 多窗口 + React）已删除，workspace 注册一并移除。
- 旧阶段 1（独立可用）/ 阶段 2（Desktop 接入）的 Rust + React 代码作废。
- 废弃原因：macOS 上从 socket 后台线程动态创建 webview 通知窗口，窗口框出现但内容永不加载（透明空窗 / 白屏），是架构性死路。详见 [hebisland-spec.md §0](hebisland-spec.md)。
- **保留不动**：`apps/desktop/src/hebisland_client.rs`（socket 客户端 + `hitl::resolve_hitl_from_island`）。它走 socket 协议、不依赖旧 crate，native 端按 [hebisland-spec.md §4](hebisland-spec.md) 对齐协议即可继续工作。

### native 实现路线（进行中）

- 新建 `apps/island-mac/`（Swift Package），实现见 [hebisland-spec.md](hebisland-spec.md)。
- 里程碑 M1–M8 见 [hebisland-spec.md §12](hebisland-spec.md)。
- 协议、socket 路径、action 回传值与旧实现保持兼容，Desktop 端零改动。

### 后续（native 可用后）

- 阶段 3：heb CLI / hebweb 在 `permission_requested` 事件出现时也发给 `hebisland`，三 surface 审批通知统一。
- 阶段 4：协议扩展支持 design.html 的进阶卡片（工具命令详情 / diff / 子命令勾选 / 问答选项 / 文本输入）。

## 错误处理

| 场景 | 行为 |
|---|---|
| `hebisland` 未启动 | 发起方尝试拉起；失败则走 fallback |
| socket 断连 | 当前通知窗口继续显示，但动作回传失败时提示「无法发送决定」 |
| 重复通知 ID | 后来的 `show` 视为 update，更新原窗口内容 |
| 审批 action 回传失败 | 不关闭窗口，提示用户回到主界面处理 |
| 图标路径不可读 | 使用 theme 默认图标 |
| 多屏 | 默认使用鼠标所在屏幕；发起方可指定 monitor |

## 安全边界

- `hebisland` 不执行 shell、不读 session、不访问远程 URL。
- 图标路径只读本地图片，且不允许 `http://` / `https://`。
- socket 只监听本机 Unix socket，文件权限限制为当前用户。
- `metadata` 只在 IPC 中透传，不渲染为 HTML。
- 通知内容作为文本渲染，不使用 `dangerouslySetInnerHTML`。

## 测试与验收

### 单元 / 集成测试

- socket JSON decode / encode。
- 堆叠位置计算：右上、右下、多高度、超屏。
- action 回传路由。
- 重复 ID update 行为。

### 手动验收

```bash
# 启动 daemon
cargo run -p hebisland -- daemon &

# 发送 info 通知（3s 自动消失）
cargo run -p hebisland -- notify --msg '{"type":"show","id":"test-1","card":{"id":"test-1","cardType":"info","title":"完成","body":"cargo check 已通过"}}'

# 发送审批通知（不自动消失，需手动操作）
cargo run -p hebisland -- notify --msg '{"type":"show","id":"test-2","card":{"id":"test-2","cardType":"approval","title":"需要审批","body":"Bash 想执行 cargo check","sessionId":"abc"}}'

# 发送问题通知
cargo run -p hebisland -- notify --msg '{"type":"show","id":"test-3","card":{"id":"test-3","cardType":"question","title":"有疑问","body":"请确认是否继续"}}'
```

视觉验收：

- 右上角旧消息在上，新消息在下。
- 多条通知不会互相遮挡。
- 审批通知不会自动消失。
- 点击按钮后窗口有明确反馈并关闭。

## 第一版不做

- 不做通知历史中心。
- 不做复杂规则编辑。
- 不做远程图片图标。
- 不做跨机器通知。
- 不做完整审批 JSON 展开编辑。
- 不替代 Hebbian 主 UI 的审批弹窗；只替代后台通知入口。

## HTML 样式示例

见 [hebisland-design.html](hebisland-design.html)。
