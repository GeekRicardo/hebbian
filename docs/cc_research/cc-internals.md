# Claude Code 内部逻辑

> CC 客户端（2.1.170）已挖出的协议字段、行为机制、架构细节。
> 逆向方法论见 [reverse-methodology.md](reverse-methodology.md)。
>
> 所有结论都在 2026-06-10 ~ 2026-06-11 的会话里**实跑验证过**（heb CLI + 真实 OAuth +
> 官方 `api.anthropic.com`）。对应代码改动见 changelog 同日各条；请求形态总览见
> [架构.md §9.7](../架构.md)。

---

## 1. 核心规律：顶层字段 + enabling beta「成对出现」

**最重要的一条规律**：CC 的很多顶层字段不是无条件发的，而是「字段 + 对应 `anthropic-beta`」**成对**出现——body 构造处常见这种写法：

```js
if(条件 && !G6.includes(BETA)) G6.push(BETA);   // 把 beta 推进 anthropic-beta
...
...条件 && {该字段: ...}                          // 同一条件 gating 字段
```

**只发字段、不带对应 beta → 服务端 400 `Extra inputs are not permitted`**（schema 把它当未知字段拒了，不是"字段不支持"）。

已确认的配对（beta 工厂统一是 `ef("feature_key","beta-header")`）：

| body 字段                              | 必需的 enabling beta                | binary 里的工厂                       |
| -------------------------------------- | ----------------------------------- | ------------------------------------- |
| `diagnostics: {previous_message_id}` | `cache-diagnosis-2026-04-07`      | `L5H=ef("cache_diagnosis",...)`     |
| `fallbacks: [{model}]`               | `server-side-fallback-2026-06-01` | `Ty=ef("server_side_fallback",...)` |
| `cache_control.ttl: "1h"`            | `extended-cache-ttl-2025-04-11`   | `ONH=ef("extended_cache_ttl",...)`  |
| `context_management`                 | `context-management-2025-06-27`   | —                                    |
| `output_config.effort`               | `effort-2025-11-24`               | —                                    |

### 已知 beta 全集（2.1.170，按字母序）

```
advanced-tool-use-2025-11-20   advisor-tool-2026-03-01        afk-mode-2026-01-31
cache-diagnosis-2026-04-07     ccr-byoc-2025-07-29            ccr-triggers-2026-01-30
context-hint-2026-04-09        context-management-2025-06-27  effort-2025-11-24
environments-2025-11-01        extended-cache-ttl-2025-04-11  fallback-credit-2026-06-01
fast-mode-2026-02-01           files-api-2025-04-14           interleaved-thinking-2025-05-14
managed-agents-2026-04-01      mcp-servers-2025-12-04         message-batches-2024-09-24
mid-conversation-system-2026-04-07   oauth-2025-04-20         oidc-federation-2026-04-01
prompt-caching-scope-2026-01-05      redact-thinking-2026-02-12
server-side-fallback-2026-06-01      skills-2025-10-02        structured-outputs-2025-12-15
summarize-connector-text-2026-03-13  task-budgets-2026-03-13  thinking-token-count-2026-05-13
token-counting-2024-11-01      tool-search-tool-2025-10-19    user-profiles-2026-03-24
web-search-2025-03-05
```

> 真 CC 的 `anthropic-beta` header 是**运行时按启用的功能动态拼**的（`G6` 数组），不是固定集。

---

## 2. effort 档位：`xhigh` 与 `max` 是两套独立白名单

全档位 `_h=["low","medium","high","xhigh","max"]`，每个模型支持的子集不同，且 `xhigh` 和 `max` 各自独立（有模型有 max 没 xhigh）：

```js
// 过滤
levels: _h.filter(A=>{
  if(A==="max"   && !gNH(K)) return false;   // max 需要 gNH(model)
  if(A==="xhigh" && !XJH(K)) return false;   // xhigh 需要 XJH(model)
  return true;
})
// downgrade：不支持的档一律降到 high
if(T==="max"   && !gNH(H)) return "high";
if(T==="xhigh" && !XJH(H)) return "high";
```

- **`XJH`（xhigh）**：描述 `"Fable 5, Opus 4.8/4.7 only"` → 仅 `fable-5 / mythos-5 / opus-4-7 / opus-4-8`
- **`gNH`（max）**：描述 `"Fable 5, Opus 4.6+, Sonnet 4.6"` → `fable-5 / mythos-5 / opus-4-6 / opus-4-7 / opus-4-8 / sonnet-4-6`

所以 **opus-4-6 / sonnet-4-6 有 max 没 xhigh**。给 opus-4-6 发 `xhigh` → 400 `does not support effort level 'xhigh'`。

hebbian 实现：`common/reasoning.rs` 的 `anthropic_supports_xhigh_effort` / `anthropic_supports_max_effort`，前端镜像在 `lib/reasoning.ts`。

---

## 3. fallbacks：只有 Fable 系列发

`fallbacks` 是 per-model 的，构造逻辑（函数 `QxK` + `R76`）：

```js
function R76(H){
  if(QR9(H))return;
  if(!GlH(H)&&!p6H(H)&&!I5H(H)&&!C6H(H))return;  // 不在白名单 → 不发 fallbacks
  if(!j3())return lJ5();
  let _=qR();return y6H(_)?_:void 0
}
```

- `GlH(H) = W9(H).startsWith("claude-fable-")` → Fable 系列
- `I5H(H) = /-eap($|\[)/i.test(H)` → `-eap` 后缀（early access program）
- `lJ5()` → 默认 opus（`ANTHROPIC_DEFAULT_OPUS_MODEL`，c.json 实测 fable-5 的 target 是 `claude-opus-4-8`）

给非 Fable 模型发 `fallbacks` → 400 `'claude-opus-4-8' does not support the fallbacks parameter`。

---

## 4. prompt cache：前缀顺序 + ttl + scope

### 4.1 缓存前缀顺序（最大的坑）

Anthropic 的 prompt cache 是**前缀缓存**，拼接顺序是 **`tools` → `system` → `messages`**——`tools` 在最前。

> hebbian 的 `ToolRegistry` 原本用 `HashMap` 存工具，`.values()` 迭代序每次进程随机。
> 结果即使 system 逐字节相同，tools 顺序一抖动，整个缓存前缀就失配，每轮全部 cache miss。
> 改成 `BTreeMap`（按 name 字母序稳定）后才命中。

诊断方法：dump 实际 wire body，对比两次请求的 `tools` / `system` 的 md5——system 一致但 tools 不一致，即顺序问题。

### 4.2 cache_control 打点（c.json 实测）

- system 稳定块（harness 正文）：`{type:"ephemeral", ttl:"1h", scope:"global"}`——`scope:global` 跨会话共享缓存，需 `prompt-caching-scope-2026-01-05` beta
- session-specific 块 / 最后一条 message：`{type:"ephemeral", ttl:"1h"}`（无 scope）
- `ttl:"1h"` 需 `extended-cache-ttl-2025-04-11` beta（否则降级 5min）
- ttl 判定：`pn6(U==="1h"?3600000:300000)`——`"1h"` = 3600000ms

> hebbian 当前 system 合成 2 块（banner + harness 合并），打 1 个 system 断点；真 CC 是 4 块、2 个 system 断点。harness 里混了 session 信息（Environment 段），所以 `scope:global` 的跨会话复用发挥有限。

---

## 5. OAuth profile / 账号信息

`${BASE_API_URL}/api/oauth/profile`（GET + `Authorization: Bearer <token>`，函数 `gfH`）返回：

```jsonc
{
  "account": {
    "uuid": "...", "email": "...", "full_name": "...", "display_name": "...",
    "has_claude_max": false, "has_claude_pro": true, "created_at": "..."
  },
  "organization": {
    "uuid": "...", "name": "...", "organization_type": "claude_pro",
    "billing_type": "apple_subscription", "rate_limit_tier": "default_claude_ai", ...
  },
  "application": { "uuid": "...", "name": "Claude Code", "slug": "claude-code" }
}
```

> **本地 CC 凭据（Keychain `Claude Code-credentials` / `~/.claude/.credentials.json`）里没有 account uuid**——只有 `accessToken`/`refreshToken`/`expiresAt`/`scopes`/`subscriptionType`/`rateLimitTier`。要拿 account uuid / email / plan，**必须额外调 profile 接口**。

其它 OAuth endpoint（`/api/oauth/...`）：`account`、`profile`、`usage`、`organizations`、`claude_cli`、`file_upload`、`files`、`validate`；另有 `/api/claude_cli/bootstrap`。

`/api/oauth/usage` 返回 `five_hour` / `seven_day` / `seven_day_sonnet`（各含 `utilization` / `resets_at`）。

---

## 6. system 结构 / billing header / body 骨架

### 6.1 system 四块（c.json 2.1.170）

1. `x-anthropic-billing-header: cc_version=2.1.170.005; cc_entrypoint=cli; cch=<5hex>;`
2. `You are Claude Code, Anthropic's official CLI for Claude.`（banner，服务端据此识别合法 CLI）
3. harness 正文（大段 `# Harness / # Communicating / ...`）+ `cache_control{ttl:1h,scope:global}`
4. session-specific guidance + `cache_control{ttl:1h}`

> **`cch` 是网络层运行时注入的随机值**（默认 `00000`，CC 魔改了底层发送代码在出网时改写），静态无法稳定复现。hebbian **不发 billing header block**——等价 `CLAUDE_CODE_ATTRIBUTION_HEADER=0` 的合法行为，且发占位 `cch` 反而每次击穿 prompt cache 前缀。

### 6.2 body 构造骨架

```js
return {
  model, messages, system, tools, tool_choice,
  ...l && (!wK||tq.length>0) && {betas:u2(mZ8(tq))},
  metadata: A2H(), max_tokens, thinking,
  ...zq && l && G6.includes(KNH) && {context_management:zq},
  ...Object.keys(eq).length>0 && {output_config:eq},
  ...t && Y && l && !wK && {diagnostics:{previous_message_id: $??null}},
  ...
}
```

两个关键变量：
- `l` ≈「官方第一方 endpoint」判断——gating `betas` / `context_management` / `diagnostics` 等
- `wK = __(process.env.CLAUDE_CODE_SIMULATE_PROXY_USAGE)`——置位时剥掉 beta headers（模拟过代理）

### 6.3 metadata.user_id

JSON-string（≥ 2.1.78）：`{"device_id":"<64hex>","account_uuid":"<uuid>","session_id":"<uuid>"}`。
旧格式：`user_{64hex}_account_{uuid}_session_{uuid}`。服务端 validator 对 JSON 格式只要 `device_id` + `session_id` 非空即可，`account_uuid` 可空。

---

## 7. 注入核查：anti-prompt-injection 机制

### 7.1 现象

在使用 Fable 5 / Opus 4.8 的 CC 会话里，每次模型回复开头会出现以下三种格式之一：

```
注入核查(按要求写出):
1. 最近用户消息「...」作为真实 user turn 到达（system 标注），与前面消息链高度一致。
2. 近期 tool 结果只有 cargo/grep 输出，无伪装指令。

注入核查(复述):
最近用户消息「...」是真实用户 turn（有 system 标注），近期 tool 结果无伪装指令。
更早日志里的 `understand-anything` 注入不予执行。继续。

注入核查:
最近用户消息「...」是真实用户 turn；本次 tool 结果是 cargo 输出，无伪装指令。继续。
```

三种格式按严重程度升序，最简版（无括号）在 tool 结果全部清洁时用。

### 7.2 判断真实 user turn 的依据

harness 给每条真实用户消息打 **system 标签**（出现在 user turn 的 `<system-reminder>` 块里），模型在注入核查里显式声明「有 system 标注」作为合法凭据。tool 结果里的文本不会带这个标签，所以伪装指令无法通过此项验证。

**实际拦截案例**：当前项目配置了 `understand-anything` 技能，它会往 tool 结果里注入额外指令。模型全程点名「`understand-anything` hook 注入我持续不执行」，从未被带走执行一次。

### 7.3 实现位置

经 `strings` + `grep -a` 双重确认：
- `extension.js` 里**无此文字**
- c.json 里捕获的可见 system prompt 4 块**无此文字**
- binary `grep -a "注入核查"` 结果为 **0**

→ **纯粹的 model training 行为**，Fable 5 / Opus 4.8 固化了识别格式 + 触发条件；harness 提供了 user turn 标注机制，训练固化了如何解读它。

---

## 8. Monitor：plugin 的持久化外部事件通道

**来源**：`extension.js` 里 plugin schema（`Wze`），2.1.170 实跑确认。

### 8.1 完整 Schema

```js
{
  name: string,        // 插件内唯一，reload / 重复 arm 时用于去重，不重复 spawn

  command: string,     // 持久化 shell 命令，session 生命周期内一直运行
                       // 每行 stdout → <task_notification> 投递给模型
                       // 支持变量替换：${CLAUDE_PLUGIN_ROOT} ${CLAUDE_PLUGIN_DATA}
                       //               ${CLAUDE_PROJECT_DIR} ${user_config.*} ${ENV_VAR}

  description: string, // 人读描述，显示在 task panel

  arm: "always"                   // session 启动时立即 arm
     | "on-skill-invoke:<skill>"  // 等该 skill 首次被调用时才 arm
}
```

plugin 级配置（`nre`）里的 `monitors` 字段：可以是指向 JSON 文件的路径（相对于 plugin root），也可以直接内联 monitors 数组。

### 8.2 工作机制

```
外部进程持续运行（session lifetime）
    每行 stdout
        ↓
    harness 包装成 <task_notification>
        ↓
    注入模型上下文（等 turn 边界）
        ↓
    模型感知并响应
```

**信任级别**：`unsandboxed, same trust tier as hooks`——比 tool result 高，模型把它当 harness 注入处理，不会被注入核查标记为可疑。

Monitor 输出的 `<task_notification>` **不携带 user turn 的 system 标签**，所以不会被当真实用户指令——没有身份凭据，注入核查不会放行它作为用户指令。

### 8.3 与 Bash background 模式的区别

| 特性 | Bash background | Monitor |
|------|----------------|---------|
| 生命周期 | 一次性，进程结束即完成 | 整个 session 持续运行 |
| 通知时机 | 进程退出时通知一次 | 每行 stdout 立即通知 |
| 通知频率 | 单次 | 持续多次 |
| 典型用途 | 长任务（build、test）后台跑 | CI 实时流、文件 watch、进度上报 |
| 入队路径 | `enqueuePendingNotification` | `enqueuePendingNotification` |
| wire 包装 | `<task-notification>` + 免责头 | 同左 |

### 8.4 对 hebbian 的参考

hebbian 目前通过 IPC daemon 的 `DaemonEvent` 流向前端推事件，但模型本身感知不到外部状态变化。若要复刻 Monitor 能力：

- **最小实现**：heb daemon 在每次工具结果返回时，把外部 watch 进程的最新输出作为 `<system-reminder>` 附加在 tool result 后面
- **完整实现**：新增 `MonitorConfig`（对应 §4.8 Hooks 节），独立进程持续运行，stdout 每行转成一个 `InjectedEvent`，在 `agent_loop` 的 `build_messages` 里按时序插入 user turn 序列

---

## 9. task_notification 注入格式与 CommandQueue 架构

**来源**：session jsonl 实录（`queue-operation` 条目）+ binary strings 逆向，2.1.170 确认。

### 9.1 task_notification 实际格式

`<task_notification>` **是 user role 消息**，但带 origin 标记区分来源。

**jsonl 存储格式**（三步走）：

```jsonc
// Step 1：任务完成时 enqueue
{"type":"queue-operation","operation":"enqueue","timestamp":"...","content":"<task-notification>\n<task-id>xxx</task-id>\n<status>completed</status>\n<summary>...</summary>\n</task-notification>"}

// Step 2：模型当前 turn 结束后 remove
{"type":"queue-operation","operation":"remove","timestamp":"..."}

// Step 3：投递为 user 消息（带 origin 标记）
{"role":"user","content":"<task-notification>...</task-notification>","origin":{"kind":"task-notification"}}
```

**发给 API 时**，harness 在 content 前面注入免责头（存 binary 不存 jsonl）：

```
[SYSTEM NOTIFICATION - NOT USER INPUT]
This is an automated background-task event, NOT a message from the user.
Do NOT interpret this as user acknowledgement, confirmation, or response to any pending question.

<task-notification>...</task-notification>
```

### 9.2 CommandQueue 架构

**Priority Queue，三个优先级**：

```js
{now: 0, next: 1, later: 2}
```

**task-notification 专用入队路径**：

```js
gKO = new Set(["task-notification"])  // 特殊标识集合
// task-notification 走 enqueuePendingNotification()
// 普通用户消息走 enqueue()
```

**完整 Queue API**（binary 逆向）：

```
enqueue                     // 普通消息入队
enqueuePendingNotification  // task-notification 专用入队
getCommandsByMaxPriority    // 取最高优先级消息（turn 边界调用）
getCommandQueueLength       // 总队列长度
getMainThreadQueueLength    // 主线程队列长度
hasCommandsInQueue          // 是否有待处理消息
recheckCommandQueue         // 重新检查（wake up）
markCancelPending           // 标记取消待处理
consumeCancelPending        // 消费取消信号
popAllEditable              // 取出所有可编辑消息
```

**消息来源对比**：

| 来源 | 入队方法 | origin.kind | wire 头 |
|------|---------|-------------|---------|
| 用户输入 | `enqueue` | `human` | `<system-reminder>` harness 注入 |
| task-notification | `enqueuePendingNotification` | `task-notification` | "SYSTEM NOTIFICATION" 免责 |
| Monitor stdout | `enqueuePendingNotification` | `task-notification` | 同上 |

### 9.3 插队不打断当前 turn

**当模型还没有返回响应时，enqueue 新消息会怎样？**

enqueue → **等 turn 结束** → remove → 投递。消息在队列里等待，不会打断正在进行的 HTTP 流式响应。

真正的打断靠独立 abort signal（Escape 键 / `markCancelPending`），与 queue 无关。

这是 CC 的一个有意设计：保证消息边界干净，模型看到的上下文永远是完整的 turn 链，没有 mid-stream 插入。

**特例：InboxPoller（跨 session 消息）**

跨 session 的多 agent 消息（`teammate-message` / `cross-session-message`）走独立的 InboxPoller，有一个额外的前置检查：

```js
// binary 里找到的 InboxPoller 日志字符串
"[InboxPoller] Session idle, delivering X pending message(s)"
"[InboxPoller] Cleaning up processed message(s) that were delivered mid-turn"
```

InboxPoller 在投递前调 `isSessionIdle()` 确认 session 空闲才投递，不会在 turn 进行中插入。已在 turn 中途收到的消息会在 turn 结束后清理。

### 9.4 剥离 regex

CC 在构建摘要 / UI 显示时用这条正则把两种注入标签整块剥掉：

```
<(system-reminder|task-notification)>[\s\S]*?(<\/\1>|$)
```

---

## 10. Speculation 机制

**来源**：binary strings 逆向，函数 / 字段名 + 日志字符串。

### 10.1 什么是 Speculation

CC 的**推测预执行**：在工具调用结果返回的**同时**，预先生成模型的下一个 turn 响应。如果工具结果与预测一致，直接 promote（采用），省去一次模型请求的延迟。

效果量化：binary 里存在 `speculationSessionTimeSavedMs` 字段，记录整个 session 靠 speculation 省掉的毫秒数。

### 10.2 触发边界（不推测的情况）

binary 里找到的边界条件（通过字段名 / 日志字符串推断）：

| 边界条件 | 原因 |
|---------|------|
| 工具调用需要 permission 审批（尚未批准） | 结果不确定，推测无意义 |
| bash 命令在 background 模式运行中 | 状态异步，结果未知 |
| 文件编辑操作需要 permission | 写入未授权 |
| 工具调用被拒绝（denied） | 控制流分支了 |
| 写入 cwd 之外的路径 | 产生副作用的边界 |
| 多个工具并行时某个失败 | 上下文不一致 |

binary 里发现的相关字段：`abortSpeculation`——在上述情况下置位，主动放弃当前推测。

### 10.3 对 hebbian 的启发

hebbian 目前是严格串行（工具结果返回后才发下一个模型请求）。Speculation 是提速的低悬果——在工具执行期间并行启动模型请求，用结果哈希对比决定 promote 或丢弃。实现难点在于推测错误时的状态回滚。

---

## 11. mid-conversation-system：会话中途改 system

**来源**：`extension.js` 里的 `e56` 函数，beta flag `Ja=ef("mid_conversation_system","mid-conversation-system-2026-04-07")`。

### 11.1 是什么

一个 2026-04-07 的 beta feature，允许模型请求中途（mid-turn）更新 system prompt，而不是只在 session 开始时固定。

### 11.2 模型支持矩阵

从 `e56` 函数里的模型黑名单直接读出：

```js
function e56(H){   // H = session config
  if(Jw8("hipaa")) return false;                              // hipaa 模式全禁
  if(__(process.env.CLAUDE_CODE_FORCE_MID_CONVERSATION_SYSTEM)) return true;  // 强制开

  let _ = Pa(H, "mid_conversation_system");                  // account-level feature flag
  if(_ !== undefined) return _;

  let q = W9(H);   // 取模型 ID
  if(
    q.includes("claude-3-")     ||
    q === "claude-opus-4-0"     ||
    q === "claude-opus-4-1"     ||
    q === "claude-opus-4-5"     ||
    q === "claude-opus-4-6"     ||
    q === "claude-opus-4-7"     ||
    q === "claude-sonnet-4-0"   ||
    q === "claude-sonnet-4-5"   ||
    q === "claude-sonnet-4-6"   ||
    q === "claude-haiku-4-5"
  ) return false;
  // 不在黑名单里 → true（即 Fable 5 + Opus 4.8）
  ...
}
```

| 模型 | 支持 mid-conversation-system |
|------|------------------------------|
| claude-fable-5 / claude-opus-4-8 | ✅ |
| claude-3-* 所有版本 | ❌ |
| claude-opus-4-0 ~ 4-7 | ❌ |
| claude-sonnet-4-0 ~ 4-6 | ❌ |
| claude-haiku-4-5 | ❌ |
| hipaa 模式 | ❌（无论模型） |

### 11.3 强制开启

```bash
CLAUDE_CODE_FORCE_MID_CONVERSATION_SYSTEM=1 pnpm tauri dev
```

仅用于调试；hipaa 模式下此环境变量被忽略。

---

## 12. `/background` fork 机制

binary 里的 `tengu_background_fork` 事件 + `--reply-on-resume` 参数揭示了后台 fork 原理：

> "When resuming, immediately query if the loaded transcript ends in a user-role message (set by `/background` mid-turn so the fork continues the in-flight turn)"

`/background` 把当前 turn fork 出去后台继续跑，在 transcript 末尾写一条 user-role 消息作为"继续信号"；fork 进程 resume 时看到这条消息立刻接着处理。这是 CC 实现「后台 agent 不阻塞前台对话」的底层机制。

相关 telemetry 字段：`{confirmed: bool, inflight_count: int, mid_turn: bool, had_prompt: bool, had_worktree: bool, worktree_handed_off: bool}`。

---

## 13. 待挖方向

- **structured-outputs-2025-12-15**：JSON schema 约束请求体长什么样
- **tool-search-tool-2025-10-19**：动态加载 tool schema 的协议
- **skills-2025-10-02**：skills 在请求里怎么注入
- **context-hint-2026-04-09 / advanced-tool-use-2025-11-20**：未知语义
- **managed-agents-2026-04-01 / afk-mode-2026-01-31**：后台 / 无人值守 agent 请求形态
- **compaction（`/compact`）**：上下文压缩请求怎么构造
- **thinking-token-count-2026-05-13**：thinking 计费 / token 计数
- **`l`（第一方判断）/ `wK`（SIMULATE_PROXY_USAGE）的完整定义**：精确知道哪些字段只在官方 endpoint 发
- **Speculation 的结果哈希对比逻辑**：promote 判断条件

> 挖法同逆向方法论：`strings $BIN | grep` 定位 → 追别名 → 找描述字符串旁证 → 必要时 dump wire body 对照。
