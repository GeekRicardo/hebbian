# Hebbian

一个轻量、本地优先的 AI agent 框架。**Agent = Model + Harness**，模型可换，harness 是产品本体。

核心理念：**一套核心、多 surface**——业务逻辑全在 `crates/agent-core`（唯一大脑），surface 只翻译输入、渲染输出，共享同一个 `~/.hebbian/` 数据目录，行为对称。四个 surface：

- **Desktop**：Tauri 2 + React 18 桌面客户端（GUI surface，含内置浏览器 / 终端 / 微信渠道等 native 能力）
- **heb CLI**：Rust binary，unix-socket + NDJSON 事件流，给 AI 脚本化自主调试用
- **hebweb**：axum HTTP + WebSocket server，浏览器 surface，与 Desktop 共享同一份 React 代码（transport 运行时探测走 Tauri 还是 WS）
- **channel-gateway**：渠道网关（微信，未来 QQ / 飞书）

三 surface 的对话事件流统一消费 `protocol::WireEvent`：core 内部 `EventPayload → to_wire → WireEvent`，surface 只做投递差异（Desktop emit / heb NDJSON / hebweb WS broadcast），不再各自定义事件类型 + 各自翻译。

完整架构见 [docs/架构.md](docs/架构.md)（唯一设计准则）。

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
pnpm --dir apps/desktop install
pnpm --dir apps/desktop tauri dev     # 首次 Rust 编译约 3–5 分钟
pnpm --dir apps/desktop tauri build   # 产出在 target/release/bundle/
```

### heb CLI（脚本化 surface）

`heb` 是 daemon 模式的命令行 surface：`heb new` 起一个常驻 session，把对话事件以 **NDJSON** 流式打到 stdout，其余子命令通过 unix-socket 控制它——给 AI 脚本化自主调试用（完整手册见 [docs/heb-cli-debug.md](docs/heb-cli-debug.md)）。

```bash
cargo build -p hebbian-cli           # 产出 ./target/debug/heb
HEB=./target/debug/heb

# 1) 起 daemon：新建 session，stdout 持续输出 NDJSON 事件流（首行含 session_id）
$HEB new --provider <id> --workdir /path/to/project > /tmp/heb.log 2>&1 &
SID=$(jq -r .session_id < <(head -n1 /tmp/heb.log))

# 2) 发消息（有活跃 run 自动注入，无则开新 run）；事件流在后台 log 里实时看
$HEB input "$SID" "src 下有哪些 rust 文件？"

# 3) HITL：事件流出现 permission_requested / question_requested 时回应
$HEB allow  "$SID" <request_id>                # 批准（--scope session|project|global 可记忆）
$HEB deny   "$SID" <request_id>
$HEB answer "$SID" <request_id> --kind selected --value "选项A"

$HEB stop "$SID"                               # 中断当前 run
$HEB list-sessions                             # 列已有 session

# 一次性无人值守跑完即退出（评测 / 脚本用：审批自动拒、提问自动取消，结尾可 --json 打结构化结果）
$HEB run "把 README 翻译成英文" --yolo --json
```

`heb new` 常用参数：`--provider <id|name/model>`、`-m/--model <m>`、`--workdir <dir>`、`--mode default|plan-mode|auto-mode|yolo`、`--session-id <id>`（连已有 session）、`--data-dir <path>`。

### hebweb（浏览器 surface）

`hebweb` 是 HTTP + WebSocket server，跑与 Desktop **同一份 React 代码**（前端运行时探测走 WS 而非 Tauri），适合远程访问 / 多人各开一个端口调试。

```bash
pnpm --dir apps/desktop build              # 首次 / 前端改动后：产出 apps/desktop/frontend/dist
cargo build -p hebbian-web-server          # 产出 ./target/debug/hebweb
./target/debug/hebweb --port 38080         # 然后浏览器打开 http://127.0.0.1:38080
```

参数：`--port <n>`（默认 3030）、`--static-dir <dir>`（默认自动探测 `apps/desktop/frontend/dist`）、`--data-dir <path>`。

> **hebcore（常驻核心进程）**：run 现在跑在独立的 `hebcore` 进程里（持唯一 dispatch + 全部活 session，架构 §7.8），三 surface 都作为客户端连入 `~/.hebbian/hebcore.sock` 看同一份活对话状态。**无需手动启动**——Desktop / hebweb 启动时会自动拉起，首个拿到 `~/.hebbian/hebcore.lock` 单例锁的进程即充当 hebcore。

### 单项检查

```bash
cargo check --workspace
pnpm --dir apps/desktop exec tsc --noEmit
pnpm --dir apps/desktop build
```

---

## 功能

| 能力 | 说明 |
|---|---|
| **多供应商** | OpenAI 兼容 / Anthropic / Gemini，独立 base_url + api_key |
| **OAuth 登录** | Anthropic 账号、Codex Device Flow、Gemini CLI 凭据导入 |
| **Agent 循环** | tool call + iteration 内自动推理，最大 10 轮 |
| **流式输出** | SSE 逐字追加，broadcast 事件总线给多 surface |
| **Human-in-the-Loop** | 工具审批三态门 + agent 主动提问（`ask` 工具，2-5 选项 + 自由输入框，ESC 取消） |
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
│   ├── desktop/                  Tauri + React 桌面应用
│   │   ├── frontend/             Vite / React 前端
│   │   │   ├── index.html        Vite 入口
│   │   │   └── src/
│   │   │       ├── App.tsx
│   │   │       ├── main.tsx
│   │   │       ├── index.css
│   │   │       ├── assets/       品牌与动效资源
│   │   │       └── desktop/
│   │   │           ├── bridge/   Tauri invoke + Channel 封装
│   │   │           └── ui/       React 组件、store、lib、types
│   │   ├── src/                  Tauri Rust 端
│   │   │   ├── lib.rs            Tauri 命令注册（IPC 入口）
│   │   │   ├── chat.rs           Harness 桥接，DesktopObserver 经 protocol::to_wire 推 WireEvent
│   │   │   ├── hitl.rs           Desktop HITL request/response 桥接
│   │   │   ├── title_gen.rs      会话标题自动生成
│   │   │   ├── engine/mod.rs     Tauri command 返回 Dto（TodoItemDto / PlanCommentDto）
│   │   │   └── window_control.rs 窗口管理、全局快捷键
│   │   ├── tauri.conf.json       Desktop 构建配置
│   │   ├── package.json          Desktop 前端脚本与依赖
│   │   ├── pnpm-lock.yaml        Desktop 前端锁文件
│   │   ├── vite.config.ts        Vite root / alias / dist 配置
│   │   ├── tsconfig.json         TypeScript include / path alias
│   │   ├── tailwind.config.cjs   Tailwind content 扫描前端路径
│   │   ├── postcss.config.cjs    PostCSS 插件配置
│   │   ├── capabilities/         Tauri 权限 capability
│   │   └── icons/                App / tray 图标资源
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
│   │       │   ├── mod.rs        Tool trait / default_tools / builtin_tool_definitions（ask 等内置）
│   │       │   ├── registry.rs   ToolRegistry
│   │       │   ├── permissions.rs PermissionGate（三态 + oneshot waiter）
│   │       │   └── question.rs   QuestionGate（ask 工具的 oneshot waiter）
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
├── docs/
│   └── architecture.md           完整架构设计文档（含 4 个里程碑路线图）
│
├── Cargo.toml                    Rust workspace 根
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
    Approve { request_id, decision },           // 工具审批回应
    AnswerQuestion { request_id, answer },      // ask 工具的回应
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
    PermissionRequested / PermissionResolved,         // HITL：工具审批
    UserQuestionRequested / UserQuestionAnswered,     // HITL：ask 工具
    ContextCompacted,
    Log,
}
```

`EventPayload` 是 core 内部领域模型（嵌套 enum、强类型）。对外通过**唯一转换 `protocol::to_wire(&Event) → WireEvent`** 降成线协议 DTO（字段拍平、enum 降成 `tag + payload`），三 surface 共享这一份（架构 §3.1.1）：

- **Desktop**：`Channel<WireEvent>` 经 Tauri IPC 推给前端
- **heb CLI**：`DaemonEvent`（WireEvent 业务事件 + daemon 信令 + 终端截断的超集）写 NDJSON
- **hebweb**：`WireEvent` 经 WS broadcast 推给浏览器

> 历史上这层转换写过三遍且不一致（desktop / cli / web 各一套）；2026-06 的步骤4 收口到单一 `to_wire`，业务事件字段（risk / decision / reason 等）逐字节一致，差异（cli 的 result 截断等）下沉各 surface 渲染层。

---

## 工具系统

| 类别 | 暴露给 UI 工具菜单 | 用户可关 | 例子 | 实现位置 |
|------|--------|---------|------|---------|
| **内置（builtin）** | ❌ | ❌ | `ask`（未来 `bash` / `read` / `write`） | `agent_loop` 直接派发 + `QuestionGate` / `PermissionGate` |
| **用户可选（registry）** | ✅ | ✅ | `web_search`、`web_fetch` | `Tool` trait 实现，`ToolRegistry` 注册 |
| **Hosted（provider 端运行）** | ✅ | ✅ | `image_generation` | 仅传 schema 给 provider，结果由 provider 返回 |

每轮 ModelRequest 的 tools = builtin（永远）+ registry 中 enabled 的 + hosted 中 enabled 的。

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

参考 [docs/架构.md](docs/架构.md) 的里程碑路线图：

| # | M1 项 | 状态 |
|---|------|------|
| 1 | `crates/protocol` + `Submission/Op` | ✓ |
| 2 | per-run seq | ✓ |
| 3 | `PermissionDecision` 三态 | ✓ |
| 4 | oneshot waiter HITL 通路 | ✓ |
| 5 | Harness `spawn_run` + `subscribe` actor 模式 | ✓（旧 `run()` 已删除） |
| 6 | `TurnContext` 抽象 | ◐（结构已立，loop 还在用 LoopParams） |
| 7 | 10 个 hook 点位 | ✓ |
| 8 | Tool trait `classify` / `ToolCtx` / `ToolResult` | ☐ |
| 9 | Desktop 工具审批弹窗（PermissionApprovalPopup） | ✓ |
| 10 | Ask 提问通路：`QuestionGate` + `ask` 内置工具 + `UserQuestion*` 协议 + CLI inquire 渲染 + Desktop `UserQuestionPopup` | ✓ |
| 11 | 内置工具与用户可选工具分离（`builtin_tool_definitions` 永远注入） | ✓ |

M2 ~ M4：persistence / memory / observability / multi-agent / channels / TUI / server / sandbox / MCP —— 见架构文档路线图。

---

## License

本项目采用 [PolyForm Noncommercial License 1.0.0](LICENSE)：源码公开，允许个人学习、研究、修改、再分发；**禁止任何商业用途**（包括但不限于销售、内嵌商用产品、对外提供付费服务、企业内部商业运营场景使用）。

商用授权请单独联系作者协商。

> 注：PolyForm Noncommercial 严格意义上不属于 OSI 定义的「开源」（OSI 第 6 条禁止限制使用领域），属于 source-available 协议。如需 OSI 认证的开源协议，目前不提供。
