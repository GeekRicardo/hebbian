# Hebbian 架构

> 本文是 Hebbian 项目的目标架构。它直接面向实现，每一节都包含三类信息：
>
> - **职责**：这一层对外提供什么能力，对内拆成什么子模块。
> - **现状**：仓库里当下的实现位置，列出关键 trait / struct / 文件路径。
> - **差距与演进**：要达到生产可用还需要补什么、按什么顺序补。
>
> 项目的核心判断是 **Agent = Model + Harness**：模型是可替换的，harness 是产品本体。本文不解释为什么如此，只描述这个判断落到代码上长什么样。

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
├── src/desktop/ui/        现状：React 组件、Tauri bridge、store
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

### 3.1 Harness

`Harness` 是 Core 的对外门面。它持有：

- 一个 `Submission` 接收端（在 actor task 里循环消费）
- 一张 `RunRegistry`：`RunId -> RunHandle`
- 一个 `Event` 广播总线（`tokio::sync::broadcast`），任何 surface 通过 `Subscribe` 拿到 receiver
- 依赖注入：`ModelClient`、`ToolRegistry`、`MemoryStore`、`HookManager`、`PermissionPolicy`、`PersistenceStore`

```rust
// crates/agent-core/src/harness.rs
pub struct Harness {
    runtime: Arc<HarnessRuntime>,    // 持有所有依赖与 RunRegistry
    submit_tx: mpsc::Sender<Submission>,
}

impl Harness {
    pub fn submit(&self, op: Op) -> Result<SubmissionId> { ... }
    pub fn subscribe(&self, run: &RunId, since_seq: Option<u64>) -> EventStream { ... }
    pub fn handle(&self) -> HarnessHandle { ... }   // 给 channel/tool 用的轻量句柄
}
```

设计要点：

- Harness 只暴露 **submit / subscribe** 两个动词。所有更复杂的交互都是 Op 的不同 variant。
- `HarnessHandle` 是 clone 友好的轻量句柄，传给工具、子 agent、channel adapter，避免循环引用。

**现状**：`crates/agent-core/src/harness.rs` 已有 `Harness` 结构，但目前是"调一次 run 一个生命周期"的模式（`pub async fn run(...)`），没有 SQ/EQ 队列也没有 `RunRegistry`。

**演进**：把 `run()` 拆成 `submit()` + 内部 actor。同一个 Harness 实例必须能同时持有多个并发 run。

### 3.2 Run / Turn / Step：三级生命周期

```text
Run     一次完整对话（可能跨多 turn，可被中断、压缩、fork、resume）
 └── Turn   一次"用户输入 → 助手最终输出"的往返
      └── Step   一次模型调用 + 0..N 个 tool 执行
```

**Turn 必须显式持有 TurnContext**（这一点直接借自 codex）：

```rust
pub struct TurnContext {
    pub model: ModelSelector,           // 这个 turn 用哪个 model
    pub tools: ToolSetSnapshot,         // 这个 turn 启用哪些 tool
    pub approval: ApprovalPolicy,       // 这个 turn 的审批策略
    pub sandbox: SandboxPolicy,         // 这个 turn 的沙箱策略
    pub context_policy: ContextPolicy,  // 给子 agent 用
    pub budget: TokenBudget,
    pub cwd: Option<PathBuf>,
}
```

意义：

- 同一个 run 的不同 turn 可以切换 model（"先 Sonnet 草拟、再 Opus 审"）
- Fork / Rollback 时不需要重建配置——直接复制 `TurnContext`
- 所有审批与沙箱判定都在 `TurnContext` 范围内做，避免隐式全局状态

**现状**：`agent_loop.rs` 直接接收一堆参数（`enabled_tools`, `compaction_policy`, `stream`, ...），没有 `TurnContext` 这个抽象。

**演进**：把这些参数收拢成 `TurnContext`，由 `Op::StartRun.turn_overrides` 和 `AgentDefinition` 共同决定。

### 3.3 Agent Loop

Loop 是 Core 中最稳定、也是最容易被乱改的部分。规约如下：

```text
loop:
  1. 检查取消
  2. 触发 BeforeTurn hooks（含 memory 注入）
  3. 检查 token budget，必要时调用 Context Engine 压缩
  4. 组装 ModelRequest（system + transcript + tools）
  5. 调用 Gateway，按 stream 转发 TextDelta / ToolCallDelta
  6. 模型返回：
     - Done → 触发 AfterTurn hooks → 返回
     - ToolCalls → 进入 7
  7. 对每个 tool call：
     - 触发 BeforeToolCall hooks
     - 询问 PermissionGate（可能挂起等待 Approve）
     - 在 sandbox 里执行
     - 触发 AfterToolCall hooks
     - 把结果写回 transcript
  8. iteration += 1，回到 1
  9. 达到 MAX_ITERATIONS 时不直接 fail，而是询问用户是否继续
```

**现状**（`crates/agent-core/src/agent_loop.rs`）：

- 已有 1、3、4、5、6、7 的主体（含工具并发执行 `join_all`）
- hooks 在 BeforeTool / AfterTool 已挂上
- 压缩在循环顶端做，使用 `compact_structural`
- **缺**：BeforeTurn / AfterTurn hook 点
- **缺**：超过 `MAX_TOOL_ITERATIONS=10` 直接 `RunFailed`，没有"询问继续"的口子（详见 §11.5）
- **缺**：permission gate 当前只有 Allowed / Denied 两态，没有"挂起等用户审批"的第三态

**演进**：

```rust
pub enum PermissionDecision {
    Allowed,
    Denied { reason: String },
    NeedsApproval { request_id: PermissionRequestId, kind: PermissionKind },
}
```

当 loop 拿到 `NeedsApproval` 时：发出 `PermissionRequested` 事件 → 在 `Run` 状态里登记一个 oneshot waiter → 暂停该 tool 的执行 → 直到 `Op::Approve` 到达 → resolve waiter → 继续。**注意**：同一 turn 内的其他 tool call 应该并行不阻塞，每个 tool 各自挂自己的 waiter。

### 3.4 Tool 系统

```rust
// crates/agent-core/src/tools/mod.rs
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;          // name + description + JSON schema
    fn classify(&self, input: &Value) -> ToolClassification;  // ReadOnly / Mutating / Destructive / Network
    async fn invoke(&self, ctx: ToolCtx<'_>, input: Value) -> ToolResult;
}

pub struct ToolCtx<'a> {
    pub run_id: &'a RunId,
    pub turn_ctx: &'a TurnContext,
    pub harness: &'a HarnessHandle,       // 让工具可以 spawn 子 run
    pub cancel: CancelFlag,
    pub blob_store: &'a dyn BlobStore,    // 用来把大输出落 blob
}

pub struct ToolResult {
    pub content: ToolContent,             // Text / Json / BlobRef / Multi
    pub metadata: ToolMetadata,           // 是否截断、原始大小、外链等
}
```

设计要点：

- **`classify` 是权限系统的核心输入**。`Destructive` 默认需要审批，`ReadOnly` 默认放行。
- `ToolResult` 不强制把所有内容塞进 transcript：网页正文、大文件、长日志默认走 `BlobRef`，transcript 里只放 preview + ref。这是 Context Engine 第一层压缩（见 §3.6）。
- 工具通过 `ctx.harness` 可以 `submit(StartRun {...})` 起子 agent —— `spawn_agent` 工具不再是特殊魔法，而是普通工具。

**现状**：

- `Tool` trait 已有 `name / description / parameters_schema / execute(input) -> AppResult<String>`
- 已实现的工具：`web_fetch`、`web_search`
- **缺**：`classify` 方法、`ToolCtx` 上下文、`ToolResult` 结构（当前直接返回 `String`）
- **缺**：`BlobStore` 接入（大输出现在直接被截断到 6000 字符塞回 transcript）

**演进顺序**：先补 `classify`（HITL 必需），再补 `ToolCtx`（multi-agent 必需），最后补 `BlobRef`（长上下文必需）。

### 3.5 Permission Gate

详见 §11，本节只列结构。

```rust
pub struct PermissionGate {
    policy: ApprovalPolicy,
    rules: Vec<PermissionRule>,           // 用户/项目/会话级累积
    pending: Mutex<HashMap<PermissionRequestId, oneshot::Sender<ApprovalDecision>>>,
}

impl PermissionGate {
    pub async fn check(&self, tool: &dyn Tool, input: &Value, ctx: &TurnContext)
        -> PermissionDecision { ... }

    pub fn resolve(&self, req: PermissionRequestId, decision: ApprovalDecision);
}
```

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
- **子 agent 继承不是复制 transcript**：是按 `ContextPolicy` 投影。

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

子 agent 不是"工具技巧"，是 Core 协议的一等公民。

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

Hook 是 Core 的官方扩展点，用于注入"非 agent loop 主线"的关注点：memory、observability、自定义审计、shell 命令包装。

```rust
pub enum HookPoint {
    BeforeRun, AfterRun,
    BeforeTurn, AfterTurn,
    BeforeModelCall, AfterModelCall,
    BeforeToolCall { tool: String },
    AfterToolCall { tool: String },
    BeforePermissionRequest,
    OnContextCompaction,
    OnMemoryWriteCandidate,
    OnError,
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn matches(&self, point: &HookPoint) -> bool;
    async fn invoke(&self, ctx: HookCtx<'_>) -> HookOutcome;
}

pub enum HookOutcome {
    Continue,
    Modify(HookPatch),                              // 改 transcript / 加 system prefix / 改 input
    Block { reason: String },                       // 拦截这次操作
}
```

Memory 写入和注入应该实现成两个内置 hook，而不是写死在 loop 里。

**现状**：`hooks/manager.rs` 与 `HookPoint` 已存在，loop 里挂了 `BeforeTool / AfterTool`。**缺** `BeforeRun / AfterRun / BeforeTurn / AfterTurn / BeforeModelCall / AfterModelCall` 等点位。

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

- 启动时获取 `HarnessHandle`
- 把用户输入翻译成 `Op`，调 `submit()`
- 调 `subscribe()` 拿 `EventStream`，渲染
- 不持有任何 agent 业务状态

### 9.2 Desktop（现状重点）

```text
apps/desktop/
├── src/                          Tauri Rust 端
│   ├── main.rs / lib.rs          Tauri 启动 + command 注册
│   ├── bridge.rs                 Op ↔ Tauri command 的薄翻译层
│   └── chat.rs                   现状：直接调用 harness.run()，演进为 submit/subscribe
└── ...
src/desktop/                       前端
├── ui/                            React 组件
├── bridge/tauri.ts                IPC 封装
└── store/                         前端状态
```

**现状**：

- Tauri command 已有：`get_providers`、`save_providers`、`upsert_provider`、`list_provider_presets`、`fetch_provider_models`、`test_provider_model`、`list_prompts`、`upsert_prompt`、`delete_prompt`、`set_default_prompt`、`list_sessions`、`send_message`
- 流式：`Channel<EngineEvent>` 已通
- **缺** Inspector UI、permission approval UI

**演进重点**：

- `send_message` 拆成多个 `submit(Op::*)` 包装
- 新建 `bridge.rs`，让 Tauri command 只做 Op 翻译，不调业务
- 前端订阅 `EventStream`，permission UI 直接消费 `PermissionRequested` 事件

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

### 11.1 三个层次的 HITL

| 层次 | 目的 | 触发 | 阻塞性 |
|------|------|------|--------|
| **L1 Inline 审批** | "可以执行这个工具吗？" | tool 调用前 | 阻塞该 tool（其他 tool 仍并行） |
| **L2 Plan 审批** | "可以按这个计划继续吗？" | run 中段或开头 | 阻塞 run |
| **L3 终端确认** | "这次结果接受吗？" | run 结束前 | 阻塞 run finalize |

L1 是 Hebbian 当前最缺的、也是最先必须做的。

### 11.2 L1：Tool 审批协议

```text
loop ──(检测到 destructive 工具)──► PermissionGate.check
PermissionGate ──► 无明确规则 ──► NeedsApproval { request_id, kind }
loop ──► emit PermissionRequested 事件 + 在 Run 上挂 oneshot waiter
                       │
                       │ (该 tool 的执行 future 在 waiter 上 await)
                       │
Surface 渲染审批 UI ──► 用户点击 ──► Op::Approve { request_id, decision }
                       │
Harness ──► PermissionGate.resolve(request_id, decision)
                       │
                       │ ──► oneshot waiter 收到 decision
                       │
loop ──► 根据 decision：
          AllowOnce            → 执行该 tool
          AllowAndRemember     → 写入会话/项目级规则后执行
          Deny                 → 不执行；把"被用户拒绝"作为 ToolResult 回灌
          DenyWithFeedback(s)  → 同上，把 s 作为 user message 注入下一轮
```

要点：

- **同 turn 多个 tool call 各自挂自己的 waiter**，互不阻塞
- **超时**：每个 PermissionRequest 默认 5 分钟超时，超时按策略处理（Slack 默认 deny，Desktop 默认仍等待）
- **取消传播**：`Op::Interrupt` 必须级联 cancel 所有挂起的 waiter
- **持久化**：审批结果作为 `PermissionResolved` 事件写入 rollout，replay 时不重复询问

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

### 11.5 L3：超长 Run 的兜底

`MAX_TOOL_ITERATIONS=10` 这种硬上限应该改为可配置，且达到时不直接失败：

```rust
// 当前
return Err(ModelError::Other(format!("已达到最大工具调用轮数 {}", MAX_TOOL_ITERATIONS)));

// 演进
emit PermissionRequested { kind: ContinueLongRun, summary: "已迭代 10 轮，是否继续？", risk: Medium }
await ApprovalDecision
match decision {
    AllowOnce => { iteration_budget += 10; continue; }
    AllowAndRemember(scope) => { TurnContext.iteration_budget = unlimited (in scope); continue; }
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

**现状**：`AgentEventPayload::PermissionRequested` 已经定义，但 loop 里从未发出过这个事件，gate 也只有 Allowed/Denied 两态。

**演进里程碑**：HITL 闭环（gate 三态 → emit Requested → Op::Approve → resolve waiter → 持久化）是阶段一的关键交付。

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

## 14. 当前 → 目标的差距清单

按优先级排序，每条都是可独立交付的工作单元。

### 阶段一：Core 闭环可用（HITL 必须）

| # | 工作 | 涉及 |
|---|------|------|
| 1 | 抽 `crates/protocol`，迁移 `AgentEvent`，新增 `Submission/Op` | protocol、agent-core、apps/desktop |
| 2 | `seq` 改为每 run 私有 | agent-core/agent_loop |
| 3 | `PermissionDecision` 三态化（Allowed / Denied / NeedsApproval） | agent-core/tools/permissions |
| 4 | Loop 实现"挂起等审批"通路（oneshot waiter） | agent-core/agent_loop, harness |
| 5 | Harness 改为 `submit/subscribe` actor 模式 | agent-core/harness |
| 6 | `TurnContext` 抽象，替换零散参数 | agent-core |
| 7 | 全套 hook 点位（BeforeRun/AfterRun/BeforeTurn/AfterTurn/BeforeModelCall/AfterModelCall） | agent-core/hooks |
| 8 | Tool trait 加 `classify`、`ToolCtx`、`ToolResult` | agent-core/tools |
| 9 | Desktop 前端审批 UI + Op 翻译层 | apps/desktop, src/desktop/ui |

### 阶段二：生产可用基础设施

| # | 工作 | 涉及 |
|---|------|------|
| 10 | **拆 platform**：`storage/sessions` → 新 `crates/persistence`；`config/prompts` → 新 `crates/config` | platform、persistence、config、agent-core、apps/desktop |
| 11 | `crates/persistence` 加 JSONL rollout（与 #10 在同一 crate） | persistence、agent-core |
| 12 | Resume / Fork / Rollback 三个 Op 落地 | persistence、agent-core |
| 13 | `crates/memory` fs 后端 + 内置注入/写候选 hook | memory、agent-core |
| 14 | `crates/observability` 标准 Signal + Inspector UI 雏形 | observability、apps/desktop |
| 15 | `platform/blob.rs` + 工具结果落 blob | platform、agent-core |
| 16 | 完整的 LLM 摘要压缩（L3） | agent-core/context |
| 17 | Model Gateway 加 routing / retry / cost | model-gateway |
| 18 | AgentDefinition YAML 加载 + 内置 5 个角色（在 `crates/config` 里做） | config、configs/ |

### 阶段三：多 agent 与扩展入口

| # | 工作 | 涉及 |
|---|------|------|
| 19 | `RunTree` + `spawn_agent` 工具 + `ContextPolicy::Isolated` | agent-core/multi_agent |
| 20 | `spawn_parallel` + `JoinAll/JoinFirst` | agent-core/multi_agent |
| 21 | InheritSummary / InheritSelected 上下文继承 | agent-core/context |
| 22 | `apps/server` HTTP + SSE | apps/server |
| 23 | `crates/channels` 框架 + Slack 适配 | channels |
| 24 | `apps/tui` 最小可用 | apps/tui |

### 阶段四：扩展与硬化

| # | 工作 | 涉及 |
|---|------|------|
| 25 | `crates/sandbox` Seatbelt/Landlock 实现 | sandbox |
| 26 | Bash / Write / Edit 内置工具（带 sandbox） | agent-core/tools |
| 27 | MCP client（支持外部工具） | agent-core/tools 或独立 crate |
| 28 | OpenTelemetry exporter | observability |
| 29 | Plan 审批模式（L2 HITL） | agent-core, configs |

---

## 15. 落地路线图（4 个里程碑）

### M1：HITL 闭环（2-3 周）
**目标**：Desktop 上能看到"AI 想跑 X 工具，是否允许"对话框，能 Allow / Deny / Always Allow，能持久化。
- 阶段一 #1 ~ #9

### M2：可持久 / 可观测（2-3 周）
**目标**：platform 拆分完成；崩溃后能 Resume；Inspector 能看 token / cost / tool timeline；记忆能注入。
- 阶段二 #10 ~ #18

### M3：多 agent + Server（3-4 周）
**目标**：能从 Desktop / TUI / HTTP 三种 surface 起 run；能 spawn 子 agent；Slack channel 能跑。
- 阶段三 #19 ~ #24

### M4：生产硬化（持续）
**目标**：能安全跑 Bash 工具；接入 MCP；OTel 上链路。
- 阶段四 #25 ~ #29

---

## 16. 协议清单（最稳定的部分）

下面是协议的最终形态。任何修改要走"PR + 文档同步"流程。

```rust
// crates/protocol/src/lib.rs
pub mod ids;          // RunId / TurnId / SubmissionId / PermissionRequestId / MessageId / AgentRef / ProjectId
pub mod submission;   // Submission, Op, ApprovalDecision, TurnOverrides
pub mod event;        // Event, EventPayload, StopReason, RiskLevel
pub mod context;      // TurnContext, TurnContextSummary, ContextPolicy, TokenBudget
pub mod permission;   // PermissionKind, PermissionScope, ApprovalPolicy, PermissionRule
pub mod tool;         // ToolSpec, ToolClassification, ToolResultSummary
pub mod usage;        // Usage, UsageTotals, ModelSelector
pub mod error;        // ErrorReport
pub mod trace;        // TraceContext (W3C)
```

四个最不可漂移的 enum：

```rust
pub enum Op { /* 见 §2.1 */ }
pub enum EventPayload { /* 见 §2.2 */ }
pub enum ApprovalDecision { /* 见 §2.1 */ }
pub enum ContextPolicy { /* 见 §3.6 */ }
```

---

## 17. 术语对照

| 术语 | 含义 |
|------|------|
| **Submission / Op** | 外界进入 Core 的统一请求 |
| **Event / EventPayload** | Core 向外的统一输出 |
| **Run** | 一次完整对话，由若干 Turn 组成 |
| **Turn** | 一次"用户输入 → 助手最终输出"的往返 |
| **Step** | 一次模型调用 + 工具执行批 |
| **TurnContext** | 一次 Turn 的所有显式参数（model/tools/approval/sandbox/budget） |
| **Harness** | Core 对外的门面，只有 submit/subscribe |
| **HarnessHandle** | 给工具/channel 用的轻量 clone 句柄 |
| **RunTree** | 父子 run 的关系图 |
| **ContextPolicy** | 子 agent 如何继承父上下文 |
| **ApprovalPolicy** | 工具审批的整体策略 |
| **PermissionRule** | 单条匹配工具+input 的规则 |
| **SandboxPolicy** | 工具执行时的资源/能力限制 |
| **MemoryStore** | 记忆后端 trait |
| **RolloutStore** | 事件持久化后端 trait |
| **EventLog** | 单个 run 的 jsonl 文件 |
| **Surface** | 用户主动入口（Desktop/TUI/Server） |
| **Channel** | 自动/外部入口（Slack/Webhook/Cron） |
| **EventSource** | Submission 的来源标识，用于审计与策略分支 |
