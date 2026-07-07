# Step 9：上下文管理策略详细设计

本文是 Hebbian 当前要实现的上下文管理策略详细设计。它只描述 Hebbian 自身的目标态、落地步骤、阈值、存储与验收标准。

## 1. 目标与非目标

### 1.1 目标

1. **降低模型可见上下文压力**：在不丢任务状态、不丢原始信息的前提下，尽量减少每次模型请求携带的历史 token。
2. **大输出默认预算化**：工具可以产生任意长度原始输出，但进入模型上下文前必须经过清洗、截断、落盘与可恢复提示。
3. **中压先压可再生信息**：上下文压力达到中等水平时，优先把久远、巨大、可重新检索的工具输出替换为占位符，而不是过早对整段对话做 LLM 摘要。
4. **高压再做正式压缩**：上下文压力达到高水平时，生成任务 checkpoint、归档旧 transcript、产出摘要，并用“summary + checkpoint + recent tail”重建模型可见上下文。
5. **压缩信息可追溯**：被替换掉的工具输出、旧消息窗口、压缩前 transcript 都必须作为 artifact 落盘，并在模型可见文本里给出路径和检索方式。
6. **任务状态不能丢**：压缩前必须从真实状态源生成 checkpoint，不能只依赖 LLM summary 回忆当前任务。
7. **三 surface 行为一致**：上下文清洗、截断、压缩都在 agent-core 完成，Desktop / heb CLI / hebweb 只负责展示事件与 marker。

### 1.2 非目标

1. 不实现真正“无限模型上下文”。Hebbian 的无限感来自完整轨迹持久化、artifact 可检索和可控的上下文重建，不是把所有历史重新塞回模型。
2. 不在第一版做复杂的输出形状识别全集。第一版只做通用清洗、secret redaction、head+tail、基础错误关键词倾斜；专门的 pytest / cargo / tsc / npm / diff 提炼可以逐步补。
3. 不引入第二份历史账本。`session.jsonl` 仍是唯一对话历史文件；artifact 是可检索配套文件，不替代历史账本。
4. 不让 surface 各自实现截断策略。surface 可做视觉折叠，但不能改变进入模型的上下文内容。
5. 不让 token-budget reset 替代常规压缩。它只在已有新鲜 checkpoint 时作为快速窗口重置或故障兜底。

## 2. 架构定位与影响评估

### 2.1 对应架构章节

本设计落在以下架构章节内：

- `docs/架构.md §4.1.3`：Session 是压缩策略的承载主体，`SessionConfig.compaction` 提供默认阈值与工具级覆盖。
- `docs/架构.md §4.3.1`：AgentLoop 在模型调用前执行上下文治理，在工具执行后包装大输出 artifact。
- `docs/架构.md §4.4`：工具输出统一进入 `ToolOutput { text, attachments[] }`，模型可见文本由 core 决定。
- `docs/架构.md §4.7`：Context Engine 与压缩工件。
- `docs/架构.md §4.9`：Recorder 与 `session.jsonl` 唯一历史账本。
- `docs/架构.md §6.1 / §6.2 / §6.3`：session 目录布局、storage 模块和文件锁 / atomic rename。
- `docs/架构.md §9.3`：上下文重建不能破坏 STABLE prompt cache；mode 变化只动 SEMI 段和工具列表。

### 2.2 是否与架构相悖

不相悖。本设计延续以下硬约束：

- 上下文管理在 agent-core 内完成。
- `session.jsonl` 是唯一历史账本。
- 被压缩内容落盘，模型按路径检索。
- 工具名在协议层保持 PascalCase。
- artifact、compaction、session 追加写均走 storage 模块、文件锁和 atomic rename。

### 2.3 是否引入新设计 / 需修改架构

本设计细化现有 §4.7 的目标态，不改变已有对外协议语义。它新增的是实现策略：压力阈值、清洗 pipeline、head+tail 预览、checkpoint 内容、token-budget reset 适用边界。

短期实现不必须新增协议字段；已有 `ToolResultsCompacted`、`ContextCompactionStarted`、`ContextCompactionProgress`、`ContextCompacted` 足够表达主要事件。若后续希望前端结构化渲染 artifact path、before/after token、compaction window id，可在 protocol 中 additive 增加 payload 字段，并同步 `to_wire` 与前端 `types.ts`。

### 2.4 影响模块

- **agent-core / Context Engine**：新增压力监控、microcompact 候选选择、正式 compact 流程、artifact index 与 replacement transcript builder。
- **agent-core / dispatch**：工具执行后统一进入输出清洗和 L1 artifact 包装；工具实现本身不各自截断。
- **agent-core / task 状态**：压缩前从 todo / plan / run state / pending HITL / background tasks 生成 checkpoint。
- **agent-core / storage**：复用 `tool_results/`、`compactions/`、`plans/`；必要时在 `compactions/` 下增加 `.jsonl` raw segment 与 `.meta.json`。
- **protocol**：第一版可以复用现有 compaction 事件；后续字段只做 additive。
- **Desktop / heb CLI / hebweb**：展示 tool result marker、compaction marker、warning，不做业务截断。

### 2.5 取舍

- **prompt cache vs 节省 token**：L2 microcompact 会改变历史消息，可能让 provider prompt cache 失效；因此在 50% 前不动历史，只对新工具输出做 L0/L1，50% 后才压老工具输出。
- **语义保留 vs 实现复杂度**：第一版不做复杂 shape filter，避免误删错误细节；先用通用清洗、head+tail、错误尾部倾斜解决大多数问题。
- **压缩积极性 vs 可恢复性**：成功的大工具输出优先压，失败的 Edit / Write-like 输出更保守，因为失败细节通常直接影响下一步修复。
- **速度 vs 语义完整性**：token-budget reset 快且稳定，但不生成摘要，语义损失更大；只允许在 checkpoint 新鲜且无 pending 状态时使用。

## 3. 总体分层

上下文管理分五层，每层只解决一类问题：

| 层级 | 名称 | 触发时机 | 作用 | 落盘物 |
|---|---|---|---|---|
| L0 | Tool Output Sanitize | 工具输出产生后 | 清洗控制字符、进度条、secret、超长行 | 无，或记录 metadata |
| L1 | Tool Output Artifact | 单条工具输出超预算 | 原文落盘，模型只看 head+tail 预览和恢复指引 | `tool_results/<call_id>.txt` + `.meta.json` |
| L2 | Pressure Microcompact | 上下文压力 ≥ 50% | 把老的、巨大的、可再生工具结果替换为占位符 | 复用 `tool_results/` |
| L3 | Checkpoint + Summary Compact | 上下文压力 ≥ 75% 或手动 compact | 归档旧窗口，生成 checkpoint 和摘要，重建 transcript | `compactions/compact-*.md/jsonl/meta.json` |
| L4 | Emergency Reset | ≥ 85% 或 provider 报 prompt_too_long | 尽力归档与 checkpoint 后急救；必要时 checkpoint-backed reset | `compactions/` |

核心原则：**越低层越机械、越可逆；越高层越语义化、越需要保护任务状态。**

## 4. 上下文压力阈值

上下文压力使用 provider 实际上限作为分母，优先采用服务端 `usage.input_tokens` 校准后的估算值；没有服务端样本时退化到本地估算，但必须保守留 buffer。

### 4.1 压力区间

| 压力区间 | 策略 |
|---|---|
| `< 50%` | 不改写历史；只对新工具输出执行 L0/L1。 |
| `50% - 65%` | light microcompact：压最早、最大、成功且可再生的工具输出，目标降到约 `45%`。 |
| `65% - 75%` | aggressive microcompact：扩大候选集合，但保护最近 `2-3` 个完整 turns，目标降到约 `55% - 60%`。 |
| `>= 75%` | 正式 compact：checkpoint、归档旧窗口、生成 summary、重建 replacement transcript。 |
| `>= 85%` | emergency compact / reset：若 summary compact 来不及或失败，在已有 checkpoint 时执行 token-budget reset。 |
| provider `prompt_too_long` | reactive compact：不得丢原文；先归档，再重建上下文并重试或让用户选择。 |

阈值必须留有余量，因为工具 schema、system prompt、图片、provider tokenizer 差异都会让本地估算偏离真实值。

### 4.2 Token 估算校准

压力计算必须同时服务两个入口：模型请求前的 `needs_compaction`，以及 surface 上展示的 `context_usage`。两者必须使用同一套口径，避免 UI 显示安全但下一轮请求直接超限。

默认公式：

```text
local = estimate_transcript_tokens(transcript)
ratio = clamp(last_real_input_tokens / last_estimated_tokens, 1.0, 3.0)
calibrated = local * ratio
pressure = calibrated / provider_context_window
```

规则：

- 每次模型请求成功后，持久化这一轮的 `usage.input_tokens` 与请求前的 `last_estimated_tokens`，下一轮用它们校准当前估算。
- `last_real_input_tokens == 0`、`last_estimated_tokens == 0` 或 provider 不返回 usage 时，退化为裸估算，但阈值仍按保守策略触发。
- `estimate_transcript_tokens` 必须把 system prompt、developer prompt、tool schema 等恒定开销纳入估算分母；否则短对话会让校准比值异常放大。
- 图片 token 估算与 base64 字节数无关：按模型原生图片 token 估算或按 VisionBridge 文本描述上限估算，取覆盖两条路径的保守值。
- `ratio` 默认上限为 `3.0`，防止异常样本把后续压力整体抬爆；实现中可按 provider 覆盖，但必须有上限。
- compact 后不清空 ratio。压缩会让 `local` 下降，ratio 继续吸收 tokenizer 差异和恒定开销，下一轮真实 usage 会自然校正。

## 5. L0：Bash / Tool Output 清洗

所有文本工具输出进入 transcript 前先经过同一条清洗 pipeline。工具实现返回 raw text，清洗由 dispatcher / context engine 包装层完成。

### 5.1 Pipeline 顺序

1. **Fold carriage return progress**
   - 处理 `\r` 进度条、下载条、spinner、单行刷新日志。
   - 同一物理行内多次 `\r` 只保留最后一帧。
   - 目的：避免把“进度 1%、2%、3%……”全部写入上下文。

2. **Strip ANSI / control chars**
   - 去掉 CSI / OSC / DCS ANSI escape。
   - 处理 backspace overstrike，如 `a\bb` 只保留最终可见字符。
   - 删除无意义控制字节，但保留 `\n`、`\t`。

3. **Redact secrets**
   - 在截断前执行，避免 secret 留在 head 或 tail。
   - 匹配常见形态：`Bearer ...`、JWT、PEM block、AWS access key、GitHub token、OpenAI / Anthropic key、Slack token、`token=...`、`api_key=...`、`password=...`、`secret=...`。
   - 替换为稳定占位，例如 `[REDACTED:aws_access_key]`。
   - 默认 artifact 保存 sanitized full text，不保存被识别出的 secret 原文。metadata 只记录 redaction 类型与计数，不记录 secret 值。
   - 如果未来需要保存未脱敏 raw output，必须另行设计加密、权限、生命周期和清理策略；不能把 raw secret 混进普通 `tool_results/`。

4. **Long-line elide**
   - 单行超过阈值时折叠，避免 base64、minified JSON、bundle、长 SQL、单行日志打爆上下文。
   - 默认：`max_line_chars = 800`，保留 `head = 240`，`tail = 120`。
   - 占位格式：`<elided N chars>`。

5. **Optional shape filters**
   - 第一版只做基础错误关键词倾斜，不做全量专用解析。
   - 后续可逐步增加：cargo / rustc、tsc、pytest、npm、git diff、stacktrace、JSON logs。
   - shape filter 只能删噪声，不能删掉失败原因、文件路径、行列号、exit code。

### 5.2 Bash 输出特殊规则

- `command` 参数完整保留，stdout / stderr 可清洗和压缩。
- exit code、是否 timeout、是否 background、task_id 必须保留。
- 如果输出以失败结束，tail 权重提高，因为错误通常在末尾。
- 对持续进度输出，优先保留最终状态、最后错误、最后 N 行。

## 6. L1：大工具输出 artifact 与 head+tail 预览

### 6.1 触发

首版沿用架构 §4.1.3 / §4.7 的既有阈值，不在本文强行改默认值：

- Read 由自身分页保护，首版可继续豁免 L1，但 Read 的大结果仍可被 L2 microcompact。
- Bash / Grep 等工具按现有预算进入 artifact 包装。
- 其他工具结果超过 artifact 阈值时落盘。

后续若统一阈值，必须只改 `SessionConfig.compaction` 默认值，并在 changelog 记录。

### 6.2 落盘内容

工具输出超限时：

1. 保存 sanitized full text 到 `~/.hebbian/sessions/<sid>/tool_results/<call_id>.txt`。
2. 保存 metadata 到 `~/.hebbian/sessions/<sid>/tool_results/<call_id>.meta.json`：
   - `call_id`
   - `tool_name`
   - `created_at`
   - `raw_bytes` / `sanitized_bytes`
   - `line_count`
   - `estimated_tokens`
   - `input_summary`
   - `exit_code` / `success` / `error_kind`
   - `redactions_count`
   - `preview_policy`
3. transcript 中的 tool result 替换为模型可见预览。

### 6.3 Head+tail 预览

模型可见预览不是只保留 head，而是 head+tail：

- 普通输出：head `60%` + tail `40%`。
- 失败输出或含错误关键词：head `35%` + tail `65%`。
- 若 tail 中出现 `error`、`failed`、`panic`、`traceback`、`exception`、`exit code`、`mismatched`、`cannot find` 等关键词，提高 tail 权重。
- marker 必须写清楚省略规模和恢复路径。

示例：

```text
[工具输出过长，已保存完整内容]
Tool: Bash
Call ID: call_abc123
Original: 18,420 lines / 1.9 MB / ~260,000 tokens
Shown: first 120 lines + last 180 lines
Full output: tool_results/call_abc123.txt

--- BEGIN HEAD ---
...
--- END HEAD ---

... 18,120 lines omitted ...

--- BEGIN TAIL ---
...
--- END TAIL ---

Need details? Use Grep on the artifact first, then Read with offset/limit. Do not read the full file unless necessary.
```

## 7. L2：压力触发的 microcompact

L2 的目标是在正式 compact 前，先压缩低价值、高体积、可恢复的老工具输出。

### 7.1 候选保护规则

以下内容默认不压：

- 最近 `2-3` 个完整 turns。
- 当前未完成的 tool loop。
- 尚未配对完成的 tool_use / tool_result。
- user messages。
- assistant final answers。
- Ask 的问题与用户答案。
- TodoWrite / PlanMode / task checkpoint。
- approval / permission 相关决策。
- memory 写入结果。
- subagent final summary。

失败的 Edit 或 Write-like 工具结果默认更晚压，因为失败细节往往直接影响下一步修复。

### 7.2 优先压缩顺序

1. Bash 成功大输出。
2. Read 大文件输出。
3. Grep 大量匹配。
4. Glob 大列表。
5. Fetch / Web 页面正文。
6. WebSearch 多结果列表。
7. 成功 Edit / Write-like 工具的大参数或大结果摘要。

### 7.3 评分算法

候选按评分从高到低压缩：

```text
score = size_score * 3
      + age_score * 2
      + regenerable_score * 2
      - semantic_value_score * 3
      - failure_value_score * 2
      - recent_protected * INF
```

含义：

- `size_score`：越大越该压。
- `age_score`：越早越该压。
- `regenerable_score`：可通过参数重新查询 / 重新运行 / artifact 恢复则更该压。
- `semantic_value_score`：决策、需求、计划、最终结论更不该压。
- `failure_value_score`：失败上下文对后续修复更重要。
- `recent_protected`：命中最近 turn 保护则直接排除。

### 7.4 占位符内容

microcompact 后的占位符必须足够恢复：

```text
[旧工具结果已压缩]
Tool: Grep
Call ID: call_def456
Input: pattern="ContextCompacted", path="crates/agent-core"
Original: 2,140 lines / 180 KB / ~24,000 tokens
Status: success
Artifact: tool_results/call_def456.txt
Reason: context pressure 63%, old large regenerable output

Use Grep on the artifact or rerun the original tool with a narrower query if details are needed.
```

### 7.5 停止条件

- `50% - 65%`：压到约 `45%` 后停止。
- `65% - 75%`：压到约 `55% - 60%` 后停止。
- 压完所有安全候选仍 `>= 75%`：进入 L3 正式 compact。

## 8. Edit / Write-like 参数压缩策略

Hebbian 当前内置写文件能力以 `Edit` 为主；如果未来重新引入 `Write`，或 MCP 工具提供写文件能力，统一按 Write-like 处理。

### 8.1 Edit 成功

- 小型 `old_string` / `new_string` 可以保留。
- 大型参数压缩为：
  - `file_path`
  - operation：replace / replace_all / create
  - `old_hash` / `new_hash`
  - bytes / lines
  - 关键 preview
  - 受影响行号范围（能计算时）
  - artifact path
- 创建或全覆盖大文件时，不把完整新内容长期留在模型上下文里。真实文件是最终真源，artifact 用于回看当时输入。

### 8.2 Edit 失败

失败结果更保守：

- 保留失败原因。
- 保留相关 `old_string` / `new_string` snippet。
- 若因为匹配失败，保留足够定位上下文的片段。
- 若参数过大，仍可 artifact，但预览要偏向失败位置和 tail。

### 8.3 Bash / Read / Grep 参数

- Bash command 完整保留，输出可压。
- Read / Grep 的 path、offset、limit、pattern、glob 参数完整保留，输出可压。
- URL / query 等外部检索参数完整保留，正文可压。

## 9. L3：Checkpoint + Summary Compact

正式 compact 不是简单“总结历史”。它必须先固定状态，再归档，再摘要，最后重建模型可见上下文。

### 9.1 流程

```text
compact_transcript(reason):
  acquire session compaction lock

  flush_partial_outputs()                  # 已流式输出的 assistant 段必须先落盘
  checkpoint = build_task_checkpoint()

  if has_unresolved_tool_use_or_pending_hitl():
    postpone_or_fail_safe()

  old_window = select_compaction_window(transcript)
  archive = write_compaction_artifacts(old_window, checkpoint)

  summary = compact_with_llm(old_window, checkpoint, archive.reference)
  replacement = build_replacement_transcript(
    canonical_context,
    checkpoint,
    archive.reference,
    summary,
    recent_full_turns,
    recent_user_messages,
  )

  append_compaction_marker_to_session_jsonl(archive, summary, before_after_tokens)
  swap_visible_transcript(replacement)
  emit ContextCompacted
```

### 9.2 Compact window 选择

`old_window` 必须按 transcript 结构边界切，不能只按 token 数粗暴裁剪。

规则：

- system / developer / project context 不进入可压缩窗口；replacement transcript 总是使用当前最新 canonical context。
- 只能按完整 turn 或完整 tool loop 边界切分，不能切半个 assistant tool call，也不能留下未配对 tool_use / tool_result。
- 当前 active turn、最近 `2-3` 个完整 turns、最新用户消息默认留在 recent tail，不进 old window。
- pending approval / pending Ask / 未完成后台任务相关上下文默认不进 old window；若 emergency 下必须压缩，checkpoint 必须完整记录恢复方式。
- compact marker 本身可作为历史事实保留，但不能被当成普通 user/assistant 内容反复总结；多次 compact 时，新 summary 应引用旧 compact artifact，而不是把旧归档全文重新塞入 prompt。
- 手动 compact 可允许用户附加压缩指令，但仍必须遵守边界切分、checkpoint、归档和不丢原文规则。

### 9.3 session.jsonl 顺序与 marker 约束

`session.jsonl` 是唯一历史账本，compact marker 的物理顺序就是恢复语义的一部分。

硬约束：

- 写 `ContextCompacted` / CompactBoundary marker 前，必须先 flush 已经流出的 assistant partial 和已完成 tool result，保证 marker 落在被压缩窗口之后。
- reload session 时必须按 `session.jsonl` 物理行顺序重建 transcript；`created_at` 只能用于展示或诊断，不能作为排序真源。
- compact marker 必须记录被压缩窗口的 `start_entry_id`、`end_entry_id`、`archive_id`、summary hash、checkpoint hash 和 artifact index id。
- 前端不能靠时间戳或相邻 bubble 猜压缩边界；只能消费 core 给出的 marker / event。
- 如果写 marker 前任何一步失败，保留原 transcript，不写半成品 marker。已经写好的 artifact 可作为 orphan 由后续 sync/cleanup 处理，但不能让 transcript 指向不存在或未完成的归档。

### 9.4 Replacement transcript 组成

正式 compact 后，模型可见上下文按以下顺序重建：

1. **canonical system / developer / project context**
   - 使用当前最新上下文，不复用旧的 stale system prompt。
   - 不改变 STABLE prompt；mode 变化仍只动 SEMI 段与工具列表。

2. **task checkpoint**
   - 当前目标、任务状态、下一步、已读/已改文件、pending 风险。

3. **compaction archive reference**
   - 旧窗口摘要路径、raw jsonl segment 路径、检索方式。

4. **summary**
   - 旧窗口语义摘要，重点保留用户需求、决策、约束、已尝试路径、失败原因、结论。

5. **recent full turns**
   - 默认保留最近 `2` 个完整 turns。
   - 如果最近有错误修复、活跃工具 loop、用户刚改需求，则保留 `3+` 个。

6. **recent user messages**
   - 确保最新用户要求不会只存在 summary 里。
   - 若已包含在 recent full turns 中，不重复。
   - 超长用户消息也做 head+tail 或 artifact marker。

### 9.5 Summary 要覆盖的内容

summary prompt 必须要求模型输出：

- 用户最终目标。
- 当前任务进度。
- 已确认的设计决策与约束。
- 已修改或重点阅读的文件。
- 已运行的命令和关键结果。
- 失败路径与根因。
- 当前阻塞点。
- 下一步建议。
- 可检索 artifact 列表。

summary 不能把 artifact path 吞掉；路径是恢复细节的索引。

## 10. Task checkpoint

任务 checkpoint 是压缩安全性的核心。它必须来自真实状态源和 transcript 结构化提取，不能只靠 LLM summary。

### 10.1 checkpoint 内容

```text
TaskCheckpoint:
  active_goal: 当前目标 / 最新用户请求
  todos:
    - id
    - content
    - status: pending | in_progress | completed | blocked
    - active_form
  current_focus: 当前正在处理的点
  next_action: 下一步具体动作
  files_read: 最近读过且仍相关的文件
  files_touched: 本轮或本任务改过的文件
  commands_run: 关键命令及结果摘要
  background_tasks: running / exited / killed task_id 与用途
  pending_approvals: 未完成审批
  pending_questions: 未回答 Ask
  plan_mode: 是否处于 PlanMode、当前 plan 摘要与路径
  risks: 压缩后必须记得的风险 / 禁止事项
  artifacts: tool_results 与 compactions 索引
```

### 10.2 压缩前状态规则

- 有 pending approval / pending Ask 时，默认延期 compact，除非进入 emergency 且能够完整保存 pending 状态。
- 有未完成 tool_use / tool_result pair 时不能 compact。
- 有后台任务运行时，checkpoint 必须记录 task_id、启动命令、当前状态、如何读取输出。
- 有 PlanMode 活动计划时，必须记录 plan 内容或 plan 文件路径。
- TodoWrite 的状态必须原样保留，不能让压缩把 in_progress 变成未知。

## 11. 压缩归档与读取

### 11.1 文件布局

沿用架构 §6.1 的 session 目录：

```text
~/.hebbian/sessions/<sid>/
├── session.jsonl
├── tool_results/
│   ├── <call_id>.txt
│   └── <call_id>.meta.json
└── compactions/
    ├── compact-<window_id>.md
    ├── compact-<window_id>.jsonl
    └── compact-<window_id>.meta.json
```

建议：

- `.md`：给人和模型按需阅读的压缩前 transcript 渲染版。
- `.jsonl`：原始消息窗口，便于精确恢复和调试。
- `.meta.json`：窗口范围、before/after tokens、summary hash、checkpoint hash、artifact 列表。

### 11.2 Artifact index

每次 L1/L2/L3 产生 artifact 时，都要在同一 session 内形成可引用索引。索引可以先写入各自 `.meta.json` 并在 compact `.meta.json` 汇总；后续如果检索需求变强，再增加 session 级 `artifact_index.jsonl`。

最小结构：

```text
ArtifactIndexItem:
  id: artifact id，稳定且 session 内唯一
  kind: tool_result | compaction_archive | checkpoint | edit_payload
  path: session 相对路径
  source_entry_id: 来源 transcript entry id
  tool_call_id: 可选，工具输出对应 call_id
  summary: 一句话说明内容
  bytes: sanitized bytes
  estimated_tokens: 估算 token
  created_at: 写入时间，仅展示，不用于排序
  hash: 内容 hash
  redactions_count: secret 脱敏数量
```

引用规则：

- checkpoint、summary、marker 中引用 artifact id + path，不只写自然语言说明。
- artifact id 是恢复索引；path 是读取入口；hash 用于诊断 artifact 与 marker 是否匹配。
- artifact index 只描述可检索文件，不替代 `session.jsonl` 的历史顺序。

### 11.3 模型可见 marker

```text
[Older transcript was compacted]
Window: compact-0003
Reason: context pressure 78%
Archive:
- Summary and readable transcript: compactions/compact-0003.md
- Raw JSONL segment: compactions/compact-0003.jsonl
- Metadata: compactions/compact-0003.meta.json
Artifact index: compact-0003.meta.json#artifacts

Use Grep first, then Read with offset/limit if exact details are needed. Do not read the full archive unless necessary.
```

### 11.4 读取约束

- 模型需要旧细节时，优先 `Grep` artifact。
- 找到关键位置后，用 `Read offset/limit` 读取局部。
- 整文 Read 只在确有必要时使用，并应向用户说明原因。
- dispatcher 可以对 compaction artifact 的整文读取做默认 limit，避免刚压缩又整段读回。

## 12. L4：Token-budget reset

### 12.1 定义

Token-budget reset 指：不调用 LLM 总结旧历史，直接用当前 canonical context、已有 task checkpoint、archive reference、recent tail 构造新的模型可见上下文。

它不是常规压缩，而是 checkpoint-backed 的快速窗口重置。

### 12.2 适用条件

仅在以下条件同时满足时允许：

- 已有新鲜 task checkpoint。
- 最新用户消息已经进入 checkpoint 或 recent tail。
- 没有未完成 tool_use / tool_result。
- 没有 pending approval / pending Ask。
- 没有必须由 LLM 摘要才能保留的复杂语义窗口。

典型场景：

- 用户手动要求“清上下文，但继续当前任务状态”。
- summary compact 因 provider 临时失败而无法完成，但 checkpoint 足够恢复。
- provider `prompt_too_long`，继续原请求前必须快速降上下文。

### 12.3 禁用条件

- 没有 checkpoint。
- 当前正在工具调用循环中。
- 存在 pending HITL。
- 最新用户要求还没固化。
- 刚经历复杂设计讨论且没有结构化计划。

### 12.4 取舍

优点：便宜、快、失败面小。

代价：不生成语义摘要，旧讨论只能通过 artifact 检索恢复，语义连续性弱于 LLM summary compact。

因此默认策略仍是 L3 summary compact；L4 只做急救或用户明确选择的窗口重置。

## 13. 事件与存储契约

### 13.1 事件

第一版可复用已有事件语义：

- `ToolResultsCompacted`：工具结果被 L1/L2 替换为 artifact marker。
- `ContextCompactionStarted`：开始正式 compact。
- `ContextCompactionProgress`：归档、checkpoint、summary、replacement 等阶段进度。
- `ContextCompacted`：压缩完成，携带 archive path、before/after tokens、summary 摘要。
- `Warning` / `Error`：压缩失败但保留原文、artifact 写入失败、估算异常等。

如果新增字段，只能 additive；不能改变旧字段含义。

事件 payload 至少应能表达：

```text
ContextCompactionStarted:
  session_id
  request_id
  reason: auto | manual | reactive | emergency
  before_tokens
  threshold
  cancellable

ContextCompacted:
  session_id
  request_id
  archive_id
  archive_path
  start_entry_id
  end_entry_id
  before_tokens
  after_tokens
  summary_preview
  artifact_index_path
```

### 13.2 取消与 per-session 状态

自动 compact 与手动 compact 的取消语义不同：

- 自动 compact 复用当前 run 的 cancel flag；用户 Stop 当前 run 时，自动 compact 必须可取消。
- 手动 compact 是用户显式操作，可使用独立不可取消流程，除非后续 UI 明确提供取消按钮。
- 自动 compact 被取消时，不写 CompactBoundary / `ContextCompacted`，不替换 transcript；已经写好的临时 artifact 只能作为 orphan 清理，不能被 marker 引用。
- 自动 compact 失败时保留原 transcript，并通过 warning/error 清掉 surface 压缩中状态。

surface 状态必须按 session 隔离：

- 前端状态使用 `compactingSessionId` 或等价结构，而不是全局 bool。
- 收到 compaction 结果前必须确认 started session 仍是当前会话；不是当前会话时只更新持久状态，不污染当前 UI。
- CLI / hebweb / Desktop 都消费同一 core 事件，不自行推断压缩状态。
- 压缩进行中不应借用输入框 sending 态；它是独立的 context operation。

### 13.3 存储

- `session.jsonl` 仍是唯一历史账本。
- `tool_results/` 和 `compactions/` 是 artifact，不是第二历史源。
- artifact 写入必须走 storage 模块。
- session marker 追加必须走 `append_jsonl`。
- artifact 文件写入必须走 atomic write。
- compact 过程需要 session 级互斥，避免两个 run 同时压缩同一 transcript。

## 14. 实施顺序

### Phase 1：L0/L1 通用输出治理

- 实现 sanitize pipeline。
- 工具输出统一过 dispatcher 包装。
- 大输出落 `tool_results/`。
- 模型可见内容改为 head+tail + marker。
- 增加 secret redaction 测试、ANSI 测试、`\r` 进度条测试、long-line 测试。

### Phase 2：压力监控与 L2 microcompact

- 接入校准后的 token pressure。
- 实现候选选择和评分。
- 保护 recent turns / HITL / TodoWrite / PlanMode。
- 输出占位符，复用 artifact。
- 增加“50% 后压老 Bash/Read/Grep，最近 turns 不动”的回归测试。

### Phase 3：L3 checkpoint + summary compact

- 定义 `TaskCheckpoint`。
- 定义 artifact index item 与 compaction meta 结构。
- 压缩前 flush partial、snapshot todo/plan/background/pending 状态。
- 按完整 turn / tool loop 边界选择 compact window。
- 写 `compactions/*.md/jsonl/meta.json`。
- 调 LLM summary。
- 构造 replacement transcript。
- 在已 flush 段之后追加 `ContextCompacted` marker。

### Phase 4：L4 emergency reset

- provider `prompt_too_long` 进入 reactive compact。
- 有 checkpoint 时可 reset。
- 没 checkpoint 时 fail safe：保留原文，提示用户手动 compact / 新会话 / 降低输入。

### Phase 5：surface 展示与调试工具

- Desktop / hebweb 显示 tool artifact marker 与 compaction marker。
- heb CLI NDJSON 能看到 compaction 事件。
- 如现有 heb 命令不够调试 compaction，可新增只读 dump 命令，但必须走 core/storage 主路径。

## 15. 验收标准

### 15.1 行为验收

1. 大 Bash 输出只进入模型 head+tail，完整 sanitized 输出落盘。
2. 含 ANSI、`\r` 进度条、超长行、常见 secret 的输出被正确清洗。
3. 失败命令的 tail 被优先保留，exit code 可见。
4. 50% 压力后，老 Bash / Read / Grep 大结果被替换为占位符，token 使用下降。
5. 最近 `2-3` 个完整 turns 不被 microcompact。
6. Ask / TodoWrite / PlanMode / approval 决策不被压成不可恢复占位符。
7. 正式 compact 前生成 task checkpoint，compact 后当前目标、todo 状态、下一步仍可见。
8. compact 前旧 transcript 落 `compactions/`，可用 Grep / Read 找回原文。
9. summary 中包含 artifact id + path，不吞掉恢复索引。
10. 没有 checkpoint 时禁止 token-budget reset。
11. provider `prompt_too_long` 不导致 session 死掉或原文丢失。
12. Desktop / heb CLI / hebweb 看到的 compaction 行为一致。
13. reload 旧 session 后仍按 `session.jsonl` 物理顺序恢复，不因 `created_at` 漂移改变 compact boundary。
14. 自动 compact 被 Stop 取消时不写 boundary、不替换 transcript、不污染当前 session UI。

### 15.2 可执行验证

实现落地时必须至少覆盖以下测试或脚本：

```bash
# 单元层：清洗与预览
cargo test -p agent-core --lib context_sanitize
cargo test -p agent-core --lib tool_output_head_tail_preview
cargo test -p agent-core --lib secret_redaction_before_truncation

# 单元层：压力、候选与 checkpoint
cargo test -p agent-core --lib calibrated_context_pressure
cargo test -p agent-core --lib pressure_microcompact_preserves_recent_turns
cargo test -p agent-core --lib task_checkpoint_survives_compaction

# 单元层：顺序、取消与 reset 禁用条件
cargo test -p agent-core --lib compaction_boundary_after_flushed_segments
cargo test -p agent-core --lib reload_uses_session_jsonl_physical_order
cargo test -p agent-core --lib auto_compaction_cancel_does_not_write_boundary
cargo test -p agent-core --lib token_budget_reset_requires_checkpoint
```

现象级验证至少用 heb CLI 跑通三类场景：

1. 大 Bash 输出：触发 long output，确认 NDJSON 中模型可见内容是 head+tail，`tool_results/<call_id>.txt` 可 Grep / Read。
2. 压力触发 microcompact：构造接近 50% 的 transcript，确认老工具结果变占位符、最近 turns 保留。
3. 正式 compact / reactive compact：构造超过 75% 或模拟 provider `prompt_too_long`，确认 checkpoint、compactions artifact、ContextCompacted marker 都出现，且原文可检索。

## 16. 第一版必须避免的坑

- 不要只保留 head；错误通常在 tail。
- 不要把 secret redaction 放在截断之后。
- 不要压缩未配对 tool_use / tool_result。
- 不要把 `session.jsonl` 之外的 artifact 当作新的历史真源。
- 不要在 surface 做各自的模型上下文截断。
- 不要让 token-budget reset 在无 checkpoint 时运行。
- 不要把复杂 shape filter 做成第一版阻塞项。
- 不要在压缩失败时删除原上下文。
