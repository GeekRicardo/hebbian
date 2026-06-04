# Claude Code 源码逆向研究方法

## 文件位置

```
~/.vscode/extensions/anthropic.claude-code-<version>-darwin-arm64/extension.js
```

版本号示例：`2.1.152`、`2.1.153`。安装多个版本时并列存在，取最新的。

## 核心文件结构

```
extension.js      ← 所有逻辑打包成单文件，minified，通常几百 KB~几 MB
package.json      ← 版本号、入口、依赖
resources/        ← 静态资源
webview/          ← UI 部分的独立 HTML/JS
```

extension.js 是 **单行 minified bundle**。无 source map，无换行，变量名混淆。

## 搜索方法

### 基本原则

1. 用 `grep -o '.\{N\}KEYWORD.\{N\}'` 提取关键词上下文（N 建议 200~500）
2. 混淆后的变量名不可依赖，只能依赖字符串字面量
3. 多个相关关键词交叉验证，避免误判

### 有效关键词

| 目的 | 关键词 |
|------|--------|
| 压缩逻辑 | `contextTokenThreshold`, `compactionControl`, `summaryPrompt`, `compact_20260112` |
| 缓存 | `cache_creation_input_tokens`, `cache_read_input_tokens`, `context_management` |
| tool result | `tool_use_id`, `tool_result` |
| 错误处理 | `Expected text response`, `Unexpected event order` |
| 服务端能力 | `edits:`, `BetaToolRunner`, `x-stainless-helper` |

### 常用命令

```bash
EXT=~/.vscode/extensions/anthropic.claude-code-2.1.152-darwin-arm64/extension.js

# 提取关键词上下文（300字符）
grep -o '.\{300\}contextTokenThreshold.\{300\}' "$EXT" | head -3

# 搜索多个关键词（OR）
grep -o '.\{200\}\(compact_20260112\|compactionControl\).\{200\}' "$EXT" | head -3

# 找函数体（关键词到分号/花括号截止）
grep -o '.\{100\}summaryPrompt.\{600\}' "$EXT" | sed -n '1p'
```

## 关键发现索引

| 功能 | 关键词 | 说明 |
|------|--------|------|
| 客户端压缩函数 | `contextTokenThreshold` | `Ai`/`an` 函数，token 计数 + LLM 摘要 + 消息替换 |
| 服务端压缩接口 | `compact_20260112` | deprecated 警告 + 推荐用法 |
| 流式响应解析 | `context_management`, `message_delta` | 响应体字段处理 |
| tool runner | `BetaToolRunner`, `x-stainless-helper` | 请求头标记 |
