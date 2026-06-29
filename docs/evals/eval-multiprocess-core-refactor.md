# Coding Agent 测评题：多进程核心重构的缺陷诊断与修复

> 本题取材自 hebbian 真实工程事件：一次「多 surface 统一 + 核心全进 agent-core + run 移到独立 hebcore 进程」的大架构重构**之后**，暴露出一批跨进程/并发/状态机缺陷。基线为 git tag `template-core-reflactor`（重构刚落地、缺陷尚未修复的状态）；标准答案为其后的修复提交（`1f96407` + `f1a422f`）。
>
> 测评目标：考察一个 coding agent 在**大型真实代码库**里**自主诊断 + 彻底修复**并发/分布式缺陷的综合能力——不是写算法题，而是「在一坨互相牵连的真实代码里，先复现、找根因、评估影响面、给最干净的修法、再验证」。

---

## 0. 为什么这是一道好题

- **真实**：不是构造的玩具 bug，是大重构后真实涌现的 15 个缺陷，互相牵连。
- **反作弊**：答案不是某个 LeetCode 式片段，而是「在 50+ 文件的 Rust workspace 里改对几行 + 加对几个锁/超时/状态位」。靠背题拿不到分。
- **分层**：从「用户报的 1 个显性卡死」到「主动审计挖出 14 个隐性缺陷」，能区分初级（修表象）和高级（挖根因 + 防回归）。
- **覆盖面广**：死锁、竞态、资源泄漏、状态机不一致、跨进程一致性、崩溃恢复、多端对称——并发/分布式系统的核心难点几乎全覆盖。

---

## 1. 被测系统背景（提供给被测 agent）

**hebbian**：Rust workspace + Tauri/React 的多 surface 本地 AI agent 框架。

关键架构事实（重构后）：

- **一套核心，多 surface**：所有业务逻辑在 `crates/agent-core`，三个 surface（Desktop=Tauri / heb=CLI daemon / hebweb=axum WS）共享数据目录 `~/.hebbian/`，**行为必须对称**。
- **run 移到独立 hebcore 进程**：对话的 run 不再在各 surface 进程内跑，而是在常驻的 `hebcore` 进程（持唯一 dispatch + 全部活 session），surface 作为它的 unix-socket 客户端连入。**这是大多数缺陷的根源**：很多旧代码假设「run 在本进程」，搬到独立进程后没跟着搬。
- **HITL 统一**：审批/提问的 oneshot 通道被换成「活 run 持有的 `HitlGate`」，surface 的回应经 transport 直接戳它。
- **工具并发池**：dispatch 把工具（含 Bash）从串行链改成并发池（`buffer_unordered`）。
- **落盘单点化**：assistant 累积 + 落盘收归 agent-core 的 `RunPersister`（actor 模式异步落 partial sidecar）。
- **AutoMode judge**：每个工具调用前由一个 LLM「判官」决定 allow/deny/ask，调用 `ModelClient`。

被测 agent 应当能拿到完整 repo（在 tag `template-core-reflactor` 处 checkout），并被告知：

- 设计准则文档 `docs/架构.md`、修改时间线 `docs/changelog.md`。
- 验证命令：`cargo check --workspace`、`cargo test -p agent-core --lib`、`cargo check -p hebbian-cli`、前端 `pnpm exec tsc --noEmit`。
- 三个 surface 行为对称、agent-core 不得直接 `fs::write`/`use tauri`/`use reqwest`、持久化必经 storage 模块。

---

## 2. 任务分层

### Level 1 — 复现并修复用户报告的显性缺陷（入门门槛）

> 给定唯一一条用户反馈，要求：先复现，再修，再用同一路径验证。

**用户反馈原文**（提供给被测 agent）：

> "桌面端发消息，渲染上 agent 已经回复完成了，但下面第一次 toolcall 的 automode 的 judge 一直卡着（黄色呼吸不消失），整个 run 不往下走。"

**期望被测 agent 做到**：

1. **定位根因**（不是改表象）：AutoMode 的判官 `judge_automode_request` 内部两次 LLM 调用（Classifier + judge）是**裸 `await`**，只靠底层 HTTP `read_timeout` 兜底；provider 抖动 / 同一会话并发请求挂起时，两次调用累计能卡几分钟，判官不返回 → 不 emit 决策事件 → 前端黄呼吸不灭、工具不执行、整条 ToolStep 卡死。
2. **给出彻底修法**：给判官加一个 wall-clock 整体超时（量级 ~25s），超时**降级为 Ask（转人工审批）**——与判官 `parse_decision` 的 fail-closed 策略同源（拿不准就交用户），而不是静默放行或继续卡。用户中断（cancel）仍须随时生效。
3. **写确定性回归测试**：用一个「永久挂起的 mock judge」复现卡死（修前必 fail），修后转人工放行（pass）。**注意**：测试档超时阈值要远大于正常 mock 判官耗时（避免高负载误触发把 Allow 误降级），又远小于 mock 挂起时长（能区分超时路径）——这是个真实的 flaky 陷阱。

**评分要点**：
- 只在前端加个「N 秒后隐藏黄呼吸」-> 治标不治本，run 仍卡死，0 分。
- 给判官加超时但超时后静默 Allow -> 安全漏洞（判官本是安全闸门），扣分。
- 超时 -> 降级 Ask + cancel 仍生效 + 确定性回归测试 + 处理测试 flaky 阈值。

---

### Level 2 — 主动审计：挖出重构引入的隐性缺陷（核心区分度）

> 不再给具体 bug，只给一句话：「这次『run 移 hebcore』的大重构刚落地，帮我把它可能引入的缺陷一次性审计出来，并评估严重度。」

考察 agent 是否能**系统化地沿架构变更面**找缺陷，而不是漫无目的地读代码。理想做法：按子系统维度并行审查（HITL / 工具并发 / 落盘 / 进程生命周期 / 事件流对称 / 前端状态机 / 注入状态机），每条缺陷给出 `文件:行 + 触发路径 + 为什么是 bug`，并对每条做**对抗性自我验证**（默认怀疑、尝试证伪，触发路径真实成立才确认），过滤假阳性。

**标准答案应当覆盖的 14 个隐性缺陷**（按严重度）：

#### HIGH（3 个，都是「run 移进程没收尾干净」）

| # | 缺陷 | 根因 | 修法 |
|---|------|------|------|
| H1 | hebcore 进程**不注册 wakeup resume handler** -> 后台任务/cron 唤醒的挂起 run 永远变孤儿、续跑整类静默失效 | resume handler 注册在旧的 surface 进程，run 却在 hebcore 进程跑——handler 与 run 劈到两进程即断链 | 在 run 所在进程（hebcore main）注册 resume handler；抽成 `register_wakeup_resume_handler` 供 hebcore/hebweb 共用 |
| H2 | desktop `run_conversation` **不处理 `RunSuspended`** -> `send_message` 命令永久阻塞 + 后台线程泄漏 + cancellation 不注销 | 挂起 run 不发 `RunFinished`、per-session broadcast 不关，订阅循环永久 `recv` | 把 `RunSuspended` 纳入 terminal break 集合 |
| H3 | CLI daemon observer **未迁移到 HitlGate** -> AutoMode 交互下单线程 drive 卡在 `on_permission_request` 死锁 | surface-session/desktop 的 observer 已改成「返回 None 不阻塞 + 审批走活 run HitlGate」，CLI 仍是旧的「oneshot pending + 阻塞 recv」，判官 resolve 的是另一个 gate | DaemonState 同步迁移到 HitlGate，observer 返回 None，allow/answer 命令直接戳 gate |

#### MEDIUM（4 个）

| # | 缺陷 | 根因 | 修法 |
|---|------|------|------|
| M1 | 并发工具对**不同文件**做 edits 快照时争用同一个 worktree `.git/index`，失败被 `let _ =` 吞 -> 静默丢 before 快照、Run 无法回退 | 工具改并发池后，不同文件各拿不同 per-path 锁（互不互斥），并发跑 `git add`/`commit` 撞同一 `.git/index.lock`（git fail-fast 不重试） | EditsWorktree 加一把 repo 级锁，串行化 snapshot 的 `add->commit` |
| M2 | 前端「活 partial 持久消息」与「streaming bubble」**双渲染**当前 assistant 段 | 后端新引入 `live-<id>` 持久 message，前端去重只认 `user_injected` 项 | 去重函数同时剔除 `live-` 前缀（streaming 时当前段由 bubble 独占） |
| M3 | **late-inject 静默丢消息**：run 收尾窗口的注入被接受（返回 Accepted）但永不 drain/落盘 | `inject` 只看 `pending_inputs.is_some()`，agent_loop 已过末次 drain | 接上 agent_loop 已维护的 `accepting` flag，收尾窗口 inject 返回 false -> surface 回落起新 run（+ 回归测试） |
| M4 | CLI 与其他 surface **事件不对称**：`DaemonEvent` 漏 `auto_handled`/`call_id` 字段、`PermissionAutoJudged` 被 `_ => None` 静默吞 | 新增协议字段没同步到 CLI 翻译层（`_ => None` 兜底掩盖了漏译） | 补字段透传 + 新增 variant 翻译；理想方案是把 `_ => None` 改成穷尽 match 强制未来同步 |

#### LOW（2 个）

| # | 缺陷 | 根因 | 修法 |
|---|------|------|------|
| L1 | desktop `remote_session_of` **取走式 remove** -> 代理 IPC 瞬时失败后映射被消费、不可重试、请求永挂；且 `track_remote` 无条件登记 auto_handled 请求 -> map 只增不减泄漏 | 把「定位 session」和「消费映射」耦合在代理成功之前 | `remote_session_of` 改只读，代理成功才 `forget_remote`；`track_remote` 跳过 auto_handled |
| L2 | 崩溃恢复时两 surface 并发打开同一 session -> **重复折叠** partial 成两份 Interrupted 进 jsonl | 折盘无跨进程互斥 | partial live guard 加非阻塞 `try_acquire`，折盘跨进程独占 + 持锁后复查 partial 仍在（幂等） |

**评分要点**：
- 找到 HIGH 全 3 个 = 及格线（这 3 个是真实功能回归，用户迟早撞上）。
- 每条缺陷必须能说清**触发的操作序列**，泛泛说「可能有竞态」不给分。
- 加分项：做了对抗性验证、报告里标注了 confidence、识别出「这是重构遗留而非新引入」。
- 高分项：识别出根因的**系统性模式**（如「H1/H2/H3 同源——run 移进程后多处旧假设没跟着搬」「M4 暴露了 `_ => None` 兜底破坏三端对称」）。

---

### Level 3 — 彻底性：把「最小修复」升级为「根除 + 防回归」（顶尖区分度）

> 给定 Level 2 的修复后留下的 3 个「留尾巴」，要求给出**从类型层/不变量层杜绝**的彻底修法，而非补丁。

| # | 留尾巴 | 补丁式（不够） | 彻底式（满分） |
|---|--------|----------------|----------------|
| T1 | 续跑的**中间段**误带「本 run 耗时」徽章（违反「仅末段带耗时」契约） | 在中间 flush 处手动传 None | **从类型层杜绝**：`flush_segment` 直接去掉耗时参数（中间段不可能再传错），run 耗时统一由 `finish(duration)` 负责——有新末段盖新段、无新段（已预落）按 message id 回填 |
| T2 | partial「段已落 jsonl + 异步删 partial」之间有**崩溃窗口**，下次 load 重复折成 Interrupted | 删 partial 改同步 | **建立 happens-before**：删 partial 的 actor 命令带 oneshot ack，flush/finish 改 async、锁外 await ack，保证「段已落 + partial 已清」有序；热路径的 Append 仍 fire-and-forget |
| T3 | 跨进程「一 session 一活 run」**无锁**，两 surface 并发跑同一 session 会双写 + 状态互相覆盖 | 进程内加个 AtomicBool | **跨进程文件锁**：每 session 一个 `run.lock`（OS 级排他），run 启动 try_acquire 持有整个 run 周期，抢不到则拒绝起 run（改走 inject）；崩溃时锁由 OS 释放 |

**评分要点**：
- 区分「能编译过的补丁」和「让 bug 在类型/不变量层面不可能再发生」。T1 的「删掉参数」是典型——用类型系统让「中间段传耗时」这件事写不出来。
- T2 考察对 async happens-before / actor 消息时序的理解。
- T3 考察「进程内锁 != 跨进程锁」的分布式意识。

---

## 3. 综合评分维度（横切所有 Level）

| 维度 | 不及格 | 及格 | 优秀 |
|------|--------|------|------|
| **复现优先** | 直接改代码 | 改前先复现现象 | 复现写成确定性测试，修前 fail/修后 pass |
| **根因分析** | 改表象（前端隐藏/重试/吞异常） | 找到直接原因 | 找到系统性模式 + 评估影响面 |
| **修法质量** | 能编译 | 最小可用修复 | 从类型/不变量层根除 + 最干净 |
| **多端对称** | 只改报告的那个 surface | 三 surface 都改 | 发现并修复对称性破坏的机制（`_ => None`） |
| **防回归** | 无测试 | 加了测试 | 测试覆盖 A/B 翻转 + 处理了 flaky 陷阱 |
| **影响面评估** | 不评估 | 列出受影响文件 | 识别破坏性变更、协议兼容性、签名变更连带 |
| **验证诚实** | 声称修好但没验 | 跑了 cargo check | 现象级复现路径验证 + 全量测试 + 如实报告 flaky |
| **文档纪律** | 不留痕 | 改了 changelog | changelog（意图/权衡/留尾巴）+ 架构文档同步 |

---

## 4. 陷阱清单（专门考验「假装修好了」）

测评中可埋入以下诱饵，看 agent 是否会踩：

1. **flaky 阈值陷阱**（Level 1）：判官超时测试档若设太小（如 300ms），全量并发跑时正常 mock 判官也会超时，把无关测试的 Allow 误降级成 Ask 导致 flaky。能识别并放宽阈值 = 真懂。
2. **`_ => None` 兜底陷阱**（M4）：CLI 翻译层的通配兜底让漏译**无编译告警**。只补字段不改兜底 = 没看穿本质。
3. **静默 Allow 陷阱**（Level 1）：判官超时后图省事直接放行——把安全闸门变成摆设。
4. **进程内锁假象**（T3）：用 `AtomicBool`/`Mutex` 解决跨进程并发——单进程测试能过，多 surface 实战必翻车。
5. **`let _ =` 吞错**（M1）：git 争锁失败被吞，表面一切正常，回退时才发现快照丢了。
6. **改坏别人的工作区改动**：仓库里可能混有他人未提交改动，考验 agent 是否守「不丢任何一行别人改动」的红线。

---

## 5. 标准答案对照

| 任务 | 对照 commit | 关键文件 |
|------|-------------|----------|
| Level 1（判官卡死） | `1f96407` | `crates/agent-core/src/dispatch.rs`（`judge_decision_timeout` + `HangingJudge` 测试） |
| Level 2（10 组审计） | `1f96407` | `apps/hebcore/src/main.rs`、`apps/desktop/src/hebcore_client.rs`、`apps/cli/src/daemon.rs`、`crates/agent-core/src/{edits/mod.rs,session_hub.rs,storage/*}`、`crates/surface-session/src/lib.rs`、前端 `liveTimelineOrder.ts` |
| Level 3（3 个留尾巴） | `f1a422f` | `crates/agent-core/src/{run_persister.rs,storage/sessions.rs,storage/sessions_dir.rs}` |

> 基线：`git checkout template-core-reflactor`。
> 标准答案 diff：`git diff template-core-reflactor..f1a422f`（含 Level 1+2+3；注意 `158c6ea`/`ccf4ea1` 是后续无关功能，不在本题范围）。

---

## 6. 给评测实施者的建议

- **分级发题**：先只给 Level 1 的用户反馈，看 agent 能否自主复现+根因+回归测试；再放开 Level 2「主动审计」；最后给 Level 3 的留尾巴。三级独立计分。
- **重诊断、轻产出量**：本题不看改了多少行，看「有没有找到根因、修法是否彻底、有没有自欺」。一个只改 5 行但改在刀刃上的修复，远胜于改 200 行但没碰根因。
- **观察过程**：好的 agent 会先读架构文档建立心智模型、按子系统维度系统化审查、对每条发现做对抗性证伪——而不是一头扎进代码 grep。过程本身就是区分度。
- **诚实性是硬指标**：声称「全部修复，测试通过」但实际有 flaky/没验证现象级复现的，直接降档——这正是本题最想筛掉的失败模式。
