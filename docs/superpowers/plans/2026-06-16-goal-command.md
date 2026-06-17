# `//goal` 命令实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给会话挂一个完成条件，模型每次想结束 turn 时由 judge LLM 判 transcript 是否达成，没达成就自动注入「还差什么」续跑，达成/判不可能/出错/cancel 时停。

**Architecture:** 复用 hebbian 现有三块——Stop 注入回路（`agent_loop.rs`）、AutoMode judge 范式（`automode.rs`）、`active_plan` 风格的可清空 meta 字段（`storage/sessions.rs`）。goal 裁决排在外部 Stop hook 之后；goal 续跑用独立计数器，不受 `MAX_STOP_INJECTIONS=3` 约束（无上限）。

**Tech Stack:** Rust（agent-core / protocol crate）、Tauri command（desktop 后端）、TypeScript/React（desktop 前端）。

**设计依据：** `docs/superpowers/specs/2026-06-16-goal-command-design.md`

---

## 文件结构

**新建：**
- `crates/agent-core/src/goal.rs` — goal judge 模块（仿 `automode.rs`）：`GoalVerdict` 枚举、`judge_goal()`、JSON 解析、judge prompt 格式化
- `crates/agent-core/prompts/goal_judge.md` — judge 的 system prompt（编译进二进制）

**修改：**
- `crates/agent-core/src/storage/sessions.rs` — `Session` / `SessionMeta` / `MetaUpdate` 加 `active_goal` 字段（仿 `active_plan`）；新增 `set_active_goal()` setter
- `crates/agent-core/src/agent_loop.rs` — Stop 自然结束分支加 goal 裁决；`LoopParams` 已有 `judge_client`，复用
- `crates/agent-core/src/lib.rs` — 注册 `pub mod goal;`
- `crates/protocol/src/event.rs` — 新增 3 个 EventPayload variant
- `apps/desktop/src/chat.rs` — 3 个 Tauri command + EventPayload 翻译
- `apps/desktop/src/lib.rs`（或 command 注册处）— 注册 3 个 command
- `apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts` — 注册 `//goal` 内置命令
- `apps/desktop/frontend/src/desktop/bridge/tauri.ts` — 3 个 invoke 绑定

---

## Task 1: meta 加 `active_goal` 字段（持久化地基）

**Files:**
- Modify: `crates/agent-core/src/storage/sessions.rs`
- Test: 同文件 `#[cfg(test)] mod tests`

照搬 `active_plan` 的完整范式（Session 字段 + SessionMeta 字段 + MetaUpdate 字段+clear bool + fold 两处 + setter）。`active_goal` 的值是一个结构体 `ActiveGoal { condition, created_at, iterations, last_reason }`。

- [ ] **Step 1: 定义 `ActiveGoal` 结构体**

在 `sessions.rs` 顶部其它结构体附近（如 `PendingContinue` 定义旁）加：

```rust
/// 会话当前的「完成条件」目标（架构 §4.8.3 / §8）。模型每次想结束 turn 时
/// 由 judge LLM 判 transcript 是否满足 `condition`，没满足就注入续跑。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveGoal {
    /// 用户用 `//goal <条件>` 设的完成条件原文。
    pub condition: String,
    /// 设目标的时间戳（ms）。
    pub created_at: i64,
    /// 已自动续跑轮数（展示 / 日志用，不做上限）。
    pub iterations: u32,
    /// judge 上次判定「还差什么」；首次设目标时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
}
```

- [ ] **Step 2: `Session` 结构体加字段**

找到 `Session` 结构体里 `pub active_plan: Option<String>,`（约 271 行），在其后加：

```rust
    /// 会话当前的完成条件目标（架构 §4.8.3）。None = 无目标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_goal: Option<ActiveGoal>,
```

- [ ] **Step 3: `RolloutMeta`（SessionMeta）加字段**

找到 `RolloutMeta` 里 `pub active_plan: Option<String>,`（约 506 行），其后加：

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_goal: Option<ActiveGoal>,
```

- [ ] **Step 4: `MetaUpdate` 加字段 + clear bool**

找到 `MetaUpdate` 里 `pub active_plan: Option<String>,` + `pub clear_active_plan: bool,`（约 565/568 行），其后加：

```rust
    /// 设置 / 更新会话目标。`None` = 本次更新不动；要清空走 `clear_active_goal`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_goal: Option<ActiveGoal>,
    /// 显式清空 `active_goal`（达成 / 判不可能 / 用户 //goal clear）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_active_goal: bool,
```

- [ ] **Step 5: fold 两处加映射**

在 `default_meta_from_session`（约 851 行 `active_plan: s.active_plan.clone(),`）后加：
```rust
        active_goal: s.active_goal.clone(),
```

在 `apply_meta`（约 879 行 `s.active_plan = m.active_plan;`）后加：
```rust
    s.active_goal = m.active_goal;
```

在 `apply_update`（约 944-949 行 active_plan 的 clear/set 块）后，照同样「先 clear 再 set」模式加：
```rust
    if u.clear_active_goal {
        s.active_goal = None;
    }
    if let Some(v) = u.active_goal {
        s.active_goal = Some(v);
    }
```

- [ ] **Step 6: 补齐所有 `Session { ... active_plan: None, ... }` 构造点**

`active_plan: None,` 在 sessions.rs 出现多处构造（约 1016 / 1212 / 1782 / 2839 行）。每一处 `active_plan: None,` 后补 `active_goal: None,`。

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo check -p agent-core 2>&1 | grep -E "missing field|error" | head`
Expected: 无 `missing field \`active_goal\`` 报错（全部构造点已补齐）

- [ ] **Step 7: 新增 `set_active_goal` setter**

在 `set_active_plan`（约 1996 行）后加（仿其结构）：

```rust
/// 设置 / 清空会话目标（架构 §4.8.3 / §8）。
/// `Some(goal)` 写入或覆盖；`None` 清空。沿用 [`set_active_plan`] 的 append-only 模式。
pub fn set_active_goal(
    data_dir: &Path,
    id: &str,
    goal: Option<ActiveGoal>,
) -> AppResult<Session> {
    let path = ensure_jsonl(data_dir, id)?;
    let (set, clear) = match goal {
        Some(g) => (Some(g), false),
        None => (None, true),
    };
    append_line(
        &path,
        &RolloutLine::MetaUpdate(MetaUpdate {
            at: now(),
            active_goal: set,
            clear_active_goal: clear,
            ..Default::default()
        }),
    )?;
    load(data_dir, id)
}
```

- [ ] **Step 8: 写回归测试**

在 sessions.rs 的 `#[cfg(test)] mod tests` 里加：

```rust
#[test]
fn active_goal_set_clear_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let dd = tmp.path();
    let s = create(dd, /* 用本文件其它测试创建 session 的同款入参 */).unwrap();
    let id = s.id.clone();

    // 设目标
    let goal = ActiveGoal {
        condition: "所有测试通过".to_string(),
        created_at: 1,
        iterations: 0,
        last_reason: None,
    };
    let s = set_active_goal(dd, &id, Some(goal.clone())).unwrap();
    assert_eq!(s.active_goal.as_ref().unwrap().condition, "所有测试通过");

    // load 后仍在（跨「重启」）
    let s2 = load(dd, &id).unwrap();
    assert_eq!(s2.active_goal, Some(goal));

    // 清空
    let s3 = set_active_goal(dd, &id, None).unwrap();
    assert_eq!(s3.active_goal, None);
    assert_eq!(load(dd, &id).unwrap().active_goal, None);
}
```

> 注：`create()` 的入参照抄本文件其它已有测试（如测 `set_active_plan` 的那个，搜 `set_active_plan` 在 tests 里的用法；若无则照 `create` 签名构造最小 session）。

- [ ] **Step 9: 跑测试**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo test -p agent-core --lib active_goal_set_clear_roundtrip 2>&1 | tail -15`
Expected: PASS

- [ ] **Step 10: Commit**

```bash
cd /Users/ricardo/code/ricardo/rust/hebbian
git add crates/agent-core/src/storage/sessions.rs
git commit -m "$(cat <<'EOF'
新增 session meta 的 active_goal 字段（//goal 持久化地基）

- Why: //goal 命令需把完成条件落盘跨重启，仿 active_plan 的可清空 Option 范式
- 影响范围: agent-core storage，纯 additive，老 jsonl 无字段反序列化为 None
- 留尾巴: 无
EOF
)"
```

---

## Task 2: goal judge 模块（裁决核心）

**Files:**
- Create: `crates/agent-core/src/goal.rs`
- Create: `crates/agent-core/prompts/goal_judge.md`
- Modify: `crates/agent-core/src/lib.rs`（注册 `pub mod goal;`）
- Test: `goal.rs` 内 `#[cfg(test)] mod tests`

- [ ] **Step 1: 写 judge system prompt**

创建 `crates/agent-core/prompts/goal_judge.md`：

```markdown
你在评估 Claude Code 的一个「停止条件」。仔细读对话记录（transcript），判断用户给定的完成条件是否已经满足。

只输出一行 JSON，三选一：
- `{"ok": true, "reason": "<引用 transcript 里证明条件已满足的具体内容>"}`
- `{"ok": false, "reason": "<还差什么 / 什么阻塞了条件>"}`
- `{"ok": false, "impossible": true, "reason": "<为什么这个条件在本会话里永远无法满足>"}`

规则：
- 必须带 reason，尽量引用 transcript 原文作为证据。
- 如果 transcript 里没有清晰证据证明条件已满足，返回 `{"ok": false, "reason": "transcript 里证据不足"}`。
- 只有当条件**确实无法达成**时才用 `impossible: true`。助手自己声称「做不到」只是证据、不是证明——要独立确认，不要因为「还没达成」或「进度慢」就判 impossible。拿不准时返回 `{"ok": false}`，不带 impossible。
- 不要输出 JSON 以外的任何文字。
```

- [ ] **Step 2: 写 `goal.rs` 骨架 + GoalVerdict + 解析（先写测试）**

创建 `crates/agent-core/src/goal.rs`，先写解析测试（TDD）：

```rust
//! `//goal` 命令的 judge：模型想结束 turn 时，判 transcript 是否满足用户设的完成条件。
//!
//! 架构 §4.8.3 / §8。复用 [`crate::automode`] 的 judge 调用范式，但用会话主 client+主模型。

use std::sync::Arc;

use serde::Deserialize;
use tracing::warn;

use model_gateway::client::ModelClient;
use model_gateway::types::{ModelError, ModelRequest, ModelResponse, TranscriptEntry, UserEntry};

/// goal judge 的 system prompt（编译进二进制，跨会话稳定）。
pub const GOAL_JUDGE_SYSTEM: &str = include_str!("../prompts/goal_judge.md");

/// judge 裁决结果。
#[derive(Debug, Clone, PartialEq)]
pub enum GoalVerdict {
    /// 条件已满足，附证据。
    Achieved(String),
    /// 条件永远无法满足，附原因。
    Impossible(String),
    /// 尚未满足，附「还差什么」——注入续跑。
    NotYet(String),
}

/// judge 返回的 JSON 形态。
#[derive(Debug, Deserialize)]
struct RawVerdict {
    ok: bool,
    #[serde(default)]
    impossible: bool,
    #[serde(default)]
    reason: String,
}

/// 解析 judge 模型返回的文本为 [`GoalVerdict`]。
/// 解析失败 fail-safe 为 `NotYet`——绝不误判达成，宁可多续跑一轮。
fn parse_verdict(raw: &str) -> GoalVerdict {
    // 容错：从文本里抠出第一个 {...} JSON 片段（judge 可能裹了多余文字）。
    let json_slice = raw
        .find('{')
        .and_then(|start| raw.rfind('}').map(|end| &raw[start..=end]));
    let Some(slice) = json_slice else {
        return GoalVerdict::NotYet(format!("judge 返回无法解析：{}", trim(raw, 120)));
    };
    match serde_json::from_str::<RawVerdict>(slice) {
        Ok(v) if v.ok => GoalVerdict::Achieved(v.reason),
        Ok(v) if v.impossible => GoalVerdict::Impossible(v.reason),
        Ok(v) => GoalVerdict::NotYet(v.reason),
        Err(e) => GoalVerdict::NotYet(format!("judge JSON 解析失败：{e}")),
    }
}

fn trim(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_achieved() {
        let v = parse_verdict(r#"{"ok": true, "reason": "测试全绿见 tool_result"}"#);
        assert_eq!(v, GoalVerdict::Achieved("测试全绿见 tool_result".into()));
    }

    #[test]
    fn parse_not_yet() {
        let v = parse_verdict(r#"{"ok": false, "reason": "还有 2 个测试失败"}"#);
        assert_eq!(v, GoalVerdict::NotYet("还有 2 个测试失败".into()));
    }

    #[test]
    fn parse_impossible() {
        let v = parse_verdict(r#"{"ok": false, "impossible": true, "reason": "依赖的外部 API 已下线"}"#);
        assert_eq!(v, GoalVerdict::Impossible("依赖的外部 API 已下线".into()));
    }

    #[test]
    fn parse_garbage_falls_back_to_not_yet() {
        assert!(matches!(parse_verdict("我觉得差不多了"), GoalVerdict::NotYet(_)));
        assert!(matches!(parse_verdict(""), GoalVerdict::NotYet(_)));
    }

    #[test]
    fn parse_json_wrapped_in_prose() {
        // judge 裹了多余文字也能抠出 JSON
        let v = parse_verdict("分析后：\n{\"ok\": true, \"reason\": \"done\"}\n以上");
        assert_eq!(v, GoalVerdict::Achieved("done".into()));
    }
}
```

- [ ] **Step 3: 注册模块**

在 `crates/agent-core/src/lib.rs` 里，`pub mod automode;` 附近加一行：
```rust
pub mod goal;
```

- [ ] **Step 4: 跑解析测试**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo test -p agent-core --lib goal:: 2>&1 | tail -15`
Expected: 5 个 parse 测试全 PASS

- [ ] **Step 5: 加 `judge_goal()` 调用函数**

在 `goal.rs` 的 `parse_verdict` 后、`mod tests` 前加：

```rust
/// 调一次模型作为 goal judge（架构 §4.8.3）。
///
/// 用会话主 client + 主模型（与 AutoMode 的专属 judge 不同——goal 裁决质量比成本重要，
/// 且不引入额外配置）。`recent_transcript` 传最近若干轮，judge 据此找完成证据。
pub async fn judge_goal(
    client: &Arc<dyn ModelClient>,
    model: &str,
    condition: &str,
    recent_transcript: &[TranscriptEntry],
    cancel: common::CancelFlag,
) -> GoalVerdict {
    let prompt = format_judge_prompt(condition, recent_transcript);
    let request = ModelRequest {
        model: model.to_string(),
        system: Some(GOAL_JUDGE_SYSTEM.to_string()),
        entries: vec![TranscriptEntry::User(UserEntry::text(prompt))],
        tools: Vec::new(),
        max_tokens: 400,
        reasoning: None,
    };
    match client.complete(request, cancel).await {
        Ok(resp) => parse_verdict(&extract_text(&resp)),
        // judge 调用本身失败 / 被取消 → fail-safe NotYet（不误判达成，也不熔断）。
        // 真正的 cancel 由 agent_loop 主 loop 的 CancelFlag 兜底停止续跑。
        Err(ModelError::Cancelled) => GoalVerdict::NotYet("goal judge 被取消".into()),
        Err(err) => {
            warn!(%err, "goal judge 调用失败，本轮按未达成处理");
            GoalVerdict::NotYet(format!("goal judge 调用失败：{err}"))
        }
    }
}

fn extract_text(resp: &ModelResponse) -> String {
    match resp {
        ModelResponse::Done { text, .. } | ModelResponse::ToolCalls { text, .. } => text.clone(),
    }
}

fn format_judge_prompt(condition: &str, recent_transcript: &[TranscriptEntry]) -> String {
    let recent: Vec<String> = recent_transcript
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(summarize_entry)
        .collect();
    format!(
        "完成条件（用户设定）：\n{condition}\n\n\
         对话记录（旧→新）：\n{}\n\n\
         按 system prompt 的格式输出一行 JSON。",
        recent.join("\n")
    )
}

fn summarize_entry(entry: &TranscriptEntry) -> String {
    match entry {
        TranscriptEntry::User(u) => format!("- user: {}", trim(&u.text, 300)),
        TranscriptEntry::Assistant(a) => format!("- assistant: {}", trim(&a.text, 300)),
        TranscriptEntry::ToolResults(results) => {
            let s: Vec<String> = results
                .iter()
                .map(|t| format!("{}={}", t.name, trim(&t.content, 120)))
                .collect();
            format!("- tool_results: {}", s.join(" / "))
        }
    }
}
```

> 注：`summarize_entry` / `trim` 与 `automode.rs` 里同名函数逻辑一致但本模块自带一份（两模块解耦，不互相 use 私有函数）。`TranscriptEntry` 的变体名（`User` / `Assistant` / `ToolResults`）与 `automode.rs:377` 的 `summarize_entry` 完全一致，照抄即可。

- [ ] **Step 6: 跑编译 + 全模块测试**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo test -p agent-core --lib goal:: 2>&1 | tail -15`
Expected: 全 PASS（解析测试不受影响，新增函数编译通过）

- [ ] **Step 7: Commit**

```bash
cd /Users/ricardo/code/ricardo/rust/hebbian
git add crates/agent-core/src/goal.rs crates/agent-core/prompts/goal_judge.md crates/agent-core/src/lib.rs
git commit -m "$(cat <<'EOF'
新增 goal judge 模块（//goal 裁决核心）

- Why: //goal 需在模型想停时判 transcript 是否满足完成条件，仿 automode judge 范式但用主模型
- 改动: goal.rs（GoalVerdict + judge_goal + JSON 解析 fail-safe NotYet）+ goal_judge.md prompt
- 影响范围: agent-core，新增模块不影响现有路径
- 留尾巴: 尚未接入 agent_loop（Task 3）
EOF
)"
```

---

## Task 3: agent_loop Stop 分支接入 goal 裁决

**Files:**
- Modify: `crates/agent-core/src/agent_loop.rs`
- Test: `agent_loop.rs` 内 `#[cfg(test)] mod tests`

接入点在 `agent_loop.rs:949` 的外部 Stop hook 块**之后**、`break Ok(AssistantOutput {...})`（约 979 行）**之前**。

- [ ] **Step 1: 加 goal 续跑计数器**

在 `let mut stop_hook_injections: u32 = 0;`（约 471 行）后加：

```rust
    // goal 续跑次数。与 stop_hook_injections 解耦——goal 是「不达目标不停」，
    // 无上限（架构 §4.8.3）；防失控靠 judge 判 impossible / turn 出错 / cancel 三道熔断。
    let mut goal_iterations: u32 = 0;
```

- [ ] **Step 2: 在外部 Stop hook 块后插入 goal 裁决**

在外部 Stop hook 的 `if !hooks.is_empty() && stop_hook_injections < MAX_STOP_INJECTIONS { ... }` 块（949-978 行）结束后，`break Ok(...)` 前插入：

```rust
                // 架构 §4.8.3：外部 Stop hook（cargo check 等 verify）放行后，
                // 若会话挂了 //goal 目标，跑 judge 判 transcript 是否满足完成条件。
                // judge 用会话主 client+主模型（judge_client 在 AutoMode 未配置时可能为
                // None，此时 goal 无法裁决——保留目标但本 run 不再自动续跑，避免静默放行）。
                if let (Some(dd), Some(sid)) = (data_dir.as_ref(), session_id.as_deref()) {
                    if let Ok(sess) = crate::storage::sessions::load(dd, sid) {
                        if let Some(goal) = sess.active_goal.clone() {
                            match judge_client.as_ref() {
                                None => {
                                    tracing::warn!(
                                        "active_goal 存在但 judge_client 未配置，无法裁决，本 run 不续跑"
                                    );
                                }
                                Some(jc) => {
                                    let model = model_id.clone().unwrap_or_default();
                                    let verdict = crate::goal::judge_goal(
                                        jc,
                                        &model,
                                        &goal.condition,
                                        transcript.entries(),
                                        cancel.clone(),
                                    )
                                    .await;
                                    match verdict {
                                        crate::goal::GoalVerdict::Achieved(reason) => {
                                            let _ = crate::storage::sessions::set_active_goal(
                                                dd, sid, None,
                                            );
                                            emit(EventPayload::GoalAchieved {
                                                condition: goal.condition.clone(),
                                                reason,
                                            });
                                            // 目标达成 → 正常出 turn（落到下方 break）。
                                        }
                                        crate::goal::GoalVerdict::Impossible(reason) => {
                                            let _ = crate::storage::sessions::set_active_goal(
                                                dd, sid, None,
                                            );
                                            emit(EventPayload::GoalImpossible {
                                                condition: goal.condition.clone(),
                                                reason,
                                            });
                                            // 熔断1：判不可能 → 清目标、正常出 turn。
                                        }
                                        crate::goal::GoalVerdict::NotYet(reason) => {
                                            goal_iterations += 1;
                                            // 落盘更新 iterations + last_reason（跨重启可见）。
                                            let updated = crate::storage::sessions::ActiveGoal {
                                                condition: goal.condition.clone(),
                                                created_at: goal.created_at,
                                                iterations: goal_iterations,
                                                last_reason: Some(reason.clone()),
                                            };
                                            let _ = crate::storage::sessions::set_active_goal(
                                                dd,
                                                sid,
                                                Some(updated),
                                            );
                                            emit(EventPayload::GoalProgress {
                                                iteration: goal_iterations,
                                                reason: reason.clone(),
                                            });
                                            let wrapped = format!(
                                                "[SYSTEM NOTIFICATION - NOT USER INPUT]\n<goal-feedback>\n目标尚未达成。{reason}\n继续推进，达成后会自动结束。\n</goal-feedback>"
                                            );
                                            transcript.push_user(wrapped, Vec::new());
                                            set_pending_inputs_accepting(
                                                pending_inputs_accepting.as_ref(),
                                                true,
                                            );
                                            output_attachments = all_attachments;
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
```

> 说明：`transcript.entries()` 取全部历史给 judge——若该方法不存在，用现有取 transcript 条目的 API（搜 `transcript.entries` 或 `transcript.iter` 确认方法名；automode 调用方传的 `recent_transcript` 同源，照它的取法）。`all_attachments` 在此作用域已定义（Stop 分支顶部，约 929 行）。

- [ ] **Step 3: 处理熔断2（turn 出错时不裁决，保留目标）**

goal 裁决只在「模型 end_turn 自然结束」分支跑（上面插入处）。turn 出错走的是另一条 `break Err(...)` 路径，天然不会触发 goal 裁决——目标保留在 meta 里，本 run 结束。**无需额外代码**，但要确认：turn error 路径不会误清 goal。

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && grep -n "set_active_goal" crates/agent-core/src/agent_loop.rs`
Expected: 只在 Step 2 插入处出现（Achieved / Impossible 清空 + NotYet 更新），错误路径无 set_active_goal 调用

- [ ] **Step 4: 编译**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo check -p agent-core 2>&1 | grep -E "error" | head`
Expected: 无 error（EventPayload 的 Goal* variant 在 Task 4 才加——此处会报「no variant GoalAchieved」，**先做 Task 4 再回来编译**，或把 Task 4 提到本步前）

> **执行顺序提示**：Task 4（EventPayload）与本 Task 有编译依赖。建议先做 Task 4 的 Step 1-2（加 variant），再回来编译 Task 3。

- [ ] **Step 5: 写 agent_loop 集成测试**

参照 `agent_loop.rs` 现有测试（搜 `mod tests` 里 mock ModelClient 的用法，如 `PendingInputAfterTurnFinishedClient`）。新增一个 mock judge client 返回固定 verdict 的测试：

```rust
#[tokio::test]
async fn goal_not_yet_injects_followup_and_continues() {
    // 构造：active_goal 已设；主模型第一轮 end_turn；judge 返回 NotYet；
    // 断言：transcript 末尾出现 <goal-feedback>，且 loop 进入了第二轮（未直接退出）。
    // mock judge client 的 complete() 返回 {"ok": false, "reason": "还差 X"}。
    // mock 主 client 第二轮返回 end_turn 且 judge 第二次返回 {"ok": true} → 退出。
    // 用本文件已有的 run_loop 测试入口 + tempdir data_dir（active_goal 需落盘读取）。
    // 断言 transcript 含 "goal-feedback"，且最终 active_goal 被清空。
}
```

> 该测试需要 data_dir（goal 读写走 storage）。照搬本文件里「带 data_dir 的 run_loop 测试」的脚手架；若现有测试都不带 data_dir，则此集成测试改为：直接单测 Stop 分支抽出的纯函数（见下方备选），或留到 Task 7 的 heb CLI A/B 验证。**优先尝试集成测试；若脚手架成本过高，在本步注释说明改由 Task 7 端到端验证，不写半吊子测试。**

- [ ] **Step 6: 跑测试 + 全量 check**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo test -p agent-core --lib 2>&1 | tail -20`
Expected: 全 PASS（含新增 goal 集成测试或确认其改由 Task 7 验证）

- [ ] **Step 7: Commit**

```bash
cd /Users/ricardo/code/ricardo/rust/hebbian
git add crates/agent-core/src/agent_loop.rs
git commit -m "$(cat <<'EOF'
agent_loop Stop 分支接入 goal 裁决（//goal 续跑闭环）

- Why: 模型 end_turn 时若挂了 goal，跑 judge 判达成；NotYet 注入续跑、Achieved/Impossible 清目标
- 改动: 外部 Stop hook 后插入 goal 裁决；goal_iterations 独立计数不受 MAX_STOP_INJECTIONS 约束
- 影响范围: agent-core 主 loop。无 goal 会话行为字节级不变；judge_client 为 None 时保留目标不续跑
- 留尾巴: turn 出错=熔断2（保留目标本 run 停）；EventPayload variant 依赖 Task 4
EOF
)"
```

---

## Task 4: 新增 3 个 EventPayload variant

**Files:**
- Modify: `crates/protocol/src/event.rs`
- Modify: `apps/desktop/src/chat.rs`（翻译）

- [ ] **Step 1: 加 3 个 variant**

在 `crates/protocol/src/event.rs` 的 `CronFired` variant（约 418 行）附近加：

```rust
    /// `//goal` 目标达成（judge 判 ok:true）。
    GoalAchieved {
        condition: String,
        reason: String,
    },
    /// `//goal` 目标被 judge 判定无法达成（熔断1）。
    GoalImpossible {
        condition: String,
        reason: String,
    },
    /// `//goal` 一次自动续跑（judge 判 NotYet，注入续跑前 emit）。
    GoalProgress {
        iteration: u32,
        reason: String,
    },
```

- [ ] **Step 2: 编译 protocol**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo check -p protocol 2>&1 | grep error | head`
Expected: 无 error

- [ ] **Step 3: chat.rs 翻译这 3 个事件**

在 `apps/desktop/src/chat.rs` 里找到 EventPayload → 前端事件的翻译 match（搜 `EventPayload::CronFired` 或 `EventPayload::RunModeChanged`，照其范式）。为 3 个新 variant 加翻译分支，emit 到前端对应事件名（照现有 `emit_event` / `app_handle.emit` 范式，事件名如 `goal-achieved` / `goal-impossible` / `goal-progress`）。

> 具体翻译目标取决于 chat.rs 现有事件分发结构。照抄 `RunModeChanged` 的翻译写法——它同样是「后端状态变更 → 前端 toast/状态条」类事件。

- [ ] **Step 4: 编译 desktop 后端**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo check -p hebbian-desktop 2>&1 | grep -E "error|non-exhaustive" | head`
Expected: 无 error（match 已覆盖新 variant）

- [ ] **Step 5: 回到 Task 3 编译 agent-core**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo check --workspace 2>&1 | grep error | head`
Expected: 无 error（Task 3 的 Goal* variant 引用现在有定义了）

- [ ] **Step 6: Commit**

```bash
cd /Users/ricardo/code/ricardo/rust/hebbian
git add crates/protocol/src/event.rs apps/desktop/src/chat.rs
git commit -m "$(cat <<'EOF'
新增 GoalAchieved/Impossible/Progress 事件 + chat.rs 翻译

- Why: //goal 裁决结果需推给前端渲染目标状态条
- 影响范围: protocol + desktop 翻译，additive，旧客户端忽略未知 event
- 留尾巴: 前端渲染在 Task 6
EOF
)"
```

---

## Task 5: 3 个 Tauri command（set/clear/get active_goal）

**Files:**
- Modify: `apps/desktop/src/chat.rs`（或 command 定义处）
- Modify: `apps/desktop/src/lib.rs`（`invoke_handler` 注册）

- [ ] **Step 1: 写 3 个 command**

在 chat.rs（或 desktop command 模块）加，照 `set_run_mode` / `set_force_automode` 范式：

```rust
#[tauri::command]
pub fn set_active_goal(
    state: tauri::State<'_, AppState>,
    session_id: String,
    condition: String,
) -> Result<(), String> {
    let dd = state.data_dir();
    let goal = agent_core::storage::sessions::ActiveGoal {
        condition,
        created_at: chrono::Utc::now().timestamp_millis(),
        iterations: 0,
        last_reason: None,
    };
    agent_core::storage::sessions::set_active_goal(&dd, &session_id, Some(goal))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_active_goal(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    let dd = state.data_dir();
    agent_core::storage::sessions::set_active_goal(&dd, &session_id, None)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_active_goal(
    state: tauri::State<'_, AppState>,
    session_id: String,
) -> Result<Option<agent_core::storage::sessions::ActiveGoal>, String> {
    let dd = state.data_dir();
    agent_core::storage::sessions::load(&dd, &session_id)
        .map(|s| s.active_goal)
        .map_err(|e| e.to_string())
}
```

> 注：`AppState` / `state.data_dir()` 的精确取法照 `set_run_mode` command 现有写法（搜 `pub fn set_run_mode` 在 desktop 里的定义，照抄它拿 data_dir 的方式）。`ActiveGoal` 需 `#[derive(Serialize)]`（Task 1 已加）才能作为 command 返回值——确认 Task 1 的 derive 含 Serialize。

- [ ] **Step 2: 注册到 invoke_handler**

在 `apps/desktop/src/lib.rs` 的 `tauri::generate_handler![...]`（搜 `set_run_mode`）里加：
```rust
            set_active_goal,
            clear_active_goal,
            get_active_goal,
```

- [ ] **Step 3: 编译**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo check -p hebbian-desktop 2>&1 | grep error | head`
Expected: 无 error

- [ ] **Step 4: Commit**

```bash
cd /Users/ricardo/code/ricardo/rust/hebbian
git add apps/desktop/src/chat.rs apps/desktop/src/lib.rs
git commit -m "$(cat <<'EOF'
新增 set/clear/get_active_goal 三个 Tauri command

- Why: //goal 前端命令需调后端读写会话目标
- 影响范围: desktop 后端，照 set_run_mode 范式
- 留尾巴: 前端绑定+命令注册在 Task 6
EOF
)"
```

---

## Task 6: 前端 `//goal` 命令 + 状态条渲染

**Files:**
- Modify: `apps/desktop/frontend/src/desktop/bridge/tauri.ts`
- Modify: `apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts`
- Modify: 渲染目标状态条的组件（照 RunModeChip / 现有事件监听处）

- [ ] **Step 1: tauri.ts 加 3 个 invoke 绑定**

在 `tauri.ts`（约 352-371 行，`set_run_mode` 附近）加：

```typescript
export const setActiveGoal = (sessionId: string, condition: string) =>
  invoke<void>("set_active_goal", { sessionId, condition });

export const clearActiveGoal = (sessionId: string) =>
  invoke<void>("clear_active_goal", { sessionId });

export type ActiveGoal = {
  condition: string;
  created_at: number;
  iterations: number;
  last_reason?: string;
};

export const getActiveGoal = (sessionId: string) =>
  invoke<ActiveGoal | null>("get_active_goal", { sessionId });
```

- [ ] **Step 2: slashCommands.ts 注册 `//goal` 内置命令**

在 `builtinSlashCommands` 数组（约 69 行，`hands-off` 之后）加一项：

```typescript
  {
    name: "goal",
    description: "设一个完成条件，达成前不停（//goal clear 清除）",
    argumentHint: "<条件> | clear",
    kind: "builtin",
  },
```

并在 `builtinRegistry`（该文件内 handler 注册处，搜 `hands-off` 的 handler）加 `goal` 的 handler：
- 无参 → 调 `getActiveGoal`，toast 显示当前条件/轮数/上次判定；无目标则 toast「当前没有目标，用 //goal <条件> 设一个」
- `clear` → 调 `clearActiveGoal`，toast「已清除目标」
- 其它（条件文本）→ 调 `setActiveGoal(sid, 条件)`，toast「目标已设：<条件>」

> handler 签名照 `hands-off` 现有 handler（拿 session_id、调 invoke、弹 toast 的范式）。

- [ ] **Step 3: 监听 3 个 goal 事件渲染状态条**

在监听后端事件的前端处（搜 `goal-achieved` 暂无、照 `run-mode-changed` 或现有 `listen(` 范式）加监听：
- `goal-progress` → 状态条显示「目标进行中 · 第 N 轮 · <reason 摘要>」
- `goal-achieved` → 绿色 toast「目标达成 ✓」+ 清状态条
- `goal-impossible` → 橙色 toast「目标无法达成：<reason>」+ 清状态条

> 状态条最简实现：复用现有 toast + 一个轻量 banner（照 RunModeChip 的展示位）。不强制做复杂 UI——首版 toast + 一行 banner 即可。

- [ ] **Step 4: tsc 类型检查**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian/apps/desktop/frontend && pnpm exec tsc --noEmit 2>&1 | tail -15`
Expected: 无 error

- [ ] **Step 5: Commit**

```bash
cd /Users/ricardo/code/ricardo/rust/hebbian
git add apps/desktop/frontend/src/desktop/bridge/tauri.ts apps/desktop/frontend/src/desktop/ui/lib/slashCommands.ts
# + 渲染组件文件
git commit -m "$(cat <<'EOF'
前端 //goal 命令 + 目标状态条渲染

- Why: 用户用 //goal <条件> 设目标、//goal clear 清除，监听裁决事件展示进度
- 影响范围: desktop 前端（bridge + slashCommands + 事件监听）
- 留尾巴: 复杂状态条 UI 可后续打磨，首版 toast+banner
EOF
)"
```

---

## Task 7: 端到端 A/B 验证（heb CLI，遵循 CLAUDE.md 修 bug 流程）

**Files:** 无（验证任务）

- [ ] **Step 1: 构建 heb CLI**

Run: `cd /Users/ricardo/code/ricardo/rust/hebbian && cargo build -p hebbian-cli 2>&1 | tail -5`
Expected: 编译成功

- [ ] **Step 2: 确认 heb 能设 goal**

heb CLI 首版可能没有 `//goal` 前端拦截——goal 设置走 Tauri command，CLI 没有。**先确认 CLI 怎么设 goal**：
- 若 CLI 有等价 IPC：用它设 goal
- 若没有：直接对 session jsonl 调 `set_active_goal`（写个最小 Rust 测试 bin，或在 heb 加一个临时 IPC——按 CLAUDE.md「现有命令不够用时允许新增」评估）

> 本步的产出是一个能复现的脚本，不是改产品代码。最简路径：写一个 `#[tokio::test]` 或临时 example，调 `set_active_goal` 后跑 agent_loop，断言行为。

- [ ] **Step 3: 阶段 A 复现「不达目标续跑」**

```bash
heb new --provider=<id> --workdir /tmp/goal_repro > /tmp/heb.log 2>&1 &
sleep 1; SID=$(jq -r .session_id < <(head -n1 /tmp/heb.log))
# 设目标「创建 /tmp/goal_repro/done.txt」（通过上一步确定的方式）
heb input $SID "看一下当前目录有哪些文件"   # 故意给个不会创建 done.txt 的任务
# 观察事件流：应看到 GoalProgress（judge 判 NotYet，注入续跑）
tail -f /tmp/heb.log | grep -i goal
```
Expected: 事件流出现 `goal-progress` / GoalProgress，模型被反复要求继续（因为 done.txt 没创建）

- [ ] **Step 4: 阶段 B 验证「达成即停」**

同一会话，让模型真的创建文件：
```bash
heb input $SID "创建 /tmp/goal_repro/done.txt 文件"
tail -f /tmp/heb.log | grep -i goal
```
Expected: 模型创建后，事件流出现 `goal-achieved` / GoalAchieved；`~/.hebbian/sessions/$SID/session.jsonl` 里 active_goal 被清空（grep `active_goal` 看最后一条 MetaUpdate 是 clear）

- [ ] **Step 5: 记录验证结果**

把 A 阶段（GoalProgress 反复出现）和 B 阶段（GoalAchieved + 目标清空）的事件行贴到 changelog 对应条目，作为「修前现象 / 修后验证」证据。

- [ ] **Step 6: 追加 changelog**

```bash
cd /Users/ricardo/code/ricardo/rust/hebbian
# 在 docs/changelog.md 末尾追加一条「//goal 命令实现完成」，含：
# - Why（用户要 CC 式 /goal）
# - 改动（Task 1-6 涉及文件）
# - 影响范围（agent-core + protocol + desktop，纯 additive）
# - 验证（Step 3/4 的 A/B 事件流证据）
# - 留尾巴（heb/hebweb 命令对称、状态条 UI 打磨）
git add docs/changelog.md
git commit -m "补充 //goal 实现的 changelog 与 A/B 验证记录"
```

---

## 验证清单（全部完成后）

```bash
cd /Users/ricardo/code/ricardo/rust/hebbian
cargo check --workspace                          # Rust 全编译
cargo test -p agent-core --lib                   # agent-core 单测（goal:: + active_goal_*）
cd apps/desktop/frontend && pnpm exec tsc --noEmit && cd -   # 前端类型
# heb CLI A/B（Task 7）：GoalProgress 续跑 → GoalAchieved 停止
```

完整跑通即交付。架构.md 的 §4.8.3 / §8.2 / §4.9 三处需同步补一句 goal 说明（实现时一并改，遵循 CLAUDE.md「引入新设计须先更新架构.md」）。