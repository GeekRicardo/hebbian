# 上下文压缩：参考项目调研与 Hebbian 设计对照

> 本文档为背景资料，记录三大参考项目（claude-code-haha / codex / opencode）的压缩策略调研，以及 Hebbian 在此基础上的取舍。
>
> 当前压缩设计的权威描述位于 [架构.md §4.7](架构.md)，本文不重复定义，仅提供横向对照与设计依据。

---

## 1. 为什么需要压缩

LLM 上下文窗口存在上限：

- 旧消息无限累加导致每轮请求重传整段历史，token 成本与延迟随对话长度上升
- 接近窗口上限时模型对长尾位置信息的检索能力下降（lost-in-the-middle）
- 超过上限时 provider 直接返回 `prompt_too_long`，session 不可继续

压缩的目标是：在不破坏 agent 任务连续性的前提下，将对话历史替换为更短的、信息密度更高的等价表达。

---

## 2. claude-code-haha 的多层防线

claude-code-haha 是参考实现中最完整的方案。其压缩策略由轻到重分四层叠加，每层只解决一类问题：

```
轻 ──────────────────────────────────────────────────────────────► 重
[1] microcompact     [2] snipCompact     [3] auto / reactive    [4] manual /compact
   shadow 工具结果      裁剪长结果中间        达阈值整段摘要        用户主动整段摘要
```

### 2.1 microcompact（轮次 / 时间触发的工具结果影子化）

来源：[`src/services/compact/microCompact.ts`](https://github.com/.../microCompact.ts)

**适用工具白名单**：`Read / Bash / Grep / Glob / WebSearch / WebFetch / Edit / Write`

这些工具的输出特征：
- 占 token 比例高
- 一旦读过，再回看价值低（文件已编辑、命令已跑完）

`Task` / `TodoWrite` 等状态型工具不在列表内，因其结果对后续决策仍有用。

**两种触发机制**：

1. **轮数触发**（cached microcompact，需 `CACHED_MICROCOMPACT` feature）
   - 每条 user 消息作为一组 `tool_result` 的容器
   - 按"已注册工具数量"达到 `triggerThreshold` 触发
   - 保留最近 `keepRecent` 个不动，老的标记删除
   - 阈值由 GrowthBook 远程下发，典型值约触发 20+ / 保留最近 5
   - 关键机制：使用 Anthropic 的 `cache_edits` API 在不打破前缀缓存的前提下"打洞"

2. **时间触发**（time-based microcompact）
   - 距上一条 assistant 消息超过 `gapThresholdMinutes` 分钟（默认 60，对齐 Anthropic prompt 缓存 TTL）触发
   - 此时 server 端缓存已大概率过期，下一次请求无论如何都要重写整段前缀
   - 直接将老 tool_result 内容置空（`[Old tool result content cleared]`）
   - 保留最近 `keepRecent` 个（默认 5）

**不动的内容**：user 文本、assistant 文本、thinking、最近 N 个工具结果。

**后处理**：
- 触发 `notifyCacheDeletion` / `notifyCompaction`，告知 cache-break 监控本次命中率下降属于预期
- `suppressCompactWarning`：UI 不再提示"快撞墙"

### 2.2 snipCompact（裁剪长结果中间）

为 claude-code-haha 内部 feature（外部 build 为 stub）。

行为：当单个 tool_result 极大（如 web_fetch 抓取的长网页）时，保留头尾、中间替换为 `[trimmed N tokens]` 占位。这是"消息内裁剪"，不影响轮数，对 microcompact 之后仍嫌大的单条结果做精修。

### 2.3 autoCompact（接近上限时自动整段摘要）

来源：[`src/services/compact/autoCompact.ts`](https://github.com/.../autoCompact.ts)

**触发条件**：

```
effective_window = model_context_window - max_output_tokens_for_summary
                  （上限 20_000，p99.99 摘要输出）
auto_compact_threshold = effective_window - AUTOCOMPACT_BUFFER_TOKENS（13_000）
```

剩余预算不足约 13k token 时触发。

**三个守门人**：

1. `DISABLE_COMPACT` / `DISABLE_AUTO_COMPACT` 环境变量
2. recursion guard：`session_memory` / `compact` / `marble_origami` 等 forked agent 不能再触发自己（防死锁与状态损坏）
3. 熔断：连续 3 次失败即本 session 不再尝试

**三种实现按优先级尝试**：

1. `trySessionMemoryCompaction` —— session memory 快路径，若存在上次摘要可增量更新则复用
2. `compactConversation` —— 主路径，详见 2.5
3. `reactiveCompact` —— 413 fallback：模型已报 `prompt_too_long` 时再"反应式"压一次

### 2.4 manual `/compact`

来源：[`src/commands/compact/compact.ts`](https://github.com/.../compact.ts)

- 用户主动 `/compact [自定义指令]`
- 先尝试 sessionMemory（不接受 `customInstructions`）
- 失败则 microcompact + compactConversation
- 自定义指令通过 `mergeHookInstructions` 合并到摘要 prompt 中，引导模型按用户意图侧重

### 2.5 摘要 Prompt 的关键设计

来源：[`src/services/compact/prompt.ts`](https://github.com/.../prompt.ts)

**双层 XML 结构**：

- `<analysis>` 作为模型的 scratchpad，强制其先逐条记录"用户做了什么 / 我做了什么 / 出了什么错 / 用户怎么反馈"
- `<summary>` 是最终保留并注入 transcript 的部分
- `formatCompactSummary` 在写回 transcript 前剥掉 `<analysis>`——它是为了写好 summary 而存在的草稿，不进 context

**强制 9 段结构**：

1. Primary Request and Intent
2. Key Technical Concepts
3. Files and Code Sections（含完整代码片段，不省略）
4. Errors and fixes（含用户反馈）
5. Problem Solving
6. ALL user messages（非工具结果）
7. Pending Tasks
8. Current Work（含 verbatim 引用）
9. Optional Next Step（含 verbatim 引用，避免 task drift）

**NO_TOOLS_PREAMBLE / NO_TOOLS_TRAILER**：因 cache-share fork 路径继承了完整工具集，Sonnet 4.6+ 偶尔在压缩这一轮也尝试调用工具，故首尾两段 CRITICAL 提示禁止任何 tool_call。

**三种变体**：

- `BASE_COMPACT_PROMPT` —— 整段压缩
- `PARTIAL_COMPACT_PROMPT` —— 前有 retained context，仅压最近尾段
- `PARTIAL_COMPACT_UP_TO_PROMPT` —— 摘要在前，未来消息接在后

**Compact 保留器 `compactBoundary` 系统消息**：

- 模型看到的 transcript = `getMessagesAfterCompactBoundary(messages)`
- REPL 端仍保留 boundary 之前的消息，UI 上能滚回去看

**后处理 `runPostCompactCleanup`**：

- 重新读最近 ≤5 个文件（5k tokens / 文件）
- 重新注入最近用过的 skill 文件（5k / skill，总额 25k）
- `notifyCompaction` + `markPostCompaction` + `clearCompactWarningSuppression`

---

## 3. codex 的压缩策略

codex 的压缩相对集中（无 microcompact 那一层），但**双实现**与**回退裁剪**值得借鉴。

### 3.1 触发与实现

来源：[`codex-rs/core/src/compact.rs`](https://github.com/openai/codex)

**配置项** `model_auto_compact_token_limit`：每个模型独立的 token 阈值。主循环监控 token 使用，超过即调 `run_inline_auto_compact_task`。

**手动 `/compact`** —— `run_compact_task`，强制 `InitialContextInjection::DoNotInject`。

**两套实现**：

1. **Inline（本地）** `run_inline_auto_compact_task`：本地客户端发一次 `client.stream(prompt)` 拿摘要
2. **Remote（服务端）** `compact_remote.rs`：当 provider `supports_remote_compaction()` 时走服务端压缩 API（OpenAI Responses API 的 server-side compact），不需要把整段历史回传一次

### 3.2 InitialContextInjection —— 上下文重注入策略

```rust
pub(crate) enum InitialContextInjection {
    BeforeLastUserMessage, // mid-turn 自动压缩用：把 initial_context 注入到最后一条 user message 之前
    DoNotInject,           // 手动 /compact 用：清空 reference_context_item，下一轮自然重注入
}
```

- mid-turn 压缩必须保留 initial context（环境 / 工作目录 / 工具清单），否则模型会忘了自己在哪干活
- 手动 /compact 主动清空，让用户主动开启的下一段对话从干净状态开始

### 3.3 压缩后的 history 重建

codex 的 history 是 `Vec<ResponseItem>`（细粒度），压缩后并非简单替换全部 entries，而是按 ResponseItem 类型重组：

- 保留 `ResponseItem::Message { role: "system" }`
- 摘要写为新的 `ResponseItem::Message { role: "user", content: vec![ContentItem::InputText { text: summary }] }`
- 紧跟一个 `ResponseItem::Message { role: "assistant", content: ack }`
- 工具调用历史（FunctionCall / FunctionCallOutput）按需保留或丢弃

详细的 `CompactedItem` 类型定义见 codex 的 `protocol::CompactedItem`。

---

## 4. opencode 的压缩策略

opencode 的压缩相对简单：

- 没有 microcompact 层
- 按 token 上限触发，单一摘要 prompt
- 摘要写入 `[前情概要]` user message + assistant ack
- 不保留压缩前的内容

适合短对话场景，长对话场景下信息丢失明显。

---

## 5. Hebbian 设计的取舍

Hebbian 综合上述三家的优点，在 [架构.md §4.7](架构.md) 定义了完整的四层压缩 + 压缩工件落盘机制。本节仅列要点对照。

### 5.1 与 claude-code-haha 的对照

| 层 | claude-code-haha | Hebbian |
|---|---|---|
| L0 微压缩 | 单一白名单（8 个工具同等对待）| Tier 化白名单（3 层，按工具语义粗分）|
| L1 大输出 | snipCompact 中间裁剪 | 落盘到 `session/tool_results/<call_id>.txt`，transcript 放占位符 + 路径 |
| L2 结构裁剪 | 有 | 有 |
| L3 LLM 摘要 | 4 段 XML prompt | 沿用 4 段 XML prompt，落盘到 `session/compactions/compact-<ts>.md` |
| 压缩可追溯 | session memory | 落盘 md，LLM 可通过 Read 工具按需检索 |
| Cache 边界 | cache_edits API | STABLE / SEMI / MUTABLE 三段，cache_breakpoints 显式标记 |

**关键差异**：
- Hebbian 通过文件落盘机制保证压缩**信息不丢**，LLM 可在需要时通过 Read 工具检索原始内容
- 配套 `context_recall.md` prompt 引导 LLM 避免"压缩后又整文 read"的反模式
- Tier 化白名单使 Read / Edit 这类语义价值高的结果更晚被压缩

### 5.2 与 codex 的对照

| 项 | codex | Hebbian |
|---|---|---|
| 压缩实现 | inline + remote 双实现 | 仅 inline（本地） |
| InitialContextInjection | mid-turn vs manual 区分 | mid-turn 通过 `<environment>` 块自动续注入 |
| 压缩后 history | ResponseItem 细粒度重组 | TranscriptEntry 扁平 3-variant 重组（User + Assistant ack）|

**未采用的能力**：
- Remote 压缩（codex 依赖 OpenAI Responses API 特性，Hebbian 多 provider 难统一）
- 多模型独立 `auto_compact_token_limit` 配置（Hebbian 用 `token_budget_factor` 按比例触发）

### 5.3 创新点

1. **压缩工件落盘**：将被压缩的原始内容存为 txt / md，LLM 可按需检索（claude-code-haha 的 session memory 路径仅在 forked agent 内有效，Hebbian 直接走文件系统）
2. **按需检索 prompt**：context_recall.md 内置 prompt 引导 LLM 优先用 transcript + Grep / Read 局部检索，避免压缩被无效化
3. **Tier 化白名单**：按工具语义分层压缩，Read / Edit 等保留更久
4. **Cache 边界感知**：STABLE / SEMI / MUTABLE 三段切分，Mode 切换不动 system prompt

---

## 6. 实施参考

Hebbian 压缩相关的实施细节由架构.md §4.7 与迁移路线 Step 9 描述。当前现状与目标状态的差距记录于 changelog.md。

后续详细设计阶段将单独产出：

- `docs/step-9-compaction.md`：Tier 化 microcompact 算法、artifact 落盘格式、context_recall prompt 文本
- `docs/system-prompt.md`：context_recall 段的最终 prompt 内容
