# `heb` CLI — AI 自主调试操作手册

> **写给 AI**：你正在调试 Hebbian agent。本文档让你不依赖 Desktop GUI，纯命令行 + 文件流就能驱动 agent_core，并观察它的全部内部状态。读完即可上手。
>
> 适用场景：自动化回归、bug 复现、压测 prompt / 工具行为、CI 端到端验证、agent 自我调试 agent。

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
{"event":"permission_requested","request_id":"perm_…","kind":"tool_call","tool_name":"Write","risk":"Medium"}
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
| `heb input <SID> "<text>"` | 发用户输入。**自动判定**：无 active run 时开新 run；有 active run 时注入 pending 队列 | 提问、回答、补充上下文、流式中插队 |
| `heb allow <SID> <RID> [scope]` | 批准权限审批。`scope ∈ {once,session,project,global}`，默认 `once` | 收到 `permission_requested` 后 |
| `heb deny <SID> <RID>` | 拒绝审批（agent 收到工具失败结果） | 同上 |
| `heb deny-feedback <SID> <RID> "<反馈>"` | 拒绝 + 把反馈作为工具结果回灌给 agent，引导改用别的方案 | 想纠正而不是终止 |
| `heb answer <SID> <RID> "<value>" [--custom] [--cancel]` | 回答 agent 用 `AskFollowup` 工具问的问题 | 收到 `question_requested` 后 |
| `heb stop <SID>` | 设 cancel flag，立刻中断当前 run | 跑飞了 / 死循环 |
| `heb mode <SID> <MODE>` | 切换 run mode：`ask-before-edits / edit-automatically / plan-mode / auto-mode` | 下一轮起生效 |
| `heb ping <SID>` | 检测 daemon 存活，返回 `{"session_id":...}` | 写守护脚本 |

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
| `run_suspended` | `reason` | 等待 HITL 决策（权限 / 提问）时挂起 |
| `run_resumed` | `cause` | HITL 决策到位后恢复 |
| `text_delta` | `text` | 流式文本片段（按 token 增量） |
| `text_done` | `full_text` | 一段连续文本结束的全文 |
| `reasoning` | `text` | 思考链（部分模型有） |
| `tool_start` | `id, name, input` | 工具开始执行（input 是 JSON 对象） |
| `tool_done` | `id, result, duration_ms` | 工具执行完，`result` 是字符串结果（含错误信息） |
| `permission_requested` | `request_id, kind, tool_name, summary, risk` | 等待审批 |
| `permission_resolved` | `request_id, decision` | 审批结果（含自动审批） |
| `question_requested` | `request_id, question, options[{label,description}], multi` | agent 用 `AskFollowup` 工具问问题 |
| `question_answered` | `request_id` | 问题被回答（不重复回答内容） |
| `run_mode_changed` | `from, to` | mode 切换 |
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

