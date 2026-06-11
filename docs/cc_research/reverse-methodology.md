# Claude Code 逆向方法论

> 如何从 CC binary / extension.js 里找出特征函数、协议字段、行为规则。
> 配套内容：[cc-internals.md](cc-internals.md) 记录已挖出的具体逻辑。

---

## 1. 找文件

### 1.1 extension.js

VS Code 扩展目录：

```
~/.vscode/extensions/anthropic.claude-code-<版本>-darwin-arm64/
├── extension.js                       ← 所有 JS 逻辑（minified，~几 MB）
└── resources/native-binary/claude    ← bun 打包单文件可执行（~200 MB）
```

两者分工：
- **extension.js**：插件 schema（Monitor / Skill）、UI 字符串、beta feature flag 函数（`e56` 等）、MCP 相关配置。可直接文本搜索。
- **native binary**：runtime 逻辑、harness 文本、CommandQueue、Speculation、InboxPoller 等。JS 以明文字符串内嵌在二进制里。

### 1.2 找版本号

```bash
ls ~/.vscode/extensions/ | grep anthropic.claude-code | tail -1
# → anthropic.claude-code-2.1.170-darwin-arm64
```

---

## 2. 两种提取方式

### 2.1 `strings`（ASCII only）

```bash
BIN=~/.vscode/extensions/anthropic.claude-code-2.1.170-darwin-arm64/resources/native-binary/claude

# 找 beta header（形如 xxx-2026-04-07）
strings "$BIN" | grep -oE "[a-z][a-z-]+-20[0-9]{2}-[0-9]{2}-[0-9]{2}" | sort -u

# 找某字段上下文（截 400 字符窗口）
strings "$BIN" | grep -oE ".{400}diagnostics:\{previous_message_id" | head -1

# 找函数定义片段
strings "$BIN" | grep -oE "function XJH\(H\)\{[^}]{0,300}" | head -1
```

`strings` 默认提取 ≥4 连续可打印 ASCII。输出动辄上 MB，配合 `head -n5` / `grep -oE ".{N}pattern.{M}"` 截窗口看。

### 2.2 `grep -a`（UTF-8 中文）

`strings` **只找 ASCII**——中文 UTF-8 字节序列全部被跳过。要搜中文必须直接对 binary：

```bash
# 搜中文字符串（是否存在）
grep -c -a "注入核查" "$BIN"          # 0 → 纯模型训练行为，不在 binary 里

# 提取包含中文的行（binary 当文本流读）
grep -a "某个中文关键词" "$BIN" | head -5
```

**实战经验**：`注入核查` 系列文字 `grep -a` 结果为 0，确认这是 Fable 5 / Opus 4.8 的模型训练行为，harness 里没有对应指令文本。

---

## 3. minified JS 三板斧

### 3.1 追别名链到底

minified 代码变量是单字母/短哈希（`L5H`、`ef`、`Ty`），且复用严重（`ef` 在不同作用域是不同东西）。要追到那个真正带字面量的工厂调用：

```bash
# Step 1：看 L5H 是啥
strings "$BIN" | grep -oE "L5H=[^;]{0,80}"
# → L5H=ef("cache_diagnosis","cache-diagnosis-2026-04-07")

# Step 2：追 ef 的定义
strings "$BIN" | grep -oE "function ef\([^)]+\)\{[^}]{0,200}" | head -1
# → ef 是 beta feature 工厂，接 (key, header-name)
```

### 3.2 优先信描述字符串

给用户看的 UI 文案往往直接说明语义，比读 minified 逻辑快得多：

```bash
# 找 effort 档位描述
strings "$BIN" | grep "Fable 5"
# → "Fable 5, Opus 4.8/4.7 only"（xhigh 白名单的描述字符串）
# → "Fable 5, Opus 4.6+, Sonnet 4.6"（max 白名单）
```

一句描述字符串顶半小时读 minified 黑名单逻辑。

### 3.3 从错误处理反推服务端规则

CC 里有巨大的 error-classify 函数（`GC6` / `pW3`），逐条 `H.message.includes("...")` 匹配 400 文案。这些字符串就是服务端的校验规则清单：

```bash
strings "$BIN" | grep -oE '"does not support[^"]{0,80}"'
strings "$BIN" | grep -oE '"Extra inputs[^"]{0,60}"'
strings "$BIN" | grep -oE '"does not support effort level[^"]{0,40}"'
```

实战挖出来的服务端规则：
- `"Extra inputs are not permitted"` → 发了字段但没带对应 enabling beta
- `"does not support the fallbacks parameter"` → 给非 Fable 模型发了 `fallbacks`
- `"does not support effort level 'xhigh'"` → 给 opus-4-6/sonnet-4-6 发了 `xhigh`
- `"diagnostics.previous_message_id"` → diagnostics 字段格式错误

---

## 4. extension.js 的搜法

extension.js 是明文 JS（minified 但不加密），可直接 `grep`：

```bash
EXT=~/.vscode/extensions/anthropic.claude-code-2.1.170-darwin-arm64/extension.js

# 找函数 e56（mid-conversation-system 支持判断）
grep -oE "e56=[^;]{0,500}" "$EXT" | head -1

# 找 Monitor schema（Wze）
grep -oE "Wze=[^;]{0,800}" "$EXT" | head -1

# 找 beta feature 工厂调用
grep -oE 'ef\("[^"]+","[^"]+"\)' "$EXT" | sort -u
```

比 binary 好搜，因为 JS 完整，行号稳定，`-oE ".{N}pattern.{M}"` 截窗口即可。

---

## 5. session jsonl 实录（行为逆向）

binary 逆向看到的是**静态逻辑**，有时需要**动态证据**验证。CC session 写全量 jsonl：

```
~/.claude/projects/<enc>/
├── <session-uuid>.jsonl    ← 完整消息历史（含 queue-operation 条目）
└── ...
```

用法：
```bash
# 找 queue-operation 条目
grep '"queue-operation"' ~/.claude/projects/*/**.jsonl | head -20

# 找实际的 task-notification 投递
grep '"task-notification"' ~/.claude/projects/*/**.jsonl | head -10

# 找注入核查输出（模型 assistant turn）
grep '注入核查' ~/.claude/projects/*/**.jsonl | head -5
```

**jsonl 实录能告诉你**：
- 消息的真实 `origin.kind`
- enqueue / remove / 投递的三步时序
- 模型实际输出什么（对照 binary 里的描述）

---

## 6. 常见陷阱

| 陷阱 | 正确做法 |
|------|---------|
| `strings` 搜中文得到空结果 | 改用 `grep -a` 直接对 binary |
| 别名没追到底就下结论 | 追到工厂函数的字面量参数 |
| 只看 extension.js 不看 binary | 两者分工不同，runtime 逻辑在 binary |
| 只看 binary 不看 jsonl | jsonl 有动态行为的真实记录 |
| error 文案「字段不支持」当字面理解 | `Extra inputs are not permitted` 说的是 **schema 拒了未知字段**，不是"功能没开" |
| 发字段不带 enabling beta | 先找对应的 `ef(...)` 工厂，字段和 beta **成对** |

---

## 7. 快速定位某个功能的套路

```
1. 用 UI 文案 / 错误文案在 extension.js / binary 里锁定关键字符串
2. 追别名链到工厂调用，拿到 beta-header 名
3. 在 binary 找字段构造逻辑（搜字段名 / beta 名的上下文）
4. 看 error-classify 函数确认服务端拒绝条件
5. 用 jsonl 实录动态验证
```
