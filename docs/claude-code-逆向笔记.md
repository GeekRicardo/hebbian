# Claude Code 逆向笔记

> 目的：hebbian 的「CC 兼容模式」要让 OAuth 请求在协议层尽量贴近真实 Claude Code 客户端
> （被服务端 / 中转网关认作合法 CC 流量）。这份笔记记录**怎么逆向 CC 客户端**、以及
> 已经挖出来的 ground truth，供后续继续挖。
>
> 所有结论都在 2026-06-10 这次会话里**实跑验证过**（heb CLI + 真实 OAuth + 官方
> `api.anthropic.com`）。对应代码改动见 changelog 同日各条；请求形态总览见 [架构.md §9.7](架构.md)。

---

## 1. 怎么读 CC 客户端代码

### 1.1 binary 在哪

VS Code 扩展自带一个打包好的 native binary：

```
~/.vscode/extensions/anthropic.claude-code-<版本>-darwin-arm64/resources/native-binary/claude
```

本次用的是 `anthropic.claude-code-2.1.170-darwin-arm64`。它是 **bun 打包的单文件可执行**，里面塞了一整坨 minified JS（`build/release/tmp_modules/...`）。

### 1.2 提取方法：strings + grep

binary 是二进制，但 JS 源码以明文字符串存在里面，`strings` 就能掏出来：

```bash
BIN=~/.vscode/extensions/anthropic.claude-code-2.1.170-darwin-arm64/resources/native-binary/claude

# 提全部 beta header（形如 xxx-2026-04-07）
strings "$BIN" | grep -oE "[a-z][a-z-]+-20[0-9]{2}-[0-9]{2}-[0-9]{2}" | sort -u

# 看某字段的构造上下文（前 N 个字符）
strings "$BIN" | grep -oE ".{400}diagnostics:\{previous_message_id" | head -1

# 找某函数定义
strings "$BIN" | grep -oE "function XJH\(H\)\{[^}]{0,300}" | head -1
```

输出动辄上 MB，配合 `head` / `grep -oE ".{N}pattern.{M}"` 截窗口看。

### 1.3 读 minified JS 的三板斧

minified 代码变量是单字母 / 短哈希（`L5H`、`ef`、`Ty`、`gNH`），而且**复用严重**（`ef` 在不同作用域是不同东西）。三个技巧：

1. **追别名链到底**。`L5H=ef` → 再 `grep "L5H=ef\("` → 拿到 `L5H=ef("cache_diagnosis","cache-diagnosis-2026-04-07")`。别名要追到那个真正带字面量的工厂调用。
2. **优先信描述字符串**。给用户看的 UI 文案往往直接说明语义，比读 minified 逻辑快得多。例：effort 档位 `xhigh` 的说明字符串就是 `"Fable 5, Opus 4.8/4.7 only"`——一句话顶半小时读 `XJH` 黑名单。
3. **从错误处理反推服务端规则**。CC 里有巨大的 error-classify 函数（`GC6` / `pW3`），逐条 `H.message.includes("...")` 匹配 400 文案。这些字符串就是服务端的校验规则清单（"diagnostics.previous_message_id"、"does not support the fallbacks parameter"、"Extra inputs are not permitted" 等）。

---

## 2. 核心机制：顶层字段 ↔ enabling beta「成对出现」

**最重要的一条规律**：CC 的很多顶层字段不是无条件发的，而是「字段 + 对应 `anthropic-beta`」**成对**出现——body 构造处常见这种写法：

```js
if(条件 && !G6.includes(BETA)) G6.push(BETA);   // 把 beta 推进 anthropic-beta
...
...条件 && {该字段: ...}                          // 同一条件 gating 字段
```

**只发字段、不带对应 beta → 服务端 400 `Extra inputs are not permitted`**（注意：不是"字段不支持"，是 schema 把它当未知字段拒了）。

已确认的配对（beta 工厂统一是 `ef("feature_key","beta-header")`）：

| body 字段 | 必需的 enabling beta | binary 里的工厂 |
|---|---|---|
| `diagnostics: {previous_message_id}` | `cache-diagnosis-2026-04-07` | `L5H=ef("cache_diagnosis",...)` |
| `fallbacks: [{model}]` | `server-side-fallback-2026-06-01` | `Ty=ef("server_side_fallback",...)` |
| `cache_control.ttl: "1h"` | `extended-cache-ttl-2025-04-11` | `ONH=ef("extended_cache_ttl",...)` |
| `context_management` | `context-management-2025-06-27` | — |
| `output_config.effort` | `effort-2025-11-24` | — |

> ttl 的判定：`pn6(U==="1h"?3600000:300000)`——`"1h"` = 3600000ms，需 `extended-cache-ttl`；否则默认 5min。

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
> hebbian 取了其中稳定必需的核心子集（见 `providers/mod.rs apply_auth` 的 OAuth 分支）。

---

## 3. effort 量程：`xhigh` 与 `max` 是两套独立的 per-model 白名单

全档位 `_h=["low","medium","high","xhigh","max"]`，但每个模型支持的子集不同，且 **`xhigh` 和 `max` 各自独立**（有的模型有 max 没 xhigh）：

```js
// 过滤
levels:_h.filter(A=>{
  if(A==="max"  && !gNH(K)) return false;   // max 需要 gNH(model)
  if(A==="xhigh"&& !XJH(K)) return false;   // xhigh 需要 XJH(model)
  return true;
})
// downgrade：不支持的档一律降到 high
if(T==="max"  && !gNH(H)) return "high";
if(T==="xhigh"&& !XJH(H)) return "high";
```

- **`XJH`（xhigh）**：描述 `"Fable 5, Opus 4.8/4.7 only"` → 仅 `fable-5 / mythos-5 / opus-4-7 / opus-4-8`
- **`gNH`（max）**：描述 `"Fable 5, Opus 4.6+, Sonnet 4.6"` → `fable-5 / mythos-5 / opus-4-6 / opus-4-7 / opus-4-8 / sonnet-4-6`

所以 **opus-4-6 / sonnet-4-6 有 max 没 xhigh**（档位 `low/medium/high/max`）。给 opus-4-6 发 `xhigh` → 400 `does not support effort level 'xhigh'`。

hebbian 实现见 `common/reasoning.rs` 的 `anthropic_supports_xhigh_effort` / `anthropic_supports_max_effort`，前端镜像在 `lib/reasoning.ts`。

---

## 4. fallbacks：只有 Fable 系列发

`fallbacks` 是 per-model 的，构造逻辑（函数 `QxK` + `R76`）：

```js
function R76(H){
  if(QR9(H))return;
  if(!GlH(H)&&!p6H(H)&&!I5H(H)&&!C6H(H))return;  // 不在白名单 → 不发 fallbacks
  if(!j3())return lJ5();
  let _=qR();return y6H(_)?_:void 0
}
```

- `GlH(H) = W9(H).startsWith("claude-fable-")` → **Fable 系列**（这是主力，用户实测"只有 fable 才支持"）
- `I5H(H) = /-eap($|\[)/i.test(H)` → `-eap` 后缀（early access program）
- `lJ5()` → 默认 opus（`ANTHROPIC_DEFAULT_OPUS_MODEL`，c.json 实测 fable-5 的 target 是 `claude-opus-4-8`）

给非 Fable 模型发 `fallbacks` → 400 `'claude-opus-4-8' does not support the fallbacks parameter`。

hebbian：`anthropic_supports_fallbacks`（仅 fable/mythos），target 硬编码 opus-4-8。

---

## 5. prompt cache：前缀顺序 + ttl + scope

### 5.1 缓存前缀顺序是 tools → system → messages（最大的坑）

Anthropic 的 prompt cache 是**前缀缓存**，前缀拼接顺序是 **`tools` → `system` → `messages`**——`tools` 在最前。

> 教训：hebbian 的 `ToolRegistry` 原本用 `HashMap` 存工具，`.values()` 迭代序**每次进程随机**。
> 结果即使 system 逐字节相同，tools 顺序一抖动，整个缓存前缀就失配，**每轮全部 cache miss**
> （`cache_creation` 满额、`cache_read=0`）。改成 `BTreeMap`（按 name 字母序稳定）后才命中。
> 这个 bug 还连累了所有 provider 的 prompt cache。

诊断方法：dump 实际 wire body，对比两次请求的 `tools` / `system` 的 md5——system 一致但 tools 不一致，即顺序问题。

### 5.2 cache_control 打点（c.json 实测）

- system 稳定块（harness 正文）：`{type:"ephemeral", ttl:"1h", scope:"global"}`——`scope:global` 跨会话共享缓存，需 `prompt-caching-scope-2026-01-05` beta
- session-specific 块 / 最后一条 message：`{type:"ephemeral", ttl:"1h"}`（无 scope）
- `ttl:"1h"` 需 `extended-cache-ttl-2025-04-11` beta（否则降级 5min）

> hebbian 当前 system 合成 2 块（banner + harness 合并），打 1 个 system 断点；真 CC 是 4 块、
> 2 个 system 断点。harness 里混了 session 信息（Environment 段），所以 `scope:global` 的跨会话
> 复用发挥有限——这是留给后续的优化点（把 system 拆成 [稳定 harness scope:global, session-specific ttl]）。

---

## 6. OAuth profile / 账号信息

`${BASE_API_URL}/api/oauth/profile`（`BASE_API_URL` = `https://api.anthropic.com`），GET + `Authorization: Bearer <token>`（函数 `gfH`）。返回：

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

> 关键：**本地 CC 凭据（Keychain `Claude Code-credentials` / `~/.claude/.credentials.json`）里没有
> account uuid**——只有 `accessToken`/`refreshToken`/`expiresAt`/`scopes`/`subscriptionType`/`rateLimitTier`。
> 且 access_token 是 `sk-ant-oat01-...` 格式，**不是 JWT**，解不出 uuid。要拿真实 account uuid /
> email / plan，**必须额外调 profile 接口**。

其它 OAuth endpoint（`/api/oauth/...`）：`account`、`profile`、`usage`、`organizations`、
`claude_cli`、`file_upload`、`files`、`validate`；另有 `/api/claude_cli/bootstrap`。

`/api/oauth/usage` 返回 `five_hour` / `seven_day` / `seven_day_sonnet`（各含 `utilization` / `resets_at`）。

---

## 7. system 结构 / metadata / billing header

### 7.1 system 四块（c.json 2.1.170）

1. `x-anthropic-billing-header: cc_version=2.1.170.005; cc_entrypoint=cli; cch=<5hex>;`
2. `You are Claude Code, Anthropic's official CLI for Claude.`（banner，服务端据此识别合法 CLI）
3. harness 正文（大段 # Harness / # Communicating / ...）+ `cache_control{ttl:1h,scope:global}`
4. session-specific guidance + `cache_control{ttl:1h}`

> **`cch` 是网络层运行时注入的随机值**（默认 `00000`，CC 魔改了底层发送代码在出网时改写），
> 静态无法稳定复现。hebbian **不发 billing header block**——等价 `CLAUDE_CODE_ATTRIBUTION_HEADER=0`
> 的合法行为，且发占位 `cch` 反而每次击穿 prompt cache 前缀。

### 7.2 body 构造骨架

```js
return {
  model, messages, system, tools, tool_choice,
  ...l && (!wK||tq.length>0) && {betas:u2(mZ8(tq))},   // l = 第一方/官方 endpoint
  metadata: A2H(), max_tokens, thinking,
  ...zq && l && G6.includes(KNH) && {context_management:zq},
  ...Object.keys(eq).length>0 && {output_config:eq},
  ...t && Y && l && !wK && {diagnostics:{previous_message_id: $??null}},
  ...
}
```

两个有用的开关变量：
- `l` ≈ 「官方第一方 endpoint」判断——gating `betas` / `context_management` / `diagnostics` 等
- `wK = __(process.env.CLAUDE_CODE_SIMULATE_PROXY_USAGE)`——置位时**剥掉 beta headers**（模拟过代理）

### 7.3 metadata.user_id

JSON-string（≥ 2.1.78）：`{"device_id":"<64hex>","account_uuid":"<uuid>","session_id":"<uuid>"}`。
旧格式：`user_{64hex}_account_{uuid}_session_{uuid}`。服务端（中转网关）的 validator 对 JSON 格式
只要 `device_id` + `session_id` 非空即可，`account_uuid` 可空。

---

## 8. 通用经验 & 坑

- **字段缺 beta 报的是 `Extra inputs are not permitted`**，不是"字段不支持"——别被文案带偏，先想"这个字段要不要配 beta"。
- **per-model 的能力（effort / fallbacks）一定有白名单函数**，且通常有一句给用户看的描述字符串可做旁证。
- **缓存前缀顺序 tools→system→messages**——tools 顺序不稳定是最隐蔽的 cache-miss 元凶（HashMap 迭代序）。
- **凭据文件 ≠ 全部身份信息**——account uuid / email / plan 要调 profile 接口。
- **error-classify 函数是金矿**——服务端所有 400 校验文案都在那一坨 `includes(...)` 里。
- minified 变量**追别名到工厂**才知道真值；描述字符串是捷径。

---

## 9. 还没挖、值得继续看的方向

按"请求形态 / 能力"分组，留给后续：

- **structured-outputs-2025-12-15**：结构化输出（JSON schema 约束）请求体长什么样
- **tool-search-tool-2025-10-19**：工具搜索工具（动态加载 tool schema）的协议
- **skills-2025-10-02**：skills 在请求里怎么注入
- **mid-conversation-system-2026-04-07**：会话中途改 system 的机制
- **context-hint-2026-04-09 / advanced-tool-use-2025-11-20**：未知语义
- **managed-agents-2026-04-01 / afk-mode-2026-01-31**：后台 / 无人值守 agent 的请求形态
- **compaction（`/compact`）**：上下文压缩请求怎么构造（对照 hebbian 的压缩实现）
- **thinking-token-count-2026-05-13**：thinking 计费 / token 计数
- **`l`（第一方判断）/ `wK`（SIMULATE_PROXY_USAGE）的完整定义**：精确知道哪些字段只在官方 endpoint 发

> 挖法同 §1：`strings $BIN | grep` 定位 → 追别名 → 找描述字符串旁证 → 必要时 dump wire body 对照。
