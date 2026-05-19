"""
Claude Code 后台执行机制 —— 极简 Python 模拟

对应文档：hebbian/docs/claude-code-后台执行机制.md

跑得起来。展示：
  1. 模型返回什么形状的 tool_call
  2. Bash 同步 / Bash 后台 / Monitor 怎么落地
  3. <task-notification> 怎么被"注入"主对话队列 = 自动唤醒
  4. TaskStop 的 owner 校验
  5. 令牌桶 + 暴流熔断 + 单行截断

依赖：纯标准库，Python 3.10+

运行： python3 claude-code-后台执行机制-demo.py
"""

from __future__ import annotations

import asyncio
import re
import time
import uuid
from dataclasses import dataclass, field
from enum import Enum


# ============================================================================
# 1. 数据模型
# ============================================================================

class TaskKind(str, Enum):
    LOCAL_BASH = "local_bash"   # Claude Code: kind="local_bash"
    MONITOR    = "monitor"      # Claude Code: kind="monitor"


class TaskStatus(str, Enum):
    RUNNING   = "running"
    COMPLETED = "completed"
    FAILED    = "failed"
    KILLED    = "killed"


@dataclass
class Task:
    """taskRegistry 的一条记录（对应 FjH() 注册的 Task）"""
    id: str
    kind: TaskKind
    status: TaskStatus
    agent_id: str               # owner —— 子 agent 不能 stop 别人的 task
    description: str
    command: str
    proc: asyncio.subprocess.Process | None = None
    started_at: float = field(default_factory=time.time)


# ============================================================================
# 2. NotificationInjector
#    对应 nkH() + UO({mode:"task-notification", priority:"next"})
#    把事件包成 <task-notification> XML，塞进对话队列的"下一轮 user message"位
# ============================================================================

class NotificationInjector:
    def __init__(self, queue: asyncio.Queue):
        self.queue = queue

    async def push(self, *, task_id: str, kind: str,
                   description: str, payload: str):
        xml = (
            f'<task-notification task_id="{task_id}" kind="{kind}">\n'
            f'  [{description}] {payload}\n'
            f'</task-notification>'
        )
        await self.queue.put({
            "role": "user",
            "content": xml,
            "system_origin": True,    # 标记：这是系统注入的，不是真人发的
        })


# ============================================================================
# 3. TokenBucket  ——  对应 zA6(OA6=10, ff_=2000)
#    稳态 0.5 行/秒，突发 10 行。防止 monitor 日志雷暴吞 context。
# ============================================================================

class TokenBucket:
    def __init__(self, capacity: int = 10, refill_interval: float = 2.0):
        self.capacity = capacity
        self.refill_interval = refill_interval
        self.tokens = capacity
        self.last_refill = time.monotonic()

    def try_consume(self) -> bool:
        now = time.monotonic()
        ticks = int((now - self.last_refill) / self.refill_interval)
        if ticks > 0:
            self.tokens = min(self.capacity, self.tokens + ticks)
            self.last_refill += ticks * self.refill_interval
        if self.tokens > 0:
            self.tokens -= 1
            return True
        return False


# ============================================================================
# 4. TaskRegistry  ——  进程级单例
# ============================================================================

class TaskRegistry:
    def __init__(self):
        self._tasks: dict[str, Task] = {}

    def register(self, t: Task):                 self._tasks[t.id] = t
    def get(self, tid: str) -> Task | None:      return self._tasks.get(tid)
    def all_running(self) -> list[Task]:
        return [t for t in self._tasks.values() if t.status == TaskStatus.RUNNING]

    def kill(self, tid: str) -> Task | None:
        """对应 KVH(taskId, registry)"""
        t = self._tasks.get(tid)
        if not t or t.status != TaskStatus.RUNNING:
            return None
        if t.proc and t.proc.returncode is None:
            try:
                t.proc.kill()
            except ProcessLookupError:
                pass
        t.status = TaskStatus.KILLED
        return t


# ============================================================================
# 5. BashTool  ——  同步 + 后台两种模式
# ============================================================================

class BashTool:
    name = "Bash"
    ASSISTANT_BLOCKING_BUDGET_MS = 15_000     # A83
    DEFAULT_TIMEOUT_MS           = 120_000    # _HH 默认

    def __init__(self, registry, injector, agent_id="main"):
        self.registry = registry
        self.injector = injector
        self.agent_id = agent_id

    async def call(self, *, command: str, description: str = "",
                   run_in_background: bool = False,
                   timeout_ms: int | None = None) -> dict:
        if run_in_background:
            return await self._bg(command, description)
        return await self._fg(command, (timeout_ms or self.DEFAULT_TIMEOUT_MS) / 1000)

    async def _fg(self, command: str, timeout_s: float) -> dict:
        proc = await asyncio.create_subprocess_shell(
            command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        try:
            stdout, stderr = await asyncio.wait_for(proc.communicate(),
                                                    timeout=timeout_s)
        except asyncio.TimeoutError:
            proc.kill()
            return {"interrupted": True, "stdout": "", "stderr": "timeout"}
        return {
            "interrupted": False,
            "stdout": stdout.decode(errors="replace"),
            "stderr": stderr.decode(errors="replace"),
            "code": proc.returncode,
        }

    async def _bg(self, command: str, description: str) -> dict:
        tid = f"bash_{uuid.uuid4().hex[:6]}"
        proc = await asyncio.create_subprocess_shell(
            command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        self.registry.register(Task(
            id=tid, kind=TaskKind.LOCAL_BASH, status=TaskStatus.RUNNING,
            agent_id=self.agent_id, description=description,
            command=command, proc=proc,
        ))
        # 进程结束后注入 <task-notification kind="bash_finished">（对应 $w6）
        asyncio.create_task(self._reap(tid))
        return {
            "backgroundTaskId": tid,
            "content": (f"Command running in background (id={tid}). "
                        "You will be notified when it completes. "
                        "Keep working, do not poll."),
        }

    async def _reap(self, tid: str):
        t = self.registry.get(tid)
        assert t and t.proc
        stdout, _ = await t.proc.communicate()
        if t.status == TaskStatus.RUNNING:
            t.status = (TaskStatus.COMPLETED
                        if t.proc.returncode == 0 else TaskStatus.FAILED)
        tail = stdout.decode(errors="replace")[-400:]
        await self.injector.push(
            task_id=t.id, kind="bash_finished", description=t.description,
            payload=f"exit={t.proc.returncode}, {len(stdout)}B\ntail:\n{tail}",
        )


# ============================================================================
# 6. MonitorTool  ——  流式事件（核心机制：令牌桶 + 暴流熔断 + 行注入）
# ============================================================================

class MonitorTool:
    name = "Monitor"
    DEFAULT_TIMEOUT_MS           = 300_000     # Vc7
    MAX_LINE_LENGTH              = 500         # KA6
    SUPPRESSION_KILL_THRESHOLD_S = 30.0        # dF7

    def __init__(self, registry, injector, agent_id="main"):
        self.registry = registry
        self.injector = injector
        self.agent_id = agent_id

    async def call(self, *, command: str, description: str,
                   timeout_ms: int | None = None,
                   persistent: bool = False) -> dict:
        timeout_ms = timeout_ms or self.DEFAULT_TIMEOUT_MS
        tid = f"mon_{uuid.uuid4().hex[:6]}"
        proc = await asyncio.create_subprocess_shell(
            command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        self.registry.register(Task(
            id=tid, kind=TaskKind.MONITOR, status=TaskStatus.RUNNING,
            agent_id=self.agent_id, description=description,
            command=command, proc=proc,
        ))
        asyncio.create_task(self._stream(tid, persistent=persistent,
                                          timeout_s=timeout_ms / 1000))
        return {
            "taskId": tid,
            "content": (
                f"Monitor started (task {tid}, "
                f"{'persistent' if persistent else f'timeout {timeout_ms}ms'}). "
                "You will be notified on each event. Do not poll or sleep. "
                "Events may arrive while you wait for the user — "
                "an event is NOT their reply."
            ),
        }

    async def _stream(self, tid: str, *, persistent: bool, timeout_s: float):
        """对应 xa5() + TA6() 行缓冲 + 令牌桶 + 暴流熔断"""
        t = self.registry.get(tid)
        assert t and t.proc and t.proc.stdout

        bucket = TokenBucket(capacity=10, refill_interval=2.0)
        suppressed = 0
        first_suppress_at: float | None = None
        ended = asyncio.Event()

        async def read_loop():
            nonlocal suppressed, first_suppress_at
            while True:
                line = await t.proc.stdout.readline()
                if not line or ended.is_set():
                    break

                line_str = line.decode(errors="replace").rstrip("\r\n").strip()
                if not line_str:
                    continue

                # 单行截断
                if len(line_str) > self.MAX_LINE_LENGTH:
                    line_str = line_str[:self.MAX_LINE_LENGTH] + "...(truncated)"

                if bucket.try_consume():
                    # 之前积压的被抑制行 → 先送 housekeeping
                    if suppressed > 0:
                        await self.injector.push(
                            task_id=tid, kind="monitor_suppressed",
                            description=t.description,
                            payload=(f"[{suppressed} events suppressed — "
                                     "output rate too high. Consider TaskStop "
                                     "and re-arm with a stricter filter.]"),
                        )
                        suppressed = 0
                        first_suppress_at = None
                    await self.injector.push(
                        task_id=tid, kind="monitor_event",
                        description=t.description, payload=line_str,
                    )
                else:
                    suppressed += 1
                    if first_suppress_at is None:
                        first_suppress_at = time.monotonic()
                    elif (time.monotonic() - first_suppress_at
                          > self.SUPPRESSION_KILL_THRESHOLD_S):
                        # 暴流熔断
                        await self.injector.push(
                            task_id=tid, kind="monitor_killed",
                            description=t.description,
                            payload=(
                                f"[Monitor stopped — script produced too much "
                                f"output ({suppressed} events suppressed over "
                                f"{self.SUPPRESSION_KILL_THRESHOLD_S:.0f}s). "
                                "Re-arm with a stricter filter (grep/awk).]"
                            ),
                        )
                        self.registry.kill(tid)
                        ended.set()
                        return

            # 自然退出
            if t.status == TaskStatus.RUNNING:
                t.status = (TaskStatus.COMPLETED
                            if t.proc.returncode == 0 else TaskStatus.FAILED)
                await self.injector.push(
                    task_id=tid, kind="monitor_exited",
                    description=t.description,
                    payload=f"process exited (code={t.proc.returncode})",
                )
            ended.set()

        async def watchdog():
            if persistent:
                return
            try:
                await asyncio.wait_for(ended.wait(), timeout=timeout_s)
            except asyncio.TimeoutError:
                if t.status == TaskStatus.RUNNING:
                    await self.injector.push(
                        task_id=tid, kind="monitor_timeout",
                        description=t.description,
                        payload="[Monitor timed out — re-arm if needed.]",
                    )
                    self.registry.kill(tid)
                    ended.set()

        await asyncio.gather(read_loop(), watchdog(), return_exceptions=True)


# ============================================================================
# 7. TaskStopTool  ——  对应 $A6()，带 owner 校验（FF7）
# ============================================================================

class TaskStopTool:
    name = "TaskStop"

    def __init__(self, registry, injector, caller_agent_id="main"):
        self.registry = registry
        self.injector = injector
        self.caller_agent_id = caller_agent_id

    async def call(self, *, task_id: str) -> dict:
        t = self.registry.get(task_id)
        if t is None:
            return {"error": f"No task with id {task_id}"}
        if t.status != TaskStatus.RUNNING:
            return {"error": f"Task {task_id} is not running ({t.status.value})"}
        # owner 校验（对应 FF7）
        if t.agent_id != self.caller_agent_id:
            return {"error": (f"Task {task_id} is owned by '{t.agent_id}'; "
                              f"agent '{self.caller_agent_id}' cannot stop it.")}
        self.registry.kill(task_id)
        await self.injector.push(
            task_id=task_id, kind="stopped",
            description=t.description, payload="stopped by TaskStop",
        )
        return {"taskId": task_id, "stopped": True}


# ============================================================================
# 8. MockModel  ——  脚本化"模型"，用规则模拟 LLM 决策
#
# 真实场景下这部分是 LLM 推理。此处用规则演示 tool_call 的形态和触发条件。
# 关键 tool_call 形状：{"name": "<ToolName>", "input": {<kwargs>}}
# ============================================================================

class MockModel:
    """
    Demo 剧本：
      1) 用户问 → 模型起 Monitor
      2) Monitor tool_result 回来 → 模型说"已开，等事件"
      3) 第一个 monitor_event 到 → 模型说"看到了，继续等"
      4) 第二个 monitor_event 到 → 模型主动 TaskStop
      5) stopped 通知到 → 模型收尾
    """
    def __init__(self):
        self.events_seen = 0

    def respond(self, messages: list[dict]) -> dict:
        # 找到上一条 assistant 消息之后所有的新内容
        last_asst = max((i for i, m in enumerate(messages)
                          if m.get("role") == "assistant"), default=-1)
        new = messages[last_asst + 1:]

        # 第一轮：还没说过话 + 用户提了问题 → 起 Monitor
        if last_asst < 0:
            return {
                "text": "我用 Monitor 监听一个 mock 事件源，每行就是一个事件。",
                "tool_calls": [{
                    "name": "Monitor",
                    "input": {
                        # 模拟一个"每 0.7s 产一行"的数据源
                        "command": (
                            "for i in 1 2 3 4 5 6 7; do "
                            "echo \"event #$i at $(date +%H:%M:%S)\"; "
                            "sleep 0.7; done"
                        ),
                        "description": "watch mock event source",
                        "timeout_ms": 60_000,
                    },
                }],
            }

        # 从新消息里抽出所有 <task-notification> 的 kind / task_id
        notes: list[tuple[str, str]] = []
        for m in new:
            if m.get("role") != "user":
                continue
            c = m.get("content", "")
            km = re.search(r'kind="([^"]+)"', c)
            tm = re.search(r'task_id="([^"]+)"', c)
            if km and tm:
                notes.append((km.group(1), tm.group(1)))

        # 没新事件 + 只有 tool_result → 静默 1 轮（让出 await，等下一波）
        if not notes:
            return {"text": "Monitor 跑着，等事件触发。期间不轮询。",
                    "tool_calls": []}

        # stopped 通知 → 收尾
        if any(k == "stopped" for k, _ in notes):
            return {"text": "Monitor 已停。任务完成。", "tool_calls": []}

        # 累计 monitor_event 个数
        evts = [(k, t) for k, t in notes if k == "monitor_event"]
        self.events_seen += len(evts)

        if self.events_seen >= 2 and evts:
            tid = evts[-1][1]
            return {
                "text": (f"已累计看到 {self.events_seen} 个事件 → "
                          f"满足条件，主动停掉 monitor {tid}。"),
                "tool_calls": [{"name": "TaskStop", "input": {"task_id": tid}}],
            }

        return {
            "text": (f"看到 {len(evts)} 个新事件，累计 {self.events_seen}/2，"
                      "继续等下一个。"),
            "tool_calls": [],
        }


# ============================================================================
# 9. AgentLoop  ——  把以上所有部件粘起来
#
# 单次推理循环的伪代码（对应 Claude Code agent loop）：
#   while not done:
#       drain(pending_user_queue)        # ← <task-notification> 进 transcript
#       resp = model(messages)           # ← LLM 推理（这里是 MockModel）
#       for call in resp.tool_calls:     # ← 串行执行 tool（可并发）
#           result = tool[call.name](**call.input)
#       if no tool_call and queue empty and has running task:
#           await queue.get()            # ← "挂起"，被 push 通知唤醒
# ============================================================================

class AgentLoop:
    def __init__(self):
        self.messages: list[dict] = []
        self.pending_user: asyncio.Queue = asyncio.Queue()
        self.injector = NotificationInjector(self.pending_user)
        self.registry = TaskRegistry()
        self.tools = {
            BashTool.name:     BashTool(self.registry, self.injector),
            MonitorTool.name:  MonitorTool(self.registry, self.injector),
            TaskStopTool.name: TaskStopTool(self.registry, self.injector),
        }
        self.model = MockModel()

    async def run(self, initial_user_msg: str, max_turns: int = 30):
        self.messages.append({"role": "user", "content": initial_user_msg})
        self._log("👤 USER", initial_user_msg)

        for turn in range(1, max_turns + 1):
            print(f"\n────────────── TURN {turn} ──────────────")

            # ① drain：把已经到的 <task-notification> 转成 user message
            while not self.pending_user.empty():
                msg = self.pending_user.get_nowait()
                self.messages.append(msg)
                self._log("🔔 INJECTED (was pending)", msg["content"])

            # ② 让模型生成本轮回应（text + tool_calls）
            resp = self.model.respond(self.messages)
            self.messages.append({
                "role": "assistant",
                "content": resp["text"],
                "tool_calls": resp.get("tool_calls", []),
            })
            self._log("🤖 ASSISTANT", resp["text"])
            for c in resp.get("tool_calls", []):
                print(f"   ⇣ tool_call → {c['name']}({_short(c['input'])})")

            # ③ 执行 tool_calls
            for call in resp.get("tool_calls", []):
                tool = self.tools[call["name"]]
                result = await tool.call(**call["input"])
                self.messages.append({
                    "role": "tool", "name": call["name"], "result": result,
                })
                print(f"   ⇡ tool_result[{call['name']}] = {_short(result)}")

            # ④ 决定下一步
            running = self.registry.all_running()
            has_tool_call = bool(resp.get("tool_calls"))

            if not has_tool_call and self.pending_user.empty():
                if not running:
                    print("\n✅ 没有 running task 也没有 pending 事件 → 退出循环\n")
                    return

                # 这就是"自动唤醒"机制的体现：
                # 模型什么都不做，主循环 await 在 queue.get() 上，
                # 直到某个 task 通过 injector.push() 投递事件 → 唤醒。
                # 不需要 sleep、不需要轮询。
                print(f"\n💤 idle — {len(running)} task 跑着，"
                      "await pending_user.get() 阻塞等通知...")
                msg = await self.pending_user.get()
                self.messages.append(msg)
                self._log("🔔 INJECTED (woke us up)", msg["content"])

        print("\n⚠️  max_turns reached")

    def _log(self, label: str, content: str):
        print(f"\n{label}:")
        for line in content.splitlines():
            print(f"   {line}")


def _short(obj, n=200):
    s = repr(obj) if not isinstance(obj, str) else obj
    return (s[:n] + "...") if len(s) > n else s


# ============================================================================
# 10. 入口
# ============================================================================

async def main():
    print("=" * 60)
    print("Claude Code 后台执行机制 —— Python 极简模拟")
    print("=" * 60)
    print("场景：模型用 Monitor 监听 mock 数据源，看到 2 个事件就 TaskStop。")
    print("观察：")
    print("  • Monitor 启动后立即返回 task_id（不阻塞）")
    print("  • 模型继续走，没有 sleep/poll；")
    print("  • 每次有 stdout 行 → <task-notification> 进队列 → 唤醒主循环")
    print("  • 模型在 idle 时 await pending_user.get()，事件到达即重启推理")
    print()

    loop = AgentLoop()
    await loop.run("帮我监听 mock 事件源，看到 2 个事件就停。")


if __name__ == "__main__":
    asyncio.run(main())
