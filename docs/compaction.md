# 上下文压缩（Context Compaction）

> 这份文档梳理 Hebbian 的上下文压缩设计与三个主要参考项目（Claude Code、Codex、DeepSeek-TUI / OpenCode）。
> Hebbian 的设计是参考它们之后裁剪出来的最小可用版本，本文也指明了未来可以从它们身上继续吸收的能力。

---

## 0. 为什么要压缩

LLM 的上下文窗口是有上限的：

- 旧消息被无限累加 ⇒ 每轮都得把整段历史重新喂进去 ⇒ token 成本与延迟爆炸
- 越接近窗口上限，模型在长尾位置的信息检索能力越差（lost-in-the-middle）
- 一旦超限，provider 直接 `prompt_too_long` 报错，整个 session 卡死

「压缩」的目的是在不破坏 agent 任务连续性的前提下，把对话历史替换成更短的、信息密度更高的等价表达，让对话能继续往下走。

不同项目对「等价表达」的取舍不一样，下面分头看。

---

## 1. Claude Code（`claude-code-haha`）— 多层防线

CC 是参考实现里最完整的。**它不是单点压缩，而是从轻到重四层叠加**，每一层只解决一类问题：

```
轻 ──────────────────────────────────────────────────────────────► 重
[1] microcompact   [2] snipCompact   [3] auto / reactive   [4] manual /compact
   shadow 工具       裁长结果中间        到阈值整段摘要         用户主动整段摘要
```

### 1.1 microcompact（轮次 / 时间触发的"工具结果影子化"）

**这是题主问的「N 轮之后 shadow tool_call 输出」**，对应 [src/services/compact/microCompact.ts](https://github.com/.../microCompact.ts)。

- **谁会被影子化**：只针对**可压缩工具**白名单：
  ```
  Read, Bash (Shell 系列), Grep, Glob, WebSearch, WebFetch, Edit, Write
  ```
  — 这些工具的输出特点是：占 token 多 + 一旦读过、再回看价值不大（文件已编辑、命令已跑完）。`Task` / `TodoWrite` 这种状态型工具不在列表里，因为它们的结果对后续决策仍有用。

- **两种触发器**：
  1. **轮数触发（cached microcompact，需 `CACHED_MICROCOMPACT` feature）**
     - 每条 user 消息 = 一组 `tool_result` 的容器；按"已注册工具数量"达到 `triggerThreshold` 时，保留最近 `keepRecent` 个，老的标记删除
     - 阈值由 GrowthBook 远程下发；典型值（社区猜测）大约是触发 20+，保留最近 5
     - **关键点**：用 Anthropic 的 **`cache_edits` API** 在不打破前缀缓存的情况下"打洞"——cache key 仍命中，模型读到的内容里这些 tool_result 已经被替换。第一次清掉之后，后续每轮重新把 `cache_reference` + `cache_edits` 包进请求即可，不用真正改本地 message
  2. **时间触发（time-based microcompact）**
     - 距上一条 assistant 消息超过 `gapThresholdMinutes` 分钟（默认 60，对齐 Anthropic prompt 缓存 TTL）就触发
     - 此时 server 端缓存几乎肯定过期了，下一次请求无论如何都要重写整段前缀；**索性提前把老 tool_result 内容置空（`[Old tool result content cleared]`）**，这样重写出去的 prompt 已经是变小过的
     - 同样保留最近 `keepRecent` 个（默认 5）

- **不动什么**：user 文本、assistant 文本、thinking、最近 N 个工具结果。即只删"中间老旧的工具大爆炸输出"。

- **后处理**：
  - 触发 `notifyCacheDeletion` / `notifyCompaction`，告诉 cache-break 监控这次命中率下降是预期行为
  - `suppressCompactWarning`：UI 不再提示"快撞墙"

### 1.2 snipCompact（裁长结果中间）

ant-internal feature（外部 build 是 stub）。粗略含义：当**单个** tool_result 巨大（如 web_fetch 一个长网页）时，保留头尾、把中间替换为 `[trimmed N tokens]` 之类的占位符。这是"消息内裁剪"，不影响轮数，对 microcompact 之后仍嫌大的单条结果做精修。

### 1.3 autoCompact（接近上限时自动整段摘要）

[src/services/compact/autoCompact.ts](https://github.com/.../autoCompact.ts)：

- **何时触发**：
  ```text
  effective_window = model_context_window - max_output_tokens_for_summary
                    （上限 20_000，p99.99 摘要输出）
  auto_compact_threshold = effective_window - AUTOCOMPACT_BUFFER_TOKENS（13_000）
  ```
  剩余预算不足 ~13k token 时即触发。
- **三个守门人**：
  1. `DISABLE_COMPACT` / `DISABLE_AUTO_COMPACT` 环境变量
  2. recursion guard：`session_memory` / `compact` / `marble_origami` 这几个 forked agent 自己不能再触发自己（避免死锁 + 主线程状态被搞坏）
  3. 熔断：连续 3 次失败就这个 session 不再尝试（修线上 250K 浪费调用/天的回归）
- **三种实现，按优先级尝试**：
  1. `trySessionMemoryCompaction` —— 走 session memory 那条快路径（如果有上次的摘要可以增量更新就用，省一次大模型调用）
  2. `compactConversation` —— 主路径，下文详述
  3. `reactiveCompact` —— **413 fallback**：模型已经报 `prompt_too_long`，再"反应式"压一次。这是最后一根稻草

### 1.4 manual `/compact`

[src/commands/compact/compact.ts](https://github.com/.../compact.ts)：

- 用户主动 `/compact [自定义指令]`
- 先尝试 sessionMemory（不接受 `customInstructions`）；不行就 microcompact + compactConversation
- 自定义指令通过 `mergeHookInstructions` 合并到摘要 prompt 里，让模型按用户意图侧重摘要

### 1.5 摘要 Prompt 的关键设计

[src/services/compact/prompt.ts](https://github.com/.../prompt.ts) 极其精细，几个亮点：

- **`<analysis>` + `<summary>` 双层 XML**：
  - `<analysis>` 是模型的 scratchpad，强制它先把"用户做了什么 / 我做了什么 / 出了什么错 / 用户怎么反馈"逐条写一遍
  - `<summary>` 是最终保留下来注入 transcript 的部分
  - `formatCompactSummary` 在写回 transcript 前**剥掉 `<analysis>`**——它是"为了写好 summary 而存在的草稿"，不进 context
- **强制 9 段结构**：
  1. Primary Request and Intent
  2. Key Technical Concepts
  3. Files and Code Sections（含**完整代码片段**——CC 不省）
  4. Errors and fixes（含用户反馈）
  5. Problem Solving
  6. **ALL user messages**（不是工具结果）
  7. Pending Tasks
  8. Current Work（含 verbatim 引用）
  9. Optional Next Step（含 verbatim 引用，避免 task drift）
- **NO_TOOLS_PREAMBLE / NO_TOOLS_TRAILER**：因为 cache-share fork 路径继承了完整工具集，Sonnet 4.6+ 偶尔会在压缩这一轮也尝试调用工具，于是首尾两段 CRITICAL 提示禁止任何 tool_call
- **三种变体**：
  - `BASE_COMPACT_PROMPT` —— 整段都压
  - `PARTIAL_COMPACT_PROMPT` —— 前面有 retained context，只压最近的尾段
  - `PARTIAL_COMPACT_UP_TO_PROMPT` —— 摘要放在前面、未来消息接在后面
- **Compact 保留器** `compactBoundary` 系统消息：
  - 模型看到的 transcript = `getMessagesAfterCompactBoundary(messages)`
  - REPL 端**仍保留 boundary 之前的消息**，UI 上能滚回去看（这正是 hebbian 学的那一层）
- **后处理 `runPostCompactCleanup`**：
  - 重新读最近 ≤5 个文件（5k tokens / 文件）
  - 重新注入最近用过的 skill 文件（5k / skill，总额 25k）
  - `notifyCompaction` + `markPostCompaction` + `clearCompactWarningSuppression`

---

## 2. Codex（`codex-rs`）

Codex 的压缩相对窄（没有 microcompact 那一层），但**双实现**和**回退裁剪**值得抄。

### 2.1 触发与实现

[codex-rs/core/src/compact.rs](https://github.com/openai/codex)：

- **配置项** `model_auto_compact_token_limit`：每个模型独立的 token 阈值。主循环监控 token 使用，超过即调 `run_inline_auto_compact_task`
- **手动 `/compact`** —— `run_compact_task`，强制 `InitialContextInjection::DoNotInject`
- **两套实现**：
  1. **Inline（本地）** `run_inline_auto_compact_task`：本地客户端发一次 `client.stream(prompt)` 拿摘要
  2. **Remote（服务端）** `compact_remote.rs`：当 provider `supports_remote_compaction()` 时走服务端压缩 API（OpenAI Responses API 的 server-side compact），不需要把整段历史回传一次

### 2.2 InitialContextInjection — 上下文重注入策略

```rust
pub(crate) enum InitialContextInjection {
    BeforeLastUserMessage, // mid-turn 自动压缩用：把 initial_context 注入到最后一条 user message 之前
    DoNotInject,           // 手动 /compact 用：清空 reference_context_item，下一轮自然重注入
}
```

- **mid-turn 压缩**必须保留 initial context（环境/工作目录/工具清单），不然模型会忘了自己在哪干活
- **手动 /compact** 主动清空，让用户主动开启的下一段对话从干净状态开始

### 2.3 压缩后的 history 重建

`build_compacted_history` 的产物是：

```
[                                  ← 旧的全部 ResponseItem 都被丢
  最近 K 条 user 消息（≤ 20_000 tokens，从最新往回挑）,
  user message{ "[summary_prefix]\n[summary]" },
] + （可选）initial_context 注入到「最后一条真实 user 之前」
```

要点：
- **保留最近 user 消息**（≤20K tokens）。tool 输出全丢，因为：(1) 工具调用是为达成 user intent 的中间手段；(2) summary 已经把"做过什么"写下来了
- **summary 以 user 角色写入** + 用 `summary_prefix.md` 包：
  > "Another language model started to solve this problem and produced a summary of its thinking process. ... Here is the summary..."
- 模型必须能把"摘要"和"真实用户输入"区分开：靠 `is_summary_message(text) = text.starts_with(SUMMARY_PREFIX)` 这一个前缀判断

### 2.4 Prompt 模板

`templates/compact/prompt.md`（短得很）：

> You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.
> Include: Current progress and key decisions / Important context, constraints, or user preferences / What remains to be done / Critical data, examples, or references.
> Be concise, structured, and focused on helping the next LLM seamlessly continue the work.

跟 CC 那个 9 段结构相比，Codex 更"指令式"——靠模型自由发挥，但加了 SUMMARY_PREFIX 来让其他逻辑识别这条消息。

### 2.5 压缩本身爆窗的回退

`run_compact_task_inner_impl` 的循环里：当**压缩这一次请求**自己拿到 `ContextWindowExceeded`，会从 history 头部 `remove_first_item()` 重试，最多到只剩一条为止。这避免了"压缩爆掉就再也压不动了"的死循环。CC 用熔断器解决同问题；Codex 是裁剪到能压为止。

### 2.6 完整警告

压缩成功后 emit 一条 Warning event：

> "Heads up: Long threads and multiple compactions can cause the model to be less accurate. Start a new thread when possible to keep threads small and targeted."

承认压缩有损，建议用户长期用新会话。

---

## 3. DeepSeek-TUI / OpenCode

简短记一下：

- **DeepSeek-TUI**：源码搜过，**没有压缩**。它把模型上限暴露给用户，对话超长就让用户自己开新会话。代码里的 `truncate_*` 全是 UI 层的 ID/标题截断，不是上下文管理
- **OpenCode**（这个仓库）：是 UI / desktop 包装层，core 包里 grep `compact|summarize` 搜不到压缩逻辑。它依赖各 provider 自身或更上层 server 的策略

所以本质上**只有 CC 和 Codex 是真正在客户端做上下文压缩**。Hebbian 的设计就是从这两家身上各取所需。

---

## 4. Hebbian 的策略（当前实现）

### 4.1 总体定位

**两层防线** + **provider 级 prompt 缓存** + **per-session token 统计**：

1. **microcompact**（轻）—— 模型请求前把老 tool_result 影子化（学 CC）
2. **LLM 整段摘要**（重）—— 自动 / 手动 `/compact`（核心思路抄 Codex 的"接力摘要"）
3. **prompt cache**（横切）—— Anthropic / OpenAI / DeepSeek / Gemini 的命中数据穿透到 UI
4. **token 统计面板**（UI）—— 输入框外侧右边持续展示 input / output / cache 命中

仍未做（演进路线见 §5）：

- snipCompact（长结果中段裁剪）
- session memory / 增量摘要
- remote compaction
- reactive 413 fallback
- 自动整段摘要的"超阈值即调 LLM"（目前自动路径仍是结构化裁剪 fallback）

### 4.2 数据流

```
                              ┌──── 自动 ────┐
                              │              │
agent_loop 每轮开始        ┌──┴──┐           │
  ↓                        │     │           │
microcompact(transcript)   │     │      用户在输入框敲 /compact
  ↓ 老 tool_result→占位符  │     │           │
  ↓                        │     │           │
needs_compaction(transcript) ─yes─┤           │
compact_structural ──────► 结构化裁剪保留最近 N 轮    │
                                                     │
                                                     ▼
                                             chat::compact_session
                                                     │
                                                     ▼
                                       Session::compact() （agent-core）
                                                     │
                              ┌──────────────────────┼─────────────────────┐
                              │                      ▼                     │
                              │       compact_with_llm(client, system,     │
                              │                       entries, custom)     │
                              │                      │                     │
                              │  把 [前情概要 prompt] 追加到 entries 末尾    │
                              │                      │                     │
                              │              client.complete(req)          │
                              │                      │                     │
                              │              拿到 summary text             │
                              │                      │                     │
                              │   new_entries =                            │
                              │     [User("[前情概要]\n" + summary),        │
                              │      Assistant("已收到前情概要…")]          │
                              │                      │                     │
                              │                      ▼                     │
                              │       transcript.entries = new_entries    │
                              │                      │                     │
                              │       desktop 在 session 末尾追加一条       │
                              │       Role::Marker + MessageMeta::         │
                              │       CompactBoundary { summary,           │
                              │                         before_tokens,     │
                              │                         after_tokens }     │
                              └──────────────────────┼─────────────────────┘
                                                     ▼
                                          落盘 + 前端拉新 session
```

关键文件：

| 模块 | 文件 | 职责 |
|---|---|---|
| **microcompact** | [crates/agent-core/src/context/microcompact.rs](../crates/agent-core/src/context/microcompact.rs) `microcompact` | 工具结果影子化；trigger=12, keep=5 |
| 自动结构化裁剪 | [crates/agent-core/src/context/compaction.rs](../crates/agent-core/src/context/compaction.rs) `compact_structural` | 超 budget 时跳过老消息，保留 system + 最近 N 轮 |
| LLM 摘要 | [crates/agent-core/src/context/compaction.rs](../crates/agent-core/src/context/compaction.rs) `compact_with_llm` | 一次 `client.complete()`，用中文 prompt 让模型产出摘要 |
| Token 估算 | [crates/agent-core/src/context/budget.rs](../crates/agent-core/src/context/budget.rs) | 中文 ~1 token/char，英文 ~4 char/token |
| Session 入口 | [crates/agent-core/src/session.rs](../crates/agent-core/src/session.rs) `Session::compact / context_usage` | 暴露给 surface |
| Boundary marker | [crates/platform/src/storage/sessions.rs](../crates/platform/src/storage/sessions.rs) `MessageMeta::CompactBoundary` | 持久化 boundary |
| Token 统计 | [crates/platform/src/storage/sessions.rs](../crates/platform/src/storage/sessions.rs) `TokenStats` | session.json 持久化字段 |
| Usage 透传 | [crates/model-gateway/src/types.rs](../crates/model-gateway/src/types.rs) `Usage` | 4 项：input/output/cache_read/cache_creation |
| Transcript 重建 | [crates/agent-core/src/context/transcript.rs](../crates/agent-core/src/context/transcript.rs) `Transcript::from_session` | 加载时跳过最近 boundary 之前的消息 |
| Desktop 入口 | [apps/desktop/src/chat.rs](../apps/desktop/src/chat.rs) `compact_session / context_usage / accumulate_session_tokens` | Tauri 命令背后逻辑 |
| CLI 入口 | [apps/cli/src/session.rs](../apps/cli/src/session.rs) `run_compact` | `/compact [指令]` 交互命令 |
| Anthropic cache | [crates/model-gateway/src/protocols/anthropic.rs](../crates/model-gateway/src/protocols/anthropic.rs) `apply_cache_control` | 给 system 末尾 + 倒数第二条 message 打 ephemeral 标记 |
| OpenAI cache 解析 | [crates/model-gateway/src/protocols/openai.rs](../crates/model-gateway/src/protocols/openai.rs) `parse_usage` | `usage.prompt_tokens_details.cached_tokens` |
| DeepSeek cache 解析 | 同上 (OpenAI 兼容路径) | `usage.prompt_cache_hit_tokens` |
| Gemini cache 解析 | [crates/model-gateway/src/protocols/gemini.rs](../crates/model-gateway/src/protocols/gemini.rs) `parse_usage` | `usageMetadata.cachedContentTokenCount` |
| TokenStatsPanel | [apps/desktop/frontend/src/desktop/ui/components/TokenStatsPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/TokenStatsPanel.tsx) | 输入框外侧右边四行统计 |

### 4.3 Prompt

[crates/agent-core/src/context/compaction.rs](../crates/agent-core/src/context/compaction.rs) `COMPACT_PROMPT`：

```
你正在执行【上下文压缩】。请把当前对话历史浓缩成一份简明、结构化的接力摘要，
让另一个 LLM 能在不读原对话的情况下无缝继续工作。

请覆盖：
- 用户的核心目标 / 约束 / 偏好
- 已完成的关键工作和重要决策（含影响后续判断的细节）
- 仍未完成的事项 / 下一步
- 关键数据：文件路径、命令、代码片段、错误信息、外部链接
- 任何模型不读上下文就会丢的隐含上下文

输出要求：
- 直接给摘要正文，不要寒暄、不要 "以下是摘要" 之类的引导语
- 紧凑但不丢关键信息；优先 bullet list
- 保持中文
```

参考：

- 结构上学了 Codex 的"接力摘要"指令式风格 + CC 的覆盖项分类
- **没有**抄 CC 的 `<analysis> + <summary>` 双层 XML（暂时不需要，模型可以一遍出）
- **没有**抄 CC 的 NO_TOOLS_PREAMBLE，因为 `compact_with_llm` 调的是不带 tools 的 `complete()`，provider 那边自然没工具可用
- 自定义指令：`/compact 重点保留代码细节` ⇒ prompt 末尾追加 `\n\n附加指令：重点保留代码细节`

### 4.4 触发策略

| 场景 | 触发方式 | 做什么 |
|---|---|---|
| **每轮模型请求前** | `agent_loop::run_loop` 调 `microcompact` | 累积 ≥12 个可压缩 tool_result 后，把除最近 5 个之外的全部 content 替换成 `[结果已被压缩]` |
| 每轮开始时超 budget | `agent_loop::run_loop` 调 `needs_compaction` | `compact_structural`（结构化裁剪，不走 LLM） |
| 用户主动 `/compact` | CLI / Desktop 输入框拦截 | `Session::compact` → `compact_with_llm` |
| 接近上限（70/90%） | 输入框旁的 ContextRing 变色提醒 | 由用户决定是否手动压缩 |

**注意**：超 budget 的自动路径还是结构化裁剪而非 LLM 摘要。把它也接上 LLM 摘要列在演进路线 #1。

**Microcompact 白名单**（[microcompact.rs `COMPACTABLE_TOOLS`](../crates/agent-core/src/context/microcompact.rs)）：
`Bash` / `Read` / `Grep` / `Glob` / `Write` / `Edit` / `web_fetch` / `web_search`。
这些工具结果"看过就没用"且占 token 大头；`ask` / TodoWrite / Skill 等状态型工具不在白名单。

### 4.5 持久化 + UI（关键差异点）

CC 有 `compactBoundary` 系统消息但只在 REPL 缓存里能看到原历史。**Hebbian 把这个能力做成了正经数据：**

- `MessageMeta::CompactBoundary { summary, before_tokens, after_tokens }` 落盘到 session.json
- `Transcript::from_session` 加载时找最近一条 boundary，**跳过之前所有消息**，把摘要作为前情提要注入：
  ```
  [User] [前情概要]\n<summary>
  [Assistant] 已收到前情概要，将基于此继续。
  [User] <压缩后的第一条新消息>
  ...
  ```
- Desktop UI（[apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx](../apps/desktop/frontend/src/desktop/ui/components/MessageBubble.tsx)）：
  - **boundary 之前的原始消息默认折叠**（在 ChatView 跳过渲染）
  - 分隔条本身可点击，渲染成两个并列按钮：
    1. 主体「上下文已压缩 N → M tokens」⇒ 切换**摘要展开**，把 `meta.summary` 全文渲染在分隔条下方，用来让用户**判断压缩质量**
    2. 「历史对话 · X」⇒ 切换**原始历史展开**，把 boundary 前的原消息显示出来，每条以 `archived` 淡化样式 + tooltip
  - 两套展开状态独立（`expandedSummaries` / `expandedHistories`），多次压缩时每条 boundary 各自切换
- Desktop UI 输入框右侧 **ContextRing**（[apps/desktop/frontend/src/desktop/ui/components/ContextRing.tsx](../apps/desktop/frontend/src/desktop/ui/components/ContextRing.tsx)）：
  - 圆形 SVG 进度条 + 中心百分比
  - 70%/90% 阈值变色（amber / destructive）
  - 点击 = 触发 `/compact`
- CLI 提示符：`[N%] ›`，70/90% 阈值变色，输入 `/compact [指令]` 触发

### 4.6 Provider 级 Prompt 缓存

每家 provider 的 prompt cache 形态差异很大，hebbian 把它们统一规约成 `Usage` 上的两个字段：
`cache_read_tokens`（命中读出）+ `cache_creation_tokens`（写入；只 Anthropic 有）。**两者都已计入 `input_tokens`**——和账单口径对齐。

| Provider | 是否需要客户端打标记 | 命中字段 | 写入字段 | TTL / 折扣 |
|---|---|---|---|---|
| **Anthropic** | **是**：在 content block 上加 `cache_control: { type: "ephemeral" }` | `usage.cache_read_input_tokens` | `usage.cache_creation_input_tokens` | 5 分钟（默认）/ 1 小时（beta header）；命中读 = 0.1×，写入 = 1.25× |
| **OpenAI** | 否（自动）：超 1024 token 的稳定前缀自动缓存 | `usage.prompt_tokens_details.cached_tokens` | n/a（不计费） | 5–10 分钟，闲时延长到 1 小时；命中折扣 50% |
| **DeepSeek**（api.deepseek.com 走 OpenAI 兼容路径） | 否（自动） | `usage.prompt_cache_hit_tokens` | n/a | 命中价 ≈ 0.1× |
| **Gemini** | 显式 cache 用 `cachedContent` API；implicit cache 自动 | `usageMetadata.cachedContentTokenCount` | n/a | implicit 命中折扣 25% |

**Anthropic 的客户端打标记**（[anthropic.rs `apply_cache_control`](../crates/model-gateway/src/protocols/anthropic.rs)）：

最多 4 个 `cache_control` 标记，前一个标记到下一个标记之间的 prefix 都会被缓存。我们贴 2 个：

1. **system 末尾** —— 几乎所有轮都不变，命中率最高
2. **倒数第二条 message 的最后一个 block** —— 把"上一轮已经发过的历史"整段标缓存，本轮第一次发就把它写进缓存，下一轮 0 成本读

实现细节：
- system 是 `String` 的话先升格成 `[{type:"text", text, cache_control}]`
- 同样地把目标 message 的 `content` 升格成 block 数组再贴
- 只有 messages.len() ≥ 2 时才贴第二个标记，避免新会话单条消息也加 cache 写入费

### 4.7 Per-Session Token 统计

每次 run 结束（含 Done / Cancelled / Failed），surface 调
[`accumulate_session_tokens`](../apps/desktop/src/chat.rs) 把这一轮 `summary.usage` 累加进
`session.json` 的 `token_stats` 字段：

```rust
pub struct TokenStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,      // 已包含在 input
    pub cache_creation_tokens: u64,  // 已包含在 input
    pub run_count: u64,
}
```

前端从 `currentSession.token_stats` 直接读，不用再调一次后端。**重启后仍持久化**。

UI（[TokenStatsPanel.tsx](../apps/desktop/frontend/src/desktop/ui/components/TokenStatsPanel.tsx)）固定在
ChatInput **外侧右边**（`flex` row 布局，宽 176px，`hidden md:block` 在窄屏自动隐藏），
显示四行：

```
Token 用量              ×N
↑ 输入                 12.3k
↓ 输出                  4.5k
🗄  缓存命中            8.2k (66%)   ← 命中率 = cache_read / input
📦 缓存写入            1.2k         ← 仅 Anthropic 写入>0 时显示
```

数值压成 `1.2k` / `4.5M` 形态避免太宽。

### 4.8 与参考项目的对照

| 能力 | CC | Codex | Hebbian |
|---|---|---|---|
| 工具结果影子化（轮次） | ✓ cache_edits API | ✗ | **✓** trigger=12 / keep=5（content 占位符） |
| 工具结果影子化（时间） | ✓ 60min 缓存过期触发 | ✗ | ✗（计划中） |
| 长结果中段裁剪 | ✓ snipCompact | ✗ | ✗ |
| 自动整段摘要 | ✓ 阈值 | ✓ 阈值 | △ 当前是结构化裁剪，未走 LLM |
| 手动 /compact | ✓ | ✓ | ✓ |
| 自定义压缩指令 | ✓ | ✓ | ✓ |
| Reactive 413 fallback | ✓ | ✗ | ✗ |
| Server-side compact | ✗ | ✓ | ✗ |
| Initial context 重注入 | ✓ runPostCompactCleanup | ✓ InitialContextInjection | ✗ |
| 压缩 boundary 持久化 | ✓ compactBoundary 系统消息 | ✓ Compaction ResponseItem | ✓ MessageMeta::CompactBoundary |
| UI 上原历史可滚回 | ✓ | ✓ | ✓ |
| UI 上摘要可展开看质量 | ✗ | ✗ | **✓** |
| 上下文用量 % 实时显示 | ✓ /context 命令 | ✓ TUI 角标 | ✓ 输入框右侧 ContextRing |
| **Anthropic prompt cache 客户端打标** | ✓ | n/a | **✓** system + 倒数第二条 message |
| **OpenAI prompt cache 解析** | n/a | ✓ | **✓** prompt_tokens_details |
| **DeepSeek prompt cache 解析** | n/a | n/a | **✓** prompt_cache_hit_tokens |
| **Gemini prompt cache 解析** | n/a | n/a | **✓** cachedContentTokenCount |
| **Per-session token 持久化** | △ 仅最近 | △ 仅当前 | **✓** 落盘 + 跨重启 |
| **缓存命中率 UI 展示** | ✗ | ✗ | **✓** TokenStatsPanel |
| 压缩失败熔断 | ✓ 3 次连失 | △ 头部裁剪重试 | ✗ |
| 摘要 prompt 结构化 | ✓ 9 段 + analysis 草稿 | △ 短指令 | △ 短指令 + bullet 要求 |

---

## 5. 演进路线（按价值排序）

1. **自动触发的 LLM 摘要**：当前 `agent_loop` 超 budget 只跑 `compact_structural`（粗暴丢老消息）。改成超阈值时调 `compact_with_llm`，丢之前先要一份摘要。
2. **microcompact 时间触发**：除现有"轮次触发"外，加一条"距上次 assistant > 60min 触发"（学 CC）——server cache 几乎肯定过期了，索性提前压。
3. **snipCompact**：单条超长 tool_result（如 web_fetch 整页）保留头尾、中段替换成 `[trimmed N tokens]`。和 microcompact 的"整条替换"互补。
4. **压缩失败熔断**：连续 3 次失败禁用本 session 的自动压缩（学 CC）。
5. **prompt-too-long reactive 路径**：当 provider 直接报 prompt 超长时，先压一次再重试本轮请求（学 CC）。
6. **细化 Prompt**：抄 CC 的 9 段结构 + `<analysis>+<summary>` XML、「ALL user messages」、「Optional Next Step + verbatim 引用」段，提升摘要的可执行性。
7. **`InitialContextInjection`-like 注入**：压缩后下一轮自动重注入 system prompt 中的 workspace XML / skill 列表，避免摘要漏掉环境信息。
8. **`compact_remote`**：对支持的 provider（OpenAI Responses 等）走 server-side 压缩，省一次完整 history 上传。
9. **CLI TokenStatsPanel**：CLI 提示符里加一行 `↑12.3k ↓4.5k 🗄8.2k(66%)`，对齐 desktop 体验。
10. **`reasoning` 字段也参与 token 估算**：当前 `entry_tokens` 只算 reasoning 字符串数；DeepSeek 等 thinking-aware provider 的 reasoning 占比可观。
11. **Anthropic 1h cache TTL beta**：高频会话开 `extended-cache-ttl-2025-04-11` beta header，把 5min 拉成 1h，减少 cache miss。

---

## 6. 调试清单

- 想看一次压缩前后 token：CLI `/compact` 会 stderr 打印 `before → after tokens` + 摘要全文
- Desktop 上点击分隔条主体 = 看摘要本体 = 评估压缩质量
- 上下文进度环超过 70% 还没 /compact 的话，问问自己是不是该开新会话或先 /compact
- 想看压缩后模型实际看到了什么：`Transcript::from_session` 是真理；它跳过最近一条 `CompactBoundary` 之前的消息，把 `summary` 作为前情提要注入
