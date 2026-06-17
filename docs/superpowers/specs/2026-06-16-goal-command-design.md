# `//goal` 命令设计 — 不达目标不停的会话目标

> 日期：2026-06-16
> 状态：设计已确认，待出实现计划
> 参考来源：Claude Code 2.1.177 native binary 逆向（Stop-hook + LLM judge 机制）；codex `/goal`（持久化目标引擎，对照取舍）

---

## 1. 目标与动机

给一个会话挂一个「完成条件」，模型每次想结束 turn（end_turn 且无 pending tool_call）时，在 Stop 点跑一次 judge LLM 看对话历史是否满足条件：

- **没满足** → 把「还差什么」注入下一轮 user message，逼模型继续干
- **满足** → 清除目标、放行出 turn
- **判定不可能 / turn 出错 / 用户 cancel** → 三道熔断，停止续跑

用户痛点：长任务（如「把所有测试跑绿并提 PR」）现在要人盯着，每轮手动催「继续」。`//goal` 让会话在无人值守时自我推进到目标达成。

### 1.1 与参考实现的关系

| | Claude Code `/goal` | codex `/goal` | hebbian `//goal`（本设计） |
|---|---|---|---|
| 本质 | Stop-hook + 独立 LLM judge 裁决 | 持久化目标状态机（独立 crate + sqlite） | Stop 点位 + judge LLM，复用现有回路 |
| 完成判定 | judge 读 transcript 找证据 | 模型自调 `update_goal(complete)` + audit prompt | judge 读 transcript 找证据（同 CC） |
| 状态 | session 内（trusted workspace） | sqlite 6 态机 | session meta，两态（有/无目标） |
| 防失控 | judge 判 impossible 兜底 | error→Blocked 熔断 + token 预算 | impossible / turn 出错 / cancel 三道熔断 |

选 CC 路线而非 codex：hebbian 已有 Stop 注入回路（`agent_loop.rs:949`）和 judge 范式（`automode.rs`），CC 式实现几乎是把现成三块拼起来，无需 codex 那种重型状态机 crate。judge 用独立 context 读 transcript 判定，比让主模型自评（codex 路线）更可靠。

---

## 2. 决策汇总（已与用户确认）

| 项 | 决定 | 理由 |
|---|---|---|
| 裁决点位 | 复用现有 Stop 点位 | 改动最小，时机正是「模型想停时」 |
| 防失控 | 无迭代/token 上限 + 三道熔断 | 长跑目标不该被固定次数砍掉；熔断防无限烧 token |
| 持久化 | 写 session meta，跨重启保留 | 真·长跑，关 App 再开目标还在 |
| judge 模型 | 始终用会话主 client + 主模型 | 不引入额外配置；目标裁决质量比成本重要 |
| 命令前缀 | `//goal <条件>` / `//goal clear` | 遵循 hebbian §8 双斜杠内置命令约定 |
| goal vs 外部 Stop hook 顺序 | 外部 Stop hook 先，goal 裁决后 | 先让 cargo check 等 verify 修干净（有明确对错），再判整体目标达成，省一次 judge 调用 |

---

## 3. 架构落点（架构.md 对应章节）

横跨三节，每节都需改：

- **§4.8.3**（Stop hook 的 InjectFollowup 协议）：goal 裁决挂在 Stop 自然结束分支，**在外部 Stop hook 之后**跑。**关键解耦**：现有 `MAX_STOP_INJECTIONS = 3` 是所有 Stop 注入共享的硬上限；goal 续跑必须**不受此上限约束**（否则 3 轮就被砍）。goal 注入走独立计数器（仅记录、不设上限），外部 verify hook 注入仍守 3 次上限。两者都用同一个 `transcript.push_user(...) + continue` 机制。
- **§8.2 表 A**（内置控制命令）：新增 `//goal` 一行，参数 `[<条件> | clear | （空=查看状态）]`
- **§4.9**（会话 meta）：新增 `active_goal` 字段，append `MetaUpdate` 落盘，`load` 时 fold 回内存

---

## 4. 数据模型

session meta 里持久化（架构 §4.9 MetaUpdate 机制）：

```rust
struct ActiveGoal {
    condition: String,            // 用户设的完成条件原文
    created_at: i64,
    iterations: u32,              // 已自动续跑轮数（展示/日志用，不做上限）
    last_reason: Option<String>,  // judge 上次判定「还差什么」
}
```

比 codex 6 态机轻得多：只有「有目标 / 无目标」两态，paused/blocked/budget 这些态全不需要——状态由 judge 每轮现算。

---

## 5. 控制流（核心）

在 `agent_loop.rs` 的 Stop 自然结束分支（end_turn 且无 pending tool_call）：

```
模型 end_turn
  ├─ drain_pending_inputs > 0 → 用户插了新消息，不裁决，续跑（现有逻辑）
  ├─ 跑外部 Stop hook（现有逻辑，守 MAX_STOP_INJECTIONS=3）
  │    InjectFollowup → 注入 <hook-feedback>、续跑
  └─ 外部 hook 放行后，有 active_goal？
       否 → 正常出 turn（现状）
       是 → 跑 goal judge（一次 judge_client.complete，用主 client+主模型）
            ├─ Achieved        → 清除 goal、emit GoalAchieved、正常出 turn
            ├─ Impossible(why) → 清除 goal、emit GoalImpossible(why)、出 turn 【熔断1】
            └─ NotYet(reason)  → iterations+=1、写 last_reason 落盘、
                                 注入 <goal-feedback>reason</goal-feedback>、续跑（无上限）

熔断2：turn 出错（非可重试 / 压缩失败）→ 不裁决，保留 goal 但停止本 run 续跑，emit 提示
熔断3：用户 cancel → CancelFlag 已覆盖 judge 调用与主 loop，立即停
```

goal 注入文本走与现有 Stop hook 完全相同的 `transcript.push_user(...)` + `continue` 路径，**不新增 loop 控制结构**。

---

## 6. judge 模块（复用 automode 范式）

新增 `crates/agent-core/src/goal.rs`，仿 `automode.rs` 结构：

- `GOAL_JUDGE_SYSTEM`：新 prompt 文件 `crates/agent-core/prompts/goal_judge.md`，照搬 CC judge prompt 精神：
  - 读 transcript，判断用户条件是否满足
  - 返回 JSON：`{"ok": true, "reason": "<引用 transcript 证据>"}` / `{"ok": false, "reason": "<还差什么>"}` / `{"ok": false, "impossible": true, "reason": "<为何永远达不成>"}`
  - 必须引用 transcript 原文；无清晰证据 → `{"ok": false, "reason": "insufficient evidence"}`
  - **谨慎判 impossible**：模型自称「做不到」只是证据不是证明；存疑返回 `{"ok": false}` 不带 impossible
- `judge_goal(client, model, condition, recent_transcript, cancel) -> GoalVerdict`
- `GoalVerdict::{ Achieved(String), Impossible(String), NotYet(String) }`，解析模型返回 JSON；解析失败 fail-safe 为 `NotYet`（继续干，绝不误判达成）

设目标时注入主模型的指令（照搬 CC，作为 system-reminder）：「把条件本身当指令，别停下来问用户，hook 会拦着不让你停，达成后自动清除；成功后不要让用户去 `//goal clear`，那只用于提前清除」。

---

## 7. 命令链路（§8 内置控制命令）

`//goal` 是内置控制命令（改 agent-core 状态），**不是** skill：

- `//goal <条件>` → 前端拦截 → Tauri `set_active_goal(session_id, condition)` → 写 meta + 注入「目标已设」system-reminder → toast 回显
- `//goal clear` → Tauri `clear_active_goal(session_id)` → 清 meta → toast
- 裸 `//goal` → Tauri `get_active_goal(session_id)` → toast 显示当前条件 / 已续跑轮数 / 上次判定

遵循 §8 fail-closed：参数为空且无 active goal → toast「当前没有目标，用 //goal <条件> 设一个」。

---

## 8. UI 事件（协议）

新增 EventPayload（架构 §3 通信契约 + chat.rs 翻译 + 前端 types.ts）：

- `GoalAchieved { condition }` — 目标达成，绿色提示
- `GoalImpossible { condition, reason }` — judge 判不可达，橙色提示带原因
- `GoalProgress { iteration, reason }` — 每次续跑，让用户看到「第 N 轮，还差 X」

前端 MessageBubble / 状态条渲染一个「目标进行中」指示器，显示条件 + 当前轮数。

---

## 9. 测试

- 单测 `goal::tests`：judge JSON 解析三种 verdict（含 impossible 字段）、解析失败 fail-safe 为 NotYet
- 单测 agent_loop：
  - mock judge 返回 NotYet → 验证注入 `<goal-feedback>` 且 loop 不退出
  - mock judge 返回 Achieved → 验证 goal 清除 + 正常出 turn
  - 验证 goal 续跑不受 MAX_STOP_INJECTIONS=3 限制（注入 >3 次仍继续）
- 回归（heb CLI A/B，遵循 CLAUDE.md 修 bug 流程）：
  - 设条件「创建 /tmp/goal_done.txt」
  - 复现：模型不创建 → 事件流应看到 GoalProgress 反复续跑
  - 验证：创建后 → judge 放行，事件流出现 GoalAchieved，goal 清除

---

## 10. 影响范围与兼容

- **agent-core**：新增 `goal.rs` + `prompts/goal_judge.md`；改 `agent_loop.rs` Stop 分支；meta 加 `active_goal`
- **协议**：新增 3 个 EventPayload（additive，旧客户端忽略未知 event）
- **desktop**：新增 3 个 Tauri command（`set/clear/get_active_goal`）+ chat.rs 翻译 + 前端命令注册 + 渲染
- **storage**：meta 加字段，向后兼容（老 jsonl 无 active_goal 字段 → None）
- **不破坏兼容**：纯 additive，无目标的会话行为与现状字节级一致

---

## 11. 留尾巴 / 已知风险

- judge 每轮一次额外 LLM 调用，长跑目标累积 token 成本——由「无人值守自动推进」的价值覆盖，且三道熔断防失控
- 「慢但有进展」任务理论上可能被 judge 误判 impossible——靠 prompt 里「谨慎判 impossible、存疑返回 ok:false」缓解；未加「无进展软熔断」（用户已确认不要）
- heb CLI / hebweb surface 的 `//goal` 命令注册需对称实现（三 surface 共享 agent-core，但命令拦截在各 surface 前端）——首版可只做 Desktop，CLI 用直接 IPC 设 goal 验证
