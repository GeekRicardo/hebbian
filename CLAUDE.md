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
│   └── cli/                     ★ Rust CLI / 协议 harness（mock + 真实 provider）
│       └── src/
│           ├── main.rs          run / interactive 子命令
│           └── mock_provider.rs 确定性 mock client
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
│   │       ├── lib.rs           pub use Harness, RunState, TurnContext
│   │       ├── harness.rs       Harness（统一 spawn_run + subscribe + submit 风格）
│   │       ├── agent_loop.rs    主循环（HITL waiter / 工具并发 / 10 个 hook 触发点）
│   │       ├── run_state.rs     RunState（per-run seq + turn 计数 + event 工厂）
│   │       ├── turn_context.rs  TurnContext（结构已立，loop 还在用 LoopParams）
│   │       ├── definition.rs    AgentDefinition / CompactionPolicy / PermissionPolicy
│   │       ├── tools/
│   │       │   ├── mod.rs       Tool trait + default_tools()（web_search / web_fetch）
│   │       │   ├── registry.rs  ToolRegistry
│   │       │   └── permissions.rs PermissionGate（三态 + oneshot waiter + learned rules）
│   │       ├── context/
│   │       │   ├── transcript.rs Transcript / from_session
│   │       │   ├── budget.rs    token 估算
│   │       │   └── compaction.rs 结构化裁剪（L3 LLM 摘要未实现）
│   │       ├── hooks/
│   │       │   ├── mod.rs       Hook trait + HookManager
│   │       │   └── types.rs     HookPoint × 10（BeforeRun/AfterRun/BeforeTurn/AfterTurn/
│   │       │                    BeforeModelCall/AfterModelCall/BeforeToolCall/AfterToolCall/
│   │       │                    BeforePermissionRequest/OnContextCompaction）
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
│   │   │   ├── MessageBubble.tsx
│   │   │   ├── Sidebar.tsx
│   │   │   └── ...
│   │   ├── store/useStore.ts    Zustand 全局状态（含 pendingApproval）
│   │   ├── lib/
│   │   └── types.ts             EngineEvent / PendingApproval / ApprovalDecisionPayload
│   └── bridge/tauri.ts          invoke 封装（含 approvePermission）
│
├── scripts/
│   └── test.py                  ★ Python 协议验证器（4 个用例，跑 hebbian-cli）
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

### M1 — Core 闭环可用 + HITL（**已完成 7/9**）

| # | 项 | 状态 |
|---|---|---|
| 1 | `crates/protocol` + `Submission/Op` | ✓ |
| 2 | per-run seq | ✓（`RunState::next_seq`） |
| 3 | `PermissionDecision` 三态 | ✓ |
| 4 | oneshot waiter HITL 通路 | ✓ |
| 5 | Harness `spawn_run` + `subscribe` actor 风格 | ✓（旧 `run()` / `run_with_gate` 已删除，统一新 API） |
| 6 | `TurnContext` 抽象 | ◐（结构已立，loop 还在用 `LoopParams`） |
| 7 | 10 个 hook 点位 | ✓ |
| 8 | Tool trait `classify` / `ToolCtx` / `ToolResult` | ✗ |
| 9 | Desktop 审批 UI + Op 翻译层 | ✓（[PermissionApprovalPopup](src/desktop/ui/components/PermissionApprovalPopup.tsx)） |

### M2 / M3 / M4 全部未做
persistence / memory / observability / multi-agent / channels / TUI / server / sandbox / MCP — 见架构文档。

---

## HITL 完整数据流（重点，刚完成）

```
[模型请求执行 destructive 工具]
       ↓
agent_loop: gate.check(tool_name, input)
       ↓
PermissionGate: 三态判断
       ↓
  返回 NeedsApproval { request_id, waiter: oneshot::Receiver }
       ↓
agent_loop emit Event::PermissionRequested { request_id, kind, ... }
       ↓
       ├─→ harness.event_tx broadcast → CLI 订阅者 / 测试
       │
       └─→ chat.rs 的 on_event 回调
              ↓
              ① 把 (request_id → gate_arc) 注册进 Tauri State<HitlState>
              ② 通过 Channel<EngineEvent> 转发 PermissionRequested 给前端
                     ↓
                     useStore: set pendingApproval = { requestId, toolName, ... }
                     ↓
                     <PermissionApprovalPopup /> 渲染（挂在 ChatInput 上方）
                     ↓
                     [用户点击按钮]
                     ↓
                     resolveApproval({ kind: "allow_once" | ... })
                     ↓
                     api.approvePermission(requestId, decision, feedback?)
                     ↓
                     Tauri command approve_permission
                     ↓
                     HitlState::resolve(request_id, ApprovalDecision)
                     ↓
                     gate.resolve(request_id, decision, None)
                     ↓
                     oneshot::Sender 推送 → waiter 唤醒
       ↓
agent_loop 收到 ApprovalDecision，决定执行 / 跳过工具
agent_loop emit Event::PermissionResolved
       ↓
chat.rs: hitl.unregister_gate（清理）
前端: pendingApproval = null（关闭弹窗）
```

**关键文件**：
- 后端订阅 loop：[apps/desktop/src/chat.rs](apps/desktop/src/chat.rs) `send_and_save_in_data_dir_with_client_factory`（搜 `spawn_run`）
- HITL 桥接：[apps/desktop/src/hitl.rs](apps/desktop/src/hitl.rs)
- Tauri 命令：[apps/desktop/src/lib.rs](apps/desktop/src/lib.rs) `approve_permission`
- Gate 三态：[crates/agent-core/src/tools/permissions.rs](crates/agent-core/src/tools/permissions.rs)
- 弹窗组件：[src/desktop/ui/components/PermissionApprovalPopup.tsx](src/desktop/ui/components/PermissionApprovalPopup.tsx)
- store action：[src/desktop/ui/store/useStore.ts](src/desktop/ui/store/useStore.ts) 搜 `pendingApproval`

---

## Harness API（surface 与 core 唯一接口）

```rust
// 1. 构造（只传 tools / hooks，不绑定 client）
let harness = Harness::new(default_tools(), HookManager::empty());

// 2. ★ 必须先订阅再 spawn，否则丢失 RunStarted
let mut events = harness.subscribe();

// 3. 启动 run（异步，立刻返回 RunId）
let run_id = harness.spawn_run(
    client,                              // 本次 run 用的 ModelClient
    RunParams {
        agent: AgentRef::new("default"),
        gate: Arc::new(PermissionGate::new(policy)),  // 持有 Arc 以便外部 resolve
        transcript,                                    // 已组装好的 Transcript
        enabled_tools, compaction_policy,
        stream: true, cancel, parent: None,
    },
);

// 4. 消费事件流直到 run 终止
while let Ok(event) = events.recv().await {
    if event.run_id != run_id { continue; }   // 多 run 共享一个 broadcast
    match event.payload {
        EventPayload::RunFinished { .. } => break,
        EventPayload::RunFailed { .. }   => break,
        EventPayload::RunCancelled       => break,
        _ => { /* 累积 / 转发 */ }
    }
}

// 控制指令（HITL / 中断）
harness.resolve_permission(&run_id, &request_id, decision)?;
harness.interrupt(&run_id)?;
harness.submit(Submission::new(Op::Approve { .. }))?;  // 协议化路径
```

`Harness` 不持有 `ModelClient`，每次 `spawn_run` 显式传 client——这样 desktop 的多 session 多 provider 场景天然支持。

---

## 怎么跑 / 怎么测

```bash
# Rust 编译检查
cargo check --workspace

# TS 类型检查
pnpm exec tsc --noEmit

# 协议端到端验证（4 用例：seq 单调 / Run/Turn 配对 / TextDelta 累加 / HITL 时序）
cargo build -p hebbian-cli && python3 scripts/test.py

# 桌面 dev 模式
pnpm tauri dev

# CLI 快速验证（mock，无需 API key）
./target/debug/hebbian-cli run "你好" --mock
./target/debug/hebbian-cli run "用工具" --mock --mock-tool-call --mock-needs-approval

# CLI 真实跑（自动用 desktop 的 default provider）
./target/debug/hebbian-cli run "你好"
```

CLI 与 desktop **共享同一个 data_dir**（macOS：`~/Library/Application Support/dev.ricardo.hebbian/`）。

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

改协议前先想清楚兼容性。`scripts/test.py` 会断言协议不变量。

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
2. **UI surface 不编排 agent**。Tauri command 是薄翻译层，把请求转成 `Op` 或直接调 `Harness::run_with_gate`
3. **输出只消费 Event 流**：`AgentEvent → EngineEvent` 是 chat.rs 的唯一职责
4. **子 agent 上下文继承默认 `Isolated`**，显式 `InheritRecent` / `InheritSummary` 才能继承
5. **HITL 走 oneshot waiter**，不能用 sleep 轮询；新增审批类型在 `PermissionKind` 加 variant
6. **压缩 / 记忆走 hook 机制**，不硬编码进 loop
7. **per-run seq**：永远从 `RunState::next_seq()` 取，绝不重新引入全局 `static AtomicU64`

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

1. **M1 #6 完结 TurnContext**：把 `agent_loop::LoopParams` 进一步合进 `TurnContext`
2. **M1 #8 Tool trait 升级**：加 `classify(&self, input)` 方法（默认 ReadOnly），HITL 默认放行 ReadOnly 工具
3. **M2 #10 拆 platform**：`crates/platform/src/storage/sessions.rs` → `crates/persistence`；`crates/platform/src/config/prompts.rs` → `crates/config`
4. **M2 #15 BlobStore**：长 tool 输出（如 web_fetch 整页）落 blob，transcript 只放 preview + ref
5. **修 model-gateway 的 stream 路径 usage 统计**：当前 `RunFinished.total_*_tokens` 在真实 provider 下显示 0

---

## 给后续 agent 的提醒

- **改协议前先跑 `python3 scripts/test.py`**，把不变量打出来再改
- **agent-core 改完先 `cargo check -p agent-core --tests`**：测试已存在并会被 cargo 检查
- **desktop 改完跑 `cargo check -p hebbian` 和 `pnpm exec tsc --noEmit`**
- **不要重新生成已有文件**：先 Read，按需 Edit；尤其 `chat.rs` 已经 990+ 行，重写代价很大
- **CLI 可以做端到端验证**，比启动 `pnpm tauri dev` 快得多
- **加新 EventPayload 变体后**：同步更新 [src/desktop/ui/types.ts](src/desktop/ui/types.ts) 的 `EngineEvent` union 与 [chat.rs](apps/desktop/src/chat.rs) 的 `agent_event_to_engine_event` 映射，否则前端拿不到

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
