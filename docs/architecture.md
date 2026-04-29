# Hebbian 架构

> 本文是 Hebbian 项目的目标架构。前半（设计前言）说明项目为什么这样切，后半（§0 起）描述这套切分落到代码上长什么样。每一节包含三类信息：
>
> - **职责**：这一层对外提供什么能力，对内拆成什么子模块。
> - **现状**：仓库里当下的实现位置，列出关键 trait / struct / 文件路径。
> - **差距与演进**：要达到生产可用还需要补什么、按什么顺序补。

---

## 设计前言：这个项目为什么要这样做

这部分不是抽象架构原则，而是项目一开始就应该被保留下来的设计动机。

### 核心判断

- 借鉴 `claude-code-haha` 与 `codex`，但不照搬它们的实现细节，借鉴背后的 **harness engineering** 思路。
- App 里目前只有一个 agent 页面入口，但这个页面只是入口，不应该成为 agent 本体。
- 这个项目不会只停留在桌面窗口，必须能自然长出 TUI / server / ACP / Slack / Webhook 等外部 channel。
- 前期架构必须既**足够简单**，又**天然可扩展**。

### 这里说的「简单」不是功能弱

- 不过度设计，不过度抽象
- 不一开始就做插件系统、复杂 DAG、agent team、scheduler、memory graph
- 不把 UI、运行时、模型访问、工具、权限、状态混在一起
- 先把真正长期稳定的边界切清楚

> **简单的结构，清楚的边界，稳定的协议，后续可扩展的核心。**

### 最重要的产品判断

> **Agent = Model + Harness**

模型不是产品，真正可扩展、可复用、可持续演化的是外层那一层 harness：上下文管理、工具系统、权限控制、状态机、事件流、验证、恢复、记忆、运行时边界。

- UI surface 只是入口
- Agent harness 才是产品核心
- Runtime / Model adapters 负责连接 Claude / Codex / Gemini / 未来更多 provider

### 必须坚持的方向

当前只保留一个 agent 页面入口是为了先把核心跑通；从架构上，这个入口绝不能和 agent 本体绑死。

正确方向：

- **一个稳定的 Agent Core / Harness**
- **一个统一的 Model Gateway**
- **多个可替换的 Surface**（Desktop / TUI / Server / CLI）
- **多个可扩展的 Channel**（Slack / Webhook / Cron / ACP / workflow）

接任何新入口，都不是重写 agent，而只是给同一个核心增加新的适配层。

### 本文回答的本质问题

不是「当前 Tauri 工程怎么分目录」，而是：

1. 哪一层才是产品核心？
2. 哪一层只是入口壳？
3. provider / oauth / protocol 应该归在哪？
4. subagent / multi-agent 将来怎么自然长出来？
5. context / memory / hooks / observability 应该挂在哪一层？
6. 扩展到 TUI / server / ACP / channel 时，哪些部分应该完全不用重写？

如果后面的任何设计违背了这段前言，就说明架构开始偏了。

---

## 0. 顶层模型

Hebbian 由四类要素组成。任何一段代码都应该能被准确放进其中一类，否则说明边界画错了。

| 要素 | 角色 | 典型成员 |
|------|------|----------|
| **Core** | 唯一的编排中心，决定"做什么、什么时候做" | Agent loop、Tool 执行、Permission、Context、Multi-agent、Hooks |
| **Gateway** | 可替换的"做"的能力，被 Core 调用 | Model Gateway、Memory、Platform（Storage / Observability / Config） |
| **Surface** | 用户/调用者进入 Core 的入口 | Desktop、TUI、Server、CLI |
| **Channel** | 自动/外部触发 Core 的入口 | Slack、Webhook、Cron、Email、ACP |

三条不可越界的规则：

1. Surface 与 Channel 不实现 agent 行为，它们只把外界输入翻译成 `Submission`，并消费 `Event` 渲染。
2. Core 只通过 Gateway trait 访问外部世界（模型、存储、记忆、IO），不直接拼 HTTP 或 OAuth。
3. Gateway 不持有 agent 状态，不感知 Run/Turn 的存在。

```text
┌──────────────────────────────────────────────────────────────────────┐
│ Surfaces & Channels                                                  │
│ Desktop · TUI · Server · Slack · Webhook · Cron · CLI                │
│        │ Submission                          ▲ Event                 │
│        ▼                                     │                       │
│  ┌─────────────────────────────────────────────────────────────┐     │
│  │ Agent Core                                                   │     │
│  │ Harness · Run/Turn · Loop · Tools · Permission · Context     │     │
│  │ Multi-Agent · Hooks                                          │     │
│  └─────────────────────────────────────────────────────────────┘     │
│        │             │             │             │                   │
│        ▼             ▼             ▼             ▼                   │
│  Model-Gateway    Memory        Platform     (Sandbox)              │
│  Provider+Auth+   Store+Index   Storage+     Exec policy            │
│  Protocol         Retrieval     Obs+Config   Approval               │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 1. Workspace 拓扑

仓库已经是 cargo workspace，无需迁移阶段，可以直接按目标结构演进。

### 1.1 目标布局

```text
hebbian/
├── apps/
│   ├── desktop/           现状：Tauri shell + 部分 Tauri command
│   ├── tui/               差距：未建立
│   └── server/            差距：未建立（HTTP/WS/SSE）
│
├── crates/
│   ├── protocol/          差距：未抽出，目前夹在 agent-core/types.rs
│   ├── agent-core/        现状：harness/loop/context/tools/hooks 骨架已有
│   ├── model-gateway/     现状：3 provider × 3 protocol，OAuth refresh 已有
│   ├── memory/            差距：未建立
│   ├── platform/          演进：瘦身为纯工具箱（runtime / error / attachments / blob）
│   ├── config/            差距：未建立（吞下 platform/config/prompts，未来加 agent_defs/permissions/hooks）
│   ├── persistence/       差距：未建立（吞下 platform/storage/sessions，新增 rollout/snapshot/replay）
│   ├── observability/     差距：未建立（独立成 crate 或并入 platform）
│   ├── channels/          差距：未建立
│   └── sandbox/           差距：未建立（可选，按 surface 引入）
│
├── apps/desktop/frontend/ 现状：React 组件、Tauri bridge、store
├── configs/               差距：agent 角色定义、permission policy、hooks
├── docs/                  现状：本文档、随记、todos
└── tests/                 差距：当前为空
```

### 1.2 依赖方向（必须是 DAG）

```text
protocol  ──►  无依赖（所有人都可以依赖它）
platform  ──►  protocol
config    ──►  protocol, platform
observability ──► protocol, platform
model-gateway ──► protocol, platform
memory    ──►  protocol, platform
persistence ──► protocol, platform
agent-core ──►  protocol, platform, config, model-gateway, memory, persistence, observability
channels  ──►  protocol, agent-core
sandbox   ──►  protocol, platform
apps/*    ──►  agent-core, channels（按需）, sandbox（按需）
```

要点：

- **`protocol` 是唯一被所有人依赖的 crate**。它只放数据类型、不放行为。
- **`agent-core` 不直接 import Tauri、reqwest、Slack SDK**。这是判断分层是否仍然干净的最简单方法。
- **app crate 才感知具体 surface**（窗口、IPC、终端、HTTP 路由）。

### 1.3 与现状的差异

| 项 | 现状 | 演进 |
|----|------|------|
| `crates/protocol` | 不存在 | 从 `agent-core/types.rs` 抽出 `AgentEvent`，并新增 `Submission/Op` |
| `crates/platform` 边界 | 混着业务持久化（`storage/sessions`、`config/prompts`） | 瘦身为纯工具箱；业务部分迁出 |
| `crates/config` | 不存在 | 新建，吞下 `platform/config/prompts`，未来加 `agent_defs / permissions / hooks` |
| `crates/persistence` | 不存在 | 新建，吞下 `platform/storage/sessions`，新增 `rollout / snapshot / replay` |
| `crates/memory` | 不存在 | 阶段二建立 |
| `crates/observability` | 不存在 | 阶段二，先并入 platform 也可接受 |
| `crates/channels` | 不存在 | 阶段三 |
| `apps/tui`、`apps/server` | 不存在 | 阶段三/四 |

---

## 2. 协议：单一可信锚点

Hebbian 中最不可漂移的资产是协议。所有 surface、所有 channel、所有 core 内部模块都基于这套协议通信。**协议必须独立成 crate**，且只依赖 serde + uuid 等基础库。

### 2.1 输入：`Submission` 与 `Op`

外界向 Core 发出的所有意图都是一个 `Submission`。Core 内部用一个有界的 channel（`mpsc::Sender<Submission>`）作为统一入口。

```rust
// crates/protocol/src/submission.rs
pub struct Submission {
    pub id: SubmissionId,        // 用于关联事件回流
    pub op: Op,
    pub trace: Option<TraceContext>,
}

pub enum Op {
    /// 启动一次新的 run（最常见）
    StartRun {
        agent: AgentRef,
        input: UserInput,
        turn_overrides: Option<TurnOverrides>,
        parent: Option<RunId>,           // 子 run 时填
    },
    /// 在已有 run 上追加一条用户消息（继续多轮对话）
    SendUserMessage {
        run_id: RunId,
        input: UserInput,
    },
    /// 回应一次审批请求
    Approve { request_id: PermissionRequestId, decision: ApprovalDecision },
    /// 中断 run（含级联取消子 run）
    Interrupt { run_id: RunId },
    /// 订阅一个 run 的事件流（用于断线重连或多端观察）
    Subscribe { run_id: RunId, since_seq: Option<u64> },
    /// 显式压缩、显式回滚、显式 fork —— 给高级用户的口子
    Compact { run_id: RunId },
    Rollback { run_id: RunId, to_turn: u32 },
    Fork { from: RunId, at_turn: Option<u32>, agent: Option<AgentRef> },
}

pub enum ApprovalDecision {
    AllowOnce,
    AllowAndRemember(PermissionScope),
    Deny,
    DenyWithFeedback(String),
}
```

设计要点：

- `StartRun` 与 `SendUserMessage` 分离，前者创建 run 上下文，后者只追加输入。
- `Approve` 是 HITL 协议的核心，详见 §11。
- `Subscribe { since_seq }` 让任意 surface 可以"接管"一个进行中的 run，是 desktop ↔ tui ↔ server 共享会话的基础。
- `Fork` / `Rollback` 是从 codex 借鉴的非破坏性时间线操作。

### 2.2 输出：`Event` 与 `EventPayload`

Core 向外只发送一种东西：带序号的 `Event`。一个 run 的事件流是有序的、可重放的、自描述的。

```rust
pub struct Event {
    pub run_id: RunId,
    pub seq: u64,
    pub at: Timestamp,
    pub payload: EventPayload,
}

pub enum EventPayload {
    // —— 生命周期 ——
    RunStarted { agent: AgentRef, parent: Option<RunId>, turn_ctx: TurnContextSummary },
    RunFinished { usage: UsageTotals, duration_ms: u64 },
    RunFailed { error: ErrorReport },
    RunCancelled,

    // —— 单个 turn ——
    TurnStarted { turn: u32 },
    TurnFinished { turn: u32, stop_reason: StopReason },

    // —— 模型流 ——
    TextDelta { text: String },
    TextDone { full_text: String },
    Reasoning { text: String },              // 思考流（可选，看模型支持）

    // —— 工具 ——
    ToolCallDelta { index: usize, id: Option<String>, name: Option<String>, arguments_delta: Option<String> },
    ToolCallStarted { call_id: String, name: String, input: Value },
    ToolCallFinished { call_id: String, result: ToolResultSummary, duration_ms: u64 },

    // —— 人机协作 ——
    PermissionRequested { request_id: PermissionRequestId, kind: PermissionKind, summary: String, risk: RiskLevel },
    PermissionResolved { request_id: PermissionRequestId, decision: ApprovalDecision },

    // —— 上下文 ——
    ContextCompacted { strategy: CompactionStrategy, before_tokens: usize, after_tokens: usize, summary_msg_id: Option<MessageId> },
    MemoryInjected { source: MemorySource, items: Vec<MemoryRef> },

    // —— 多 agent ——
    ChildRunSpawned { child: RunId, agent: AgentRef, context_policy: ContextPolicySummary },
    ChildRunFinished { child: RunId, outcome: ChildOutcome },

    // —— 时间线 ——
    RolledBack { to_turn: u32 },
    Forked { from: RunId, at_turn: u32, new_run: RunId },

    // —— 调试 ——
    HookFired { hook: String, point: HookPoint, action: HookOutcomeKind },
    Log { level: LogLevel, message: String, fields: Value },
}
```

设计要点：

- **`seq` 单调递增**。任何持久化与重放都靠它。**注意**：现状 `agent-core/agent_loop.rs` 使用全局 `static AtomicU64 SEQ`，这是个隐患——多 run 并发时 seq 不再 per-run 单调。**演进**：将 `seq` 移到 `Run` 状态对象上，每个 run 一个独立计数器。
- **`TurnContextSummary` 必须出现在 `RunStarted` 里**：这是断线重连的元信息（用了什么模型、什么 approval policy、什么 sandbox 配置）。
- 事件粒度要细到能完整回放 UI（包括思考流、压缩事件、记忆注入），但**避免每个 token 一个事件**——文本流走 `TextDelta`，按片段 batch。

### 2.3 与现状的差距

| 项 | 现状 | 演进 |
|----|------|------|
| 协议位置 | 在 `agent-core/types.rs` | 抽到 `crates/protocol` |
| `Submission/Op` | 不存在；当前由 Tauri command 触发 | 新建；Tauri command 改为 `Submission` 的薄包装 |
| `seq` 来源 | 全局 `AtomicU64` | 改成每 run 私有 |
| `RunStarted` 元信息 | 仅 run_id | 增加 `agent`、`parent`、`turn_ctx` |
| `ChildRunSpawned/Finished` | 不存在 | multi-agent 阶段补 |
| `PermissionResolved` | 不存在（只有 Requested） | HITL 闭环必备 |
| `Reasoning` | 不存在 | 接入 Anthropic 思考流时补 |

---

## 3. Agent Core

Core 是产品本体。它解决"一个 agent 如何可靠地从输入跑到输出"，包含 7 个紧密耦合的子系统。

### 3.1 Harness 与 Session / RunHandle

Core 暴露三层对象，职责清晰、生命周期递进：

```text
Harness    长生命周期工厂      持有 ToolRegistry / HookManager / Gateway 依赖
  └── Session   会话上下文       持有 transcript / workspace / definition / client
        └── RunHandle  一次 run 的句柄  独占事件流 + 控制方法
```

```rust
// crates/agent-core/src/harness.rs
pub struct Harness {
    registry: Arc<ToolRegistry>,
    hooks: Arc<HookManager>,
    blob_store: Arc<dyn BlobStore>,
    persistence: Arc<dyn PersistenceStore>,
    submit_tx: mpsc::Sender<Submission>,   // wire 协议入口（远端/跨进程用）
}

impl Harness {
    pub fn session(&self, config: SessionConfig) -> Session;
    pub fn submit(&self, op: Op) -> Result<SubmissionId>;          // 只在远端/跨进程入口走
    pub fn subscribe(&self, run: &RunId, since_seq: Option<u64>) -> EventStream;  // 断线重连
}

pub struct Session {
    transcript: Transcript,
    workspace: Arc<Workspace>,
    definition: AgentDefinition,
    client: Arc<dyn ModelClient>,
}

impl Session {
    pub fn append_user(&mut self, text: String, attachments: Vec<MessageAttachment>);
    pub fn run(&mut self) -> RunHandle;                           // 内部用累积的 transcript
    pub fn snapshot(&self) -> SessionSnapshot;                    // 落盘 / fork
}

pub struct RunHandle {
    run_id: RunId,
    events: mpsc::Receiver<Event>,                                // 独享流，无需 filter run_id
    control: RunControl,                                          // 内部持 gate / cancel
}

impl RunHandle {
    pub fn id(&self) -> &RunId;
    pub async fn recv(&mut self) -> Option<Event>;
    pub fn resolve(&self, request_id: &PermissionRequestId, decision: ApprovalDecision);
    pub fn answer(&self, request_id: &PermissionRequestId, answer: UserAnswer);
    pub fn interrupt(&self);
    pub async fn drive<O: TurnObserver>(self, observer: &mut O) -> TurnSummary;
}

impl Drop for RunHandle {
    fn drop(&mut self) { self.control.cancel(); }                 // drop 即取消，避免泄漏
}
```

设计要点：

- **本进程内调用拿 `RunHandle`**，`RunId` 只在跨进程场景使用（resume / SSE 重连 / channel adapter）。
- **`Session` 是 transcript 的唯一所有者**。`run()` 内部把 transcript 借给 agent loop，run 结束后 transcript 已就绪，外面直接读取。
- **`Harness` 跨 session 共享**。Surface 持有一个 Harness 实例，每个对话一个 Session，每条 user message 一个 RunHandle。
- **`HitlGate` / `CancelFlag` 封装在 RunHandle 内部**，公共 API 上看不到。
- **`submit(Op)` 用于跨进程入口**：Server / Channel / 远端恢复走它，本地走 `session.run()`。

`TurnObserver` 是 surface 接入 RunHandle 的标准方式（避免每个 surface 重写事件循环）：

```rust
#[async_trait]
pub trait TurnObserver: Send {
    fn on_text_delta(&mut self, text: &str) {}
    fn on_tool_started(&mut self, call: &ToolCallView) {}
    fn on_tool_finished(&mut self, result: &ToolResultView) {}
    fn on_compaction(&mut self, info: &CompactionInfo) {}
    async fn on_permission_request(&mut self, req: PermissionRequest) -> ApprovalDecision;
    async fn on_question(&mut self, q: UserQuestion) -> UserAnswer;
}
```

CLI 实现 = 终端渲染 + inquire 选择器；Desktop 实现 = 翻 EngineEvent + 注册 HitlState。事件循环、`run_id` 过滤、HITL 路由全在 `RunHandle::drive()` 里完成，surface 不再持有循环代码。

### 3.2 Session / Run / Turn / Step：四级生命周期

```text
Session   会话           transcript / workspace / definition / client 的容器
 └── Run        一次执行    run() 调用 → 直到 RunFinished/Failed/Cancelled
      └── Turn       一次往返   "用户输入 → 助手最终输出"
           └── Step       一次步进  模型调用 + 0..N 个 tool 并发执行
```

**Turn 显式持有 TurnContext**（借鉴 codex）：

```rust
pub struct TurnContext {
    pub model: ModelSelector,           // 本 turn 用的 model（可逐 turn 切换）
    pub tools: ToolSetSnapshot,         // 本 turn 启用的 tools
    pub approval: ApprovalPolicy,
    pub sandbox: SandboxPolicy,
    pub context_policy: ContextPolicy,  // 给子 agent 用
    pub budget: TokenBudget,
    pub iteration_budget: u32,          // tool 迭代上限
}
```

意义：

- 同一个 Session 的不同 turn 可以切换 model（"先 Sonnet 草拟、再 Opus 审"）
- Fork / Rollback 复制 `TurnContext` 即可重建配置
- 审批 / 沙箱 / 预算都限定在 `TurnContext` 范围，无隐式全局状态

`TurnContext` 由三处合并产生：`AgentDefinition` 默认值 → Session 配置 → `Op::StartRun.turn_overrides` / `session.run_with(overrides)` 临时覆盖。

### 3.3 Agent Loop

Loop 由几个职责单一的对象组合而成：

```rust
pub struct AgentLoop<'a> {
    transcript: &'a mut Transcript,
    compactor: ContextCompactor,
    dispatcher: ToolDispatcher,         // path 审批 → 工具审批 → 执行 → emit 全在它内部
    hooks: &'a HookManager,
    sink: EventSink,
    state: Arc<RunState>,
}

impl AgentLoop<'_> {
    pub async fn run(mut self, ctx: TurnContext) -> Result<TurnSummary, LoopError>;
}

pub enum LoopError {
    Model(ModelError),
    Cancelled,
    MaxIterations { limit: u32 },
    Tool(ToolDispatchError),
}
```

`ToolDispatcher` 单独负责「派发一组 tool call 并发返回 `Vec<ToolResult>`」：路径审批、工具审批、ask 提问、超时、cancel、emit 全在它内部，run loop 只看到 `Vec<ToolResult>`。`LoopError` 把模型错误 / 取消 / 迭代超限 / 工具失败拆开，surface 拿到能给用户精确提示。

```text
turn:
  1. 触发 BeforeModelCall（hook 可改 ModelRequest，见 §3.8）
  2. 组装 ModelRequest（带 prompt cache 边界，见 §3.6）
  3. 调用 Gateway，按 stream 转发 TextDelta / ToolCallDelta
  4. 模型返回：
     - Done → push_assistant → 返回 Finished
     - ToolCalls → 5
  5. ToolDispatcher.dispatch(calls)：
     - ReadOnly tools 并发执行
     - NeedsApproval / NeedsHumanInput 串行 await（见 §11）
     - 全部完成后写回 transcript
  6. iteration += 1；超 budget 时 emit ContinueLongRun 审批（见 §11.5）
```

`PermissionDecision` 三态：

```rust
pub enum PermissionDecision {
    Allowed,
    Denied { reason: String },
    NeedsApproval { request_id: PermissionRequestId, kind: PermissionKind },
}
```

NeedsApproval 触发流：emit `PermissionRequested` → `RunHandle.control` 登记 oneshot waiter → 该 tool future await → 用户回应 → resolve waiter → 继续执行。同 turn 内多个工具的 waiter 互相独立，但**串行 await**——见 §11.2 的并发规则。

### 3.4 Tool 系统

```rust
// crates/agent-core/src/tools/mod.rs
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;                              // name + description + JSON schema
    fn classify(&self, input: &Value) -> ToolClass;           // 自报 HITL 需求与并发策略
    fn affected_paths(&self, input: &Value) -> Vec<PathBuf> { Vec::new() }
    async fn invoke(&self, ctx: ToolCtx<'_>, input: Value) -> ToolResult;
}

pub enum ToolClass {
    ReadOnly,                                                 // 并发执行，免审批
    Network,                                                  // 默认询问（远端 channel 强制 ask）
    Mutating { risk: RiskLevel },                             // 串行执行，按 policy 询问
    Destructive { risk: RiskLevel },                          // 串行 + 默认 ask
    NeedsHumanInput { kind: HumanInputKind },                 // 走 HitlGate.ask 路径（如 ask 工具）
}

pub struct ToolCtx<'a> {
    pub run_id: &'a RunId,
    pub turn_ctx: &'a TurnContext,
    pub harness: &'a HarnessHandle,                           // 让工具可 spawn 子 run
    pub cancel: CancelFlag,
    pub blob_store: &'a dyn BlobStore,
}

pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub outcome: ToolOutcome,
    pub metadata: ToolMetadata,                               // 截断标记、原始大小、外链等
}

pub enum ToolOutcome {
    Ok { content: ToolContent, attachments: Vec<MessageAttachment> },
    Denied { reason: DenyReason },                            // 路径越界 / 权限被拒
    Failed { error: String },                                 // 执行异常
}

pub enum ToolContent {
    Text(String),
    Json(Value),
    BlobRef { id: BlobId, preview: String, mime: String },
    Multi(Vec<ToolContent>),
}
```

设计要点：

- **`classify` 是权限与并发的统一入口**。Dispatcher 按 ToolClass 决定要不要并发、要不要走 HitlGate、走哪条 HITL 路径。
- **`NeedsHumanInput` 是普通工具分类**，`ask` 是其中一个实例（不再特判 `call.name == ASK_TOOL_NAME`）。
- **`ToolOutcome` 把"被拒/失败/正常"在协议层分开**。模型 prompt 层做格式化，UI 直接根据 outcome 染色，统计层能算成功率。
- **`ToolContent::BlobRef`** 是 Context Engine 第一层压缩入口。超阈值的输出由 dispatcher 自动落 blob，transcript 只见 preview + ref。
- 工具通过 `ctx.harness` 可以 `submit(StartRun {...})` 起子 agent，`spawn_agent` 是 ReadOnly 类的普通工具。

#### 三类工具的来源

每轮 ModelRequest.tools 由这三类组合：

| 类别 | 暴露给 UI | 用户可关 | 注入策略 | 例子 |
|------|----------|---------|---------|------|
| **Builtin** | ❌ | ❌ | 每轮强制注入 | `ask`、`bash`、`read`、`write`、`grep` |
| **Optional** | ✅ | ✅ | 按 `enabled_tools` 过滤 | `web_search`、`web_fetch` |
| **Hosted** | ✅ | ✅ | 按 `enabled_tools` 过滤，仅传 schema | `image_generation` |

三类工具都实现同一个 `Tool` trait——`classify` 决定 HITL 路径，`invoke` 决定执行方式：

- 普通工具（read/grep/web_*）`invoke` 跑本地逻辑
- `ask` 工具 `classify` 返回 `NeedsHumanInput { kind: Question }`，`invoke` 由 dispatcher 接管走 HitlGate
- Hosted 工具 `invoke` 永远不会被调（provider 端执行），core 只读它的 spec

**注册约定**（[crates/agent-core/src/tools/mod.rs](../crates/agent-core/src/tools/mod.rs)）：

- `default_tools()` → 全部内置 Tool 实现
- `hosted_tool_definitions(filter)` → provider 端工具 schema
- `tool_manifest()` → UI 工具菜单元信息（仅 Optional + Hosted）

### 3.5 HitlGate

`HitlGate` 是 HITL 的统一通道：审批（destructive 工具）、提问（ask 工具）、路径越界、长 run 续跑都走它。详见 §11，本节只列结构。

```rust
pub struct HitlGate {
    policy: ApprovalPolicy,
    rules: Vec<PermissionRule>,                       // 用户/项目/会话级累积
    pending: Mutex<HashMap<PermissionRequestId, PendingHitl>>,
}

enum PendingHitl {
    Approval(oneshot::Sender<ApprovalDecision>),
    Question(oneshot::Sender<UserAnswer>),
}

impl HitlGate {
    pub fn check(&self, tool: &dyn Tool, input: &Value, ctx: &TurnContext) -> PermissionDecision;
    pub fn ask(&self, question: String, options: Vec<QuestionOption>) -> QuestionPending;
    pub fn request_path_access(&self, tool: &str, paths: Vec<PathBuf>) -> ApprovalPending;
    pub fn request_continue(&self, iteration: u32) -> ApprovalPending;

    pub fn resolve(&self, req: &PermissionRequestId, decision: ApprovalDecision);
    pub fn answer(&self, req: &PermissionRequestId, answer: UserAnswer);
    pub fn cancel_all_pending(&self);
}
```

四种 pending 共用同一张 `pending` 表，靠 `PermissionRequestId` 命名空间统一调度；surface 通过 `EventPayload` 的 kind 字段判断该用审批 UI 还是提问 UI。

### 3.6 Context Engine

Context Engine 负责"模型每次看到什么"。它是一个独立的 module，不和 loop 混在一起。

```rust
// crates/agent-core/src/context/mod.rs
pub struct ContextEngine {
    transcript: Transcript,
    budget: TokenBudget,
    compactor: Box<dyn Compactor>,
}

impl ContextEngine {
    pub fn append_user(&mut self, input: UserInput);
    pub fn append_assistant(&mut self, msg: AssistantMessage);
    pub fn append_tool_result(&mut self, results: Vec<ToolResult>);

    /// 在每次模型调用前调用，必要时压缩
    pub async fn prepare_for_model(&mut self, target: TokenLimit) -> ContextSnapshot;

    /// 用于 fork / replay / 子 agent 继承
    pub fn snapshot(&self) -> TranscriptSnapshot;
    pub fn project(&self, policy: ContextPolicy) -> Transcript;
}
```

#### 四层压缩策略（递进，不一刀切）

| 层 | 触发时机 | 做什么 | 现状 |
|----|---------|--------|------|
| **L1 BlobRef 降维** | 工具结果产生时 | 大输出落 blob，transcript 只留 preview | 缺 |
| **L2 结构化裁剪** | budget 超阈值 | 保留 system、最近 N 条、未闭合 tool loop、所有 user 消息；裁掉冗余 tool result | 现状是简单截断 |
| **L3 摘要式压缩** | L2 后仍超 | 调小模型把中段历史摘成一条 system-prefix message | 已有 `compact_structural`，**未实现 LLM 摘要** |
| **L4 投影式继承** | 子 agent / fork 时 | 按 `ContextPolicy` 投影出新 transcript | 缺 |

设计要点：

- **压缩必须可见**：每次压缩发 `ContextCompacted { strategy, before_tokens, after_tokens, summary_msg_id }`。
- **压缩可追溯**：摘要 message 持有原始 message id 范围，调试时可展开。
- **子 agent 继承按 `ContextPolicy` 投影**生成新 transcript（拷贝、摘要、按 id 选择三选一）。

#### Prompt cache 边界

`prepare_for_model()` 输出的 `ContextSnapshot` 把内容分成三段，让 provider 层精确告诉模型 API "缓存到这里"：

```text
[ STABLE   ]   AgentDefinition.system_prompt + 内置工具 spec
———— cache breakpoint A ————
[ SEMI     ]   workspace XML（allowed_dirs / cwd）
———— cache breakpoint B ————
[ MUTABLE  ]   transcript history + 当前 turn input
```

- STABLE 段在 Session 生命周期内不变，命中长效缓存
- SEMI 段在用户授权新路径时变动，单次 turn 内稳定
- MUTABLE 段每轮变化

`ModelRequest` 携带 `cache_breakpoints: Vec<usize>`，Anthropic / OpenAI 协议层翻译成各自的 `cache_control` 标记。token 成本相比每轮整段重拼可降低 30-90%。

#### `ContextPolicy` 枚举

```rust
pub enum ContextPolicy {
    Isolated,                                       // 默认
    InheritRecent { messages: usize },
    InheritSummary,                                 // 父 transcript 的 L3 摘要
    InheritSelected { ids: Vec<MessageId> },
    OnDemand,                                        // 给只读查询工具的口子
}
```

**现状**：`definition.rs` 已有 `ContextPolicy` 枚举（具体内容待确认），但 multi-agent 没有真正消费它。

### 3.7 Multi-Agent Runtime

子 agent 是 Core 协议的一等公民，通过普通工具（`spawn_agent` / `spawn_parallel`）触发。

```rust
// crates/agent-core/src/multi_agent/mod.rs
pub struct RunTree {
    nodes: HashMap<RunId, RunNode>,
    edges: Vec<(RunId, RunId)>,
}

pub struct RunNode {
    pub run: RunHandle,
    pub agent: AgentRef,
    pub parent: Option<RunId>,
    pub spawned_at_turn: Option<u32>,
    pub aggregator: Option<Aggregator>,            // 父等待子的方式
}

pub enum Aggregator {
    JoinFirst,                                      // 任一子完成
    JoinAll,                                        // 所有子完成
    ReduceWith { tool: String },                    // 用一个 reducer 工具汇总
}
```

只支持三种协作模式（保持简单）：

- **Hierarchy**：父等子串行执行。
- **Parallel Fan-out**：父用 `spawn_parallel` 起多个子，等 `JoinAll` 或 `JoinFirst`。
- **Pipeline**：父按顺序起 A→B→C，把前一个的输出作为后一个的输入。

**现状**：完全没有。
**演进顺序**：先 Hierarchy（一个 `spawn_agent` 工具），再 Fan-out（一个 `spawn_parallel` 工具），Pipeline 用调度小段代码组合。

### 3.8 Hooks

Hook 是 Core 唯一的**可改变行为**的扩展点。观察类需求一律走 Event 流（surface 订阅 EventStream / observability crate 接 Signal），Hook 只保留四个能改控制流或数据的拦截点：

```rust
pub enum HookPoint {
    BeforeModelCall { turn: u32 },          // 改 ModelRequest（注入记忆 / 改 system prefix / 加 tool）
    OnPermissionCheck { tool: String },     // 旁路 HitlGate（学习规则 / 自动审批 / 强制询问）
    OnToolResult { tool: String },          // 改写 ToolResult（截短 / 落 blob / 脱敏）
    OnCompaction { strategy: CompactionStrategy },  // 自定义压缩
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn matches(&self, point: &HookPoint) -> bool;
    async fn invoke(&self, ctx: HookCtx<'_>) -> HookOutcome;
}

pub enum HookOutcome {
    Continue,
    Modify(HookPatch),                       // 改 transcript / system / input / result / decision
    Block { reason: String },                // 拦截这次操作
}
```

Memory 注入与写候选实现为 `BeforeModelCall` / `OnToolResult` 两个内置 hook。Run / Turn 级生命周期监听通过订阅 `RunStarted / TurnStarted / RunFinished` 等事件实现，不再开 hook 点位。

---

## 4. Model Gateway

### 4.1 职责

给 Core 提供一个 provider-neutral 的"调模型"能力。**Core 不知道 OAuth、不拼 HTTP、不感知 SSE 帧格式**。

```rust
// crates/model-gateway/src/client.rs
#[async_trait]
pub trait ModelClient: Send + Sync {
    fn provider_id(&self) -> &str;
    fn supports_streaming_tools(&self) -> bool;

    async fn complete(&self, req: ModelRequest, cancel: CancelFlag) -> Result<ModelResponse, ModelError>;
    async fn stream(&self, req: ModelRequest, cancel: CancelFlag, on_event: &(dyn Fn(ModelStreamEvent) + Send + Sync))
        -> Result<ModelResponse, ModelError>;
}
```

### 4.2 内部分层

```text
model-gateway/src/
├── client.rs          ModelClient trait
├── types.rs           ModelRequest / ModelResponse / Usage / ToolCall ...
├── registry.rs        provider_id -> ModelClient 工厂
├── routing.rs         ModelSelector -> 具体 provider+model（含 fallback / load-balance）
├── retry.rs           退避、限流、特定 error 重试策略
├── protocols/         请求/响应映射（claude, openai, gemini）
├── providers/         具体 provider 客户端（含分别的 OAuth 流程触发）
├── auth/              api_key, oauth/{claude,codex,gemini}, refresh, credential_store
└── discovery/         拉取 provider 模型列表
```

**现状**：

- `protocols/`、`providers/`、`auth/refresh`、`discovery/` 已有
- ModelClient trait 已定义且基本符合上述形态
- **缺** `registry`、`routing`、`retry`：当前由 chat.rs 直接选 provider，没有抽象层

### 4.3 Provider × Auth × Protocol 的三轴模型

```text
Provider   = 一家供应商（Anthropic / OpenAI / Google / Bedrock / Vertex / Local-Ollama）
Protocol   = 请求/响应的 wire format（claude-messages-v1, openai-chat, openai-responses, gemini-v1）
Auth       = 凭据获取方式（api_key, oauth-pkce, device-flow, vertex-sa, aws-sigv4）
```

一个 ModelClient = (Provider × Protocol × Auth)。同一 Protocol 可被多 Provider 复用（OpenAI-compatible 服务），同一 Auth 也可被多 Protocol 复用。`registry` 注册的是这个三元组的具体组合。

### 4.4 演进重点

| 项 | 现状 | 演进 |
|----|------|------|
| Routing | 不存在，硬编码三个 provider | 增加 `ModelSelector { provider, model, fallback }` |
| Retry | 不存在 | 标准退避 + 区分可重试错误（429 / 5xx / 网络） |
| Cost / Usage 标准化 | `Usage` 结构有 input/output tokens，无成本 | 接入 pricing table，事件里带 cost |
| Health check | `health.rs` 已有雏形 | 暴露给 Surface 用于 provider dropdown |

---

## 5. Memory

记忆系统是独立 crate，**不内嵌进 agent-core**。Core 只调它的 trait。

```rust
// crates/memory/src/store.rs
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn query(&self, q: MemoryQuery) -> Vec<MemoryEntry>;
    async fn write(&self, entry: MemoryEntry) -> MemoryId;
    async fn update(&self, id: MemoryId, patch: MemoryPatch);
    async fn delete(&self, id: MemoryId);
}

pub struct MemoryQuery {
    pub identity: AgentRef,                         // 谁在查
    pub scopes: Vec<MemoryScope>,
    pub text: Option<String>,                       // 语义查询（可选，向量库才需要）
    pub kinds: Vec<MemoryKind>,
    pub limit: usize,
}

pub enum MemoryScope {
    Global, Project(ProjectId), Agent(AgentRef), Session(SessionId),
}

pub enum MemoryKind {
    User, Feedback, Project, Reference,             // 与 CLAUDE.md 中 auto memory 对齐
}
```

### 5.1 注入与写入走 Hook

记忆相关行为以两个内置 hook 实现：

- `MemoryInjectHook`：在 `BeforeRun`（注入长期身份记忆）和 `BeforeModelCall`（注入查询相关记忆）触发
- `MemoryWriteCandidateHook`：在 `AfterTurn` 触发，**只生成候选**，写入需要用户确认（默认）或显式策略允许

### 5.2 后端

```text
memory/src/
├── store.rs          trait
├── fs.rs             Markdown + frontmatter（默认实现，零依赖）
├── sqlite.rs         可选
└── vector.rs         可选（启用 feature=embed）
```

**现状**：完全不存在；Claude Code 的 auto memory 系统目前只在 `~/.claude/projects/...` 下作为参考。

**演进顺序**：先做 fs.rs（与现有 auto memory 的 markdown 文件兼容），稳定后再加 sqlite / vector。

---

## 6. Platform

Platform 是 **纯工具箱**——只放"任何 crate 都可能用到、与业务领域完全无关"的代码。任何带有领域语义的东西（会话、提示词、agent 配置）都不进 platform。

```text
platform/src/
├── runtime/
│   ├── cancel.rs                CancelFlag（短期）；harness 改 actor 后挂 RunHandle
│   └── clock.rs                 可注入时钟（测试用）
├── fs/
│   ├── atomic.rs                temp+rename / 文件锁
│   └── json.rs                  原子读写 JSON
├── blob.rs                      BlobStore trait + 文件实现（工具长输出落盘用）
├── attachments.rs               消息附件类型
└── error.rs                     AppError / AppResult
```

### 6.1 Platform 的判断标准

一段代码能进 platform，当且仅当它满足：

1. **没有领域含义**：换一个项目（不是 agent，是别的 Rust app）也能直接复用
2. **不感知 Run / Turn / Agent / Session / Tool / Model 任何业务概念**
3. **依赖的全是基础库**（`std` / `tokio` / `serde` / `chrono` / `uuid`）

`CancelFlag` 是边缘案例：它本身是工具，但当前用全局 `static OnceLock<HashMap>` 是为了配合"按 request_id 取消"的旧 IPC 模式。Harness 改 actor 后，cancel flag 应该挂在 `RunHandle` 上，全局表删掉。

### 6.2 现状审计

| 文件 | 真实身份 | 演进去向 |
|------|---------|---------|
| `runtime.rs`（CancelFlag 注册表） | 真·工具，但有全局状态 | 留下 trait 与类型，全局表随 harness actor 化删除 |
| `error.rs` | 真·工具 | 保留 |
| `attachments.rs` | 半工具半业务 | 保留（消息附件是通用数据类型） |
| `config/prompts.rs` | **业务**：系统提示词配置 | **迁出 → `crates/config`** |
| `storage/sessions.rs` | **业务**：对话会话持久化 | **迁出 → `crates/persistence`** |
| 缺：`blob.rs` | 真·工具 | 新建（阶段二，工具长输出降维需要） |
| 缺：`fs/atomic.rs` | 真·工具 | 新建（多 crate 写文件都需要原子语义） |

---

## 6b. Config

`crates/config` 集中**所有人写的、agent 跑起来要读的配置**。它本身不做 IO 之外的事，结构与文件系统目录直接对应。

```text
config/src/
├── prompts.rs                   原 platform/config/prompts.rs
├── agent_defs.rs                AgentDefinition YAML 加载
├── permissions.rs               PermissionPolicy YAML 加载
├── hooks.rs                     Hook 配置加载
└── loader.rs                    通用：路径解析、合并（global → project → session）
```

对应磁盘布局：

```text
~/.hebbian/configs/             用户全局
<project>/.hebbian/configs/     项目级
configs/                        仓库内置默认值
```

**为什么不并进 platform**：因为它感知 `AgentDefinition / PermissionPolicy` 这些领域概念，违反 §6.1 第二条标准。

**为什么不并进 agent-core**：agent-core 应该只关心"运行时如何使用配置"，而不关心"配置从哪里读、合并规则是什么"。channels / surfaces 也要读 config（比如 Slack channel 要读自己的 permission policy），让它们绕过 agent-core 直接依赖 config 更干净。

---

## 7. Persistence：Rollout 与 Resume

生产 agent 必须能从崩溃中恢复、能从历史回放、能 fork。

### 7.1 设计

`crates/persistence` 同时承担两件事——把 platform 现在错位的会话持久化也吞进来：

```text
persistence/src/
├── sessions.rs                  原 platform/storage/sessions.rs
│                                Session / Message / MessageToolCall 持久化
├── rollout.rs                   新增：JSONL 事件日志（崩溃恢复、replay 用）
├── snapshot.rs                  新增：transcript 阶段性 snapshot
└── store.rs                     trait：SessionStore / RolloutStore
```

```rust
// crates/persistence/src/rollout.rs
pub trait RolloutStore: Send + Sync {
    async fn open(&self, run: &RunId) -> RolloutWriter;
    async fn replay(&self, run: &RunId, since_seq: Option<u64>) -> EventStream;
    async fn list(&self, filter: RolloutFilter) -> Vec<RolloutSummary>;
}
```

格式选 **JSONL**（每行一个 `Event`）：

- 流式追加，崩溃只丢最后一行
- 易于 grep / jq 调试
- 不需要 schema migration

```text
~/.hebbian/rollouts/
└── <date>/
    └── <run_id>.jsonl                    每行一个 Event
└── snapshots/
    └── <run_id>-<turn>.json              定期 snapshot transcript
```

### 7.2 三个核心操作

- **Resume**：读 `<run_id>.jsonl`，重放到 `RunRegistry`，恢复 transcript。模型调用未完成的 turn 重新发起。
- **Fork**：读 `<run_id>.jsonl` 到 `at_turn`，复制为新 `RunId`，新建 jsonl 文件。
- **Rollback**：在 jsonl 末尾追加一个 `RolledBack` 事件，截断 transcript 视图（不物理删除）。

**现状**：完全不存在；当前 `chat.rs` 只把对话存进 `Session`（轻量持久化），没有事件级别。

**演进顺序**：先做 RolloutWriter（即时落盘）→ 再做 Replay → 再做 Fork/Rollback。

---

## 8. Observability

```rust
// crates/observability/src/lib.rs
pub trait Observer: Send + Sync {
    fn record(&self, signal: Signal);
}

pub enum Signal {
    RunDuration { run: RunId, agent: AgentRef, duration_ms: u64 },
    ModelCall { provider: String, model: String, latency_ms: u64, tokens_in: u32, tokens_out: u32, cost_usd: f64 },
    ToolCall { tool: String, duration_ms: u64, permission_wait_ms: u64, success: bool },
    Compaction { strategy: CompactionStrategy, before_tokens: usize, after_tokens: usize },
    MemoryInject { count: usize },
    HookFired { hook: String, point: HookPoint, blocked: bool },
}
```

实现：

- **logs**：`tracing` + `tracing-subscriber`（已用）
- **metrics**：默认 `tracing` 字段；可选 `metrics` crate + Prometheus exporter
- **traces**：`tracing` span，带 `run_id / turn / step` 字段；可选 OpenTelemetry exporter

UI 配套：一个 `Inspector` 组件，订阅同一条 EventStream 渲染：

- Run tree（含子 run）
- Token / cost 时间线
- Tool timeline（含权限等待时长）
- Compaction blocks
- Memory inject panel
- Hook firing list

**现状**：仅 `tracing` 调用散布在 agent_loop。**缺**：标准 Signal、Inspector UI、cost 计算。

---

## 9. Surfaces 与 Channels

### 9.1 Surface 共同规约

Surface 只做四件事：

1. 启动时拿一个 `Harness` 实例（共享）
2. 用 `harness.session(config)` 为每个对话建一个 `Session`
3. 用户消息进来：`session.append_user(...)` → `session.run()` 拿 `RunHandle`
4. 实现 `TurnObserver`，调 `handle.drive(&mut observer).await` 跑完一轮

```rust
#[async_trait]
pub trait TurnObserver: Send {
    fn on_text_delta(&mut self, text: &str) {}
    fn on_tool_started(&mut self, call: &ToolCallView) {}
    fn on_tool_finished(&mut self, result: &ToolResultView) {}
    fn on_compaction(&mut self, info: &CompactionInfo) {}
    async fn on_permission_request(&mut self, req: PermissionRequest) -> ApprovalDecision;
    async fn on_question(&mut self, q: UserQuestion) -> UserAnswer;
}
```

事件循环、`run_id` 过滤、HITL 路由、终止条件都在 `RunHandle::drive()` 内部完成。Surface 不写循环、不持有 gate Arc、不调 `resolve_permission` 反查。

跨进程入口（HTTP / SSE / 远端 channel）走 `harness.submit(Op)` + `harness.subscribe(run_id, since_seq)` 的协议路径，本地不走。

### 9.2 Desktop

```text
apps/desktop/
├── src/                          Tauri Rust 端
│   ├── main.rs / lib.rs          Tauri 启动 + command 注册
│   ├── bridge.rs                 Tauri command ↔ Session/RunHandle 的薄翻译
│   ├── observer.rs               TauriObserver 实现 TurnObserver
│   ├── session_store.rs          SessionId → Session 注册表（持有 Harness 共享）
│   └── chat.rs                   send_message / approve_permission / answer_question 路由
├── frontend/
│   ├── index.html                Vite 入口
│   └── src/                      React 前端源码
│       ├── ui/                   React 组件
│       ├── bridge/tauri.ts       IPC 封装
│       └── store/                前端状态
├── package.json                  Desktop 前端脚本与依赖
├── pnpm-lock.yaml                Desktop 前端锁文件
├── vite.config.ts                Vite root / alias / dist 配置
├── tsconfig.json                 TypeScript include / path alias
├── tailwind.config.cjs           Tailwind content 扫描前端路径
├── postcss.config.cjs            PostCSS 插件配置
└── ...
```

`TauriObserver` 把 `on_*` 回调翻译成 `EngineEvent`，通过 `Channel<EngineEvent>` 发给 React。前端 `pendingApproval` 改为 `pendingHitl: PendingHitl[]` 队列（与 §11.2 的并发规则配套）。

### 9.3 TUI（未来）

`apps/tui/` 用 `ratatui`，**不复用 React 组件、不复用 Tauri**。直接接 `HarnessHandle`。最小入口：

```text
apps/tui/src/
├── main.rs          tokio + ratatui main loop
├── input.rs         readline / key event
├── render.rs        消息流 / inspector 双 pane
└── shortcuts.rs     ctrl-c 取消 / `:fork` / `:rollback`
```

### 9.4 Server（未来）

`apps/server/` 提供 HTTP + SSE：

- `POST /v1/runs` → `Op::StartRun`
- `POST /v1/runs/:id/messages` → `Op::SendUserMessage`
- `POST /v1/runs/:id/approve` → `Op::Approve`
- `GET  /v1/runs/:id/events?since=<seq>` → SSE 流
- `POST /v1/runs/:id/cancel` → `Op::Interrupt`

### 9.5 Channels

`crates/channels/` 的每个 channel 是一个独立 module，实现 `InboundChannel + OutboundRenderer`：

```rust
#[async_trait]
pub trait InboundChannel: Send + Sync {
    fn id(&self) -> &str;
    async fn run(self: Arc<Self>, harness: HarnessHandle, cancel: CancelFlag) -> Result<()>;
}
```

每个 channel 有自己的 `EventSource`（用于审计、权限策略分支）：

```rust
pub enum EventSource {
    LocalDesktop, LocalTui, Cli,
    HttpApi { client_id: String },
    Slack { channel: String, thread_ts: String, user: String },
    Webhook { endpoint: String },
    Cron { job_id: String },
    Email { message_id: String, from: String },
}
```

**安全规约**：

- 本机 surface（Desktop / TUI / CLI）：默认 `ApprovalPolicy::OnRiskyOnly`
- 远端 channel（Slack / Webhook / Cron）：默认 `ApprovalPolicy::AlwaysAsk`，且只读工具优先
- 任何 destructive tool 在远端 channel 触发时，**必须** ask（policy 不允许 override）

---

## 10. AgentDefinition 与配置

```rust
pub struct AgentDefinition {
    pub id: AgentRef,
    pub display_name: String,
    pub system_prompt: String,
    pub model: ModelSelector,
    pub allowed_tools: Vec<ToolRef>,
    pub allowed_children: Vec<AgentRef>,
    pub default_context_policy: ContextPolicy,
    pub memory_policy: MemoryPolicy,
    pub compaction_policy: CompactionPolicy,
    pub permission_policy: PermissionPolicy,
    pub hooks: Vec<HookRef>,
    pub role_tags: Vec<String>,
}
```

配置存放：

```text
configs/
├── agents/
│   ├── default.yaml
│   ├── researcher.yaml
│   ├── coder.yaml
│   ├── reviewer.yaml
│   └── orchestrator.yaml
├── permissions/
│   ├── desktop.yaml
│   ├── slack.yaml
│   └── webhook.yaml
└── hooks/
    └── memory.yaml
```

不同 agent 的差别只是配置，不是代码。

**现状**：`AgentDefinition` 结构已有；**缺** YAML 加载、配置目录、内置角色文件。

---

## 11. Human-in-the-Loop（HITL）

这是生产可用 agent 系统的核心拼图，独立成章。

### 11.1 四个层次的 HITL

| 层次 | 目的 | 触发 | 阻塞性 | 走的 Gate 路径 |
|------|------|------|--------|---------|
| **L1 工具审批** | "可以执行这个工具吗？" | `Tool::classify` 返回 `Mutating/Destructive/Network` 且 policy 命中 | 阻塞该 tool 与同 turn 后续 tools | `HitlGate.check` → ApprovalDecision |
| **L1' 路径审批** | "可以访问 workspace 外的路径吗？" | `Tool::affected_paths` 越界 | 阻塞该 tool | `HitlGate.request_path_access` → ApprovalDecision |
| **L1'' Ask 提问** | "agent 想问你拿建议" | `Tool::classify` 返回 `NeedsHumanInput` | 阻塞该 tool | `HitlGate.ask` → UserAnswer |
| **L2 Plan 审批** | "可以按这个计划继续吗？" | `submit_plan` 工具 | 阻塞 run | `HitlGate.check` (kind=Plan) |
| **L3 长 run 续跑** | "已迭代 N 轮，继续？" | `iteration > budget` | 阻塞 run | `HitlGate.request_continue` → ApprovalDecision |

四种路径共用 `HitlGate` 内部状态机（同一张 pending 表 + oneshot），通过 `EventPayload` 的 `kind` 字段告诉 surface 该用哪种 UI 呈现。

### 11.2 协议流（统一）

```text
ToolDispatcher ──(decision := HitlGate.check / ask / request_path_access)──►
   ├─ Allowed                ──► 直接执行
   ├─ Denied { reason }      ──► ToolOutcome::Denied 回灌 transcript
   └─ NeedsApproval { id }   ──► emit PermissionRequested|UserQuestionRequested
                                   │
                                   │ (该 tool future await waiter)
                                   │
       Surface(TurnObserver) ──► 渲染 → 用户回应 → handle.resolve(id, decision)
                                                       │
       HitlGate.resolve / answer ──► oneshot 唤醒 waiter
                                   │
ToolDispatcher ──► 根据回应：
   AllowOnce            → 执行
   AllowAndRemember     → 持久化规则后执行
   Deny / Cancelled     → ToolOutcome::Denied
   DenyWithFeedback(s)  → ToolOutcome::Denied + 把 s 作为 user message 注入下一轮
```

并发规则：

- **`ReadOnly` 工具同 turn 内并发执行**（`join_all`）
- **任意需要 HITL 的工具串行执行**：每次只有一个 pending request 出现在 surface 上
- 串行的根本原因：UI 单审批模型 + 用户精力分配；并发审批的 UX 复杂度远超收益

其他规约：

- **超时**：每个 pending 默认 5 分钟，超时按 channel 策略处理（远端 channel 默认 Deny，Desktop 默认继续等待）
- **取消传播**：`RunHandle::interrupt()` 级联 `HitlGate.cancel_all_pending()`
- **持久化**：审批/回应作为 `PermissionResolved` / `UserQuestionAnswered` 事件写入 rollout，replay 不重复询问

### 11.3 ApprovalPolicy

```rust
pub enum ApprovalPolicy {
    /// 全自动：所有工具直接执行
    Bypass,
    /// 仅风险工具：destructive / network 询问
    OnRiskyOnly,
    /// 总是询问：每个工具都要点
    AlwaysAsk,
    /// 自定义：基于规则
    Custom(Vec<PermissionRule>),
}

pub struct PermissionRule {
    pub matcher: ToolMatcher,             // 工具名 + input 谓词
    pub decision: RuleDecision,           // Allow / Deny / Ask
    pub scope: PermissionScope,           // Once / Run / Session / Project / Global
}
```

策略来源（按优先级累积）：

1. `TurnContext.approval` —— 本 turn 临时策略
2. AgentDefinition.permission_policy —— agent 内建
3. configs/permissions/*.yaml —— 用户配置
4. 用户运行时 Allow-and-Remember 累积的 session/project 规则

### 11.4 L2：Plan 审批

某些 agent 模式（researcher / coder）受益于"先出 plan、人确认、再执行"。

实现：

- 一个特殊 tool `submit_plan(plan: String, steps: Vec<PlanStep>)`
- AgentDefinition 启用此 tool 后，模型 system prompt 自动注入"先用 submit_plan，等用户确认再执行"
- `submit_plan` 的执行就是 emit `PermissionRequested { kind: Plan, ... }` 并 await
- 用户审批后才允许调用其他写工具

### 11.5 L3：长 run 续跑

`TurnContext.iteration_budget` 配置工具迭代上限。达到时走 `HitlGate.request_continue`：

```rust
emit PermissionRequested { kind: ContinueLongRun { iteration }, summary: "已迭代 N 轮，是否继续？", risk: Medium }
await ApprovalDecision
match decision {
    AllowOnce => { iteration_budget += N; continue; }
    AllowAndRemember(scope) => { TurnContext.iteration_budget = unlimited within scope; continue; }
    Deny => { graceful finish with partial result; }
}
```

### 11.6 UI 规约

所有 surface 渲染审批 UI 时遵守同一组规约：

- 显示 `tool_name` + 完整 `input`（JSON 高亮）
- 显示 `risk` 等级（Low / Medium / High / Critical）
- 提供 4 个动作：**Allow Once** / **Allow & Remember** / **Deny** / **Deny with feedback**
- 显示已等待时长
- 取消 run 按钮始终可用

### 11.7 Ask 提问协议（agent 主动问用户）

Agent 在执行过程中可以主动调用 `ask` 工具向用户发起问题。`ask` 是普通 Tool 实现，`classify` 返回 `NeedsHumanInput { kind: Question }`，dispatcher 见到此分类后调 `HitlGate.ask` 走问答路径。

**协议**（[crates/protocol/src/permission.rs](crates/protocol/src/permission.rs)）：

```rust
pub struct QuestionOption {
    pub label: String,        // 短标签（按钮文字）
    pub description: String,  // 详细说明（可选）
}

pub enum UserAnswer {
    Selected { label: String },  // 选了某个固定选项
    Custom { text: String },     // 自由输入框写的文字
    Cancelled,                   // ESC / 关闭弹窗
}

// EventPayload 新增两个变体
UserQuestionRequested { request_id, question, options },
UserQuestionAnswered { request_id, answer },

// Op 新增
AnswerQuestion { request_id, answer },
```

**Schema 约束**：`ask` 工具 input 要求 2-5 个选项。UI 始终额外提供一个「自由输入框」收纳其他意见。

**关键设计点**：

1. **`ask` 是普通 Tool**，靠 `ToolClass::NeedsHumanInput` 让 dispatcher 走 HitlGate
2. **审批与提问共用 HitlGate**：同一张 pending 表，靠 EventPayload 的 kind 字段路由 UI
3. **`ask` 是 builtin**，每轮强制注入；用户在工具菜单中无法关闭

**UI 规约**：

- CLI：`inquire::Select` 列表 + `↑↓` 选项 + 末项「其他（自由输入）」，**ESC** 取消
- Desktop：[UserQuestionPopup](../apps/desktop/frontend/src/desktop/ui/components/UserQuestionPopup.tsx) 选项卡片 + 末项「其他」内嵌 textarea；右下「取消 / 提交」；ESC 取消；Cmd/Ctrl+Enter 提交

---

## 12. 沙箱与执行隔离

Hebbian 当前主要工具是 `web_fetch` / `web_search`（IO，无副作用）。但 `Bash` / `Write` / `Edit` 这类 destructive 工具是迟早要加的。**沙箱设计与权限设计分开**：权限决定"是否允许做"，沙箱决定"做的时候被限制成什么样"。

### 12.1 SandboxPolicy

```rust
pub struct SandboxPolicy {
    pub fs: FsAccess,             // ReadOnly / WritableUnder(Vec<PathBuf>) / Unrestricted
    pub network: NetAccess,       // None / AllowedHosts(Vec<String>) / All
    pub exec: ExecAccess,         // None / AllowedPrograms(Vec<String>) / All
    pub timeout: Duration,
    pub memory_limit_mb: Option<u64>,
}
```

### 12.2 实现选型（按平台）

- **macOS**：Seatbelt（`sandbox-exec` + SBPL 文本策略）
- **Linux**：Landlock（已稳定）+ 可选 bubblewrap
- **Windows**：Job Objects + Restricted Token；初期可不实现，标记 unsupported
- **跨平台兜底**：纯 Rust 的 path-allowlist + reqwest host-allowlist，能覆盖 90% 文件 / 网络场景

`crates/sandbox/` 提供统一 trait，具体实现按 cfg 切换：

```rust
#[async_trait]
pub trait SandboxedExec: Send + Sync {
    async fn run(&self, cmd: Command, policy: &SandboxPolicy) -> SandboxResult;
}
```

**现状**：完全没有。**演进时机**：在引入 `Bash` 工具之前必须做（不是之后）。

---

## 13. 可维护性与代码风格约束

### 13.1 Rust 规约

- 每个 crate 一个 `lib.rs`，所有公共 API 在 `lib.rs` 里 `pub use`，外部只看到一层路径
- trait 与实现分文件：`tool.rs` 放 trait，`tools/web_fetch.rs` 放实现
- 错误类型每个 crate 自己定义（用 `thiserror`），跨 crate 错误用 `From` 转换
- `async_trait` 仅用于公共 trait，私有 trait 用原生 async fn（Rust 1.75+）
- 所有公共 enum 配 `#[serde(tag = "type", rename_all = "snake_case")]`，前端 union 类型直接对齐
- 所有 long-running 操作接 `CancelFlag`，永远不做 detached spawn

### 13.2 测试分层

```text
tests/                          仓库根：端到端测试（surface ↔ harness ↔ mock provider）
crates/agent-core/tests/        集成测试（多模块协作）
crates/*/src/**/tests.rs        单元测试（贴近模块）
```

每个对外 trait 必须有 mock 实现（`mock-*` feature 控制）。当前 `agent_loop` 已有几个 mock client，扩展即可。

### 13.3 文档规约

- 每个 crate 一个 `README.md`：一句话职责 + 主要 trait / struct
- 每个公共 trait 与公共 fn 必须有 rustdoc，写"为什么"而不是"是什么"
- 协议变更必须同步更新本文档 §2 与 §17

---

## 14. 工作清单

按优先级排序，每条都是可独立交付的工作单元。

### 阶段一：Core API 收敛

| # | 工作 | 涉及 |
|---|------|------|
| 1 | `crates/protocol` 抽出，`Submission/Op/Event` 集中 | protocol |
| 2 | per-run `seq`，由 `RunState` 维护 | agent-core |
| 3 | `PermissionDecision` 三态 + oneshot waiter HITL 通路 | agent-core |
| 4 | `RunHandle` 取代 `RunId` 反查：events 独享 mpsc + control 内化 gate/cancel | agent-core |
| 5 | `Session` 上升为 agent-core 一等公民：transcript / workspace / definition / client 内化 | agent-core |
| 6 | `TurnObserver` trait + `RunHandle::drive(&mut observer)`：surface 不再写事件循环 | agent-core, apps/* |
| 7 | `TurnContext` 收拢零散参数（model / tools / approval / sandbox / budget / iteration_budget） | agent-core |
| 8 | `Tool::classify` + `ToolCtx` + `ToolResult { outcome: Ok / Denied / Failed }` | agent-core |
| 9 | `HitlGate` 合并 PermissionGate + QuestionGate，统一 pending 表 | agent-core |
| 10 | `AgentLoop` 拆为 `ContextCompactor / ToolDispatcher` 等子对象，移除 800 行长函数 | agent-core |
| 11 | `LoopError` 分类型（Model / Cancelled / MaxIterations / Tool） | agent-core |
| 12 | `ask` 改为普通 Tool 实现（`NeedsHumanInput` 分类） | agent-core/tools |
| 13 | Hook 缩到 4 个拦截点（BeforeModelCall / OnPermissionCheck / OnToolResult / OnCompaction） | agent-core/hooks |
| 14 | Desktop pending 改队列模型，与 ToolClass 串行规则配套 | apps/desktop |

### 阶段二：性能与可观测

| # | 工作 | 涉及 |
|---|------|------|
| 15 | Prompt cache 边界：`ContextSnapshot` 三段切分 + provider 层 cache_control | agent-core/context, model-gateway |
| 16 | `platform/blob.rs` + `ToolContent::BlobRef` 自动落 blob | platform, agent-core |
| 17 | LLM 摘要压缩（L3）：`Compactor` trait + 默认实现 | agent-core/context |
| 18 | `crates/observability` 标准 Signal + Inspector UI | observability, apps/desktop |
| 19 | Model Gateway routing / retry / cost 字段 | model-gateway |

### 阶段三：持久化与扩展入口

| # | 工作 | 涉及 |
|---|------|------|
| 20 | 拆 platform：`storage/sessions` → `crates/persistence`；`config/prompts` → `crates/config` | platform → persistence/config |
| 21 | JSONL rollout + Resume / Fork / Rollback Op 实现 | persistence, agent-core |
| 22 | `crates/memory` fs 后端 + Memory Hook（BeforeModelCall 注入 / OnToolResult 写候选） | memory, agent-core |
| 23 | `AgentDefinition` YAML 加载 + 内置 5 个角色 | config, configs/ |
| 24 | `apps/server` HTTP + SSE：`submit(Op)` + `subscribe(run_id, since_seq)` 路径打通 | apps/server |
| 25 | `crates/channels` 框架 + Slack 适配 | channels |
| 26 | `apps/tui` 最小可用 | apps/tui |

### 阶段四：多 agent 与硬化

| # | 工作 | 涉及 |
|---|------|------|
| 27 | `RunTree` + `spawn_agent` / `spawn_parallel` 工具 | agent-core/multi_agent |
| 28 | `ContextPolicy::Isolated / InheritSummary / InheritSelected` 投影 | agent-core/context |
| 29 | `crates/sandbox` Seatbelt/Landlock 实现 | sandbox |
| 30 | Bash / Write / Edit 内置工具（带 sandbox） | agent-core/tools |
| 31 | MCP client | agent-core/tools 或独立 crate |
| 32 | OpenTelemetry exporter | observability |
| 33 | Plan 审批模式（L2 HITL，`submit_plan` 工具） | agent-core, configs |

---

## 15. 里程碑

### M1：Core API 收敛
**目标**：Surface 端不写事件循环，不持有 gate Arc，不调反查 API；core 内部 agent_loop 拆解为子对象、Tool 自报 HITL 类型、HitlGate 合并、Hook 收敛到 4 个拦截点。
- 阶段一 #1 ~ #14

### M2：性能与可观测
**目标**：prompt cache 命中率显著提升；长输出落 blob；Inspector 看得见 token / cost / tool timeline；L3 压缩可用。
- 阶段二 #15 ~ #19

### M3：持久化与扩展入口
**目标**：platform 拆分完成；崩溃后能 Resume / Fork / Rollback；记忆通过 Hook 注入；Desktop / TUI / HTTP / Slack 任一入口都能起 run。
- 阶段三 #20 ~ #26

### M4：多 agent 与硬化
**目标**：父子 run + 三种协作模式；Bash / Write / Edit 在 sandbox 内可用；接入 MCP 与 OTel。
- 阶段四 #27 ~ #33

---

## 16. 协议清单（最稳定的部分）

下面是协议的最终形态。任何修改要走"PR + 文档同步"流程。

```rust
// crates/protocol/src/lib.rs
pub mod ids;          // RunId / TurnId / SubmissionId / PermissionRequestId / MessageId / AgentRef
pub mod submission;   // Submission, Op, UserInput, TurnOverrides
pub mod event;        // Event, EventPayload, StopReason, RiskLevel, LogLevel
pub mod context;      // ContextPolicy, TokenBudget, TurnOverrides
pub mod permission;   // ApprovalDecision, PermissionKind, PermissionScope,
                      // QuestionOption, UserAnswer
pub mod error;        // ErrorReport, ErrorKind
```

五个最不可漂移的 enum：

```rust
pub enum Op { /* 见 §2.1，含 Approve / AnswerQuestion 两个 HITL 回应入口 */ }
pub enum EventPayload { /* 见 §2.2 */ }
pub enum ApprovalDecision { AllowOnce, AllowAndRemember{scope}, Deny, DenyWithFeedback{feedback} }
pub enum UserAnswer    { Selected{label}, Custom{text}, Cancelled }
pub enum ContextPolicy { /* 见 §3.6 */ }
```

---

## 17. 术语对照

| 术语 | 含义 |
|------|------|
| **Submission / Op** | 跨进程入口的统一请求 |
| **Event / EventPayload** | Core 向外的统一输出 |
| **Session** | agent-core 的会话对象，持有 transcript / workspace / definition / client |
| **Run** | 一次执行（一条 user message → 终止），由若干 Turn 组成 |
| **Turn** | 一次"用户输入 → 助手最终输出"的往返 |
| **Step** | 一次模型调用 + 工具执行批 |
| **TurnContext** | 一次 Turn 的所有显式参数（model / tools / approval / sandbox / budget / iteration_budget） |
| **Harness** | Core 工厂，跨 session 共享 ToolRegistry / HookManager / Gateway 依赖 |
| **HarnessHandle** | 给工具/channel 用的轻量 clone 句柄 |
| **RunHandle** | 一次 run 的本地句柄，独享事件流 + 控制方法，drop 即取消 |
| **TurnObserver** | Surface 接入 RunHandle 的 trait，封装事件渲染 + HITL 回调 |
| **HitlGate** | HITL 统一通道：审批 / 提问 / 路径 / 续跑共用同一张 pending 表 |
| **ToolClass** | 工具自报的分类：ReadOnly / Network / Mutating / Destructive / NeedsHumanInput |
| **ToolOutcome** | ToolResult 的三种结局：Ok / Denied / Failed |
| **ContextPolicy** | 子 agent 如何继承父上下文 |
| **ApprovalPolicy** | 工具审批的整体策略 |
| **PermissionRule** | 单条匹配工具 + input 的规则 |
| **SandboxPolicy** | 工具执行时的资源/能力限制 |
| **MemoryStore** | 记忆后端 trait |
| **RolloutStore** | 事件持久化后端 trait |
| **EventLog** | 单个 run 的 jsonl 文件 |
| **Surface** | 用户主动入口（Desktop / TUI / Server / CLI） |
| **Channel** | 自动/外部入口（Slack / Webhook / Cron） |
| **EventSource** | Submission 的来源标识，用于审计与策略分支 |
