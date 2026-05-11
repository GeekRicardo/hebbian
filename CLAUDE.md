# Hebbian — Agent 协作准则

## 唯一设计准则

[docs/架构.md](docs/架构.md) 是本项目的**唯一设计准则**。所有代码、改动、新增功能必须以它为锚。

**辅助文档**：
- [docs/changelog.md](docs/changelog.md)：修改时间线，只增不减
- [docs/compaction.md](docs/compaction.md)：上下文压缩的横向调研（背景资料）

---

## ⚠️ 任何修改前必做

**适用范围**：包括但不限于——bug 修复、功能新增、重构、用户明确要求的改动、自己发现的问题。

### 步骤 1：定位架构.md 中的对应章节

任何修改都必然落在架构.md 的某一节内。先找到它：

- 改协议 → §3 通信契约
- 改 Session / Run / Turn / Step → §4.1 / §4.2
- 改 agent loop / dispatcher → §4.3
- 改 Tool / RunMode / AutoMode → §4.4
- 改 HITL / 审批 → §4.5
- 改权限规则 → §4.6
- 改压缩 → §4.7
- 改 Hooks → §4.8
- 改 Recorder / jsonl → §4.9
- 改 Observability → §4.10
- 改 model adapter → §4.11
- 改 Model Gateway → §5
- 改 Storage / 文件锁 → §6
- 改 CoreClient / surface settings → §7
- 改 TUI / CLI → §8
- 改 Prompt → §9

### 步骤 2：做设计影响评估

回答以下 5 个问题，**必须显式答完**：

1. **是否与架构.md 相悖？**
   - 违反 §0 任一条原则 → 不允许，必须先更新架构.md 或放弃改动
   - 违反 §12 任一关键原则 → 同上
   - 违反 §13 任一已定决策 → 同上

2. **是否符合既定设计？**
   - 修改的实现路径是否与对应章节描述一致
   - 命名规范是否符合 §4.4.7（工具 PascalCase / 参数 camelCase / Rust 内部 snake_case）
   - 文件落盘位置是否符合 §6.1 目录布局

3. **是否引入新设计 / 需修改架构.md？**
   - 如果引入新协议字段、新模块、新工具、新模式 → 必须先更新架构.md 对应章节
   - 仅修改实现细节但不改对外 API → 不需要更新架构.md，但需在 changelog 注明
   - 新增决策点 → 在架构.md §13 表格追加一行

4. **会影响哪些其他模块？**
   - 改 protocol → 同步 CLI render、desktop chat.rs 翻译、前端 types.ts 三处映射
   - 改 EventPayload → 同上
   - 改 Tool → CLI 与 desktop 两套观察者均需验证
   - 改 system prompt → 检查 prompt-cache 是否仍命中（§9.3 约束）
   - 改 storage 文件格式 → 检查向前向后兼容性、加载老 jsonl 是否可用
   - 改 Mode → 检查工具列表过滤（§4.4.5）、SEMI 段注入（§9.3）

5. **修改的取舍是否清楚？**
   - 若与既定设计有冲突或会影响其他模块：必须写清楚利害关系与取舍方案
   - 即使用户强烈要求修改不合理的方案，也须先向用户讲清利害再确认
   - 用户明确确认后才能动手

### 步骤 3：实施

实施时遵循：
- 仅改必要文件，避免顺手 refactor
- 命名严格遵循 §4.4.7
- 持久化必经 storage 模块（§4.9 / §6.2）
- 不允许 agent-core 直接 `fs::write` / 直接 `use tauri` / 直接 `use reqwest`（reqwest 仅 model-gateway 与 web 工具可用）

### 步骤 4：验证

```bash
# Rust 编译
cargo check --workspace
cargo check -p agent-core --tests

# TS 类型检查
pnpm exec tsc --noEmit

# CLI 端到端验证（比启动 Tauri 快）
./target/debug/hebbian-cli "..." --mock
./target/debug/hebbian-cli --json '{"messages":[...]}' --mock
./target/debug/hebbian-cli --mock     # loop 模式

# 桌面 dev 模式
pnpm tauri dev
```

修改 EventPayload 后必须跑这三种 CLI 模式确认事件流完整。

### 步骤 5：追加 changelog

[docs/changelog.md](docs/changelog.md) 是只增不减的修改时间线。**每次修改必须追加一条**。

格式参考 changelog.md 顶部模板，最少包含：
- 日期（ISO 格式）
- 一句话总结改了什么
- Why：为什么改（用户痛点 / 设计修正 / bug / 路线图推进）
- 改动列表：哪些文件 / 哪些模块
- 影响范围：动了哪些 crate / surface / 协议；是否破坏兼容
- 留尾巴：未完成项 / 已知风险 / 后续要做的事；没有则写「无」

**无 changelog 的修改视为未完成**。

---

## 与用户讨论的规则

用户可能强烈要求某些修改，作为 agent 应：

1. **先评估**：按上面 5 个问题评估
2. **若有冲突或风险**：暂停实施，向用户回复：
   - 该修改与架构.md §X 的某条原则 / 决策冲突
   - 利害关系（带来的好处 + 引入的风险）
   - 替代方案（在不违反架构的前提下能做到什么）
3. **等待用户确认**：用户可能：
   - 同意替代方案 → 按替代方案实施
   - 坚持原修改 → 先更新架构.md 对应章节（标记决策变更原因），再实施
   - 放弃修改 → 不改

**禁止**：未经用户确认直接执行违反架构.md 的修改。即使用户原话明确要求，若与架构冲突也必须先讨论再动手。

---

## 开发命令

```bash
# CLI 启动方式
hebbian                # 默认 TUI（终端支持时）
hebbian --repl         # REPL 简易模式
hebbian "你好"          # 单次问答
hebbian --json '...'   # JSON 多轮（脚本用）
hebbian --tui          # 显式启 TUI

# Provider 管理
hebbian --providers list
hebbian --provider set openai/gpt-5

# 调试模型 IO
HEBBIAN_DUMP_MODEL_IO=1 hebbian "..."
# 输出位置：~/.hebbian/sessions/<session_id>/model_io.jsonl

# OTLP 追踪
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 hebbian ...
```

CLI 与 desktop 共享同一个数据目录 `~/.hebbian/`。

---

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
