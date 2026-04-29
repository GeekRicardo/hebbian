# Hebbian — 给 agent 的项目导航

> 这份文件是给 Claude / 其他 agent 用的快速 onboarding。
> 完整架构设计见 [docs/architecture.md](docs/architecture.md)，那份是「目标态」；本文件描述「**今天仓库实际是什么样**」。

---

## 一句话定位

**Hebbian = Model + Harness**。本仓库是一个 Rust + Tauri + React 的 AI agent 框架，已分两个 surface（Desktop、CLI）+ 协议 crate + harness 核心，HITL 闭环已通。

---

## 仓库结构（实际现状）

```
hebbian/
├── apps/
│   ├── desktop/                 Tauri 桌面应用
│   │   └── src/
│   │       ├── lib.rs           Tauri 命令注册（IPC 入口）
│   │       ├── chat.rs          Harness 桥接 + AgentEvent → EngineEvent 翻译
│   │       ├── hitl.rs          ★ HITL 桥接：app 级 PendingApprovals state
│   │       ├── engine/mod.rs    EngineEvent（Tauri Channel 用）
│   │       ├── error.rs         AppError / AppResult
│   │       ├── title_gen.rs     会话标题自动生成
│   │       └── window_control.rs 窗口管理 / 全局快捷键
│   │
│   └── cli/                     ★ 终端 surface（loop / 单次 / JSON 多轮）
│       └── src/
│           ├── main.rs          入口、模式分派、ModelClient 构建
│           ├── session.rs       CliSession（包 agent_core::Session）+ CliObserver（TurnObserver 实现）
│           ├── render.rs        Event → 终端彩色输出（colored crate）
│           └── mock_provider.rs 无网络环境固定假回复
│
├── crates/
│   ├── protocol/                ★ 唯一被所有人依赖的协议 crate
│   │   └── src/
│   │       ├── ids.rs           RunId / TurnId / SubmissionId / PermissionRequestId / AgentRef
│   │       ├── submission.rs    Submission, Op, UserInput, TurnOverrides
│   │       ├── event.rs         Event, EventPayload, StopReason, RiskLevel
│   │       ├── permission.rs    ApprovalDecision, PermissionKind, PermissionScope
│   │       ├── context.rs       ContextPolicy, TokenBudget
│   │       └── error.rs         ErrorReport
│   │
│   ├── agent-core/              ★ 产品核心 / Harness
│   │   └── src/
│   │       ├── lib.rs           pub use Harness / RunHandle / Session / TurnObserver / TurnSummary
│   │       ├── harness.rs       Harness + RunHandle + TurnObserver trait + drive() 事件循环
│   │       ├── session.rs       Session（transcript / workspace / definition / client 容器）
│   │       ├── agent_loop.rs    turn/step 主循环；工具派发委托给 dispatch.rs
│   │       ├── dispatch.rs      ToolDispatcher（路径审批 / 工具审批 / ask / 执行 / emit）
│   │       ├── run_state.rs     RunState（per-run seq + turn 计数 + event 工厂）
│   │       ├── turn_context.rs  TurnContext（结构已立，loop 还在用 LoopParams）
│   │       ├── definition.rs    AgentDefinition / CompactionPolicy / PermissionPolicy
│   │       ├── tools/
│   │       │   ├── mod.rs       Tool trait + ToolClass + default_tools + ASK_TOOL_NAME
│   │       │   ├── registry.rs  ToolRegistry
│   │       │   └── hitl.rs      ★ HitlGate：审批/提问/路径/续跑共用一张 pending 表
│   │       ├── context/
│   │       │   ├── transcript.rs Transcript / from_session
│   │       │   ├── budget.rs    token 估算
│   │       │   └── compaction.rs 结构化裁剪（L3 LLM 摘要未实现）
│   │       ├── hooks/
│   │       │   ├── mod.rs       Hook trait + HookManager（骨架，等真正 hook 接入）
│   │       │   └── types.rs     HookPoint × 4（BeforeModelCall / OnPermissionCheck /
│   │       │                    OnToolResult / OnCompaction）
│   │       └── types.rs         protocol facade（向后兼容旧 import 路径）
│   │
│   ├── model-gateway/           ★ 统一模型访问
│   │   └── src/
│   │       ├── client.rs        ModelClient trait
│   │       ├── types.rs         ModelRequest / ModelResponse / Usage
│   │       ├── config.rs        Provider CRUD + 预设（providers.json）
│   │       ├── auth/
│   │       │   ├── mod.rs       OAuth 入口
│   │       │   └── refresh.rs   token 刷新
│   │       ├── discovery/       模型列表拉取
│   │       ├── health.rs        provider 健康检查
│   │       ├── protocols/       openai / anthropic / gemini wire format
│   │       └── providers/       3 个 ModelClient 实现
│   │
│   └── platform/                基础设施层（计划进一步拆分，见 docs §6）
│       └── src/
│           ├── error.rs         AppError / AppResult
│           ├── runtime.rs       CancelFlag（全局注册表，等 actor 化后挪走）
│           ├── attachments.rs   MessageAttachment
│           ├── storage/sessions.rs    ← 计划迁出到 crates/persistence
│           └── config/prompts.rs      ← 计划迁出到 crates/config
│
├── src/desktop/                 React 前端
│   ├── ui/
│   │   ├── components/
│   │   │   ├── ChatView.tsx     主对话视图
│   │   │   ├── ChatInput.tsx    输入框（含工具菜单）
│   │   │   ├── PermissionApprovalPopup.tsx  ★ HITL 审批弹窗（挂在 ChatInput 上方）
│   │   │   ├── UserQuestionPopup.tsx         ★ Ask 提问弹窗（选项 + 自由输入 + ESC 取消）
│   │   │   ├── MessageBubble.tsx
│   │   │   ├── Sidebar.tsx
│   │   │   └── ...
│   │   ├── store/useStore.ts    Zustand 全局状态（含 pendingApproval）
│   │   ├── lib/
│   │   └── types.ts             EngineEvent / PendingApproval / ApprovalDecisionPayload
│   └── bridge/tauri.ts          invoke 封装（含 approvePermission）
│
├── docs/
│   └── architecture.md          ★ 唯一权威架构文档（M1-M4 路线图）
│
├── Cargo.toml                   workspace 根
├── README.md
└── CLAUDE.md                    本文件
```

---

## 已实现 vs 未实现（M1 / M2 / M3 / M4）

完整路线图见 [docs/architecture.md §14-15](docs/architecture.md)。当前进度：

### M1 — Core API 收敛（**已完成 12/14**）

| # | 项 | 状态 |
|---|---|---|
| 1 | `crates/protocol` + `Submission/Op` | ✓ |
| 2 | per-run seq | ✓（`RunState::next_seq`） |
| 3 | `PermissionDecision` 三态 + oneshot waiter | ✓ |
| 4 | `RunHandle` 取代 RunId 反查（独享 mpsc + 控制方法） | ✓ |
| 5 | `Session` 上升为 agent-core 一等公民 | ✓ |
| 6 | `TurnObserver` trait + `RunHandle::drive` 接管事件循环 | ✓ |
| 7 | `TurnContext` 抽象 | ◐（结构已立，loop 还在用 `LoopParams`） |
| 8 | `Tool::classify` + `ToolClass` 自报分类 | ✓ |
| 9 | `HitlGate` 合并审批/提问/路径/续跑 | ✓ |
| 10 | `ToolDispatcher` 抽出（agent_loop 从 800→405 行） | ✓ |
| 11 | `LoopError` 分类型（Model / Cancelled / MaxIterations / Tool） | ✗ |
| 12 | Hook 缩减到 4 个能改 state 的拦截点 | ✓ |
| 13 | `ask` 改为普通 Tool（`NeedsHumanInput`） | ✗（仍在 dispatch 里特判 `ASK_TOOL_NAME`） |
| 14 | Desktop 审批 UI + Op 翻译层 | ✓（[PermissionApprovalPopup](src/desktop/ui/components/PermissionApprovalPopup.tsx)） |

### M2 / M3 / M4 全部未做
persistence / memory / observability / multi-agent / channels / server / sandbox / MCP — 见架构文档。
（注：CLI 已经是 TUI 的雏形，但 ratatui 全屏式 TUI 仍在 M3 范围。）

---

## HITL 完整数据流

```
[模型请求执行 destructive 工具]
       ↓
ToolDispatcher: hitl.check(tool_name, &class)
       ↓
HitlGate: ToolClass + policy + learned 三层判断
       ↓
  返回 NeedsApproval { request_id, waiter: oneshot::Receiver }
       ↓
dispatch.rs emit Event::PermissionRequested { request_id, kind, ... }
       ↓
       ├─→ run mpsc → RunHandle.recv() → surface
       │   └─→ surface 端 TurnObserver::on_permission_request 回调
       │         ├─ CLI: 返回 Some(AllowOnce)（auto_approve）
       │         └─ Desktop: state.track(request_id, hitl) + 返回 None
       │           ↓
       │           useStore.pendingApproval ← <PermissionApprovalPopup />
       │           ↓
       │           [用户点击按钮] → api.approvePermission(...)
       │           ↓
       │           Tauri command approve_permission
       │           ↓
       │           HitlState::resolve_approval → hitl.resolve(...)
       │           ↓
       │           oneshot::Sender 推送 → waiter 唤醒
       │
       └─→ harness.event_tx broadcast（debug / observability 用）
       ↓
ToolDispatcher 收到 ApprovalDecision，决定执行 / 跳过
emit Event::PermissionResolved
       ↓
run 结束后 surface 调 hitl_state.forget(&hitl) 清理映射
```

**关键文件**：
- TurnObserver / RunHandle：[crates/agent-core/src/harness.rs](crates/agent-core/src/harness.rs)
- HitlGate：[crates/agent-core/src/tools/hitl.rs](crates/agent-core/src/tools/hitl.rs)
- 派发 + 审批 emit：[crates/agent-core/src/dispatch.rs](crates/agent-core/src/dispatch.rs)
- CLI Observer：[apps/cli/src/session.rs](apps/cli/src/session.rs) 搜 `CliObserver`
- Desktop Observer：[apps/desktop/src/chat.rs](apps/desktop/src/chat.rs) 搜 `DesktopObserver`
- Desktop HITL 桥接：[apps/desktop/src/hitl.rs](apps/desktop/src/hitl.rs)
- Tauri 命令：[apps/desktop/src/lib.rs](apps/desktop/src/lib.rs) `approve_permission`
- 弹窗组件：[src/desktop/ui/components/PermissionApprovalPopup.tsx](src/desktop/ui/components/PermissionApprovalPopup.tsx)
- store action：[src/desktop/ui/store/useStore.ts](src/desktop/ui/store/useStore.ts) 搜 `pendingApproval`

---

## Harness API（本地调用：`Session` + `RunHandle` + `TurnObserver`）

```rust
// 1. 构造 Harness（持有 tools / hooks，跨 session 共享）
let harness = Arc::new(Harness::new(default_tools(workspace, &skill_dirs), HookManager::empty()));

// 2. 建一个 Session（持有 transcript / workspace / definition / client）
let mut session = Session::new(harness, SessionConfig {
    definition,
    workspace,
    client,
    enabled_tools,
    initial_transcript: Transcript::new(system_prompt),
});

// 3. 追加 user message → 起 run → 拿独享 handle
session.append_user(user_input, attachments);
let mut handle = session.run();          // 或 run_with(cancel) 接入外部 cancel

// 4. 实现 TurnObserver，让 driver 接管事件循环
struct MyObserver { /* 渲染状态 */ }
#[async_trait]
impl TurnObserver for MyObserver {
    fn on_event(&mut self, event: &Event) { /* 渲染 / 累积 */ }
    async fn on_permission_request(&mut self, _id, _kind, _summary)
        -> Option<ApprovalDecision> { Some(ApprovalDecision::AllowOnce) }
    async fn on_question(&mut self, _id, q, opts)
        -> Option<UserAnswer> { Some(ask_user(q, opts).await) }
}

let summary = handle.drive(&mut observer).await;
match summary.outcome {
    TurnOutcome::Done       => session.commit_assistant(text, vec![]),
    TurnOutcome::Failed(e)  => /* ... */,
    TurnOutcome::Cancelled  => /* ... */,
}
```

要点：

- **本地路径不再用 `RunId` 反查**。`spawn_run`/`session.run()` 返回 `RunHandle`，所有控制方法挂在它上：
  `handle.recv() / resolve_permission(id, d) / answer_question(id, a) / interrupt() / id() / hitl()`。
- **不需要先 subscribe 再 spawn**：`RunHandle` 自带独享 mpsc，事件按时间顺序到达，不需要按 `run_id` 过滤。
- **跨进程 / 多观察者** 才走 `harness.subscribe()` + `harness.submit(Op)`：broadcast 总线收所有 run 的事件，actor 处理 `Op::Approve / AnswerQuestion / Interrupt`。
- **`Harness` 不持有 `ModelClient`**：client 在 `Session` 内，多 session 多 provider 天然隔离。

---

## 怎么跑 / 怎么测

```bash
# Rust 编译检查
cargo check --workspace

# TS 类型检查
pnpm exec tsc --noEmit

# 桌面 dev 模式
pnpm tauri dev

# CLI 三种模式（默认 loop / 单次 / JSON 多轮）
cargo build -p hebbian-cli
./target/debug/hebbian-cli                                              # 默认 loop（rustyline）
./target/debug/hebbian-cli "你好"                                        # 单次 query
./target/debug/hebbian-cli --json '{"messages":[{"role":"user","content":"hi"}]}'  # JSON 多轮
./target/debug/hebbian-cli "你好" --mock                                 # 不调真实模型

# CLI 验证 tool call 流式渲染
./target/debug/hebbian-cli "搜一下 wikipedia" --tools web_search,web_fetch

# CLI 验证 ask 工具：agent 主动向用户提问（2-5 选项 + 自由输入框，ESC 取消）
./target/debug/hebbian-cli "用 ask 工具问我想去哪玩" --tools ask
```

CLI 与 desktop **共享同一个 data_dir**（macOS：`~/Library/Application Support/dev.ricardo.hebbian/`），desktop 配过的 provider / OAuth 凭据 CLI 直接复用。

---

## 协议（最不能漂的部分）

`Submission / Op`（外界 → core）和 `Event / EventPayload`（core → 外界）在 [crates/protocol](crates/protocol/src/) 内定义。**所有 surface 都基于这套通信**：

```rust
// 入
pub enum Op {
    StartRun, SendUserMessage, Approve, Interrupt,
    Subscribe, Compact, Rollback, Fork,
}

// 出
pub enum EventPayload {
    RunStarted / RunFinished / RunFailed / RunCancelled,
    TurnStarted / TurnFinished,
    TextDelta / TextDone / Reasoning,
    ToolCallDelta / ToolCallStarted / ToolCallFinished,
    PermissionRequested / PermissionResolved,    // HITL
    ContextCompacted, Log,
}
```

改协议前先想清楚兼容性。手动验证：跑 `hebbian-cli "你好" --mock` 看事件流是否完整。

---

## 层次边界（最重要）

| 层 | crate / 目录 | 职责 | 红线 |
|---|---|---|---|
| **Protocol** | `crates/protocol` | 数据类型，不放行为 | 不依赖其他业务 crate；只用基础 serde / uuid / chrono |
| **Agent Core** | `crates/agent-core` | run 生命周期、agent loop、tool、权限、context、hooks | 不 import Tauri；不直接拼 HTTP；不知道 OAuth |
| **Model Gateway** | `crates/model-gateway` | `ModelClient` trait、provider、protocol、auth/oauth | 不做 agent loop；不持有 agent 状态 |
| **Platform** | `crates/platform` | CancelFlag、attachments、error；**目前还混着 sessions/prompts，待迁出** | 不做业务逻辑（理想态） |
| **Surfaces** | `apps/desktop` `apps/cli` | 把输入翻译成 `Op`、订阅 `Event` 渲染 | **不实现 agent 本体** |

---

## 必须遵守的设计规则

1. **`providers` 和 `oauth` 必须在 `model-gateway/` 下**，绝不出现在 agent-core / apps 根下
2. **UI surface 不编排 agent**。Tauri command 是薄翻译层，业务逻辑全在 `agent_core::Session` / `RunHandle` / `TurnObserver`
3. **surface 端事件循环走 `RunHandle::drive(&mut observer)`**，不要自己写 `recv()` + filter + 终止判定
4. **HITL 走 `HitlGate` 一张表**：`open_approval` / `open_question` / `resolve` / `answer` / `cancel_all_pending`；新增审批类型在 `PermissionKind` 加 variant
5. **工具自报 `ToolClass`**：destructive 工具必须 override `Tool::classify`；ReadOnly 是默认值
6. **per-run seq**：永远从 `RunState::next_seq()` 取，绝不重新引入全局 `static AtomicU64`
7. **子 agent 上下文继承默认 `Isolated`**，显式 `InheritRecent` / `InheritSummary` 才能继承
8. **Hook 只在能改 state 的点位触发**（4 个：`BeforeModelCall / OnPermissionCheck / OnToolResult / OnCompaction`）；纯观察走 Event 流

---

## 明确禁止

- 把 provider / auth / oauth 逻辑放到 `agent-core` 里
- 在 React 组件或 Tauri command handler 里直接编排 agent 逻辑
- 在 `agent-core` 里直接 `use tauri::*` 或 `use reqwest::*`（reqwest 应只在 model-gateway 用）
- 给 `EventPayload` 加 surface-specific 字段（surface 信息应在 channel adapter 里）
- 自动「全局统一注入」记忆——必须按 agent 身份过滤后注入
- 一开始就做：plugin 系统、复杂 DAG、agent swarm、自动团队自组织

---

## 下一步可优先做的事（M1 收尾 + M2 起步）

1. **M1 #7 完结 TurnContext**：把 `agent_loop::LoopParams` 进一步合进 `TurnContext`
2. **M1 #11 `LoopError` 分类型**：把 `Cancelled` / `MaxIterations` 从 `ModelError::Other` 拆出来
3. **M1 #13 `ask` 改为普通 Tool**：让 `Tool::classify` 返回 `NeedsHumanInput`，dispatch 不再特判 `ASK_TOOL_NAME`
4. **`ToolResult.outcome` 拆 Ok / Denied / Failed**：当前错误以字符串塞 content，UI 没法染色，统计层算不出成功率
5. **prompt cache 边界**：`ContextSnapshot` 三段切分（STABLE / SEMI / MUTABLE）+ provider 层 `cache_control`
6. **`MAX_TOOL_RESULT_INLINE` 改 BlobStore**：长 tool 输出（如 web_fetch 整页）落 blob，transcript 只放 preview + ref
7. **拆 platform**：`storage/sessions.rs` → `crates/persistence`；`config/prompts.rs` → `crates/config`
8. **修 model-gateway 的 stream 路径 usage 统计**：当前 `RunFinished.total_*_tokens` 在真实 provider 下显示 0
9. **`mpsc::unbounded` 加上限**：surface 慢消费时 buffer 会无限涨，换成 `bounded(1024)` + 满了丢非关键事件

---

## 给后续 agent 的提醒

- **改协议前先跑一遍三种 CLI 模式**：`hebbian-cli "..." --mock` / `hebbian-cli --json '...' --mock` / `hebbian-cli --mock`（loop），看事件流是否完整
- **agent-core 改完先 `cargo check -p agent-core --tests`**：测试已存在并会被 cargo 检查
- **desktop 改完跑 `cargo check -p hebbian` 和 `pnpm exec tsc --noEmit`**
- **不要重新生成已有文件**：先 Read，按需 Edit；尤其 `chat.rs` 已经 1000+ 行，重写代价很大
- **CLI 可以做端到端验证**，比启动 `pnpm tauri dev` 快得多
- **加新 EventPayload 变体后**：同步更新 [src/desktop/ui/types.ts](src/desktop/ui/types.ts) 的 `EngineEvent` union、[chat.rs](apps/desktop/src/chat.rs) 的 `agent_event_to_engine_event` 映射、[apps/cli/src/render.rs](apps/cli/src/render.rs) 的 `TurnRenderer::on_event` 渲染逻辑——三处任一漏改都会导致信息丢失
- **HITL 协议入口**：审批 / 提问 / 路径越界 / 长 run 续跑都走同一个 [HitlGate](crates/agent-core/src/tools/hitl.rs)。审批用 `open_approval` + `resolve`，提问用 `open_question` + `answer`，surface 端两条 Tauri 命令分别叫 `approve_permission` / `answer_question`。新增需要 HITL 的协议时按这两条路径中哪条更贴合选。
- **TurnObserver 是 surface 的标准接入点**：实现三个回调（`on_event` / `on_permission_request` / `on_question`），在 [harness.rs](crates/agent-core/src/harness.rs) 找 `TurnObserver` trait。本地 surface 在 `on_*` 里返回 `Some(decision)` 让 driver 自动 resolve，远端 / 异步链路返回 `None` 自己处理。

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
