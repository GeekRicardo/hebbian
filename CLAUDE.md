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
- 改 CoreClient → §7
- 改 Desktop 命令系统（`//xxx`）→ §8
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
   - 改 protocol → 同步 desktop chat.rs 翻译 + 前端 types.ts 两处映射
   - 改 EventPayload → 同上
   - 改 Tool → desktop 观察者（chat.rs + 前端 MessageBubble）需验证
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
- **代码注释里禁止出现外部项目名 / 它内部函数名 / 内部文件路径**（如 `openhanako 的 xxx`、`参考 codex/foo.rs`、`与 claude-code-haha applyX 一致`）。原因：外部项目的函数会重命名 / 文件会移动，注释会 rot 成考古碎片；且这类信息对未来读代码的人没用——他真要对比时会去看那个项目的 HEAD，而不是 hebbian 注释里某个时间点的引用。**用法**：借鉴的事实、原项目踩过的坑、为什么这么做的理由、与之前实现的对比，全部写到 changelog 那一条里（changelog 是历史档案，rot 不影响阅读）；代码注释只写「这是什么 + 为什么必须这样」的当下事实

  反例（✗ 不允许）：
  ```rust
  // 与 openhanako provider-compat/deepseek.js 的 ensureReasoningContentForToolCalls 一致
  // 参考 codex/src/foo.rs:42 的写法
  ```

  正例（✓ 允许）：
  ```rust
  // tool_calls 多轮里 reasoning_content 缺失会让 server 直接 400，
  // 比悄悄丢推理链更可控——抛错让 surface 提示用户压缩或开新会话。
  ```

  changelog 那一条里写「借鉴了 openhanako 的 XXX 函数 / 这是 issue #468 的根因 / 之前我们是怎样 / 现在改成怎样 / 好处坏处」

### 步骤 3.1：UI 文案纪律（给用户看的字必须是人话）

**适用范围**：Desktop / hebweb 前端 UI 字符串（label / description / placeholder / toast / tooltip / dialog body 等所有终端用户能看到的文本）。

**禁止**：把架构、路径、模块、source 枚举值、内部命名等内部行话写到 UI 文案。例：

  反例（✗）：
  ```
  按 workdir /Users/ricardo/code/ricardo/rust/hebbian 加载三层来源：global / project / project_code（代码内嵌）
  当前对话未设置 workdir，项目级 skill 不可导入
  selected 写入 ~/.hebbian/projects/<enc>/skills/
  ```
  → 用户看到「workdir」「项目级 skill」「project_code」「<enc>」一脸懵，且暴露内部目录细节像在写后端文档。

  正例（✓）：
  ```
  已加载的 Skills
  当前对话没绑定项目，没法装到「当前项目」里——选「全局」试试
  ```

**写法**：
- 用户视角，问"这个用户当下要决策/操作什么"，只说他要的信息
- 避免：路径、目录、source 枚举名（`global` / `project` / `project_code`）、字段名、文件名
- 状态徽章可以保留简短英文（`global` / `project`）做颜色标签，但**说明文字不要重复这些词**
- 错误 / 提示用动作建议（"先在设置里选个项目"）而不是状态描述（"workdir 为空")
- 凡是写出包含 `~/` / `<encode>` / 字段名 / Rust 类型名 / 内部模块名的字符串：**重写**

**自检**：写完一段 UI 文案后问自己——"如果我妈打开看到这句话，她能懂吗"。懂不了就改。

### 步骤 4：验证

```bash
# Rust 编译
cargo check --workspace
cargo check -p agent-core --tests
cargo test -p agent-core --lib

# TS 类型检查
pnpm exec tsc --noEmit

# 桌面 dev 模式（唯一 surface）
pnpm tauri dev
```

修改 EventPayload 后必须跑 desktop dev 模式手动验证事件流完整（chat.rs 翻译 + 前端渲染两端都看一眼）。

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
# 启动 Desktop（GUI surface）
pnpm tauri dev

# 启动 heb CLI daemon（AI 脚本化调试 surface，2026-05-20 changelog）
cargo build -p hebbian-cli
./target/debug/heb new --provider=<id> --workdir <dir>

# 启动 hebweb（浏览器/Playwright surface，2026-05-21 changelog）
cargo build -p hebbian-web-server
cd apps/desktop/frontend && pnpm build && cd -    # 首次或前端改动后
./target/debug/hebweb --port 38080                 # 之后访问 http://127.0.0.1:38080

# 调试模型 IO（任一 surface 启动前导出环境变量）
HEBBIAN_DUMP_MODEL_IO=1 pnpm tauri dev
# 输出位置：~/.hebbian/sessions/<session_id>/model_io.jsonl

# OTLP 追踪
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 pnpm tauri dev
```

Desktop / heb / hebweb 三个 surface 共享同一个数据目录 `~/.hebbian/`（文件锁保护并发写）。任一 surface 能复现的 agent_core 问题，另两个也能。

---

## ⚠️ 调试 bug 前必做：先用 `heb` CLI 或 `hebweb` 自主复现

**适用范围**：用户报 bug / 自己发现 agent 行为异常 / 验证修复是否真的解决问题。

### 步骤 1：判定走哪个 surface 复现

三个 surface 走相同的 agent_core 主路径，行为对称。哪个最快能复现就用哪个，**不要立刻让用户去 Desktop 重跑**：

| 问题类型 | 首选 surface | 理由 |
|---------|------------|------|
| agent 行为（工具调用错、回答跑偏、死循环、多轮上下文、缓存、HITL、cancel、prompt、RunMode、Hooks、provider 协议、session 持久化 / 崩溃恢复） | **heb CLI** | NDJSON 事件流可脚本化 + 完整 model_io.jsonl + 最快上手 |
| UI 渲染 / 样式 / 工具卡片显示 / 流式 bubble 折叠 / 输入框 / 侧边栏 / 设置弹窗 / 审批/提问弹窗 UX / EditsWorktree 可视化 diff / 前端 store 状态机 | **hebweb + Playwright** | 同一份 React 代码 + 真实 agent_core 数据 + DOM/截图/点击可控（详见 [docs/heb-cli-debug.md §9](docs/heb-cli-debug.md)） |
| Tauri 命令分发本身的 bug / 全局快捷键 / 菜单 / 托盘 / 系统通知 / 文件对话框 | **Desktop**（最后兜底） | hebweb 没有 Tauri native 能力对应 |

### 步骤 2：读 [docs/heb-cli-debug.md](docs/heb-cli-debug.md)

它是给 AI 看的自包含手册：一分钟上手、完整命令/事件表、常用复现 pattern、故障速查、原理。读它而不是从源码拼。

### 步骤 3：自主跑

最小 loop：

```bash
heb new --provider=<id> --workdir /tmp/repro > /tmp/heb.log 2>&1 &
sleep 1; SID=$(jq -r .session_id < <(head -n1 /tmp/heb.log))
heb input $SID "<触发 bug 的输入>"
# tail -f /tmp/heb.log 看事件流；按 permission_requested / question_requested 自动响应
# 看实际模型 IO：~/.hebbian/sessions/$SID/model_io.jsonl
# 看完整对话历史：~/.hebbian/sessions/$SID/session.jsonl
```

报告 bug 时附上：触发输入 + 关键事件行 + （如有）`model_io.jsonl` 中相关请求段。

### 步骤 4：修完后自验

修完 bug 必须**用同一个 heb 脚本重跑确认修复**，再交付给用户。"我改完了，请你试试"是低质量交付。

---

## 现有 `heb` 命令不够用时：允许新增

如果某种调试场景 **能在 agent_core 层实现但现有 heb 命令做不到**（例：dump 某个 session 的 transcript 但不开新 run、注入一条历史 user message、查询当前 pending_approvals 状态、强制重置某项 ReadStateTracker），允许新增 IPC 命令或事件。

新增前先评估：

1. **现有命令真的不够吗？**
   - 90% 的调试需求能用现有 8 个命令 + 文件系统直接看 jsonl/model_io 解决
   - 先确认问题不能通过组合现有命令 + 读 `~/.hebbian/` 下文件解决，再考虑加命令

2. **新命令是否走 agent_core 主路径？**
   - **必须**：新命令的语义对应 agent_core / storage 已有的某个能力，CLI 只是 surface 化它
   - **禁止**：新命令做 agent_core 之外的事（例如直接 fs::write session.jsonl 篡改历史、绕过 ApprovalDecision 直接修改 rules.json）—— 这会让 CLI / Desktop 行为脱钩，违反"两 surface 对称"原则

3. **是否破坏 Desktop 兼容？**
   - 加 `IpcCommand` variant：纯 additive，旧客户端无感
   - 加 `DaemonEvent` variant：旧脚本会忽略未知 event，可接受
   - 改 `IpcCommand` / `DaemonEvent` **现有字段语义** → 禁止，必须新加 variant

4. **走完[动手前必做](#️-任何修改前必做)5 步**
   - 大概率落在架构.md §7（CoreClient）或新设的 surface 节
   - 如果新命令暴露的是 agent_core 已有但未导出的能力 → 不动架构.md，只走 changelog
   - 如果新命令需要 agent_core 加新能力 → 先改架构.md 对应章节

### 实现完后必须同步更新

- [apps/cli/src/ipc.rs](apps/cli/src/ipc.rs)：协议类型
- [apps/cli/src/main.rs](apps/cli/src/main.rs)：clap 子命令
- [apps/cli/src/daemon.rs](apps/cli/src/daemon.rs)：`handle_command` 分支
- [docs/heb-cli-debug.md](docs/heb-cli-debug.md)：命令表 §2 + 事件表 §3 + （如有）pattern §4
- [docs/changelog.md](docs/changelog.md)：追加一条，注明为什么加、典型用例

**新增命令的提交 = CLI 代码 + 文档 + changelog 三件套缺一不可**，仅改代码不更新文档视为未完成。

---

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
