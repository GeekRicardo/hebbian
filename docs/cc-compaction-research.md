# Claude Code 上下文管理机制逆向分析

> 来源：CC 2.1.152 `extension.js` 逆向 + Codex 源码对比
> 用途：为 Hebbian §4.7 压缩设计提供参照

---

## 一、CC 的完整上下文管理栈

### 0. 大输出如何处理（Bash/Read 读超大文件）

**CC 对 tool_result 没有显式截断**——工具输出全量发给模型，不在 JS 层截断。

**Bash 工具**：Shell 进程的 stdout 通过 Node.js `maxBuffer:1e6`（1MB）捕获，超过 1MB 的输出会抛 MaxBufferError。1MB 原始文本约等于 250k tokens——对于 1M context 的 Claude 模型完全可以容纳。

```javascript
// Bash 执行参数：1MB buffer 是 Node.js 进程层面的唯一限制
pV(command, args, {
    maxBuffer: 1e6,      // 1MB shell 输出上限
    timeout: ...,
    reject: false        // 失败不抛异常，让模型看到 stderr
})
```

**`verbose` 设置**：`verbose: false`（默认）只影响 **UI 显示**——在终端里给用户看截断摘要；**发给模型的 tool_result 内容不受此影响**，始终是完整输出。

**整体逻辑**：CC 信任 Claude 的大上下文窗口（1M token），不在工具层截断。上下文快满时由自动压缩（server-side）处理。

---

### 1. 压缩触发条件

**两种模式**：

**旧版（客户端 `compactionControl`，已废弃）**——基于上一次响应实际 token 计数：

```javascript
// 默认阈值 100k（Mi=1e5），含 cache 命中量
N = H.usage.input_tokens
  + (H.usage.cache_creation_input_tokens ?? 0)
  + (H.usage.cache_read_input_tokens ?? 0)
  + H.usage.output_tokens

if (N < contextTokenThreshold ?? 1e5) return false;
```

注意：cache 命中量也算进触发计数。读了 80k cache + 新增 30k = 110k → 触发。

这个 100k 是**废弃路径**的默认值，针对旧的 200k context 模型设计的。1M context 的新模型用的是 server-side 路径，不受此限制。

**新版（server-side `compact_20260112`，推荐）**：

```javascript
// CC 配置项，用户可配置自动压缩窗口（100k ~ 1M token）
autoCompactWindow: j.number().int().min(1e5).max(1e6).optional()
    .describe("Auto-compact window size")

// 客户端只传意图，服务端决定时机
toolRunner({ edits: [{ type: "compact_20260112" }] })
```

服务端清楚模型剩余容量，在合适时机执行压缩。`autoCompactWindow` 是用户偏好。

### 2. 压缩流程（`compactionControl`，客户端，已废弃）

```javascript
// Step 1：清掉末尾 assistant 消息里的 tool_use block（防止悬空 call）
if (messages[last].role === "assistant") {
    let blocks = messages[last].content.filter(b => b.type !== "tool_use");
    if (blocks.length === 0) messages.pop();    // 全是 tool_use → 整条删掉
    else messages[last].content = blocks;        // 有文字 → 只保留文字
}

// Step 2：把全量历史 + summaryPrompt 发给模型生成摘要
let summary = await client.messages.create({
    model: compactionModel,
    messages: [...allMessages, { role: "user", content: [{ type:"text", text: summaryPrompt }] }],
    headers: { "x-stainless-helper": "compaction" }   // 标记请求类型
});

// Step 3：用摘要完全替换全部历史
if (summary.content[0]?.type !== "text") throw Error("Expected text response");
params.messages = [{ role: "user", content: summary.content }];  // 只剩 1 条消息
```

**结论**：全量替换，不是选择性 shadow，所有 tool_call/tool_result 全部丢弃，只留摘要。

### 3. 默认 summaryPrompt（Gi 常量）

```
You have been working on the task described above but have not yet completed it.
Write a continuation summary that will allow you (or another instance of yourself)
to resume work efficiently in a future context window where the conversation history
will be replaced with this summary. Your summary should be structured, concise, and actionable.
```

### 4. 服务端压缩（`compact_20260112`，推荐）

```javascript
toolRunner({ edits: [{ type: "compact_20260112" }] })
// 废弃警告：
// "`compactionControl` is deprecated... Use server-side compaction instead"
```

客户端只声明意图，Anthropic 服务端执行压缩。服务端响应里有 `context_management` 字段（通过 `message_delta` 事件获取），表示压缩状态。

### 5. Cache 失效处理

CC **不处理**，直接接受一次全量 miss：
- 压缩前：messages 数组有 N 条，cache 基于这 N 条建立
- 压缩后：messages 只有 1 条摘要，cache key 完全不同
- 下一次请求：`cache_creation_input_tokens` 飙高一次（重建缓存），之后正常命中

### 6. 消息预处理

```javascript
// 构造时深拷贝，防止外部修改影响内部状态
params: { ...V, messages: structuredClone(V.messages) }
```

Tool result 注入：直接 `Promise.all` 执行所有 tool_use，结果完整追加为 `role:"user"` 消息，无截断。

---

## 二、Codex 上下文管理策略

Codex 采用**多层次渐进截断**，比 CC 精细得多。

### 1. 触发条件：两种模式

```rust
// AutoCompactTokenLimitScope::Total
//   计数全部活跃上下文 token
scope_tokens = active_context_tokens

// AutoCompactTokenLimitScope::BodyAfterPrefix  ← 关键设计
//   只计 prompt cache prefix 之后新增的 token
//   baseline = prefill_input_tokens（来自服务器观察或本地估计）
scope_tokens = active_context_tokens - baseline
```

**`BodyAfterPrefix` 模式的意义**：prompt cache 里的内容"免费"——已缓存的 token 不占 context budget。只有 cache prefix 之后的新内容才算进压缩阈值，极大延迟了压缩触发点。

压缩条件：
```rust
token_limit_reached = 
    auto_compact_scope_tokens >= auto_compact_scope_limit
    || full_context_window_limit_reached
```

### 2. 压缩阶段

| 阶段 | 时机 | 行为 |
|------|------|------|
| `PreTurn` | 采样前检查 | 压缩，下一 turn 重新注入初始上下文 |
| `MidTurn` | 采样中检查 | 压缩，在最后一条 user 消息前注入初始上下文 |
| `ModelDownshift` | 切到小上下文窗口模型 | 按新模型上下文窗口大小压缩 |

压缩原因（`CompactionReason`）：`UserRequested` / `ContextLimit` / `ModelDownshift`

### 3. 压缩策略：摘要 + 最近 N 条用户消息

```rust
const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;

fn build_compacted_history(
    initial_context: Vec<ResponseItem>,
    user_messages: &[String],   // 过滤掉历史摘要消息后的用户消息
    summary_text: &str,         // LLM 生成的摘要
) -> Vec<ResponseItem>
// 从最新用户消息逆序选择，直到填满 20k token 预算
// 结果：摘要 + 最近 N 条用户消息（不含 tool_call/tool_result）
```

比 CC 多了一步：保留最近 N 条**用户消息**（20k token 限额），让模型看到对话意图脉络，而不只是摘要。

### 4. Tool Result 截断（CC 没有，Codex 有）

| 类型 | 截断限制 | 处理 |
|------|---------|------|
| MCP tool result | 1MB 字节 | 超出部分截断，转为单条 text，丢弃 structured_content |
| Exec 命令输出 | `DEFAULT_MAX_OUTPUT_TOKENS = 10,000` | token 预算控制 |
| Assistant 输出 | `truncate_assistant_output_text_to_token_budget` | 按 token 预算平均分配给各 assistant 消息 |

### 5. Prompt Cache 管理

```rust
pub struct AutoCompactWindow {
    prefill_input_tokens: Option<AutoCompactWindowPrefill>,
}

enum AutoCompactWindowPrefill {
    ServerObserved(i64),  // 服务端返回的真实值（优先）
    Estimated(i64),       // 本地估计（服务端值到来前临时用）
}

// 压缩时：start_next() 前进到下一个窗口，清除 prefill 基线
// cache key 默认用 thread_id，保证同一对话 prefix 稳定命中
```

### 6. 分区 token 预算（注入内容精细控制）

```rust
REALTIME_TURN_TOKEN_BUDGET:       300    // 实时上下文
CURRENT_THREAD_SECTION_BUDGET:  1_200   // 当前线程
RECENT_WORK_SECTION_BUDGET:     2_200   // 近期工作
WORKSPACE_SECTION_BUDGET:       1_600   // workspace 信息
COMPACT_USER_MESSAGE_MAX_TOKENS: 20_000  // 压缩后保留的用户消息
DEFAULT_MAX_OUTPUT_TOKENS:       10_000  // exec 输出默认
MCP_TOOL_CALL_MAX_BYTES:          1MB   // MCP 工具结果
```

每种注入内容都有独立预算，防止单类内容打爆 context。

---

## 三、CC vs Codex 对比

| 维度 | Claude Code | Codex |
|------|------------|-------|
| 触发计算 | input+cache_creation+cache_read+output 总和 | 两种模式：Total 或 BodyAfterPrefix（只算 cache 之后的新增） |
| 默认阈值 | 100,000 tokens（固定） | `model_auto_compact_token_limit`（按模型配置） |
| 压缩策略 | LLM 摘要 → 只剩 1 条消息 | LLM 摘要 + 最近 N 条用户消息（20k token） |
| Tool result 大小限制 | **无**（全量注入） | MCP 1MB、Exec 10k token |
| Cache 失效处理 | 直接接受全量 miss | BodyAfterPrefix 模式尽量只压 cache 之外的部分，减少 miss 范围 |
| 分区 token 预算 | 无 | 有（多区域独立限额） |
| 压缩阶段 | 每次 turn 开始前检查 | PreTurn / MidTurn / ModelDownshift 三阶段 |
| 持久化 | 无（内存操作，重启丢失） | 有（rollout 存储，按字节上限落盘） |

---

## 四、对 Hebbian §4.7 的设计启示

### 最值得借鉴的两点

**1. BodyAfterPrefix 模式（Codex 独创）**

不把 prompt cache 里的内容算进压缩触发预算。cache prefix 是"稳定前缀"，已被服务端缓存，相当于免费。只计 prefix 之后的新增量触发压缩，大幅延迟了压缩点。

Hebbian 目前用估算 token 计数，没有区分 cache 命中部分。可以在 budget 计算里引入 `cached_token_baseline`，实现同等效果。

**2. 摘要 + 最近 N 用户消息（Codex）**

纯摘要（CC 和 Hebbian 当前 L3）会丢失"最近在说什么"的感觉。Codex 在摘要后还追加最近 20k token 的用户消息，让模型既有历史全貌，又知道最近几轮在聊什么。

Hebbian L3 压缩后可以在 `CompactBoundary` 后保留最近 K 轮的 user messages（不含 tool 结果），实现类似效果。

### 当前 Hebbian 与架构对齐情况

| 机制 | 架构设计 | 当前实现 | 差距 |
|------|---------|---------|------|
| L0 微压缩 | 老 tool result shadow | `microcompact` 按大小 shadow | 已实现，但未按轮次位置 |
| L2 结构化裁剪 | 最近 N 轮 + 老 tool 选择性 shadow | 直接删除老消息 | 差"选择性 shadow"，见下节 |
| L3 LLM 摘要 | 手动 /compact | `compact_session` 已实现 | 已实现，无自动触发 |
| 触发阈值 | 70% context_window | **已修复**：`context_window * 0.7` | 已修复 |
| Cache-aware 阈值 | 未设计 | 无 | 可借鉴 Codex BodyAfterPrefix |

### L2 "选择性 shadow"的正确理解

架构 §4.1.3 说的"最近 N 轮之前的 tool_call 选择性 shadow"，实际上是 **L0 微压缩的位置化延伸**：

- L0 当前：按工具输出大小决定是否 shadow（大的 shadow，小的保留）
- L2 应该：按轮次位置决定——最近 N 轮的 tool 结果全量保留，N 轮之前的全部 shadow（无论大小）

这样做的效果：
- 不删除消息（保持 `user/assistant/tool_result` 结构完整）
- 旧 tool_result 内容替换成 `[此结果已被压缩，见 tool_results/<id>.txt]`
- 模型还能看到"当时调用了什么工具、调用序列是怎样的"，只是不能直接读输出

**Cache 影响**：替换旧 tool_result 内容 → 那条消息的 cache key 变化 → 之后所有消息的 cache prefix 都失效。这和全量删除对 cache 的影响是一样的——无法避免 cache miss。CC 和 Codex 都直接接受这次代价。
