# Hebbian

一个轻量、本地优先的 AI agent 框架。**Agent = Model + Harness**，模型可换，harness 是产品本体。

包含两个 surface：

- **Desktop**：Tauri 2 + React 18 桌面客户端（macOS 原生标题栏 + HSL 主题）
- **CLI**：Rust binary，配合 mock provider 与 NDJSON I/O，用于协议验证 / headless 跑模型

完整架构见 [docs/architecture.md](docs/architecture.md)。

---

## 快速开始

### 环境依赖

| 工具 | 最低版本 | 说明 |
|---|---|---|
| Rust | 1.80+ | `rustup` stable |
| Node.js | 18+ | 推荐 22 |
| pnpm | 8+ | `npm i -g pnpm` |
| Xcode CLT | — | macOS：`xcode-select --install` |

### Desktop 模式

```bash
git clone <repo-url> && cd hebbian
pnpm install
pnpm tauri dev          # 首次 Rust 编译约 3–5 分钟
pnpm tauri build        # 产出在 apps/desktop/target/release/bundle/
```

### CLI / TUI 模式

```bash
cargo build -p hebbian-cli
CLI=./target/debug/hebbian-cli

# 1) 交互 loop（默认）：rustyline readline，多 turn 上下文累积
$CLI                              # Ctrl+D 或 /exit 退出

# 2) 单次 query：发起一次请求，流式输出后退出
$CLI "用一句话介绍 Hebbian 学习规则"
$CLI "搜一下 wikipedia" --tools web_search,web_fetch

# 3) JSON 多轮上下文：吃下完整对话历史，跑最后一条 user message
$CLI --json '{"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"嗨"},{"role":"user","content":"刚才我说啥"}]}'
$CLI --json -                     # 从 stdin 读 JSON

# 共享选项
--provider <id>                   # 默认用 desktop 里配过的 default provider
-m / --model <name>
-s / --system <text>
--tools web_search,web_fetch
--mock                            # 不调真实模型，输出固定假回复
--data-dir <path>                 # 默认与 desktop 共享 ~/Library/Application Support/dev.ricardo.hebbian/
```

终端中流式逐字输出文本、工具调用以彩色 `🔧 web_search(...)` 显示、stderr 输出耗时 / token 用量。
管道（`| jq`、`| less`）时自动禁用 ANSI 颜色（依赖 `colored` crate 的 tty 检测）。

### 单项检查

```bash
cargo check --workspace
pnpm exec tsc --noEmit
pnpm build
```

---

## 功能

| 能力 | 说明 |
|---|---|
| **多供应商** | OpenAI 兼容 / Anthropic / Gemini，独立 base_url + api_key |
| **OAuth 登录** | Anthropic 账号、Codex Device Flow、Gemini CLI 凭据导入 |
| **Agent 循环** | tool call + iteration 内自动推理，最大 10 轮 |
| **流式输出** | SSE 逐字追加，broadcast 事件总线给多 surface |
| **Human-in-the-Loop** | 三态权限门：Approved / Denied / NeedsApproval（async oneshot waiter） |
| **Hooks** | BeforeRun / AfterRun / BeforeTurn / AfterTurn / BeforeModelCall / AfterModelCall / BeforeToolCall / AfterToolCall / BeforePermissionRequest / OnContextCompaction |
| **Per-run seq** | 事件流 seq 在每个 run 内单调递增，可断线重连 |
| **会话持久化** | session JSON，按 updated_at 倒序，支持搜索 / fork / 重新生成 |
| **上下文压缩** | 结构化裁剪（保留 system + 最近 N 轮）；LLM 摘要待实现 |
| **Markdown 渲染** | 表格 / 代码块 / 引用，用户消息保留原文换行 |
| **深色 / 浅色主题** | HSL CSS 变量，localStorage 记忆 |

---

## 架构

```
                    ┌─────────────────────────────────────┐
                    │  Surfaces                            │
                    │  Desktop (Tauri+React) │ CLI │ ...   │
                    └─────────┬─────────────────┬──────────┘
                  Submission/Op │             ▲ Event (NDJSON / IPC / SSE)
                                ▼             │
                    ┌─────────────────────────────────────┐
                    │  agent-core / Harness               │
                    │                                      │
                    │  submit / subscribe (actor)          │
                    │  run() (旧 API，兼容 desktop)        │
                    │                                      │
                    │  agent_loop                          │
                    │  ToolRegistry + PermissionGate(三态) │
                    │  Context + Compaction                │
                    │  Hooks (10 个 lifecycle points)      │
                    │  RunState (per-run seq + turn)       │
                    └─────────┬───────────────────────────┘
                              │ ModelRequest / ModelStreamEvent
                    ┌─────────▼───────────────────────────┐
                    │  model-gateway                       │
                    │  ModelClient trait                   │
                    │  protocols: openai/anthropic/gemini  │
                    │  auth: api_key / oauth + refresh     │
                    └─────────────────────────────────────┘

  protocol crate ── 所有人共享：Submission / Op / Event / EventPayload /
                    ApprovalDecision / PermissionKind / ContextPolicy / 各类 ID
```

---

## 目录结构

```
hebbian/
├── apps/
│   ├── desktop/                  Tauri 桌面应用壳
│   │   └── src/
│   │       ├── lib.rs            Tauri 命令注册（IPC 入口）
│   │       ├── chat.rs           Harness 桥接，AgentEvent → EngineEvent
│   │       ├── title_gen.rs      会话标题自动生成
│   │       ├── engine/mod.rs     EngineEvent（Tauri Channel）
│   │       └── window_control.rs 窗口管理、全局快捷键
│   │
│   └── cli/                      ★ 终端 surface（loop / 单次 / JSON 多轮）
│       └── src/
│           ├── main.rs           入口、模式分派、ModelClient 构建
│           ├── session.rs        Session：transcript、单 turn 跑通、loop 交互
│           ├── render.rs         Event → 终端彩色输出
│           └── mock_provider.rs  无网络环境下的固定假回复
│
├── crates/
│   ├── protocol/                 ★ 协议层（所有人都依赖它）
│   │   └── src/
│   │       ├── ids.rs            RunId / TurnId / SubmissionId / PermissionRequestId
│   │       ├── submission.rs     Submission, Op, UserInput, TurnOverrides
│   │       ├── event.rs          Event, EventPayload, StopReason, RiskLevel
│   │       ├── permission.rs     ApprovalDecision, PermissionKind, PermissionScope
│   │       ├── context.rs        ContextPolicy, TokenBudget
│   │       └── error.rs          ErrorReport, ErrorKind
│   │
│   ├── agent-core/               ★ 产品核心 / Harness
│   │   └── src/
│   │       ├── harness.rs        Harness（spawn_run + subscribe，actor 风格）
│   │       ├── agent_loop.rs     主循环（HITL waiter / 工具并发 / hook 触发）
│   │       ├── run_state.rs      RunState（per-run seq + turn 计数）
│   │       ├── turn_context.rs   TurnContext（model / tools / 预算 / 策略）
│   │       ├── definition.rs     AgentDefinition, CompactionPolicy, PermissionPolicy
│   │       ├── tools/
│   │       │   ├── mod.rs        Tool trait + 内置工具
│   │       │   ├── registry.rs   ToolRegistry
│   │       │   └── permissions.rs PermissionGate（三态 + oneshot waiter）
│   │       ├── context/
│   │       │   ├── transcript.rs Transcript
│   │       │   ├── budget.rs     token 估算
│   │       │   └── compaction.rs 结构化裁剪
│   │       ├── hooks/            Hook trait + 10 个生命周期 HookPoint
│   │       └── types.rs          protocol facade（向后兼容）
│   │
│   ├── model-gateway/            ★ 统一模型访问
│   │   └── src/
│   │       ├── client.rs         ModelClient trait
│   │       ├── types.rs          ModelRequest / ModelResponse / Usage
│   │       ├── config.rs         Provider CRUD + 预设
│   │       ├── auth/             api_key / oauth / refresh
│   │       ├── discovery/        模型列表拉取
│   │       ├── protocols/        openai / anthropic / gemini wire format
│   │       └── providers/        HTTP client 实现
│   │
│   └── platform/                 基础设施层
│       └── src/
│           ├── error.rs          AppError, AppResult
│           ├── runtime.rs        CancelFlag
│           ├── attachments.rs    MessageAttachment
│           ├── storage/sessions.rs  ← 计划迁出到 crates/persistence
│           └── config/prompts.rs    ← 计划迁出到 crates/config
│
├── src/                          前端 React
│   ├── App.tsx
│   ├── store/useStore.ts         Zustand 全局状态
│   ├── api/tauri.ts              invoke 封装 + Channel 流式订阅
│   └── components/
│
├── docs/
│   └── architecture.md           完整架构设计文档（含 4 个里程碑路线图）
│
├── Cargo.toml                    workspace 根
├── package.json
└── README.md                     本文件
```

---

## 协议（事件流）

CLI 和 desktop 共享同一套 `protocol` crate。所有 surface 通过两个动词与 core 通信：

**入：** `Submission { id, op }` —— 见 [crates/protocol/src/submission.rs](crates/protocol/src/submission.rs)

```rust
pub enum Op {
    StartRun { agent, input, turn_overrides, parent },
    SendUserMessage { run_id, input },
    Approve { request_id, decision },
    Interrupt { run_id },
    Subscribe { run_id, since_seq },
    Compact { run_id },
    Rollback { run_id, to_turn },
    Fork { from, at_turn, agent },
}
```

**出：** `Event { run_id, seq, at_ms, payload }` —— 见 [crates/protocol/src/event.rs](crates/protocol/src/event.rs)

```rust
pub enum EventPayload {
    RunStarted / RunFinished / RunFailed / RunCancelled,
    TurnStarted / TurnFinished,
    TextDelta / TextDone / Reasoning,
    ToolCallDelta / ToolCallStarted / ToolCallFinished,
    PermissionRequested / PermissionResolved,     // HITL
    ContextCompacted,
    Log,
}
```

desktop 通过 Tauri IPC `Channel<EngineEvent>` 把协议事件转译给前端；CLI 直接调 `Harness::spawn_run` 后订阅事件流渲染到终端。

---

## 数据存储

CLI 与 Desktop **共享同一个 data_dir**——在 desktop 配过的 provider / OAuth 凭据，CLI 可直接复用。

| 平台 | 路径 |
|------|------|
| macOS | `~/Library/Application Support/dev.ricardo.hebbian/` |
| Linux | `~/.local/share/dev.ricardo.hebbian/` |
| Windows | `%APPDATA%\dev.ricardo.hebbian\` |

```
data_dir/
├── providers.json
├── prompts.json
└── sessions/
    └── {YYYY-MM-DD}/
        └── {uuid}.json
```

如果想隔离 CLI 数据，传 `--data-dir <path>` 覆盖。

---

## 使用 Desktop

1. 点击侧边栏左下角 **🖥 供应商** 配置 API Key
2. 点击 **➕ 新建对话** 开始
3. 右下角切换 **⚡ 流式 / 一次性**
4. 消息悬停可见：**复制 / 分叉 / 重新生成**
5. 顶栏 **对话设置** 切换模型 / system prompt
6. 输入框左侧工具图标启用 **Agent 模式**（web_search / web_fetch）

---

## 当前进度（M1）

参考 [docs/architecture.md §14](docs/architecture.md) 的 4 个里程碑：

| # | M1 项 | 状态 |
|---|------|------|
| 1 | `crates/protocol` + `Submission/Op` | ✓ |
| 2 | per-run seq | ✓ |
| 3 | `PermissionDecision` 三态 | ✓ |
| 4 | oneshot waiter HITL 通路 | ✓ |
| 5 | Harness `spawn_run` + `subscribe` actor 模式 | ✓（旧 `run()` 已删除） |
| 6 | `TurnContext` 抽象 | ✓（结构已立，loop 还在用 LoopParams） |
| 7 | 10 个 hook 点位 | ✓ |
| 8 | Tool trait `classify` / `ToolCtx` / `ToolResult` | ☐ |
| 9 | Desktop 审批 UI | ☐（core 已通，前端待消费 `permission_requested`） |

M2 ~ M4：persistence / memory / observability / multi-agent / channels / TUI / server / sandbox / MCP —— 见架构文档路线图。

---

## License

私人项目，按需使用。
