# Claude Code 研究笔记（索引）

> 内容已拆分到 `docs/cc_research/` 下两个专门文件。

---

## 目录

### [cc_research/reverse-methodology.md](cc_research/reverse-methodology.md)

**如何从 CC binary / extension.js 里挖东西**——逆向方法论：

- binary 位置 / `strings` 提取
- UTF-8 中文 vs ASCII 的搜法差异（`grep -a`）
- minified JS 三板斧（追别名链、描述字符串捷径、错误处理反推）
- extension.js vs binary 的分工
- session jsonl 实录做动态验证
- 常见陷阱

### [cc_research/cc-internals.md](cc_research/cc-internals.md)

**CC 实际内部逻辑**——已挖出的具体机制：

1. 顶层字段 + enabling beta 成对规律 + 已知 beta 全集
2. effort 档位白名单（xhigh vs max 两套独立）
3. fallbacks（只有 Fable 系列发）
4. prompt cache（前缀顺序 + ttl + scope）
5. OAuth profile / 账号信息接口
6. system 四块结构 / billing header / body 骨架
7. 注入核查（anti-prompt-injection，纯模型训练行为）
8. Monitor（plugin 持久化外部事件通道）
9. task_notification 格式与 CommandQueue 架构（插队行为、InboxPoller）
10. Speculation 机制（预执行 + promote）
11. mid-conversation-system（模型支持矩阵）
12. `/background` fork 机制
13. 待挖方向
