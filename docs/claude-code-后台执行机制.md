# Claude Code 后台执行机制研究

> 研究对象：`~/.vscode/extensions/anthropic.claude-code-2.1.140-darwin-arm64/resources/native-binary/claude`
> （Bun 编译的单文件 Node 二进制，约 200 MB；字符串与函数主体从 `strings(1)` dump 后反向得到）

本文系统拆解 Claude Code 的所有 Bash / 后台 / 定时执行机制，并对比 Hebbian 架构文档 §4.12「长任务挂起 + Wakeup」的现有设计，给出可借鉴点和改进建议。

---

## 0. 全景图

Claude Code 在「让模型不阻塞地操作 shell」这件事上演化出了 **6 个一等公民工具 + 1 个统一调度核**：

```
                          ┌─────────────────────────────┐
                          │      TaskRegistry           │
                          │  (kind: local_bash /        │
                          │   monitor / dream / agent)  │
                          └──┬──────────────┬───────────┘
                             │              │
       ┌───────┬────────┬────┴────┬─────────┴──┬─────────────┐
       │       │        │         │            │             │
     Bash  Bash(bg)  Monitor   CronCreate  ScheduleWakeup  Push
   同步     一次性    流式      定时        延迟         主动通知
   等返回   后台跑    每行=事件 cron 表    /loop 节拍    用户终端
       │       │        │         │            │
       └───────┴────────┴────┬────┴────────────┘
                             │
                   TaskOutput / TaskStop
                   （读 / 杀 任何后台 task）
```

底层共享：
- **`_HH()`** —— 唯一的进程 spawn 入口（bash / pwsh / sh），所有命令都走它，自带 sandbox-exec / seccomp / cwd 探针
- **`taskRegistry`** —— 进程级单例 + agent 归属，task 状态机统一管理
- **`UO({mode:"task-notification", priority:"next"})`** —— 通知注入器，把事件包成 `<task-notification>` XML，插到下一轮推理输入

---

## 1. Bash（前台同步）

### 1.1 触发
模型调用 `Bash` 工具，**不带** `run_in_background` 或带 `false`。

### 1.2 入口
- 工具定义：`v7 = M9({ name: bq, ... })`，绑定模块 `gjH`
- 输入 schema：`{ command, timeout?, description?, run_in_background?, dangerouslyDisableSandbox? }`
- 输出 schema：`{ stdout, stderr, interrupted, returnCodeInterpretation?, persistedOutputPath?, persistedOutputSize?, backgroundTaskId?, assistantAutoBackgrounded?, ... }`

### 1.3 执行流程
```
v7.call(input)
  └→ checkPermissions（沙箱判定、用户规则匹配）
  └→ _HH(command, signal, "bash", { timeout, onStdout, shouldUseSandbox, shouldAutoBackground:true })
        ├→ 取 shell：CLAUDE_CODE_SHELL > $SHELL > 探测 zsh/bash
        ├→ 写一个临时 wrapper：保存退出码 + cwd 探针（命令前后捕获 pwd 写到临时文件）
        ├→ spawn(shell, ["-c", wrapper])，sandbox-exec 包一层
        └→ 等 result（带 timeout）
  └→ 读 cwd 探针文件，若变更则在 result 加 cwd 警告
  └→ kill timer
  └→ 返回 { stdout, stderr, code, interrupted }
```

关键常量（`Cn` 模块）：
- `m73 = 3600000` —— 子 agent 内 background shell 上限 1h
- `A83 = 15000` —— **assistant 模式阻塞预算**：命令同步等 ≥ 15s 自动转后台（设 `assistantAutoBackgrounded:true`）
- `b73 = 5000` —— 子 agent 启动后的输出嗅探窗口
- `u73 = 1024` —— 内存 tail buffer 容量上限
- `p73` —— 一组交互提示正则（`(y/n)`、`Press any key`、`Continue?`），命中后强制把命令杀掉并提示「don't use interactive prompts」

### 1.4 简单示例
```text
模型 → Bash { command: "cargo test", timeout: 120000 }
       (同步等，至多 2 min；若超 15s 自动转后台)
模型 ← { stdout: "...", stderr: "", interrupted: false }
```

---

## 2. Bash with `run_in_background: true`（后台一次性）

### 2.1 触发
模型显式传 `run_in_background: true`。**不**需要在命令末尾加 `&`，工具自己接管。

### 2.2 执行
- 复用 `_HH` 但 stdout 走 `onStdout` 回调进 tail buffer + 写到 `bg/<task_id>.log`
- 注册任务：`FjH({command, shellCommand, kind:"local_bash"})` → taskRegistry，状态 `isBackgrounded: true`，归属 agentId
- 立刻返回 `{ backgroundTaskId, persistedOutputPath }` 给模型
- 进程退出时通过 `$w6` 触发 `<task-notification>` 注入下一轮 user message

### 2.3 读 / 停
- **`TaskOutput`**：增量取 stdout（按 byte offset，超过 100 MB tail buffer 落盘文件回读）
- **`TaskStop`**：通过 `$A6` 校验 owner agent → 调 `local_bash.kill` → `KVH` 杀进程

### 2.4 示例
```text
模型 → Bash { command: "pnpm tauri dev", run_in_background: true }
模型 ← { backgroundTaskId: "bash_001", persistedOutputPath: ".../bg/bash_001.log",
         content: "Command running in background with ID: bash_001 ..." }

   ...继续做别的事...

(进程退出，事件被注入)
user: <task-notification task_id="bash_001" status="completed" exit_code=0>
       Build finished in 142s
      </task-notification>
```

---

## 3. Monitor（流式事件）

> 详细原理见前序研究记录，这里压缩。

### 3.1 一句话
**长跑后台子进程 + 行级管道 + 令牌桶限流 + 通知注入器**——每一行 stdout 都立刻变成 push 通知，模型不用 sleep 也不用 polling。

### 3.2 关键常量（`hf_` / `AA6`）
- `bV8 = 3600000` —— timeout 上限 1h
- `Vc7 = 300000` —— 默认 timeout 5min
- `kc7 = 1800000` —— 远程模式（`CLAUDE_CODE_REMOTE`）强制 cap 30min
- `OA6 = 10`，`ff_ = 2000` —— 令牌桶：容量 10、每 2 秒补 1 个（稳态 ≤ 0.5 行/秒）
- `dF7 = 30000` —— 连续 30 秒被限流就**强杀**
- `KA6 = 500` —— 单行最大字符（超长截断 + `...(truncated)`）

### 3.3 核心算法（`xa5`）
```
spawn bash 命令 (preventCwdChanges, sandboxed)
TA6 行缓冲器：按 \n 切，超长截断，200 ms 定时 flush 残留
每行进入令牌桶：
  • 拿到 token → nkH() 包 <task-notification> 注入 → UO({priority:"next"})
  • 拿不到 → j++，记录 J=now()
            等下次能发 → 先发 "[X events suppressed ...]"
            Date.now()-J > 30s → 发 kill 通知 + KVH(taskId)
非 persistent → setTimeout(timeout_ms) → "[Monitor timed out — re-arm if needed.]"
```

### 3.4 示例
```text
模型 → Monitor {
        description: "watch test logs for failures",
        command: "tail -F target/test.log | grep --line-buffered FAIL",
        persistent: true
      }
模型 ← "Monitor started (task mon_x, persistent — runs until TaskStop or session end).
       Events may arrive while you are waiting for the user — an event is not their reply."

   ...

(grep 命中一行 FAIL)
user: <task-notification task_id="mon_x">
        FAIL tests::auth::token_expiry
      </task-notification>
```

---

## 4. CronCreate / CronList / CronDelete（定时任务）

### 4.1 触发
模型调用 `CronCreate`，参数：
```text
cron       : 标准 5 字段 "M H DoM Mon DoW"，本地时区
prompt     : 点火时投到 user message 的 prompt
recurring? : true（默认）= 每次匹配点火，7 天自动过期
             false       = 一次性，点火后自动 delete
durable?   : false（默认）= 进程内存，session 结束即丢
             true        = 写 .claude/scheduled_tasks.json，跨重启存活
```

### 4.2 入口
- `CronCreateTool`（`Pa5`）：`Kc7` 模块
- `CronListTool`（`ka5`）：`Ac7` 模块
- `CronDeleteTool`（`Ga5`）：`Tc7` 模块

### 4.3 关键约束
- 单进程上限 `_c7 = 50` 个 job
- 子 agent（teammates）不能创建 durable cron（不持久跨 session）
- 必须在未来 1 年内能匹配上日历（防止设了死表达式）
- durable cron 的元数据带 `createdBySessionId / createdByPid / createdByProcStart`，用来识别进程归属

### 4.4 Jitter / 防雷暴（`Iu` 配置，`Xz_` 模块）
```javascript
{
  recurringFrac: 0.5,        // 循环任务在窗口前 50% 内随机点火
  recurringCapMs: 1800000,   // 抖动上限 30 min
  oneShotMaxMs: 90000,       // 一次性最多提前 90s
  oneShotFloorMs: 0,
  oneShotMinuteMod: 30,      // 仅当 minute % 30 != 0 时抖动
                              // (防止 :00 整点和 :30 半点的"密集预约"打满 API)
  recurringMaxAgeMs: 604800000, // 循环任务 7 天自动清理
  cacheLeadMs: 15000,        // 整点前 15s 提前点火，
                              // 让模型有缓存预热时间
}
```

抖动的随机数源是 cron job id 的前 8 个 hex，**确定性抖动**：同一个 job 每次抖偏移一致，便于复现 debug。

### 4.5 Loop 模式（重要彩蛋）
`/loop` slash 命令可以调用 cron 让 prompt 周期复跑：
- prompt 字段填特殊 sentinel `<<autonomous-loop>>` / `<<autonomous-loop-dynamic>>`
- 点火时运行时解析回真正的 loop 指令
- 配合 **ScheduleWakeup**（§5）实现动态自调节节奏

### 4.6 示例
```text
模型 → CronCreate {
        cron: "*/15 * * * *",
        prompt: "Check CI status for the open PR and report regressions",
        recurring: true,
        durable: false
      }
模型 ← "Scheduled recurring job 9a3f (every 15 minutes). Session-only.
        Auto-expires after 7 days. Use CronDelete to cancel sooner."

   ...15 分钟后...

user: [cron 9a3f fired]
      Check CI status for the open PR and report regressions
(模型按 prompt 重新进入推理)
```

---

## 5. ScheduleWakeup（单次延迟唤醒）

### 5.1 触发
专门给 `/loop dynamic` 模式：模型每轮结束前可选调用 `ScheduleWakeup({ delay_secs, reason, prompt })`，自己决定下一波多久后醒。

### 5.2 实现（`mUH` 模块）
- 不是独立调度器——**而是把 delay 翻译成下一个分钟边界的 cron 表达式**，复用 §4 全套基础设施
- delay_secs 钳到 `[Pz_=60, Le_=3600]`（1 min ~ 1 h）
- 翻译：`new Date(now + delay).getMinutes() + " " + ...getHours() + " * * *"`
- recurringMaxAgeMs（7 天）兜底：连续触发超过 7 天自动 ageOut 停止

### 5.3 设计取舍
为什么不做独立的延迟调度？因为：
1. cron 引擎已经有持久化、jitter、归属、列表查询、UI 显示
2. 单次唤醒 = 设一次 cron job + recurring=false → 完全复用
3. 节省一套 timer 管理代码

### 5.4 示例
```text
模型 → ScheduleWakeup {
        delay_secs: 270,
        reason: "wait for CI run #4523",
        prompt: "<<autonomous-loop-dynamic>>"
      }
模型 ← "Wakeup scheduled in ~4-5 min. Use TaskStop or cron list to cancel."
```

> 注：270s 是个有趣的数字 —— 工具文档明确写「Anthropic 提示缓存 TTL 是 5 min，超过 300s 就 cache miss」，所以延迟既不要超 5 min（破缓存），也不要短轮询（浪费），270s 是最优 sweet spot。

---

## 6. TaskOutput / TaskStop（统一读写）

### 6.1 角色
- 所有「kind」的后台 task（local_bash / monitor / dream / agent）都注册到同一 `taskRegistry`
- `TaskOutput` 拉新增 stdout；`TaskStop` 停任意 task
- 旧名 `BashOutputTool` / `KillShellTool` 已废弃，只在 analytics 表残留

### 6.2 TaskOutput 实现要点（`Yw` class）
- 内存 tail buffer + 落盘双轨：`bg/<task_id>.log`
- 按 byte offset 增量：先尝试 buffer，超出则 `HV_(path, offset, JnH())` 从文件 tail 读
- 返回字段：`{ stdout, stderr, totalLines, totalBytes, isOverflowed, outputFileSize }`
- 工具结果末尾会带提示「Output truncated (X KB total). Full output saved to: <path>」

### 6.3 TaskStop 安全语义（`$A6`）
```javascript
// owner 校验：子 agent 不能停别人的 task
if (!FF7(callerAgentId, task.agentId))
  throw "Task X is owned by Y; agent Z cannot stop it."
```
保证子 agent 沙箱内启动的 monitor / 后台进程不会被父 agent 或其他兄弟 agent 误杀。

---

## 7. PushNotification（外发通知）

不严格属于「执行」机制，但和 Monitor 配套：

### 7.1 三态触发条件
```javascript
if (mobilePushDisabled)         → disabledReason: "config_off"
else if (userActive in terminal) → disabledReason: "user_present"
else if (noRemoteControl)        → 只发终端弹窗
else                             → 终端 + 移动端推送
```

「用户活跃」判断：`hasFocus(terminal)` 或 `Date.now() - lastKeystroke < xL_`（默认 30s）

### 7.2 价值
让 Monitor 在 idle 长任务命中事件时，可以**穿透 OS 通知给用户**——不只是把通知塞进模型 context。

---

## 8. 跨切关键设计

### 8.1 通知载荷格式（统一）
```xml
<task-notification task_id="..." status="..." agent_id="...">
  ...payload (单行截断 / 累计抑制提示 / 完成摘要)
</task-notification>
```
- 注入路径：`UO({ value, mode:"task-notification", priority:"next" })`
- 实际效果：插到下一次模型推理的 user message 队列**最前**，模型当作系统通知处理（不算"用户回复"）

### 8.2 Agent 归属与孤儿清理（`Pc7`）
- 每个 task 记录 `agentId`（由 `_A6(ctx)` 取当前 agent context）
- 子 agent 退出时 `Pc7(agentId, registry)` 遍历清掉所有归属该 agent 的 running task
- 防止 sub-agent 启的 monitor 漏到主会话

### 8.3 Sandbox 一致性
所有走 `_HH()` 的命令默认 sandbox：
- macOS：`sandbox-exec` profile，约束网络（按 allowedDomains）、文件写、Mach lookup
- Linux：seccomp filter
- 触发 `dangerouslyDisableSandbox: true` 时强制提示用户审批

---

## 9. 对比 Hebbian 架构 §4.12 设计

### 9.1 对照表

| 维度 | Claude Code | Hebbian 现设计 §4.12 |
|---|---|---|
| 长跑命令 | Bash + `run_in_background:true` + TaskOutput | BashTool 转后台 + BashOutput 拉 |
| 流式事件 → 模型 | **Monitor**（push + 令牌桶 + 暴流熔断） | **缺失** — 只能轮询 |
| 模型显式挂起 | 不需要（事件总是 push） | `WaitForTask` |
| 单次延迟 | `ScheduleWakeup` →（复用 cron） | `ScheduleWakeup`（独立调度） |
| 定时循环 | `CronCreate`（5 字段 + jitter + durable + 上限 + 自动过期） | **缺失** |
| 用户通知 | `PushNotification`（idle 检测） | **缺失** |
| 任务挂起态 | 进程内（崩了即丢） | **`RunCheckpoint` 落盘** ✅ |
| 三态显式建模 | 隐式 | **Active / Suspended / Finished** ✅ |
| 协议事件 | 仅内部 telemetry | **`RunSuspended` / `RunResumed`** ✅ |
| 任务归属 | `agentId` + 子 agent 退出清孤儿 | `session_id` scoped |
| 通知载荷 | `<task-notification>` XML | `<wakeup>` XML（仅 Suspended 态） |
| 状态快照注入 | 无（每个事件单独 push） | `<background_tasks>` 块挂 user message 头 |
| 统一 task 注册表 | **共享 `taskRegistry`** ✅ | BackgroundShells 单独，未来加 Monitor/Cron 会碎 |

### 9.2 Hebbian 比 Claude Code 强的地方

1. **RunCheckpoint 持久化** —— Claude Code 完全进程内，崩 = 一切丢；hebbian 设计支持 Suspended 态落盘，理论上可恢复（即便决策是「重启不 auto resume」，落盘也方便重启后用户手动 resume / 留痕）
2. **三态显式状态机** —— `Active / Suspended / Finished` 比 Claude Code 散在各种 `task.status` 字段更工程化，方便协议层和 UI 一等公民支持
3. **`<background_tasks>` SEMI 快照** —— Claude Code 没有这个：每个事件单独 push，但模型新发起 Run 时看不到「当前有什么后台在跑」。hebbian 设计在首条 user message 头部追加列表，模型一眼掌握全局——这是更好的 UX
4. **协议事件** —— `RunSuspended/Resumed` 给 surface 提供了原生事件流，比 Claude Code 内部 telemetry 更清晰

### 9.3 Hebbian 应该借鉴 / 改进的地方

#### A. 必须补 **Monitor 类原语**（最优先）

**问题**：当前 hebbian 设计长任务只有 BashTool 转后台 + BashOutput 拉。对「等 X 出现就提醒我」类场景（监 log、监 PR 状态、监文件变更），模型只能：
1. 写 `WaitForTask` 等整个进程结束 —— 但 `tail -F` 永远不会结束
2. 反复调 BashOutput 轮询 —— 费 token、卡 turn、不优雅

**建议**：加 `MonitorTool` 工具，借鉴 Claude Code 整套设计：

```rust
// crates/agent-core/src/tools/monitor.rs (新)
pub struct MonitorTool;

#[derive(Deserialize)]
pub struct MonitorInput {
    pub command: String,
    pub description: String,
    pub timeout_ms: Option<u64>,    // 默认 5 min，上限 1 h
    pub persistent: Option<bool>,   // true 时无超时
}

// 关键设计：
// 1. 令牌桶（capacity=10, refill=1/2s）—— 防 log 雷暴
// 2. 暴流熔断（连续 30s 被限流 → 强杀 + 提示模型 grep 过滤）
// 3. 单行 500 字符截断
// 4. 200ms flush 残留行
// 5. 通知载荷：<wakeup kind="monitor_event" task_id="..." description="...">line</wakeup>
//    复用 §4.12.5 已有的 <wakeup> 格式
```

**路由方式**：和 §4.12.6 一致——
- Run 在 Active 态：走 PendingInputs 引导通路（§4.2.3）
- Run 在 Suspended 态：直接 resume，inject `<wakeup kind="monitor_event">` 作为 user message

这样 hebbian 的 `<wakeup>` 协议既覆盖 cron 定时唤醒、又覆盖 bg task 完成、又覆盖 monitor 流式事件——**一套 XML 协议三种场景**，比 Claude Code 的 `<task-notification>` + 各种 status 字段更收敛。

#### B. Cron 定时任务（v1 至少留扩展点）

**问题**：当前设计只有 `ScheduleWakeup`（单次延迟，max 1h）。无法表达「每天 9 点 review 一次代码」「每 15 分钟 check CI」。

**建议**：加 `CronCreateTool / CronListTool / CronDeleteTool`，关键防御性约束**必须从 v1 就上**——否则后期改不动：

| 约束 | Claude Code 值 | hebbian 建议 |
|---|---|---|
| 单 session 上限 | 50 | 32（hebbian 偏轻量） |
| 循环任务自动过期 | 7 天 | 7 天（兜底防泄漏） |
| 抖动算法 | id hash → [0, recurringFrac × window] | 同 |
| 抖动上限 | 30 min | 同（防多 session 同时点火打满 API） |
| 整点提前量 | 15s（缓存预热） | 同 |
| durable 模式 | `.claude/scheduled_tasks.json` | `~/.hebbian/sessions/<sid>/cron.json` |
| 子 agent 限制 | teammates 不能创建 durable | 同 |

**与 §13「重启不自动 resume」决策的冲突点**：
- Claude Code 的 durable cron 是「**hebbian 重启后 cron 仍然能点火**」
- 这违反了你们 §13 的决策
- **解决方案**：durable cron 重启后**只列表显示，不自动点火**，需要用户手动 `/cron resume <id>`。这样既不丢用户的「记得明天提醒我」请求，又不违反「重启不自动 resume」原则

#### C. 统一 TaskRegistry + TaskOutput/TaskStop 工具

**问题**：当前 §4.12 设计里 BackgroundShells 是独立的 session-scoped 注册表；将来加 Monitor、Cron 状态查询会演化出 BashOutput / MonitorRead / CronStatus / KillBash / StopMonitor / CancelCron... 一堆碎片化工具。

**建议**：复用 Claude Code 的收敛设计——

```rust
// crates/agent-core/src/tasks/registry.rs
pub enum TaskKind {
    LocalBash,    // 后台 shell（一次性）
    Monitor,      // 流式事件
    AgentDream,   // §未来：后台 agent compact
    Cron,         // 注册中（未点火的循环）
}

pub struct Task {
    pub id: TaskId,
    pub kind: TaskKind,
    pub status: TaskStatus,  // running / completed / failed / killed
    pub agent_id: AgentId,   // 关键：owner 校验
    pub session_id: SessionId,
    pub description: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    // kind-specific payload 走 enum 或 trait object
}

// 两个统一工具，covers 所有 kind：
TaskOutput { task_id, offset? }  → 增量读 stdout
TaskStop   { task_id }            → 杀任务（带 owner 校验）
```

owner 校验照搬 Claude Code 的 `FF7`：子 agent 不能停别人的任务。

#### D. autoBackground 兜底（assistant 模式下的 15s 预算）

**问题**：当前 hebbian BashTool 设计要求模型**显式**选 `run_in_background`。如果模型忘了标但命令实际跑 30 min，会卡住 turn。

**建议**：实现 Claude Code 的 `A83 = 15000` 兜底——
```rust
// crates/agent-core/src/tools/bash.rs
const ASSISTANT_BLOCKING_BUDGET_MS: u64 = 15_000;

// 在 assistant 模式（非 REPL 模式）下，命令同步等 ≥ 15s
// 自动转后台，返回结果带 `assistant_auto_backgrounded: true`
```

#### E. PushNotification + idle 检测

**问题**：未来加桌面通知时，如果不做 idle 检测，monitor 命中事件时会狂轰用户。

**建议**：照 Claude Code 设计——
- 跟踪 `last_keystroke_at` + `terminal_has_focus`
- idle 超 30s 且 not focused → 触发桌面 / 移动通知
- 否则只插队到模型 context，UI 端用浮动栏静默提示

#### F. 通知载荷的「累计抑制」语义

**Claude Code Monitor 的妙处**：被限流的行不是丢弃，而是计数；下次能发时先发一条 housekeeping「X events suppressed — consider TaskStop to restart with stricter filter」。

**建议**：hebbian 的 `<wakeup kind="monitor_event">` 也支持累计抑制：

```xml
<wakeup kind="monitor_suppressed" task_id="mon_x" suppressed_count="47" duration_ms="12000">
  Output rate too high. Last 47 events dropped over 12s.
  Consider TaskStop and re-arm with a stricter grep.
</wakeup>
```

这样模型能自我诊断「我设的 monitor 太宽了」，主动修正。

---

## 10. 行动建议（按优先级）

### P0：必须做（在 §4.12 Phase 3 之前）
1. **`MonitorTool` 设计**纳入 §4.12.4，与 `WaitForTask` / `ScheduleWakeup` 并列
2. **统一 TaskRegistry** + `TaskOutput` / `TaskStop` 工具替代单纯的 BackgroundShells + BashOutput
3. **owner agent 校验**：sub-agent 不能 stop 不归属自己的 task

### P1：v1 留扩展点（可以不实现，但 schema 要预留）
4. `CronCreate/List/Delete` 工具骨架 + 关键防御性约束（上限、过期、jitter）
5. autoBackground 兜底（15s 阻塞预算）

### P2：未来增强
6. 累计抑制语义的 `<wakeup>` 子 kind
7. PushNotification + idle 检测（桌面 surface 上线后）

---

## 附录 A：Claude Code 二进制中的关键函数对照

| 功能 | 函数符号 | 模块 |
|---|---|---|
| 进程 spawn 入口 | `_HH` | 全局 |
| 任务注册 | `FjH` | `Cn` |
| 任务杀手 | `KVH` | `Cn` |
| 孤儿 task 清理 | `Pc7` | `Cn` |
| 通知注入 | `nkH` / `UO` | `Mf_` |
| 行缓冲器 | `TA6` | `AA6` |
| 令牌桶 | `zA6` | `AA6` |
| Monitor 主体 | `xa5` | `hf_` |
| Monitor 工具对象 | `xV8` | `hf_` |
| TaskOutput class | `Yw` | (anonymous) |
| TaskStop 主体 | `$A6` | (anonymous) |
| Bash 工具对象 | `v7` | `gjH` |
| CronCreate 工具 | `Pa5` | `Kc7` |
| CronList 工具 | `ka5` | `Ac7` |
| CronDelete 工具 | `Ga5` | `Tc7` |
| ScheduleWakeup → cron 翻译 | `wd9` | `mUH` |
| Cron jitter 算法 | `fz_` / `Re_` | `bu` |

## 附录 B：关键常量速查

```
A83 = 15_000        ms     Bash assistant-mode 阻塞预算
m73 = 3_600_000     ms     子 agent 后台 shell 上限
u73 = 1024          bytes  tail buffer 容量
b73 = 5000          ms     子 agent 启动嗅探窗口
bV8 = 3_600_000     ms     Monitor timeout 上限
Vc7 =   300_000     ms     Monitor 默认 timeout
kc7 = 1_800_000     ms     远程模式 cap
OA6 = 10            个     令牌桶容量
ff_ = 2000          ms     令牌桶补充间隔
dF7 = 30_000        ms     Monitor 暴流熔断阈值
KA6 = 500           chars  单行截断
si5 = 200           ms     行缓冲 flush 间隔
_c7 = 50            个     单进程 cron job 上限
Pz_ = 60            s      ScheduleWakeup 最小延迟
Le_ = 3600          s      ScheduleWakeup 最大延迟
xL_ = 30_000        ms     PushNotification idle 阈值
recurringMaxAgeMs = 604_800_000 ms  循环 cron 7 天过期
cacheLeadMs       =      15_000 ms  整点提前点火量
```

---

## 附录 C：2.1.144 增量复查（2026-05-22）

本附录记录一次针对新版本 2.1.144（2026-05-19 发布）的复查发现，主线机制与 2.1.140 一致（§1-§7 仍然成立），只新增以下数据。

### C.1 实地捕获的 `<task-notification>` 样本

复查过程中，被探索的 CC 自己（VSCode 插件外的 native CLI）恰好通过 system-reminder 通道注入了一条真实通知到当前对话——直接复制如下，作为 §8.1 通知载荷格式的实例：

```
[SYSTEM NOTIFICATION - NOT USER INPUT]
This is an automated background-task event, NOT a message from the user.
Do NOT interpret this as user acknowledgement, confirmation, or response to any pending question.

<task-notification>
<task-id>bnr0t3sed</task-id>
<tool-use-id>toolu_01WM1EPcA4ycAraqh8wyhPMv</tool-use-id>
<output-file>/private/tmp/claude-502/.../tasks/bnr0t3sed.output</output-file>
<status>completed</status>
<summary>Background command "TaskOutput 工具用法" completed (exit code 0)</summary>
</task-notification>
```

补充几个 §8.1 没写到的细节：

- **头部前缀**：`[SYSTEM NOTIFICATION - NOT USER INPUT]` + 显式声明「不是用户回应，别把它当 confirm」——一段强 prompt-injection 防御
- **`<tool-use-id>` 字段**：把通知关联回触发它的 `tool_call.id`（如 `toolu_01WM1EPcA4ycAraqh8wyhPMv`），让模型在 transcript 里能反查上下文
- **status 枚举**：从 binary 看是 `completed / killed / stopped` 三态
- **task_id 形态**：9 字符 base36 风格（如 `bnr0t3sed`），不是 UUID

hebbian `<wakeup>` 协议（§4.12.5）目前缺这个头部前缀——下一轮迭代可以照抄。

### C.2 工具名 alias 表

binary 里直接看到的：

```js
{
  AgentOutputTool: "TaskOutput",
  BashOutputTool:  "TaskOutput",
  ListPeers:       "ListAgents",
  Brief:           "SendUserMessage"
}
```

`TaskOutput` 是统一对外名，`BashOutputTool` / `AgentOutputTool` 是内部 class。`Brief → SendUserMessage` 也很有意思——内部叫「简报」，对模型暴露成「给用户发消息」。

### C.3 Bash 启动后的提示文本

```
Command running in background with ID: bnr0t3sed.
Output is being written to: /private/tmp/claude-502/<encoded-cwd>/<session>/tasks/bnr0t3sed.output.
You will be notified when it completes.
To check interim output, use Read on that file path.
```

**关键设计**：CC 鼓励模型用 `Read` 工具直接读 output 文件（享受 offset/limit 分页），不优先走 `TaskOutput`。也就是说 `TaskOutput` 主要服务于「无文件路径的 task」（如 hosted agent）；纯 Bash 任务 Read 文件即可。

hebbian 当前用专门的 `BashOutput` 拿 task_id 拉——和 CC 设计哲学有差异。短期不动，长期可参考。

### C.4 完整 hook 体系（24 项，2.1.144）

binary 里穷举到的 hook 名单，按字母排序：

```
ConfigChange         CwdChanged           Elicitation
ElicitationResult    FileChanged          Notification
PermissionDenied     PermissionRequest    PostCompact
PostToolBatch        PostToolUse          PostToolUseFailure
PreCompact           PreToolUse           SessionEnd
SessionStart         Setup                Stop
SubagentStop         TaskCompleted        TaskCreated
TeammateIdle         UserPromptExpansion  UserPromptSubmit
WorktreeRemove
```

与后台任务 / wakeup 相关的几个：

- **`Stop`**：「Right before Claude concludes its response」——turn 结束前钩子。用于「确认无 active task 才允许结束」
- **`SubagentStop`**：subagent 结束前
- **`PostToolBatch`**：「Fires once after every tool call in a batch has resolved, before the next model request. Input includes `tool_calls` (array of `{tool_name, tool_input, tool_use_id, tool_response}`)」——**批粒度**，比 PostToolUse 单粒度更高效
- **`FileChanged`**：watched file 变化时触发（配合 Monitor 工具或 `monitors.json` 声明式 watch）
- **`TaskCreated` / `TaskCompleted`**：teammate 任务级
- **`Notification`**：notification 派发时（type ∈ `permission_prompt / idle_prompt / auth_success / elicitation_dialog / elicitation_complete / elicitation_response`）

### C.5 async hook + exit code 2 唤醒

binary 关键短语：

```
If true, hook runs in background and wakes the model on exit code 2 (blocking error). Implies async.
```

**含义**：hook 可声明 `async: true`，进程后台运行；退出 code 2 时**唤醒模型**——和 hebbian wakeup 思路完全一致。借鉴价值高（让 hook 共享 wakeup 协议）。

### C.6 plugin-level `monitors.json` 声明式 watch

```
Background watch scripts the host arms as persistent Monitor tasks
(unsandboxed, same trust tier as hooks) so plugins need not instruct
the model to arm them.
When omitted, monitors/monitors.json at the plugin root is loaded if present.
```

CC 插件根目录可以放 `monitors/monitors.json`，host 启动时自动 arm 这些 Monitor task——模型不用在 transcript 里主动 arm。

这跟 §9.3 A 节推荐的 `MonitorTool` 是配套：工具级 + 插件级两种 arm 方式。

### C.7 background session 概念（升格设计）

```
Delete a background session and its worktree. Unlike `stop`, works on already-exited sessions.
Restart a background session (or all of them) so it picks up the current Claude binary.
```

CC 把「长跑任务」可以升格为 **background session**（独立 git worktree）。
- `stop` 杀进程
- `delete` 同时删 session 和 worktree（已退出的也能删）
- `restart` 重启 session 以拾取最新 binary 版本

这是比 §1 / §2 的 background shell 更高层抽象——session-level 隔离。hebbian 当前 BackgroundShells 是单进程 / 单 log 文件，没有 worktree 隔离。

升级路径（如果未来需要）：当 task 超过某阈值（运行 > 5 min 或 output > N KB）自动 promote 为 session-with-worktree。

### C.8 telemetry 命名空间

```
tengu_bash_command_explicitly_backgrounded     # 模型显式 run_in_background=true
tengu_bash_command_timeout_backgrounded        # 超时转后台
tengu_powershell_command_explicitly_backgrounded
tengu_powershell_command_interrupt_backgrounded
tengu_powershell_command_timeout_backgrounded
```

`tengu` 是 CC 内部 telemetry 前缀（疑似 internal codename）。三态：explicitly / timeout / interrupt（用户 Ctrl+B）。hebbian 未来加 telemetry 时可参考这套命名。

---

## 附录 D：UI 端形态分析

复查 2.1.144 的 webview UI 端，意外发现：

### D.1 webview 几乎全是 monaco editor

| 文件 | 大小 | 内容 |
|---|---|---|
| `extension.js` | ~2 MB | host 端 IPC 转发；几乎无业务文本 |
| `webview/index.js` | ~4.8 MB | 99% 是 monaco editor bundle + tailwind 工具类 |
| `webview/index.css` | ~375 KB | 同上，主要 monaco 类名 |

webview 里反复 grep 业务关键词（`BackgroundTask` / `sidebar` / `RunSuspended` / `background_task`）**全部 0 命中**。

### D.2 推断：CC 没有专门的「后台任务面板」UI

所有 task 状态信息都通过 chat 流呈现：

- task 启动 → 普通 tool_call 卡片（带「Background task ID: ...」result）
- task 通知 → 在新的一轮 turn 里以 `<task-notification>` 段显示在 transcript
- task 输出 → 模型 Read output 文件，结果以普通 Read tool_call 卡片显示

**没有右侧 sidebar / 独立浮动 panel**——所有信息时间序混排在对话流。

### D.3 CC 派 vs hebbian 派 UI 哲学对比

| 维度 | CC（chat 流内嵌） | hebbian 当前（独立面板） |
|---|---|---|
| 信息源 | 单一：transcript | 双：注册表 + transcript |
| 「当前在跑什么」可见性 | 需翻历史 transcript | 一眼可见 |
| 跳转 | 自由滚动 | 用户提议「点卡片跳到对应 chat tool_call」 |
| 已完成 task | 通知 XML 一直留 transcript | 注册表清掉 / panel 不显示 |
| 简单度 | UI 简单，逻辑全在 transcript | 需要数据同步 |
| 多任务体感 | 同时跑 5 个 monitor 时很乱 | panel 聚类清晰 |

### D.4 hebbian 后续 panel 重构方向（结合用户需求）

用户已要求：
1. 完成的 task 不消失（折叠保留）
2. 按 tool_call 顺序展示（running 优先）
3. 点击卡片跳转 chat 对应 tool_call 位置
4. 右侧 VSCode 风 sidebar（小图标 → hover 稍大 → 点击展开挤压 chat）

这是 CC 派 + hebbian 派的折中——**保留 hebbian 的「独立 panel」形态，但把数据源单一化（panel 仅作为 transcript 的过滤聚合视图）**：

```
session.messages (source of truth)
  └── filter(tool_call.name === "Bash" && input.run_in_background)
       └── 排序：[运行中 (查 BackgroundShells.get)] + [已完成 (按 tool_call 顺序)]
            └── 渲染：每张卡片
                ├── 命令 + 状态徽章 (running / exited / killed / failed)
                ├── 实时输出（运行中：polling /read_background_task_output）
                ├── 已完成：从 tool_call.result 取最终输出
                └── 点击 → scroll-to-message(`#msg-${tool_call.message_id}`)
```

收益：

- panel 不持有独立状态，永远跟 transcript 一致
- session 切换 / 重启都不会丢历史 task
- 用户表述「点击跳转」自然落到 message id 锚点

---

## 附录 E：针对当前 sidebar 重构的借鉴优先级

按用户对当前 hebbian sidebar 重构需求的相关性排序（与 §10 的工具体系借鉴不同，这里聚焦 UI / UX）：

### E.1 直接借鉴

1. **`<task-notification>` 头部前缀**（CC 派 prompt injection 防御）
   - 现状：hebbian wakeup 注入的 user message 没有「这不是用户消息」头部
   - 改：`<wakeup>` 协议加 `[SYSTEM NOTIFICATION - NOT USER INPUT]` 前缀
   - 收益：模型不会把通知误判为用户 confirm

2. **task 输出落到磁盘文件、模型用 Read 读路径**（vs 走专门工具）
   - CC：output 在 `<session>/tasks/<task_id>.output`，模型 Read 该路径
   - hebbian：output 在 `<session>/bg/<task_id>.log`，模型用 BashOutput 拉
   - 后续优化方向：可以告诉模型路径让它 Read，BashOutput 主要给「需要等通知 + 增量游标」场景

### E.2 不冲突但可参考

3. **「completed task 不消失」 = 数据源单一化（transcript-derived）**
   - 见附录 D.4
   - 改：BackgroundTaskPanel 数据源切到 session.messages 派生

4. **PostToolBatch hook**（与 panel 重构无关，但是顺手的工程价值）
   - 改：hebbian PostToolUse 改 batch 粒度（一批 tool_call 完成后触发一次）

### E.3 与用户需求正交，不在本轮做

5. CC 派的「全部 in chat 流」UI ——用户明确要 sidebar 设计，不采纳
6. Monitor 工具体系（见 §9.3.A）——独立大特性，未来路线图
7. background session + worktree（见 §C.7）——长期演进方向

