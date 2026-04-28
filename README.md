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
| Python | 3.10+ | 跑 `scripts/test.py` 协议验证 |
| Xcode CLT | — | macOS：`xcode-select --install` |

### Desktop 模式

```bash
git clone <repo-url> && cd hebbian
pnpm install
pnpm tauri dev          # 首次 Rust 编译约 3–5 分钟
pnpm tauri build        # 产出在 apps/desktop/target/release/bundle/
```

### CLI 模式

```bash
cargo build -p hebbian-cli
CLI=./target/debug/hebbian-cli

# 真实模型：自动用 desktop 里配过的默认 provider + 默认 model
$CLI run "你好"

# 指定 provider + model（与 desktop 共享 data_dir）
$CLI run "你好" --provider <provider-id> --model claude-sonnet-4.5

# 启用工具
$CLI run "搜索一下 Hebbian rule" --tools web_search,web_fetch

# Mock provider（无需配 API key，用于协议测试）
$CLI run "你好" --mock
$CLI run "用工具" --mock --mock-tool-call --mock-needs-approval

# 交互模式：从 stdin 读 Submission，stdout 输出 Event NDJSON
echo '{"id":"s","op":{"type":"start_run","agent":"default","input":{"text":"hi"}}}' \
  | $CLI interactive --mock --auto-approve
```

输出格式：每行一个 `protocol::Event` JSON，写到 stdout；tracing 日志走 stderr。
管道接 `jq` 或 `python3 -c "import json; ..."` 解析。

### 协议验证

```bash
python3 scripts/test.py
# 4 个用例：seq 单调、Run/Turn 配对、TextDelta 累加、HITL 时序
```

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
│   └── cli/                      ★ Rust CLI / 协议测试 harness
│       └── src/
│           ├── main.rs           run / interactive 两个子命令
│           └── mock_provider.rs  确定性 mock，用于无网络协议测试
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
│   │       ├── harness.rs        Harness（submit/subscribe + 旧 run() 共存）
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
├── scripts/
│   └── test.py                   ★ 协议事件流验证器（4 个用例）
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

CLI `interactive` 模式直接收发这两个 JSON 协议；desktop 通过 Tauri IPC Channel 转译。

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
| 5 | Harness `submit/subscribe` actor 模式 | ✓（与旧 `run()` 共存） |
| 6 | `TurnContext` 抽象 | ✓（结构已立，loop 还在用 LoopParams） |
| 7 | 10 个 hook 点位 | ✓ |
| 8 | Tool trait `classify` / `ToolCtx` / `ToolResult` | ☐ |
| 9 | Desktop 审批 UI | ☐（core 已通，前端待消费 `permission_requested`） |

M2 ~ M4：persistence / memory / observability / multi-agent / channels / TUI / server / sandbox / MCP —— 见架构文档路线图。

---

## License

私人项目，按需使用。
