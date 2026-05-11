# Hebbian 修改时间线（Changelog）

> **这是一个只增不减的修改时间线**。每次有 agent / 人改了仓库，都要在文件**末尾追加**一条记录。
>
> 目的：仓库主人是「想到什么就让 AI 改」的工作模式，需要一份**始终可信的回溯线索**，让下一个 agent 在动手前能看到：
> - 这个想法之前是不是讨论过、改过、又退回来过
> - 当前做法背后有没有一个我看不到的旧理由
> - 我现在的方案会不会和上周的某次改动冲突
>
> 没有这份时间线，AI 只能从代码现状反推意图，**很容易把别人慎重决策的结果当成代码债清掉**。

---

## 写入规则

1. **只追加，不修改、不删除旧条目**。旧条目错了 → 在新条目里更正它（引用旧条目日期）
2. **一次有意义的改动 = 一条**。一次会话里改了多个无关的事就写多条；不要把所有事塞进一条
3. **必须包含**：
   - 日期（`YYYY-MM-DD`，使用绝对日期，不写「今天/昨天」）
   - 改动一句话总结（动词开头：新增 / 重构 / 修复 / 删除 / 调整）
   - **Why**：为什么这么改（用户痛点 / 设计修正 / bug / 路线图推进）
   - **影响范围**：动了哪些 crate / 文件 / surface / 协议
   - **留尾巴**：有没有埋下未完成的事、TODO、可能的回归点（没有就写「无」）
4. **可选**：commit hash、关联 PR、相关 docs 链接
5. 不要写「what code did」级别的 diff —— 那是 git log 的事；这里写**意图、权衡、后果**

---

## 模板（复制改用）

```markdown
### 2026-MM-DD — <一句话总结>

- **Why**: <用户原话或场景；为什么这是问题>
- **改动**:
  - <主要文件 / 模块 1>: <做了什么>
  - <主要文件 / 模块 2>: <做了什么>
- **影响范围**: <哪些 crate / surface / 协议；是否破坏兼容>
- **留尾巴**: <未完成项 / 已知风险 / 后续要做的事；没有写「无」>
- **关联**: <commit / PR / docs 链接，可选>
```

---

## 时间线

<!-- 新条目追加在文件末尾 -->

### 2026-05-10 — 新增「动手前必做」流程 + changelog 时间线

- **Why**: 项目主人的工作模式是「想到什么就让 AI 改」，多次出现 AI 局部最优、违背初衷、与既有设计冲突、给未来埋坑。需要在 CLAUDE.md 里强制 agent 先读文档 + 做全局影响评估，并落一份只增不减的时间线作为回溯依据
- **改动**:
  - [CLAUDE.md](../CLAUDE.md): 顶部新增「⚠️ 动手前必做」三步流程（读 docs / 全局影响四问 / 写 changelog）；「必须遵守的设计规则」加第 11 条；「给后续 agent 的提醒」前置三条强提醒
  - [docs/changelog.md](changelog.md): 新建本文件，定义写入规则 + 模板
- **影响范围**: 仅文档约束，不动代码、不动协议、不影响构建
- **留尾巴**: 无。后续 agent 是否真的遵守，靠每条新改动有没有对应 changelog 条目来检验

### 2026-05-10 — 流式中「立即发送」队列：在 streaming 中插队 user message

- **Why**: 之前 streaming 中只能等当前 turn 结束才发新 user message；用户经常想中途补一句话（"换个方向"/"加个约束"），目前只能 cancel 重发，破坏 turn 上下文。现需要：流式时输入新内容能立刻推到当前 run 的 pending 队列，agent_loop 在下一次 model.request 之前 drain 它们作为新的 user message 加入 transcript（不打断当前 loop，下一个 iteration 立刻可见）
- **改动**:
  - [crates/platform/src/runtime.rs](../crates/platform/src/runtime.rs): `RuntimeHandle` 由单 `CancelFlag` 升级为 `{cancel, pending_inputs: Arc<Mutex<Vec<PendingUserInput>>>}`；新增 `inject_pending_input(request_id, input)`
  - [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs): `RunParams` 多一个 `pending_inputs` 字段透传到 agent_loop
  - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): 每次 model.request 前 drain pending_inputs 当作新的 user entry push 进 transcript；run 结束 clean
  - [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs): 新增 `Session::run_with_pending(cancel, pending_inputs)` 让 surface 把外部 pending 队列共享给 run
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `SendArgs` 加 `pending_inputs` 字段，run 启动时透传
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): 新增 `inject_user_message` Tauri 命令——立刻把新 user message append 到 session.json 同时推进 run pending 队列
  - 前端 [InputQueuePanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/InputQueuePanel.tsx) / [ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx) / [useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts) / [types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts) / [tauri.ts](../apps/desktop/frontend/src/desktop/bridge/tauri.ts): 流式中输入框可继续输入并提交，UI 在 ChatInput 上方展示队列；流式 bubble 之后也即时插入这些 user message
  - [ChatView.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx): 把 InputQueuePanel 接进底部布局；流式中 `injectedSinceStream` 列表渲染在 streaming bubble 之后
- **影响范围**: agent-core / platform / desktop 三层；不动 protocol（pending_inputs 只是 surface 与 run 的内部约定，不进 EventPayload）；CLI 没接入
- **留尾巴**:
  - CLI surface 还没用上 pending_inputs（loop 模式可以补一个 `/queue` 命令）
  - 多条 pending 队列模型如何合并到一条 user message 还是分多条 entry 暂未明确——当前实现是各自独立 push 成多条 user entry
  - run 结束 reload session 时前端把 `injectedSinceStream` 清空，但如果 reload 路径异常这条逻辑可能错位

### 2026-05-10 — 关窗时 pending ask 自动按取消收尾 + 问题头部展示完整问题

- **Why**: 桌面端 ask 弹窗未答时直接关窗，进程退出会让 oneshot drop、agent_loop 中断、session.json 丢失整轮 assistant + ask tool_call，下次开会话只剩半截 user message 看不到曾经被问过什么。讨论过完整 LangGraph 式 checkpoint+resume，但代价不成比例（参考 codex / opencode 都没做）。退而求其次：关窗 = 把 pending ask 当「用户取消」收尾、模型在 transcript 里能看到 cancelled tool_result，UI 也显示「取消」答案
- **改动**:
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs) `spawn_ask`: 把 `ToolCallFinished` emit 提到 cancel-flag 检查之前。原来 cancel flag 一设直接 `return Err`，UI 看不到取消答案；现在不论 X 按钮、ESC、关窗，事件流里都有完整的 `UserQuestionAnswered + ToolCallFinished("[用户取消了提问]")`，observer 把这条结果落到 session.json 的 `tool_calls[i].result`
  - [crates/platform/src/runtime.rs](../crates/platform/src/runtime.rs): 新增 `cancel_all()` / `has_active_runs()`，关窗时批量取消所有 in-flight run
  - [apps/desktop/src/hitl.rs](../apps/desktop/src/hitl.rs): `HitlState` 新增 `cancel_all_pending()`（去重把所有 gate 的 pending 按取消 resolve）/ `has_pending()`
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `WindowEvent::CloseRequested` 拦截——若有 pending HITL 或活跃 run，`prevent_close` + cancel_all_pending + cancel_all flag + 起 std::thread 轮询 2s 等 `persist_interrupted_assistant_output` 写盘 → 关窗
  - [apps/desktop/frontend/src/desktop/ui/components/UserQuestionPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/UserQuestionPopup.tsx): 头部由 `items-center + truncate` 改 `items-start + whitespace-pre-wrap`，长 question 不再被 ... 截断
  - [crates/agent-core/src/tools/mod.rs](../crates/agent-core/src/tools/mod.rs): `ask_tool_definition` 描述加明确禁令——禁止「其他/让我重新描述/以上都不是/自由回答/再想想」之类兜底元选项（UI 已有自由输入框）
- **影响范围**: agent-core / platform / desktop。不动 protocol。强制 kill 进程仍会丢——这是合作式关窗的合理边界
- **留尾巴**:
  - 真正的 HITL checkpoint+resume（LangGraph interrupt 范式）暂不做。讨论中明确了它的代价：sibling tool_call 并发取消语义、snapshot 漂移、热路径 IO 增加、半成品 checkpointer 不如不做。等真有用户反馈再启
  - `handle_close_with_pending_hitl` 用 `std::thread::sleep` 轮询而非 tokio sleep，是因为 desktop 没有直接 tokio dep；性价比够用
  - 多个 session 同时活跃时 `cancel_all` 是真的全杀，没有「只杀当前 session」的粒度——目前 desktop 也不支持多窗口

### 2026-05-10 — 新建 PROJECT.md 作为新的 source of truth + 文档体系治理

- **Why**: 仓库存在三大文档问题：(1) architecture.md (77KB) + hebbian-harness-detailed-design.md (73KB) 两份巨大「目标态」互相漂移没人维护；(2) M1 进度有三套互不对齐的编号（CLAUDE.md 1-14 / architecture #1-#33 / todos M1 #N）；(3) 「目标态」混入大量空想 struct（ContextEngine / RunTree / Aggregator）让读者误以为已实现。需要一份「站在今天看仓库」的事实文档。同时把已知的产品 bug（AllowAndRemember Project/Global 死按钮、compact 末尾连排 user 致 Anthropic 400、mpsc unbounded）和半作品（TurnContext / MaxIterations / ask 特判）按 P0–P3 重新排过
- **改动**:
  - 新建 [docs/PROJECT.md](PROJECT.md)：10 节 ~600 行，含一句话定位 / 仓库地图 / 术语 / 5 条核心数据流（含新加的 pending_inputs 流） / 真实在用 vs 协议先行的 Op&Event 表 / 16 项已实现功能详细说明 / 4 档优先级（P0–P3 + FR）的待办清单（每项含现状 / 痛点 / 关键设计 / 影响面）/ 12 条已知不合理点 + 改动建议 / 自测命令（含桌面端人工验证清单）/ 文档体系治理规则
  - 文档分工新规则（写在 PROJECT.md 顶部 + §10）：PROJECT.md = source of truth；architecture.md 留作目标态参考、冲突时以 PROJECT 为准；详细设计文档只在追根溯源时翻
  - 不动代码、不动协议、不动 architecture.md / todos.md（保留旧文档供回溯）
- **影响范围**: 仅文档结构。后续改动统一去 PROJECT.md §6/§7/§8 维护
- **留尾巴**:
  - architecture.md / hebbian-harness-detailed-design.md 没归档——下次大改时再决定是 deprecate 还是删（避免一次性大动作）
  - todos.md 没合并到 PROJECT.md §7——保留扁平视角；新增 todo 同时落两处（这是个长期协调成本，等下次盘点再决定要不要去掉 todos.md）
  - CLAUDE.md 顶部的「⚠️ 动手前必做」还指向 architecture.md / todos.md，等读者习惯 PROJECT.md 后再调整指向

### 2026-05-11 — 新建 设计详解.md：教学性视角的 Rust 项目设计讲解

- **Why**: 项目主人是 Rust 新手，看 PROJECT.md（紧凑地图）和 architecture.md（目标态）都有概念门槛——`Arc<T>` / `mpsc` / `oneshot` / `async fn` / `dyn Trait` 这些 Rust 行话没解释，trait + 装饰器模式读起来卡壳。需要一份「同一片地图的导游词」，把每个设计点讲透：是什么、Rust 怎么写的、为什么这么写、现状评价、怎么改
- **改动**:
  - 新建 [docs/设计详解.md](设计详解.md)：15 节 ~900 行
    - §1 Rust 行话小词典（trait/impl/Arc/Mutex/mpsc/oneshot/async/Box<dyn>/Result/Option/Send/Sync/cargo+crate+workspace）
    - §2-§12 按模块讲解（三层骨架 / 事件流 / 协议 / 工具系统 / HITL / 上下文管理 / Model Gateway / Hooks / Recorder / Observability / Surface），每节都按 「是什么 → Rust 实现 → 怎么工作 → 现状评价 → 怎么改」 五段式
    - §13 总账：P0-P3 + FR + 文档体系不合理点，所有问题汇总改法
    - §14 建议改动顺序：12 步实操路线，每步预估工时
    - §15 怎么读代码：从浅到深 9 个文件顺序 + 用 mock 跑现场看数据的 tip
  - 与 [PROJECT.md](PROJECT.md) 分工明确：PROJECT.md 是地图、信息密度高；设计详解.md 是导游词、教学性强。事实冲突时以 PROJECT.md 为准（在顶部声明）
- **影响范围**: 仅文档。不动代码、不动协议
- **留尾巴**:
  - 现在文档已经有 4 份相关联：CLAUDE.md（入口）/ PROJECT.md（地图）/ 设计详解.md（教程）/ architecture.md（目标态）。短期 OK，但长期维护成本叠加——下次大改时合并到 2 份（CLAUDE.md + PROJECT.md+教程合一）
  - 设计详解.md 的 Rust 词典是「项目专用」精选，没覆盖完整 Rust 语言；读者需要更深时仍要查 Rust Book

### 2026-05-11 — 新建 架构.md：按用户想法重组的目标态架构 + 5 个参考项目取舍

- **Why**: 用户明确提出新架构想法：(1) CLI 和桌面对话场景只与 agent_core 走事件流；(2) 非对话场景（配置供应商 / 读对话历史 / 配置项目）UI 不同但底层代码必须一样；(3) agent_core 是完整大脑（loop / 派发 / 上下文 / HITL / 观测 + **审批必须持久化到项目和全局，不只是内存**）；(4) model_gateway 接模型但 agent_core 内部允许做特定模型适配（如 DeepSeek 把 thinking 从 content 拆出来）。同时要求结合 claude-code-haha / codex / opencode / openhanako 的设计。现有的 architecture.md 是过度设计的目标态、PROJECT.md 是现状描述，都不是用户想要的「按我想法的目标态」
- **改动**:
  - 新建 [docs/架构.md](架构.md)：12 节
    - §0 设计目标 + 4 参考项目取舍表（codex 协议化 RPC / opencode core-as-SDK / openhanako 三层简洁 / cc-haha 反面教材）
    - §1 行话翻译表：把所有 Rust / 项目内部行话翻译成 Python/Go 类比（trait/Arc/Mutex/mpsc/oneshot/async/Box<dyn>/emit/match/Op/Result/Option）
    - §2 三层架构图（Surface / CoreClient / Agent Core + Model Gateway / Storage / Observability）
    - §3 通信契约：**双通路**（对话事件流 + 同步 API），列出同步 API 的具体方法清单（供应商 / 历史 / 项目 / 权限 / agent 配置 / 用量 / skills）
    - §4 Agent Core 11 个内部模块详解，每个 是什么/当前/改成怎样，包含两个新模块：**PermissionStore（§4.6 持久化审批）** + **ModelFeatureAdapter（§4.11 DeepSeek thinking 拆解放这里）**
    - §5 Model Gateway 装饰器链：原始 provider → InstrumentedClient → ModelFeatureAdapter → NamedClient
    - §6 Storage 统一持久化层（settings / sessions / permissions / blobs 四类）+ 数据目录布局
    - §7 **Surface 共享层 CoreClient trait**：LocalCoreClient（本地）+ HttpCoreClient（远程，未来）两种实现；桌面 Tauri command 和 CLI subcommand 背后调同一份函数
    - §8 5 个具体数据流场景（发消息 / 切供应商 / 读历史 / 改项目设置 / 持久化审批命中）
    - §9 迁移路线 9 步，每步独立可工作（最先 P0 修 bug，再落 PermissionStore，再抽 CoreClient，再 ModelFeatureAdapter，再 BlobStore，再 prompt cache）
    - §10 8 条不可违反原则
    - §11 迁移后的文件结构（新增 `crates/storage` + `agent-core/model_adapters/` + `agent-core/core_client.rs`）
    - §12 7 个决策点等用户回答
- **影响范围**: 仅文档。不动代码、不动协议
- **留尾巴**:
  - 这份是「目标态」与 architecture.md 关系待理清——可能后者最终归档，但本次不动
  - §12 的 7 个决策点需要和用户单独讨论后再开工
  - CoreClient trait 的具体方法签名还是粗粒度，落地时要逐个细化

### 2026-05-11 — 重写架构.md：按项目主人逐条反馈完整落地新设计 + 参 CodeIsland/codex/opencode

- **Why**: 项目主人对前一版架构.md 给了 10+ 条具体反馈，要求按这些反馈写一份「更详细」的文档。关键点：(1) 不要加 server 目录，保留 apps/crates 布局；(2) 不做"一行代码对照"，详设阶段再讲；(3) Session/Run/Turn 要多举例 + 说清跟压缩的关系；(4) 派发器审批算 HITL 一种；(5) Tool 接口简化为 spec+invoke，权限解耦，引入 AutoMode（仅 opus-4-7）；(6) 内置工具用驼峰，MAX_STEPS=100；(7) 项目级审批跟 session jsonl 走，全局存 ~/.hebbian/permissions.json；(8) 压缩工件落 txt/md，LLM 可 read；(9) hooks 参 CodeIsland 11 点位；(10) jsonl 是对话历史唯一文件；(11) 数据目录 ~/.hebbian 共享；(12) Desktop/CLI 设置存两份；(13) SDK 简单→TUI 完整，参考 codex/opencode；(14) Prompt 太长→重构生产可用
- **调研**: 看了
  - `other/CodeIsland/Sources/CodeIsland/Resources/codeisland-remote-hook.py` 的 EventNormalizer——拿到完整 11 点位标准
  - `codex-rs/tools/src/` 的工具列表（agent_job/agent/apply_patch/plan/goal/local 等）
  - `opencode/packages/opencode/src/tool/`（edit/glob/grep/read/shell/skill/task/todo/lsp/mcp-websearch 等）
  - 三个项目的 system prompt 大小：codex 68 行、cc-haha 68 行、opencode 36 行——Hebbian 现 276 行偏长
- **改动**:
  - 完全重写 [docs/架构.md](架构.md)，从 ~1080 行到 ~1500 行（更详细）
  - 新增章节：
    - §4.4 工具系统详解：4.4.1 Tool 接口简化 / 4.4.2 effects 分析与权限解耦 / 4.4.3 RunMode 4 种 / 4.4.4 AutoMode 实现（含 judge prompt）/ 4.4.5 完整工具列表（驼峰命名）/ 4.4.6 MAX_STEPS=100 / 4.4.7 命名规范
    - §4.6 PermissionStore：三层 scope 存储位置（Project 跟 jsonl + 全局 ~/.hebbian/permissions.json）+ 数据结构 + 决策树
    - §4.7 Context Engine 增加压缩工件落盘：tool_results/<call_id>.txt + compactions/compact-<ts>.md，LLM 通过 read 按需取
    - §4.8 Hook 11 点位（参 CodeIsland）+ 4 内置可改 state hook + socket+JSON 互操作
    - §4.9 Recorder = 对话历史唯一文件（jsonl 同时存 transcript/event/PermissionRule/Marker）
    - §4.10 Trace UI 设计占位（远期）
    - §4.11 ModelFeatureAdapter（DeepSeek thinking 拆解）
    - §7.3 设置分离设计：CoreClient 接口一份、surface_settings 存两份
    - §8 TUI 设计（参 codex ratatui，apps/cli 内部加 tui/ 模块）
    - §9 System Prompt 体系：三段式 + cache 边界 + 重写计划
  - 修订章节：
    - §4.1 Session 与压缩的关系（compaction policy 是 Session 配置）
    - §4.2 Run/Turn/Step 给了 4 个具体例子（纯文本/单工具/插队/长循环）
    - §4.3 Rust 这么做的好处/坏处类比 Python/Go 解释
    - §6 ~/.hebbian 完整目录布局（含每段对话的配套目录）
  - 8 条设计原则升级为 12 条（加 jsonl 唯一文件 / 驼峰命名 / MAX_STEPS / 压缩不丢）
  - §13 决策点扩到 10 个
  - §14 迁移路线扩到 13 步（含 TUI / 系统 prompt 重构 / todo/task 工具）
- **影响范围**: 仅文档。不动代码、不动协议
- **留尾巴**:
  - D1（Project 审批存哪里）跟传统 IDE 工具语义不同，需要主人最终确认
  - System prompt 重写正文留到详设阶段单独 docs/system-prompt.md
  - TUI 具体组件设计待详设
  - Trace UI 是远期，先占位

### 2026-05-11 — 架构.md 第三轮重写：合并主人所有反馈、敲定所有决策

- **Why**: 主人对前一版架构.md 又给了详细反馈：
  - 命名：platform 改 common；工具用 PascalCase 不是 camelCase；HitlGate/HitlState 命名其实没冲突，不要瞎改
  - Step 划分：ModelStep + ToolStep 分开，不能合并；插队时机在 ToolStep 后不是 ModelStep 后
  - RunMode 4 种正名：AskBeforeEdits / EditAutomatically / PlanMode / AutoMode；mode 不能进 system prompt（破坏 cache）；走 SEMI 段 + 工具过滤
  - AllowScope 3 种：Once / Session / Global；Run scope 等价 Session 合并
  - 压缩 tier 化要诚实说优缺点；按需检索三管齐下；压缩工件落 txt/md
  - session 目录化（不要文件+目录分离）；session_id = {yyyymmddHHmm}-{uuid}
  - 流式 partial sidecar 保中断恢复（不是简单 Done 时落盘）
  - 文件锁必加（CLI+Desktop 真并发）
  - Prompt 单独目录 + md 文件 + 用户只能覆盖 persona
  - PlanMode 退出：上一个模式是 AutoMode 则 10s 倒计时自动切，其他手动切
  - codex sub-item 调研：扁平 3-variant 够用不抄
- **调研**:
  - codex `ResponseItem` enum：12+ variant 细粒度拆分（Message/Reasoning/FunctionCall/FunctionCallOutput/LocalShellCall/CustomToolCall/...），跟我们扁平结构对比
  - 三家 system prompt 大小：codex 68 行 / cc-haha 68 行 / opencode 36 行（hebbian 现 276 行偏长，重构计划合理）
  - codex 工具列表 vs opencode 工具列表对比，确认 Hebbian 13 个工具的选择
- **改动**:
  - 完全重写 [docs/架构.md](架构.md)，从 ~1500 行扩到 ~2100 行（更详细 + 所有反馈整合）
  - **核心修正**：
    - 命名：crates/platform → crates/common；工具 13 个全 PascalCase（Ask/Bash/PowerShell/Read/Write/Edit/Glob/Grep/Skill/WebSearch/Fetch/TodoWrite/ExitPlanMode）
    - Step：ModelStep 和 ToolStep 分开；MAX_STEPS=100 只数 ToolStep；插队时机精确化（ToolStep 后 + Turn 边界）
    - Mode：mode 不进 system prompt（保 cache）；走 SEMI 段 runMode/runModeHint + ModelRequest.tools 过滤；切 mode 不重建 system prompt
    - AllowScope：3 种（Once/Session/Global），Session 不再叫 Project，UI 中文显示
    - HitlGate/HitlState 名字保留——明确两者职责完全不同不冲突
    - 流式：partial sidecar 方案（session/partial/<msg_id>.partial.jsonl）保中断恢复
    - session 目录化：每段对话一个目录含 session.jsonl + meta.json + tool_results/ + compactions/ + plans/ + partial/
    - 文件锁：fs2/fd-lock，每文件独立 lock；write_atomic / append_jsonl 都加锁
    - Prompt：内置 6 个 md（base_system/tools_guide/context_recall/communication/persona/automode_judge）+ include_str! 编译进二进制；用户只能覆盖 persona
    - PlanMode 退出：之前 AutoMode → 10s 倒计时；其他模式 → 用户手动切；ExitPlanMode 工具仅 PlanMode 时注入
    - 压缩 tier 化诚实评估：列了优缺点（Tier 划分是经验主义，用户可覆盖 tier_overrides）
    - 按需检索 prompt 设计：context_recall.md 全文写出（编译进二进制）
  - 13 步迁移路线，每步独立交付，推荐 Step 6（Tool 接口简化）第一开工
- **影响范围**: 仅文档。不动代码、不动协议
- **留尾巴**:
  - system prompt 详细文本留 docs/system-prompt.md 单独写
  - architecture.md / hebbian-harness-detailed-design.md 还没归档；本轮没动避免一次性大改
  - 13 步迁移路线工时估算偏乐观，落地时可能扩展
  - PlanMode 工具过滤 + ExitPlanMode 工具的实现细节（如 plan markdown 格式约定）留详设阶段

### 2026-05-11 — 文档体系收束：架构.md 成为唯一设计准则 + 删冗余文档 + 重写 CLAUDE.md / compaction.md

- **Why**: 项目主人确认架构.md 已敲定，要求：
  - 架构.md 作为唯一设计准则
  - 其他过渡性文档（PROJECT.md / 设计详解.md / architecture.md / hebbian-harness-detailed-design.md / 随记.md）不再保留，避免多份文档漂移
  - 仅保留 changelog.md（时间线）与 compaction.md（横向调研背景资料）
  - CLAUDE.md 改为强制执行「修改前评估 + 修改后入 changelog」的协作准则
  - 架构.md 去除对话式表达，写成正式设计文档
  - 参考项目对比写得更详细（多维度逐项对照）
- **改动**:
  - 删除 5 个旧文档：
    - `docs/PROJECT.md`（现状描述，被架构.md §14 迁移路线覆盖）
    - `docs/architecture.md`（旧目标态 77KB，过度设计且与新版架构.md 冲突）
    - `docs/hebbian-harness-detailed-design.md`（73KB 详细设计，部分过时）
    - `docs/设计详解.md`（Rust 新手教程，主人非新手）
    - `docs/随记.md`（已合并到 todos / 架构.md）
  - 重写 [docs/架构.md](架构.md)：
    - 去除全部对话式表达（"你说"/"我推荐"/"抄 cc-haha"等 16 处）
    - 改为正式设计文档语言（"本设计采用"/"沿用 X"/"与 Y 一致"）
    - 新增 §16 综合参考项目对比（12 个子节，逐项对照 claude-code-haha / codex / opencode / openhanako / CodeIsland）
    - §1 文档语言风格说明
    - §13 决策记录表（去掉"拍板"等口语化用词）
    - §14 迁移路线复杂度（替代之前的"工时估算"，更客观）
    - §15 推进流程（按 Step 单独产文档的规范）
  - 重写 [docs/compaction.md](compaction.md)：
    - 与架构.md §4.7 对齐
    - 保留三家参考调研（claude-code-haha 多层防线 / codex 双实现 / opencode 简单触发）作为背景资料
    - 新增 §5 与三家参考项目的对照表（说明 Hebbian 取舍）
    - 新增 §6 实施参考（指向 step-9-compaction.md 与 system-prompt.md 详设文档）
  - 重写 [CLAUDE.md](../CLAUDE.md)：
    - 顶部声明架构.md 为唯一设计准则
    - 「⚠️ 任何修改前必做」5 步流程：定位章节 / 设计影响评估 5 问 / 实施约束 / 验证命令 / 追加 changelog
    - 「与用户讨论的规则」段：禁止未经确认直接执行违反架构.md 的修改；即使用户强烈要求也必须先讲清利害再确认
    - 保留开发命令清单与 graphify 段
    - 去除大量过时章节（M1/M2/M3/M4 进度 / 现状描述 / Harness API 示例 / 已知漂移点等，全部已迁入架构.md 或不再有效）
- **影响范围**: 仅文档。不动代码、不动协议
- **留尾巴**:
  - 架构.md `image/` 目录保留（未来章节可能引用 4.4.3 截图）
  - system prompt 详细文本（架构.md §9 描述形状）仍待在 `docs/system-prompt.md` 单独产出
  - 各 Step 详细设计文档（`docs/step-{N}-{name}.md`）尚未开始，将按架构.md §14 路线顺序产出
  - changelog 已积累 9 条历史记录，无需精简（其本身就是只增不减）

### 2026-05-11 — 复原 docs/随记.md（误删后恢复）

- **Why**: 项目主人确认前一轮"删冗余文档"误删了随记.md。该文件是产品 idea 备忘录，部分想法已被架构.md 吸收（流式插队 / 上下文落盘 / ask 用户 / 设置分离），部分仍是未排期愿望（skill 商城 / git tree / AST 加速 grep / IDE 显示 diff），有保留价值
- **改动**:
  - 复原 [docs/随记.md](随记.md) 原始内容（10 条产品 idea，按主人原话保留）
- **影响范围**: 仅文档。文档体系定位：架构.md 是设计准则、changelog 是时间线、compaction.md 是调研背景、随记.md 是未排期 idea 备忘
- **留尾巴**:
  - 随记里的想法是否要排期 / 何时进入架构.md，需要按需评估
  - 「git tree」「IDE 显示 diff」「skill 商城」等远期能力暂未纳入架构.md §14 迁移路线

### 2026-05-11 — CLI 单次调试模式 + Resume + Auto-Approve + 删 Mock

- **Why**: 项目主人提出新设计想法——CLI single 模式应当：(1) 输出格式与 TUI 一致，便于 LLM 自调试 / CI 端到端验证；(2) 遇 HITL 立即退出并在终端留下 HITL 痕迹；(3) 支持 `--resume <sid>` 在已有 session 上追加 user message；(4) 支持 `--auto-approve` 自动通过审批。同时确认：所有调用真实模型，不再保留 mock 模式。这套能力组合让 LLM 可以程序化验证 agent_core 与 CLI/TUI 的运行稳定性
- **流程**: 按 CLAUDE.md「⚠️ 任何修改前必做」5 步评估，确认与架构.md 不相悖、属于新增设计、影响 §8 / §10 / §12 / §13 / §16，待主人逐项拍板后实施
- **改动**:
  - [docs/架构.md](架构.md) §8.2 模块布局：新增 `render/` 共用渲染层（TerminalRenderer + RatatuiRenderer），TUI 与 single 共用
  - §8.3 启动方式：完整重写
    - §8.3.1 模式总览（5 种模式，明确不提供 mock）
    - §8.3.2 单次调试模式：TUI 格式输出 + HITL 立退 + partial sidecar 自动落盘
    - §8.3.3 `--resume <sid>` 行为：load_session + banner + 不重放历史
    - §8.3.4 `--auto-approve` 行为：仅 CLI observer 层生效，AllowOnce 不写 PermissionStore，不覆盖 Ask，isatty stderr 警告
    - §8.3.5 退出码：0 / 1 / 42 / 43 / 130 语义固定
    - §8.3.6 输出格式示例（含 auto-approved 标记）
  - §8.5 与参考项目对比：新增「单次调试模式」与「Mock 模式」两行（前者本设计独有，后者不提供）
  - §10 数据流场景：新增 §10.6 / §10.7 两个调试场景，原 §10.6 改为 §10.8
  - §12 关键原则汇总：新增第 13 条「CLI 退出码语义化」+ 第 14 条「所有调用真实模型，不提供 mock」
  - §13 决策记录：新增 10 行（8.3.2 / 8.3.3 / 8.3.4 / 8.3.5 等所有 CLI 调试相关决策）
  - §16.12 综合评估：「本设计相对参考项目的优势」新增第 7 条（CLI 单次调试模式独有）
- **影响范围**: 仅文档。后续实施需动 apps/cli（重写 single.rs + 新增 render/ + 删 --mock 参数）。core / protocol / storage 不动。CoreClient.load_session 与 AllowOnce 已存在，无需新 API
- **留尾巴**:
  - apps/cli 端实施（render/ 共用层、single.rs 重写、删 --mock 参数）待 §14 迁移路线 Step 13 阶段
  - MockClient 删除涉及 model-gateway 内的 mock 实现，迁移时需同步删
  - CI 集成指南留待 docs/step-13 详设阶段
  - `--from-message <msg_id>` 精细切点暂不实现，按需后补
