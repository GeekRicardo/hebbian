# 内置浏览器 CDP 能力 spec（注释功能补全：眼睛 + 手 + 类意识）

> 状态：M1 实施中。锚定架构.md §8.5 / §13（决策 8.5-2 的"视觉回传推迟"在此解除）。
> 前置验证：CEF × Tauri PoC 三阶段全绿（见项目记忆 cef-embed-poc，产物 ~/code/ricardo/rust/cef-poc/）。

## 0. 原始目标（用户三痛点）

注释功能（元素圈选 + 旁支样式助手）当前不可用于复杂场景，根因三个：

| # | 痛点 | 根因 | 解法 |
|---|------|------|------|
| P1 | 解不了复杂样式问题 | 模型瞎调：工具是单向信号，无视觉/规则反馈；WKWebView 无 CDP，查不到 matched rules | CEF 承载 → CDP `CSS.getMatchedStylesForNode` + 截图回传 |
| P2 | 不能自主操作元素 | target 只能 @N（圈选元素），操作后无回报 | target 支持任意 CSS selector；操作后回传 DOM/样式真实状态 |
| P3 | 不理解元素关系、「改一个」应是「改一类」 | prompt 零引导 + 工具无批量手段 | selector 批量应用 + prompt 意图泛化引导 + 提交总结要求改共享组件 |

## 1. 终态架构

```
旁支会话 LLM
  │ 工具调用（不再是纯信号——经 bridge 拿真实回执）
  ▼
agent-core: PreviewBridge trait（async，agent-core 不碰 webview/tauri 的边界由它维持）
  │ SessionConfig.preview_bridge: Option<Arc<dyn PreviewBridge>>
  ▼
desktop: CdpBridge（实现 trait）── WebSocket ──▶ CEF 实例 CDP 端口
                                                   │
                                  Tauri 窗口 tab 内 CEF 子视图（PoC 阶段3 形态）
```

- **CEF 承载**：feature `cef-preview`。开 = 预览区用 CEF 子视图（`set_as_child` 挂 Tauri 主窗，`external_message_pump` + `RunEvent::MainEventsCleared` 泵）；关 = 现状 wry 路径原样保留（渐进迁移，不破坏现有功能）。
- **CDP 通道**：CEF 启动带 `remote_debugging_port`（仅 127.0.0.1）。desktop 内薄 CDP 客户端（tokio-tungstenite），只封装用到的 domain：Page（截图/注入）、DOM、CSS、Runtime、Input。
- **inspector.js 在 CEF 模式**：注入走 `Page.addScriptToEvaluateOnNewDocument`，上行走 `Runtime.addBinding`（替代 wry 导航拦截），下行走 `Runtime.evaluate`（替代 eval）。圈选交互逻辑不变。

## 2. 工具面（旁支会话）

| 工具 | 变化 | 输入 | 回执（经 bridge） |
|------|------|------|------|
| PreviewStyle | 升级 | prop, value, target(@N **或 selector**), allMatches | 应用元素数 + 首元素应用后 computed value |
| PreviewMutate | 不变（M1） | 同现状 | 同现状（信号） |
| PreviewAct | 升级 | action, target(@N **或 selector**), … | 操作后焦点元素/URL/可见 DOM 变化摘要 |
| **PreviewCapture** | 新增 | 无参 / selector（局部截图） | PNG → 多模态图片注入下一轮（vision_bridge 复用） |
| **PreviewInspect** | 新增 | target(@N 或 selector), what: styles\|rules\|tree\|siblings | matched CSS rules（含来源 selector/specificity）/ 子树结构 / 同构兄弟分析 |

无 bridge（feature off / CDP 断连）时：Style/Mutate/Act 回退现状信号语义；Capture/Inspect 返回「当前预览不支持」。

## 3. Prompt 面

- `aside_system_prompt` 重写：新增「意图泛化」节（圈选元素有同构兄弟时默认按一类处理，用 selector 批量），「先看再改」节（复杂问题先 PreviewInspect rules / PreviewCapture，再动手）。
- 单条/批量提交总结 prompt：新增「重复结构改共享组件/类，禁止单实例特例（:nth-child 之类）」要求。

## 4. 里程碑

- **M1（本次）**：能力层全量——PreviewBridge trait（读路径：截图/matched rules/eval）+ PreviewCapture/PreviewInspect 新工具 + PreviewStyle/Act 升级 selector target（写路径仍走 inspector 信号通道，diff 账本不变）+ desktop 薄 CDP 客户端 + prompt 重写。CDP 端点 M1 用 attach 模式（`HEBBIAN_PREVIEW_CDP=<port>` 连任意 CDP 浏览器，加载同一预览 URL）——能力层与承载解耦，今天即可端到端验证。
- **M2**：CEF 承载进 hebbian（PoC 已验三阶段，剩 dev 模式 helper bundle 结构这一个未验证项：裸二进制 + `browser_subprocess_path`/`framework_dir_path` 显式指定，需单独 PoC）；popout 迁移；打包链。
- **M3**：移除 wry 预览路径（CEF 稳定后）。

**M1 写/读分工的设计理由**：改动（style/mutate/act）继续走 inspector 信号通道——它持有 diff 账本（styleDiff/structuralChanges），提交到主对话的精确性依赖它；观察（截图/规则/DOM 状态）走 CDP——这是 wry 给不了的。两通道作用同一页面实例（M2 起 CEF 即预览本体；M1 attach 模式下是镜像实例，仅用于验证）。

## 5. 设计影响评估（CLAUDE.md 5 问）

1. **与架构.md 相悖？** §8.5 决策 8.5-2 写明截图回传"推迟单独立项，需先抽截图通道 async trait"——本 spec 就是该立项，PreviewBridge 即该 trait。agent-core 不依赖 tauri/reqwest 的红线不破（trait 在 agent-core，实现在 desktop；CDP 用 tokio-tungstenite 在 desktop 侧）。
2. **符合既定设计？** 工具 PascalCase / 参数 camelCase；信号工具机制 B 保留为降级路径。
3. **需改架构.md？** 是：§8.5 增补 PreviewBridge 与 CEF 承载双轨；§13 加决策行。随 M1 一并改。
4. **影响模块**：agent-core tools/session config（additive）、desktop browser/、chat.rs send_aside（additive）、前端工具卡片渲染（新工具名显示，TS 不强依赖）。协议 EventPayload 不动。
5. **取舍**：体积 +~180MB（仅 feature on）；CEF 双轨期维护两套承载——以 feature gate 控制爆炸半径，M3 收敛。
