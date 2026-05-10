# Hebbian — 给 agent 的项目导航

> 这份文件是给 Claude / 其他 agent 用的快速 onboarding。
> 完整架构设计见 [docs/architecture.md](docs/architecture.md)，那份是「目标态」；本文件描述「**今天仓库实际是什么样**」。

---

## 一句话定位

**Hebbian = Model + Harness**。本仓库是一个 Rust + Tauri + React 的 AI agent 框架，已分两个 surface（Desktop、CLI）+ 协议 crate + harness 核心 + 观测层，HITL 闭环已通，事件流可 jsonl 落盘可重放。

---

## 仓库结构（实际现状）

```
hebbian/
├── apps/
│   ├── desktop/             Tauri 桌面应用（lib.rs Tauri 命令 / chat.rs Harness 桥接 / hitl.rs 桥接）
│   └── cli/                 终端 surface：loop / 单次 / JSON 多轮 / mock
│
├── crates/
│   ├── protocol/            协议唯一锚点：Submission/Op/Event/EventPayload/PermissionKind/...
│   ├── agent-core/          产品核心：Harness / Session / RunHandle / TurnObserver / agent_loop /
│   │                        dispatch / recorder / model_io_dump / system_prompt / workspace /
│   │                        context（含 microcompact + compact_with_llm）/ hooks /
│   │                        tools（含 ask + Bash/Read/Write/Grep/Skill + web_search/web_fetch）
│   ├── model-gateway/       ModelClient trait + 4 provider（openai/anthropic/gemini/deepseek）+
│   │                        InstrumentedClient（自动 span/metrics）+ context_window + OAuth
│   ├── observability/       tracing init + OTLP exporter + GenAI 语义属性 + 业务 metrics 工厂
│   └── platform/            CancelFlag / attachments / reasoning / error；
│                            还混着 storage/sessions、config/prompts、config/settings（计划迁出）
│
├── src/desktop/             React 前端：ChatView / ChatInput / PermissionApprovalPopup /
│                            UserQuestionPopup / Zustand store / EngineEvent types
│
├── docs/                    architecture.md（架构 + 路线图）/ compaction.md / hebbian-harness-detailed-design.md /
│                            todos.md（功能 todo + 代码漂移点）
└── CLAUDE.md                本文件
```

详细到文件级别的目录树见 [docs/architecture.md §1](docs/architecture.md)。

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
| 7 | `TurnContext` 抽象 | ◐（结构 / lib.rs 暴露已立，但**无人 import**，loop 仍用 `LoopParams`） |
| 8 | `Tool::classify` + `ToolClass` 自报分类 | ✓（含 `NeedsHumanInput { kind: HumanInputKind }`） |
| 9 | `HitlGate` 合并审批/提问/路径 | ✓（续跑 `ContinueLongRun` PermissionKind 已加但**没人发**） |
| 10 | `ToolDispatcher` 抽出 | ✓（agent_loop ~525 行，dispatch ~630 行） |
| 11 | `LoopError` 分类型 | ◐（`ModelError::Cancelled` 已拆，`MaxIterations` 仍用 `Other(String)`） |
| 12 | Hook 缩减到 4 个能改 state 的拦截点 | ✓ |
| 13 | `ask` 改为普通 Tool | ✗（`ToolClass::NeedsHumanInput` 已有，但 dispatch 仍按 `ASK_TOOL_NAME` 特判走 `spawn_ask`） |
| 14 | Desktop 审批 UI + Op 翻译层 | ✓（[PermissionApprovalPopup](src/desktop/ui/components/PermissionApprovalPopup.tsx)） |

### M2 / M3 / M4 — 详见 [架构文档 §14-15](docs/architecture.md)

要点：
- **M2 部分完成**：事件 jsonl 落盘（[`Recorder`](crates/agent-core/src/recorder.rs)，CLI 默认开 / desktop 未接）、Session 持久化、Marker 体系、Workspace 三层目录、microcompact、LLM 摘要式 compact、OTLP 导出（[`crates/observability`](crates/observability/)）已就绪；BlobStore / Memory 注入仍未做。
- **M3 / M4 全部未做**：multi-agent / channels / server / sandbox / MCP / 全屏式 TUI。

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
       run mpsc → RunHandle.recv() → driver.drive() → TurnObserver
            ├─ CLI: 返回 Some(AllowOnce)（auto_approve）
            └─ Desktop: state.track(request_id, hitl) + 返回 None
              ↓
              useStore.pendingApproval ← <PermissionApprovalPopup />
              ↓
              [用户点击按钮] → api.approvePermission(...)
              ↓
              Tauri command approve_permission
              ↓
              HitlState::resolve_approval → hitl.resolve(...)
              ↓
              oneshot::Sender 推送 → waiter 唤醒
       ↓
ToolDispatcher 收到 ApprovalDecision，决定执行 / 跳过
emit Event::PermissionResolved
       ↓
run 结束后 surface 调 hitl_state.forget(&hitl) 清理映射
```

⚠️ **目前没有 broadcast 总线**：`Harness::subscribe()` 不存在，事件**只**通过 `RunHandle` 自带的独享 mpsc 出。`Op::Subscribe` 协议已经定义但 actor 不处理（落到 debug log）。跨进程多观察者需要后续接入。

**关键文件**：
- TurnObserver / RunHandle：[crates/agent-core/src/harness.rs](crates/agent-core/src/harness.rs)
- HitlGate：[crates/agent-core/src/tools/hitl.rs](crates/agent-core/src/tools/hitl.rs)
- 派发 + 审批 emit：[crates/agent-core/src/dispatch.rs](crates/agent-core/src/dispatch.rs)
- CLI Observer：[apps/cli/src/session.rs](apps/cli/src/session.rs) 搜 `CliObserver`
- Desktop Observer：[apps/desktop/src/chat.rs](apps/desktop/src/chat.rs) 搜 `DesktopObserver`
- Desktop HITL 桥接：[apps/desktop/src/hitl.rs](apps/desktop/src/hitl.rs)
- Tauri 命令：[apps/desktop/src/lib.rs](apps/desktop/src/lib.rs) `approve_permission` / `answer_question` / `approve_path_access`
- 弹窗组件：[src/desktop/ui/components/PermissionApprovalPopup.tsx](src/desktop/ui/components/PermissionApprovalPopup.tsx)
- store action：[src/desktop/ui/store/useStore.ts](src/desktop/ui/store/useStore.ts) 搜 `pendingApproval`

---

## Harness API（本地调用：`Session` + `RunHandle` + `TurnObserver`）

```rust
// 1. 构造 Harness（持有 tools / hooks，跨 session 共享）
let harness = Arc::new(Harness::new(default_tools(workspace, &skill_dirs), HookManager::empty()));

// 2. 建一个 Session（持有 transcript / workspace / definition / client / 可选 recorder）
let mut session = Session::new(harness, SessionConfig {
    definition,
    workspace,
    client,
    enabled_tools,
    initial_transcript: Transcript::new(system_prompt),
    recorder: Some(Recorder::open(&path).await?),  // 可选事件落盘
});

// 3. 追加 user message → 起 run → 拿独享 handle
session.append_user(user_input, attachments);
let mut handle = session.run();          // 或 run_with(cancel) 接入外部 cancel

// 4. 实现 TurnObserver，让 driver 接管事件循环
#[async_trait]
impl TurnObserver for MyObserver {
    fn on_event(&mut self, event: &Event) { /* 渲染 / 累积 */ }
    async fn on_permission_request(&mut self, _id, _kind, _summary)
        -> Option<ApprovalDecision> { Some(ApprovalDecision::AllowOnce) }
    async fn on_question(&mut self, _id, q, opts, multi)
        -> Option<UserAnswer> { Some(ask_user(q, opts, multi).await) }
}

let summary = handle.drive(&mut observer).await;
match summary.outcome {
    TurnOutcome::Done       => {
        session.commit_assistant(text, vec![]);
        // summary.usage 含本轮 input/output/cache_read/cache_creation tokens
    }
    TurnOutcome::Failed(e)  => /* ... */,
    TurnOutcome::Cancelled  => /* ... */,
}
```

要点：

- **本地路径不再用 `RunId` 反查**。`spawn_run`/`session.run()` 返回 `RunHandle`：
  `handle.recv() / resolve_permission(id, d) / answer_question(id, a) / interrupt() / id() / hitl()`。
- **不需要先 subscribe 再 spawn**：`RunHandle` 自带独享 mpsc，事件按时间顺序到达，不需要按 `run_id` 过滤。
- **跨进程协议入口走 `harness.submit(Op)`**：actor 处理 `Op::Approve / AnswerQuestion / Interrupt`；其他 Op（含 `Subscribe / Compact / Rollback / Fork / StartRun / SendUserMessage`）当前**不处理**，留给 surface 自行解析后调本地 API。
- **`Harness` 不持有 `ModelClient`**：client 在 `Session` 内，多 session 多 provider 天然隔离。
- **`TurnSummary.usage`**：driver 在 `RunFinished` 时填入本轮累计 token，surface 端可直接累加进 session 文件（desktop chat.rs 已用此累加 `token_stats`）。

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
./target/debug/hebbian-cli                                              # 默认 loop（rustyline，含 [token%] 进度提示）
./target/debug/hebbian-cli "你好"                                        # 单次 query
./target/debug/hebbian-cli --json '{"messages":[{"role":"user","content":"hi"}]}'  # JSON 多轮
./target/debug/hebbian-cli "你好" --mock                                 # 不调真实模型

# CLI 验证 tool call 流式渲染
./target/debug/hebbian-cli "搜一下 wikipedia" --tools web_search,web_fetch

# CLI 验证 ask 工具（agent 主动提问，2-5 选项 + 自由输入框，ESC 取消）
./target/debug/hebbian-cli "用 ask 工具问我想去哪玩" --tools ask

# CLI 推理控制
./target/debug/hebbian-cli "..." --thinking --effort extra --long-context

# CLI 内置 slash command：/compact [指令] 主动压缩 · /exit /quit /q 退出

# CLI 管理 provider
./target/debug/hebbian-cli --providers list
./target/debug/hebbian-cli --provider set openai/gpt-5

# 事件落盘（默认开）：data_dir/sessions/rollout-<ts>-<uuid>.jsonl
# --no-record 关闭；--auto-approve / --allowed-dir / --data-dir 见 --help
```

CLI 与 desktop **共享同一个 data_dir**（macOS：`~/Library/Application Support/dev.ricardo.hebbian/`），desktop 配过的 provider / OAuth 凭据 CLI 直接复用。

OTLP 导出：设 `OTEL_EXPORTER_OTLP_ENDPOINT=http://...` 后 desktop / CLI 自动批量导出 trace + metrics；不设则只装 stderr 日志。

---

## 协议（最不能漂的部分）

`Submission / Op`（外界 → core）和 `Event / EventPayload`（core → 外界）在 [crates/protocol](crates/protocol/src/) 内定义。**所有 surface 都基于这套通信**：

```rust
// 入
pub enum Op {
    StartRun, SendUserMessage, Approve, AnswerQuestion, Interrupt,
    Subscribe, Compact, Rollback, Fork,
}

// 出
pub enum EventPayload {
    RunStarted / RunFinished / RunFailed / RunCancelled,
    TurnStarted / TurnFinished,
    TextDelta / TextDone / Reasoning,
    ToolCallDelta / ToolCallStarted / ToolCallFinished,
    PermissionRequested / PermissionResolved,        // HITL 审批
    UserQuestionRequested / UserQuestionAnswered,    // HITL 提问（ask）
    ContextCompacted, Log,
}
```

⚠️ **`Op::SendUserMessage { run_id }` 协议存在但实现是 turn-as-run**：每条 user message 都新 spawn 一个 run，run_id 不复用。要做真正的"持续 run"先理清 transcript / runspace 的所有权。

改协议前先想清楚兼容性。手动验证：跑 `hebbian-cli "你好" --mock` 看事件流是否完整。

---

## 层次边界（最重要）

| 层 | crate / 目录 | 职责 | 红线 |
|---|---|---|---|
| **Protocol** | `crates/protocol` | 数据类型，不放行为 | 不依赖其他业务 crate；只用基础 serde / uuid / chrono |
| **Agent Core** | `crates/agent-core` | run 生命周期、agent loop、tool、权限、context、hooks、recorder | 不 import Tauri；不直接拼 HTTP；不知道 OAuth |
| **Model Gateway** | `crates/model-gateway` | `ModelClient` trait、provider、protocol、auth/oauth、context_window | 不做 agent loop；不持有 agent 状态 |
| **Observability** | `crates/observability` | tracing init、OTLP exporter、metrics 工厂 | 不耦合业务类型；只暴露 attr 常量 + helper |
| **Platform** | `crates/platform` | CancelFlag、attachments、reasoning、error；**目前还混着 sessions/prompts/settings，待迁出** | 不做业务逻辑（理想态） |
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
9. **System prompt 要保 prompt-cache**：system 段 = [`BASE_SYSTEM_PROMPT`](crates/agent-core/src/system_prompt.rs) 常量 + 用户 persona，跨会话字节恒定；workspace / cwd / allowed_dirs 等环境信息**不进 system 段**——首条 user message 头部注入 `<environment>` 块，运行时新增允许目录通过下条 user message 的 `<workspace-update>` 块宣告。改 prompt 文案先动 `BASE_SYSTEM_PROMPT`，环境字段动 [`EnvironmentSnapshot`](crates/agent-core/src/system_prompt.rs)
10. **ModelClient 装饰器顺序**：`provider impl → InstrumentedClient → ModelWithName/NamedModelClient`。surface 不要绕过 InstrumentedClient（否则丢 metrics / span）

---

## 明确禁止

- 把 provider / auth / oauth 逻辑放到 `agent-core` 里
- 在 React 组件或 Tauri command handler 里直接编排 agent 逻辑
- 在 `agent-core` 里直接 `use tauri::*` 或 `use reqwest::*`（reqwest 应只在 model-gateway 用）
- 给 `EventPayload` 加 surface-specific 字段（surface 信息应在 channel adapter 里）
- 自动「全局统一注入」记忆——必须按 agent 身份过滤后注入
- 一开始就做：plugin 系统、复杂 DAG、agent swarm、自动团队自组织

---

## 当前已知漂移点 & 下一步

详见 [docs/todos.md](docs/todos.md)（按"代码漂移点"和"路线图收尾项"分类列出，含 file:line 引用）。
路线图阶段总览见 [docs/architecture.md §14-15](docs/architecture.md)。

---

## 给后续 agent 的提醒

- **改协议前先跑一遍三种 CLI 模式**：`hebbian-cli "..." --mock` / `hebbian-cli --json '...' --mock` / `hebbian-cli --mock`（loop），看事件流是否完整
- **agent-core 改完先 `cargo check -p agent-core --tests`**：测试已存在并会被 cargo 检查
- **desktop 改完跑 `cargo check -p hebbian` 和 `pnpm exec tsc --noEmit`**
- **不要重新生成已有文件**：先 Read，按需 Edit；尤其 `chat.rs` 已经 1500+ 行，重写代价很大
- **CLI 可以做端到端验证**，比启动 `pnpm tauri dev` 快得多
- **加新 EventPayload 变体后**：同步更新 [src/desktop/ui/types.ts](src/desktop/ui/types.ts) 的 `EngineEvent` union、[chat.rs](apps/desktop/src/chat.rs) 的 `agent_event_to_engine_event` 映射、[apps/cli/src/render.rs](apps/cli/src/render.rs) 的 `TurnRenderer::on_event` 渲染逻辑——三处任一漏改都会导致信息丢失
- **HITL 协议入口**：审批 / 提问 / 路径越界都走同一个 [HitlGate](crates/agent-core/src/tools/hitl.rs)。审批用 `open_approval` + `resolve`，提问用 `open_question` + `answer`，surface 端两条 Tauri 命令分别叫 `approve_permission` / `answer_question`（路径走 `approve_path_access`，逻辑同 approve）。新增需要 HITL 的协议时按这两条路径中哪条更贴合选。
- **TurnObserver 是 surface 的标准接入点**：实现三个回调（`on_event` / `on_permission_request` / `on_question`），在 [harness.rs](crates/agent-core/src/harness.rs) 找 `TurnObserver` trait。本地 surface 在 `on_*` 里返回 `Some(decision)` 让 driver 自动 resolve，远端 / 异步链路返回 `None` 自己处理。
- **推理 / thinking 配置入口**：[`platform::reasoning`](crates/platform/src/reasoning.rs) 统一抽象 Anthropic / OpenAI 两家的 schema 差异；想加新模型先看 `anthropic_thinking_mode` / `openai_supports_xhigh` 等 helper 而不是直接改 protocol。
- **调试模型 IO**：`HEBBIAN_DUMP_MODEL_IO=1 ./target/debug/hebbian-cli "..."` 把每次 model 请求的完整 `{request, response}` 落到 `<data_dir>/sessions/<session_id>.model_io.jsonl`，每行一对。具体实现在 [`ModelIoDump`](crates/agent-core/src/model_io_dump.rs)，attachments 只存 metadata 不写 base64。CLI 与桌面都已接入，无 session_id 的临时模式（`--no-record` / `--json`）不开启。
- **跑 OTLP 调试**：`OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 ./target/debug/hebbian-cli ...`，再开个 Jaeger / Tempo / Langfuse 看 span tree（run → turn → model.request / tool.call → permission.check）。

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
