# `heb` CLI / `hebweb` — AI 自主调试操作手册

> **写给 AI**：你正在调试 Hebbian agent。本文档让你不依赖 Desktop GUI 也能驱动 agent_core 并观察它的全部内部状态。两套 surface 供选择：
>
> - **`heb` CLI**：纯命令行 + NDJSON 事件流。最快、最适合脚本化回归 / bug 复现。§1–§8。
> - **`hebweb`**：HTTP + WebSocket server，跑真实前端代码（与 Desktop 同一份 React 代码）。配合 Playwright 可以让 AI 看到 DOM / 截图、点击界面、定位 UI bug。§9。
>
> 适用场景：自动化回归、bug 复现、压测 prompt / 工具行为、CI 端到端验证、agent 自我调试 agent、UI 视觉/交互回归测试。

---

## 1. 一分钟上手

```bash
# 终端 A：起 daemon（持续输出 NDJSON 事件流到 stdout）
heb new --provider=<provider_id> --workdir /tmp/work > /tmp/heb.log 2>&1 &
sleep 1
SID=$(jq -r '.session_id' < <(head -n1 /tmp/heb.log))
echo "session=$SID"

# 终端 B：与 daemon 交互
heb input $SID "请用 Write 工具创建 /tmp/work/hello.txt，内容写 hi"

# 持续 tail 事件流
tail -f /tmp/heb.log
```

事件流形如：

```json
{"event":"started","session_id":"202605201609-fe2ec5d8"}
{"event":"run_started"}
{"event":"text_delta","text":"我"}
{"event":"tool_start","id":"call_…","name":"Write","input":{"file_path":"…","new_string":"hi"}}
{"event":"permission_requested","request_id":"perm_…","kind":"tool_call","tool_name":"Write","risk":"medium"}
```

看到 `permission_requested` 就在终端 B 回应：

```bash
heb allow $SID perm_xxx           # 一次性放行
heb allow $SID perm_xxx session   # 本次会话内同类全部放行
heb deny  $SID perm_xxx           # 拒绝
heb deny-feedback $SID perm_xxx "改用 Edit 工具"   # 拒绝并把反馈塞回 agent
```

`run_finished` 出现即一轮结束。继续 `heb input $SID "下一步问题"` 即可（多轮上下文自动续）。

---

## 2. 完整命令参考

| 命令 | 作用 | 何时用 |
|------|------|--------|
| `heb new [--session-id SID] [--provider P] [--model M] [--workdir DIR] [--mode MODE] [--data-dir DIR]` | 起 daemon，新建或连接 session | 一切的入口 |
| `heb run "<task>" [--provider P] [--model M] [--workdir DIR] [--mode MODE \| --yolo] [--session-id SID] [--timeout SECS] [--json] [--data-dir DIR]` | **一次性无人值守**跑完一个完整任务即退出（不监听 socket）。审批自动拒 + reason 回灌 agent、提问自动取消。退出码：完成 0 / 失败 1 / 超时 2 / 取消 130 | 评测 / 脚本化批量跑任务，无需交互 |
| `heb input <SID> "<text>"` | 发用户输入。**自动判定**：无 active run 时开新 run；有 active run 时注入 pending 队列 | 提问、回答、补充上下文、流式中插队 |
| `heb allow <SID> <RID> [scope]` | 批准权限审批。`scope ∈ {once,session,project,global}`，默认 `once` | 收到 `permission_requested` 后 |
| `heb deny <SID> <RID>` | 拒绝审批（agent 收到工具失败结果） | 同上 |
| `heb deny-feedback <SID> <RID> "<反馈>"` | 拒绝 + 把反馈作为工具结果回灌给 agent，引导改用别的方案 | 想纠正而不是终止 |
| `heb answer <SID> <RID> "<value>" [--custom] [--cancel]` | 回答 agent 用 `AskFollowup` 工具问的问题 | 收到 `question_requested` 后 |
| `heb stop <SID>` | 设 cancel flag，立刻中断当前 run | 跑飞了 / 死循环 |
| `heb mode <SID> <MODE>` | 切换 run mode：`default / plan-mode / auto-mode / yolo` | 下一轮起生效 |
| `heb ping <SID>` | 检测 daemon 存活，返回 `{"session_id":...}` | 写守护脚本 |
| `heb list-sessions` | 扫 `~/.hebbian/cli-sockets/` 列出所有存活 daemon，自动清理死 socket | 多 AI 并发调试时发现其他 AI 起的 daemon |
| `heb model-io <SID>` | 拉当前 session 已记录的 model_io.jsonl | 排查模型到底收到了什么 |
| `heb memory backfill [--session-id SID] [--limit N] [--offset N] [--reset-cursor] [--consolidate] [--execute] [--json] [--data-dir DIR]` | 重跑历史对话的记忆抽取；默认只预览，加 `--execute` 才调用模型并写盘 | 记忆系统回灌 / 质量评测 / 重建 links |

**`heb run` 与 `heb new` 的区别**：`heb new` 起一个**持久 daemon**（监听 socket，靠 `heb input/allow/answer` 交互驱动，适合调试 / 多轮）。`heb run` 是**一次性、无人值守**——起 in-process、跑一个 run、终态即退，没有 socket、不接交互审批。审批一律自动拒（reason 回灌 agent 让它换路子）、提问一律自动取消。配 `--yolo`（= `--mode yolo`）让界内编辑 + 命令全放、只拦 catastrophic 红线，无人值守一气呵成。

**`heb run --json` 结果 schema**（stdout **最后一行**，前面是与 daemon 一致的 NDJSON 事件流）：

```json
{
  "session_id": "...",
  "outcome": "done | suspended | failed | cancelled",
  "exit_code": 0,
  "final_text": "最后一条 assistant 文本",
  "tool_calls": 2,
  "files_changed": ["/abs/path/changed.txt"],
  "denied_approvals": 0,
  "cancelled_questions": 0,
  "duration_ms": 6698,
  "error": null
}
```

评测 / 脚本只需 `heb run ... --json | tail -n1 | jq` 抓最后一行。中间 NDJSON 事件可一并 tail 看实时进度。


**`heb answer` 三种形态**：

```bash
heb answer $SID $RID "猫"            # 选项 label（默认）
heb answer $SID $RID "紫色" --custom # 自由文本
heb answer $SID $RID "" --cancel    # 用户取消提问
```

**返回约定**：

- 成功 → exit 0，stdout 可能有一行 pretty-print JSON
- 失败 → exit 非 0，stderr 有人话错误（如 `未找到 request_id: ...` 表示该 ID 已被解析过）

---

## 3. 完整事件参考

daemon stdout 是 **NDJSON 流**——每行一个 JSON 对象，字段 `event` 是事件类型。下表是全部事件：

| `event` | 字段 | 含义 |
|---------|------|------|
| `started` | `session_id` | daemon 就绪，第一条事件 |
| `run_started` | — | 一轮 agent loop 开始 |
| `run_finished` | `input_tokens, output_tokens, cache_read_tokens, duration_ms` | 一轮正常结束 |
| `run_failed` | `error` | 一轮失败（provider 4xx / 网络 / panic） |
| `run_cancelled` | — | `heb stop` 触发 |
| `run_suspended` | `reason` | 等待 HITL 决策（权限 / 提问）时挂起。`reason` ∈ background_task/cron/manual（小写规范形态，三 surface 一致） |
| `run_resumed` | `cause` | HITL 决策到位后恢复。`cause` 如 `bg_task_finished:<id>` / `cron_fired:<reason>` / `user_message_arrived` / `manual_resume` |
| `text_delta` | `text` | 流式文本片段（按 token 增量） |
| `text_done` | `full_text` | 一段连续文本结束的全文 |
| `reasoning` | `text` | 思考链（部分模型有） |
| `tool_start` | `id, name, input` | 工具开始执行（input 是 JSON 对象） |
| `tool_done` | `id, result, duration_ms` | 工具执行完，`result` 是字符串结果（含错误信息） |
| `permission_requested` | `request_id, kind, tool_name, summary, risk` | 等待审批。`risk` ∈ critical/high/medium/low（小写规范形态，三 surface 一致） |
| `permission_resolved` | `request_id, decision` | 审批结果（含自动审批）。`decision` ∈ allow_once/allow_and_remember/deny/deny_with_feedback（snake_case 规范形态） |
| `question_requested` | `request_id, question, options[{label,description}], multi` | agent 用 `AskFollowup` 工具问问题 |
| `question_answered` | `request_id` | 问题被回答（不重复回答内容） |
| `run_mode_changed` | `from, to` | mode 切换 |
| `run_edits_committed` | `run_id, files[{real_path, action, before_bytes, after_bytes}]` | 一个 Run 跑完后本次文件净变化汇总（§4.13）。`action` ∈ create/modify/overwrite/delete。无文件变化的 Run 不发本事件 |
| `error` | `message` | 非致命错误通告 |

---

## 4. 自主调试常用 pattern

### 4.1 跑一句话 → 等结果（同步阻塞风格）

```bash
heb input $SID "$task"
# 轮询 log 直到 run_finished 或 run_failed
while true; do
  last=$(tail -n1 /tmp/heb.log)
  ev=$(echo "$last" | jq -r .event)
  case $ev in
    run_finished|run_failed|run_cancelled) break ;;
    permission_requested)
      rid=$(echo "$last" | jq -r .request_id)
      heb allow $SID $rid    # 或按 tool_name / risk 决策
      ;;
  esac
  sleep 0.5
done
```

### 4.2 自动审批策略（白名单 / 黑名单）

```bash
# 持续 tail 事件流，按 tool_name 决策
tail -f /tmp/heb.log | while read line; do
  ev=$(echo "$line" | jq -r .event)
  if [ "$ev" = "permission_requested" ]; then
    tool=$(echo "$line" | jq -r .tool_name)
    rid=$(echo "$line" | jq -r .request_id)
    case $tool in
      Read|Grep|Glob)            heb allow $SID $rid session ;;
      Write|Edit)                heb allow $SID $rid once ;;
      Bash)                      heb deny-feedback $SID $rid "本测试禁止 Bash" ;;
      *)                         heb deny $SID $rid ;;
    esac
  fi
done
```

### 4.3 复现一个 bug

```bash
# 1. 起 daemon，限定 provider/model 锁定可复现性
heb new --provider=$PID --model=claude-sonnet-4-6 --workdir $REPRO_DIR > $LOG 2>&1 &
sleep 1; SID=$(jq -r '.session_id' < <(head -n1 $LOG))

# 2. 灌入触发 bug 的输入
heb input $SID "$(cat ./repro-prompt.txt)"

# 3. 用脚本 4.1 跑到结束
# 4. 看 session 落盘
cat ~/.hebbian/sessions/$SID/session.jsonl | jq -s '.'
# 5. 如果有问题，看完整模型 IO
HEBBIAN_DUMP_MODEL_IO=1 重跑；之后看 ~/.hebbian/sessions/$SID/model_io.jsonl
```

### 4.4 检验中途插队是否生效

```bash
heb input $SID "请慢慢写一篇长文章"
sleep 2
heb input $SID "等等改主题为太空"     # 应在下一次 model.request 前作为新 user message 加入
# log 里应看到 run 继续，模型上下文里多了第二条 user message
```

### 4.5 复现 HITL 死锁

```bash
heb input $SID "请用 Write 工具…"
# 看到 permission_requested 但不响应
sleep 30
heb ping $SID    # daemon 仍应活着
heb stop $SID    # cancel flag 立刻打断，run_cancelled 出现
```

---

## 5. 数据持久化位置

所有状态在 `~/.hebbian/`（与 Desktop 共享，文件锁保护并发写）：

| 路径 | 内容 |
|------|------|
| `~/.hebbian/cli-sockets/<SID>.sock` | daemon 的 Unix socket |
| `~/.hebbian/sessions/<SID>/session.jsonl` | 完整对话历史（每行一条 Message） |
| `~/.hebbian/sessions/<SID>/session.json` | session 元数据（workdir / provider / mode / token stats） |
| `~/.hebbian/sessions/<SID>/rules.json` | session 级权限规则 |
| `~/.hebbian/sessions/<SID>/model_io.jsonl` | 完整模型请求/响应（需 `HEBBIAN_DUMP_MODEL_IO=1`） |
| `~/.hebbian/sessions/<SID>/partial/<msg_id>.partial.jsonl` | 进行中的增量输出（崩溃恢复用） |
| `~/.hebbian/permissions.json` | 全局权限规则 |
| `~/.hebbian/projects/<PID>.json` | 项目（workspace）配置 |
| `~/.hebbian/providers.json` | provider/model 配置 |

**调试小窍门**：

- 看实际发给模型的请求：`tail -f ~/.hebbian/sessions/$SID/model_io.jsonl | jq '.request | .messages[-3:]'`
- 看历史对话：`cat ~/.hebbian/sessions/$SID/session.jsonl | jq -c '{role, parts: [.parts[]? | .type]}'`
- 重置 session 权限规则：`rm ~/.hebbian/sessions/$SID/rules.json`
- 把 session 倒回 N 轮前：手动截断 `session.jsonl` 末尾的若干行（每个 message 独占一行）

---

## 6. 原理

> 想知道为什么这样设计、怎么排查 daemon 自身的问题，往下读。

### 6.1 整体架构

```
┌────────────────────────┐         ┌──────────────────────────────┐
│  heb input/allow/...   │ 1 cmd / │  heb new (daemon 进程)        │
│  (CLI 客户端进程)        │ <─────> │  ┌────────────────────────┐  │
│                        │ 1 resp  │  │  Unix socket listener   │  │
└────────────────────────┘  JSON   │  │  (~/.hebbian/cli-sockets│  │
                                   │  │   /<SID>.sock)          │  │
                                   │  └─────────┬──────────────┘  │
                                   │            │                  │
                                   │  ┌─────────▼──────────────┐  │
                                   │  │  DaemonState (Arc)      │  │
                                   │  │  - pending_approvals    │  │
                                   │  │  - pending_questions    │  │
                                   │  │  - cancel_flag          │  │
                                   │  │  - pending_inputs       │  │
                                   │  │  - input_tx             │  │
                                   │  └─────────┬──────────────┘  │
                                   │            │                  │
                                   │  ┌─────────▼──────────────┐  │
                                   │  │  run_turn(text)         │  │
                                   │  │  = Desktop send_and_save│  │
                                   │  │  + DaemonObserver       │  │
                                   │  └─────────┬──────────────┘  │
                                   │            │                  │
                                   │  ┌─────────▼──────────────┐  │
                                   │  │  agent_core            │  │ → stdout NDJSON
                                   │  │  (CoreSession/Harness) │  │
                                   │  └────────────────────────┘  │
                                   └──────────────────────────────┘
```

`heb new` 是一个长期运行的进程，全部状态在内存里靠 `Arc<DaemonState>` 共享。每条 IPC 命令由独立 tokio task 处理（短链接：连上 → 一来一回 JSON → 关闭）。事件不通过 socket 推送，而是直接 print 到 daemon 的 stdout（写脚本时一律重定向到文件再 tail）。

### 6.2 IPC 协议

**传输层**：Unix domain socket，路径 `~/.hebbian/cli-sockets/<session_id>.sock`。每次客户端短连接 → 写一行 JSON `IpcCommand` → 读一行 JSON `IpcResponse` → 关闭。

**`IpcCommand` 形如**：

```json
{"type":"send","text":"hello"}
{"type":"allow","request_id":"perm_xxx","scope":"once"}
{"type":"deny_with_feedback","request_id":"perm_xxx","feedback":"改用 Edit"}
{"type":"answer","request_id":"perm_xxx","kind":"selected","value":"猫"}
{"type":"stop"}
```

**`IpcResponse` 形如**：

```json
{"ok":true}
{"ok":true,"data":{"session_id":"..."}}
{"ok":false,"error":"未找到 request_id: perm_xxx"}
```

**事件流 `DaemonEvent`**：daemon 进程的 stdout 是不可寻址的纯 NDJSON 流，所有发生在 agent_core 内部的事件按时间顺序输出。事件结构定义在 [`apps/cli/src/ipc.rs`](../apps/cli/src/ipc.rs)。

### 6.3 HITL 阻塞模型（核心难点）

Hebbian 的 agent loop 内部需要在 `permission_requested` / `question_requested` 时 **真正阻塞**，等用户决策——不能 busy poll，不能 panic，不能丢消息。

**实现要点**：

1. agent-core 通过 `TurnObserver::on_permission_request(&mut self, req) -> Option<Decision>` 回调通知 surface
   - 返回 `Some(decision)` → harness 立即调 `resolve_permission(decision)` 继续
   - 返回 `None` → harness 把请求挂起，等 surface 异步调 `resolve_permission`（Desktop 的 HitlGate 走这条路）
2. **CLI daemon 走 `Some(decision)` 路径**，避免引入 HitlGate：
   - `DaemonObserver::on_permission_request` 创建一个 `tokio::sync::oneshot::channel::<ApprovalDecision>()`
   - 把 sender 按 `request_id` 存进 `DaemonState.pending_approvals: Mutex<HashMap<_, _>>`
   - 在 observer 里 **同步 await** receiver（observer 方法是 `async`，可以直接 `.await`）
   - 同时通过 `state.emit(PermissionRequested { request_id, ... })` 把请求发给 stdout
3. `heb allow/deny/deny-feedback` 命令到达时，`handle_command`：
   - 从 `pending_approvals` 里 `remove(request_id)` 取出 sender
   - `tx.send(decision)` 把决策塞回 oneshot
   - observer 端 `.await` 返回 → observer 返回 `Some(decision)` → harness 自动 resolve
4. `question_requested` 完全同构，走 `pending_questions: Mutex<HashMap<_, oneshot::Sender<UserAnswer>>>`

**为什么不复用 Desktop 的 HitlGate？** HitlGate 是 Desktop 把异步事件投递回 GUI 主线程的中间层，CLI 没有这个跨线程边界（observer 自己就在 tokio runtime 里），oneshot 直接阻塞最干净。

**`run_suspended` / `run_resumed` 事件**：HITL 等待期间 daemon 不算"卡死"——agent loop 真的暂停在 observer 的 `.await` 上，等 oneshot 到达。两个事件是 observer 提供给客户端的状态机提示，便于脚本判断"现在该我决策了"。

### 6.4 流式中的 user message 注入

`heb input` 在有 active run 时不是排队等待，而是 **直接注入当前 run**：

1. `run_turn` 启动时把一个 `Arc<Mutex<Vec<PendingUserInput>>>` 共享给 agent_loop（通过 `RuntimeHandle.pending_inputs`）
2. 同时把它存进 `DaemonState.pending_inputs`（`Mutex<Option<…>>`）
3. `heb input` → `handle_command` 检测 `state.is_active()` → 调 `pending.lock().push(input)`
4. agent_loop 在 **下一次 `model.request` 之前** drain pending_inputs，作为新的 user message 加入 transcript

效果：用户说"长篇大论"，agent 写了一半，用户再说"换主题"，下一次模型调用就能看到第二条 user message 并切换方向。如果在工具执行中插队，工具结果回到模型时新 user message 会和 tool_result 一起出现在 transcript 末尾。

### 6.5 多轮持久化与上下文重建

每次 `run_turn` 开头 **重新 `sessions::load(data_dir, session_id)` 从磁盘读完整 jsonl**，rebuild 出 transcript 喂给 model。这意味着：

- daemon 不在内存维护"对话历史"，只有 transient 的 pending 状态
- 杀掉 daemon 重新 `heb new --session-id <SID>` 即可无缝续聊
- Desktop 和 CLI 共用同一个 session 时不会互相覆盖（文件锁串行化写入，每次完整 load + append）
- 单轮内的 `text_delta` 也通过 `PartialFileWriter` 写到 `partial/<msg_id>.partial.jsonl`，下次 `run_turn` 启动时 `recover_and_save_interrupted_partials` 会把中断内容追加到 `session.jsonl` 并打上 `Interrupted` 标记（详见 2026-05-20 changelog）

### 6.6 cancel 语义

`heb stop` 走的是 `CancelFlag = Arc<AtomicBool>`：

1. `run_turn` 启动时把 `cancel_flag` 存进 `DaemonState.cancel_flag` 并传给 agent_loop
2. `heb stop` → `handle_command` 调 `state.stop()` → `flag.store(true, Ordering::SeqCst)`
3. agent_loop 在每个 token / 工具边界检查 flag，True 就中断
4. observer 发 `run_cancelled` 事件

**注意**：cancel 不会撤销已发出的工具调用（比如已经写出去的文件不会回滚），只是停止后续步骤。要安全实验，配 `--workdir` 到一个临时目录或开 git worktree。

### 6.7 模式（RunMode）

`heb mode` 改的是 `DaemonState.run_mode: Mutex<RunMode>`，**只对下一次 `run_turn` 生效**——当前 run 在启动瞬间已经把 mode 快照传给 agent_loop。

四种 mode 决定工具白名单 / 自动审批策略（详见架构.md §4.4.5）：

| mode | 含义 |
|------|------|
| `ask-before-edits` | 默认。写操作（Write/Edit/Bash）每次问 |
| `edit-automatically` | 写操作自动放行，但敏感操作（删文件 / 网络）仍问 |
| `plan-mode` | 只允许只读工具 + AskFollowup，禁止任何副作用 |
| `auto-mode` | 全自动，所有工具默认放行（CI / 自动化用） |

### 6.8 与 Desktop 的关系

CLI daemon 的 `run_turn` 是 Desktop `apps/desktop/src/chat.rs` 中 `send_and_save_in_data_dir` 的等价实现——同样的 `CoreSession` 构造、同样的 `SessionConfig`、同样的工具集（`default_tools`）、同样的 HookManager / ReadStateTracker / EditsWorktree。区别只在 surface：

| | Desktop | CLI daemon |
|--|---------|------------|
| 事件出口 | Tauri event → React store | NDJSON stdout |
| HITL 等待 | HitlGate（跨线程通道） | `oneshot` 直接 await |
| 用户输入 | `inject_user_message` Tauri 命令 | `heb input` IPC 命令 |
| 数据目录 | `~/.hebbian/` | `~/.hebbian/`（同一处） |

所以 Desktop 能复现的 bug，CLI 一定能复现；反之亦然。这是 CLI 作为调试工具的核心价值。

---

## 7. 故障速查

| 现象 | 可能原因 | 排查 |
|------|----------|------|
| `heb input` 报 `无法连接 daemon` | daemon 没起 / socket 残留 | `ls ~/.hebbian/cli-sockets/`；`ps aux \| grep heb` |
| `run_failed: ...400 No tool output found for...` | session.jsonl 里有半截 tool_call | 见 changelog 2026-05-20「跳过未完成历史 tool_call」 |
| `run_failed: 401/403` | provider token 过期 | `~/.hebbian/providers.json` 重新填 token |
| `permission_requested` 一直不出来 | 该工具已被 session/global 规则自动放行 | `cat ~/.hebbian/sessions/$SID/rules.json` 和 `~/.hebbian/permissions.json` |
| `heb allow` 报 `未找到 request_id` | 该 request 已被规则/上一条命令解析掉 | 看 `permission_resolved` 事件确认 |
| 事件流卡住没 `run_finished` | HITL 等待中 / 死锁 | grep `run_suspended` 或 `permission_requested`；ping 看 daemon 是否活着；最后 `heb stop` |
| `heb mode` 切了但没生效 | 当前 run 已经在跑 | 等当前 run 结束或先 `heb stop` |

---

## 8. 不会做 / 暂不支持

- **没有事件回放**：daemon 退出后 stdout 流就丢了。要持久化把 stdout 重定向到文件，或加 `--data-dir` 然后看 `session.jsonl` / `model_io.jsonl`
- **没有身份认证**：socket 走默认 umask，本地单用户场景安全；多用户机器上不要把数据目录放共享路径
- **不支持远程 daemon**：socket 只能本地连接，要远程驱动请 ssh 进去再起 daemon
- **`heb mode` 只影响下一轮**：当前进行中的 run mode 是冻结的；切到 `auto-mode` 不会让当前 run 突然不再问审批

---

## 9. `hebweb` —— 让 AI 看到并操作真实前端

`hebweb` 是 Hebbian 的浏览器 surface：HTTP + WebSocket server，跑同一份 React 前端代码，数据走真实 agent_core，与 Desktop 共享 `~/.hebbian/`。AI 配合 Playwright 即可看到 DOM、截图、点击界面、定位 UI bug。

### 9.1 何时用 hebweb 而不是 heb CLI

`heb` 覆盖 agent_core 内部行为；UI 层的问题必须借 hebweb：

| 适合 hebweb | 仍适合 heb CLI |
|------------|---------------|
| 前端组件渲染错乱 / 样式 bug | agent 行为问题（工具调用错、回答跑偏） |
| 工具调用详情卡片显示问题 | 多轮上下文 / 缓存命中 |
| 流式 bubble 折叠 / portal 渲染 | 权限审批流程本身 |
| 侧边栏 / 输入框 / 设置弹窗 | provider 协议问题 |
| 输入队列面板的状态机问题 | session 持久化 / 崩溃恢复 |
| 审批弹窗 / 提问弹窗的 UX | HITL 阻塞 / cancel 语义 |

### 9.2 启动

```bash
cargo build -p hebbian-web-server
# 一次性构建前端（仅首次或前端代码变更后）
cd apps/desktop/frontend && pnpm install && pnpm build && cd -

# 起 server——standalone 模式，不需要 desktop 在跑
./target/debug/hebweb --port 38080 --static-dir apps/desktop/dist
# → "hebweb listening on http://127.0.0.1:38080  (data_dir=/Users/.../.hebbian)"
```

参数：

```
hebweb [OPTIONS]

--addr <ADDR>          监听地址（默认 127.0.0.1:3030）
-p, --port <PORT>      监听端口（覆盖 --addr 的端口）
--data-dir <DIR>       数据目录（默认 ~/.hebbian）
--static-dir <DIR>     前端静态文件目录（默认自动探测 apps/desktop/frontend/dist）
```

健康检查：

```bash
curl -s http://127.0.0.1:38080/healthz
# {"active_sessions":[],"data_dir":"...","ok":true,"version":"0.1.0"}
```

### 9.3 多 AI 并发：两种隔离模型

**模型 A（推荐）：一个 hebweb 进程 + 多 session**

```
hebweb --port 38080
        │
        ├─ AI 1 浏览器 → WS subscribe session_A
        ├─ AI 2 浏览器 → WS subscribe session_B
        └─ AI 3 浏览器 → WS subscribe session_C
```

每个 WS 连接通过首条 `subscribe` 消息绑定一个 `session_id`，server 按 session_id 路由 invoke 和 event。多个 AI 共享同一进程内的 agent_core，但 `SessionRuntime` 完全独立（各有自己的 cancel_flag / pending_inputs / pending_approvals），互不阻塞。

**模型 B：每个 AI 一个独立 hebweb 进程**

```
hebweb --port 38080 --data-dir /tmp/hebweb-ai1
hebweb --port 38081 --data-dir /tmp/hebweb-ai2
hebweb --port 38082 --data-dir /tmp/hebweb-ai3
```

进程间通过 `~/.hebbian/` 文件锁保证写安全；用 `--data-dir` 可以做到完全隔离的数据目录（甚至不共享 session 列表）。

### 9.4 WS 协议

每条 WS 连接是 **JSON 行**：

**client → server**：

```json
{"type":"subscribe","session_id":"<SID>"}
{"type":"invoke","id":"<uuid>","cmd":"send_message","args":{"text":"..."},"session_id":"<SID>"}
{"type":"unsubscribe"}
```

**server → client**：

```json
{"type":"hello","server_version":"0.1.0"}
{"type":"subscribed","session_id":"<SID>"}
{"type":"invoke_response","id":"<uuid>","ok":true,"data":{...}}
{"type":"invoke_response","id":"<uuid>","ok":false,"error":"..."}
{"type":"event","session_id":"<SID>","name":"engine-event","payload":{...}}
```

`engine-event` 的 payload 是与 desktop Tauri emit 完全一致的 `EngineEvent` JSON（`text_delta` / `tool_start` / `permission_requested` / `user_question_requested` / ...）。前端代码不需要任何改动。

### 9.5 v1 支持的 invoke 命令

> **设计原则**：hebweb 内部持有一个 `Arc<LocalCoreClient>`（与 desktop 同一个 facade，定义在 [crates/agent-core/src/core_client/](../crates/agent-core/src/core_client/)）。CoreClient trait 已经把 25+ 业务方法集中在一个对象上，desktop 的 Tauri command body 大多是一行 `core(&app)?.xxx()` wrap——hebweb 同样直接调 `state.core.xxx()`，避免重复 wrap storage / model_gateway。**未来 agent_core 给 CoreClient 加新方法时 hebweb 自动获得**。

**已镜像（35 个）—— 主对话 + 配置管理 + 业务面板全套**：

| 分类 | 命令 | 用途 |
|------|------|------|
| 主对话 | `list_sessions / get_session / create_session` | session 列表/详情/新建 |
| 主对话 | `send_message / inject_user_message` | 发用户消息（active 注入 / idle 开新 run）/ 强制注入 |
| 主对话 | `approve_permission / answer_question / cancel_message` | HITL 审批 / 回答提问 / 中断 |
| Session 管理 | `rename_session / delete_session / fork_session` | 重命名 / 删除 / 分叉 |
| Session 管理 | `truncate_after / truncate_inclusive / search_sessions` | 截断 / 全局搜索 |
| Session 管理 | `update_session_config` | 切 provider/model/stream/reasoning |
| RunMode | `get_run_mode / set_run_mode` | 模式切换 |
| RunMode | `get_force_automode / set_force_automode` | 强制 auto 开关（内存态） |
| Providers | `get_providers / save_providers / upsert_provider / get_provider / list_provider_presets` | provider CRUD + 16 个预设 |
| Providers | `fetch_provider_models / test_provider_model` | 真正调外部 API 拉模型列表 / 测试 API Key 通不通（走 LocalCoreClient） |
| 工具 / 规则 | `list_tools` | UI 工具菜单（走 LocalCoreClient） |
| 权限规则 | `list_permission_rules / remove_permission_rule / clear_permission_rules` | 列/删/清 PermissionStore（走 LocalCoreClient） |
| Prompts | `list_prompts / upsert_prompt / delete_prompt / set_default_prompt` | Agent prompt CRUD |
| Projects | `list_projects / save_project / delete_project` | 项目（workspace）CRUD |
| Settings | `get_settings / save_settings` | 应用级偏好 |

**已验证的 UI 流**（Playwright 实跑通过）：

- ✅ 起 hebweb → 浏览器加载 → 侧边栏完整渲染
- ✅ 打开供应商配置弹窗 → 添加 DeepSeek 预设 → 填 API Key → 保存 → `providers.json` 落盘
- ✅ 打开设置弹窗 → 切 Agent 配置 tab → 保存 → `settings.json` 落盘
- ✅ 新建对话 → 进对话视图（标题、Agent 选择、模型选择、textarea、Token 用量全渲染）
- ✅ 侧边栏 hover → 重命名/删除按钮可点 → rename inline 输入 → 持久化 + UI 同步
- ✅ 点对话设置 → SessionSettingsDialog 弹出（Agent / 系统指令 / 字段覆盖）
- ✅ session 落盘到 `~/.hebbian/sessions/<id>/meta.json` + `session.jsonl`，与 desktop 完全同构

**未镜像（暂走 desktop）**：

| 命令 | 原因 |
|------|------|
| `compact_session` / `preview_session_payload` / `get_context_usage` / `generate_session_title` | 走 chat / context 管线，不在 CoreClient trait 里。下次可单独 wrap |
| `discover_rules_files` | 需要 workspace 上下文构建 |
| `list_background_tasks` / `kill_background_task` | 需要 background registry |
| `list_edits` / `diff_edit` / `revert_edit` / `edits_worktree_status` | 需要 EditsWorktree git 集成 |
| `attach_path` / `approve_path_access` | 与 PathAccess HITL 弹窗联动 |
| `import_vscode_project` / `import_project_file` | 需要 file dialog 选文件（Tauri native） |
| `update_session_settings` | 与 `update_session_config` 部分重复 |
| `oauth_*`（13 个） / `deepseek_login` | OAuth 浏览器跳转 + token 交换，AI 调试场景基本用不上 |

**前端碰到 not_implemented 的行为**：next-level 面板（Edits 历史、Token 用量进度环、Preview Payload、OAuth 登录）打开时会拿到错误，但 **不阻塞主对话流**——sidebar / 对话视图 / 配置弹窗 / Agent 切换 / RunMode 切换 / 审批 / 提问 / 删除/重命名 全部能用。

### 9.6 AI 自主驱动 hebweb（Playwright 模板）

```bash
# 一次性安装
pnpm dlx playwright install chromium

# 写一个 Playwright 脚本，让 AI 完整复现 UI bug
cat > /tmp/repro.mjs <<'EOF'
import { chromium } from "playwright";
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
page.on("console", (msg) => console.log("[browser]", msg.text()));
await page.goto("http://127.0.0.1:38080/");
// 等到前端加载完
await page.waitForSelector('[data-testid="chat-input"], textarea', { timeout: 10000 });

// 1. 截图当前 UI
await page.screenshot({ path: "/tmp/before.png", fullPage: true });
// 2. 模拟用户输入
await page.fill("textarea", "请用 Write 工具写 /tmp/work/hello.txt");
await page.keyboard.press("Enter");
// 3. 等审批弹窗出现
await page.waitForSelector('[role="dialog"]', { timeout: 30000 });
await page.screenshot({ path: "/tmp/permission-dialog.png" });
// 4. dump DOM 给 AI 分析
const html = await page.content();
console.log("DOM length:", html.length);
// ...
await browser.close();
EOF
node /tmp/repro.mjs
```

AI 拿到 `before.png` / `permission-dialog.png` 截图 + DOM HTML 即可定位 UI bug。

#### 9.6.1 推荐：用 `playwright-cli`（一行一动，AI 直接可控）

```bash
# 起 chromium 加载 hebweb
playwright-cli open http://127.0.0.1:38080

# 看当前页面 ARIA snapshot（自动包含 ref=e3, e4... 给后续 click/hover 用）
playwright-cli --raw snapshot

# 控制台 + 网络
playwright-cli console
playwright-cli network

# 操作元素（用 snapshot 给的 ref）
playwright-cli click e17                      # 点 "新建对话" 按钮
playwright-cli fill "textarea" "你好"          # 用 CSS 选择器
playwright-cli press Enter
playwright-cli hover e3224                    # hover 某个 button（如 OAuth 启动）
playwright-cli mousemove 1140 680             # 像素级 hover（hover 圆环 tooltip）
playwright-cli screenshot --filename=ui.png   # 截图（路径必须在仓库根目录或允许目录）

# 取 DOM 内任意状态（eval 直接跑 JS）
playwright-cli eval "() => window.__hebStore.getState().sessions ? Object.keys(window.__hebStore.getState().sessions).length : 0"

# 关浏览器
playwright-cli close
```

#### 9.6.2 实战示例：hover 右下角 Token 用量小圆环显示 tooltip

hebweb 浏览器里 hover 右下角 Token 用量小圆环会弹 native tooltip 显示 `上下文 % · 用量/总额`——这是 `get_context_usage` 实时返回的真实数据。`tooltip` 是 native 的，必须 **像素级 mousemove** 触发：

```bash
# 1. 起 hebweb（standalone）
./target/debug/hebweb --port 38080 --static-dir apps/desktop/dist &
sleep 2

# 2. 打开浏览器 + 点进一个已有 session
playwright-cli open http://127.0.0.1:38080
sleep 2
playwright-cli eval "() => {
  const li = [...document.querySelectorAll('aside ul li')].find(l => l.textContent?.includes('Bash'));
  li?.querySelector('[class*=cursor-pointer]')?.click();
  return 'clicked';
}"

# 3. 找右下角 Token 用量按钮的位置
playwright-cli --raw eval "() => {
  const btn = [...document.querySelectorAll('button')].find(b => /上下文.*200/.test(b.getAttribute('aria-label') ?? ''));
  return btn ? JSON.stringify(btn.getBoundingClientRect()) : '{}';
}"
# {"x":1130,"y":670,"width":22,"height":22,...}

# 4. 像素级 hover 触发 native tooltip
playwright-cli mousemove 1140 680
sleep 1
playwright-cli screenshot --filename=context-usage-tip.png
# 截图右下能看到 "上下文 25% · 50.3k / 200.0k" tooltip
```

#### 9.6.3 注意

- `playwright-cli` 每条命令完成后自动回个 snapshot；不想要 raw text 就加 `--raw` 只输出结果
- `playwright-cli screenshot --filename=` 路径必须在仓库根目录或 `.playwright-cli/` 下（cli 有沙箱）；写 `.mjs` 用 `page.screenshot` library 路径不限
- JS `mouseover` 事件触发不了 OS native tooltip，**必须** `mousemove` 到像素位置
- 涉及用户真实 `~/.hebbian/` 的 session 时遵守 §9.10 五条安全规则

### 9.7 hebweb 调试自身 / 故障速查

| 现象 | 排查 |
|------|------|
| `curl /healthz` 502 / 拒绝连接 | hebweb 没起 / 端口被占；`lsof -i :38080` 查冲突 |
| 前端访问 404 | 没建 `dist/`；先 `pnpm build` 或加 `--static-dir <path>` |
| 前端连不上 WS | 端口跨域？hebweb 监听 `127.0.0.1` 不接受其他 host；本地访问没问题 |
| invoke 返回 `not implemented` | v1 范围外的命令；要么用 Desktop，要么等 v2 抽共享 commands |
| 多 AI 切换 session 后事件错乱 | 检查 WS subscribe 是否换了 session_id；server 是按连接 + session_id 路由 |
| WS 连接断开 | 看 hebweb stdout 日志（默认 INFO 级），多半是 client 侧关闭 |

### 9.8 hebweb 已知限制

- 42 个命令已镜像（覆盖主对话 + 配置管理 + 部分业务），剩余 ~24 个 desktop 专有命令（OAuth 14 / Edits 4 / preview_payload / file_dialog 2 / 等）尚未镜像，invoke 会拿到 `not_implemented` 错误。按需照 `chat_helpers.rs` 模式从 desktop lib.rs 搬过来
- 仅本地监听（`127.0.0.1` 或 `[::]:`），不要在公网开放
- 单 WS 连接同时只订阅一个 session（切 session 自动取消旧订阅）
- Tauri native 能力（系统通知 / 文件对话框 / tray / 全局快捷键）浏览器没有等价物——浏览器 surface 不模拟

---

## 9.9 hebweb 与 desktop 互不依赖

设计上 hebweb 是**独立的 surface**——自己持有 agent_core、自己跑 SessionRuntime / agent_loop、自己写 `~/.hebbian/`。不需要 desktop 跑着。

desktop 与 hebweb 通过 `~/.hebbian/` 文件锁共享数据（providers / sessions / settings / permissions）；两边同时跑也安全，但同一 session 同时操作 UI 会双份显示。

历史上曾实现过 "desktop invoke proxy bridge"——让 desktop 当 hebweb 的代理把所有 Tauri 命令转发出去（参见 git 历史 commit `54e008b` / `Add hebweb` 与 `Remove desktop bridge`）。删除原因：双进程依赖耦合复杂、心跳重连维护负担大、AI unattended 调试场景用不上。**hebweb 镜像命令到 standalone 是长期方向**——剩余 desktop 专有命令（OAuth 14 / Edits 4 / preview_payload / file_dialog 2 / etc）按需照 `chat_helpers.rs` 模式从 desktop lib.rs 搬过来。

---

## 9.10 AI 自主调试时的安全实践

hebweb 用真实 `~/.hebbian/` 时，任何操作都会**直接读写真实 session.jsonl / providers.json / settings.json**。**AI 必须遵守**：

1. **绝不在用户真实 session 上 send_message** —— 任何测试消息都会真的发给 LLM + 落盘 + 占 token + 污染历史。先 `create_session` 起一个 throw-away session 再操作
2. **删除测试 session 不要手动 `rm`** —— 走 `delete_session` 命令，让后端处理好 partial / lock / bg-task 等关联状态
3. **不要擅自 truncate jsonl** —— 用 `truncate_inclusive(id, messageId)` 命令，partial 会被一起清掉
4. **隔离 data_dir**：调试环境用 `hebweb --data-dir /tmp/<专用>` + `--static-dir` 指向 desktop dist，与用户真实 `~/.hebbian/` 完全分开
5. **session_id 路由要明确**：subscribe / invoke 都带 `session_id`；不要在调用前没确认就发命令——可能误打到用户当前在用的 session

### 9.10.1 安全测试模板

```bash
# 起一个完全隔离的 hebweb
mkdir -p /tmp/hebweb-test
echo '{"providers":[<复制需要的 provider>],"default_provider_id":"..."}' > /tmp/hebweb-test/providers.json
hebweb --port 38080 --data-dir /tmp/hebweb-test --static-dir apps/desktop/dist &

# Playwright 操作时所有数据都在 /tmp/hebweb-test，不污染真实环境
```



